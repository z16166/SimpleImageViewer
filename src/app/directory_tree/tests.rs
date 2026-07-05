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

use super::BOOTSTRAP_STRIP_VISIBLE_ROW_CAP;
use super::sort::{
    compare_image_list_sort_keys, compare_optional_unix_time, image_list_sort_order,
};
use super::ui::{
    DirectoryTreeNodeIcon, apply_directory_tree_splitter_frame_delta,
    clamp_directory_tree_left_panel_width, directory_ancestor_chain, directory_display_name,
    directory_tree_left_panel_width_limits, directory_tree_node_icon_fields,
    directory_tree_panel_layout, filesystem_ancestor_chain, image_list_column_layout,
    image_list_home_end_index, image_list_modified_column, image_list_name_column,
    image_list_size_column, image_list_thumb_column, min_scroll_offset_to_show_row,
    preview_texture_contain_rect, unc_share_root, wrapped_image_list_index,
};
use super::workers::read_child_directories;
use super::*;
use crate::app::ImageViewerApp;
use std::cmp::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

struct TempTreeDir {
    path: PathBuf,
}

impl TempTreeDir {
    fn new() -> Self {
        for attempt in 0..100 {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "simple_image_viewer_dir_tree_test_{}_{}_{}",
                std::process::id(),
                unique,
                attempt
            ));
            if std::fs::create_dir(&path).is_ok() {
                return Self { path };
            }
        }
        panic!("create temp directory tree test directory");
    }

    fn mkdir(&self, name: &str) -> PathBuf {
        let path = self.path.join(name);
        std::fs::create_dir(&path).expect("create subdirectory");
        path
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempTreeDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[test]
fn read_child_directories_lists_only_subdirectories() {
    let root = TempTreeDir::new();
    root.mkdir("alpha");
    root.mkdir("beta");
    root.mkdir("$RECYCLE.BIN");
    std::fs::write(root.path().join("photo.jpg"), b"image").expect("write file");

    let dirs = read_child_directories(root.path()).expect("read child directories");
    assert_eq!(dirs.len(), 2);
    assert_eq!(
        dirs[0].file_name().and_then(|name| name.to_str()),
        Some("alpha")
    );
    assert_eq!(
        dirs[1].file_name().and_then(|name| name.to_str()),
        Some("beta")
    );
}

#[test]
fn is_non_browsable_system_directory_matches_recycle_bin() {
    assert!(crate::scanner::is_non_browsable_system_directory(
        Path::new(r"F:\$RECYCLE.BIN")
    ));
    assert!(crate::scanner::is_non_browsable_system_directory(
        Path::new(r"C:\System Volume Information")
    ));
    assert!(!crate::scanner::is_non_browsable_system_directory(
        Path::new(r"F:\photos")
    ));
}

#[test]
fn apply_children_result_ignores_stale_generation() {
    let root = PathBuf::from("/tmp/siv-dir-tree-test-root");
    let child = PathBuf::from("/tmp/siv-dir-tree-test-child");

    let mut state = DirectoryTreeState::default();
    state.tree.places_loaded = true;
    state.tree.selected_fs_path = Some(root.clone());
    state.tree.generation = 2;
    let _ = state.tree.nodes.insert(
        root.clone(),
        DirectoryTreeNode {
            display_name: "root".to_string(),
            fs_path: root.clone(),
            expanded: true,
            loading: true,
            children_loaded: false,
            children: Vec::new(),
            error: None,
        },
        super::MAX_DIRECTORY_TREE_NODES,
    );

    state.apply_children_result(DirectoryChildrenResult {
        namespace_path: root.clone(),
        generation: 1,
        result: Ok(vec![child.clone()]),
    });

    let node = state.tree.nodes.get(&root).expect("root node");
    assert!(node.loading);
    assert!(!node.children_loaded);
    assert!(node.children.is_empty());
    let child_tree = super::namespace::namespace_child_path(&root, &root, &child);
    assert!(!state.tree.nodes.contains_key(&child_tree));
}

#[test]
fn apply_children_result_merges_children_and_clears_loading() {
    let root = PathBuf::from("/tmp/siv-dir-tree-test-root-2");
    let child = PathBuf::from("/tmp/siv-dir-tree-test-child-2");

    let mut state = DirectoryTreeState::default();
    state.tree.places_loaded = true;
    state.tree.selected_fs_path = Some(root.clone());
    state.tree.generation = 1;
    let _ = state.tree.nodes.insert(
        root.clone(),
        DirectoryTreeNode {
            display_name: "root".to_string(),
            fs_path: root.clone(),
            expanded: true,
            loading: true,
            children_loaded: false,
            children: Vec::new(),
            error: None,
        },
        super::MAX_DIRECTORY_TREE_NODES,
    );

    state.apply_children_result(DirectoryChildrenResult {
        namespace_path: root.clone(),
        generation: 1,
        result: Ok(vec![child.clone()]),
    });

    let node = state.tree.nodes.get(&root).expect("root node");
    assert!(!node.loading);
    assert!(node.children_loaded);
    let child_tree = super::namespace::namespace_child_path(&root, &root, &child);
    assert_eq!(node.children, vec![child_tree.clone()]);
    assert!(state.tree.nodes.contains_key(&child_tree));
}

#[test]
fn apply_children_result_records_read_error() {
    let root = PathBuf::from("/tmp/siv-dir-tree-test-missing");

    let mut state = DirectoryTreeState::default();
    state.tree.places_loaded = true;
    state.tree.selected_fs_path = Some(root.clone());
    state.tree.generation = 1;
    let _ = state.tree.nodes.insert(
        root.clone(),
        DirectoryTreeNode {
            display_name: "root".to_string(),
            fs_path: root.clone(),
            expanded: true,
            loading: true,
            children_loaded: false,
            children: Vec::new(),
            error: None,
        },
        super::MAX_DIRECTORY_TREE_NODES,
    );

    state.apply_children_result(DirectoryChildrenResult {
        namespace_path: root.clone(),
        generation: 1,
        result: Err("permission denied".to_string()),
    });

    let node = state.tree.nodes.get(&root).expect("root node");
    assert!(!node.loading);
    assert!(node.children_loaded);
    assert!(node.children.is_empty());
    assert_eq!(node.error.as_deref(), Some("permission denied"));
}

#[test]
fn apply_metadata_result_ignores_stale_generation() {
    let mut state = DirectoryTreeState::default();
    state.list.file_metadata_generation = 2;
    state.list.image_rows = vec![DirectoryTreeFileRow::new(
        PathBuf::from("/tmp/a.jpg"),
        "a.jpg".to_string(),
        10,
        None,
    )];

    state.apply_metadata_result(FileMetadataResult {
        generation: 1,
        paths: vec![PathBuf::from("/tmp/a.jpg")],
        modified_unix: vec![Some(1_700_000_000)],
    });

    assert!(state.list.image_rows[0].modified_unix.is_none());
}

#[test]
fn apply_metadata_result_updates_modified_times() {
    let mut state = DirectoryTreeState::default();
    state.list.file_metadata_generation = 1;
    state.list.image_rows = vec![
        DirectoryTreeFileRow::new(PathBuf::from("/tmp/a.jpg"), "a.jpg".to_string(), 10, None),
        DirectoryTreeFileRow::new(PathBuf::from("/tmp/b.jpg"), "b.jpg".to_string(), 20, None),
    ];

    state.apply_metadata_result(FileMetadataResult {
        generation: 1,
        paths: vec![PathBuf::from("/tmp/a.jpg"), PathBuf::from("/tmp/b.jpg")],
        modified_unix: vec![Some(1_700_000_000), None],
    });

    assert_eq!(state.list.image_rows[0].modified_unix, Some(1_700_000_000));
    assert!(state.list.image_rows[1].modified_unix.is_none());
}

#[test]
fn left_panel_width_limits_stay_ordered_on_narrow_viewport() {
    let (min, max) = directory_tree_left_panel_width_limits(364.0);
    assert!(min <= max);
    assert_eq!(min, 0.0);
    assert_eq!(max, 174.0);
    assert_eq!(clamp_directory_tree_left_panel_width(340.0, 364.0), 174.0);
}

#[test]
fn directory_tree_panel_layout_keeps_splitter_when_viewport_shrinks_from_right() {
    let (left, list) = directory_tree_panel_layout(340.0, 400.0, 640.0);
    assert_eq!(left, 340.0);
    assert_eq!(list, 290.0);

    let (left, list) = directory_tree_panel_layout(340.0, 400.0, 560.0);
    assert_eq!(left, 340.0);
    assert_eq!(list, 210.0);

    let (left, list) = directory_tree_panel_layout(340.0, 400.0, 530.0);
    assert_eq!(left, 340.0);
    assert_eq!(list, 180.0);
}

#[test]
fn left_panel_width_limits_allow_wide_folder_tree() {
    let (min, max) = directory_tree_left_panel_width_limits(640.0);
    assert_eq!(min, 0.0);
    assert_eq!(max, 450.0);
    assert_eq!(clamp_directory_tree_left_panel_width(500.0, 640.0), 450.0);
}

#[test]
fn directory_tree_splitter_applies_frame_delta_not_cumulative_drag_delta() {
    let width = apply_directory_tree_splitter_frame_delta(340.0, -4.0, 640.0);
    let width = apply_directory_tree_splitter_frame_delta(width, -4.0, 640.0);

    assert_eq!(width, 332.0);
}

#[test]
fn embedded_side_panel_clamped_width_never_panics_on_narrow_viewport() {
    use super::embedded_side_panel_clamped_width;

    assert_eq!(
        embedded_side_panel_clamped_width(Some(400.0), 380.0, 280.0),
        320.0
    );
    assert_eq!(
        embedded_side_panel_clamped_width(Some(400.0), 380.0, 200.0),
        320.0
    );
    assert_eq!(embedded_side_panel_clamped_width(None, 380.0, 640.0), 380.0);
    assert_eq!(
        embedded_side_panel_clamped_width(Some(500.0), 380.0, 640.0),
        500.0
    );
    assert_eq!(
        embedded_side_panel_clamped_width(Some(900.0), 380.0, 640.0),
        640.0
    );
}

#[test]
fn should_restore_embedded_side_panel_state_when_not_resizing() {
    assert!(super::should_restore_embedded_side_panel_state(false));
    assert!(!super::should_restore_embedded_side_panel_state(true));
}

#[test]
fn seed_embedded_side_panel_states_writes_persisted_layout() {
    use super::{DIRECTORY_TREE_EMBEDDED_SIDE_PANEL_ID, seed_embedded_side_panel_states};
    use eframe::egui::{self, Pos2, Rect};

    let ctx = egui::Context::default();
    let available = Rect::from_min_max(Pos2::new(8.0, 0.0), Pos2::new(1008.0, 800.0));
    seed_embedded_side_panel_states(&ctx, available, 420.0);

    let state = egui::PanelState::load(&ctx, egui::Id::new(DIRECTORY_TREE_EMBEDDED_SIDE_PANEL_ID))
        .expect("panel state");
    assert_eq!(state.rect.min, available.min);
    assert!((state.rect.width() - 420.0).abs() < f32::EPSILON);
}

#[test]
fn sync_images_updates_list_while_places_still_loading() {
    let paths = vec![PathBuf::from("/tmp/a.avif"), PathBuf::from("/tmp/b.avif")];
    let mut state = DirectoryTreeState::default();
    state.tree.places_loading = true;
    state.tree.places_loaded = false;
    state.list.image_rows.clear();
    state.list.scanning = true;

    state.sync_images(
        &paths,
        &[0, 0],
        &[None, None],
        0,
        true,
        String::from("scanning"),
    );

    assert_eq!(state.list.image_rows.len(), 2);
    assert!(state.list.scanning);
}

#[test]
fn main_window_canvas_rects_insets_embedded_nav_panel() {
    use crate::app::rendering::geometry::main_window_canvas_rects;
    use eframe::egui::{Pos2, Rect};

    let available = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1000.0, 800.0));
    let panel = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(380.0, 800.0));
    let grab = 8.0;
    let (paint, interact) = main_window_canvas_rects(available, grab, Some(panel));
    assert_eq!(paint.min.x, 380.0);
    assert_eq!(interact.min.x, 388.0);
}

