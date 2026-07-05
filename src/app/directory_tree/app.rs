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
use std::time::{Duration, Instant};

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
    DIRECTORY_TREE_EMBEDDED_DEFAULT_WIDTH, DIRECTORY_TREE_EMBEDDED_MIN_WIDTH,
    DIRECTORY_TREE_EMBEDDED_SIDE_PANEL_ID, DIRECTORY_TREE_LEFT_MIN_WIDTH,
    DIRECTORY_TREE_LEFT_WIDTH, DIRECTORY_TREE_MIN_HEIGHT, DIRECTORY_TREE_MIN_WIDTH,
    DIRECTORY_TREE_RIGHT_MIN_WIDTH, DIRECTORY_TREE_SPLITTER_GRAB_WIDTH, DIRECTORY_TREE_VIEWPORT_ID,
    DirectoryChildrenRequest, DirectoryTreeCommand, DirectoryTreeListPreviewLayout,
    DirectoryTreeListSnapshot, DirectoryTreeListState, DirectoryTreePreviewSnapshot,
    DirectoryTreeTreeSnapshot, DirectoryTreeTreeState, ImageListSortColumn, domains,
    embedded_side_panel_clamped_width, is_places_sentinel_namespace_path, view,
};

struct DirectoryTreePanelRefs<'a> {
    view: &'a Arc<ArcSwap<view::DirectoryTreeView>>,
    chrome: &'a Arc<Mutex<view::DirectoryTreeUiChrome>>,
    tree: &'a Arc<Mutex<DirectoryTreeTreeState>>,
    list: &'a Arc<Mutex<DirectoryTreeListState>>,
    tree_snapshot: &'a Arc<ArcSwap<DirectoryTreeTreeSnapshot>>,
    list_snapshot: &'a Arc<ArcSwap<DirectoryTreeListSnapshot>>,
    preview_snapshot: &'a Arc<ArcSwap<DirectoryTreePreviewSnapshot>>,
    command_tx: &'a crossbeam_channel::Sender<DirectoryTreeCommand>,
    root_wake: Option<&'a crate::app::RootRedrawWake>,
    theme: &'a Arc<parking_lot::Mutex<crate::theme::ThemePalette>>,
    embedded: bool,
    allow_image_context_menu: bool,
}

#[cfg_attr(not(feature = "preload-debug"), allow(dead_code))]
struct EmbeddedSidePanelLayoutSample {
    available_before: f32,
    available_after: f32,
    max_rect_width_before: f32,
    panel_width: f32,
    panel_left: f32,
    panel_right: f32,
    default_width: f32,
    min_width: f32,
    tree_embedded_width_before: f32,
    chrome_embedded_width_after: Option<f32>,
}

#[cfg(feature = "preload-debug")]
mod embedded_side_panel_layout_diag {
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};

    use parking_lot::Mutex;

    use super::EmbeddedSidePanelLayoutSample;

    #[derive(Default)]
    struct EmbeddedSidePanelLayoutDiag {
        last_available_before: Option<f32>,
        last_panel_width: Option<f32>,
        last_chrome_embedded_width: Option<f32>,
        last_log_at: Option<Instant>,
    }

    static EMBEDDED_SIDE_PANEL_LAYOUT_DIAG: OnceLock<Mutex<EmbeddedSidePanelLayoutDiag>> =
        OnceLock::new();

    pub(super) fn maybe_log_embedded_side_panel_layout(
        sample: super::EmbeddedSidePanelLayoutSample,
    ) {
        let EmbeddedSidePanelLayoutSample {
            available_before,
            available_after,
            max_rect_width_before,
            panel_width,
            panel_left,
            panel_right,
            default_width,
            min_width,
            tree_embedded_width_before,
            chrome_embedded_width_after,
        } = sample;
        const WIDTH_CHANGE_EPS: f32 = 2.0;
        const LOG_INTERVAL: Duration = Duration::from_millis(1000);

        let diag = EMBEDDED_SIDE_PANEL_LAYOUT_DIAG
            .get_or_init(|| Mutex::new(EmbeddedSidePanelLayoutDiag::default()));
        let Some(mut diag) = diag.try_lock() else {
            return;
        };

        let now = Instant::now();
        let available_delta = diag
            .last_available_before
            .map(|prev| available_before - prev)
            .unwrap_or(0.0);
        let panel_delta = diag
            .last_panel_width
            .map(|prev| panel_width - prev)
            .unwrap_or(0.0);
        let chrome_embedded_delta: f32 = diag
            .last_chrome_embedded_width
            .zip(chrome_embedded_width_after)
            .map(|(prev, now)| now - prev)
            .unwrap_or(0.0);
        let first_sample = diag.last_panel_width.is_none();
        let interval_elapsed = diag.last_log_at.map_or(true, |last| {
            now.saturating_duration_since(last) >= LOG_INTERVAL
        });
        let changed = first_sample
            || available_delta.abs() >= WIDTH_CHANGE_EPS
            || panel_delta.abs() >= WIDTH_CHANGE_EPS
            || chrome_embedded_delta.abs() >= WIDTH_CHANGE_EPS;

        if interval_elapsed && changed {
            log::info!(
                "[PreloadDebug][DirectoryTree][OuterPanelDiag] avail_before={:.1} d_avail={:+.1} avail_after={:.1} \
                 max_rect_before={:.1} panel_w={:.1} d_panel={:+.1} panel_x={:.1}->{:.1} \
                 default={:.1} min={:.1} tree_embedded_before={:.1} chrome_embedded_after={:?} \
                 d_chrome_embedded={:+.1}",
                available_before,
                available_delta,
                available_after,
                max_rect_width_before,
                panel_width,
                panel_delta,
                panel_left,
                panel_right,
                default_width,
                min_width,
                tree_embedded_width_before,
                chrome_embedded_width_after,
                chrome_embedded_delta,
            );
            diag.last_log_at = Some(now);
        }

        diag.last_available_before = Some(available_before);
        diag.last_panel_width = Some(panel_width);
        if let Some(width) = chrome_embedded_width_after {
            diag.last_chrome_embedded_width = Some(width);
        }
    }
}

