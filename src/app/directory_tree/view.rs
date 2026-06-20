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

//! RCU-style read model for directory-tree paint (`ArcSwap`) plus frame-local UI chrome.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use arc_swap::ArcSwap;
use eframe::egui;

use crate::directory_tree_places::KnownFolderEntry;

use super::{DirectoryTreeFileRow, DirectoryTreeNode, DirectoryTreeState, ImageListSortColumn};

/// Immutable tree/list snapshot published from `logic()`; paint reads via `Arc::clone` only.
pub(crate) struct DirectoryTreeView {
    pub(super) places_loaded: bool,
    pub(super) places_loading: bool,
    pub(super) places_load_error: Option<String>,
    pub(super) workers_available: bool,
    pub(super) known_folders: Vec<KnownFolderEntry>,
    pub(super) selected_dir: Option<PathBuf>,
    pub(super) nodes: HashMap<PathBuf, Arc<DirectoryTreeNode>>,
    pub(super) scanning: bool,
    pub(super) scan_status: String,
    pub(super) sync_warning: Option<String>,
    pub(super) image_rows: Arc<[DirectoryTreeFileRow]>,
    pub(super) current_index: usize,
    pub(super) scroll_folder_to_selected: bool,
    pub(super) scroll_image_list_to_current: bool,
    pub(super) preview_textures: HashMap<usize, egui::TextureHandle>,
    pub(super) preview_logical_sizes: HashMap<usize, (u32, u32)>,
    pub(super) image_list_col_size_w: f32,
    pub(super) image_list_col_modified_w: f32,
    pub(super) left_panel_width: f32,
    pub(super) image_list_panel_width: f32,
    pub(super) network_visible: bool,
    pub(super) image_list_sort_column: ImageListSortColumn,
    pub(super) image_list_sort_ascending: bool,
    pub(super) image_list_sort_active: bool,
    pub(super) image_list_reordering: bool,
    pub(super) show_list_previews: bool,
    pub(super) list_preview_thumb_px: f32,
}

impl DirectoryTreeView {
    pub(super) fn from_state(state: &DirectoryTreeState) -> Self {
        Self::build_from_state(state, None)
    }

    fn build_from_state(state: &DirectoryTreeState, previous: Option<&Self>) -> Self {
        let mut nodes = HashMap::with_capacity(state.nodes.len());
        for (path, node) in state.nodes.iter() {
            let arc = previous
                .and_then(|prev| prev.nodes.get(path))
                .filter(|existing| existing.as_ref() == node)
                .cloned()
                .unwrap_or_else(|| Arc::new(node.clone()));
            nodes.insert(path.clone(), arc);
        }

        let image_rows = previous
            .map(|prev| share_image_rows(prev, &state.image_rows))
            .unwrap_or_else(|| Arc::from(state.image_rows.clone().into_boxed_slice()));

        Self {
            places_loaded: state.places_loaded,
            places_loading: state.places_loading,
            places_load_error: state.places_load_error.clone(),
            workers_available: state.workers_available,
            known_folders: state.known_folders.clone(),
            selected_dir: state.selected_dir.clone(),
            nodes,
            scanning: state.scanning,
            scan_status: state.scan_status.clone(),
            sync_warning: state.sync_warning.clone(),
            image_rows,
            current_index: state.current_index,
            scroll_folder_to_selected: state.scroll_folder_to_selected,
            scroll_image_list_to_current: state.scroll_image_list_to_current,
            preview_textures: state.preview_textures.clone(),
            preview_logical_sizes: state.preview_logical_sizes.clone(),
            image_list_col_size_w: state.image_list_col_size_w,
            image_list_col_modified_w: state.image_list_col_modified_w,
            left_panel_width: state.left_panel_width,
            image_list_panel_width: state.image_list_panel_width,
            network_visible: state.network_visible,
            image_list_sort_column: state.image_list_sort_column,
            image_list_sort_ascending: state.image_list_sort_ascending,
            image_list_sort_active: state.image_list_sort_active,
            image_list_reordering: state.image_list_reordering,
            show_list_previews: state.show_list_previews,
            list_preview_thumb_px: state.list_preview_thumb_px,
        }
    }
}

