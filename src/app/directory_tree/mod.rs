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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
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
use crate::directory_tree_places::types::KnownFolderKind;
use crate::directory_tree_places::{DirectoryTreePlaces, KnownFolderEntry};
use crate::loader::DIRECTORY_TREE_STRIP_POOL;
use crate::loader::{
    DecodedImage, PreviewStage, TiledImageSource, generate_directory_tree_thumb_from_path,
    preview_aspect_matches_logical,
};
use crate::path_location::is_unc_path;
use crate::settings::{BrowseMode, Settings};
use crate::theme::ThemePalette;
use crate::ui::osd::{format_file_modified, format_file_size};

pub(super) const DIRECTORY_TREE_VIEWPORT_ID: &str = "siv_directory_tree_viewport";
pub(super) const DIRECTORY_TREE_EMBEDDED_SIDE_PANEL_ID: &str = "siv_directory_tree_embedded";
pub(super) const DIRECTORY_TREE_EMBEDDED_LOADING_PANEL_ID: &str =
    "siv_directory_tree_embedded_loading";
pub(super) const DIRECTORY_TREE_EMBEDDED_DEFAULT_WIDTH: f32 = 380.0;
pub(super) const DIRECTORY_TREE_EMBEDDED_MIN_WIDTH: f32 = 320.0;
pub(super) const DIRECTORY_TREE_MIN_WIDTH: f32 = 640.0;
pub(super) const DIRECTORY_TREE_MIN_HEIGHT: f32 = 420.0;
pub(super) const DIRECTORY_TREE_LEFT_WIDTH: f32 = 340.0;
pub(super) const DIRECTORY_TREE_LEFT_MIN_WIDTH: f32 = 240.0;
pub(super) const DIRECTORY_TREE_RIGHT_MIN_WIDTH: f32 = 180.0;
pub(super) const DIRECTORY_TREE_SPLITTER_GRAB_WIDTH: f32 = 10.0;
pub(super) const DIRECTORY_TREE_LEFT_MAX_WIDTH_RATIO: f32 = 0.55;
pub(super) const DIRECTORY_TREE_COL_THUMB_WIDTH: f32 = 48.0;
pub(super) const DIRECTORY_TREE_IMAGE_ROW_HEIGHT: f32 = 48.0;
pub(super) const DIRECTORY_TREE_COLD_NEIGHBOR_RADIUS: usize = 20;
pub(super) const MAX_COLD_STRIP_GENERATES_PER_FRAME: usize = 2;
pub(super) const MAX_STRIP_GENERATE_INFLIGHT: usize = 2;
pub(super) const MAX_TILED_STRIP_GENERATES_PER_FRAME: usize = 1;
const DIRECTORY_TREE_WORKER_CHANNEL_BOUND: usize = 64;
pub(super) const DIRECTORY_TREE_EXPAND_ICON_WIDTH: f32 = 14.0;
pub(super) const DIRECTORY_TREE_FOLDER_ICON_WIDTH: f32 = 16.0;
pub(super) const DIRECTORY_TREE_ROW_HEIGHT: f32 = 22.0;
pub(super) const DIRECTORY_TREE_HEADER_HEIGHT: f32 = 22.0;
pub(super) const DIRECTORY_TREE_COL_SIZE_WIDTH: f32 = 88.0;
pub(super) const DIRECTORY_TREE_COL_MODIFIED_WIDTH: f32 = 172.0;
pub(super) const DIRECTORY_TREE_COL_SIZE_MIN_WIDTH: f32 = 56.0;
pub(super) const DIRECTORY_TREE_COL_MODIFIED_MIN_WIDTH: f32 = 96.0;
pub(super) const DIRECTORY_TREE_COL_NAME_MIN_WIDTH: f32 = 32.0;
pub(super) const DIRECTORY_TREE_INDENT: f32 = 14.0;
const THIS_PC_TREE_PATH: &str = "\\\\?\\siv-tree\\ThisPC";
const NETWORK_TREE_PATH: &str = "\\\\?\\siv-tree\\Network";

