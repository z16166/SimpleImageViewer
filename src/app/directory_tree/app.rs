// Simple Image Viewer - A high-performance, cross-platform image viewer
// Copyright (C) 2024-2026 Simple Image Viewer Contributors
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use arc_swap::ArcSwap;
use eframe::egui;
use parking_lot::Mutex;
use rust_i18n::t;

use crate::app::ImageViewerApp;
use crate::app::types::CachedWindowPlacement;
use crate::directory_tree_places::DirectoryTreePlaces;
use crate::path_location::is_unc_path;
use crate::settings::{BrowseMode, Settings};

use super::sort::image_list_sort_order;
use super::ui::{
    draw_directory_tree_window, image_list_interaction_enabled, image_list_sorting_available,
    unc_share_root,
};
use super::{
    DIRECTORY_TREE_EMBEDDED_DEFAULT_WIDTH, DIRECTORY_TREE_EMBEDDED_LOADING_PANEL_ID,
    DIRECTORY_TREE_EMBEDDED_MIN_WIDTH, DIRECTORY_TREE_EMBEDDED_SIDE_PANEL_ID,
    DIRECTORY_TREE_LEFT_WIDTH, DIRECTORY_TREE_MIN_HEIGHT, DIRECTORY_TREE_MIN_WIDTH,
    DIRECTORY_TREE_RIGHT_MIN_WIDTH, DIRECTORY_TREE_SPLITTER_GRAB_WIDTH, DIRECTORY_TREE_VIEWPORT_ID,
    DirectoryChildrenRequest, DirectoryTreeCommand, DirectoryTreeListPreviewLayout,
    DirectoryTreeListSnapshot, DirectoryTreeListState, DirectoryTreePreviewSnapshot,
    DirectoryTreeTreeSnapshot, DirectoryTreeTreeState, FileMetadataRequest, ImageListSortColumn,
    domains, is_places_sentinel_path, view,
};

impl ImageViewerApp {
    pub(crate) fn directory_tree_nav_blocks_main_window_wheel(&self, ctx: &egui::Context) -> bool {
        if !self.directory_tree_settings_active() {
            return false;
        }
        let pointer = ctx.input(|i| i.pointer.interact_pos().or(i.pointer.hover_pos()));
        let block_rect = ctx.data(|d| {
            d.get_temp::<egui::Rect>(egui::Id::new(super::DIRECTORY_TREE_NAV_WHEEL_BLOCK_RECT_ID))
        });
        super::ui::pointer_in_directory_tree_nav_block_rect(pointer, block_rect)
    }

    fn send_directory_tree_metadata_request(
        metadata_tx: &crossbeam_channel::Sender<FileMetadataRequest>,
        request: FileMetadataRequest,
    ) {
        if let Err(err) = metadata_tx.send(request) {
            log::warn!("[DirectoryTree] file metadata request dropped: {err}");
        }
    }

    /// Publish an immutable paint snapshot after `logic()` mutates tree/list writers.
    pub(crate) fn publish_directory_tree_view_from_state(&mut self, force_list: bool) {
        let mut tree = self.directory_tree.tree.lock();
        let mut list = self.directory_tree.list.lock();
        let preview_cleared = if self.directory_tree_list_previews_active() {
            false
        } else {
            let had_preview = self.directory_tree.preview_snapshot.load().revision != 0;
            domains::clear_preview_snapshot(&self.directory_tree.preview_snapshot);
            had_preview
        };
        let (preview_cache_revision, preview_textures, preview_logical_sizes) =
            if self.directory_tree_list_previews_active() {
                (
                    Some(self.directory_tree_strip_cache.gpu_revision()),
                    Some(self.directory_tree_strip_cache.textures()),
                    Some(self.directory_tree_strip_cache.logical_sizes()),
                )
            } else {
                (None, None, None)
            };
        let changed = view::publish_directory_tree_domains(
            &self.directory_tree,
            &mut tree,
            &mut list,
            force_list,
            preview_cache_revision,
            preview_textures,
            preview_logical_sizes,
        );
        if preview_cleared && !changed {
            view::assemble_directory_tree_view(
                &self.directory_tree.view,
                &self.directory_tree.tree_snapshot,
                &self.directory_tree.list_snapshot,
                &self.directory_tree.preview_snapshot,
            );
        }
    }

