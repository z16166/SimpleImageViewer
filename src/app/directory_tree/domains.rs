// Simple Image Viewer - directory-tree domain writers, snapshots, and publish.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use eframe::egui;

use crate::directory_tree_places::KnownFolderEntry;

use super::node_store;
use super::{
    DIRECTORY_TREE_COL_MODIFIED_WIDTH, DIRECTORY_TREE_COL_SIZE_WIDTH, DIRECTORY_TREE_LEFT_WIDTH,
    DIRECTORY_TREE_RIGHT_MIN_WIDTH, DirectoryTreeFileRow, DirectoryTreeListPreviewLayout,
    DirectoryTreeNode, ImageListSortColumn,
};

pub(super) const LIST_PUBLISH_COALESCE: Duration = Duration::from_millis(100);

// --- Tree writer ---------------------------------------------------------------------------

pub(crate) struct DirectoryTreeTreeState {
    pub(crate) places_loaded: bool,
    pub(crate) places_loading: bool,
    pub(crate) places_load_error: Option<String>,
    pub(crate) workers_available: bool,
    pub(crate) known_folders: Vec<KnownFolderEntry>,
    pub(crate) selected_dir: Option<PathBuf>,
    /// Tree-node key last selected in the folder pane (distinct from [`selected_dir`] when aliases share a path).
    pub(crate) selected_tree_path: Option<PathBuf>,
    pub(crate) nodes: node_store::DirectoryTreeNodeArena,
    pub(crate) generation: u64,
    pub(crate) network_label: String,
    pub(crate) network_visible: bool,
    pub(crate) scroll_folder_to_selected: bool,
    pub(crate) left_panel_width: f32,
    pub(crate) embedded_nav_panel_width: f32,
    pub(crate) panel_layout_dirty: bool,
    pub(crate) detached_startup_maximize_applied: bool,
    pub(crate) snapshot_dirty: bool,
}

impl Default for DirectoryTreeTreeState {
    fn default() -> Self {
        Self {
            places_loaded: false,
            places_loading: false,
            places_load_error: None,
            workers_available: true,
            known_folders: Vec::new(),
            selected_dir: None,
            selected_tree_path: None,
            nodes: node_store::DirectoryTreeNodeArena::default(),
            generation: 0,
            network_label: String::new(),
            network_visible: false,
            scroll_folder_to_selected: false,
            left_panel_width: DIRECTORY_TREE_LEFT_WIDTH,
            embedded_nav_panel_width: 0.0,
            panel_layout_dirty: false,
            detached_startup_maximize_applied: false,
            snapshot_dirty: true,
        }
    }
}

// --- List writer ---------------------------------------------------------------------------

pub(crate) struct DirectoryTreeListState {
    pub(crate) image_list_generation: u64,
    pub(crate) file_metadata_generation: u64,
    pub(crate) publish_generation: u64,
    pub(crate) image_rows: Vec<DirectoryTreeFileRow>,
    pub(crate) current_index: usize,
    pub(crate) scanning: bool,
    pub(crate) scan_status: String,
    pub(crate) sync_warning: Option<String>,
    pub(crate) image_list_panel_width: f32,
    pub(crate) panel_layout_dirty: bool,
    pub(crate) scroll_image_list_to_current: bool,
    pub(crate) image_list_scroll_offset_y: f32,
    pub(crate) image_list_keyboard_active: bool,
    pub(crate) image_list_visible_row_range: Option<(usize, usize)>,
    pub(crate) image_list_col_size_w: f32,
    pub(crate) image_list_col_modified_w: f32,
    pub(crate) image_list_col_widths_font_size: f32,
    pub(crate) image_list_col_widths_dirty: bool,
    pub(crate) image_list_sort_column: ImageListSortColumn,
    pub(crate) image_list_sort_ascending: bool,
    pub(crate) image_list_sort_active: bool,
    pub(crate) image_list_reordering: bool,
    pub(crate) show_list_previews: bool,
    pub(crate) list_preview_thumb_px: f32,
    pub(crate) list_preview_strip_max_side: u32,
    pub(crate) snapshot_dirty: bool,
}

