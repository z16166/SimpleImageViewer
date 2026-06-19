use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use eframe::egui;
use parking_lot::Mutex;
use rust_i18n::t;

use crate::app::ImageViewerApp;
use crate::app::directory_tree_strip_cache::{
    DIRECTORY_TREE_STRIP_THUMBNAIL_MAX_SIDE, DirectoryTreeStripPreviewJobResult,
    decoded_rgba_size_valid,
};
use crate::app::types::CachedWindowPlacement;
use crate::directory_tree_places::DirectoryTreePlaces;
use crate::loader::DIRECTORY_TREE_STRIP_POOL;
use crate::loader::{
    DecodedImage, PreviewStage, TiledImageSource, generate_directory_tree_thumb_from_path,
    preview_aspect_matches_logical,
};
use crate::path_location::is_unc_path;
use crate::settings::{BrowseMode, Settings};
use crate::theme::ThemePalette;
use crate::ui::osd::{format_file_modified, format_file_size};

use super::sort::image_list_sort_order;
use super::ui::{
    directory_tree_panel_layout, draw_directory_tree_window, image_list_interaction_enabled,
    image_list_sorting_available, unc_share_root,
};
use super::workers::strip_worker_com_initialized;
use super::{
    DIRECTORY_TREE_COLD_NEIGHBOR_RADIUS, DIRECTORY_TREE_EMBEDDED_DEFAULT_WIDTH,
    DIRECTORY_TREE_EMBEDDED_LOADING_PANEL_ID, DIRECTORY_TREE_EMBEDDED_MIN_WIDTH,
    DIRECTORY_TREE_EMBEDDED_SIDE_PANEL_ID, DIRECTORY_TREE_LEFT_WIDTH, DIRECTORY_TREE_MIN_HEIGHT,
    DIRECTORY_TREE_MIN_WIDTH, DIRECTORY_TREE_RIGHT_MIN_WIDTH, DIRECTORY_TREE_SPLITTER_GRAB_WIDTH,
    DIRECTORY_TREE_VIEWPORT_ID, DirectoryTreeCommand, DirectoryTreeState, ImageListSortColumn,
    MAX_COLD_STRIP_GENERATES_PER_FRAME, MAX_STRIP_GENERATE_INFLIGHT,
    MAX_TILED_STRIP_GENERATES_PER_FRAME, is_places_sentinel_path,
};