#[cfg(feature = "preload-debug")]
use embedded_side_panel_layout_diag::maybe_log_embedded_side_panel_layout;

#[cfg(not(feature = "preload-debug"))]
#[inline]
fn maybe_log_embedded_side_panel_layout(_sample: EmbeddedSidePanelLayoutSample) {}

fn embedded_side_panel_stable_rect_before_show(
    ui: &egui::Ui,
    panel_id: egui::Id,
    default_width: f32,
) -> egui::Rect {
    let available = ui.available_rect_before_wrap();
    let width = embedded_side_panel_clamped_width(
        egui::PanelState::load(ui.ctx(), panel_id).map(|state| state.rect.width()),
        default_width,
        available.width(),
    );
    egui::Rect::from_min_max(
        available.min,
        egui::pos2(available.min.x + width, available.max.y),
    )
}

fn restore_embedded_side_panel_state_if_not_resizing(
    ctx: &egui::Context,
    panel_id: egui::Id,
    stable_rect: egui::Rect,
) {
    let resize_active = ctx
        .read_response(panel_id.with("__resize"))
        .is_some_and(|response| response.dragged());
    if !super::should_restore_embedded_side_panel_state(resize_active) {
        return;
    }
    ctx.data_mut(|data| data.insert_persisted(panel_id, egui::PanelState { rect: stable_rect }));
}

impl ImageViewerApp {
    pub(crate) fn directory_tree_nav_blocks_main_window_wheel(&self, ctx: &egui::Context) -> bool {
        if !self.directory_tree_settings_active() || !self.directory_tree_nav_is_embedded() {
            return false;
        }
        let pointer = ctx.input(|i| i.pointer.interact_pos().or(i.pointer.hover_pos()));
        let block_rect = ctx.data(|d| {
            d.get_temp::<egui::Rect>(egui::Id::new(super::DIRECTORY_TREE_NAV_WHEEL_BLOCK_RECT_ID))
        });
        super::ui::pointer_in_directory_tree_nav_block_rect(pointer, block_rect)
    }

    fn directory_tree_list_row_to_file_index(
        &self,
        list: &DirectoryTreeListState,
        row_index: usize,
    ) -> Option<usize> {
        list.image_rows
            .get(row_index)
            .and_then(|row| self.image_files.iter().position(|path| path == &row.path))
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
    fn paint_directory_tree_panel(ui: &mut egui::Ui, refs: DirectoryTreePanelRefs<'_>) -> bool {
        let DirectoryTreePanelRefs {
            view,
            chrome,
            tree,
            list,
            tree_snapshot,
            list_snapshot,
            preview_snapshot,
            command_tx,
            root_wake,
            theme,
            embedded,
            allow_image_context_menu,
        } = refs;
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
                && domains::publish_list_snapshot(list_snapshot, &mut list_guard)
            {
                view::assemble_directory_tree_view(
                    view,
                    tree_snapshot,
                    list_snapshot,
                    preview_snapshot,
                );
            }
        }
        let view_data = view.load();
        let list_keyboard_active = list
            .try_lock()
            .map(|guard| guard.image_list_keyboard_active)
            .unwrap_or(false);
        let Some(mut chrome_guard) = chrome.try_lock() else {
            return false;
        };
        chrome_guard.begin_paint_frame(&view_data, list_keyboard_active);
        draw_directory_tree_window(
            ui,
            super::ui::DirectoryTreeDrawParams {
                view: &view_data,
                chrome: &mut chrome_guard,
                command_tx,
                root_wake,
                palette: &palette,
                embedded,
                allow_image_context_menu,
            },
        );
        let scanning = view_data.scanning();
        drop(chrome_guard);
        if let (Some(mut tree_guard), Some(mut list_guard)) = (tree.try_lock(), list.try_lock())
            && let Some(chrome_guard) = chrome.try_lock()
        {
            chrome_guard.apply_to_domains(&mut tree_guard, &mut list_guard);
            let tree_published = domains::publish_tree_snapshot(tree_snapshot, &mut tree_guard);
            let list_published = domains::publish_list_snapshot(list_snapshot, &mut list_guard);
            if tree_published || list_published {
                view::assemble_directory_tree_view(
                    view,
                    tree_snapshot,
                    list_snapshot,
                    preview_snapshot,
                );
            }
        }
        scanning
    }