impl Default for DirectoryTreeListState {
    fn default() -> Self {
        Self {
            image_list_generation: 0,
            file_metadata_generation: 0,
            publish_generation: 0,
            image_rows: Vec::new(),
            current_index: 0,
            scanning: false,
            scan_status: String::new(),
            sync_warning: None,
            image_list_panel_width: DIRECTORY_TREE_RIGHT_MIN_WIDTH,
            panel_layout_dirty: false,
            scroll_image_list_to_current: false,
            image_list_scroll_offset_y: 0.0,
            image_list_keyboard_active: false,
            image_list_visible_row_range: None,
            image_list_col_size_w: DIRECTORY_TREE_COL_SIZE_WIDTH,
            image_list_col_modified_w: DIRECTORY_TREE_COL_MODIFIED_WIDTH,
            image_list_col_widths_font_size: 0.0,
            image_list_col_widths_dirty: true,
            image_list_sort_column: ImageListSortColumn::default(),
            image_list_sort_ascending: true,
            image_list_sort_active: false,
            image_list_reordering: false,
            show_list_previews: true,
            list_preview_thumb_px: crate::settings::DirectoryTreeListPreviewSize::Small.thumb_px(),
            list_preview_strip_max_side: crate::settings::DirectoryTreeListPreviewSize::Small
                .strip_max_side(),
            snapshot_dirty: true,
        }
    }
}

impl DirectoryTreeListState {
    pub(super) fn mark_snapshot_dirty(&mut self) {
        self.snapshot_dirty = true;
        self.publish_generation = self.publish_generation.wrapping_add(1);
    }

    pub(super) fn apply_preview_layout(&mut self, layout: DirectoryTreeListPreviewLayout) {
        self.show_list_previews = layout.show_previews;
        self.list_preview_thumb_px = layout.thumb_px;
        self.list_preview_strip_max_side = layout.strip_max_side;
        self.mark_snapshot_dirty();
    }
}

// --- Snapshots -----------------------------------------------------------------------------

pub(crate) struct DirectoryTreeTreeSnapshot {
    pub(super) generation: u64,
    pub(super) places_loaded: bool,
    pub(super) places_loading: bool,
    pub(super) places_load_error: Option<String>,
    pub(super) workers_available: bool,
    pub(super) known_folders: Vec<KnownFolderEntry>,
    pub(super) selected_tree_path: Option<PathBuf>,
    pub(super) nodes: HashMap<PathBuf, Arc<DirectoryTreeNode>>,
    pub(super) network_visible: bool,
    pub(super) scroll_folder_to_selected: bool,
    pub(super) left_panel_width: f32,
}

pub(crate) struct DirectoryTreeListSnapshot {
    pub(super) publish_generation: u64,
    pub(super) image_list_generation: u64,
    pub(super) image_rows: Arc<[DirectoryTreeFileRow]>,
    pub(super) current_index: usize,
    pub(super) scanning: bool,
    pub(super) scan_status: String,
    pub(super) sync_warning: Option<String>,
    pub(super) scroll_image_list_to_current: bool,
    pub(super) image_list_col_size_w: f32,
    pub(super) image_list_col_modified_w: f32,
    pub(super) image_list_panel_width: f32,
    pub(super) image_list_sort_column: ImageListSortColumn,
    pub(super) image_list_sort_ascending: bool,
    pub(super) image_list_sort_active: bool,
    pub(super) image_list_reordering: bool,
    pub(super) show_list_previews: bool,
    pub(super) list_preview_thumb_px: f32,
}

pub(crate) struct DirectoryTreePreviewSnapshot {
    pub(super) revision: u64,
    pub(super) list_publish_generation: u64,
    pub(super) textures: HashMap<usize, egui::TextureHandle>,
    pub(super) logical_sizes: HashMap<usize, (u32, u32)>,
}

impl Default for DirectoryTreeTreeSnapshot {
    fn default() -> Self {
        Self {
            generation: 0,
            places_loaded: false,
            places_loading: false,
            places_load_error: None,
            workers_available: true,
            known_folders: Vec::new(),
            selected_tree_path: None,
            nodes: HashMap::new(),
            network_visible: false,
            scroll_folder_to_selected: false,
            left_panel_width: DIRECTORY_TREE_LEFT_WIDTH,
        }
    }
}

