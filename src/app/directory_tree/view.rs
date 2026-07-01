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

//! RCU domain snapshots assembled for paint plus frame-local UI chrome.

use std::sync::Arc;

use arc_swap::ArcSwap;
use eframe::egui;

use super::{
    DirectoryTreeFileRow, ImageListSortColumn,
    domains::{
        DirectoryTreeListSnapshot, DirectoryTreeListState, DirectoryTreePreviewSnapshot,
        DirectoryTreeTreeSnapshot, DirectoryTreeTreeState,
    },
};

/// Paint read model: three domain snapshots (tree / list / preview).
pub(crate) struct DirectoryTreeView {
    pub(super) tree: Arc<DirectoryTreeTreeSnapshot>,
    pub(super) list: Arc<DirectoryTreeListSnapshot>,
    pub(super) preview: Arc<DirectoryTreePreviewSnapshot>,
}

impl DirectoryTreeView {
    pub(super) fn assemble(
        tree: Arc<DirectoryTreeTreeSnapshot>,
        list: Arc<DirectoryTreeListSnapshot>,
        preview: Arc<DirectoryTreePreviewSnapshot>,
    ) -> Self {
        Self {
            tree,
            list,
            preview,
        }
    }

    pub(super) fn places_loaded(&self) -> bool {
        self.tree.places_loaded
    }

    pub(super) fn places_loading(&self) -> bool {
        self.tree.places_loading
    }

    pub(super) fn places_load_error(&self) -> Option<&str> {
        self.tree.places_load_error.as_deref()
    }

    pub(super) fn workers_available(&self) -> bool {
        self.tree.workers_available
    }

    pub(super) fn known_folders(&self) -> &[crate::directory_tree_places::KnownFolderEntry] {
        &self.tree.known_folders
    }

    pub(super) fn selected_namespace_path(&self) -> Option<&std::path::PathBuf> {
        self.tree.selected_namespace_path.as_ref()
    }

    /// Mount/share root to paint while Places is still loading (bootstrap reveal chain).
    pub(super) fn pre_places_folder_display_root(&self) -> Option<std::path::PathBuf> {
        if self.places_loaded() {
            return None;
        }
        let selected = self.selected_namespace_path()?;
        let chain = super::namespace::namespace_path_ancestor_chain(selected);
        chain.into_iter().find(|path| {
            self.nodes().contains_key(path)
                && (super::namespace::is_mount_namespace_path(path)
                    || super::namespace::is_network_share_namespace_path(path))
        })
    }

    pub(super) fn nodes(
        &self,
    ) -> &std::collections::HashMap<std::path::PathBuf, Arc<super::DirectoryTreeNode>> {
        &self.tree.nodes
    }

    pub(super) fn network_visible(&self) -> bool {
        self.tree.network_visible
    }

    pub(super) fn left_panel_width(&self) -> f32 {
        self.tree.left_panel_width
    }

    pub(super) fn image_rows(&self) -> &Arc<[DirectoryTreeFileRow]> {
        &self.list.image_rows
    }

    pub(super) fn current_index(&self) -> usize {
        self.list.current_index
    }

    pub(super) fn scanning(&self) -> bool {
        self.list.scanning
    }

    pub(super) fn scan_status(&self) -> &str {
        &self.list.scan_status
    }

    pub(super) fn sync_warning(&self) -> Option<&str> {
        self.list.sync_warning.as_deref()
    }

    pub(super) fn scroll_image_list_to_current(&self) -> bool {
        self.list.scroll_image_list_to_current
    }

    pub(super) fn scroll_folder_tree_to_selected(&self) -> bool {
        self.tree.scroll_folder_tree_to_selected
    }

    pub(super) fn preview_textures(
        &self,
    ) -> &std::collections::HashMap<usize, egui::TextureHandle> {
        &self.preview.textures
    }

    pub(super) fn preview_logical_sizes(&self) -> &std::collections::HashMap<usize, (u32, u32)> {
        &self.preview.logical_sizes
    }

    pub(super) fn image_list_col_size_w(&self) -> f32 {
        self.list.image_list_col_size_w
    }

    pub(super) fn image_list_col_modified_w(&self) -> f32 {
        self.list.image_list_col_modified_w
    }

    pub(super) fn image_list_panel_width(&self) -> f32 {
        self.list.image_list_panel_width
    }

    pub(super) fn image_list_sort_column(&self) -> ImageListSortColumn {
        self.list.image_list_sort_column
    }

    pub(super) fn image_list_sort_ascending(&self) -> bool {
        self.list.image_list_sort_ascending
    }

    pub(super) fn image_list_sort_active(&self) -> bool {
        self.list.image_list_sort_active
    }

    pub(super) fn image_list_reordering(&self) -> bool {
        self.list.image_list_reordering
    }

    pub(super) fn show_list_previews(&self) -> bool {
        self.list.show_list_previews
    }

    pub(super) fn list_preview_thumb_px(&self) -> f32 {
        self.list.list_preview_thumb_px
    }
}

/// Per-frame UI chrome: mutated during paint, merged back into writers after draw.
pub(crate) struct DirectoryTreeUiChrome {
    pub(super) left_panel_width: f32,
    pub(super) panel_layout_dirty: bool,
    pub(super) embedded_nav_panel_width: Option<f32>,
    pub(super) image_list_scroll_offset_y: f32,
    pub(super) image_list_visible_row_range: Option<(usize, usize)>,
    pub(super) image_list_keyboard_active: bool,
    pub(super) current_index: usize,
    pub(super) scroll_image_list_to_current: bool,
    pub(super) folder_scroll_offset_y: f32,
    pub(super) scroll_folder_tree_to_selected: bool,
    pub(super) folder_selected_row_rect: Option<egui::Rect>,
    pub(super) image_list_selected_row_rect: Option<egui::Rect>,
    pub(super) pending_image_context_menu: Option<(egui::Pos2, egui::ViewportId)>,
}