#[test]
fn directory_tree_panel_layout_honors_saved_split() {
    let (left, list) = directory_tree_panel_layout(280.0, 320.0, 640.0);
    assert_eq!(left, 280.0);
    assert_eq!(list, 350.0);
}

#[test]
fn directory_tree_panel_layout_shrinks_for_display_on_narrow_viewport() {
    let (left, list) = directory_tree_panel_layout(340.0, 400.0, 364.0);
    assert_eq!(left, 174.0);
    assert_eq!(list, 180.0);
    // Stored preferences are unchanged — only the layout tuple shrinks.
    let mut state = DirectoryTreeState::default();
    state.tree.left_panel_width = 340.0;
    state.list.image_list_panel_width = 400.0;
    assert_eq!(state.tree.left_panel_width, 340.0);
    assert_eq!(state.list.image_list_panel_width, 400.0);
}

#[test]
fn visible_strip_row_indices_skips_stale_range_while_scroll_pending() {
    assert!(
        ImageViewerApp::visible_strip_row_indices(Some((100, 110)), true, 200, false).is_empty()
    );
    assert_eq!(
        ImageViewerApp::visible_strip_row_indices(Some((100, 110)), true, 200, true),
        (100..110).collect::<Vec<_>>()
    );
    assert_eq!(
        ImageViewerApp::visible_strip_row_indices(Some((100, 110)), false, 200, false),
        (100..110).collect::<Vec<_>>()
    );
    assert_eq!(
        ImageViewerApp::visible_strip_row_indices(None, false, 7, true),
        (0..7).collect::<Vec<_>>()
    );
    assert_eq!(
        ImageViewerApp::visible_strip_row_indices(None, false, 200, true),
        (0..200.min(BOOTSTRAP_STRIP_VISIBLE_ROW_CAP)).collect::<Vec<_>>()
    );
    assert!(ImageViewerApp::visible_strip_row_indices(None, false, 7, false).is_empty());
}

#[test]
fn sync_images_marks_list_scroll_when_current_index_changes() {
    let paths = vec![PathBuf::from("/tmp/a.avif"), PathBuf::from("/tmp/b.avif")];
    let mut state = DirectoryTreeState::default();
    state.list.image_rows = paths
        .iter()
        .map(|path| DirectoryTreeFileRow::new(path.clone(), directory_display_name(path), 0, None))
        .collect();
    state.list.current_index = 0;
    state.list.scroll_image_list_to_current = false;

    state.sync_images(&paths, &[0, 0], &[None, None], 1, false, String::new());

    assert_eq!(state.list.current_index, 1);
    assert!(state.list.scroll_image_list_to_current);
}

#[test]
fn sync_images_preserves_keyboard_list_selection_until_main_window_catches_up() {
    let paths = vec![PathBuf::from("/tmp/a.avif"), PathBuf::from("/tmp/b.avif")];
    let mut state = DirectoryTreeState::default();
    state.list.image_rows = paths
        .iter()
        .map(|path| DirectoryTreeFileRow::new(path.clone(), directory_display_name(path), 0, None))
        .collect();
    state.list.current_index = 1;
    state.list.image_list_keyboard_active = true;
    state.list.scroll_image_list_to_current = true;

    state.sync_images(&paths, &[0, 0], &[None, None], 0, false, String::new());

    assert_eq!(state.list.current_index, 1);
    assert!(state.list.scroll_image_list_to_current);
}

#[test]
fn sync_images_follows_main_index_after_list_keyboard_capture_released() {
    let paths = vec![PathBuf::from("/tmp/a.avif"), PathBuf::from("/tmp/b.avif")];
    let mut state = DirectoryTreeState::default();
    state.list.image_rows = paths
        .iter()
        .map(|path| DirectoryTreeFileRow::new(path.clone(), directory_display_name(path), 0, None))
        .collect();
    state.list.current_index = 0;
    state.list.image_list_keyboard_active = true;
    state.list.scroll_image_list_to_current = false;

    state.list.image_list_keyboard_active = false;
    state.sync_images(&paths, &[0, 0], &[None, None], 1, false, String::new());

    assert_eq!(state.list.current_index, 1);
    assert!(state.list.scroll_image_list_to_current);
}

#[test]
fn wrapped_image_list_index_loops_at_bounds() {
    assert_eq!(wrapped_image_list_index(0, -1, 10), Some(9));
    assert_eq!(wrapped_image_list_index(9, 1, 10), Some(0));
    assert_eq!(wrapped_image_list_index(4, 1, 10), Some(5));
    assert_eq!(wrapped_image_list_index(4, -1, 10), Some(3));
    assert!(wrapped_image_list_index(0, -1, 1).is_none());
    assert!(wrapped_image_list_index(0, 1, 1).is_none());
    assert!(wrapped_image_list_index(0, 1, 0).is_none());
}

#[test]
fn image_list_home_end_index_jumps_to_bounds() {
    assert_eq!(image_list_home_end_index(4, egui::Key::Home, 10), Some(0));
    assert_eq!(image_list_home_end_index(4, egui::Key::End, 10), Some(9));
    assert!(image_list_home_end_index(0, egui::Key::Home, 10).is_none());
    assert!(image_list_home_end_index(9, egui::Key::End, 10).is_none());
    assert!(image_list_home_end_index(0, egui::Key::Home, 0).is_none());
}

#[test]
fn min_scroll_offset_to_show_row_keeps_visible_rows_in_place() {
    assert!(min_scroll_offset_to_show_row(5, 30.0, 24.0, 260.0, 150.0).is_none());
}

#[test]
fn min_scroll_offset_to_show_row_scrolls_down_for_row_below_viewport() {
    assert_eq!(
        min_scroll_offset_to_show_row(20, 30.0, 24.0, 260.0, 0.0),
        Some(364.0)
    );
}

#[test]
fn min_scroll_offset_to_show_row_scrolls_up_for_row_above_viewport() {
    assert_eq!(
        min_scroll_offset_to_show_row(2, 30.0, 24.0, 260.0, 600.0),
        Some(60.0)
    );
}

#[test]
fn min_scroll_offset_to_show_row_scrolls_when_row_bottom_clipped_at_viewport_edge() {
    assert_eq!(
        min_scroll_offset_to_show_row(8, 54.0, 48.0, 260.0, 0.0),
        Some(220.0)
    );
}

#[test]
fn preview_texture_contain_rect_preserves_aspect_ratio() {
    let cell = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(100.0, 100.0));
    let wide = preview_texture_contain_rect(cell, 200.0, 100.0);
    assert!((wide.width() - 100.0).abs() < f32::EPSILON);
    assert!((wide.height() - 50.0).abs() < f32::EPSILON);
    assert!((wide.center().y - 50.0).abs() < f32::EPSILON);

    let tall = preview_texture_contain_rect(cell, 100.0, 200.0);
    assert!((tall.width() - 50.0).abs() < f32::EPSILON);
    assert!((tall.height() - 100.0).abs() < f32::EPSILON);
    assert!((tall.center().x - 50.0).abs() < f32::EPSILON);
}

#[test]
fn image_list_columns_do_not_overlap_when_panel_is_narrow() {
    let row_rect = egui::Rect::from_min_size(
        egui::pos2(0.0, 0.0),
        egui::vec2(
            320.0,
            crate::settings::DirectoryTreeListPreviewSize::Small.thumb_px(),
        ),
    );
    let columns = image_list_column_layout(row_rect.width(), 4.0, 72.0, 148.0, 48.0);
    let spacing = 4.0;
    let thumb = image_list_thumb_column(row_rect, spacing, 48.0);
    let name = image_list_name_column(row_rect, &columns, spacing, 48.0);
    let size = image_list_size_column(row_rect, &columns, spacing);
    let modified = image_list_modified_column(row_rect, &columns, spacing);
    assert!(thumb.right() <= name.left());
    assert!(name.right() <= size.left());
    assert!(size.right() <= modified.left());
}