    /// Paint from RCU view + frame chrome; no clone and no structural state lock during draw.
    fn paint_directory_tree_panel(
        ui: &mut egui::Ui,
        view: &Arc<ArcSwap<view::DirectoryTreeView>>,
        chrome: &Arc<Mutex<view::DirectoryTreeUiChrome>>,
        tree: &Arc<Mutex<DirectoryTreeTreeState>>,
        list: &Arc<Mutex<DirectoryTreeListState>>,
        tree_snapshot: &Arc<ArcSwap<DirectoryTreeTreeSnapshot>>,
        list_snapshot: &Arc<ArcSwap<DirectoryTreeListSnapshot>>,
        preview_snapshot: &Arc<ArcSwap<DirectoryTreePreviewSnapshot>>,
        list_preview: DirectoryTreeListPreviewLayout,
        command_tx: &crossbeam_channel::Sender<DirectoryTreeCommand>,
        root_wake: Option<&crate::app::RootRedrawWake>,
        theme: &Arc<parking_lot::Mutex<crate::theme::ThemePalette>>,
        embedded: bool,
        allow_image_context_menu: bool,
    ) -> bool {
        let palette = theme.lock().clone();
        if let Some(mut list_guard) = list.try_lock() {
            let cols_before = (
                list_guard.image_list_col_size_w,
                list_guard.image_list_col_modified_w,
            );
            list_guard.update_image_list_column_widths(ui.ctx());
            if cols_before
                != (
                    list_guard.image_list_col_size_w,
                    list_guard.image_list_col_modified_w,
                )
            {
                if domains::publish_list_snapshot(list_snapshot, &mut list_guard) {
                    view::assemble_directory_tree_view(
                        view,
                        tree_snapshot,
                        list_snapshot,
                        preview_snapshot,
                    );
                }
            }
        }
        let view = view.load();
        let Some(mut chrome_guard) = chrome.try_lock() else {
            return false;
        };
        chrome_guard.begin_paint_frame(&view);
        draw_directory_tree_window(
            ui,
            &view,
            &mut chrome_guard,
            list_preview,
            command_tx,
            root_wake,
            &palette,
            embedded,
            allow_image_context_menu,
        );
        let scanning = view.scanning();
        drop(chrome_guard);
        if let (Some(mut tree_guard), Some(mut list_guard)) = (tree.try_lock(), list.try_lock()) {
            if let Some(chrome_guard) = chrome.try_lock() {
                chrome_guard.apply_to_domains(&mut tree_guard, &mut list_guard);
            }
        }
        scanning
    }

