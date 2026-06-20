use super::sort::{
    compare_image_list_sort_keys, compare_optional_unix_time, image_list_sort_order,
};
use super::ui::{
    DirectoryTreeNodeIcon, clamp_directory_tree_left_panel_width, directory_ancestor_chain,
    directory_display_name, directory_tree_left_panel_width_limits,
    directory_tree_node_icon_fields, directory_tree_panel_layout, filesystem_ancestor_chain,
    image_list_column_layout, image_list_modified_column, image_list_name_column,
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
    state.tree.selected_dir = Some(root.clone());
    state.tree.generation = 2;
    let _ = state.tree.nodes.insert(
        root.clone(),
        DirectoryTreeNode {
            display_name: "root".to_string(),
            browse_path: root.clone(),
            expanded: true,
            loading: true,
            children_loaded: false,
            children: Vec::new(),
            error: None,
        },
        super::MAX_DIRECTORY_TREE_NODES,
    );

    state.apply_children_result(DirectoryChildrenResult {
        tree_path: root.clone(),
        generation: 1,
        result: Ok(vec![child.clone()]),
    });

    let node = state.tree.nodes.get(&root).expect("root node");
    assert!(node.loading);
    assert!(!node.children_loaded);
    assert!(node.children.is_empty());
    assert!(!state.tree.nodes.contains_key(&child));
}

#[test]
fn apply_children_result_merges_children_and_clears_loading() {
    let root = PathBuf::from("/tmp/siv-dir-tree-test-root-2");
    let child = PathBuf::from("/tmp/siv-dir-tree-test-child-2");

    let mut state = DirectoryTreeState::default();
    state.tree.places_loaded = true;
    state.tree.selected_dir = Some(root.clone());
    state.tree.generation = 1;
    let _ = state.tree.nodes.insert(
        root.clone(),
        DirectoryTreeNode {
            display_name: "root".to_string(),
            browse_path: root.clone(),
            expanded: true,
            loading: true,
            children_loaded: false,
            children: Vec::new(),
            error: None,
        },
        super::MAX_DIRECTORY_TREE_NODES,
    );

    state.apply_children_result(DirectoryChildrenResult {
        tree_path: root.clone(),
        generation: 1,
        result: Ok(vec![child.clone()]),
    });

    let node = state.tree.nodes.get(&root).expect("root node");
    assert!(!node.loading);
    assert!(node.children_loaded);
    assert_eq!(node.children, vec![child.clone()]);
    assert!(state.tree.nodes.contains_key(&child));
}