impl Default for DirectoryTreeListSnapshot {
    fn default() -> Self {
        Self {
            publish_generation: 0,
            image_list_generation: 0,
            image_rows: Arc::from([]),
            current_index: 0,
            scanning: false,
            scan_status: String::new(),
            sync_warning: None,
            scroll_image_list_to_current: false,
            image_list_col_size_w: DIRECTORY_TREE_COL_SIZE_WIDTH,
            image_list_col_modified_w: DIRECTORY_TREE_COL_MODIFIED_WIDTH,
            image_list_panel_width: DIRECTORY_TREE_RIGHT_MIN_WIDTH,
            image_list_sort_column: ImageListSortColumn::default(),
            image_list_sort_ascending: true,
            image_list_sort_active: false,
            image_list_reordering: false,
            show_list_previews: true,
            list_preview_thumb_px: crate::settings::DirectoryTreeListPreviewSize::Small.thumb_px(),
        }
    }
}

impl Default for DirectoryTreePreviewSnapshot {
    fn default() -> Self {
        Self {
            revision: 0,
            list_publish_generation: 0,
            textures: HashMap::new(),
            logical_sizes: HashMap::new(),
        }
    }
}

// --- Publish helpers -----------------------------------------------------------------------

fn share_image_rows(
    previous: &Arc<[DirectoryTreeFileRow]>,
    rows: &[DirectoryTreeFileRow],
) -> Arc<[DirectoryTreeFileRow]> {
    if previous.as_ref() == rows {
        return Arc::clone(previous);
    }
    let prev_len = previous.len();
    if rows.len() >= prev_len && prev_len > 0 && rows.get(0..prev_len) == Some(previous.as_ref()) {
        let mut shared = Vec::with_capacity(rows.len());
        shared.extend_from_slice(previous);
        shared.extend_from_slice(&rows[prev_len..]);
        return Arc::from(shared.into_boxed_slice());
    }
    Arc::from(rows.to_vec().into_boxed_slice())
}

pub(super) fn publish_tree_snapshot(
    swap: &ArcSwap<DirectoryTreeTreeSnapshot>,
    tree: &mut DirectoryTreeTreeState,
) -> bool {
    let prev = swap.load();
    if !tree.snapshot_dirty {
        debug_assert_eq!(prev.generation, tree.generation);
        let _ = prev.generation;
        return false;
    }
    let mut nodes = HashMap::with_capacity(tree.nodes.len());
    for (path, node) in tree.nodes.iter() {
        let arc = prev
            .nodes
            .get(path)
            .filter(|existing| existing.as_ref() == node)
            .cloned()
            .unwrap_or_else(|| Arc::new(node.clone()));
        nodes.insert(path.clone(), arc);
    }
    swap.store(Arc::new(DirectoryTreeTreeSnapshot {
        generation: tree.generation,
        places_loaded: tree.places_loaded,
        places_loading: tree.places_loading,
        places_load_error: tree.places_load_error.clone(),
        workers_available: tree.workers_available,
        known_folders: tree.known_folders.clone(),
        selected_tree_path: tree.selected_tree_path.clone(),
        nodes,
        network_visible: tree.network_visible,
        scroll_folder_to_selected: tree.scroll_folder_to_selected,
        left_panel_width: tree.left_panel_width,
    }));
    tree.snapshot_dirty = false;
    true
}