    pub(crate) fn finish_directory_tree_image_list_context_menu(
        &mut self,
        ctx: &egui::Context,
        embedded: bool,
    ) {
        {
            let mut chrome_guard = self.directory_tree.chrome.lock();
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
    pub(crate) fn ensure_root_redraw_wake(&mut self, frame: &eframe::Frame, ctx: &egui::Context) {
        if self.root_redraw_wake.is_some() {
            return;
        }
        let ctx = ctx.clone();
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(window) = frame.winit_window() {
            let window = Arc::clone(window);
            let wake = Arc::new(move || {
                // egui routes through the winit UserEvent proxy and wakes WaitUntil;
                // request_redraw alone can stall until the next user input on Windows.
                ctx.request_repaint();
                window.request_redraw();
            }) as crate::app::RootRedrawWake;
            self.root_redraw_wake = Some(Arc::clone(&wake));
            self.loader.set_root_redraw_wake(wake);
            crate::app::tray_handlers::register_tray_logic_wake(Arc::clone(
                self.root_redraw_wake.as_ref().expect("wake just set"),
            ));
            return;
        }
        let wake = Arc::new(move || {
            ctx.request_repaint();
        }) as crate::app::RootRedrawWake;
        self.root_redraw_wake = Some(Arc::clone(&wake));
        self.loader.set_root_redraw_wake(wake);
        crate::app::tray_handlers::register_tray_logic_wake(Arc::clone(
            self.root_redraw_wake.as_ref().expect("wake just set"),
        ));
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
        match self.settings.browse_mode {
            BrowseMode::Tree
                if self.directory_tree_settings_active() || self.auto_hidden_directory_tree_nav =>
            {
                // Tree browsing (including session auto-hide) stays folder-scoped.
                false
            }
            _ => self.settings.recursive,
        }
    }

    pub(crate) fn current_browse_directory(&self) -> Option<PathBuf> {
        if self.directory_tree_settings_active() {
            self.settings
                .tree_nav_selected_dir
                .clone()
                .or_else(|| self.settings.last_image_dir.clone())
        } else {
            self.settings
                .transient_image_dir
                .clone()
                .or_else(|| self.settings.last_image_dir.clone())
        }
    }

    pub(crate) fn auto_hide_directory_tree_nav_for_single_image_open(
        &mut self,
        ctx: &egui::Context,
    ) {
        if self.settings.browse_mode == BrowseMode::Tree && self.settings.show_directory_tree_nav {
            self.hide_detached_directory_tree_nav_viewport(ctx);
            self.auto_hidden_directory_tree_nav = true;
        }
    }

    pub(crate) fn clear_auto_hidden_directory_tree_nav(&mut self) {
        self.auto_hidden_directory_tree_nav = false;
    }

    pub(crate) fn saved_directory_tree_selection_dir(&self) -> Option<PathBuf> {
        self.settings.last_image_dir.clone().or_else(|| {
            self.settings
                .last_viewed_image
                .as_ref()
                .and_then(|path| path.parent().map(|parent| parent.to_path_buf()))
        })
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
            tree.scroll_folder_tree_to_selected = true;
            tree.expand_requests_for_selection(&dir)
        };
        for request in requests {
            self.send_directory_tree_children_request(request);
        }
        self.request_directory_tree_folder_scroll_to_selected();
        self.wake_root_for_logic();
    }

    pub(crate) fn request_directory_tree_folder_scroll_to_selected(&mut self) {
        let mut tree = self.directory_tree.tree.lock();
        tree.scroll_folder_tree_to_selected = true;
        tree.mark_snapshot_dirty();
    }

    /// Point the directory tree at the current browse folder and expand/reveal its path.
    pub(crate) fn ensure_directory_tree_reveals_current_browse_dir(&mut self) {
        if !self.directory_tree_settings_active() {
            return;
        }
        let Some(dir) = self.saved_directory_tree_selection_dir() else {
            return;
        };
        self.settings.tree_nav_selected_dir = Some(dir.clone());
        {
            let mut tree = self.directory_tree.tree.lock();
            let saved_namespace = self.settings.tree_nav_selected_namespace_path.clone();
            let needs_restore = tree.selected_fs_path.as_ref() != Some(&dir)
                || tree.selected_namespace_path.as_deref() != saved_namespace.as_deref();
            if needs_restore {
                tree.restore_tree_selection(dir.clone(), saved_namespace);
            }
        }
        self.reveal_directory_tree_for_saved_selection();
    }

    fn send_directory_tree_children_request(&mut self, request: DirectoryChildrenRequest) {
        let namespace_path = request.namespace_path.clone();
        if let Err(err) = self.directory_tree.try_send_children_request(request) {
            log::warn!(
                "[DirectoryTree] children request dropped for {}: {err}",
                namespace_path.display()
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
            tree.mark_children_request_failed(&namespace_path, error);
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
            tree.places_load_started_at = Some(std::time::Instant::now());
        }

        if std::thread::Builder::new()
            .name("siv-directory-tree-places".to_string())
            .spawn(move || {
                let result = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                    crate::directory_tree_places::load,
                )) {
                    Ok(places) => Ok(places),
                    Err(_) => Err(t!("directory_tree.places_load_panicked").to_string()),
                };
                let _ = tx.send(result);
            })
            .is_err()
        {
            log::error!("[DirectoryTree] Failed to spawn places loader");
            let mut tree = self.directory_tree.tree.lock();
            tree.places_loading = false;
            tree.places_load_started_at = None;
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
        tree.places_load_started_at = None;
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
            let saved_namespace = self.settings.tree_nav_selected_namespace_path.clone();
            tree.restore_tree_selection(dir, saved_namespace);
        }
        self.apply_saved_directory_tree_panel_layout(&mut tree, &mut list);
        drop(tree);
        drop(list);
        self.publish_directory_tree_view_from_state(false);
        self.ensure_directory_tree_reveals_current_browse_dir();
    }

    pub(crate) fn poll_directory_tree_places_load(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.directory_tree_places_load_rx.as_ref() else {
            return;
        };
        if let Ok(result) = rx.try_recv() {
            self.directory_tree_places_load_rx = None;
            match result {
                Ok(places) => self.apply_directory_tree_places(places),
                Err(err) => {
                    log::error!("[DirectoryTree] Places load failed: {err}");
                    let mut tree = self.directory_tree.tree.lock();
                    tree.places_loading = false;
                    tree.places_load_started_at = None;
                    tree.places_load_error =
                        Some(t!("directory_tree.places_load_failed").to_string());
                }
            }
            if self.directory_tree_settings_active() {
                ctx.request_repaint();
                self.request_directory_tree_viewport_repaint(ctx);
            }
            return;
        }

        let timed_out = {
            let tree = self.directory_tree.tree.lock();
            tree.places_loading
                && tree.places_load_started_at.is_some_and(|started| {
                    started.elapsed() > super::DIRECTORY_TREE_PLACES_LOAD_TIMEOUT
                })
        };
        if timed_out {
            log::warn!(
                "[DirectoryTree] Places load timed out after {}s",
                super::DIRECTORY_TREE_PLACES_LOAD_TIMEOUT.as_secs()
            );
            self.directory_tree_places_load_rx = None;
            let mut tree = self.directory_tree.tree.lock();
            tree.places_loading = false;
            tree.places_load_started_at = None;
            tree.places_load_error = Some(t!("directory_tree.places_load_timeout").to_string());
            if self.directory_tree_settings_active() {
                ctx.request_repaint();
                self.request_directory_tree_viewport_repaint(ctx);
            }
        }
    }