#[test]
fn apply_children_result_records_read_error() {
    let root = PathBuf::from("/tmp/siv-dir-tree-test-missing");

    let mut state = DirectoryTreeState::default();
    state.tree.places_loaded = true;
    state.tree.selected_dir = Some(root.clone());
    state.tree.generation = 1;
    let _ = state.tree.nodes.insert(
        root.clone(),
        DirectoryTreeNode {
            display_name: "root".to_string(),
            browse_path: root.clone(),
            expanded: true,
            loading: true,
            children_loaded: false,
            children: Vec::new(),
            error: None,
        },
        super::MAX_DIRECTORY_TREE_NODES,
    );

    state.apply_children_result(DirectoryChildrenResult {
        tree_path: root.clone(),
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
    state.list.image_rows = vec![DirectoryTreeFileRow {
        path: PathBuf::from("/tmp/a.jpg"),
        name: "a.jpg".to_string(),
        size_bytes: 10,
        modified_unix: None,
    }];

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
        DirectoryTreeFileRow {
            path: PathBuf::from("/tmp/a.jpg"),
            name: "a.jpg".to_string(),
            size_bytes: 10,
            modified_unix: None,
        },
        DirectoryTreeFileRow {
            path: PathBuf::from("/tmp/b.jpg"),
            name: "b.jpg".to_string(),
            size_bytes: 20,
            modified_unix: None,
        },
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
    assert_eq!(min, 174.0);
    assert_eq!(clamp_directory_tree_left_panel_width(340.0, 364.0), 174.0);
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
    assert!(left + list <= 354.0);
    assert!(list >= DIRECTORY_TREE_RIGHT_MIN_WIDTH);
    // Stored preferences are unchanged — only the layout tuple shrinks.
    let mut state = DirectoryTreeState::default();
    state.tree.left_panel_width = 340.0;
    state.list.image_list_panel_width = 400.0;
    assert_eq!(state.tree.left_panel_width, 340.0);
    assert_eq!(state.list.image_list_panel_width, 400.0);
}

#[test]
    fn visible_cold_strip_indices_skips_stale_range_while_scroll_pending() {
    assert!(
        ImageViewerApp::visible_cold_strip_indices(Some((100, 110)), true, 200, false).is_empty()
    );
    assert_eq!(
        ImageViewerApp::visible_cold_strip_indices(Some((100, 110)), true, 200, true),
        (100..110).collect::<Vec<_>>()
    );
    assert_eq!(
        ImageViewerApp::visible_cold_strip_indices(Some((100, 110)), false, 200, false),
        (100..110).collect::<Vec<_>>()
    );
}

#[test]
fn sync_images_marks_list_scroll_when_current_index_changes() {
    let paths = vec![PathBuf::from("/tmp/a.avif"), PathBuf::from("/tmp/b.avif")];
    let mut state = DirectoryTreeState::default();
    state.list.image_rows = paths
        .iter()
        .map(|path| DirectoryTreeFileRow {
            path: path.clone(),
            name: directory_display_name(path),
            size_bytes: 0,
            modified_unix: None,
        })
        .collect();
    state.list.current_index = 0;
    state.list.scroll_image_list_to_current = false;

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
fn reveal_selected_dir_mounts_unc_share_under_network() {
    let places = DirectoryTreePlaces {
        known_folders: Vec::new(),
        drives: Vec::new(),
        network_locations: Vec::new(),
        this_pc_label: "This PC".to_string(),
        network_label: "Network".to_string(),
    };

    let mut state = DirectoryTreeState::default();
    state.initialize_places(places);
    state.set_selected_dir(PathBuf::from("//192.168.2.1/pictures/2024"));

    assert!(state.tree.network_visible);
    let network = state
        .tree
        .nodes
        .get(&network_tree_path())
        .expect("network node");
    assert_eq!(
        network.children,
        vec![PathBuf::from("//192.168.2.1/pictures")]
    );
    assert!(
        state
            .tree
            .nodes
            .contains_key(&PathBuf::from("//192.168.2.1/pictures"))
    );
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
    assert!(state.tree.nodes.contains_key(&this_pc_tree_path()));
    assert!(!state.tree.network_visible);
    assert!(!state.tree.nodes.contains_key(&network_tree_path()));

    let _ = state.tree.nodes.insert(
        PathBuf::from("/tmp/siv-dir-tree-stale"),
        directory_tree_node("stale", PathBuf::from("/tmp/siv-dir-tree-stale")),
        super::MAX_DIRECTORY_TREE_NODES,
    );

    state.initialize_places(places);
    assert_eq!(state.tree.generation, 2);
    assert_eq!(state.tree.nodes.len(), 1);
    assert!(state.tree.nodes.contains_key(&this_pc_tree_path()));
    assert!(!state.tree.network_visible);
    assert!(!state.tree.nodes.contains_key(&network_tree_path()));
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
    use crate::directory_tree_places::types::{KnownFolderKind, known_folder_tree_path};

    let docs_fs = PathBuf::from("/tmp/siv-known-docs");
    let drive = PathBuf::from("/tmp/siv-drive");
    let places = DirectoryTreePlaces {
        known_folders: vec![KnownFolderEntry {
            kind: KnownFolderKind::Pictures,
            display_name: "Pictures".to_string(),
            tree_path: known_folder_tree_path(KnownFolderKind::Pictures),
            filesystem_path: docs_fs.clone(),
        }],
        drives: vec![crate::directory_tree_places::types::DriveEntry {
            display_name: "Data".to_string(),
            path: drive.clone(),
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
            &this_pc_tree_path()
        ),
        DirectoryTreeNodeIcon::ThisPc
    );
    assert_eq!(
        directory_tree_node_icon_fields(
            &state.tree.known_folders,
            &state.tree.nodes,
            &network_tree_path()
        ),
        DirectoryTreeNodeIcon::Network
    );
    assert_eq!(
        directory_tree_node_icon_fields(
            &state.tree.known_folders,
            &state.tree.nodes,
            &known_folder_tree_path(KnownFolderKind::Pictures),
        ),
        DirectoryTreeNodeIcon::KnownFolder(KnownFolderKind::Pictures)
    );
    assert_eq!(
        directory_tree_node_icon_fields(&state.tree.known_folders, &state.tree.nodes, &drive),
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
fn reveal_known_folder_does_not_expand_this_pc() {
    use crate::directory_tree_places::types::{KnownFolderKind, known_folder_tree_path};

    let docs_fs = PathBuf::from("/tmp/siv-known-docs");
    let places = DirectoryTreePlaces {
        known_folders: vec![KnownFolderEntry {
            kind: KnownFolderKind::Documents,
            display_name: "Documents".to_string(),
            tree_path: known_folder_tree_path(KnownFolderKind::Documents),
            filesystem_path: docs_fs.clone(),
        }],
        drives: Vec::new(),
        network_locations: Vec::new(),
        this_pc_label: "This PC".to_string(),
        network_label: "Network".to_string(),
    };

    let mut state = DirectoryTreeState::default();
    state.initialize_places(places);
    state.set_selected_dir(docs_fs);
    let _requests = state.reveal_selected_dir();
    assert!(
        !state
            .tree
            .nodes
            .get(&this_pc_tree_path())
            .is_some_and(|node| node.expanded)
    );
}

#[test]
fn reveal_selected_dir_expands_nested_known_folder_path_after_places_init() {
    use crate::directory_tree_places::types::{KnownFolderKind, known_folder_tree_path};

    let docs_fs = PathBuf::from("/tmp/siv-known-docs");
    let nested = docs_fs.join("2024").join("06");
    let places = DirectoryTreePlaces {
        known_folders: vec![KnownFolderEntry {
            kind: KnownFolderKind::Documents,
            display_name: "Documents".to_string(),
            tree_path: known_folder_tree_path(KnownFolderKind::Documents),
            filesystem_path: docs_fs.clone(),
        }],
        drives: Vec::new(),
        network_locations: Vec::new(),
        this_pc_label: "This PC".to_string(),
        network_label: "Network".to_string(),
    };

    let mut state = DirectoryTreeState::default();
    assert!(state.reveal_selected_dir().is_empty());
    state.set_selected_dir(nested.clone());
    state.initialize_places(places);
    let requests = state.reveal_selected_dir();
    assert!(!requests.is_empty());
    let docs_tree = known_folder_tree_path(KnownFolderKind::Documents);
    assert!(
        state
            .tree
            .nodes
            .get(&docs_tree)
            .is_some_and(|node| node.expanded)
    );
    assert!(
        state
            .tree
            .nodes
            .get(&docs_fs.join("2024"))
            .is_some_and(|node| node.expanded)
    );
}

#[test]
fn selected_tree_path_distinguishes_alias_nodes_with_same_browse_path() {
    use crate::directory_tree_places::types::{KnownFolderKind, known_folder_tree_path};

    let downloads_fs = PathBuf::from("/home/user/Downloads");
    let known_tree = known_folder_tree_path(KnownFolderKind::Downloads);
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
        .set_selected_tree_node(profile_tree.clone(), downloads_fs.clone());
    assert_eq!(
        state.tree.selected_tree_path.as_deref(),
        Some(profile_tree.as_path())
    );
    assert_eq!(
        state.tree.selected_dir.as_deref(),
        Some(downloads_fs.as_path())
    );

    state
        .tree
        .set_selected_tree_node(known_tree.clone(), downloads_fs);
    assert_eq!(
        state.tree.selected_tree_path.as_deref(),
        Some(known_tree.as_path())
    );
}

#[test]
fn apply_to_domains_marks_tree_snapshot_dirty_when_folder_scroll_clears() {
    use super::view::DirectoryTreeUiChrome;

    let mut tree = DirectoryTreeTreeState::default();
    tree.scroll_folder_to_selected = true;
    tree.snapshot_dirty = false;
    let mut list = DirectoryTreeListState::default();
    let mut chrome = DirectoryTreeUiChrome::from_domains(&tree, &list);
    chrome.scroll_folder_to_selected = false;

    chrome.apply_to_domains(&mut tree, &mut list);

    assert!(!tree.scroll_folder_to_selected);
    assert!(tree.snapshot_dirty);
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
fn coalesce_children_requests_keeps_latest_per_tree_path() {
    use super::workers::coalesce_children_requests;

    let (tx, rx) = crossbeam_channel::unbounded();
    let root = PathBuf::from("/tmp/coalesce-root");
    let first = DirectoryChildrenRequest {
        tree_path: root.clone(),
        browse_path: PathBuf::from("/browse/old"),
        generation: 1,
    };
    tx.send(DirectoryChildrenRequest {
        tree_path: root.clone(),
        browse_path: PathBuf::from("/browse/new"),
        generation: 2,
    })
    .expect("queue coalesced request");
    let out = coalesce_children_requests(first, &rx);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].browse_path, PathBuf::from("/browse/new"));
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
    let tree_path = PathBuf::from("/tmp/siv-dir-tree-failed-node");
    let mut state = DirectoryTreeState::default();
    let _ = state.tree.nodes.insert(
        tree_path.clone(),
        DirectoryTreeNode {
            display_name: "failed".to_string(),
            browse_path: tree_path.clone(),
            expanded: false,
            loading: true,
            children_loaded: false,
            children: Vec::new(),
            error: None,
        },
        super::MAX_DIRECTORY_TREE_NODES,
    );

    state.mark_children_request_failed(&tree_path, "read busy".to_string());

    let node = state.tree.nodes.get(&tree_path).expect("node");
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
        DirectoryTreeFileRow {
            path: path_a.clone(),
            name: "b".to_string(),
            size_bytes: 1,
            modified_unix: None,
        },
        DirectoryTreeFileRow {
            path: path_b.clone(),
            name: "a".to_string(),
            size_bytes: 2,
            modified_unix: None,
        },
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