impl ImageViewerApp {
    /// Lazily capture a root-window redraw hook (Windows child viewports do not wake ROOT).
    pub(crate) fn ensure_root_redraw_wake(&mut self, frame: &eframe::Frame) {
        if self.root_redraw_wake.is_some() {
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(window) = frame.winit_window() {
            let window = Arc::clone(window);
            self.root_redraw_wake = Some(Arc::new(move || {
                window.request_redraw();
            }));
        }
    }

    pub(crate) fn wake_root_for_logic(&self) {
        if let Some(wake) = &self.root_redraw_wake {
            wake();
        }
    }

    pub(crate) fn root_redraw_wake_handle(&self) -> Option<crate::app::RootRedrawWake> {
        self.root_redraw_wake.clone()
    }

    pub(crate) fn effective_scan_recursive(&self) -> bool {
        self.settings.effective_scan_recursive()
    }

    pub(crate) fn current_browse_directory(&self) -> Option<PathBuf> {
        match self.settings.browse_mode {
            BrowseMode::Tree => self
                .settings
                .tree_nav_selected_dir
                .clone()
                .or_else(|| self.settings.last_image_dir.clone()),
            BrowseMode::Linear => self.settings.last_image_dir.clone(),
        }
    }

    pub(crate) fn saved_directory_tree_selection_dir(&self) -> Option<PathBuf> {
        self.settings
            .tree_nav_selected_dir
            .clone()
            .or_else(|| {
                self.settings
                    .last_viewed_image
                    .as_ref()
                    .and_then(|path| path.parent().map(|parent| parent.to_path_buf()))
            })
            .or_else(|| self.settings.last_image_dir.clone())
    }

    pub(crate) fn reveal_directory_tree_for_saved_selection(&mut self) {
        if !self.directory_tree_settings_active() {
            return;
        }
        let Some(dir) = self.saved_directory_tree_selection_dir() else {
            return;
        };
        let requests = {
            let mut state = self.directory_tree.state.lock();
            if !state.places_loaded {
                return;
            }
            state.set_selected_dir(dir.clone());
            let mut requests = state.reveal_selected_dir();
            if let Some(request) = state.expand_tree_for_filesystem_dir(&dir) {
                requests.push(request);
            }
            requests
        };
        for request in requests {
            let _ = self.directory_tree.children_request_tx.send(request);
        }
    }

    pub(crate) fn ensure_directory_tree_places_loaded(&mut self) {
        {
            let state = self.directory_tree.state.lock();
            if state.places_loaded {
                return;
            }
            if state.places_loading {
                return;
            }
        }

        if self.directory_tree_places_load_rx.is_some() {
            return;
        }

        let (tx, rx) = crossbeam_channel::bounded(1);
        self.directory_tree_places_load_rx = Some(rx);
        {
            let mut state = self.directory_tree.state.lock();
            state.places_loading = true;
            state.places_load_error = None;
        }

        if std::thread::Builder::new()
            .name("siv-directory-tree-places".to_string())
            .spawn(move || {
                let result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                    crate::directory_tree_places::load,
                )) {
                    Ok(places) => Ok(places),
                    Err(_) => Err("places load panicked".to_string()),
                };
                let _ = tx.send(result);
            })
            .is_err()
        {
            log::error!("[DirectoryTree] Failed to spawn places loader");
            let mut state = self.directory_tree.state.lock();
            state.places_loading = false;
            state.places_load_error = Some(t!("directory_tree.places_load_failed").to_string());
            self.directory_tree_places_load_rx = None;
        }
    }

    fn apply_directory_tree_places(&mut self, places: DirectoryTreePlaces) {
        let saved_dir = if self.settings.browse_mode == BrowseMode::Tree {
            self.saved_directory_tree_selection_dir()
        } else {
            self.settings.last_image_dir.clone()
        };
        let mut state = self.directory_tree.state.lock();
        state.places_loading = false;
        state.places_load_error = None;
        state.initialize_places(places);
        if saved_dir
            .as_ref()
            .is_some_and(|path| is_unc_path(path.as_path()))
        {
            state.ensure_network_visible();
            if let Some(share) = saved_dir.as_ref().and_then(|path| unc_share_root(path)) {
                state.ensure_network_share_mounted(&share);
            }
        }
        if let Some(dir) = saved_dir {
            state.set_selected_dir(dir);
        }
        self.apply_saved_directory_tree_panel_layout_to_state(&mut state);
        drop(state);
        self.reveal_directory_tree_for_saved_selection();
    }

    pub(crate) fn poll_directory_tree_places_load(&mut self) {
        let Some(rx) = self.directory_tree_places_load_rx.as_ref() else {
            return;
        };
        let Ok(result) = rx.try_recv() else {
            return;
        };
        self.directory_tree_places_load_rx = None;
        match result {
            Ok(places) => self.apply_directory_tree_places(places),
            Err(err) => {
                log::error!("[DirectoryTree] Places load failed: {err}");
                let mut state = self.directory_tree.state.lock();
                state.places_loading = false;
                state.places_load_error = Some(t!("directory_tree.places_load_failed").to_string());
            }
        }
    }

    fn apply_directory_tree_image_list_sort(
        &mut self,
        column: ImageListSortColumn,
        ascending: bool,
    ) -> bool {
        if self.scanning || self.image_files.is_empty() {
            return false;
        }

        let len = self.image_files.len();
        if len <= 1 {
            return false;
        }

        let order = image_list_sort_order(
            len,
            column,
            ascending,
            &self.image_files,
            &self.file_byte_len_by_index,
            &self.file_modified_unix_by_index,
        );

        let already_sorted = order
            .iter()
            .enumerate()
            .all(|(new_idx, &old_idx)| new_idx == old_idx);
        if already_sorted {
            return false;
        }

        let current_path = self.image_files.get(self.current_index).cloned();
        let mut old_to_new = vec![0usize; len];
        for (new_idx, &old_idx) in order.iter().enumerate() {
            old_to_new[old_idx] = new_idx;
        }

        self.permute_image_file_arrays(&order);
        self.permute_index_keyed_caches(&old_to_new);
        if let Some(path) = current_path {
            if let Some(index) = self.image_files.iter().position(|entry| entry == &path) {
                self.current_index = index;
                self.image_status.set_current_index(self.current_index);
                self.raw_metadata.set_current_index(self.current_index);
            }
        }
        true
    }

    pub(crate) fn initialize_directory_tree_root(&mut self, root: PathBuf) {
        self.settings.browse_mode = BrowseMode::Tree;
        self.settings.show_directory_tree_nav = true;
        self.settings.tree_nav_root_dir = Some(root.clone());
        self.settings.tree_nav_selected_dir = Some(root.clone());
        self.settings.last_image_dir = Some(root.clone());

        self.ensure_directory_tree_places_loaded();
        let runtime = &self.directory_tree;
        let requests = {
            let mut state = runtime.state.lock();
            state.set_selected_dir(root.clone());
            let mut requests = state.reveal_selected_dir();
            if let Some(request) = state.expand_tree_for_filesystem_dir(&root) {
                requests.push(request);
            }
            requests
        };
        for request in requests {
            let _ = runtime.children_request_tx.send(request);
        }
    }

    pub(crate) fn process_directory_tree_events(&mut self, ctx: &egui::Context) {
        while let Ok(result) = self.directory_tree.result_rx.try_recv() {
            let requests = {
                let mut state = self.directory_tree.state.lock();
                state.apply_children_result(result);
                state.reveal_selected_dir()
            };
            for request in requests {
                let _ = self.directory_tree.children_request_tx.send(request);
            }
            ctx.request_repaint();
            self.request_directory_tree_viewport_repaint(ctx);
        }

        while let Ok(result) = self.directory_tree.metadata_result_rx.try_recv() {
            self.directory_tree
                .state
                .lock()
                .apply_metadata_result(result);
            ctx.request_repaint();
            self.request_directory_tree_viewport_repaint(ctx);
        }

        while let Ok(command) = self.directory_tree.command_rx.try_recv() {
            match command {
                DirectoryTreeCommand::SelectDirectory(path) => {
                    if is_places_sentinel_path(&path) {
                        continue;
                    }
                    self.settings.browse_mode = BrowseMode::Tree;
                    self.settings.show_directory_tree_nav = true;
                    self.settings.tree_nav_selected_dir = Some(path.clone());
                    {
                        let mut state = self.directory_tree.state.lock();
                        state.set_selected_dir(path.clone());
                        state.image_list_keyboard_active = false;
                        if let Some(request) = state.expand_tree_for_filesystem_dir(&path) {
                            let _ = self.directory_tree.children_request_tx.send(request);
                        }
                    }
                    self.load_directory(path);
                    self.queue_save();
                    self.wake_root_for_logic();
                    ctx.request_repaint();
                }
                DirectoryTreeCommand::ToggleExpanded(path) => {
                    let request = self.directory_tree.state.lock().toggle_expanded(&path);
                    if let Some(request) = request {
                        let _ = self.directory_tree.children_request_tx.send(request);
                    }
                    ctx.request_repaint();
                }
                DirectoryTreeCommand::SelectImage(index) => {
                    if self
                        .directory_tree
                        .state
                        .try_lock()
                        .is_some_and(|state| !image_list_interaction_enabled(&state))
                    {
                        continue;
                    }
                    if index < self.image_files.len() {
                        self.pending_directory_tree_select_index = Some(index);
                        let mut state = self.directory_tree.state.lock();
                        state.current_index = index;
                        state.scroll_image_list_to_current = true;
                        ctx.request_repaint();
                        self.request_directory_tree_viewport_repaint(ctx);
                    }
                }
                DirectoryTreeCommand::SortImageList(column) => {
                    let (sort_column, sort_ascending) = {
                        let mut state = self.directory_tree.state.lock();
                        if !image_list_sorting_available(&state) {
                            continue;
                        }
                        let ascending = if state.image_list_sort_column == column {
                            !state.image_list_sort_ascending
                        } else {
                            true
                        };
                        state.image_list_sort_column = column;
                        state.image_list_sort_ascending = ascending;
                        state.image_list_sort_active = true;
                        state.image_list_reordering = true;
                        (column, ascending)
                    };

                    let changed =
                        self.apply_directory_tree_image_list_sort(sort_column, sort_ascending);
                    {
                        let mut state = self.directory_tree.state.lock();
                        state.image_list_reordering = false;
                        if changed {
                            state.image_list_generation =
                                state.image_list_generation.wrapping_add(1);
                            state.current_index = self.current_index;
                            state.image_list_col_widths_dirty = true;
                            state.scroll_image_list_to_current = true;
                        }
                    }
                    if changed {
                        self.sync_directory_tree_file_list_state(ctx);
                        self.wake_root_for_logic();
                    }
                    ctx.request_repaint();
                    self.request_directory_tree_viewport_repaint(ctx);
                }
                DirectoryTreeCommand::CloseWindow => {
                    self.settings.show_directory_tree_nav = false;
                    self.queue_save();
                    ctx.request_repaint();
                }
            }
        }
    }

    pub(crate) fn directory_tree_settings_active(&self) -> bool {
        self.settings.browse_mode == BrowseMode::Tree && self.settings.show_directory_tree_nav
    }

    fn directory_tree_viewport_active(&self) -> bool {
        if !self.directory_tree_settings_active() {
            return false;
        }
        match self.directory_tree.state.try_lock() {
            Some(state) => state.places_loaded,
            // Embedded panel may hold the lock during paint; still treat as active.
            None => true,
        }
    }

    pub(crate) fn directory_tree_nav_is_detached(&self) -> bool {
        matches!(
            self.settings.directory_tree_nav_style,
            crate::settings::DirectoryTreeNavStyle::Detached
        )
    }

    pub(crate) fn directory_tree_nav_is_embedded(&self) -> bool {
        matches!(
            self.settings.directory_tree_nav_style,
            crate::settings::DirectoryTreeNavStyle::Embedded
        )
    }

    pub(crate) fn directory_tree_repaint_viewport_id(&self) -> egui::ViewportId {
        if self.directory_tree_nav_is_detached() {
            Self::directory_tree_viewport_id()
        } else {
            egui::ViewportId::ROOT
        }
    }

    pub(crate) fn directory_tree_list_accepts_keyboard_input(
        ctx: &egui::Context,
        embedded: bool,
    ) -> bool {
        if embedded {
            Self::root_viewport_has_os_focus(ctx)
        } else {
            Self::directory_tree_viewport_has_os_focus(ctx)
        }
    }

    pub(crate) fn directory_tree_embedded_list_captures_main_navigation(&self) -> bool {
        if !self.directory_tree_nav_is_embedded() || !self.directory_tree_settings_active() {
            return false;
        }
        self.directory_tree
            .state
            .try_lock()
            .is_some_and(|state| state.image_list_keyboard_active)
    }

    pub(crate) fn on_directory_tree_nav_style_changed(
        &mut self,
        ctx: &egui::Context,
        was_detached: bool,
    ) {
        if was_detached && self.directory_tree_nav_is_embedded() {
            ctx.send_viewport_cmd_to(
                Self::directory_tree_viewport_id(),
                egui::ViewportCommand::Close,
            );
        }
        {
            let mut state = self.directory_tree.state.lock();
            self.apply_saved_directory_tree_panel_layout_to_state(&mut state);
        }
        ctx.request_repaint();
    }

    fn apply_saved_directory_tree_panel_layout_to_state(&self, state: &mut DirectoryTreeState) {
        if let Some(width) = self.settings.directory_tree_folder_panel_width {
            state.left_panel_width = width;
        }
        if let Some(width) = self.settings.directory_tree_image_list_panel_width {
            state.image_list_panel_width = width;
        }
        #[cfg(feature = "preload-debug")]
        crate::preload_debug!(
            "[PreloadDebug][Panel] apply_saved folder={:?} list={:?} embedded={:?} -> left={:.1} list={:.1}",
            self.settings.directory_tree_folder_panel_width,
            self.settings.directory_tree_image_list_panel_width,
            self.settings.directory_tree_embedded_panel_width,
            state.left_panel_width,
            state.image_list_panel_width
        );
    }

    pub(crate) fn restore_saved_directory_tree_panel_layout(&self) {
        let mut state = self.directory_tree.state.lock();
        self.apply_saved_directory_tree_panel_layout_to_state(&mut state);
        #[cfg(feature = "preload-debug")]
        crate::preload_debug!(
            "[PreloadDebug][Panel] restore_saved at startup folder={:?} list={:?} embedded={:?}",
            self.settings.directory_tree_folder_panel_width,
            self.settings.directory_tree_image_list_panel_width,
            self.settings.directory_tree_embedded_panel_width
        );
    }

    pub(crate) fn persist_directory_tree_layout_to_settings(&mut self) {
        let state = self.directory_tree.state.lock();
        #[cfg(feature = "preload-debug")]
        let before_folder = self.settings.directory_tree_folder_panel_width;
        #[cfg(feature = "preload-debug")]
        let before_list = self.settings.directory_tree_image_list_panel_width;
        #[cfg(feature = "preload-debug")]
        let before_embedded = self.settings.directory_tree_embedded_panel_width;
        if state.left_panel_width > 0.0 {
            self.settings.directory_tree_folder_panel_width = Some(state.left_panel_width);
        }
        if state.image_list_panel_width > 0.0 {
            self.settings.directory_tree_image_list_panel_width =
                Some(state.image_list_panel_width);
        }
        if self.directory_tree_nav_is_embedded() && state.embedded_nav_panel_width > 0.0 {
            self.settings.directory_tree_embedded_panel_width = Some(
                state
                    .embedded_nav_panel_width
                    .max(DIRECTORY_TREE_EMBEDDED_MIN_WIDTH),
            );
        }
        #[cfg(feature = "preload-debug")]
        crate::preload_debug!(
            "[PreloadDebug][Panel] persist state left={:.1} list={:.1} embedded={:.1} -> settings folder {:?}->{:?} list {:?}->{:?} embedded {:?}->{:?}",
            state.left_panel_width,
            state.image_list_panel_width,
            state.embedded_nav_panel_width,
            before_folder,
            self.settings.directory_tree_folder_panel_width,
            before_list,
            self.settings.directory_tree_image_list_panel_width,
            before_embedded,
            self.settings.directory_tree_embedded_panel_width
        );
    }

    pub(crate) fn sync_directory_tree_preview_textures_to_state(
        &mut self,
        ctx: &egui::Context,
    ) -> bool {
        if !self.directory_tree_settings_active() {
            return false;
        }
        let revision = self.directory_tree_strip_cache.gpu_revision();
        #[cfg(feature = "preload-debug")]
        let cache_count = self.directory_tree_strip_cache.textures().len();
        let Some(mut state) = self.directory_tree.state.try_lock() else {
            #[cfg(feature = "preload-debug")]
            crate::preload_debug!(
                "[PreloadDebug][DirTree] sync_preview_textures skipped: state locked cache_rev={} cache_count={}",
                revision,
                cache_count
            );
            self.defer_directory_tree_file_list_sync();
            return false;
        };
        let updated = state.sync_preview_textures(
            self.directory_tree_strip_cache.textures(),
            self.directory_tree_strip_cache.logical_sizes(),
            revision,
        );
        drop(state);
        if updated {
            ctx.request_repaint_of(self.directory_tree_repaint_viewport_id());
            self.mark_directory_tree_repaint_pending();
            if self.directory_tree_nav_is_embedded() {
                ctx.request_repaint();
            }
        }
        updated
    }

    fn flush_directory_tree_panel_layout_persist(&mut self) {
        let dirty = {
            let mut state = self.directory_tree.state.lock();
            let dirty = state.panel_layout_dirty;
            state.panel_layout_dirty = false;
            dirty
        };
        if dirty {
            self.persist_directory_tree_layout_to_settings();
            self.queue_save();
        }
    }

    pub(crate) fn persist_directory_tree_window_placement_to_settings(
        settings: &mut crate::settings::Settings,
        placement: CachedWindowPlacement,
        restore: Option<CachedWindowPlacement>,
    ) {
        settings.directory_tree_window_maximized = placement.maximized;
        settings.directory_tree_window_outer_position = Some(placement.outer_position);
        settings.directory_tree_window_inner_size = Some(placement.inner_size);
        settings.directory_tree_window_maximized_screen_center = Some(placement.outer_center);
        if placement.maximized {
            settings.directory_tree_window_maximized_inner_size = Some(placement.inner_size);
            let restore_inner = restore
                .map(|p| p.inner_size)
                .or(settings.directory_tree_window_restore_inner_size)
                .unwrap_or(placement.inner_size);
            if let Some(restore) = restore {
                settings.directory_tree_window_restore_outer_position =
                    Some(restore.outer_position);
                settings.directory_tree_window_restore_inner_size = Some(restore.inner_size);
            } else if let Some(pos) =
                crate::settings::Settings::valid_outer_position(placement.outer_position)
            {
                settings.directory_tree_window_restore_outer_position = Some(pos);
                settings.directory_tree_window_restore_inner_size = Some(restore_inner);
            } else if let Some(top_left) =
                crate::settings::Settings::restore_outer_top_left_for_screen_center(
                    placement.outer_center,
                    restore_inner,
                )
            {
                settings.directory_tree_window_restore_outer_position = Some(top_left);
                settings.directory_tree_window_restore_inner_size = Some(restore_inner);
            }
        } else {
            settings.directory_tree_window_restore_outer_position = Some(placement.outer_position);
            settings.directory_tree_window_restore_inner_size = Some(placement.inner_size);
            settings.directory_tree_window_maximized_inner_size = None;
        }
    }

    pub(crate) fn cache_directory_tree_viewport_placement(&mut self, ctx: &egui::Context) {
        if !self.directory_tree_settings_active() || !self.directory_tree_nav_is_detached() {
            return;
        }
        let Some(placement) = ctx.viewport_for(Self::directory_tree_viewport_id(), |viewport| {
            let viewport = viewport.input.viewport();
            let outer_rect = viewport.outer_rect?;
            let inner_size = viewport.inner_rect.unwrap_or(outer_rect).size();
            let center = outer_rect.center();
            Some(CachedWindowPlacement {
                outer_position: [
                    outer_rect.min.x.round() as i32,
                    outer_rect.min.y.round() as i32,
                ],
                outer_center: [center.x.round() as i32, center.y.round() as i32],
                inner_size: [
                    inner_size.x.round().max(1.0) as u32,
                    inner_size.y.round().max(1.0) as u32,
                ],
                maximized: viewport.maximized.unwrap_or(false),
            })
        }) else {
            return;
        };
        if !placement.maximized
            && Settings::valid_outer_position(placement.outer_position).is_some()
        {
            self.cached_directory_tree_restore_placement = Some(placement);
        }
        self.cached_directory_tree_window_placement = Some(placement);
    }

    fn directory_tree_embedded_panel_default_width(settings: &Settings) -> f32 {
        if let Some(width) = settings.directory_tree_embedded_panel_width {
            return width.max(DIRECTORY_TREE_EMBEDDED_MIN_WIDTH);
        }
        let folder = settings
            .directory_tree_folder_panel_width
            .unwrap_or(DIRECTORY_TREE_LEFT_WIDTH);
        let list = settings
            .directory_tree_image_list_panel_width
            .unwrap_or(DIRECTORY_TREE_RIGHT_MIN_WIDTH);
        (folder + DIRECTORY_TREE_SPLITTER_GRAB_WIDTH + list)
            .max(DIRECTORY_TREE_EMBEDDED_DEFAULT_WIDTH)
    }

    pub(crate) fn embedded_nav_panel_width_estimate(&self) -> f32 {
        if let Some(width) = self.settings.directory_tree_embedded_panel_width {
            return width.max(DIRECTORY_TREE_EMBEDDED_MIN_WIDTH);
        }
        if let Some(state) = self.directory_tree.state.try_lock() {
            if state.embedded_nav_panel_width > 0.0 {
                return state.embedded_nav_panel_width;
            }
        }
        Self::directory_tree_embedded_panel_default_width(&self.settings)
    }

    pub(crate) fn directory_tree_viewport_id() -> egui::ViewportId {
        egui::ViewportId::from_hash_of(DIRECTORY_TREE_VIEWPORT_ID)
    }

    pub(crate) fn root_viewport_has_os_focus(ctx: &egui::Context) -> bool {
        Self::viewport_has_os_focus(ctx, egui::ViewportId::ROOT)
    }

    pub(crate) fn directory_tree_viewport_has_os_focus(ctx: &egui::Context) -> bool {
        Self::viewport_has_os_focus(ctx, Self::directory_tree_viewport_id())
    }

    fn viewport_has_os_focus(ctx: &egui::Context, viewport_id: egui::ViewportId) -> bool {
        ctx.input(|i| {
            i.raw
                .viewports
                .get(&viewport_id)
                .and_then(|info| info.focused)
                .unwrap_or(false)
        })
    }

    /// Release directory-tree list keyboard capture when the main window is focused.
    pub(crate) fn sync_directory_tree_keyboard_focus_with_viewports(
        &mut self,
        ctx: &egui::Context,
    ) {
        if !self.directory_tree_viewport_active() || !self.directory_tree_nav_is_detached() {
            return;
        }
        if Self::root_viewport_has_os_focus(ctx) && !Self::directory_tree_viewport_has_os_focus(ctx)
        {
            self.release_directory_tree_list_keyboard_capture();
        }
    }

    pub(crate) fn release_directory_tree_list_keyboard_capture(&mut self) {
        if self.directory_tree_settings_active() {
            if let Some(mut state) = self.directory_tree.state.try_lock() {
                state.image_list_keyboard_active = false;
            }
        }
    }

    pub(crate) fn deactivate_directory_tree_list_keyboard(&mut self, ctx: &egui::Context) {
        self.release_directory_tree_list_keyboard_capture();
        ctx.memory_mut(|mem| mem.request_focus(egui::Id::NULL));
    }

    pub(crate) fn request_directory_tree_viewport_repaint(&self, ctx: &egui::Context) {
        if self.directory_tree_viewport_active() {
            ctx.request_repaint_of(self.directory_tree_repaint_viewport_id());
        }
    }

    pub(crate) fn sync_directory_tree_theme_snapshot(&mut self) {
        if let Ok(mut theme) = self.directory_tree_theme.lock() {
            *theme = self.cached_palette.clone();
        }
    }

    pub(crate) fn mark_directory_tree_repaint_pending(&mut self) {
        if self.directory_tree_viewport_active() && self.directory_tree_nav_is_detached() {
            self.pending_directory_tree_repaint = true;
        }
    }

    pub(crate) fn take_pending_directory_tree_repaint(&mut self) -> Option<egui::ViewportId> {
        if !self.directory_tree_nav_is_detached()
            || !self.pending_directory_tree_repaint
            || !self.directory_tree_viewport_active()
        {
            return None;
        }
        self.pending_directory_tree_repaint = false;
        Some(Self::directory_tree_viewport_id())
    }

    pub(crate) fn process_pending_directory_tree_state_sync(&mut self, ctx: &egui::Context) {
        if !self.pending_directory_tree_state_sync {
            return;
        }
        if self.directory_tree.state.try_lock().is_none() {
            return;
        }
        self.pending_directory_tree_state_sync = false;
        self.sync_directory_tree_file_list_state(ctx);
    }

    fn defer_directory_tree_file_list_sync(&mut self) {
        self.pending_directory_tree_state_sync = true;
    }

    /// Sync scan results into the directory-tree file list without registering the viewport.
    /// Safe to call from `logic()` after `process_scan_results`.
    pub(crate) fn sync_directory_tree_file_list_state(&mut self, ctx: &egui::Context) {
        if !self.directory_tree_settings_active() {
            return;
        }

        let viewport_id = self.directory_tree_repaint_viewport_id();
        let request_viewport_repaint = {
            let Some(mut state) = self.directory_tree.state.try_lock() else {
                self.defer_directory_tree_file_list_sync();
                return;
            };
            let preview_updated = state.sync_preview_textures(
                self.directory_tree_strip_cache.textures(),
                self.directory_tree_strip_cache.logical_sizes(),
                self.directory_tree_strip_cache.gpu_revision(),
            );
            if !state.places_loaded {
                (preview_updated, false, ImageListSortColumn::default(), true)
            } else {
                let previous_index = state.current_index;
                let previous_scanning = state.scanning;
                let previous_row_count = state.image_rows.len();
                let resort_after_scan =
                    previous_scanning && !self.scanning && state.image_list_sort_active;
                let resort_column = state.image_list_sort_column;
                let resort_ascending = state.image_list_sort_ascending;
                let scan_status = Self::directory_tree_scan_status_message(self);
                state.sync_images(
                    &self.image_files,
                    &self.file_byte_len_by_index,
                    &self.file_modified_unix_by_index,
                    self.current_index,
                    self.scanning,
                    scan_status,
                    &self.directory_tree.metadata_request_tx,
                );
                let repaint = preview_updated
                    || state.scroll_image_list_to_current
                    || state.current_index != previous_index
                    || state.scanning != previous_scanning
                    || state.image_rows.len() != previous_row_count;
                #[cfg(feature = "preload-debug")]
                if repaint
                    && (state.scanning != previous_scanning
                        || state.image_rows.len() != previous_row_count
                        || preview_updated)
                {
                    crate::preload_debug!(
                        "[PreloadDebug][Scan] directory tree viewport repaint: scanning {} -> {} rows {} -> {} preview_sync={}",
                        previous_scanning,
                        state.scanning,
                        previous_row_count,
                        state.image_rows.len(),
                        preview_updated
                    );
                }
                (repaint, resort_after_scan, resort_column, resort_ascending)
            }
        };

        if request_viewport_repaint.1
            && self.apply_directory_tree_image_list_sort(
                request_viewport_repaint.2,
                request_viewport_repaint.3,
            )
        {
            if let Some(mut state) = self.directory_tree.state.try_lock() {
                state.sync_images(
                    &self.image_files,
                    &self.file_byte_len_by_index,
                    &self.file_modified_unix_by_index,
                    self.current_index,
                    self.scanning,
                    self.status_message.clone(),
                    &self.directory_tree.metadata_request_tx,
                );
                state.image_list_generation = state.image_list_generation.wrapping_add(1);
                state.current_index = self.current_index;
                state.image_list_col_widths_dirty = true;
                state.scroll_image_list_to_current = true;
            }
        }

        if request_viewport_repaint.0 {
            ctx.request_repaint_of(viewport_id);
            self.mark_directory_tree_repaint_pending();
        }
        if self.directory_tree_viewport_active() {
            // Keep ROOT painting while the tree viewport is open. logic() may run on a child
            // repaint; egui repaint requests alone do not wake ROOT on Windows.
            self.wake_root_for_logic();
            if self.scanning || self.scan_results_pending_since.is_some() {
                ctx.request_repaint();
            }
        }
    }

    /// Register the detached directory-tree viewport (draw only; state is synced from `logic()`).
    pub(crate) fn prepare_directory_tree_file_list_viewport(&mut self, ctx: &egui::Context) {
        if !self.directory_tree_viewport_active() || !self.directory_tree_nav_is_detached() {
            return;
        }

        let viewport_id = Self::directory_tree_viewport_id();
        let state = Arc::clone(&self.directory_tree.state);
        let command_tx = self.directory_tree.command_tx.clone();
        let root_wake = self.root_redraw_wake_handle();
        let theme = std::sync::Arc::clone(&self.directory_tree_theme);
        let inner_size = self.settings.directory_tree_startup_inner_size();
        let outer_position = self.settings.directory_tree_startup_outer_position();
        let startup_maximized = self.settings.directory_tree_window_maximized;
        let mut builder = egui::ViewportBuilder::default()
            .with_title(t!("directory_tree.title").to_string())
            .with_inner_size(inner_size)
            .with_min_inner_size([DIRECTORY_TREE_MIN_WIDTH, DIRECTORY_TREE_MIN_HEIGHT])
            .with_resizable(true)
            .with_close_button(true)
            .with_maximized(false);
        if let Some(pos) = outer_position {
            builder = builder.with_position(pos);
        }

        ctx.show_viewport_deferred(viewport_id, builder, move |ui, _class| {
            if ui.ctx().input(|i| i.viewport().close_requested()) {
                let _ = command_tx.send(DirectoryTreeCommand::CloseWindow);
                return;
            }

            let maximize_id = ui.id().with("directory_tree_startup_maximize");
            if startup_maximized && !ui.data(|d| d.get_temp::<bool>(maximize_id).unwrap_or(false)) {
                ui.ctx()
                    .send_viewport_cmd(egui::ViewportCommand::Maximized(true));
                ui.data_mut(|d| d.insert_temp(maximize_id, true));
            }

            let palette = theme
                .lock()
                .map(|guard| guard.clone())
                .unwrap_or_else(|poisoned| {
                    log::warn!(
                        "[DirectoryTree] directory_tree_theme mutex poisoned; recovering palette"
                    );
                    poisoned.into_inner().clone()
                });
            let scanning = {
                let Some(mut state) = state.try_lock() else {
                    return;
                };
                draw_directory_tree_window(
                    ui,
                    &mut state,
                    &command_tx,
                    root_wake.as_ref(),
                    &palette,
                    false,
                );
                state.scanning
            };
            if scanning {
                if let Some(wake) = &root_wake {
                    wake();
                }
                ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
            }
        });
    }

    /// Draw the directory tree inside a resizable left panel on the main window.
    pub(crate) fn draw_embedded_directory_tree_panel(&mut self, ui: &mut egui::Ui) {
        if !self.directory_tree_settings_active() || !self.directory_tree_nav_is_embedded() {
            return;
        }

        let state = Arc::clone(&self.directory_tree.state);
        let command_tx = self.directory_tree.command_tx.clone();
        let root_wake = self.root_redraw_wake_handle();
        let theme = Arc::clone(&self.directory_tree_theme);
        let default_width = Self::directory_tree_embedded_panel_default_width(&self.settings);
        let has_places = state.try_lock().is_some_and(|guard| guard.places_loaded);
        if !has_places {
            egui::Panel::left(DIRECTORY_TREE_EMBEDDED_LOADING_PANEL_ID)
                .resizable(false)
                .show_inside(ui, |ui| {
                    if let Some(guard) = state.try_lock() {
                        if guard.places_loading {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label(t!("directory_tree.places_loading"));
                            });
                        } else if let Some(err) = &guard.places_load_error {
                            ui.label(
                                egui::RichText::new(err.as_str())
                                    .color(ui.visuals().error_fg_color),
                            );
                        } else if !guard.workers_available {
                            ui.label(t!("directory_tree.workers_unavailable"));
                        }
                    }
                });
            return;
        }

        egui::Panel::left(DIRECTORY_TREE_EMBEDDED_SIDE_PANEL_ID)
            .resizable(true)
            .default_size(default_width)
            .min_size(DIRECTORY_TREE_EMBEDDED_MIN_WIDTH)
            .show_inside(ui, |ui| {
                let palette = theme
                    .lock()
                    .map(|guard| guard.clone())
                    .unwrap_or_else(|poisoned| {
                        log::warn!(
                            "[DirectoryTree] directory_tree_theme mutex poisoned; recovering palette"
                        );
                        poisoned.into_inner().clone()
                    });
                let Some(mut state) = state.try_lock() else {
                    return;
                };
                draw_directory_tree_window(
                    ui,
                    &mut state,
                    &command_tx,
                    root_wake.as_ref(),
                    &palette,
                    true,
                );
                if state.scanning {
                    ui.ctx().request_repaint();
                }
            });
    }

    /// Apply a directory-tree list selection queued in `process_directory_tree_events`.
    /// Runs from `logic()` only (never from `ui()`): `navigate_to` may pump the Windows
    /// message loop and must not block on `directory_tree.state` held by embedded paint.
    pub(crate) fn process_pending_directory_tree_select(&mut self, ctx: &egui::Context) {
        let Some(index) = self.pending_directory_tree_select_index.take() else {
            return;
        };
        if index >= self.image_files.len() {
            return;
        }
        self.navigate_to(index, ctx);
    }

    /// Drain directory scans, apply tree commands, sync the file list, then run strip/preloads.
    /// Must run at the start of `logic()` (before HDR/GPU work) and again after tree selection
    /// so a scan that finishes on a background thread is not left in `scan_rx` until the next
    /// frame's heavy upload path (see preload-debug `wait_ms` logs).
    pub(crate) fn process_directory_scan_pipeline(&mut self, ctx: &egui::Context) {
        self.process_scan_results();
        self.process_directory_tree_events(ctx);
        self.process_scan_results();
        #[cfg(feature = "preload-debug")]
        if let Some(since) = self.scan_results_pending_since {
            let wait_ms = crate::preload_debug::elapsed_ms(since);
            if wait_ms > 100 {
                crate::preload_debug!(
                    "[PreloadDebug][Scan] scan still pending after pipeline wait_ms={} scanning={} scan_rx={}",
                    wait_ms,
                    self.scanning,
                    self.scan_rx.is_some()
                );
            }
        }
    }

    fn directory_tree_scan_status_message(app: &ImageViewerApp) -> String {
        if !app.scanning {
            return String::new();
        }
        if app.image_files.is_empty() {
            t!("directory_tree.scanning").to_string()
        } else {
            t!(
                "directory_tree.scanning_found",
                count = app.image_files.len().to_string()
            )
            .to_string()
        }
    }

    /// Strip-thumbnail polling/generation and deferred main-image preloads after a scan.
    pub(crate) fn run_directory_tree_logic_updates(&mut self, ctx: &egui::Context) {
        if !self.directory_tree_settings_active() {
            return;
        }

        self.poll_directory_tree_places_load();
        self.ensure_directory_tree_places_loaded();

        if self.pending_preload_after_directory_scan {
            self.pending_preload_after_directory_scan = false;
            self.schedule_preloads(true);
        }

        self.ensure_directory_tree_strip_thumbnails(ctx);
        self.sync_directory_tree_preview_textures_to_state(ctx);

        if self.directory_tree.state.try_lock().is_none() {
            self.defer_directory_tree_file_list_sync();
        }
        if self.directory_tree_strip_cache.gpu_revision() > 0 {
            self.request_directory_tree_viewport_repaint(ctx);
        }
        let viewport_id = self.directory_tree_repaint_viewport_id();
        ctx.request_repaint_of(viewport_id);
        self.mark_directory_tree_repaint_pending();
        if self.directory_tree_nav_is_embedded() {
            ctx.request_repaint();
        }
        self.wake_root_for_logic();
        self.flush_directory_tree_panel_layout_persist();
    }

    pub(crate) fn cache_directory_tree_strip_thumbnail(
        &mut self,
        index: usize,
        decoded: &crate::loader::DecodedImage,
        stage: crate::loader::PreviewStage,
        logical_size: Option<(u32, u32)>,
        ctx: &egui::Context,
    ) {
        if index >= self.image_files.len() {
            return;
        }
        self.directory_tree_strip_cache.upsert_from_decoded(
            index,
            decoded,
            stage,
            logical_size,
            ctx,
            self.current_index,
            self.image_files.len(),
        );
    }

    pub(crate) fn directory_tree_strip_logical_size(&self, index: usize) -> Option<(u32, u32)> {
        if let Some((width, height)) = self.texture_cache.get_original_res(index) {
            return Some((width, height));
        }
        if let Some(&(width, height)) = self.directory_tree_strip_cache.logical_sizes().get(&index)
        {
            return Some((width, height));
        }
        if let Some(tm) = self.prefetched_tiles.get(&index) {
            let source = tm.get_source();
            return Some((source.width(), source.height()));
        }
        if let Some(tm) = self.tile_manager.as_ref()
            && tm.image_index == index
        {
            let source = tm.get_source();
            return Some((source.width(), source.height()));
        }
        None
    }

    fn tiled_sdr_source_for_index(&self, index: usize) -> Option<Arc<dyn TiledImageSource>> {
        if let Some(tm) = self.prefetched_tiles.get(&index) {
            return Some(tm.get_source());
        }
        if let Some(tm) = self.tile_manager.as_ref()
            && tm.image_index == index
        {
            return Some(tm.get_source());
        }
        None
    }

    pub(crate) fn try_sync_strip_from_tile_manager_preview(&mut self, index: usize) {
        let Some(logical) = self.directory_tree_strip_logical_size(index) else {
            return;
        };
        let preview_texture = self
            .prefetched_tiles
            .get(&index)
            .and_then(|tm| tm.preview_texture.as_ref())
            .or_else(|| {
                self.tile_manager
                    .as_ref()
                    .filter(|tm| tm.image_index == index)
                    .and_then(|tm| tm.preview_texture.as_ref())
            });
        let Some(texture) = preview_texture else {
            return;
        };
        let size = texture.size();
        let preview_w = size[0] as u32;
        let preview_h = size[1] as u32;
        if !preview_aspect_matches_logical(preview_w, preview_h, logical.0, logical.1) {
            return;
        }
        let incoming_max = preview_w.max(preview_h);
        if self
            .directory_tree_strip_cache
            .is_valid_for_logical(index, logical)
        {
            if self
                .directory_tree_strip_cache
                .cached_preview_max_side(index)
                .is_some_and(|cached_max| incoming_max <= cached_max)
            {
                return;
            }
        }
        self.directory_tree_strip_cache.insert_from_texture_handle(
            index,
            texture.clone(),
            crate::loader::PreviewStage::Refined,
            incoming_max,
            Some(logical),
            self.current_index,
            self.image_files.len(),
        );
    }

    pub(crate) fn try_sync_strip_from_texture_cache(&mut self, index: usize) {
        let Some(logical) = self.directory_tree_strip_logical_size(index) else {
            return;
        };
        if self
            .directory_tree_strip_cache
            .is_valid_for_logical(index, logical)
        {
            return;
        }
        let Some(texture) = self.texture_cache.get(index).cloned() else {
            return;
        };
        let size = texture.size();
        let preview_w = size[0] as u32;
        let preview_h = size[1] as u32;
        if !preview_aspect_matches_logical(preview_w, preview_h, logical.0, logical.1) {
            return;
        }
        let incoming_max = preview_w.max(preview_h);
        self.directory_tree_strip_cache.insert_from_texture_handle(
            index,
            texture,
            crate::loader::PreviewStage::Refined,
            incoming_max,
            Some(logical),
            self.current_index,
            self.image_files.len(),
        );
        #[cfg(feature = "preload-debug")]
        crate::preload_debug!(
            "[PreloadDebug][DirTree] strip sync from texture_cache idx={} logical={}x{} tex={}x{} cache_rev={}",
            index,
            logical.0,
            logical.1,
            preview_w,
            preview_h,
            self.directory_tree_strip_cache.gpu_revision()
        );
    }

    fn strip_index_needs_cold_thumbnail(&self, index: usize) -> bool {
        if index >= self.image_files.len() {
            return false;
        }
        if self.tiled_sdr_source_for_index(index).is_some() {
            return false;
        }
        if self
            .deferred_sdr_uploads
            .get(&index)
            .is_some_and(|decoded| !crate::loader::decoded_looks_like_black_placeholder(decoded))
        {
            return false;
        }
        if self.directory_tree_strip_generate_inflight.contains(&index) {
            return false;
        }
        if self.directory_tree_strip_cold_attempted.contains(&index) {
            return false;
        }
        if let Some(logical) = self.directory_tree_strip_logical_size(index) {
            if self
                .directory_tree_strip_cache
                .is_valid_for_logical(index, logical)
            {
                return false;
            }
        } else if self.directory_tree_strip_cache.contains(index) {
            return false;
        }
        true
    }

    pub(super) fn visible_cold_strip_indices(
        visible_row_range: Option<(usize, usize)>,
        scroll_to_current_pending: bool,
        total: usize,
        bootstrap_visible: bool,
    ) -> Vec<usize> {
        if total == 0 {
            return Vec::new();
        }
        if scroll_to_current_pending && !bootstrap_visible {
            return Vec::new();
        }
        visible_row_range
            .map(|(start, end)| (start..end.min(total)).collect())
            .unwrap_or_default()
    }

    fn collect_cold_strip_thumbnail_candidates(
        &self,
        visible_row_range: Option<(usize, usize)>,
        scroll_to_current_pending: bool,
        bootstrap_visible: bool,
    ) -> Vec<usize> {
        let total = self.image_files.len();
        if total == 0 {
            return Vec::new();
        }
        let current = self.current_index.min(total.saturating_sub(1));
        let mut ordered = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut push = |index: usize| {
            if index < total && seen.insert(index) && self.strip_index_needs_cold_thumbnail(index) {
                ordered.push(index);
            }
        };

        push(current);

        for index in Self::visible_cold_strip_indices(
            visible_row_range,
            scroll_to_current_pending,
            total,
            bootstrap_visible,
        ) {
            push(index);
        }

        for delta in 1..=DIRECTORY_TREE_COLD_NEIGHBOR_RADIUS {
            push(current.saturating_sub(delta));
            if current + delta < total {
                push(current + delta);
            }
        }

        ordered
    }

    pub(crate) fn try_generate_cold_directory_tree_strip_thumbnail(&mut self, index: usize) {
        if !self.strip_index_needs_cold_thumbnail(index) {
            return;
        }
        let path = self.image_files[index].clone();
        let list_generation = self.directory_tree.state.lock().image_list_generation;
        self.directory_tree_strip_cold_attempted.insert(index);
        self.directory_tree_strip_generate_inflight.insert(index);
        let tx = self.directory_tree_strip_preview_tx.clone();
        let max_side = DIRECTORY_TREE_STRIP_THUMBNAIL_MAX_SIDE;
        DIRECTORY_TREE_STRIP_POOL.spawn(move || {
            crate::preload_debug!(
                "[PreloadDebug][Strip] cold worker start idx={} path={}",
                index,
                path.display()
            );
            #[cfg(target_os = "windows")]
            let com_ok = strip_worker_com_initialized();
            #[cfg(not(target_os = "windows"))]
            let com_ok = true;

            let mut decoded = DecodedImage::new(0, 0, Vec::new());
            let mut logical = (0u32, 0u32);
            if com_ok {
                match generate_directory_tree_thumb_from_path(&path, max_side) {
                    Ok((preview, logical_size)) => {
                        decoded = preview;
                        logical = logical_size;
                    }
                    Err(err) => {
                        log::warn!(
                            "[DirectoryTree] Cold strip preview failed for index {index} ({}): {err}",
                            path.display()
                        );
                    }
                }
            } else {
                log::warn!(
                    "[DirectoryTree] COM init failed for cold strip preview worker index {index}"
                );
            }
            crate::preload_debug!(
                "[PreloadDebug][Strip] cold worker done idx={} out={}x{} logical={}x{} aspect_ok={}",
                index,
                decoded.width,
                decoded.height,
                logical.0,
                logical.1,
                preview_aspect_matches_logical(
                    decoded.width,
                    decoded.height,
                    logical.0,
                    logical.1,
                )
            );
            let job = DirectoryTreeStripPreviewJobResult {
                index,
                path,
                image_list_generation: list_generation,
                decoded,
                logical,
                stage: PreviewStage::Initial,
            };
            if let Err(err) = tx.try_send(job) {
                log::warn!(
                    "[DirectoryTree] Cold strip preview result dropped for index {index}: {err}"
                );
            }
        });
    }

    fn clear_strip_preview_attempt_state(&mut self, index: usize) {
        self.directory_tree_strip_generate_inflight.remove(&index);
        self.directory_tree_strip_tiled_attempted.remove(&index);
        self.directory_tree_strip_cold_attempted.remove(&index);
    }

    fn strip_preview_result_matches_index(
        &self,
        result: &DirectoryTreeStripPreviewJobResult,
    ) -> bool {
        self.image_files.get(result.index) == Some(&result.path)
    }

    fn try_apply_relocated_strip_preview_result(
        &mut self,
        result: DirectoryTreeStripPreviewJobResult,
        ctx: &egui::Context,
    ) -> bool {
        self.clear_strip_preview_attempt_state(result.index);
        let Some(new_index) = self
            .image_files
            .iter()
            .position(|path| path == &result.path)
        else {
            return false;
        };
        self.clear_strip_preview_attempt_state(new_index);

        if result.decoded.width == 0 || result.decoded.height == 0 {
            return false;
        }
        if !decoded_rgba_size_valid(&result.decoded) {
            log::warn!(
                "[DirectoryTree] Relocated strip preview size mismatch for {}: {}x{}",
                result.path.display(),
                result.decoded.width,
                result.decoded.height
            );
            return false;
        }
        if !preview_aspect_matches_logical(
            result.decoded.width,
            result.decoded.height,
            result.logical.0,
            result.logical.1,
        ) {
            log::warn!(
                "[DirectoryTree] Relocated strip preview aspect mismatch for {}: {}x{} vs {}x{}",
                result.path.display(),
                result.decoded.width,
                result.decoded.height,
                result.logical.0,
                result.logical.1
            );
            return false;
        }

        self.cache_directory_tree_strip_thumbnail(
            new_index,
            &result.decoded,
            result.stage,
            Some(result.logical),
            ctx,
        );
        if !self
            .directory_tree_strip_cache
            .is_valid_for_logical(new_index, result.logical)
        {
            self.directory_tree_strip_tiled_attempted.remove(&new_index);
            return false;
        }
        ctx.request_repaint();
        ctx.request_repaint_of(self.directory_tree_repaint_viewport_id());
        true
    }

    pub(crate) fn poll_directory_tree_strip_preview_results(&mut self, ctx: &egui::Context) {
        let active_list_generation = self
            .directory_tree
            .state
            .try_lock()
            .map(|state| state.image_list_generation);
        let Some(active_list_generation) = active_list_generation else {
            return;
        };
        while let Ok(result) = self.directory_tree_strip_preview_rx.try_recv() {
            self.directory_tree_strip_generate_inflight
                .remove(&result.index);
            if result.image_list_generation != active_list_generation {
                #[cfg(feature = "preload-debug")]
                crate::preload_debug!(
                    "[PreloadDebug][DirTree] strip result stale gen idx={} job_gen={} active_gen={}",
                    result.index,
                    result.image_list_generation,
                    active_list_generation
                );
                self.clear_strip_preview_attempt_state(result.index);
                continue;
            }
            if !self.strip_preview_result_matches_index(&result) {
                let _ = self.try_apply_relocated_strip_preview_result(result, ctx);
                continue;
            }
            if result.decoded.width == 0 || result.decoded.height == 0 {
                self.directory_tree_strip_tiled_attempted
                    .remove(&result.index);
                self.directory_tree_strip_cold_attempted
                    .remove(&result.index);
                continue;
            }
            if !decoded_rgba_size_valid(&result.decoded) {
                log::warn!(
                    "[DirectoryTree] Strip preview job size mismatch for index {}: {}x{}",
                    result.index,
                    result.decoded.width,
                    result.decoded.height
                );
                self.directory_tree_strip_tiled_attempted
                    .remove(&result.index);
                self.directory_tree_strip_cold_attempted
                    .remove(&result.index);
                continue;
            }
            if !preview_aspect_matches_logical(
                result.decoded.width,
                result.decoded.height,
                result.logical.0,
                result.logical.1,
            ) {
                log::warn!(
                    "[DirectoryTree] Strip preview job aspect mismatch for index {}: {}x{} vs {}x{}",
                    result.index,
                    result.decoded.width,
                    result.decoded.height,
                    result.logical.0,
                    result.logical.1
                );
                self.directory_tree_strip_tiled_attempted
                    .remove(&result.index);
                self.directory_tree_strip_cold_attempted
                    .remove(&result.index);
                continue;
            }
            self.cache_directory_tree_strip_thumbnail(
                result.index,
                &result.decoded,
                result.stage,
                Some(result.logical),
                ctx,
            );
            if !self
                .directory_tree_strip_cache
                .is_valid_for_logical(result.index, result.logical)
            {
                self.directory_tree_strip_tiled_attempted
                    .remove(&result.index);
            } else {
                ctx.request_repaint();
                ctx.request_repaint_of(self.directory_tree_repaint_viewport_id());
            }
        }
    }

    pub(crate) fn try_generate_directory_tree_strip_from_tiled_source(&mut self, index: usize) {
        if self.directory_tree_strip_tiled_attempted.contains(&index)
            || self.directory_tree_strip_generate_inflight.contains(&index)
        {
            return;
        }
        let Some(source) = self.tiled_sdr_source_for_index(index) else {
            return;
        };
        let logical = (source.width(), source.height());
        if self
            .directory_tree_strip_cache
            .is_valid_for_logical(index, logical)
        {
            return;
        }

        let path = self.image_files.get(index).cloned().unwrap_or_default();
        let list_generation = self.directory_tree.state.lock().image_list_generation;
        self.directory_tree_strip_tiled_attempted.insert(index);
        self.directory_tree_strip_generate_inflight.insert(index);
        let source = Arc::clone(&source);
        let tx = self.directory_tree_strip_preview_tx.clone();
        let max_side = DIRECTORY_TREE_STRIP_THUMBNAIL_MAX_SIDE;
        DIRECTORY_TREE_STRIP_POOL.spawn(move || {
            let mut decoded = DecodedImage::new(0, 0, Vec::new());
            crate::preload_debug!(
                "[PreloadDebug][Strip] worker start idx={} logical={}x{} max_side={}",
                index,
                logical.0,
                logical.1,
                max_side
            );
            #[cfg(target_os = "windows")]
            let com_ok = strip_worker_com_initialized();
            #[cfg(not(target_os = "windows"))]
            let com_ok = true;
            if com_ok {
                let gen_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    source.generate_full_image_preview(max_side, max_side)
                }));
                if let Ok((pw, ph, pixels)) = gen_result {
                    if pw > 0 && ph > 0 {
                        decoded = DecodedImage::new(pw, ph, pixels);
                    }
                }
            } else {
                log::warn!(
                    "[DirectoryTree] COM init failed for strip preview worker index {index}"
                );
            }
            crate::preload_debug!(
                "[PreloadDebug][Strip] worker done idx={} out={}x{} logical={}x{} aspect_ok={}",
                index,
                decoded.width,
                decoded.height,
                logical.0,
                logical.1,
                preview_aspect_matches_logical(decoded.width, decoded.height, logical.0, logical.1,)
            );
            let job = DirectoryTreeStripPreviewJobResult {
                index,
                path,
                image_list_generation: list_generation,
                decoded,
                logical,
                stage: PreviewStage::Refined,
            };
            if let Err(err) = tx.try_send(job) {
                log::warn!("[DirectoryTree] Strip preview result dropped for index {index}: {err}");
            }
        });
    }

    pub(crate) fn ensure_directory_tree_strip_thumbnails(&mut self, ctx: &egui::Context) {
        if self.settings.browse_mode != BrowseMode::Tree || !self.settings.show_directory_tree_nav {
            return;
        }

        self.poll_directory_tree_strip_preview_results(ctx);

        self.directory_tree_strip_cold_attempted.retain(|index| {
            self.directory_tree_strip_cache.contains(*index)
                || self.directory_tree_strip_generate_inflight.contains(index)
        });
        self.directory_tree_strip_tiled_attempted.retain(|index| {
            self.directory_tree_strip_cache.contains(*index)
                || self.directory_tree_strip_generate_inflight.contains(index)
        });

        let mut tiled_indices: Vec<usize> = self.prefetched_tiles.keys().copied().collect();
        if let Some(tm) = &self.tile_manager {
            if !tiled_indices.contains(&tm.image_index) {
                tiled_indices.push(tm.image_index);
            }
        }
        let current = self.current_index;
        let file_count = self.image_files.len();
        let total = file_count.max(1);
        tiled_indices.sort_by_key(|&idx| {
            if idx == current {
                0
            } else {
                let forward = (idx + total - current) % total;
                let backward = (current + total - idx) % total;
                1 + forward.min(backward)
            }
        });

        for index in &tiled_indices {
            let Some(logical) = self.directory_tree_strip_logical_size(*index) else {
                continue;
            };
            if self
                .directory_tree_strip_cache
                .invalidate_if_invalid(*index, logical)
            {
                self.directory_tree_strip_tiled_attempted.remove(index);
            }
            self.try_sync_strip_from_tile_manager_preview(*index);
            self.try_sync_strip_from_texture_cache(*index);
        }

        if file_count > 0 {
            let current = self.current_index.min(file_count - 1);
            self.try_sync_strip_from_texture_cache(current);
            for delta in 1..=DIRECTORY_TREE_COLD_NEIGHBOR_RADIUS {
                if current >= delta {
                    self.try_sync_strip_from_texture_cache(current - delta);
                }
                if current + delta < file_count {
                    self.try_sync_strip_from_texture_cache(current + delta);
                }
            }
        }

        let mut generated_this_frame = 0usize;
        for index in tiled_indices {
            let Some(logical) = self.directory_tree_strip_logical_size(index) else {
                continue;
            };
            if self
                .directory_tree_strip_cache
                .is_valid_for_logical(index, logical)
            {
                continue;
            }
            if generated_this_frame >= MAX_TILED_STRIP_GENERATES_PER_FRAME {
                break;
            }
            self.try_generate_directory_tree_strip_from_tiled_source(index);
            generated_this_frame += 1;
        }

        let deferred_indices: Vec<usize> = self.deferred_sdr_uploads.keys().copied().collect();
        for index in deferred_indices {
            if self.tiled_sdr_source_for_index(index).is_some() {
                continue;
            }
            if self.directory_tree_strip_cache.contains(index) {
                continue;
            }
            if self
                .deferred_sdr_uploads
                .get(&index)
                .is_some_and(crate::loader::decoded_looks_like_black_placeholder)
            {
                continue;
            }
            let Some(decoded) = self.deferred_sdr_uploads.get(&index).cloned() else {
                continue;
            };
            self.cache_directory_tree_strip_thumbnail(
                index,
                &decoded,
                PreviewStage::Initial,
                self.directory_tree_strip_logical_size(index),
                ctx,
            );
        }

        let (visible_row_range, scroll_to_current_pending, defer_sync) = {
            match self.directory_tree.state.try_lock() {
                Some(state) => (
                    state.image_list_visible_row_range,
                    state.scroll_image_list_to_current,
                    false,
                ),
                None => (None, false, true),
            }
        };
        if defer_sync {
            self.defer_directory_tree_file_list_sync();
        }
        let bootstrap_visible = self.directory_tree_strip_bootstrap_after_scan;
        let cold_candidates = self.collect_cold_strip_thumbnail_candidates(
            visible_row_range,
            scroll_to_current_pending,
            bootstrap_visible,
        );
        if bootstrap_visible && visible_row_range.is_some() {
            self.directory_tree_strip_bootstrap_after_scan = false;
        }
        let inflight_room = MAX_STRIP_GENERATE_INFLIGHT
            .saturating_sub(self.directory_tree_strip_generate_inflight.len());
        let mut cold_scheduled = 0usize;
        for index in cold_candidates {
            if cold_scheduled >= MAX_COLD_STRIP_GENERATES_PER_FRAME.min(inflight_room) {
                break;
            }
            self.try_generate_cold_directory_tree_strip_thumbnail(index);
            cold_scheduled += 1;
        }

        #[cfg(feature = "preload-debug")]
        if bootstrap_visible
            || cold_scheduled > 0
            || !self.directory_tree_strip_generate_inflight.is_empty()
        {
            let ui_preview_count = self
                .directory_tree
                .state
                .try_lock()
                .map(|s| s.preview_textures.len())
                .unwrap_or(0);
            crate::preload_debug!(
                "[PreloadDebug][DirTree] ensure_strip current={} rows={} cache={} ui_preview={} rev={} inflight={} cold_sched={} visible={:?} scroll_pending={} bootstrap={}",
                self.current_index,
                self.image_files.len(),
                self.directory_tree_strip_cache.textures().len(),
                ui_preview_count,
                self.directory_tree_strip_cache.gpu_revision(),
                self.directory_tree_strip_generate_inflight.len(),
                cold_scheduled,
                visible_row_range,
                scroll_to_current_pending,
                bootstrap_visible
            );
        }

        self.directory_tree_strip_cache
            .retain(|index| index < self.image_files.len());
        self.directory_tree_strip_tiled_attempted
            .retain(|index| *index < self.image_files.len());
        self.directory_tree_strip_generate_inflight
            .retain(|index| *index < self.image_files.len());
        self.directory_tree_strip_cold_attempted
            .retain(|index| *index < self.image_files.len());
    }

    pub(crate) fn invalidate_directory_tree_strip_gpu_textures(&mut self) {
        self.directory_tree_strip_cache.clear_gpu_textures();
        self.directory_tree_strip_tiled_attempted.clear();
        self.directory_tree_strip_cold_attempted.clear();
    }
}