pub(super) fn this_pc_tree_path() -> PathBuf {
    PathBuf::from(THIS_PC_TREE_PATH)
}

pub(super) fn network_tree_path() -> PathBuf {
    PathBuf::from(NETWORK_TREE_PATH)
}

pub(super) fn is_this_pc_tree_path(path: &Path) -> bool {
    path.as_os_str() == this_pc_tree_path().as_os_str()
}

pub(super) fn is_network_tree_path(path: &Path) -> bool {
    path.as_os_str() == network_tree_path().as_os_str()
}

pub(super) fn is_places_sentinel_path(path: &Path) -> bool {
    is_this_pc_tree_path(path) || is_network_tree_path(path)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub(crate) enum ImageListSortColumn {
    #[default]
    Name,
    Size,
    Modified,
}

#[derive(Debug)]
pub(crate) enum DirectoryTreeCommand {
    SelectDirectory(PathBuf),
    ToggleExpanded(PathBuf),
    SelectImage(usize),
    SortImageList(ImageListSortColumn),
    CloseWindow,
}

#[derive(Debug)]
pub(crate) struct DirectoryChildrenRequest {
    tree_path: PathBuf,
    browse_path: PathBuf,
    generation: u64,
}

#[derive(Debug)]
pub(crate) struct FileMetadataRequest {
    generation: u64,
    paths: Vec<PathBuf>,
}

#[derive(Debug)]
pub(crate) struct FileMetadataResult {
    generation: u64,
    paths: Vec<PathBuf>,
    modified_unix: Vec<Option<i64>>,
}

#[derive(Debug, Clone)]
pub(super) struct DirectoryTreeFileRow {
    path: PathBuf,
    name: String,
    size_bytes: u64,
    modified_unix: Option<i64>,
}

#[derive(Debug)]
pub(crate) struct DirectoryChildrenResult {
    tree_path: PathBuf,
    generation: u64,
    result: Result<Vec<PathBuf>, String>,
}

#[derive(Debug, Clone)]
pub(crate) struct DirectoryTreeNode {
    display_name: String,
    browse_path: PathBuf,
    expanded: bool,
    loading: bool,
    children_loaded: bool,
    children: Vec<PathBuf>,
    error: Option<String>,
}

pub(crate) struct DirectoryTreeState {
    places_loaded: bool,
    places_loading: bool,
    places_load_error: Option<String>,
    workers_available: bool,
    known_folders: Vec<KnownFolderEntry>,
    selected_dir: Option<PathBuf>,
    nodes: HashMap<PathBuf, DirectoryTreeNode>,
    generation: u64,
    image_list_generation: u64,
    file_metadata_generation: u64,
    image_rows: Vec<DirectoryTreeFileRow>,
    current_index: usize,
    scanning: bool,
    scan_status: String,
    left_panel_width: f32,
    image_list_panel_width: f32,
    embedded_nav_panel_width: f32,
    scroll_image_list_to_current: bool,
    scroll_folder_to_selected: bool,
    image_list_scroll_offset_y: f32,
    image_list_keyboard_active: bool,
    preview_textures: HashMap<usize, egui::TextureHandle>,
    preview_logical_sizes: HashMap<usize, (u32, u32)>,
    preview_textures_sync_revision: u64,
    image_list_visible_row_range: Option<(usize, usize)>,
    image_list_col_size_w: f32,
    image_list_col_modified_w: f32,
    image_list_col_widths_font_size: f32,
    image_list_col_widths_dirty: bool,
    network_label: String,
    network_visible: bool,
    image_list_sort_column: ImageListSortColumn,
    image_list_sort_ascending: bool,
    image_list_sort_active: bool,
    image_list_reordering: bool,
    panel_layout_dirty: bool,
}

impl Default for DirectoryTreeState {
    fn default() -> Self {
        Self {
            places_loaded: false,
            places_loading: false,
            places_load_error: None,
            workers_available: true,
            known_folders: Vec::new(),
            selected_dir: None,
            nodes: HashMap::new(),
            generation: 0,
            image_list_generation: 0,
            file_metadata_generation: 0,
            image_rows: Vec::new(),
            current_index: 0,
            scanning: false,
            scan_status: String::new(),
            left_panel_width: DIRECTORY_TREE_LEFT_WIDTH,
            image_list_panel_width: DIRECTORY_TREE_RIGHT_MIN_WIDTH,
            embedded_nav_panel_width: 0.0,
            scroll_image_list_to_current: false,
            scroll_folder_to_selected: false,
            image_list_scroll_offset_y: 0.0,
            image_list_keyboard_active: false,
            preview_textures: HashMap::new(),
            preview_logical_sizes: HashMap::new(),
            preview_textures_sync_revision: 0,
            image_list_visible_row_range: None,
            image_list_col_size_w: DIRECTORY_TREE_COL_SIZE_WIDTH,
            image_list_col_modified_w: DIRECTORY_TREE_COL_MODIFIED_WIDTH,
            image_list_col_widths_font_size: 0.0,
            image_list_col_widths_dirty: true,
            network_label: String::new(),
            network_visible: false,
            image_list_sort_column: ImageListSortColumn::default(),
            image_list_sort_ascending: true,
            image_list_sort_active: false,
            image_list_reordering: false,
            panel_layout_dirty: false,
        }
    }
}

fn directory_tree_node(display_name: impl Into<String>, browse_path: PathBuf) -> DirectoryTreeNode {
    DirectoryTreeNode {
        display_name: display_name.into(),
        browse_path: browse_path.clone(),
        expanded: false,
        loading: false,
        children_loaded: false,
        children: Vec::new(),
        error: None,
    }
}

fn children_request(
    tree_path: PathBuf,
    browse_path: PathBuf,
    generation: u64,
) -> DirectoryChildrenRequest {
    DirectoryChildrenRequest {
        tree_path,
        browse_path,
        generation,
    }
}

pub(crate) struct DirectoryTreeRuntime {
    pub(crate) state: Arc<Mutex<DirectoryTreeState>>,
    pub(crate) command_tx: Sender<DirectoryTreeCommand>,
    pub(crate) command_rx: Receiver<DirectoryTreeCommand>,
    pub(crate) children_request_tx: Sender<DirectoryChildrenRequest>,
    pub(crate) metadata_request_tx: Sender<FileMetadataRequest>,
    pub(crate) result_rx: Receiver<DirectoryChildrenResult>,
    pub(crate) metadata_result_rx: Receiver<FileMetadataResult>,
}

impl DirectoryTreeRuntime {
    pub(crate) fn new() -> Self {
        let (command_tx, command_rx) =
            crossbeam_channel::bounded(DIRECTORY_TREE_WORKER_CHANNEL_BOUND);
        let (children_request_tx, children_request_rx) =
            crossbeam_channel::bounded(DIRECTORY_TREE_WORKER_CHANNEL_BOUND);
        let (metadata_request_tx, metadata_request_rx) =
            crossbeam_channel::bounded(DIRECTORY_TREE_WORKER_CHANNEL_BOUND);
        let (result_tx, result_rx) =
            crossbeam_channel::bounded(DIRECTORY_TREE_WORKER_CHANNEL_BOUND);
        let (metadata_result_tx, metadata_result_rx) =
            crossbeam_channel::bounded(DIRECTORY_TREE_WORKER_CHANNEL_BOUND);

        let mut children_worker_alive = false;
        match std::thread::Builder::new()
            .name("siv-directory-tree-children".to_string())
            .spawn(move || {
                directory_tree_children_worker_loop(children_request_rx, result_tx);
            }) {
            Ok(_) => children_worker_alive = true,
            Err(err) => log::error!("[DirectoryTree] Failed to spawn children worker: {err}"),
        }

        let mut metadata_worker_alive = false;
        match std::thread::Builder::new()
            .name("siv-directory-tree-metadata".to_string())
            .spawn(move || {
                directory_tree_metadata_worker_loop(metadata_request_rx, metadata_result_tx);
            }) {
            Ok(_) => metadata_worker_alive = true,
            Err(err) => log::error!("[DirectoryTree] Failed to spawn metadata worker: {err}"),
        }

        let workers_available = children_worker_alive && metadata_worker_alive;
        if !workers_available {
            log::error!(
                "[DirectoryTree] Background workers unavailable (children={children_worker_alive} metadata={metadata_worker_alive})"
            );
        }

        Self {
            state: Arc::new(Mutex::new(DirectoryTreeState {
                workers_available,
                ..DirectoryTreeState::default()
            })),
            command_tx,
            command_rx,
            children_request_tx,
            metadata_request_tx,
            result_rx,
            metadata_result_rx,
        }
    }
}

impl DirectoryTreeState {
    pub(crate) fn initialize_places(&mut self, places: DirectoryTreePlaces) {
        self.generation = self.generation.wrapping_add(1);
        self.places_loaded = true;
        self.known_folders = places.known_folders;
        self.network_label = places.network_label;
        self.network_visible = false;
        self.nodes.clear();

        let drive_paths: Vec<PathBuf> = places
            .drives
            .iter()
            .map(|drive| drive.path.clone())
            .collect();
        self.nodes.insert(
            this_pc_tree_path(),
            DirectoryTreeNode {
                display_name: places.this_pc_label,
                browse_path: this_pc_tree_path(),
                expanded: false,
                loading: false,
                children_loaded: true,
                children: drive_paths.clone(),
                error: None,
            },
        );

        for entry in &self.known_folders {
            self.nodes.insert(
                entry.tree_path.clone(),
                DirectoryTreeNode {
                    display_name: entry.display_name.clone(),
                    browse_path: entry.filesystem_path.clone(),
                    expanded: false,
                    loading: false,
                    children_loaded: false,
                    children: Vec::new(),
                    error: None,
                },
            );
        }

        for drive in places.drives {
            self.nodes
                .entry(drive.path.clone())
                .or_insert_with(|| directory_tree_node(drive.display_name, drive.path));
        }

        if !places.network_locations.is_empty() {
            let network_children: Vec<PathBuf> = places
                .network_locations
                .iter()
                .map(|entry| entry.path.clone())
                .collect();
            self.network_visible = true;
            self.nodes.insert(
                network_tree_path(),
                DirectoryTreeNode {
                    display_name: self.network_label.clone(),
                    browse_path: network_tree_path(),
                    expanded: false,
                    loading: false,
                    children_loaded: true,
                    children: network_children,
                    error: None,
                },
            );
            for entry in places.network_locations {
                self.nodes
                    .entry(entry.path.clone())
                    .or_insert_with(|| directory_tree_node(entry.display_name, entry.path));
            }
        }
    }

    fn ensure_network_visible(&mut self) {
        if self.network_visible {
            return;
        }
        self.network_visible = true;
        self.nodes.insert(
            network_tree_path(),
            DirectoryTreeNode {
                display_name: self.network_label.clone(),
                browse_path: network_tree_path(),
                expanded: false,
                loading: false,
                children_loaded: true,
                children: Vec::new(),
                error: None,
            },
        );
    }

    fn known_folder_for_filesystem_path(&self, path: &Path) -> Option<&KnownFolderEntry> {
        self.known_folders
            .iter()
            .filter(|entry| {
                path == entry.filesystem_path.as_path() || path.starts_with(&entry.filesystem_path)
            })
            .max_by_key(|entry| entry.filesystem_path.components().count())
    }

    fn reveal_ancestor_chain(&self, selected: &Path) -> Vec<PathBuf> {
        if let Some(entry) = self.known_folder_for_filesystem_path(selected) {
            let mut chain = vec![entry.tree_path.clone()];
            if selected != entry.filesystem_path.as_path() {
                if let Ok(relative) = selected.strip_prefix(&entry.filesystem_path) {
                    let mut current = entry.filesystem_path.clone();
                    for component in relative.components() {
                        current.push(component);
                        chain.push(current.clone());
                    }
                }
            }
            return chain;
        }

        if is_unc_path(selected) {
            if let Some(share) = unc_share_root(selected) {
                return directory_ancestor_chain(&share, selected);
            }
        }

        filesystem_ancestor_chain(selected)
    }

    pub(crate) fn expand_tree_for_filesystem_dir(
        &mut self,
        dir: &Path,
    ) -> Option<DirectoryChildrenRequest> {
        let tree_path = if let Some(entry) = self.known_folder_for_filesystem_path(dir) {
            if dir == entry.filesystem_path.as_path() {
                entry.tree_path.clone()
            } else {
                dir.to_path_buf()
            }
        } else {
            dir.to_path_buf()
        };
        let node = self.nodes.get_mut(&tree_path)?;
        node.expanded = true;
        if node.children_loaded || node.loading {
            return None;
        }
        node.loading = true;
        node.error = None;
        let browse_path = node.browse_path.clone();
        Some(children_request(tree_path, browse_path, self.generation))
    }

    pub(crate) fn set_selected_dir(&mut self, dir: PathBuf) {
        if is_unc_path(&dir) {
            self.ensure_network_visible();
            if let Some(share_root) = unc_share_root(&dir) {
                self.ensure_network_share_mounted(&share_root);
            }
        }
        self.selected_dir = Some(dir.clone());
        let tree_key = self
            .known_folder_for_filesystem_path(&dir)
            .filter(|entry| entry.filesystem_path == dir)
            .map(|entry| entry.tree_path.clone())
            .unwrap_or_else(|| dir.clone());
        let display_name = self
            .known_folder_for_filesystem_path(&dir)
            .filter(|entry| entry.filesystem_path == dir)
            .map(|entry| entry.display_name.clone())
            .unwrap_or_else(|| directory_display_name(&dir));
        self.nodes
            .entry(tree_key)
            .or_insert_with(|| directory_tree_node(display_name, dir));
        self.scroll_folder_to_selected = true;
    }

    pub(crate) fn reveal_selected_dir(&mut self) -> Vec<DirectoryChildrenRequest> {
        let Some(selected) = self.selected_dir.clone() else {
            return Vec::new();
        };
        if !self.places_loaded {
            return Vec::new();
        }

        let chain = self.reveal_ancestor_chain(&selected);
        let mut requests = Vec::new();

        if should_expand_this_pc_for_path(&selected, &self.known_folders) {
            if let Some(node) = self.nodes.get_mut(&this_pc_tree_path()) {
                node.expanded = true;
            }
        }

        if is_unc_path(&selected) {
            self.ensure_network_visible();
            if let Some(share_root) = unc_share_root(&selected) {
                self.ensure_network_share_mounted(&share_root);
            }
            if let Some(node) = self.nodes.get_mut(&network_tree_path()) {
                node.expanded = true;
            }
        }

        for path in chain.iter().take(chain.len().saturating_sub(1)) {
            if is_places_sentinel_path(path) {
                continue;
            }
            self.nodes
                .entry(path.clone())
                .or_insert_with(|| directory_tree_node(directory_display_name(path), path.clone()));
            let Some(node) = self.nodes.get_mut(path) else {
                continue;
            };
            node.expanded = true;
            if !node.children_loaded && !node.loading {
                node.loading = true;
                node.error = None;
                let browse_path = node.browse_path.clone();
                requests.push(children_request(path.clone(), browse_path, self.generation));
            }
        }
        let selected_tree_key = self
            .known_folder_for_filesystem_path(&selected)
            .filter(|entry| entry.filesystem_path == selected)
            .map(|entry| entry.tree_path.clone())
            .unwrap_or_else(|| selected.clone());
        self.nodes.entry(selected_tree_key).or_insert_with(|| {
            directory_tree_node(directory_display_name(&selected), selected.clone())
        });
        requests
    }

    fn ensure_network_share_mounted(&mut self, share_root: &Path) {
        self.ensure_network_visible();
        let share_path = share_root.to_path_buf();
        if let Some(network) = self.nodes.get_mut(&network_tree_path()) {
            if !network
                .children
                .iter()
                .any(|child| child.as_os_str() == share_path.as_os_str())
            {
                network.children.push(share_path.clone());
                network.children.sort();
            }
        }
        self.nodes.entry(share_path.clone()).or_insert_with(|| {
            directory_tree_node(unc_share_display_name(&share_path), share_path.clone())
        });
    }

    pub(crate) fn sync_images(
        &mut self,
        images: &[PathBuf],
        sizes: &[u64],
        modified: &[Option<i64>],
        current_index: usize,
        scanning: bool,
        scan_status: String,
        metadata_tx: &Sender<FileMetadataRequest>,
    ) {
        let prefix_matches = images.len() >= self.image_rows.len()
            && self
                .image_rows
                .iter()
                .zip(images)
                .all(|(row, path)| row.path == *path);

        if prefix_matches {
            for (index, row) in self.image_rows.iter_mut().enumerate() {
                if let Some(size) = sizes.get(index) {
                    row.size_bytes = *size;
                }
                if let Some(Some(mtime)) = modified.get(index) {
                    row.modified_unix = Some(*mtime);
                }
            }

            if images.len() > self.image_rows.len() {
                let start = self.image_rows.len();
                let mut paths_needing_meta = Vec::new();
                for index in start..images.len() {
                    let path = &images[index];
                    let mtime = modified.get(index).copied().flatten();
                    if mtime.is_none() {
                        paths_needing_meta.push(path.clone());
                    }
                    self.image_rows.push(DirectoryTreeFileRow {
                        path: path.clone(),
                        name: directory_display_name(path),
                        size_bytes: sizes.get(index).copied().unwrap_or(0),
                        modified_unix: mtime,
                    });
                }
                if !scanning {
                    self.request_file_metadata(paths_needing_meta, metadata_tx);
                }
            }
        } else {
            self.image_rows = images
                .iter()
                .enumerate()
                .map(|(index, path)| DirectoryTreeFileRow {
                    path: path.clone(),
                    name: directory_display_name(path),
                    size_bytes: sizes.get(index).copied().unwrap_or(0),
                    modified_unix: modified.get(index).copied().flatten(),
                })
                .collect();
            if !scanning {
                let paths_needing_meta = self
                    .image_rows
                    .iter()
                    .filter(|row| row.modified_unix.is_none())
                    .map(|row| row.path.clone())
                    .collect();
                self.request_file_metadata(paths_needing_meta, metadata_tx);
            }
            self.image_list_scroll_offset_y = 0.0;
        }

        let new_index = current_index.min(self.image_rows.len().saturating_sub(1));
        if new_index != self.current_index {
            self.scroll_image_list_to_current = true;
        }
        self.current_index = new_index;
        self.scanning = scanning;
        self.scan_status = scan_status;
        if scanning {
            self.image_list_keyboard_active = false;
        }
        self.image_list_col_widths_dirty = true;
    }

    pub(crate) fn sync_preview_textures(
        &mut self,
        textures: &HashMap<usize, egui::TextureHandle>,
        logical_sizes: &HashMap<usize, (u32, u32)>,
        cache_revision: u64,
    ) -> bool {
        if self.preview_textures_sync_revision == cache_revision
            && self.preview_textures.len() == textures.len()
        {
            return false;
        }
        #[cfg(feature = "preload-debug")]
        if self.preview_textures_sync_revision == cache_revision
            && self.preview_textures.len() != textures.len()
        {
            crate::preload_debug!(
                "[PreloadDebug][DirTree] sync_preview_textures forced: revision={} ui_count={} cache_count={}",
                cache_revision,
                self.preview_textures.len(),
                textures.len()
            );
        }
        #[cfg(feature = "preload-debug")]
        let previous_revision = self.preview_textures_sync_revision;
        #[cfg(feature = "preload-debug")]
        let previous_count = self.preview_textures.len();
        self.preview_textures_sync_revision = cache_revision;
        self.preview_textures.clear();
        self.preview_logical_sizes.clear();
        for (&index, handle) in textures {
            if index < self.image_rows.len() {
                self.preview_textures.insert(index, handle.clone());
            }
        }
        for (&index, &size) in logical_sizes {
            if index < self.image_rows.len() {
                self.preview_logical_sizes.insert(index, size);
            }
        }
        #[cfg(feature = "preload-debug")]
        crate::preload_debug!(
            "[PreloadDebug][DirTree] sync_preview_textures revision {} -> {} ui {} -> {} rows={} cache={}",
            previous_revision,
            cache_revision,
            previous_count,
            self.preview_textures.len(),
            self.image_rows.len(),
            textures.len()
        );
        true
    }

    fn ensure_image_list_column_widths(
        &mut self,
        painter: &egui::Painter,
        body_font: &egui::FontId,
        header_size: &str,
        header_modified: &str,
    ) {
        let font_size = body_font.size;
        if !self.image_list_col_widths_dirty
            && (self.image_list_col_widths_font_size - font_size).abs() < f32::EPSILON
        {
            return;
        }
        let (size_w, modified_w) = measure_image_list_content_column_widths(
            painter,
            body_font,
            header_size,
            header_modified,
            &self.image_rows,
        );
        self.image_list_col_size_w = size_w;
        self.image_list_col_modified_w = modified_w;
        self.image_list_col_widths_font_size = font_size;
        self.image_list_col_widths_dirty = false;
    }

    fn request_file_metadata(
        &mut self,
        paths: Vec<PathBuf>,
        metadata_tx: &Sender<FileMetadataRequest>,
    ) {
        if paths.is_empty() {
            return;
        }
        self.file_metadata_generation = self.file_metadata_generation.wrapping_add(1);
        let _ = metadata_tx.send(FileMetadataRequest {
            generation: self.file_metadata_generation,
            paths,
        });
    }

    fn apply_metadata_result(&mut self, result: FileMetadataResult) {
        if result.generation != self.file_metadata_generation {
            return;
        }
        for (path, modified_unix) in result.paths.into_iter().zip(result.modified_unix) {
            if let Some(row) = self.image_rows.iter_mut().find(|row| row.path == path) {
                row.modified_unix = modified_unix;
            }
        }
    }

    fn toggle_expanded(&mut self, path: &Path) -> Option<DirectoryChildrenRequest> {
        let node = self.nodes.get_mut(path)?;
        node.expanded = !node.expanded;
        if is_places_sentinel_path(path) {
            return None;
        }
        if node.expanded && !node.children_loaded && !node.loading {
            node.loading = true;
            node.error = None;
            let browse_path = node.browse_path.clone();
            Some(children_request(
                path.to_path_buf(),
                browse_path,
                self.generation,
            ))
        } else {
            None
        }
    }

    fn apply_children_result(&mut self, result: DirectoryChildrenResult) {
        if result.generation != self.generation {
            return;
        }

        let Some(node) = self.nodes.get_mut(&result.tree_path) else {
            return;
        };

        node.loading = false;
        node.children_loaded = true;
        match result.result {
            Ok(children) => {
                node.children = children.clone();
                node.error = None;
                for child in children {
                    self.nodes.entry(child.clone()).or_insert_with(|| {
                        directory_tree_node(directory_display_name(&child), child)
                    });
                }
            }
            Err(err) => {
                node.children.clear();
                node.error = Some(err);
            }
        }
    }
}

mod app;
mod sort;
mod ui;
mod workers;

use ui::{
    directory_ancestor_chain, directory_display_name, filesystem_ancestor_chain,
    measure_image_list_content_column_widths, should_expand_this_pc_for_path,
    unc_share_display_name, unc_share_root,
};
use workers::{
    directory_tree_children_worker_loop, directory_tree_metadata_worker_loop,
    read_child_directories, strip_worker_com_initialized,
};

#[cfg(test)]
mod tests;