pub(super) fn publish_list_snapshot(
    swap: &ArcSwap<DirectoryTreeListSnapshot>,
    list: &mut DirectoryTreeListState,
) -> bool {
    let prev = swap.load();
    if !list.snapshot_dirty {
        debug_assert!(
            prev.publish_generation == list.publish_generation
                && prev.image_list_generation == list.image_list_generation
        );
        let _ = (prev.publish_generation, prev.image_list_generation);
        return false;
    }
    let image_rows = share_image_rows(&prev.image_rows, &list.image_rows);
    swap.store(Arc::new(DirectoryTreeListSnapshot {
        publish_generation: list.publish_generation,
        image_list_generation: list.image_list_generation,
        image_rows,
        current_index: list.current_index,
        scanning: list.scanning,
        scan_status: list.scan_status.clone(),
        sync_warning: list.sync_warning.clone(),
        scroll_image_list_to_current: list.scroll_image_list_to_current,
        image_list_col_size_w: list.image_list_col_size_w,
        image_list_col_modified_w: list.image_list_col_modified_w,
        image_list_panel_width: list.image_list_panel_width,
        image_list_sort_column: list.image_list_sort_column,
        image_list_sort_ascending: list.image_list_sort_ascending,
        image_list_sort_active: list.image_list_sort_active,
        image_list_reordering: list.image_list_reordering,
        show_list_previews: list.show_list_previews,
        list_preview_thumb_px: list.list_preview_thumb_px,
    }));
    list.snapshot_dirty = false;
    true
}

pub(super) fn publish_preview_snapshot(
    swap: &ArcSwap<DirectoryTreePreviewSnapshot>,
    list_publish_generation: u64,
    row_count: usize,
    cache_revision: u64,
    textures: &HashMap<usize, egui::TextureHandle>,
    logical_sizes: &HashMap<usize, (u32, u32)>,
) -> bool {
    let prev = swap.load();
    if cache_revision == prev.revision && list_publish_generation == prev.list_publish_generation {
        return false;
    }
    let mut preview_textures = HashMap::new();
    let mut preview_logical_sizes = HashMap::new();
    for (&index, handle) in textures {
        if index < row_count {
            preview_textures.insert(index, handle.clone());
        }
    }
    for (&index, &size) in logical_sizes {
        if index < row_count {
            preview_logical_sizes.insert(index, size);
        }
    }
    swap.store(Arc::new(DirectoryTreePreviewSnapshot {
        revision: cache_revision,
        list_publish_generation,
        textures: preview_textures,
        logical_sizes: preview_logical_sizes,
    }));
    true
}

pub(super) fn clear_preview_snapshot(swap: &ArcSwap<DirectoryTreePreviewSnapshot>) {
    swap.store(Arc::new(DirectoryTreePreviewSnapshot::default()));
}

pub(super) struct DirectoryTreePublishContext<'a> {
    pub tree: &'a mut DirectoryTreeTreeState,
    pub list: &'a mut DirectoryTreeListState,
    pub tree_snapshot: &'a ArcSwap<DirectoryTreeTreeSnapshot>,
    pub list_snapshot: &'a ArcSwap<DirectoryTreeListSnapshot>,
    pub preview_snapshot: &'a ArcSwap<DirectoryTreePreviewSnapshot>,
    pub last_list_publish_at: &'a mut Instant,
    pub force_list: bool,
    pub preview_cache_revision: Option<u64>,
    pub preview_textures: Option<&'a HashMap<usize, egui::TextureHandle>>,
    pub preview_logical_sizes: Option<&'a HashMap<usize, (u32, u32)>>,
}

pub(super) fn publish_domain_snapshots(ctx: &mut DirectoryTreePublishContext<'_>) -> bool {
    let mut changed = false;
    if publish_tree_snapshot(ctx.tree_snapshot, ctx.tree) {
        changed = true;
    }

    let list_due = ctx.list.snapshot_dirty
        && (ctx.force_list
            || !ctx.list.scanning
            || ctx.last_list_publish_at.elapsed() >= LIST_PUBLISH_COALESCE);
    if list_due && publish_list_snapshot(ctx.list_snapshot, ctx.list) {
        *ctx.last_list_publish_at = Instant::now();
        changed = true;
    }

    if let (Some(revision), Some(textures), Some(logical_sizes)) = (
        ctx.preview_cache_revision,
        ctx.preview_textures,
        ctx.preview_logical_sizes,
    ) {
        if publish_preview_snapshot(
            ctx.preview_snapshot,
            ctx.list.publish_generation,
            ctx.list.image_rows.len(),
            revision,
            textures,
            logical_sizes,
        ) {
            changed = true;
        }
    }

    changed
}

impl DirectoryTreeTreeState {
    pub(crate) fn mark_snapshot_dirty(&mut self) {
        self.snapshot_dirty = true;
    }
}