impl DirectoryTreeUiChrome {
    pub(super) fn from_domains(
        tree: &DirectoryTreeTreeState,
        list: &DirectoryTreeListState,
    ) -> Self {
        Self {
            left_panel_width: tree.left_panel_width,
            panel_layout_dirty: false,
            embedded_nav_panel_width: None,
            image_list_scroll_offset_y: list.image_list_scroll_offset_y,
            image_list_visible_row_range: list.image_list_visible_row_range,
            image_list_keyboard_active: list.image_list_keyboard_active,
            current_index: list.current_index,
            scroll_image_list_to_current: list.scroll_image_list_to_current,
            folder_scroll_offset_y: tree.folder_scroll_offset_y,
            scroll_folder_tree_to_selected: tree.scroll_folder_tree_to_selected,
            folder_selected_row_rect: None,
            image_list_selected_row_rect: None,
            pending_image_context_menu: None,
        }
    }

    pub(super) fn begin_image_list_paint(&mut self) {
        self.image_list_selected_row_rect = None;
    }

    pub(super) fn begin_paint_frame(
        &mut self,
        view: &DirectoryTreeView,
        list_keyboard_active: bool,
    ) {
        self.left_panel_width = view.left_panel_width();
        // Keep keyboard-driven list selection in chrome while the RCU snapshot may still
        // reflect the previous main-window index (sync runs on logic(), paint on ui()).
        if !list_keyboard_active {
            self.current_index = view.current_index();
        }
        self.scroll_image_list_to_current = view.scroll_image_list_to_current();
        // folder_scroll_offset_y is frame-local chrome (like image_list_scroll_offset_y):
        // do not reload from the RCU snapshot each frame because scroll changes do not mark
        // tree.snapshot_dirty, so the view would keep resetting the offset to a stale value.
        if view.scroll_folder_tree_to_selected() {
            self.scroll_folder_tree_to_selected = true;
        }
        self.image_list_keyboard_active = list_keyboard_active;
    }

    pub(super) fn apply_to_domains(
        &self,
        tree: &mut DirectoryTreeTreeState,
        list: &mut DirectoryTreeListState,
    ) {
        if tree.left_panel_width != self.left_panel_width {
            tree.left_panel_width = self.left_panel_width;
            tree.mark_snapshot_dirty();
        }
        if self.panel_layout_dirty {
            tree.panel_layout_dirty = true;
            list.panel_layout_dirty = true;
        }
        if let Some(width) = self.embedded_nav_panel_width {
            tree.embedded_nav_panel_width = width;
        }
        list.image_list_scroll_offset_y = self.image_list_scroll_offset_y;
        list.image_list_visible_row_range = self.image_list_visible_row_range;
        list.image_list_keyboard_active = self.image_list_keyboard_active;
        if list.current_index != self.current_index {
            list.current_index = self.current_index;
            list.mark_snapshot_dirty();
        }
        if list.scroll_image_list_to_current != self.scroll_image_list_to_current {
            list.scroll_image_list_to_current = self.scroll_image_list_to_current;
            list.mark_snapshot_dirty();
        }
        tree.folder_scroll_offset_y = self.folder_scroll_offset_y;
        if tree.scroll_folder_tree_to_selected != self.scroll_folder_tree_to_selected {
            tree.scroll_folder_tree_to_selected = self.scroll_folder_tree_to_selected;
            tree.mark_snapshot_dirty();
        }
    }
}

pub(super) fn assemble_directory_tree_view(
    view: &ArcSwap<DirectoryTreeView>,
    tree_snapshot: &ArcSwap<DirectoryTreeTreeSnapshot>,
    list_snapshot: &ArcSwap<DirectoryTreeListSnapshot>,
    preview_snapshot: &ArcSwap<DirectoryTreePreviewSnapshot>,
) {
    view.store(Arc::new(DirectoryTreeView::assemble(
        tree_snapshot.load_full(),
        list_snapshot.load_full(),
        preview_snapshot.load_full(),
    )));
}

pub(super) fn publish_directory_tree_domains(
    runtime: &super::DirectoryTreeRuntime,
    tree: &mut DirectoryTreeTreeState,
    list: &mut DirectoryTreeListState,
    force_list: bool,
    preview_cache_revision: Option<u64>,
    preview_textures: Option<&std::collections::HashMap<usize, egui::TextureHandle>>,
    preview_logical_sizes: Option<&std::collections::HashMap<usize, (u32, u32)>>,
) -> bool {
    let mut last_list_publish_at = runtime.last_list_publish_at.lock();
    let mut ctx = super::domains::DirectoryTreePublishContext {
        tree,
        list,
        tree_snapshot: &runtime.tree_snapshot,
        list_snapshot: &runtime.list_snapshot,
        preview_snapshot: &runtime.preview_snapshot,
        last_list_publish_at: &mut last_list_publish_at,
        force_list,
        preview_cache_revision,
        preview_textures,
        preview_logical_sizes,
    };
    let changed = super::domains::publish_domain_snapshots(&mut ctx);
    if changed {
        assemble_directory_tree_view(
            &runtime.view,
            &runtime.tree_snapshot,
            &runtime.list_snapshot,
            &runtime.preview_snapshot,
        );
    }
    changed
}
