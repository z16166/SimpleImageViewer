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

//! Visible-range checks so off-screen list rows and folder nodes skip repaints.

use std::path::Path;

use super::domains::DirectoryTreeTreeState;
use super::{BOOTSTRAP_STRIP_VISIBLE_ROW_CAP, DirectoryTreeFileRow};
use super::ui::folder_tree_flat_row_index;

/// Max folder rows that can affect the viewport before a visible range is published.
pub(super) const BOOTSTRAP_FOLDER_VISIBLE_ROW_CAP: usize = 32;

/// Whether row indices `[row_start, row_end)` overlap the last published visible range.
pub(super) fn row_range_intersects_visible(
    visible: Option<(usize, usize)>,
    row_start: usize,
    row_end: usize,
    bootstrap_cap: usize,
) -> bool {
    debug_assert!(row_start <= row_end);
    let Some((vis_start, vis_end)) = visible else {
        return row_start < bootstrap_cap;
    };
    row_start < vis_end && vis_start < row_end
}

/// Whether appended image-list rows require a viewport repaint during an active scan.
pub(super) fn appended_image_rows_affect_visible(
    previous_row_count: usize,
    new_row_count: usize,
    visible: Option<(usize, usize)>,
) -> bool {
    if new_row_count <= previous_row_count {
        return new_row_count != previous_row_count;
    }
    if previous_row_count == 0 {
        return true;
    }
    row_range_intersects_visible(
        visible,
        previous_row_count,
        new_row_count,
        BOOTSTRAP_STRIP_VISIBLE_ROW_CAP,
    )
}

/// Whether metadata updates for these list paths can affect visible rows.
pub(super) fn metadata_paths_affect_visible_list(
    paths: &[std::path::PathBuf],
    image_rows: &[DirectoryTreeFileRow],
    visible: Option<(usize, usize)>,
) -> bool {
    let Some((vis_start, vis_end)) = visible else {
        return paths.len() <= BOOTSTRAP_STRIP_VISIBLE_ROW_CAP;
    };
    paths.iter().any(|path| {
        image_rows
            .iter()
            .position(|row| row.path() == path)
            .is_some_and(|index| index >= vis_start && index < vis_end)
    })
}

/// Whether in-flight folder reveal / loading work can affect visible folder rows.
pub(super) fn folder_reveal_work_needs_repaint(tree: &DirectoryTreeTreeState) -> bool {
    if tree.scroll_folder_tree_to_selected {
        return true;
    }
    tree.nodes.iter().any(|(path, node)| {
        node.loading && folder_children_load_affects_visible(tree, path.as_path())
    })
}

/// Whether a folder children load affects the currently visible folder tree rows.
pub(super) fn folder_children_load_affects_visible(
    tree: &DirectoryTreeTreeState,
    loaded_namespace: &Path,
) -> bool {
    if tree.scroll_folder_tree_to_selected {
        return true;
    }
    if !tree.places_loaded {
        return folder_tree_flat_row_index(tree, loaded_namespace)
            .is_none_or(|index| index < BOOTSTRAP_FOLDER_VISIBLE_ROW_CAP);
    }
    let Some(node_index) = folder_tree_flat_row_index(tree, loaded_namespace) else {
        return false;
    };
    row_range_intersects_visible(
        tree.folder_visible_row_range,
        node_index,
        node_index.saturating_add(1),
        BOOTSTRAP_FOLDER_VISIBLE_ROW_CAP,
    )
}