    pub(crate) fn finish_directory_tree_image_list_context_menu(
        &mut self,
        chrome: &Arc<Mutex<view::DirectoryTreeUiChrome>>,
        ctx: &egui::Context,
        embedded: bool,
    ) {
        {
            let mut chrome_guard = chrome.lock();
            if self.active_modal.is_none() {
                if let Some((pos, viewport)) = chrome_guard.pending_image_context_menu.take() {
                    self.context_menu_pos = Some(pos);
                    self.context_menu_viewport = Some(viewport);
                }
            } else {
                chrome_guard.pending_image_context_menu = None;
            }
        }
        if embedded || self.active_modal.is_some() || self.image_files.is_empty() {
            return;
        }
        self.paint_image_context_menu_if_open(ctx);
    }

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
            let mut tree = self.directory_tree.tree.lock();
            if !tree.places_loaded {
                return;
            }
            tree.set_selected_dir(dir.clone());
            let mut requests = tree.reveal_selected_dir();
            if let Some(request) = tree.expand_tree_for_filesystem_dir(&dir) {
                requests.push(request);
            }
            requests
        };
        for request in requests {
            self.send_directory_tree_children_request(request);
        }
    }

    fn send_directory_tree_children_request(&mut self, request: DirectoryChildrenRequest) {
        let tree_path = request.tree_path.clone();
        if let Err(err) = self.directory_tree.children_request_tx.try_send(request) {
            log::warn!(
                "[DirectoryTree] children request dropped for {}: {err}",
                tree_path.display()
            );
            let error = match err {
                crossbeam_channel::TrySendError::Full(_) => {
                    t!("directory_tree.children_request_busy").to_string()
                }
                crossbeam_channel::TrySendError::Disconnected(_) => {
                    t!("directory_tree.workers_unavailable").to_string()
                }
            };
            let mut tree = self.directory_tree.tree.lock();
            tree.mark_children_request_failed(&tree_path, error);
        }
    }

    pub(crate) fn ensure_directory_tree_places_loaded(&mut self) {
        {
            let tree = self.directory_tree.tree.lock();
            if tree.places_loaded {
                return;
            }
            if tree.places_loading {
                return;
            }
        }

        if self.directory_tree_places_load_rx.is_some() {
            return;
        }

        let (tx, rx) = crossbeam_channel::bounded(1);
        self.directory_tree_places_load_rx = Some(rx);
        {
            let mut tree = self.directory_tree.tree.lock();
            tree.places_loading = true;
            tree.places_load_error = None;
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
            let mut tree = self.directory_tree.tree.lock();
            tree.places_loading = false;
            tree.places_load_error = Some(t!("directory_tree.places_load_failed").to_string());
            self.directory_tree_places_load_rx = None;
        }
    }

    fn apply_directory_tree_places(&mut self, places: DirectoryTreePlaces) {
        let saved_dir = if self.settings.browse_mode == BrowseMode::Tree {
            self.saved_directory_tree_selection_dir()
        } else {
            self.settings.last_image_dir.clone()
        };
        let mut tree = self.directory_tree.tree.lock();
        let mut list = self.directory_tree.list.lock();
        tree.places_loading = false;
        tree.places_load_error = None;
        tree.initialize_places(places);
        if saved_dir
            .as_ref()
            .is_some_and(|path| is_unc_path(path.as_path()))
        {
            tree.ensure_network_visible();
            if let Some(share) = saved_dir.as_ref().and_then(|path| unc_share_root(path)) {
                tree.ensure_network_share_mounted(&share);
            }
        }
        if let Some(dir) = saved_dir {
            tree.set_selected_dir(dir);
        }
        self.apply_saved_directory_tree_panel_layout(&mut tree, &mut list);
        drop(tree);
        drop(list);
        self.publish_directory_tree_view_from_state(false);
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
                let mut tree = self.directory_tree.tree.lock();
                tree.places_loading = false;
                tree.places_load_error = Some(t!("directory_tree.places_load_failed").to_string());
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
            let mut tree = runtime.tree.lock();
            tree.set_selected_dir(root.clone());
            let mut requests = tree.reveal_selected_dir();
            if let Some(request) = tree.expand_tree_for_filesystem_dir(&root) {
                requests.push(request);
            }
            requests
        };
        for request in requests {
            self.send_directory_tree_children_request(request);
        }
    }

    pub(crate) fn process_directory_tree_events(&mut self, ctx: &egui::Context) {
        while let Ok(result) = self.directory_tree.result_rx.try_recv() {
            let requests = {
                let mut tree = self.directory_tree.tree.lock();
                tree.apply_children_result(result);
                tree.reveal_selected_dir()
            };
            for request in requests {
                self.send_directory_tree_children_request(request);
            }
            ctx.request_repaint();
            self.request_directory_tree_viewport_repaint(ctx);
        }

        while let Ok(result) = self.directory_tree.metadata_result_rx.try_recv() {
            self.directory_tree
                .list
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
                        let mut tree = self.directory_tree.tree.lock();
                        let mut list = self.directory_tree.list.lock();
                        tree.set_selected_dir(path.clone());
                        list.image_list_keyboard_active = false;
                        if let Some(request) = tree.expand_tree_for_filesystem_dir(&path) {
                            drop(tree);
                            drop(list);
                            self.send_directory_tree_children_request(request);
                        }
                    }
                    self.load_directory(path);
                    self.queue_save();
                    self.wake_root_for_logic();
                    ctx.request_repaint();
                }
                DirectoryTreeCommand::ToggleExpanded(path) => {
                    let request = self.directory_tree.tree.lock().toggle_expanded(&path);
                    if let Some(request) = request {
                        self.send_directory_tree_children_request(request);
                    }
                    ctx.request_repaint();
                }
                DirectoryTreeCommand::SelectImage(index) => {
                    if self
                        .directory_tree
                        .list
                        .try_lock()
                        .is_some_and(|list| !image_list_interaction_enabled(&list))
                    {
                        continue;
                    }
                    if index < self.image_files.len() {
                        self.pending_directory_tree_select_index = Some(index);
                        let mut list = self.directory_tree.list.lock();
                        list.current_index = index;
                        list.scroll_image_list_to_current = true;
                        ctx.request_repaint();
                        self.request_directory_tree_viewport_repaint(ctx);
                    }
                }
                DirectoryTreeCommand::SortImageList(column) => {
                    let (sort_column, sort_ascending) = {
                        let mut list = self.directory_tree.list.lock();
                        if !image_list_sorting_available(&list) {
                            continue;
                        }
                        let ascending = if list.image_list_sort_column == column {
                            !list.image_list_sort_ascending
                        } else {
                            true
                        };
                        list.image_list_sort_column = column;
                        list.image_list_sort_ascending = ascending;
                        list.image_list_sort_active = true;
                        list.image_list_reordering = true;
                        (column, ascending)
                    };

                    let changed =
                        self.apply_directory_tree_image_list_sort(sort_column, sort_ascending);
                    {
                        let mut list = self.directory_tree.list.lock();
                        list.image_list_reordering = false;
                        if changed {
                            list.image_list_generation = list.image_list_generation.wrapping_add(1);
                            list.current_index = self.current_index;
                            list.image_list_col_widths_dirty = true;
                            list.scroll_image_list_to_current = true;
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
        self.publish_directory_tree_view_from_state(false);
    }

    pub(crate) fn directory_tree_settings_active(&self) -> bool {
        self.settings.browse_mode == BrowseMode::Tree && self.settings.show_directory_tree_nav
    }

    fn directory_tree_viewport_active(&self) -> bool {
        if !self.directory_tree_settings_active() {
            return false;
        }
        match self.directory_tree.tree.try_lock() {
            Some(tree) => tree.places_loaded,
            None => self.directory_tree.view.load().places_loaded(),
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
            .list
            .try_lock()
            .is_some_and(|list| list.image_list_keyboard_active)
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
            let mut tree = self.directory_tree.tree.lock();
            let mut list = self.directory_tree.list.lock();
            self.apply_saved_directory_tree_panel_layout(&mut tree, &mut list);
        }
        ctx.request_repaint();
    }

    fn apply_saved_directory_tree_panel_layout(
        &self,
        tree: &mut DirectoryTreeTreeState,
        list: &mut DirectoryTreeListState,
    ) {
        if let Some(width) = self.settings.directory_tree_folder_panel_width {
            tree.left_panel_width = width;
        }
        if let Some(width) = self.settings.directory_tree_image_list_panel_width {
            list.image_list_panel_width = width;
        }
        #[cfg(feature = "preload-debug")]
        crate::preload_debug!(
            "[PreloadDebug][Panel] apply_saved folder={:?} list={:?} embedded={:?} -> left={:.1} list={:.1}",
            self.settings.directory_tree_folder_panel_width,
            self.settings.directory_tree_image_list_panel_width,
            self.settings.directory_tree_embedded_panel_width,
            tree.left_panel_width,
            list.image_list_panel_width
        );
    }

    pub(crate) fn restore_saved_directory_tree_panel_layout(&self) {
        let mut tree = self.directory_tree.tree.lock();
        let mut list = self.directory_tree.list.lock();
        self.apply_saved_directory_tree_panel_layout(&mut tree, &mut list);
        #[cfg(feature = "preload-debug")]
        crate::preload_debug!(
            "[PreloadDebug][Panel] restore_saved at startup folder={:?} list={:?} embedded={:?}",
            self.settings.directory_tree_folder_panel_width,
            self.settings.directory_tree_image_list_panel_width,
            self.settings.directory_tree_embedded_panel_width
        );
    }

    pub(crate) fn persist_directory_tree_layout_to_settings(&mut self) {
        let tree = self.directory_tree.tree.lock();
        let list = self.directory_tree.list.lock();
        #[cfg(feature = "preload-debug")]
        let before_folder = self.settings.directory_tree_folder_panel_width;
        #[cfg(feature = "preload-debug")]
        let before_list = self.settings.directory_tree_image_list_panel_width;
        #[cfg(feature = "preload-debug")]
        let before_embedded = self.settings.directory_tree_embedded_panel_width;
        if tree.left_panel_width > 0.0 {
            self.settings.directory_tree_folder_panel_width = Some(tree.left_panel_width);
        }
        if list.image_list_panel_width > 0.0 {
            self.settings.directory_tree_image_list_panel_width = Some(list.image_list_panel_width);
        }
        if self.directory_tree_nav_is_embedded() && tree.embedded_nav_panel_width > 0.0 {
            self.settings.directory_tree_embedded_panel_width = Some(
                tree.embedded_nav_panel_width
                    .max(DIRECTORY_TREE_EMBEDDED_MIN_WIDTH),
            );
        }
        #[cfg(feature = "preload-debug")]
        crate::preload_debug!(
            "[PreloadDebug][Panel] persist state left={:.1} list={:.1} embedded={:.1} -> settings folder {:?}->{:?} list {:?}->{:?} embedded {:?}->{:?}",
            tree.left_panel_width,
            list.image_list_panel_width,
            tree.embedded_nav_panel_width,
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
        if !self.directory_tree_list_previews_active() {
            let Some(mut list) = self.directory_tree.list.try_lock() else {
                self.defer_directory_tree_file_list_sync();
                return false;
            };
            DirectoryTreeListPreviewLayout::from_settings(&self.settings).apply_to_list(&mut list);
            drop(list);
            let cleared = self.directory_tree.preview_snapshot.load().revision != 0;
            domains::clear_preview_snapshot(&self.directory_tree.preview_snapshot);
            view::assemble_directory_tree_view(
                &self.directory_tree.view,
                &self.directory_tree.tree_snapshot,
                &self.directory_tree.list_snapshot,
                &self.directory_tree.preview_snapshot,
            );
            if cleared {
                ctx.request_repaint_of(self.directory_tree_repaint_viewport_id());
            }
            return cleared;
        }
        let revision = self.directory_tree_strip_cache.gpu_revision();
        let previous_revision = self.directory_tree.preview_snapshot.load().revision;
        #[cfg(feature = "preload-debug")]
        let cache_count = self.directory_tree_strip_cache.textures().len();
        let Some(mut list) = self.directory_tree.list.try_lock() else {
            #[cfg(feature = "preload-debug")]
            crate::preload_debug!(
                "[PreloadDebug][DirTree] sync_preview_textures skipped: list locked cache_rev={} cache_count={}",
                revision,
                cache_count
            );
            self.defer_directory_tree_file_list_sync();
            return false;
        };
        DirectoryTreeListPreviewLayout::from_settings(&self.settings).apply_to_list(&mut list);
        drop(list);
        self.publish_directory_tree_view_from_state(false);
        let updated = revision != previous_revision;
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
            let mut tree = self.directory_tree.tree.lock();
            let mut list = self.directory_tree.list.lock();
            let dirty = tree.panel_layout_dirty || list.panel_layout_dirty;
            tree.panel_layout_dirty = false;
            list.panel_layout_dirty = false;
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
        if placement.maximized || Settings::valid_outer_position(placement.outer_position).is_some()
        {
            self.cached_directory_tree_window_placement = Some(placement);
        }
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
        if let Some(tree) = self.directory_tree.tree.try_lock() {
            if tree.embedded_nav_panel_width > 0.0 {
                return tree.embedded_nav_panel_width;
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
            if let Some(mut list) = self.directory_tree.list.try_lock() {
                list.image_list_keyboard_active = false;
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
        let mut theme = self.directory_tree_theme.lock();
        *theme = self.cached_palette.clone();
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
        if self.directory_tree.tree.try_lock().is_none()
            || self.directory_tree.list.try_lock().is_none()
        {
            return;
        }
        self.pending_directory_tree_state_sync = false;
        self.directory_tree_sync_defer_frames = 0;
        self.sync_directory_tree_file_list_state(ctx);
    }

    pub(crate) fn defer_directory_tree_file_list_sync(&mut self) {
        const MAX_DEFER_FRAMES: u32 = 120;
        self.directory_tree_sync_defer_frames =
            self.directory_tree_sync_defer_frames.saturating_add(1);
        if self.directory_tree_sync_defer_frames > MAX_DEFER_FRAMES {
            log::warn!(
                "[DirectoryTree] Dropping deferred file-list sync after {MAX_DEFER_FRAMES} contended frames"
            );
            let warning = t!("directory_tree.sync_defer_dropped").to_string();
            if let Some(mut list) = self.directory_tree.list.try_lock() {
                list.sync_warning = Some(warning);
            } else {
                self.pending_directory_tree_sync_warning = Some(warning);
            }
            self.pending_directory_tree_state_sync = false;
            self.directory_tree_sync_defer_frames = 0;
            self.mark_directory_tree_repaint_pending();
            return;
        }
        self.pending_directory_tree_state_sync = true;
    }

    /// Sync scan results into the directory-tree file list without registering the viewport.
    /// Safe to call from `logic()` after `process_scan_results`.
    pub(crate) fn sync_directory_tree_file_list_state(&mut self, ctx: &egui::Context) {
        if !self.directory_tree_settings_active() {
            return;
        }

        let viewport_id = self.directory_tree_repaint_viewport_id();
        let mut sync_warning_cleared = false;
        let mut metadata_requests = Vec::new();
        let request_viewport_repaint = {
            let pending_warning = self.pending_directory_tree_sync_warning.take();
            let tree_guard = self.directory_tree.tree.try_lock();
            let list_guard = self.directory_tree.list.try_lock();
            let (Some(tree), Some(mut list)) = (tree_guard, list_guard) else {
                self.pending_directory_tree_sync_warning = pending_warning;
                self.defer_directory_tree_file_list_sync();
                return;
            };
            if let Some(warning) = pending_warning {
                list.sync_warning = Some(warning);
            }
            DirectoryTreeListPreviewLayout::from_settings(&self.settings).apply_to_list(&mut list);
            let previous_preview_revision = self.directory_tree.preview_snapshot.load().revision;
            if !self.directory_tree_list_previews_active()
                && self.directory_tree.preview_snapshot.load().revision != 0
            {
                domains::clear_preview_snapshot(&self.directory_tree.preview_snapshot);
            }
            if !tree.places_loaded {
                (false, false, ImageListSortColumn::default(), true)
            } else {
                let previous_index = list.current_index;
                let previous_scanning = list.scanning;
                let previous_row_count = list.image_rows.len();
                let resort_after_scan =
                    previous_scanning && !self.scanning && list.image_list_sort_active;
                let resort_column = list.image_list_sort_column;
                let resort_ascending = list.image_list_sort_ascending;
                let scan_status = Self::directory_tree_scan_status_message(self);
                if let Some(request) = list.sync_images(
                    &self.image_files,
                    &self.file_byte_len_by_index,
                    &self.file_modified_unix_by_index,
                    self.current_index,
                    self.scanning,
                    scan_status,
                ) {
                    metadata_requests.push(request);
                }
                list.sync_warning = None;
                sync_warning_cleared = true;
                let preview_updated = self.directory_tree_list_previews_active()
                    && self.directory_tree_strip_cache.gpu_revision() != previous_preview_revision;
                let repaint = preview_updated
                    || list.scroll_image_list_to_current
                    || list.current_index != previous_index
                    || list.scanning != previous_scanning
                    || list.image_rows.len() != previous_row_count;
                #[cfg(feature = "preload-debug")]
                if repaint
                    && (list.scanning != previous_scanning
                        || list.image_rows.len() != previous_row_count
                        || preview_updated)
                {
                    crate::preload_debug!(
                        "[PreloadDebug][Scan] directory tree viewport repaint: scanning {} -> {} rows {} -> {} preview_sync={}",
                        previous_scanning,
                        list.scanning,
                        previous_row_count,
                        list.image_rows.len(),
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
            if let (Some(_tree), Some(mut list)) = (
                self.directory_tree.tree.try_lock(),
                self.directory_tree.list.try_lock(),
            ) {
                if let Some(request) = list.sync_images(
                    &self.image_files,
                    &self.file_byte_len_by_index,
                    &self.file_modified_unix_by_index,
                    self.current_index,
                    self.scanning,
                    Self::directory_tree_scan_status_message(self),
                ) {
                    metadata_requests.push(request);
                }
                list.sync_warning = None;
                sync_warning_cleared = true;
                list.image_list_generation = list.image_list_generation.wrapping_add(1);
                list.current_index = self.current_index;
                list.image_list_col_widths_dirty = true;
                list.scroll_image_list_to_current = true;
            }
        }

        for request in metadata_requests {
            Self::send_directory_tree_metadata_request(
                &self.directory_tree.metadata_request_tx,
                request,
            );
        }

        if sync_warning_cleared {
            self.pending_directory_tree_sync_warning = None;
        }

        if request_viewport_repaint.0 {
            ctx.request_repaint_of(viewport_id);
            self.mark_directory_tree_repaint_pending();
        }
        self.publish_directory_tree_view_from_state(request_viewport_repaint.0);
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
        let tree = Arc::clone(&self.directory_tree.tree);
        let list = Arc::clone(&self.directory_tree.list);
        let tree_snapshot = Arc::clone(&self.directory_tree.tree_snapshot);
        let list_snapshot = Arc::clone(&self.directory_tree.list_snapshot);
        let preview_snapshot = Arc::clone(&self.directory_tree.preview_snapshot);
        let view = Arc::clone(&self.directory_tree.view);
        let chrome = Arc::clone(&self.directory_tree.chrome);
        let command_tx = self.directory_tree.command_tx.clone();
        let root_wake = self.root_redraw_wake_handle();
        let theme = std::sync::Arc::clone(&self.directory_tree_theme);
        let viewpaint_app = Arc::clone(&self.directory_tree.viewpaint_app);
        let app_ptr = self as *mut ImageViewerApp;
        viewpaint_app.store(app_ptr, Ordering::Relaxed);
        let inner_size = self.settings.directory_tree_startup_inner_size();
        let outer_position = self.settings.directory_tree_startup_outer_position();
        let startup_maximized = self.settings.directory_tree_window_maximized;
        let list_preview = DirectoryTreeListPreviewLayout::from_settings(&self.settings);
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
                if command_tx.send(DirectoryTreeCommand::CloseWindow).is_err() {
                    log::warn!("[DirectoryTree] CloseWindow command channel disconnected");
                }
                return;
            }

            if startup_maximized {
                if let Some(mut guard) = tree.try_lock()
                    && !guard.detached_startup_maximize_applied
                {
                    ui.ctx()
                        .send_viewport_cmd(egui::ViewportCommand::Maximized(true));
                    guard.detached_startup_maximize_applied = true;
                }
            }

            let scanning = {
                let ptr = viewpaint_app.load(Ordering::Relaxed);
                let allow_image_context_menu = !ptr.is_null()
                    && unsafe { (*ptr).active_modal.is_none() && !(*ptr).image_files.is_empty() };
                let scanning = Self::paint_directory_tree_panel(
                    ui,
                    &view,
                    &chrome,
                    &tree,
                    &list,
                    &tree_snapshot,
                    &list_snapshot,
                    &preview_snapshot,
                    list_preview,
                    &command_tx,
                    root_wake.as_ref(),
                    &theme,
                    false,
                    allow_image_context_menu,
                );
                if !ptr.is_null() {
                    // SAFETY: The pointer is set only for the current UI frame on the UI thread.
                    unsafe {
                        (*ptr).finish_directory_tree_image_list_context_menu(
                            &chrome,
                            ui.ctx(),
                            false,
                        );
                    }
                }
                scanning
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

        let tree = Arc::clone(&self.directory_tree.tree);
        let list = Arc::clone(&self.directory_tree.list);
        let tree_snapshot = Arc::clone(&self.directory_tree.tree_snapshot);
        let list_snapshot = Arc::clone(&self.directory_tree.list_snapshot);
        let preview_snapshot = Arc::clone(&self.directory_tree.preview_snapshot);
        let view = Arc::clone(&self.directory_tree.view);
        let chrome = Arc::clone(&self.directory_tree.chrome);
        let command_tx = self.directory_tree.command_tx.clone();
        let root_wake = self.root_redraw_wake_handle();
        let theme = Arc::clone(&self.directory_tree_theme);
        let list_preview = DirectoryTreeListPreviewLayout::from_settings(&self.settings);
        let default_width = Self::directory_tree_embedded_panel_default_width(&self.settings);
        let has_places = view.load().places_loaded();
        if !has_places {
            egui::Panel::left(DIRECTORY_TREE_EMBEDDED_LOADING_PANEL_ID)
                .resizable(false)
                .show_inside(ui, |ui| {
                    if let Some(guard) = tree.try_lock() {
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
                    crate::app::directory_tree::ui::publish_directory_tree_nav_wheel_block_rect(ui);
                });
            return;
        }

        egui::Panel::left(DIRECTORY_TREE_EMBEDDED_SIDE_PANEL_ID)
            .resizable(true)
            .default_size(default_width)
            .min_size(DIRECTORY_TREE_EMBEDDED_MIN_WIDTH)
            .show_inside(ui, |ui| {
                let allow_image_context_menu =
                    self.active_modal.is_none() && !self.image_files.is_empty();
                if Self::paint_directory_tree_panel(
                    ui,
                    &view,
                    &chrome,
                    &tree,
                    &list,
                    &tree_snapshot,
                    &list_snapshot,
                    &preview_snapshot,
                    list_preview,
                    &command_tx,
                    root_wake.as_ref(),
                    &theme,
                    true,
                    allow_image_context_menu,
                ) && view.load().scanning()
                {
                    ui.ctx().request_repaint();
                }
                self.finish_directory_tree_image_list_context_menu(&chrome, ui.ctx(), true);
            });
    }

    /// Apply a directory-tree list selection queued in `process_directory_tree_events`.
    /// Runs from `logic()` only (never from `ui()`): `navigate_to` may pump the Windows
    /// message loop and must not block on directory-tree writers held by embedded paint.
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

        if self.directory_tree.tree.try_lock().is_none()
            || self.directory_tree.list.try_lock().is_none()
        {
            self.defer_directory_tree_file_list_sync();
        }
        let strip_work_pending = self.scanning
            || self.pending_directory_tree_state_sync
            || self.directory_tree_strip_bootstrap_after_scan
            || !self.directory_tree_strip_generate_inflight.is_empty();
        if strip_work_pending {
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
        }
        self.flush_directory_tree_panel_layout_persist();
        self.publish_directory_tree_view_from_state(false);
    }
}