    pub(crate) fn apply_directory_tree_image_list_sort(
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
        if let Some(path) = current_path
            && let Some(index) = self.image_files.iter().position(|entry| entry == &path)
        {
            self.set_current_index(index);
        }
        // Re-sort permutes `image_files` and calls `loader.cancel_all()`; restart the current
        // image even when list-preview deferral would skip ordinary scan-time preload.
        self.schedule_current_image_load_if_needed();
        true
    }

    /// Apply persisted list-column sort before main preload so `request_load` targets the file
    /// at `current_index` after reorder (avoids cancel/restart races in `resort_after_scan`).
    pub(crate) fn apply_directory_tree_list_sort_before_preload(&mut self) {
        if self.image_files.len() <= 1 {
            return;
        }
        let (sort_active, column, ascending) = {
            let Some(list) = self.directory_tree.list.try_lock() else {
                return;
            };
            (
                list.image_list_sort_active,
                list.image_list_sort_column,
                list.image_list_sort_ascending,
            )
        };
        if !sort_active {
            return;
        }
        let _ = self.apply_directory_tree_image_list_sort(column, ascending);
    }

    pub(crate) fn initialize_directory_tree_root(&mut self, root: PathBuf) {
        self.activate_directory_tree_nav();
        self.settings.tree_nav_selected_dir = Some(root.clone());
        self.settings.tree_nav_selected_namespace_path = None;
        self.settings
            .set_current_browse_directory(root.clone(), true);

        self.ensure_directory_tree_places_loaded();
        let runtime = &self.directory_tree;
        let requests = {
            let mut tree = runtime.tree.lock();
            tree.set_selected_fs_path(root.clone());
            tree.expand_requests_for_selection(&root)
        };
        for request in requests {
            self.send_directory_tree_children_request(request);
        }
        self.request_directory_tree_folder_scroll_to_selected();
    }