#[test]
fn image_list_columns_use_content_widths_when_panel_is_wide() {
    let columns = image_list_column_layout(640.0, 4.0, 72.0, 148.0, 48.0);
    assert_eq!(columns.size_w, 72.0);
    assert_eq!(columns.modified_w, 148.0);
}

#[test]
fn image_list_thumb_column_has_fixed_width() {
    let row_rect = egui::Rect::from_min_size(
        egui::pos2(10.0, 0.0),
        egui::vec2(
            400.0,
            crate::settings::DirectoryTreeListPreviewSize::Small.thumb_px(),
        ),
    );
    let thumb = image_list_thumb_column(row_rect, 4.0, 48.0);
    assert!((thumb.width() - 48.0).abs() < f32::EPSILON);
    assert_eq!(thumb.left(), row_rect.left() + 4.0);
}

#[test]
fn filesystem_ancestor_chain_lists_volume_root_to_target() {
    let target = PathBuf::from(r"F:\iphone15\2026-05-27");
    let chain = filesystem_ancestor_chain(&target);
    assert_eq!(chain.len(), 3);
    assert_eq!(chain[0], PathBuf::from(r"F:\"));
    assert_eq!(chain[1], PathBuf::from(r"F:\iphone15"));
    assert_eq!(chain[2], target);
}

#[test]
fn unc_share_root_extracts_server_and_share() {
    let path = PathBuf::from("//192.168.2.1/pictures/2024/06");
    assert_eq!(
        unc_share_root(&path),
        Some(PathBuf::from("//192.168.2.1/pictures"))
    );
}

#[test]
fn filesystem_ancestor_chain_lists_unc_share_to_target() {
    let target = PathBuf::from("//192.168.2.1/pictures/2024/06");
    let chain = filesystem_ancestor_chain(&target);
    assert_eq!(chain.len(), 3);
    assert_eq!(chain[0], PathBuf::from("//192.168.2.1/pictures"));
    assert_eq!(chain[1], PathBuf::from("//192.168.2.1/pictures/2024"));
    assert_eq!(chain[2], target);
}

#[test]
fn reveal_selected_namespace_mounts_unc_share_under_network() {
    let places = DirectoryTreePlaces {
        known_folders: Vec::new(),
        drives: Vec::new(),
        network_locations: Vec::new(),
        this_pc_label: "This PC".to_string(),
        network_label: "Network".to_string(),
    };

    let mut state = DirectoryTreeState::default();
    state.initialize_places(places);
    state.set_selected_fs_path(PathBuf::from("//192.168.2.1/pictures/2024"));

    assert!(state.tree.network_visible);
    let share_browse =
        unc_share_root(&PathBuf::from("//192.168.2.1/pictures/2024")).expect("share");
    let share_tree = super::namespace::network_share_namespace_path(&share_browse);
    let network = state
        .tree
        .nodes
        .get(&network_namespace_path())
        .expect("network node");
    assert_eq!(network.children, vec![share_tree.clone()]);
    assert!(state.tree.nodes.contains_key(&share_tree));
}

#[test]
fn directory_ancestor_chain_lists_root_to_target() {
    let root = PathBuf::from(r"F:\");
    let target = PathBuf::from(r"F:\iphone15\2026-05-27");
    let chain = directory_ancestor_chain(&root, &target);
    assert_eq!(chain.len(), 3);
    assert_eq!(chain[0], root);
    assert_eq!(chain[1], PathBuf::from(r"F:\iphone15"));
    assert_eq!(chain[2], target);
}

#[test]
fn initialize_places_resets_nodes_and_bumps_generation() {
    let places = DirectoryTreePlaces {
        known_folders: Vec::new(),
        drives: Vec::new(),
        network_locations: Vec::new(),
        this_pc_label: "This PC".to_string(),
        network_label: "Network".to_string(),
    };

    let mut state = DirectoryTreeState::default();
    state.initialize_places(places.clone());
    assert_eq!(state.tree.generation, 1);
    assert!(state.tree.places_loaded);
    assert!(state.tree.nodes.contains_key(&this_pc_namespace_path()));
    assert!(!state.tree.network_visible);
    assert!(!state.tree.nodes.contains_key(&network_namespace_path()));

    let _ = state.tree.nodes.insert(
        PathBuf::from("/tmp/siv-dir-tree-stale"),
        directory_tree_node("stale", PathBuf::from("/tmp/siv-dir-tree-stale")),
        super::MAX_DIRECTORY_TREE_NODES,
    );

    state.initialize_places(places);
    assert_eq!(state.tree.generation, 2);
    assert_eq!(state.tree.nodes.len(), 1);
    assert!(state.tree.nodes.contains_key(&this_pc_namespace_path()));
    assert!(!state.tree.network_visible);
    assert!(!state.tree.nodes.contains_key(&network_namespace_path()));
}

#[test]
fn compare_image_list_sort_keys_orders_by_name_case_insensitive() {
    let paths = vec![
        PathBuf::from(r"C:\beta.jpg"),
        PathBuf::from(r"C:\Alpha.jpg"),
    ];
    assert_eq!(
        compare_image_list_sort_keys(0, 1, ImageListSortColumn::Name, &paths, &[], &[],),
        Ordering::Greater
    );
}

#[test]
fn compare_image_list_sort_keys_puts_missing_modified_last() {
    assert_eq!(compare_optional_unix_time(Some(10), None), Ordering::Less);
    assert_eq!(
        compare_optional_unix_time(None, Some(10)),
        Ordering::Greater
    );
}

#[test]
fn image_list_sort_desc_mirrors_asc_index_even_with_tied_keys() {
    const LEN: usize = 15;
    let paths: Vec<PathBuf> = (0..LEN)
        .map(|index| PathBuf::from(format!(r"C:\img_{index:02}.jpg")))
        .collect();
    let sizes = vec![100u64; LEN];
    let modified = vec![Some(1_700_000_000i64); LEN];

    let asc_order = image_list_sort_order(
        LEN,
        ImageListSortColumn::Modified,
        true,
        &paths,
        &sizes,
        &modified,
    );
    let mut asc_paths = Vec::with_capacity(LEN);
    for &old_index in &asc_order {
        asc_paths.push(paths[old_index].clone());
    }

    let desc_order = image_list_sort_order(
        LEN,
        ImageListSortColumn::Modified,
        false,
        &asc_paths,
        &sizes,
        &modified,
    );
    let mut asc_to_desc = vec![0usize; LEN];
    for (new_idx, &old_idx) in desc_order.iter().enumerate() {
        asc_to_desc[old_idx] = new_idx;
    }

    let mut unsorted_to_asc = vec![0usize; LEN];
    for (new_idx, &old_idx) in asc_order.iter().enumerate() {
        unsorted_to_asc[old_idx] = new_idx;
    }

    for original_index in 0..LEN {
        let asc_index = unsorted_to_asc[original_index];
        let desc_index = asc_to_desc[asc_index];
        assert_eq!(
            asc_index + desc_index,
            LEN - 1,
            "original_index={original_index} asc={asc_index} desc={desc_index}"
        );
    }
}

#[test]
fn directory_tree_node_icon_distinguishes_places_roots() {
    use crate::directory_tree_places::types::{KnownFolderKind, known_folder_namespace_path};

    let docs_fs = PathBuf::from("/tmp/siv-known-docs");
    let drive = PathBuf::from("/tmp/siv-drive");
    let places = DirectoryTreePlaces {
        known_folders: vec![KnownFolderEntry {
            kind: KnownFolderKind::Pictures,
            display_name: "Pictures".to_string(),
            namespace_path: known_folder_namespace_path(KnownFolderKind::Pictures),
            fs_path: docs_fs.clone(),
        }],
        drives: vec![crate::directory_tree_places::types::DriveEntry {
            display_name: "Data".to_string(),
            fs_path: drive.clone(),
        }],
        network_locations: Vec::new(),
        this_pc_label: "This PC".to_string(),
        network_label: "Network".to_string(),
    };

    let mut state = DirectoryTreeState::default();
    state.initialize_places(places);
    state.tree.ensure_network_visible();

    assert_eq!(
        directory_tree_node_icon_fields(
            &state.tree.known_folders,
            &state.tree.nodes,
            &this_pc_namespace_path()
        ),
        DirectoryTreeNodeIcon::ThisPc
    );
    assert_eq!(
        directory_tree_node_icon_fields(
            &state.tree.known_folders,
            &state.tree.nodes,
            &network_namespace_path()
        ),
        DirectoryTreeNodeIcon::Network
    );
    assert_eq!(
        directory_tree_node_icon_fields(
            &state.tree.known_folders,
            &state.tree.nodes,
            &known_folder_namespace_path(KnownFolderKind::Pictures),
        ),
        DirectoryTreeNodeIcon::KnownFolder(KnownFolderKind::Pictures)
    );
    assert_eq!(
        directory_tree_node_icon_fields(
            &state.tree.known_folders,
            &state.tree.nodes,
            &super::namespace::drive_mount_namespace_path(&drive),
        ),
        DirectoryTreeNodeIcon::Drive
    );
    assert_eq!(
        directory_tree_node_icon_fields(
            &state.tree.known_folders,
            &state.tree.nodes,
            &PathBuf::from("/tmp/ordinary"),
        ),
        DirectoryTreeNodeIcon::Folder
    );
}

#[test]
fn reveal_known_folder_does_not_add_known_folder_to_this_pc_children() {
    use crate::directory_tree_places::types::{
        DriveEntry, KnownFolderKind, known_folder_namespace_path,
    };

    let pictures_fs = PathBuf::from("/tmp/siv-known-pictures");
    let pictures_tree = known_folder_namespace_path(KnownFolderKind::Pictures);
    let places = DirectoryTreePlaces {
        known_folders: vec![KnownFolderEntry {
            kind: KnownFolderKind::Pictures,
            display_name: "Pictures".to_string(),
            namespace_path: pictures_tree.clone(),
            fs_path: pictures_fs.clone(),
        }],
        drives: vec![DriveEntry {
            display_name: "Data".to_string(),
            fs_path: PathBuf::from("/tmp/siv-drive"),
        }],
        network_locations: Vec::new(),
        this_pc_label: "This PC".to_string(),
        network_label: "Network".to_string(),
    };

    let mut state = DirectoryTreeState::default();
    state.initialize_places(places);
    state
        .tree
        .nodes
        .get_mut(&this_pc_namespace_path())
        .expect("This PC")
        .expanded = true;

    state.set_selected_fs_path(pictures_fs);
    let _requests = state.reveal_selected_namespace();

    let this_pc = state
        .tree
        .nodes
        .get(&this_pc_namespace_path())
        .expect("This PC");
    assert!(
        !this_pc
            .children
            .iter()
            .any(|child| child.as_os_str() == pictures_tree.as_os_str()),
        "known folders must not appear under This PC"
    );
}

#[test]
fn reveal_known_folder_does_not_expand_this_pc() {
    use crate::directory_tree_places::types::{KnownFolderKind, known_folder_namespace_path};

    let docs_fs = PathBuf::from("/tmp/siv-known-docs");
    let places = DirectoryTreePlaces {
        known_folders: vec![KnownFolderEntry {
            kind: KnownFolderKind::Documents,
            display_name: "Documents".to_string(),
            namespace_path: known_folder_namespace_path(KnownFolderKind::Documents),
            fs_path: docs_fs.clone(),
        }],
        drives: Vec::new(),
        network_locations: Vec::new(),
        this_pc_label: "This PC".to_string(),
        network_label: "Network".to_string(),
    };

    let mut state = DirectoryTreeState::default();
    state.initialize_places(places);
    state.set_selected_fs_path(docs_fs);
    let _requests = state.reveal_selected_namespace();
    assert!(
        !state
            .tree
            .nodes
            .get(&this_pc_namespace_path())
            .is_some_and(|node| node.expanded)
    );
}

#[test]
fn reveal_selected_namespace_expands_nested_known_folder_path_after_places_init() {
    use crate::directory_tree_places::types::{KnownFolderKind, known_folder_namespace_path};

    let docs_fs = PathBuf::from("/tmp/siv-known-docs");
    let nested = docs_fs.join("2024").join("06");
    let places = DirectoryTreePlaces {
        known_folders: vec![KnownFolderEntry {
            kind: KnownFolderKind::Documents,
            display_name: "Documents".to_string(),
            namespace_path: known_folder_namespace_path(KnownFolderKind::Documents),
            fs_path: docs_fs.clone(),
        }],
        drives: Vec::new(),
        network_locations: Vec::new(),
        this_pc_label: "This PC".to_string(),
        network_label: "Network".to_string(),
    };

    let mut state = DirectoryTreeState::default();
    assert!(state.reveal_selected_namespace().is_empty());
    state.set_selected_fs_path(nested.clone());
    state.initialize_places(places);
    let requests = state.reveal_selected_namespace();
    assert!(!requests.is_empty());
    let docs_tree = known_folder_namespace_path(KnownFolderKind::Documents);
    assert!(
        state
            .tree
            .nodes
            .get(&docs_tree)
            .is_some_and(|node| node.expanded)
    );
    let year_tree =
        super::namespace::namespace_child_path(&docs_tree, &docs_fs, &docs_fs.join("2024"));
    assert!(
        state
            .tree
            .nodes
            .get(&year_tree)
            .is_some_and(|node| node.expanded)
    );
}

#[test]
fn selected_namespace_path_distinguishes_alias_nodes_with_same_fs_path() {
    use crate::directory_tree_places::types::{KnownFolderKind, known_folder_namespace_path};

    let downloads_fs = PathBuf::from("/home/user/Downloads");
    let known_tree = known_folder_namespace_path(KnownFolderKind::Downloads);
    let profile_tree = downloads_fs.clone();

    let mut state = DirectoryTreeState::default();
    let _ = state.tree.nodes.or_insert_with(
        known_tree.clone(),
        super::MAX_DIRECTORY_TREE_NODES,
        || directory_tree_node("Downloads".to_string(), downloads_fs.clone()),
    );
    let _ = state.tree.nodes.or_insert_with(
        profile_tree.clone(),
        super::MAX_DIRECTORY_TREE_NODES,
        || directory_tree_node("下载".to_string(), downloads_fs.clone()),
    );

    state
        .tree
        .set_selected_namespace_node(profile_tree.clone(), downloads_fs.clone());
    assert_eq!(
        state.tree.selected_namespace_path.as_deref(),
        Some(profile_tree.as_path())
    );
    assert_eq!(
        state.tree.selected_fs_path.as_deref(),
        Some(downloads_fs.as_path())
    );

    state
        .tree
        .set_selected_namespace_node(known_tree.clone(), downloads_fs);
    assert_eq!(
        state.tree.selected_namespace_path.as_deref(),
        Some(known_tree.as_path())
    );
}

#[test]
fn apply_children_omits_places_mount_roots_from_media_parent() {
    use crate::directory_tree_places::types::DriveEntry;

    let happy = PathBuf::from("/run/media/happy");
    let places = crate::directory_tree_places::DirectoryTreePlaces {
        known_folders: Vec::new(),
        drives: vec![
            DriveEntry {
                display_name: "/".to_string(),
                fs_path: PathBuf::from("/"),
            },
            DriveEntry {
                display_name: "happy".to_string(),
                fs_path: happy.clone(),
            },
        ],
        network_locations: Vec::new(),
        this_pc_label: "Places".to_string(),
        network_label: "Network".to_string(),
    };

    let mut state = DirectoryTreeState::default();
    state.initialize_places(places);
    let root_mount = super::namespace::drive_mount_namespace_path(Path::new("/"));
    let run_media_tree = super::namespace::namespace_child_path(
        &super::namespace::namespace_child_path(
            &root_mount,
            Path::new("/"),
            &PathBuf::from("/run"),
        ),
        &PathBuf::from("/run"),
        &PathBuf::from("/run/media"),
    );
    state
        .tree
        .nodes
        .or_insert_with(
            run_media_tree.clone(),
            super::MAX_DIRECTORY_TREE_NODES,
            || super::directory_tree_node("media".to_string(), PathBuf::from("/run/media")),
        )
        .expect("run/media node");

    let other_browse = PathBuf::from("/run/media/other");
    state.apply_children_result(super::DirectoryChildrenResult {
        namespace_path: run_media_tree.clone(),
        generation: state.tree.generation,
        result: Ok(vec![happy.clone(), other_browse.clone()]),
    });

    let node = state
        .tree
        .nodes
        .get(&run_media_tree)
        .expect("run/media node after load");
    let happy_tree = super::namespace::drive_mount_namespace_path(&happy);
    assert!(
        !node
            .children
            .iter()
            .any(|path| path.as_os_str() == happy_tree.as_os_str())
    );
    let other_tree = super::namespace::namespace_child_path(
        &run_media_tree,
        &PathBuf::from("/run/media"),
        &other_browse,
    );
    assert!(
        node.children
            .iter()
            .any(|path| path.as_os_str() == other_tree.as_os_str())
    );
}

#[test]
fn reveal_mount_path_skips_root_slash_ancestor_chain() {
    use crate::directory_tree_places::types::DriveEntry;

    let happy = PathBuf::from("/run/media/happy");
    let happy_tree = super::namespace::drive_mount_namespace_path(&happy);
    let custom = happy.join("CDROM").join("custom");
    let places = crate::directory_tree_places::DirectoryTreePlaces {
        known_folders: Vec::new(),
        drives: vec![
            DriveEntry {
                display_name: "/".to_string(),
                fs_path: PathBuf::from("/"),
            },
            DriveEntry {
                display_name: "happy".to_string(),
                fs_path: happy.clone(),
            },
        ],
        network_locations: Vec::new(),
        this_pc_label: "Places".to_string(),
        network_label: "Network".to_string(),
    };

    let mut state = DirectoryTreeState::default();
    state.initialize_places(places);
    state.set_selected_fs_path(custom);

    let requests = state.reveal_selected_namespace();
    assert!(
        !requests
            .iter()
            .any(|request| request.namespace_path == PathBuf::from("/run"))
    );
    assert!(
        !requests
            .iter()
            .any(|request| request.namespace_path == PathBuf::from("/run/media"))
    );
    assert!(
        requests
            .iter()
            .any(|request| request.namespace_path == happy_tree)
            || requests.iter().any(|request| {
                request.namespace_path
                    == super::namespace::namespace_child_path(
                        &happy_tree,
                        &happy,
                        &happy.join("CDROM"),
                    )
            })
    );
}

#[test]
fn selected_namespace_path_distinguishes_mount_namespace_branches() {
    use crate::directory_tree_places::types::DriveEntry;

    let happy = PathBuf::from("/run/media/happy");
    let browse = happy.join("CDROM");
    let root_mount = super::namespace::drive_mount_namespace_path(Path::new("/"));
    let happy_mount = super::namespace::drive_mount_namespace_path(&happy);
    let via_root = super::namespace::namespace_child_path(
        &super::namespace::namespace_child_path(
            &super::namespace::namespace_child_path(
                &super::namespace::namespace_child_path(
                    &root_mount,
                    Path::new("/"),
                    &PathBuf::from("/run"),
                ),
                &PathBuf::from("/run"),
                &PathBuf::from("/run/media"),
            ),
            &PathBuf::from("/run/media"),
            &happy,
        ),
        &happy,
        &browse,
    );
    let via_happy = super::namespace::namespace_child_path(&happy_mount, &happy, &browse);

    let places = crate::directory_tree_places::DirectoryTreePlaces {
        known_folders: Vec::new(),
        drives: vec![
            DriveEntry {
                display_name: "/".to_string(),
                fs_path: PathBuf::from("/"),
            },
            DriveEntry {
                display_name: "happy".to_string(),
                fs_path: happy.clone(),
            },
        ],
        network_locations: Vec::new(),
        this_pc_label: "Places".to_string(),
        network_label: "Network".to_string(),
    };

    let mut state = DirectoryTreeState::default();
    state.initialize_places(places);
    state
        .tree
        .set_selected_namespace_node(via_happy.clone(), browse.clone());
    assert_eq!(
        state.tree.selected_namespace_path.as_deref(),
        Some(via_happy.as_path())
    );
    state
        .tree
        .set_selected_namespace_node(via_root.clone(), browse.clone());
    assert_eq!(
        state.tree.selected_namespace_path.as_deref(),
        Some(via_root.as_path())
    );
    assert_ne!(via_root, via_happy);
}

#[test]
fn expand_namespace_node_uses_explicit_namespace_branch() {
    use crate::directory_tree_places::types::DriveEntry;

    let happy = PathBuf::from("/run/media/happy");
    let browse = happy.join("CDROM");
    let root_mount = super::namespace::drive_mount_namespace_path(Path::new("/"));
    let happy_mount = super::namespace::drive_mount_namespace_path(&happy);
    let via_root = super::namespace::namespace_child_path(
        &super::namespace::namespace_child_path(
            &super::namespace::namespace_child_path(
                &super::namespace::namespace_child_path(
                    &root_mount,
                    Path::new("/"),
                    &PathBuf::from("/run"),
                ),
                &PathBuf::from("/run"),
                &PathBuf::from("/run/media"),
            ),
            &PathBuf::from("/run/media"),
            &happy,
        ),
        &happy,
        &browse,
    );
    let via_happy = super::namespace::namespace_child_path(&happy_mount, &happy, &browse);

    let places = crate::directory_tree_places::DirectoryTreePlaces {
        known_folders: Vec::new(),
        drives: vec![
            DriveEntry {
                display_name: "/".to_string(),
                fs_path: PathBuf::from("/"),
            },
            DriveEntry {
                display_name: "happy".to_string(),
                fs_path: happy.clone(),
            },
        ],
        network_locations: Vec::new(),
        this_pc_label: "Places".to_string(),
        network_label: "Network".to_string(),
    };

    let mut state = DirectoryTreeState::default();
    state.initialize_places(places);
    state
        .tree
        .nodes
        .or_insert_with(via_root.clone(), super::MAX_DIRECTORY_TREE_NODES, || {
            super::directory_tree_node("CDROM".to_string(), browse.clone())
        })
        .expect("via_root node");
    state
        .tree
        .nodes
        .or_insert_with(via_happy.clone(), super::MAX_DIRECTORY_TREE_NODES, || {
            super::directory_tree_node("CDROM".to_string(), browse.clone())
        })
        .expect("via_happy node");

    let request = state
        .expand_namespace_node(&via_root)
        .expect("expand via_root");
    assert_eq!(request.namespace_path, via_root);
    assert_eq!(request.fs_path, browse);

    let request = state
        .expand_namespace_node(&via_happy)
        .expect("expand via_happy");
    assert_eq!(request.namespace_path, via_happy);
    assert_eq!(request.fs_path, browse);
    assert_ne!(request.namespace_path, via_root);
}

#[test]
fn reveal_selected_namespace_uses_persisted_namespace_branch() {
    use crate::directory_tree_places::types::DriveEntry;

    let happy = PathBuf::from("/run/media/happy");
    let browse = happy.join("CDROM").join("custom");
    let root_mount = super::namespace::drive_mount_namespace_path(Path::new("/"));
    let via_root = super::namespace::namespace_child_path(
        &super::namespace::namespace_child_path(
            &super::namespace::namespace_child_path(
                &super::namespace::namespace_child_path(
                    &super::namespace::namespace_child_path(
                        &root_mount,
                        Path::new("/"),
                        &PathBuf::from("/run"),
                    ),
                    &PathBuf::from("/run"),
                    &PathBuf::from("/run/media"),
                ),
                &PathBuf::from("/run/media"),
                &happy,
            ),
            &happy,
            &happy.join("CDROM"),
        ),
        &happy.join("CDROM"),
        &browse,
    );

    let places = crate::directory_tree_places::DirectoryTreePlaces {
        known_folders: Vec::new(),
        drives: vec![
            DriveEntry {
                display_name: "/".to_string(),
                fs_path: PathBuf::from("/"),
            },
            DriveEntry {
                display_name: "happy".to_string(),
                fs_path: happy.clone(),
            },
        ],
        network_locations: Vec::new(),
        this_pc_label: "Places".to_string(),
        network_label: "Network".to_string(),
    };

    let mut state = DirectoryTreeState::default();
    state.initialize_places(places);
    state
        .tree
        .set_selected_namespace_node(via_root.clone(), browse.clone());
    let requests = state.reveal_selected_namespace();
    assert!(
        requests
            .iter()
            .any(|request| request.namespace_path == via_root)
            || state.tree.selected_namespace_path.as_deref() == Some(via_root.as_path())
    );
    assert!(!requests.iter().any(|request| {
        request.namespace_path == super::namespace::drive_mount_namespace_path(&happy)
    }));
}

#[test]
fn restore_tree_selection_preserves_namespace_before_places_load() {
    let happy = PathBuf::from("/run/media/happy");
    let browse = happy.join("CDROM").join("custom");
    let root_mount = super::namespace::drive_mount_namespace_path(Path::new("/"));
    let via_root = super::namespace::namespace_child_path(
        &super::namespace::namespace_child_path(
            &super::namespace::namespace_child_path(
                &super::namespace::namespace_child_path(
                    &super::namespace::namespace_child_path(
                        &root_mount,
                        Path::new("/"),
                        &PathBuf::from("/run"),
                    ),
                    &PathBuf::from("/run"),
                    &PathBuf::from("/run/media"),
                ),
                &PathBuf::from("/run/media"),
                &happy,
            ),
            &happy,
            &happy.join("CDROM"),
        ),
        &happy.join("CDROM"),
        &browse,
    );

    let mut state = DirectoryTreeState::default();
    assert!(!state.tree.places_loaded);
    state
        .tree
        .restore_tree_selection(browse.clone(), Some(via_root.clone()));
    assert_eq!(state.tree.selected_fs_path.as_ref(), Some(&browse));
    assert_eq!(state.tree.selected_namespace_path.as_ref(), Some(&via_root));
}

#[test]
fn reveal_selected_namespace_pre_places_builds_mount_chain() {
    let browse = PathBuf::from(r"F:\iphone15\2026-05-27");
    let mount_root = super::namespace::drive_mount_namespace_path(Path::new(r"F:\"));
    let via_mount = super::namespace::namespace_child_path(
        &super::namespace::namespace_child_path(
            &mount_root,
            Path::new(r"F:\"),
            &PathBuf::from(r"F:\iphone15"),
        ),
        &PathBuf::from(r"F:\iphone15"),
        &browse,
    );
    let iphone15 = PathBuf::from(r"F:\iphone15");
    let iphone15_ns =
        super::namespace::namespace_child_path(&mount_root, Path::new(r"F:\"), &iphone15);

    let mut state = DirectoryTreeState::default();
    assert!(!state.tree.places_loaded);
    state
        .tree
        .restore_tree_selection(browse.clone(), Some(via_mount.clone()));
    let requests = state.tree.reveal_selected_namespace();
    assert!(
        state.tree.nodes.contains_key(&mount_root),
        "mount root node should exist before Places loads"
    );
    assert!(
        state.tree.nodes.contains_key(&via_mount),
        "selected namespace node should exist before Places loads"
    );
    assert!(
        !requests.is_empty()
            || state
                .tree
                .nodes
                .get(&mount_root)
                .is_some_and(|node| node.loading),
        "reveal should request children for the bootstrap chain"
    );
    let mount_node = state.tree.nodes.get(&mount_root).expect("mount node");
    assert!(
        mount_node
            .children
            .iter()
            .any(|child| child.as_os_str() == iphone15_ns.as_os_str()),
        "bootstrap chain should link mount root toward the selected path"
    );
}

#[test]
fn pre_places_folder_display_root_none_when_places_loaded() {
    use super::domains::DirectoryTreeTreeSnapshot;
    use super::view::DirectoryTreeView;
    use std::sync::Arc;

    let mount_root = super::namespace::drive_mount_namespace_path(Path::new(r"F:\"));
    let view = DirectoryTreeView::assemble(
        Arc::new(DirectoryTreeTreeSnapshot {
            places_loaded: true,
            selected_namespace_path: Some(mount_root.clone()),
            ..Default::default()
        }),
        Arc::new(super::domains::DirectoryTreeListSnapshot::default()),
        Arc::new(super::domains::DirectoryTreePreviewSnapshot::default()),
    );
    assert!(view.pre_places_folder_display_root().is_none());
}

#[test]
fn pre_places_folder_display_root_none_without_selected_namespace() {
    use super::domains::DirectoryTreeTreeSnapshot;
    use super::view::DirectoryTreeView;
    use std::sync::Arc;

    let view = DirectoryTreeView::assemble(
        Arc::new(DirectoryTreeTreeSnapshot {
            places_loading: true,
            ..Default::default()
        }),
        Arc::new(super::domains::DirectoryTreeListSnapshot::default()),
        Arc::new(super::domains::DirectoryTreePreviewSnapshot::default()),
    );
    assert!(view.pre_places_folder_display_root().is_none());
}

#[test]
fn pre_places_folder_display_root_none_without_bootstrap_nodes() {
    use super::domains::DirectoryTreeTreeSnapshot;
    use super::view::DirectoryTreeView;
    use std::sync::Arc;

    let browse = PathBuf::from(r"F:\iphone15\2026-05-27");
    let mount_root = super::namespace::drive_mount_namespace_path(Path::new(r"F:\"));
    let via_mount = super::namespace::namespace_child_path(
        &super::namespace::namespace_child_path(
            &mount_root,
            Path::new(r"F:\"),
            &PathBuf::from(r"F:\iphone15"),
        ),
        &PathBuf::from(r"F:\iphone15"),
        &browse,
    );

    let view = DirectoryTreeView::assemble(
        Arc::new(DirectoryTreeTreeSnapshot {
            places_loading: true,
            selected_namespace_path: Some(via_mount),
            ..Default::default()
        }),
        Arc::new(super::domains::DirectoryTreeListSnapshot::default()),
        Arc::new(super::domains::DirectoryTreePreviewSnapshot::default()),
    );
    assert!(
        view.pre_places_folder_display_root().is_none(),
        "without bootstrap nodes in the snapshot, paint falls back to loading status only"
    );
}

#[test]
fn pre_places_folder_display_root_returns_mount_when_bootstrap_node_exists() {
    use super::domains::DirectoryTreeTreeSnapshot;
    use super::view::DirectoryTreeView;
    use std::collections::HashMap;
    use std::sync::Arc;

    let mount_root = super::namespace::drive_mount_namespace_path(Path::new(r"F:\"));
    let browse = PathBuf::from(r"F:\Photos");
    let photos = super::namespace::namespace_child_path(&mount_root, Path::new(r"F:\"), &browse);
    let mut nodes = HashMap::new();
    nodes.insert(
        mount_root.clone(),
        Arc::new(super::directory_tree_node(
            "F:\\".to_string(),
            PathBuf::from(r"F:\"),
        )),
    );

    let view = DirectoryTreeView::assemble(
        Arc::new(DirectoryTreeTreeSnapshot {
            places_loading: true,
            selected_namespace_path: Some(photos),
            nodes,
            ..Default::default()
        }),
        Arc::new(super::domains::DirectoryTreeListSnapshot::default()),
        Arc::new(super::domains::DirectoryTreePreviewSnapshot::default()),
    );
    assert_eq!(
        view.pre_places_folder_display_root().as_deref(),
        Some(mount_root.as_path())
    );
}

#[test]
fn fs_path_for_namespace_node_pre_places_none_without_mount_roots() {
    let mount = super::namespace::drive_mount_namespace_path(Path::new(r"Z:\"));
    let mut tree = DirectoryTreeTreeState::default();
    tree.selected_fs_path = Some(PathBuf::from("relative/no/volume/file.jpg"));
    assert!(
        tree.fs_path_for_namespace_node_pre_places(&mount).is_none(),
        "unresolved relative paths yield no mount roots and no fs mapping"
    );
}

#[test]
fn apply_children_omits_network_share_roots_from_filesystem_parent() {
    use crate::directory_tree_places::types::DriveEntry;

    let share = PathBuf::from(r"\\server\photos");
    let parent_browse = PathBuf::from(r"\\server");
    let other_browse = PathBuf::from(r"\\server\other");
    let places = crate::directory_tree_places::DirectoryTreePlaces {
        known_folders: Vec::new(),
        drives: vec![DriveEntry {
            display_name: "C:".to_string(),
            fs_path: PathBuf::from("C:\\"),
        }],
        network_locations: vec![DriveEntry {
            display_name: "photos".to_string(),
            fs_path: share.clone(),
        }],
        this_pc_label: "Places".to_string(),
        network_label: "Network".to_string(),
    };

    let mut state = DirectoryTreeState::default();
    state.initialize_places(places);
    let parent_tree = super::namespace::drive_mount_namespace_path(&parent_browse);
    state
        .tree
        .nodes
        .or_insert_with(parent_tree.clone(), super::MAX_DIRECTORY_TREE_NODES, || {
            super::directory_tree_node("server".to_string(), parent_browse.clone())
        })
        .expect("parent node");

    state.apply_children_result(super::DirectoryChildrenResult {
        namespace_path: parent_tree.clone(),
        generation: state.tree.generation,
        result: Ok(vec![share.clone(), other_browse.clone()]),
    });

    let node = state
        .tree
        .nodes
        .get(&parent_tree)
        .expect("parent node after load");
    let share_tree = super::namespace::network_share_namespace_path(&share);
    assert!(
        !node
            .children
            .iter()
            .any(|path| path.as_os_str() == share_tree.as_os_str())
    );
    let other_tree =
        super::namespace::namespace_child_path(&parent_tree, &parent_browse, &other_browse);
    assert!(
        node.children
            .iter()
            .any(|path| path.as_os_str() == other_tree.as_os_str())
    );
}

#[test]
fn restore_tree_selection_uses_persisted_namespace_from_settings() {
    use crate::directory_tree_places::types::DriveEntry;

    let happy = PathBuf::from("/run/media/happy");
    let browse = happy.join("CDROM").join("custom");
    let root_mount = super::namespace::drive_mount_namespace_path(Path::new("/"));
    let via_root = super::namespace::namespace_child_path(
        &super::namespace::namespace_child_path(
            &super::namespace::namespace_child_path(
                &super::namespace::namespace_child_path(
                    &super::namespace::namespace_child_path(
                        &root_mount,
                        Path::new("/"),
                        &PathBuf::from("/run"),
                    ),
                    &PathBuf::from("/run"),
                    &PathBuf::from("/run/media"),
                ),
                &PathBuf::from("/run/media"),
                &happy,
            ),
            &happy,
            &happy.join("CDROM"),
        ),
        &happy.join("CDROM"),
        &browse,
    );

    let places = crate::directory_tree_places::DirectoryTreePlaces {
        known_folders: Vec::new(),
        drives: vec![
            DriveEntry {
                display_name: "/".to_string(),
                fs_path: PathBuf::from("/"),
            },
            DriveEntry {
                display_name: "happy".to_string(),
                fs_path: happy.clone(),
            },
        ],
        network_locations: Vec::new(),
        this_pc_label: "Places".to_string(),
        network_label: "Network".to_string(),
    };

    let mut state = DirectoryTreeState::default();
    state.initialize_places(places);
    state
        .tree
        .restore_tree_selection(browse.clone(), Some(via_root.clone()));
    assert_eq!(state.tree.selected_fs_path.as_ref(), Some(&browse));
    assert_eq!(state.tree.selected_namespace_path.as_ref(), Some(&via_root));
}

#[test]
fn restore_tree_selection_falls_back_when_saved_namespace_does_not_match_dir() {
    use crate::directory_tree_places::types::DriveEntry;

    let happy = PathBuf::from("/run/media/happy");
    let browse = happy.join("CDROM").join("custom");
    let other = happy.join("CDROM").join("other");
    let root_mount = super::namespace::drive_mount_namespace_path(Path::new("/"));
    let via_root = super::namespace::namespace_child_path(
        &super::namespace::namespace_child_path(
            &super::namespace::namespace_child_path(
                &super::namespace::namespace_child_path(
                    &super::namespace::namespace_child_path(
                        &root_mount,
                        Path::new("/"),
                        &PathBuf::from("/run"),
                    ),
                    &PathBuf::from("/run"),
                    &PathBuf::from("/run/media"),
                ),
                &PathBuf::from("/run/media"),
                &happy,
            ),
            &happy,
            &happy.join("CDROM"),
        ),
        &happy.join("CDROM"),
        &browse,
    );

    let places = crate::directory_tree_places::DirectoryTreePlaces {
        known_folders: Vec::new(),
        drives: vec![
            DriveEntry {
                display_name: "/".to_string(),
                fs_path: PathBuf::from("/"),
            },
            DriveEntry {
                display_name: "happy".to_string(),
                fs_path: happy.clone(),
            },
        ],
        network_locations: Vec::new(),
        this_pc_label: "Places".to_string(),
        network_label: "Network".to_string(),
    };

    let mut state = DirectoryTreeState::default();
    state.initialize_places(places);
    state
        .tree
        .restore_tree_selection(other.clone(), Some(via_root.clone()));
    assert_eq!(state.tree.selected_fs_path.as_ref(), Some(&other));
    assert_ne!(state.tree.selected_namespace_path.as_ref(), Some(&via_root));
}

#[test]
fn restore_tree_selection_clears_stale_namespace_when_persisted_none() {
    use crate::directory_tree_places::types::DriveEntry;

    let happy = PathBuf::from("/run/media/happy");
    let browse = happy.join("CDROM").join("custom");
    let root_mount = super::namespace::drive_mount_namespace_path(Path::new("/"));
    let via_root = super::namespace::namespace_child_path(
        &super::namespace::namespace_child_path(
            &super::namespace::namespace_child_path(
                &super::namespace::namespace_child_path(
                    &super::namespace::namespace_child_path(
                        &root_mount,
                        Path::new("/"),
                        &PathBuf::from("/run"),
                    ),
                    &PathBuf::from("/run"),
                    &PathBuf::from("/run/media"),
                ),
                &PathBuf::from("/run/media"),
                &happy,
            ),
            &happy,
            &happy.join("CDROM"),
        ),
        &happy.join("CDROM"),
        &browse,
    );
    let happy_mount = super::namespace::drive_mount_namespace_path(&happy);
    let via_happy = super::namespace::namespace_child_path(
        &super::namespace::namespace_child_path(&happy_mount, &happy, &happy.join("CDROM")),
        &happy.join("CDROM"),
        &browse,
    );

    let places = crate::directory_tree_places::DirectoryTreePlaces {
        known_folders: Vec::new(),
        drives: vec![
            DriveEntry {
                display_name: "/".to_string(),
                fs_path: PathBuf::from("/"),
            },
            DriveEntry {
                display_name: "happy".to_string(),
                fs_path: happy.clone(),
            },
        ],
        network_locations: Vec::new(),
        this_pc_label: "Places".to_string(),
        network_label: "Network".to_string(),
    };

    let mut state = DirectoryTreeState::default();
    state.initialize_places(places);
    state
        .tree
        .set_selected_namespace_node(via_root.clone(), browse.clone());
    assert_eq!(state.tree.selected_namespace_path.as_ref(), Some(&via_root));

    state.tree.restore_tree_selection(browse.clone(), None);
    assert_eq!(state.tree.selected_fs_path.as_ref(), Some(&browse));
    assert_eq!(
        state.tree.selected_namespace_path.as_ref(),
        Some(&via_happy)
    );
}

#[test]
fn reveal_selected_namespace_follows_namespace_path_not_browse_alias() {
    use crate::directory_tree_places::types::DriveEntry;

    let happy = PathBuf::from("/run/media/happy");
    let custom = happy.join("CDROM").join("custom");
    let root_mount = super::namespace::drive_mount_namespace_path(Path::new("/"));
    let happy_mount = super::namespace::drive_mount_namespace_path(&happy);
    let via_happy_custom = super::namespace::namespace_child_path(
        &super::namespace::namespace_child_path(&happy_mount, &happy, &happy.join("CDROM")),
        &happy.join("CDROM"),
        &custom,
    );
    let via_root_custom = super::namespace::namespace_child_path(
        &super::namespace::namespace_child_path(
            &super::namespace::namespace_child_path(
                &super::namespace::namespace_child_path(
                    &super::namespace::namespace_child_path(
                        &root_mount,
                        Path::new("/"),
                        &PathBuf::from("/run"),
                    ),
                    &PathBuf::from("/run"),
                    &PathBuf::from("/run/media"),
                ),
                &PathBuf::from("/run/media"),
                &happy,
            ),
            &happy,
            &happy.join("CDROM"),
        ),
        &happy.join("CDROM"),
        &custom,
    );

    let places = crate::directory_tree_places::DirectoryTreePlaces {
        known_folders: Vec::new(),
        drives: vec![
            DriveEntry {
                display_name: "/".to_string(),
                fs_path: PathBuf::from("/"),
            },
            DriveEntry {
                display_name: "happy".to_string(),
                fs_path: happy.clone(),
            },
        ],
        network_locations: Vec::new(),
        this_pc_label: "Places".to_string(),
        network_label: "Network".to_string(),
    };

    let mut state = DirectoryTreeState::default();
    state.initialize_places(places);
    state
        .tree
        .set_selected_namespace_node(via_happy_custom.clone(), custom.clone());
    let requests = state.reveal_selected_namespace();
    assert!(
        !requests
            .iter()
            .any(|request| request.namespace_path == PathBuf::from("/run")),
        "reveal must not expand filesystem-derived /run branch when namespace branch is selected"
    );
    assert!(
        !requests
            .iter()
            .any(|request| request.namespace_path == via_root_custom),
        "reveal must not touch the parallel namespace alias for the same browse path"
    );
    assert_eq!(
        state.tree.selected_namespace_path.as_deref(),
        Some(via_happy_custom.as_path())
    );
}

#[test]
fn reveal_selected_namespace_does_not_flatten_mount_children() {
    use crate::directory_tree_places::types::DriveEntry;

    let happy = PathBuf::from("/run/media/happy");
    let cdrom = happy.join("CDROM");
    let custom = cdrom.join("custom");
    let isolinux = cdrom.join("isolinux");
    let happy_mount = super::namespace::drive_mount_namespace_path(&happy);
    let cdrom_tree = super::namespace::namespace_child_path(&happy_mount, &happy, &cdrom);
    let custom_tree = super::namespace::namespace_child_path(&cdrom_tree, &cdrom, &custom);
    let isolinux_tree = super::namespace::namespace_child_path(&cdrom_tree, &cdrom, &isolinux);

    let places = crate::directory_tree_places::DirectoryTreePlaces {
        known_folders: Vec::new(),
        drives: vec![
            DriveEntry {
                display_name: "/".to_string(),
                fs_path: PathBuf::from("/"),
            },
            DriveEntry {
                display_name: "happy".to_string(),
                fs_path: happy.clone(),
            },
        ],
        network_locations: Vec::new(),
        this_pc_label: "Places".to_string(),
        network_label: "Network".to_string(),
    };

    let mut state = DirectoryTreeState::default();
    state.initialize_places(places);
    state.apply_children_result(DirectoryChildrenResult {
        namespace_path: happy_mount.clone(),
        generation: state.tree.generation,
        result: Ok(vec![cdrom.clone()]),
    });
    state.apply_children_result(DirectoryChildrenResult {
        namespace_path: cdrom_tree.clone(),
        generation: state.tree.generation,
        result: Ok(vec![custom.clone(), isolinux.clone()]),
    });
    state.set_selected_fs_path(custom.clone());
    let _requests = state.reveal_selected_namespace();

    let happy_node = state
        .tree
        .nodes
        .get(&happy_mount)
        .expect("happy mount node");
    assert_eq!(happy_node.children, vec![cdrom_tree.clone()]);
    assert!(
        !happy_node
            .children
            .iter()
            .any(|path| path.as_os_str() == custom_tree.as_os_str())
    );
    assert!(
        !happy_node
            .children
            .iter()
            .any(|path| path.as_os_str() == isolinux_tree.as_os_str())
    );
    let cdrom_node = state.tree.nodes.get(&cdrom_tree).expect("cdrom node");
    assert!(
        cdrom_node
            .children
            .iter()
            .any(|path| path.as_os_str() == custom_tree.as_os_str())
    );
    assert!(
        cdrom_node
            .children
            .iter()
            .any(|path| path.as_os_str() == isolinux_tree.as_os_str())
    );
}

#[test]
fn begin_paint_frame_preserves_folder_scroll_offset_from_chrome() {
    use super::view::{DirectoryTreeUiChrome, DirectoryTreeView};
    use std::sync::Arc;

    let tree = DirectoryTreeTreeState::default();
    let list = DirectoryTreeListState::default();
    let mut chrome = DirectoryTreeUiChrome::from_domains(&tree, &list);
    chrome.folder_scroll_offset_y = 240.0;

    let view = DirectoryTreeView::assemble(
        Arc::new(super::domains::DirectoryTreeTreeSnapshot::default()),
        Arc::new(super::domains::DirectoryTreeListSnapshot::default()),
        Arc::new(super::domains::DirectoryTreePreviewSnapshot::default()),
    );
    chrome.begin_paint_frame(&view, false);

    assert_eq!(chrome.folder_scroll_offset_y, 240.0);
}

#[test]
fn begin_paint_frame_preserves_keyboard_list_selection_from_chrome() {
    use super::view::{DirectoryTreeUiChrome, DirectoryTreeView};
    use std::sync::Arc;

    let tree = DirectoryTreeTreeState::default();
    let list = DirectoryTreeListState::default();
    let mut chrome = DirectoryTreeUiChrome::from_domains(&tree, &list);
    chrome.current_index = 1;

    let view = DirectoryTreeView::assemble(
        Arc::new(super::domains::DirectoryTreeTreeSnapshot::default()),
        Arc::new(super::domains::DirectoryTreeListSnapshot {
            current_index: 0,
            ..Default::default()
        }),
        Arc::new(super::domains::DirectoryTreePreviewSnapshot::default()),
    );
    chrome.begin_paint_frame(&view, true);

    assert_eq!(chrome.current_index, 1);
}

#[test]
fn begin_paint_frame_promotes_folder_scroll_to_selected_without_clobbering_clear() {
    use super::view::{DirectoryTreeUiChrome, DirectoryTreeView};
    use std::sync::Arc;

    let tree = DirectoryTreeTreeState::default();
    let list = DirectoryTreeListState::default();
    let mut chrome = DirectoryTreeUiChrome::from_domains(&tree, &list);
    chrome.scroll_folder_tree_to_selected = false;

    let view = DirectoryTreeView::assemble(
        Arc::new(super::domains::DirectoryTreeTreeSnapshot {
            scroll_folder_tree_to_selected: true,
            ..Default::default()
        }),
        Arc::new(super::domains::DirectoryTreeListSnapshot::default()),
        Arc::new(super::domains::DirectoryTreePreviewSnapshot::default()),
    );
    chrome.begin_paint_frame(&view, false);

    assert!(chrome.scroll_folder_tree_to_selected);
}

#[test]
fn folder_reveal_work_needs_repaint_tracks_scroll_flag() {
    use super::visibility::folder_reveal_work_needs_repaint;

    let mut tree = DirectoryTreeTreeState::default();
    assert!(!folder_reveal_work_needs_repaint(&tree));
    tree.scroll_folder_tree_to_selected = true;
    assert!(folder_reveal_work_needs_repaint(&tree));
}

#[test]
fn apply_to_domains_marks_list_snapshot_dirty_when_image_scroll_clears() {
    use super::view::DirectoryTreeUiChrome;

    let tree = DirectoryTreeTreeState::default();
    let mut list = DirectoryTreeListState::default();
    list.scroll_image_list_to_current = true;
    list.snapshot_dirty = false;
    let mut chrome = DirectoryTreeUiChrome::from_domains(&tree, &list);
    chrome.scroll_image_list_to_current = false;

    let mut tree = tree;
    chrome.apply_to_domains(&mut tree, &mut list);

    assert!(!list.scroll_image_list_to_current);
    assert!(list.snapshot_dirty);
}

#[test]
fn apply_to_domains_marks_tree_snapshot_dirty_when_left_panel_resized() {
    use super::view::DirectoryTreeUiChrome;

    let mut tree = DirectoryTreeTreeState::default();
    tree.snapshot_dirty = false;
    let mut list = DirectoryTreeListState::default();
    let mut chrome = DirectoryTreeUiChrome::from_domains(&tree, &list);
    chrome.left_panel_width = tree.left_panel_width + 24.0;
    chrome.panel_layout_dirty = true;

    chrome.apply_to_domains(&mut tree, &mut list);

    assert_eq!(tree.left_panel_width, chrome.left_panel_width);
    assert!(tree.snapshot_dirty);
    assert!(tree.panel_layout_dirty);
}

#[test]
fn pointer_in_directory_tree_nav_block_rect_respects_bounds() {
    use super::ui::pointer_in_directory_tree_nav_block_rect;

    let rect = egui::Rect::from_min_max(egui::pos2(10.0, 20.0), egui::pos2(110.0, 220.0));
    assert!(pointer_in_directory_tree_nav_block_rect(
        Some(egui::pos2(50.0, 100.0)),
        Some(rect),
    ));
    assert!(!pointer_in_directory_tree_nav_block_rect(
        Some(egui::pos2(200.0, 100.0)),
        Some(rect),
    ));
    assert!(!pointer_in_directory_tree_nav_block_rect(None, Some(rect)));
}

#[test]
fn coalesce_children_requests_keeps_latest_per_namespace_path() {
    use super::workers::coalesce_children_requests;

    let (tx, rx) = crossbeam_channel::unbounded();
    let root = PathBuf::from("/tmp/coalesce-root");
    let first = DirectoryChildrenRequest {
        namespace_path: root.clone(),
        fs_path: PathBuf::from("/browse/old"),
        generation: 1,
    };
    tx.send(DirectoryChildrenRequest {
        namespace_path: root.clone(),
        fs_path: PathBuf::from("/browse/new"),
        generation: 2,
    })
    .expect("queue coalesced request");
    let out = coalesce_children_requests(first, &rx);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].fs_path, PathBuf::from("/browse/new"));
    assert_eq!(out[0].generation, 2);
}

#[test]
fn coalesce_metadata_requests_merges_same_generation() {
    use super::workers::coalesce_metadata_requests;

    let (tx, rx) = crossbeam_channel::unbounded();
    let first = FileMetadataRequest {
        generation: 7,
        paths: vec![PathBuf::from("/a.jpg")],
    };
    tx.send(FileMetadataRequest {
        generation: 7,
        paths: vec![PathBuf::from("/b.jpg"), PathBuf::from("/c.jpg")],
    })
    .expect("queue coalesced metadata request");
    tx.send(FileMetadataRequest {
        generation: 8,
        paths: vec![PathBuf::from("/d.jpg")],
    })
    .expect("queue other generation");
    let out = coalesce_metadata_requests(first, &rx);
    assert_eq!(out.len(), 2);
    let gen7 = out
        .iter()
        .find(|request| request.generation == 7)
        .expect("generation 7 batch");
    assert_eq!(gen7.paths.len(), 3);
}

#[test]
fn split_metadata_request_chunks_large_batches() {
    use super::workers::{METADATA_BATCH_SIZE, split_metadata_request};

    let paths: Vec<PathBuf> = (0..METADATA_BATCH_SIZE + 50)
        .map(|i| PathBuf::from(format!("/tmp/file_{i}.jpg")))
        .collect();
    let request = FileMetadataRequest {
        generation: 1,
        paths,
    };
    let chunks = split_metadata_request(request);
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0].paths.len(), METADATA_BATCH_SIZE);
    assert_eq!(chunks[1].paths.len(), 50);
}

#[test]
fn mark_children_request_failed_clears_loading_and_sets_error() {
    let namespace_path = PathBuf::from("/tmp/siv-dir-tree-failed-node");
    let mut state = DirectoryTreeState::default();
    let _ = state.tree.nodes.insert(
        namespace_path.clone(),
        DirectoryTreeNode {
            display_name: "failed".to_string(),
            fs_path: namespace_path.clone(),
            expanded: false,
            loading: true,
            children_loaded: false,
            children: Vec::new(),
            error: None,
        },
        super::MAX_DIRECTORY_TREE_NODES,
    );

    state.mark_children_request_failed(&namespace_path, "read busy".to_string());

    let node = state.tree.nodes.get(&namespace_path).expect("node");
    assert!(!node.loading);
    assert_eq!(node.error.as_deref(), Some("read busy"));
}

#[test]
fn sync_images_sort_active_inserts_new_paths_without_duplicates() {
    let mut state = DirectoryTreeState::default();
    state.list.image_list_sort_active = true;
    let path_a = PathBuf::from("/dir/b.jpg");
    let path_b = PathBuf::from("/dir/a.jpg");
    state.list.image_rows = vec![
        DirectoryTreeFileRow::new(path_a.clone(), "b".to_string(), 1, None),
        DirectoryTreeFileRow::new(path_b.clone(), "a".to_string(), 2, None),
    ];
    let path_c = PathBuf::from("/dir/c.jpg");
    let images = vec![path_b.clone(), path_a.clone(), path_c.clone()];
    state.sync_images(
        &images,
        &[2, 1, 3],
        &[None, None, None],
        0,
        true,
        String::new(),
    );
    assert_eq!(state.list.image_rows.len(), 3);
    assert_eq!(state.list.image_rows[0].path, path_a);
    assert_eq!(state.list.image_rows[0].size_bytes, 1);
    assert_eq!(state.list.image_rows[1].path, path_b);
    assert_eq!(state.list.image_rows[1].size_bytes, 2);
    assert_eq!(state.list.image_rows[2].path, path_c);
    assert_eq!(state.list.image_rows[2].size_bytes, 3);
    assert!(state.list.image_rows.iter().any(|row| row.path == path_c));
    assert_eq!(
        state
            .list
            .image_rows
            .iter()
            .filter(|row| row.path == path_a)
            .count(),
        1
    );
}

#[test]
fn sync_images_realigns_sizes_after_image_order_changes() {
    let mut state = DirectoryTreeState::default();
    let path_small = PathBuf::from("/dir/small.jpg");
    let path_large = PathBuf::from("/dir/large.psb");
    state.list.image_rows = vec![
        DirectoryTreeFileRow::new(path_small.clone(), "small".to_string(), 100, None),
        DirectoryTreeFileRow::new(path_large.clone(), "large".to_string(), 200, None),
    ];
    let images = vec![path_large.clone(), path_small.clone()];
    let sizes = vec![4_637_379_310_u64, 7_962_624_u64];
    state.sync_images(&images, &sizes, &[None, None], 0, false, String::new());
    assert_eq!(state.list.image_rows[0].path, path_large);
    assert_eq!(state.list.image_rows[0].size_bytes, 4_637_379_310);
    assert_eq!(state.list.image_rows[1].path, path_small);
    assert_eq!(state.list.image_rows[1].size_bytes, 7_962_624);
}

#[test]
fn directory_tree_view_carries_sync_warning_from_state() {
    use std::sync::Arc;

    use super::domains::{
        DirectoryTreeListSnapshot, DirectoryTreePreviewSnapshot, DirectoryTreeTreeSnapshot,
    };
    use super::view::DirectoryTreeView;

    let mut state = DirectoryTreeState::default();
    state.list.sync_warning = Some("sync dropped".to_string());
    let view = DirectoryTreeView::assemble(
        Arc::new(DirectoryTreeTreeSnapshot::default()),
        Arc::new(DirectoryTreeListSnapshot {
            sync_warning: state.list.sync_warning.clone(),
            ..DirectoryTreeListSnapshot::default()
        }),
        Arc::new(DirectoryTreePreviewSnapshot::default()),
    );
    assert_eq!(view.sync_warning(), Some("sync dropped"));
}

#[test]
fn appended_image_rows_affect_visible_only_when_in_viewport() {
    use super::visibility::appended_image_rows_affect_visible;

    assert!(appended_image_rows_affect_visible(0, 5, None));
    assert!(appended_image_rows_affect_visible(10, 15, Some((0, 12))));
    assert!(!appended_image_rows_affect_visible(10, 20, Some((0, 10))));
    assert!(!appended_image_rows_affect_visible(100, 200, Some((5, 15))));
}

#[test]
fn initialize_places_preserves_bootstrap_mount_nodes() {
    use crate::directory_tree_places::types::DriveEntry;

    let mount_root = super::namespace::drive_mount_namespace_path(Path::new(r"F:\"));
    let child = super::namespace::namespace_child_path(
        &mount_root,
        Path::new(r"F:\"),
        &PathBuf::from(r"F:\photos"),
    );
    let mut state = DirectoryTreeState::default();
    let _ = state.tree.nodes.insert(
        mount_root.clone(),
        DirectoryTreeNode {
            display_name: "F:\\".to_string(),
            fs_path: PathBuf::from(r"F:\"),
            expanded: true,
            loading: false,
            children_loaded: true,
            children: vec![child.clone()],
            error: None,
        },
        super::MAX_DIRECTORY_TREE_NODES,
    );
    let _ = state.tree.nodes.insert(
        child.clone(),
        DirectoryTreeNode {
            display_name: "photos".to_string(),
            fs_path: PathBuf::from(r"F:\photos"),
            expanded: false,
            loading: false,
            children_loaded: false,
            children: Vec::new(),
            error: None,
        },
        super::MAX_DIRECTORY_TREE_NODES,
    );

    let places = crate::directory_tree_places::DirectoryTreePlaces {
        this_pc_label: "This PC".to_string(),
        known_folders: Vec::new(),
        drives: vec![DriveEntry {
            display_name: "Local Disk (F:)".to_string(),
            fs_path: PathBuf::from(r"F:\"),
        }],
        network_locations: Vec::new(),
        network_label: "Network".to_string(),
    };
    state.tree.initialize_places(places);

    assert!(state.tree.nodes.contains_key(&child));
    assert!(state.tree.places_loaded);
}