fn share_image_rows(
    previous: &DirectoryTreeView,
    rows: &[DirectoryTreeFileRow],
) -> Arc<[DirectoryTreeFileRow]> {
    let prev = previous.image_rows.as_ref();
    if prev == rows {
        return Arc::clone(&previous.image_rows);
    }
    Arc::from(rows.to_vec().into_boxed_slice())
}

/// Per-frame UI chrome: mutated during paint, merged back into state after draw.
pub(crate) struct DirectoryTreeUiChrome {
    pub(super) left_panel_width: f32,
    pub(super) panel_layout_dirty: bool,
    pub(super) embedded_nav_panel_width: Option<f32>,
    pub(super) scroll_folder_to_selected: bool,
    pub(super) image_list_scroll_offset_y: f32,
    pub(super) image_list_visible_row_range: Option<(usize, usize)>,
    pub(super) image_list_keyboard_active: bool,
    pub(super) current_index: usize,
    pub(super) scroll_image_list_to_current: bool,
    pub(super) image_list_selected_row_rect: Option<egui::Rect>,
    pub(super) pending_image_context_menu: Option<(egui::Pos2, egui::ViewportId)>,
}

impl DirectoryTreeUiChrome {
    pub(super) fn from_state(state: &DirectoryTreeState) -> Self {
        Self {
            left_panel_width: state.left_panel_width,
            panel_layout_dirty: false,
            embedded_nav_panel_width: None,
            scroll_folder_to_selected: state.scroll_folder_to_selected,
            image_list_scroll_offset_y: state.image_list_scroll_offset_y,
            image_list_visible_row_range: state.image_list_visible_row_range,
            image_list_keyboard_active: state.image_list_keyboard_active,
            current_index: state.current_index,
            scroll_image_list_to_current: state.scroll_image_list_to_current,
            image_list_selected_row_rect: None,
            pending_image_context_menu: None,
        }
    }

    pub(super) fn begin_image_list_paint(&mut self) {
        self.image_list_selected_row_rect = None;
    }

    pub(super) fn begin_paint_frame(&mut self, view: &DirectoryTreeView) {
        self.left_panel_width = view.left_panel_width;
        self.scroll_folder_to_selected = view.scroll_folder_to_selected;
        // Selection/scroll flags come from logic (`sync_images`, canvas navigation). Refresh
        // every frame so chrome does not keep a stale index across same-generation updates.
        self.current_index = view.current_index;
        self.scroll_image_list_to_current = view.scroll_image_list_to_current;
    }

    pub(super) fn apply_to_state(&self, state: &mut DirectoryTreeState) {
        state.left_panel_width = self.left_panel_width;
        if self.panel_layout_dirty {
            state.panel_layout_dirty = true;
        }
        if let Some(width) = self.embedded_nav_panel_width {
            state.embedded_nav_panel_width = width;
        }
        state.scroll_folder_to_selected = self.scroll_folder_to_selected;
        state.image_list_scroll_offset_y = self.image_list_scroll_offset_y;
        state.image_list_visible_row_range = self.image_list_visible_row_range;
        state.image_list_keyboard_active = self.image_list_keyboard_active;
        state.current_index = self.current_index;
        state.scroll_image_list_to_current = self.scroll_image_list_to_current;
    }
}

pub(super) fn publish_directory_tree_view(
    view: &ArcSwap<DirectoryTreeView>,
    state: &DirectoryTreeState,
) {
    let previous = view.load();
    view.store(Arc::new(DirectoryTreeView::build_from_state(
        state,
        Some(previous.as_ref()),
    )));
}