    pub(crate) fn process_directory_tree_events(&mut self, ctx: &egui::Context) {
        while let Ok(result) = self.directory_tree.result_rx.try_recv() {
            let loaded_namespace = result.namespace_path.clone();
            // Capture scroll intent before reveal_selected_namespace(); only re-request scroll when
            // a reveal/show/reveal-in-progress already set the flag (not for unrelated expands).
            let (requests, pending_folder_scroll, folder_repaint) = {
                let mut tree = self.directory_tree.tree.lock();
                tree.apply_children_result(result);
                let pending_folder_scroll = tree.scroll_folder_tree_to_selected;
                let folder_repaint = super::visibility::folder_children_load_affects_visible(
                    &tree,
                    &loaded_namespace,
                );
                let requests = tree.reveal_selected_namespace();
                (requests, pending_folder_scroll, folder_repaint)
            };
            for request in requests {
                self.send_directory_tree_children_request(request);
            }
            if pending_folder_scroll {
                self.request_directory_tree_folder_scroll_to_selected();
            }
            if folder_repaint {
                ctx.request_repaint();
                self.request_directory_tree_viewport_repaint(ctx);
            }
            self.publish_directory_tree_view_from_state(folder_repaint);
            self.wake_root_for_logic();
        }

        while let Ok(result) = self.directory_tree.metadata_result_rx.try_recv() {
            let metadata_repaint = {
                let mut list = self.directory_tree.list.lock();
                let visible = list.image_list_visible_row_range;
                let metadata_repaint = super::visibility::metadata_paths_affect_visible_list(
                    &result.paths,
                    &list.image_rows,
                    visible,
                );
                list.apply_metadata_result(result);
                metadata_repaint
            };
            if metadata_repaint {
                ctx.request_repaint();
                self.request_directory_tree_viewport_repaint(ctx);
            }
            self.publish_directory_tree_view_from_state(metadata_repaint);
        }

        while let Ok(command) = self.directory_tree.command_rx.try_recv() {
            match command {
                DirectoryTreeCommand::SelectDirectory {
                    namespace_path,
                    fs_path,
                } => {
                    if is_places_sentinel_namespace_path(&namespace_path) {
                        continue;
                    }
                    self.activate_directory_tree_nav();
                    self.settings.tree_nav_selected_dir = Some(fs_path.clone());
                    self.settings.tree_nav_selected_namespace_path = Some(namespace_path.clone());
                    {
                        let mut tree = self.directory_tree.tree.lock();
                        let mut list = self.directory_tree.list.lock();
                        tree.set_selected_namespace_node(namespace_path.clone(), fs_path.clone());
                        list.image_list_keyboard_active = false;
                        // Drop stale rows immediately so deferred list sync cannot flash the
                        // previous folder's header before the empty-folder message appears.
                        list.image_rows.clear();
                        list.current_index = 0;
                        list.image_list_scroll_offset_y = 0.0;
                        list.scanning = true;
                        list.scan_status = t!("directory_tree.scanning").to_string();
                        list.mark_snapshot_dirty();
                        if let Some(request) = tree.expand_namespace_node(&namespace_path) {
                            drop(tree);
                            drop(list);
                            self.send_directory_tree_children_request(request);
                        }
                    }
                    self.load_directory(fs_path);
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
                DirectoryTreeCommand::SelectImage(row_index) => {
                    if self
                        .directory_tree
                        .list
                        .try_lock()
                        .is_some_and(|list| !image_list_interaction_enabled(&list))
                    {
                        continue;
                    }
                    let mut list = self.directory_tree.list.lock();
                    let Some(file_index) =
                        self.directory_tree_list_row_to_file_index(&list, row_index)
                    else {
                        continue;
                    };
                    self.pending_directory_tree_select_index = Some(file_index);
                    list.current_index = row_index;
                    list.scroll_image_list_to_current = true;
                    list.mark_snapshot_dirty();
                    ctx.request_repaint();
                    self.request_directory_tree_viewport_repaint(ctx);
                }
                DirectoryTreeCommand::SelectImageAndHideNav(row_index) => {
                    if self
                        .directory_tree
                        .list
                        .try_lock()
                        .is_some_and(|list| !image_list_interaction_enabled(&list))
                    {
                        continue;
                    }
                    let mut list = self.directory_tree.list.lock();
                    let Some(file_index) =
                        self.directory_tree_list_row_to_file_index(&list, row_index)
                    else {
                        continue;
                    };
                    if file_index != self.current_index {
                        self.pending_directory_tree_select_index = Some(file_index);
                    }
                    list.current_index = row_index;
                    list.scroll_image_list_to_current = true;
                    drop(list);
                    // Session-only hide: keep show_directory_tree_nav persisted so Ctrl+T /
                    // Settings can restore the panel without rewriting yaml.
                    self.auto_hide_directory_tree_nav_for_single_image_open(ctx);
                    ctx.request_repaint();
                    self.request_directory_tree_viewport_repaint(ctx);
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
                    self.hide_directory_tree_nav(ctx);
                    self.queue_save();
                    ctx.request_repaint();
                }
            }
        }
        self.publish_directory_tree_view_from_state(false);
    }

    pub(crate) fn directory_tree_settings_active(&self) -> bool {
        self.settings.directory_tree_nav_active() && !self.auto_hidden_directory_tree_nav
    }

    /// Temporarily hide directory-tree navigation (Settings toggle off, Ctrl+T, close nav window).
    /// Keeps `browse_mode` and persisted tree root/selection so the panel can be restored in place.
    pub(crate) fn hide_directory_tree_nav(&mut self, ctx: &egui::Context) {
        self.clear_auto_hidden_directory_tree_nav();
        self.hide_detached_directory_tree_nav_viewport(ctx);
        self.settings.show_directory_tree_nav = false;
    }

    /// Show directory-tree navigation. Recursive scan stays stored but is ignored while visible.
    pub(crate) fn activate_directory_tree_nav(&mut self) {
        self.clear_auto_hidden_directory_tree_nav();
        self.settings.browse_mode = BrowseMode::Tree;
        self.settings.show_directory_tree_nav = true;
    }

    /// Re-enable directory-tree navigation and reveal/scroll to the current browse folder.
    pub(crate) fn show_directory_tree_nav(&mut self, ctx: &egui::Context) {
        self.activate_directory_tree_nav();
        self.ensure_directory_tree_places_loaded();
        self.ensure_directory_tree_reveals_current_browse_dir();
        self.show_detached_directory_tree_viewport_if_active(ctx);
        ctx.request_repaint();
        self.request_directory_tree_viewport_repaint(ctx);
    }

    pub(crate) fn toggle_directory_tree_nav_visibility(&mut self, ctx: &egui::Context) {
        if self.directory_tree_settings_active() {
            self.hide_directory_tree_nav(ctx);
        } else {
            self.show_directory_tree_nav(ctx);
        }
        self.queue_save();
    }

    fn directory_tree_viewport_active(&self) -> bool {
        self.directory_tree_settings_active()
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
            self.directory_tree_viewport_title_sent = false;
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
            tree.left_panel_width = width.clamp(DIRECTORY_TREE_LEFT_MIN_WIDTH, f32::MAX);
        }
        if let Some(width) = self.settings.directory_tree_image_list_panel_width {
            list.image_list_panel_width = width.max(DIRECTORY_TREE_RIGHT_MIN_WIDTH);
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

    pub(crate) fn persist_directory_tree_layout_to_settings(
        &mut self,
        persist_embedded_width: bool,
    ) {
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
        if persist_embedded_width
            && self.directory_tree_nav_is_embedded()
            && tree.embedded_nav_panel_width > 0.0
        {
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
        #[cfg(feature = "preload-debug")]
        if updated {
            let snap = self.directory_tree.preview_snapshot.load();
            crate::preload_debug!(
                "[PreloadDebug][DirTree] sync_preview rev {previous_revision} -> {revision} \
                 cache_count={cache_count} ui_preview={} snap_has_indices={:?}",
                snap.textures.len(),
                snap.textures.keys().copied().collect::<Vec<_>>()
            );
        }
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
            self.persist_directory_tree_layout_to_settings(false);
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

    fn read_detached_directory_tree_viewport_placement(
        ctx: &egui::Context,
    ) -> Option<CachedWindowPlacement> {
        ctx.viewport_for(Self::directory_tree_viewport_id(), |viewport| {
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
        })
    }

    fn detached_directory_tree_viewport_exists(ctx: &egui::Context) -> bool {
        ctx.input(|i| {
            i.raw
                .viewports
                .contains_key(&Self::directory_tree_viewport_id())
        })
    }

    pub(crate) fn snapshot_and_persist_detached_directory_tree_placement(
        &mut self,
        ctx: &egui::Context,
    ) {
        if !self.directory_tree_nav_is_detached() {
            return;
        }
        let Some(placement) = Self::read_detached_directory_tree_viewport_placement(ctx) else {
            return;
        };
        self.apply_cached_directory_tree_viewport_placement(placement, true);
    }

    fn apply_cached_directory_tree_viewport_placement(
        &mut self,
        placement: CachedWindowPlacement,
        persist_to_settings: bool,
    ) {
        if !placement.maximized
            && Settings::valid_outer_position(placement.outer_position).is_some()
        {
            self.cached_directory_tree_restore_placement = Some(placement);
        }
        if placement.maximized || Settings::valid_outer_position(placement.outer_position).is_some()
        {
            self.cached_directory_tree_window_placement = Some(placement);
            if persist_to_settings {
                Self::persist_directory_tree_window_placement_to_settings(
                    &mut self.settings,
                    placement,
                    self.cached_directory_tree_restore_placement,
                );
            }
        }
    }

    pub(crate) fn hide_detached_directory_tree_nav_viewport(&mut self, ctx: &egui::Context) {
        if !self.directory_tree_nav_is_detached() {
            return;
        }
        if !Self::detached_directory_tree_viewport_exists(ctx) {
            return;
        }
        self.snapshot_and_persist_detached_directory_tree_placement(ctx);
        ctx.send_viewport_cmd_to(
            Self::directory_tree_viewport_id(),
            egui::ViewportCommand::Visible(false),
        );
    }

    pub(crate) fn cache_directory_tree_viewport_placement(&mut self, ctx: &egui::Context) {
        if !self.directory_tree_settings_active() || !self.directory_tree_nav_is_detached() {
            return;
        }
        let Some(placement) = Self::read_detached_directory_tree_viewport_placement(ctx) else {
            return;
        };
        self.apply_cached_directory_tree_viewport_placement(placement, false);
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
        if let Some(tree) = self.directory_tree.tree.try_lock()
            && tree.embedded_nav_panel_width > 0.0
        {
            return tree.embedded_nav_panel_width;
        }
        Self::directory_tree_embedded_panel_default_width(&self.settings)
    }

    /// Target embedded side-panel width from yaml (clamped to the current viewport).
    pub(crate) fn embedded_nav_panel_clamped_target_width(&self, available_width: f32) -> f32 {
        embedded_side_panel_clamped_width(
            self.settings.directory_tree_embedded_panel_width,
            Self::directory_tree_embedded_panel_default_width(&self.settings),
            available_width,
        )
    }

    /// One-shot seed of egui [`PanelState`] so the main canvas reserves final nav width on
    /// frame 1 while embedded nav is visible.
    ///
    /// Early return does **not** set [`crate::app::ImageViewerApp::embedded_directory_tree_panel_bootstrapped`];
    /// bootstrap is retried on every ROOT `ui()` pass until embedded nav is active, then runs once.
    ///
    /// Deferred cases (flag stays false until nav becomes visible):
    /// - [`crate::app::ImageViewerApp::auto_hidden_directory_tree_nav`] (CLI / double-click session hide)
    /// - [`DirectoryTreeNavStyle::Detached`](crate::settings::DirectoryTreeNavStyle::Detached)
    /// - `show_directory_tree_nav == false` or `browse_mode != Tree`
    pub(crate) fn bootstrap_embedded_directory_tree_panel_layout(
        &mut self,
        ctx: &egui::Context,
        available: egui::Rect,
    ) {
        if self.embedded_directory_tree_panel_bootstrapped
            || !self.directory_tree_settings_active()
            || !self.directory_tree_nav_is_embedded()
        {
            return;
        }
        self.embedded_directory_tree_panel_bootstrapped = true;
        let width = self.embedded_nav_panel_clamped_target_width(available.width());
        super::seed_embedded_side_panel_states(ctx, available, width);
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
        if !self.directory_tree_viewport_active() {
            return;
        }
        if self.directory_tree_nav_is_detached() {
            if Self::root_viewport_has_os_focus(ctx)
                && !Self::directory_tree_viewport_has_os_focus(ctx)
            {
                self.release_directory_tree_list_keyboard_capture();
            }
            return;
        }
        if self.directory_tree_nav_is_embedded()
            && Self::root_viewport_has_os_focus(ctx)
            && self.pointer_clicked_main_canvas(ctx)
        {
            self.release_directory_tree_list_keyboard_capture();
        }
    }

    fn pointer_clicked_main_canvas(&self, ctx: &egui::Context) -> bool {
        let Some(rect) = self.last_canvas_rect else {
            return false;
        };
        ctx.input(|input| {
            input.pointer.button_clicked(egui::PointerButton::Primary)
                && input
                    .pointer
                    .interact_pos()
                    .is_some_and(|pos| rect.contains(pos))
        })
    }

    pub(crate) fn release_directory_tree_list_keyboard_capture(&mut self) {
        if !self.directory_tree_settings_active() {
            return;
        }
        if let Some(mut list) = self.directory_tree.list.try_lock() {
            list.image_list_keyboard_active = false;
        }
        if let Some(mut chrome) = self.directory_tree.chrome.try_lock() {
            chrome.image_list_keyboard_active = false;
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

    pub(crate) fn hide_detached_directory_tree_viewport_if_active(&self, ctx: &egui::Context) {
        if self.directory_tree_viewport_active() && self.directory_tree_nav_is_detached() {
            ctx.send_viewport_cmd_to(
                Self::directory_tree_viewport_id(),
                egui::ViewportCommand::Visible(false),
            );
        }
    }

    pub(crate) fn show_detached_directory_tree_viewport_if_active(&self, ctx: &egui::Context) {
        if self.directory_tree_viewport_active() && self.directory_tree_nav_is_detached() {
            ctx.send_viewport_cmd_to(
                Self::directory_tree_viewport_id(),
                egui::ViewportCommand::Visible(true),
            );
            self.request_directory_tree_viewport_repaint(ctx);
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
        self.directory_tree_sync_defer_frames =
            self.directory_tree_sync_defer_frames.saturating_add(1);
        if self.directory_tree_sync_defer_frames > super::DIRECTORY_TREE_SYNC_MAX_DEFER_FRAMES {
            log::warn!(
                "[DirectoryTree] Dropping deferred file-list sync after {} contended frames",
                super::DIRECTORY_TREE_SYNC_MAX_DEFER_FRAMES
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

    fn sync_directory_tree_list_images(
        &self,
        list: &mut super::domains::DirectoryTreeListState,
    ) -> Option<super::FileMetadataRequest> {
        list.sync_images(
            &self.image_files,
            &self.file_byte_len_by_index,
            &self.file_modified_unix_by_index,
            self.current_index,
            self.scanning,
            Self::directory_tree_scan_status_message(self),
        )
    }

    /// Sync scan results into the directory-tree file list without registering the viewport.
    /// Safe to call from `logic()` after `process_scan_results`.
    pub(crate) fn sync_directory_tree_file_list_state(&mut self, ctx: &egui::Context) {
        if !self.directory_tree_settings_active() {
            return;
        }

        let viewport_id = self.directory_tree_repaint_viewport_id();
        let mut metadata_requests = Vec::new();
        let request_viewport_repaint = {
            let pending_warning = self.pending_directory_tree_sync_warning.take();
            let tree_guard = self.directory_tree.tree.try_lock();
            let list_guard = self.directory_tree.list.try_lock();
            let (Some(_tree), Some(mut list)) = (tree_guard, list_guard) else {
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
            let previous_index = list.current_index;
            let previous_scanning = list.scanning;
            let previous_row_count = list.image_rows.len();
            let visible_row_range = list.image_list_visible_row_range;
            let resort_after_scan =
                previous_scanning && !self.scanning && list.image_list_sort_active;
            let resort_column = list.image_list_sort_column;
            let resort_ascending = list.image_list_sort_ascending;
            if let Some(request) = self.sync_directory_tree_list_images(&mut list) {
                metadata_requests.push(request);
            }
            list.sync_warning = None;
            let preview_updated = self.directory_tree_list_previews_active()
                && self.directory_tree_strip_cache.gpu_revision() != previous_preview_revision;
            let row_count_changed = list.image_rows.len() != previous_row_count;
            let rows_affect_visible = row_count_changed
                && (list.scanning != previous_scanning
                    || !list.scanning
                    || super::visibility::appended_image_rows_affect_visible(
                        previous_row_count,
                        list.image_rows.len(),
                        visible_row_range,
                    ));
            let repaint = preview_updated
                || list.scroll_image_list_to_current
                || list.current_index != previous_index
                || list.scanning != previous_scanning
                || rows_affect_visible;
            #[cfg(feature = "preload-debug")]
            if repaint
                && (list.scanning != previous_scanning || row_count_changed || preview_updated)
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
        };

        if request_viewport_repaint.1
            && self.apply_directory_tree_image_list_sort(
                request_viewport_repaint.2,
                request_viewport_repaint.3,
            )
            && let (Some(_tree), Some(mut list)) = (
                self.directory_tree.tree.try_lock(),
                self.directory_tree.list.try_lock(),
            )
        {
            if let Some(request) = self.sync_directory_tree_list_images(&mut list) {
                metadata_requests.push(request);
            }
            list.sync_warning = None;
            list.image_list_generation = list.image_list_generation.wrapping_add(1);
            list.current_index = self.current_index;
            list.image_list_col_widths_dirty = true;
            list.scroll_image_list_to_current = true;
        }

        for request in metadata_requests {
            if !self.directory_tree.try_send_metadata_request(request)
                && let Some(mut list) = self.directory_tree.list.try_lock()
            {
                list.sync_warning = Some(t!("directory_tree.metadata_request_busy").to_string());
            }
        }

        self.pending_directory_tree_sync_warning = None;

        if request_viewport_repaint.0 {
            ctx.request_repaint_of(viewport_id);
            self.mark_directory_tree_repaint_pending();
        }
        self.publish_directory_tree_view_from_state(request_viewport_repaint.0);
        if request_viewport_repaint.0 && self.directory_tree_viewport_active() {
            // Keep ROOT painting while the tree viewport is open. logic() may run on a child
            // repaint; egui repaint requests alone do not wake ROOT on Windows.
            self.wake_root_for_logic();
        }
    }

    /// Register the detached directory-tree viewport (draw only; state is synced from `logic()`).
    pub(crate) fn prepare_directory_tree_file_list_viewport(&mut self, ctx: &egui::Context) {
        if self.hidden_to_tray {
            return;
        }
        if !self.directory_tree_viewport_active() || !self.directory_tree_nav_is_detached() {
            return;
        }

        let viewport_id = Self::directory_tree_viewport_id();
        if Self::detached_directory_tree_viewport_exists(ctx) {
            ctx.send_viewport_cmd_to(viewport_id, egui::ViewportCommand::Visible(true));
        }

        let viewpaint_app = Arc::clone(&self.directory_tree.viewpaint_app);
        viewpaint_app.store(self as *mut ImageViewerApp, Ordering::Release);
        let command_tx = self.directory_tree.command_tx.clone();
        let inner_size = self.settings.directory_tree_startup_inner_size();
        let outer_position = self.settings.directory_tree_startup_outer_position();
        let startup_maximized = self.settings.directory_tree_window_maximized;
        let mut builder = egui::ViewportBuilder::default()
            .with_inner_size(inner_size)
            .with_min_inner_size([DIRECTORY_TREE_MIN_WIDTH, DIRECTORY_TREE_MIN_HEIGHT])
            .with_resizable(true)
            .with_close_button(true)
            .with_maximized(false);
        if !self.directory_tree_viewport_title_sent {
            builder = builder.with_title(self.cached_directory_tree_viewport_title.clone());
            self.directory_tree_viewport_title_sent = true;
        }
        let apply_startup_position = !Self::detached_directory_tree_viewport_exists(ctx);
        if apply_startup_position && let Some(pos) = outer_position {
            builder = builder.with_position(pos);
        }

        ctx.show_viewport_deferred(viewport_id, builder, move |ui, _class| {
            if ui.ctx().input(|i| i.viewport().close_requested()) {
                if command_tx
                    .try_send(DirectoryTreeCommand::CloseWindow)
                    .is_err()
                {
                    log::warn!("[DirectoryTree] CloseWindow command channel disconnected");
                }
                return;
            }

            let ptr = viewpaint_app.load(Ordering::Acquire);
            if ptr.is_null() {
                return;
            }

            // SAFETY: see `DirectoryTreeRuntime::viewpaint_app` safety contract.
            let app = unsafe { &mut *ptr };
            app.handle_cross_viewport_hotkeys(ui.ctx());

            if startup_maximized
                && let Some(mut guard) = app.directory_tree.tree.try_lock()
                && !guard.detached_startup_maximize_applied
            {
                ui.ctx()
                    .send_viewport_cmd(egui::ViewportCommand::Maximized(true));
                guard.detached_startup_maximize_applied = true;
            }

            app.flush_directory_tree_strip_pending_gpu_uploads(ui.ctx());
            let allow_image_context_menu =
                app.active_modal.is_none() && !app.image_files.is_empty();
            let scanning = Self::paint_directory_tree_panel(
                ui,
                DirectoryTreePanelRefs {
                    view: &app.directory_tree.view,
                    chrome: &app.directory_tree.chrome,
                    tree: &app.directory_tree.tree,
                    list: &app.directory_tree.list,
                    tree_snapshot: &app.directory_tree.tree_snapshot,
                    list_snapshot: &app.directory_tree.list_snapshot,
                    preview_snapshot: &app.directory_tree.preview_snapshot,
                    command_tx: &app.directory_tree.command_tx,
                    root_wake: app.root_redraw_wake_handle().as_ref(),
                    theme: &app.directory_tree_theme,
                    embedded: false,
                    allow_image_context_menu,
                },
            );
            app.finish_directory_tree_image_list_context_menu(ui.ctx(), false);
            if scanning {
                if let Some(wake) = app.root_redraw_wake_handle() {
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
        self.flush_directory_tree_strip_pending_gpu_uploads(ui.ctx());

        let default_width = Self::directory_tree_embedded_panel_default_width(&self.settings);
        let panel_frame = super::ui::embedded_directory_tree_panel_frame(&self.cached_palette);
        let available_before = ui.available_width();
        let max_rect_width_before = ui.max_rect().width();
        let panel_id = egui::Id::new(DIRECTORY_TREE_EMBEDDED_SIDE_PANEL_ID);
        let stable_panel_rect =
            embedded_side_panel_stable_rect_before_show(ui, panel_id, default_width);
        let tree_embedded_width_before = self
            .directory_tree
            .tree
            .try_lock()
            .map(|tree| tree.embedded_nav_panel_width)
            .unwrap_or(0.0);
        let panel_response = egui::Panel::left(DIRECTORY_TREE_EMBEDDED_SIDE_PANEL_ID)
            .resizable(true)
            .frame(panel_frame)
            .default_size(default_width)
            .min_size(DIRECTORY_TREE_EMBEDDED_MIN_WIDTH)
            .show_inside(ui, |ui| {
                let allow_image_context_menu =
                    self.active_modal.is_none() && !self.image_files.is_empty();
                if Self::paint_directory_tree_panel(
                    ui,
                    DirectoryTreePanelRefs {
                        view: &self.directory_tree.view,
                        chrome: &self.directory_tree.chrome,
                        tree: &self.directory_tree.tree,
                        list: &self.directory_tree.list,
                        tree_snapshot: &self.directory_tree.tree_snapshot,
                        list_snapshot: &self.directory_tree.list_snapshot,
                        preview_snapshot: &self.directory_tree.preview_snapshot,
                        command_tx: &self.directory_tree.command_tx,
                        root_wake: self.root_redraw_wake_handle().as_ref(),
                        theme: &self.directory_tree_theme,
                        embedded: true,
                        allow_image_context_menu,
                    },
                ) && self.directory_tree.view.load().scanning()
                {
                    ui.ctx().request_repaint();
                }
                self.finish_directory_tree_image_list_context_menu(ui.ctx(), true);
            });
        restore_embedded_side_panel_state_if_not_resizing(ui.ctx(), panel_id, stable_panel_rect);
        let panel_rect = panel_response.response.rect;
        let chrome_embedded_width_after = self
            .directory_tree
            .chrome
            .try_lock()
            .and_then(|chrome| chrome.embedded_nav_panel_width);
        maybe_log_embedded_side_panel_layout(EmbeddedSidePanelLayoutSample {
            available_before,
            available_after: ui.available_width(),
            max_rect_width_before,
            panel_width: panel_rect.width(),
            panel_left: panel_rect.left(),
            panel_right: panel_rect.right(),
            default_width,
            min_width: DIRECTORY_TREE_EMBEDDED_MIN_WIDTH,
            tree_embedded_width_before,
            chrome_embedded_width_after,
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
        if self.directory_tree_settings_active() {
            self.poll_directory_tree_places_load(ctx);
        }
        let was_scanning = self.scanning;
        self.process_scan_results();
        self.process_directory_tree_events(ctx);
        self.process_scan_results();
        if was_scanning && !self.scanning {
            self.ensure_directory_tree_reveals_current_browse_dir();
        }
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

        self.poll_directory_tree_places_load(ctx);
        self.ensure_directory_tree_places_loaded();

        const PENDING_PRELOAD_DEFER_RETRY_INTERVAL: Duration = Duration::from_millis(500);

        if self.pending_preload_after_directory_scan {
            let now = Instant::now();
            let due = self
                .pending_preload_after_scan_last_attempt
                .map(|last| now.duration_since(last) >= PENDING_PRELOAD_DEFER_RETRY_INTERVAL)
                .unwrap_or(true);
            if due {
                let defer_active = self.preload_deferred_for_hdr_capacity;
                self.pending_preload_after_directory_scan = false;
                self.pending_preload_after_scan_last_attempt = Some(now);
                self.schedule_preloads(true);
                if defer_active && self.preload_deferred_for_hdr_capacity {
                    self.pending_preload_after_directory_scan = true;
                }
            }
        }

        self.ensure_directory_tree_strip_thumbnails(ctx);
        self.sync_directory_tree_preview_textures_to_state(ctx);

        if self.directory_tree.tree.try_lock().is_none()
            || self.directory_tree.list.try_lock().is_none()
        {
            self.defer_directory_tree_file_list_sync();
        }
        let folder_reveal_repaint = self
            .directory_tree
            .tree
            .try_lock()
            .is_some_and(|tree| super::visibility::folder_reveal_work_needs_repaint(&tree));
        let strip_work_pending = self.pending_directory_tree_state_sync
            || self.directory_tree_strip_bootstrap_after_scan
            || !self.directory_tree_strip_generate_inflight.is_empty()
            || !self.directory_tree_strip_pending_gpu_initial.is_empty()
            || !self.directory_tree_strip_pending_gpu_refined.is_empty()
            || folder_reveal_repaint;
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
