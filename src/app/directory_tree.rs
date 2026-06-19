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

use std::cmp::Ordering;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
use crate::directory_tree_places::{DirectoryTreePlaces, KnownFolderEntry};
use crate::loader::REFINEMENT_POOL;
use crate::loader::{
    DecodedImage, PreviewStage, TiledImageSource, generate_directory_tree_thumb_from_path,
    preview_aspect_matches_logical,
};
use crate::path_location::is_unc_path;
use crate::settings::{BrowseMode, Settings};
use crate::theme::ThemePalette;
use crate::ui::osd::{format_file_modified, format_file_size};

const DIRECTORY_TREE_VIEWPORT_ID: &str = "siv_directory_tree_viewport";
const DIRECTORY_TREE_EMBEDDED_SIDE_PANEL_ID: &str = "siv_directory_tree_embedded";
const DIRECTORY_TREE_EMBEDDED_DEFAULT_WIDTH: f32 = 380.0;
const DIRECTORY_TREE_EMBEDDED_MIN_WIDTH: f32 = 320.0;
const DIRECTORY_TREE_MIN_WIDTH: f32 = 640.0;
const DIRECTORY_TREE_MIN_HEIGHT: f32 = 420.0;
const DIRECTORY_TREE_DEFAULT_WIDTH: f32 = 820.0;
const DIRECTORY_TREE_LEFT_WIDTH: f32 = 340.0;
const DIRECTORY_TREE_LEFT_MIN_WIDTH: f32 = 240.0;
const DIRECTORY_TREE_RIGHT_MIN_WIDTH: f32 = 180.0;
const DIRECTORY_TREE_SPLITTER_GRAB_WIDTH: f32 = 10.0;
const DIRECTORY_TREE_LEFT_MAX_WIDTH_RATIO: f32 = 0.55;
const DIRECTORY_TREE_COL_THUMB_WIDTH: f32 = 48.0;
const DIRECTORY_TREE_IMAGE_ROW_HEIGHT: f32 = 48.0;
const DIRECTORY_TREE_COLD_NEIGHBOR_RADIUS: usize = 20;
const MAX_COLD_STRIP_GENERATES_PER_FRAME: usize = 2;
const MAX_STRIP_GENERATE_INFLIGHT: usize = 4;
const DIRECTORY_TREE_EXPAND_ICON_WIDTH: f32 = 18.0;
const DIRECTORY_TREE_FOLDER_ICON_WIDTH: f32 = 18.0;
const DIRECTORY_TREE_ROW_HEIGHT: f32 = 24.0;
const DIRECTORY_TREE_HEADER_HEIGHT: f32 = 22.0;
const DIRECTORY_TREE_COL_SIZE_WIDTH: f32 = 88.0;
const DIRECTORY_TREE_COL_MODIFIED_WIDTH: f32 = 172.0;
const DIRECTORY_TREE_COL_SIZE_MIN_WIDTH: f32 = 56.0;
const DIRECTORY_TREE_COL_MODIFIED_MIN_WIDTH: f32 = 96.0;
const DIRECTORY_TREE_COL_NAME_MIN_WIDTH: f32 = 32.0;
const DIRECTORY_TREE_INDENT: f32 = 16.0;
const THIS_PC_TREE_PATH: &str = "\\\\?\\siv-tree\\ThisPC";
const NETWORK_TREE_PATH: &str = "\\\\?\\siv-tree\\Network";

fn this_pc_tree_path() -> PathBuf {
    PathBuf::from(THIS_PC_TREE_PATH)
}

fn network_tree_path() -> PathBuf {
    PathBuf::from(NETWORK_TREE_PATH)
}

fn is_this_pc_tree_path(path: &Path) -> bool {
    path.as_os_str() == this_pc_tree_path().as_os_str()
}

fn is_network_tree_path(path: &Path) -> bool {
    path.as_os_str() == network_tree_path().as_os_str()
}

fn is_places_sentinel_path(path: &Path) -> bool {
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
struct DirectoryTreeFileRow {
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
    known_folders: Vec<KnownFolderEntry>,
    selected_dir: Option<PathBuf>,
    nodes: HashMap<PathBuf, DirectoryTreeNode>,
    generation: u64,
    image_list_generation: u64,
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
}

impl Default for DirectoryTreeState {
    fn default() -> Self {
        Self {
            places_loaded: false,
            known_folders: Vec::new(),
            selected_dir: None,
            nodes: HashMap::new(),
            generation: 0,
            image_list_generation: 0,
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

const METADATA_BATCH_SIZE: usize = 200;

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
        let (command_tx, command_rx) = crossbeam_channel::unbounded();
        let (children_request_tx, children_request_rx) = crossbeam_channel::unbounded();
        let (metadata_request_tx, metadata_request_rx) = crossbeam_channel::unbounded();
        let (result_tx, result_rx) = crossbeam_channel::unbounded();
        let (metadata_result_tx, metadata_result_rx) = crossbeam_channel::unbounded();

        if let Err(err) = std::thread::Builder::new()
            .name("siv-directory-tree-children".to_string())
            .spawn(move || {
                directory_tree_children_worker_loop(children_request_rx, result_tx);
            })
        {
            log::error!("[DirectoryTree] Failed to spawn children worker: {err}");
        }

        if let Err(err) = std::thread::Builder::new()
            .name("siv-directory-tree-metadata".to_string())
            .spawn(move || {
                directory_tree_metadata_worker_loop(metadata_request_rx, metadata_result_tx);
            })
        {
            log::error!("[DirectoryTree] Failed to spawn metadata worker: {err}");
        }

        Self {
            state: Arc::new(Mutex::new(DirectoryTreeState::default())),
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
    ) {
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
        self.image_list_generation = self.image_list_generation.wrapping_add(1);
        let _ = metadata_tx.send(FileMetadataRequest {
            generation: self.image_list_generation,
            paths,
        });
    }

    fn apply_metadata_result(&mut self, result: FileMetadataResult) {
        if result.generation != self.image_list_generation {
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

    pub(crate) fn ensure_directory_tree_places_loaded(&mut self) {
        if self
            .directory_tree
            .state
            .try_lock()
            .is_some_and(|state| state.places_loaded)
        {
            return;
        }
        let places = crate::directory_tree_places::load();
        let saved_dir = if self.settings.browse_mode == BrowseMode::Tree {
            self.settings
                .tree_nav_selected_dir
                .clone()
                .or_else(|| self.settings.last_image_dir.clone())
        } else {
            self.settings.last_image_dir.clone()
        };
        let mut state = self.directory_tree.state.lock();
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
        self.apply_saved_directory_tree_panel_layout_to_state(&mut state);
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
            state.left_panel_width = clamp_directory_tree_left_panel_width(
                state.left_panel_width,
                DIRECTORY_TREE_DEFAULT_WIDTH,
            );
            reconcile_directory_tree_panel_widths(&mut state, DIRECTORY_TREE_DEFAULT_WIDTH);
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
    }

    pub(crate) fn restore_saved_directory_tree_panel_layout(&self) {
        let mut state = self.directory_tree.state.lock();
        self.apply_saved_directory_tree_panel_layout_to_state(&mut state);
    }

    pub(crate) fn persist_directory_tree_layout_to_settings(&mut self) {
        if !self.directory_tree_settings_active() {
            return;
        }
        let Some(state) = self.directory_tree.state.try_lock() else {
            return;
        };
        self.settings.directory_tree_folder_panel_width = Some(state.left_panel_width);
        self.settings.directory_tree_image_list_panel_width = Some(state.image_list_panel_width);
        if self.directory_tree_nav_is_embedded() && state.embedded_nav_panel_width > 0.0 {
            self.settings.directory_tree_embedded_panel_width = Some(
                state
                    .embedded_nav_panel_width
                    .max(DIRECTORY_TREE_EMBEDDED_MIN_WIDTH),
            );
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
            if !state.places_loaded {
                return;
            }
            let previous_index = state.current_index;
            let previous_scanning = state.scanning;
            let previous_row_count = state.image_rows.len();
            let resort_after_scan =
                previous_scanning && !self.scanning && state.image_list_sort_active;
            let resort_column = state.image_list_sort_column;
            let resort_ascending = state.image_list_sort_ascending;
            state.sync_images(
                &self.image_files,
                &self.file_byte_len_by_index,
                &self.file_modified_unix_by_index,
                self.current_index,
                self.scanning,
                self.status_message.clone(),
                &self.directory_tree.metadata_request_tx,
            );
            let repaint = state.scroll_image_list_to_current
                || state.current_index != previous_index
                || state.scanning != previous_scanning
                || state.image_rows.len() != previous_row_count;
            #[cfg(feature = "preload-debug")]
            if repaint
                && (state.scanning != previous_scanning
                    || state.image_rows.len() != previous_row_count)
            {
                crate::preload_debug!(
                    "[PreloadDebug][Scan] directory tree viewport repaint: scanning {} -> {} rows {} -> {}",
                    previous_scanning,
                    state.scanning,
                    previous_row_count,
                    state.image_rows.len()
                );
            }
            state.sync_preview_textures(
                self.directory_tree_strip_cache.textures(),
                self.directory_tree_strip_cache.logical_sizes(),
            );
            (repaint, resort_after_scan, resort_column, resort_ascending)
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
                .unwrap_or_else(|poisoned| poisoned.into_inner().clone());
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
                    .unwrap_or_else(|poisoned| poisoned.into_inner().clone());
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
        self.sync_directory_tree_file_list_state(ctx);
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
        if !self.scanning {
            self.run_directory_tree_logic_updates(ctx);
        }
    }

    /// Strip-thumbnail polling/generation and deferred main-image preloads after a scan.
    pub(crate) fn run_directory_tree_logic_updates(&mut self, ctx: &egui::Context) {
        if !self.directory_tree_settings_active() {
            return;
        }

        self.ensure_directory_tree_strip_thumbnails(ctx);

        if let Some(mut state) = self.directory_tree.state.try_lock() {
            if state.places_loaded {
                state.sync_preview_textures(
                    self.directory_tree_strip_cache.textures(),
                    self.directory_tree_strip_cache.logical_sizes(),
                );
            }
        } else {
            self.defer_directory_tree_file_list_sync();
        }
        let viewport_id = self.directory_tree_repaint_viewport_id();
        ctx.request_repaint_of(viewport_id);
        self.mark_directory_tree_repaint_pending();
        if self.directory_tree_nav_is_embedded() {
            ctx.request_repaint();
        }
        self.wake_root_for_logic();

        if self.pending_preload_after_directory_scan {
            self.pending_preload_after_directory_scan = false;
            self.schedule_preloads(true);
        }
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

    fn strip_index_handled_by_preload_pipeline(&self, index: usize) -> bool {
        if self.tiled_sdr_source_for_index(index).is_some() {
            return true;
        }
        self.deferred_sdr_uploads
            .get(&index)
            .is_some_and(|decoded| !crate::loader::decoded_looks_like_black_placeholder(decoded))
    }

    fn strip_index_needs_cold_thumbnail(&self, index: usize) -> bool {
        if index >= self.image_files.len() {
            return false;
        }
        if self.strip_index_handled_by_preload_pipeline(index) {
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

    fn visible_cold_strip_indices(
        visible_row_range: Option<(usize, usize)>,
        scroll_to_current_pending: bool,
        total: usize,
    ) -> Vec<usize> {
        if scroll_to_current_pending || total == 0 {
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

        for index in
            Self::visible_cold_strip_indices(visible_row_range, scroll_to_current_pending, total)
        {
            push(index);
        }

        for delta in 1..=DIRECTORY_TREE_COLD_NEIGHBOR_RADIUS {
            push(current.saturating_sub(delta));
            if current + delta < total {
                push(current + delta);
            }
        }

        for index in 0..total {
            push(index);
        }
        ordered
    }

    pub(crate) fn try_generate_cold_directory_tree_strip_thumbnail(&mut self, index: usize) {
        if !self.strip_index_needs_cold_thumbnail(index) {
            return;
        }
        let path = self.image_files[index].clone();
        self.directory_tree_strip_cold_attempted.insert(index);
        self.directory_tree_strip_generate_inflight.insert(index);
        let tx = self.directory_tree_strip_preview_tx.clone();
        let max_side = DIRECTORY_TREE_STRIP_THUMBNAIL_MAX_SIDE;
        REFINEMENT_POOL.spawn(move || {
            crate::preload_debug!(
                "[PreloadDebug][Strip] cold worker start idx={} path={}",
                index,
                path.display()
            );
            #[cfg(target_os = "windows")]
            let com_ok = crate::wic::ComGuard::new().is_ok();
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
            if let Err(err) = tx.send(DirectoryTreeStripPreviewJobResult {
                index,
                path,
                decoded,
                logical,
                stage: PreviewStage::Initial,
            }) {
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
        while let Ok(result) = self.directory_tree_strip_preview_rx.try_recv() {
            self.directory_tree_strip_generate_inflight
                .remove(&result.index);
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
        self.directory_tree_strip_tiled_attempted.insert(index);
        self.directory_tree_strip_generate_inflight.insert(index);
        let source = Arc::clone(&source);
        let tx = self.directory_tree_strip_preview_tx.clone();
        let max_side = DIRECTORY_TREE_STRIP_THUMBNAIL_MAX_SIDE;
        REFINEMENT_POOL.spawn(move || {
            let mut decoded = DecodedImage::new(0, 0, Vec::new());
            crate::preload_debug!(
                "[PreloadDebug][Strip] worker start idx={} logical={}x{} max_side={}",
                index,
                logical.0,
                logical.1,
                max_side
            );
            #[cfg(target_os = "windows")]
            let com_ok = crate::wic::ComGuard::new().is_ok();
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
            if let Err(err) = tx.send(DirectoryTreeStripPreviewJobResult {
                index,
                path,
                decoded,
                logical,
                stage: PreviewStage::Refined,
            }) {
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
        let total = self.image_files.len().max(1);
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
        }

        const MAX_TILED_STRIP_GENERATES_PER_FRAME: usize = 1;
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
        let cold_candidates = self
            .collect_cold_strip_thumbnail_candidates(visible_row_range, scroll_to_current_pending);
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

fn directory_tree_children_worker_loop(
    request_rx: Receiver<DirectoryChildrenRequest>,
    children_result_tx: Sender<DirectoryChildrenResult>,
) {
    while let Ok(request) = request_rx.recv() {
        let result = read_child_directories(&request.browse_path);
        let _ = children_result_tx.send(DirectoryChildrenResult {
            tree_path: request.tree_path,
            generation: request.generation,
            result,
        });
    }
}

fn directory_tree_metadata_worker_loop(
    request_rx: Receiver<FileMetadataRequest>,
    metadata_result_tx: Sender<FileMetadataResult>,
) {
    while let Ok(request) = request_rx.recv() {
        let mut batch_paths = Vec::with_capacity(METADATA_BATCH_SIZE);
        let mut batch_modified = Vec::with_capacity(METADATA_BATCH_SIZE);

        for path in request.paths {
            batch_paths.push(path.clone());
            batch_modified.push(read_file_modified_unix(&path));

            if batch_paths.len() >= METADATA_BATCH_SIZE {
                let _ = metadata_result_tx.send(FileMetadataResult {
                    generation: request.generation,
                    paths: batch_paths.split_off(0),
                    modified_unix: batch_modified.split_off(0),
                });
            }
        }

        if !batch_paths.is_empty() {
            let _ = metadata_result_tx.send(FileMetadataResult {
                generation: request.generation,
                paths: batch_paths,
                modified_unix: batch_modified,
            });
        }
    }
}

fn read_file_modified_unix(path: &Path) -> Option<i64> {
    use std::time::UNIX_EPOCH;
    let metadata = std::fs::metadata(path).ok()?;
    metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs() as i64)
}

fn read_child_directories(path: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = std::fs::read_dir(path).map_err(|err| err.to_string())?;
    let mut dirs = Vec::new();
    for entry in entries.flatten() {
        let child = entry.path();
        let Ok(meta) = child.symlink_metadata() else {
            continue;
        };
        if !meta.is_dir() {
            continue;
        }
        if crate::scanner::is_directory_traversal_boundary_metadata(&meta) {
            continue;
        }
        if is_non_browsable_system_directory(&child) {
            continue;
        }
        dirs.push(child);
    }
    dirs.sort();
    Ok(dirs)
}

pub(crate) fn is_non_browsable_system_directory(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(is_non_browsable_system_directory_name)
}

fn is_non_browsable_system_directory_name(name: &str) -> bool {
    matches!(
        name.to_ascii_uppercase().as_str(),
        "$RECYCLE.BIN"
            | "SYSTEM VOLUME INFORMATION"
            | "$WINDOWS.~BT"
            | "$WINDOWS.~WS"
            | "CONFIG.MSI"
    )
}

fn paint_tree_expand_icon(ui: &mut egui::Ui, expanded: bool, response: &egui::Response) {
    let openness = if expanded { 1.0 } else { 0.0 };
    egui::collapsing_header::paint_default_icon(ui, openness, response);
}

fn paint_tree_folder_icon(ui: &mut egui::Ui, rect: egui::Rect) {
    let icon_rect = egui::Rect::from_center_size(
        rect.center(),
        egui::vec2(rect.width() * 0.82, rect.height() * 0.72),
    );
    let color = ui.visuals().widgets.inactive.fg_stroke.color;
    let body = egui::Rect::from_min_max(
        icon_rect.left_bottom() + egui::vec2(0.0, -icon_rect.height() * 0.62),
        icon_rect.right_bottom(),
    );
    let tab = egui::Rect::from_min_max(
        icon_rect.left_top() + egui::vec2(0.0, icon_rect.height() * 0.12),
        icon_rect.left_top() + egui::vec2(icon_rect.width() * 0.58, icon_rect.height() * 0.42),
    );
    ui.painter()
        .rect_filled(body, 2.0, color.gamma_multiply(0.82));
    ui.painter().rect_filled(tab, 1.5, color);
}

fn directory_tree_row_selected_fill(palette: &ThemePalette) -> egui::Color32 {
    if palette.is_dark {
        egui::Color32::from_gray(78)
    } else {
        egui::Color32::from_rgba_unmultiplied(
            palette.accent2.r(),
            palette.accent2.g(),
            palette.accent2.b(),
            30,
        )
    }
}

fn directory_tree_row_selected_text(palette: &ThemePalette) -> egui::Color32 {
    if palette.is_dark {
        egui::Color32::from_gray(210)
    } else {
        palette.accent2
    }
}

fn paint_directory_tree_folder_name(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    selected: bool,
    hovered: bool,
    name: &str,
    palette: &ThemePalette,
) {
    if selected {
        ui.painter()
            .rect_filled(rect, 3.0, directory_tree_row_selected_fill(palette));
    } else if hovered {
        ui.painter().rect_filled(rect, 3.0, palette.widget_hover);
    }
    let text_color = if selected {
        directory_tree_row_selected_text(palette)
    } else {
        palette.text_normal
    };
    let font = egui::FontId::proportional(ui.style().text_styles[&egui::TextStyle::Body].size);
    ui.painter().text(
        rect.left_center() + egui::vec2(4.0, 0.0),
        egui::Align2::LEFT_CENTER,
        name,
        font,
        text_color,
    );
}

fn draw_directory_tree_window(
    ui: &mut egui::Ui,
    state: &mut DirectoryTreeState,
    command_tx: &Sender<DirectoryTreeCommand>,
    root_wake: Option<&crate::app::RootRedrawWake>,
    palette: &ThemePalette,
    embedded: bool,
) {
    ui.visuals_mut().button_frame = false;
    ui.visuals_mut().override_text_color = Some(palette.text_normal);
    ui.painter()
        .rect_filled(ui.max_rect(), 0.0, palette.panel_bg);
    draw_directory_tree_top_panels(
        ui,
        state,
        command_tx,
        root_wake,
        palette,
        egui::vec2(ui.available_width(), ui.available_height()),
        embedded,
    );
}

fn directory_tree_left_panel_width_limits(viewport_width: f32) -> (f32, f32) {
    let viewport_width = viewport_width.max(0.0);
    let layout_cap =
        (viewport_width - DIRECTORY_TREE_SPLITTER_GRAB_WIDTH - DIRECTORY_TREE_RIGHT_MIN_WIDTH)
            .max(0.0);
    let max_left = (viewport_width * DIRECTORY_TREE_LEFT_MAX_WIDTH_RATIO).min(layout_cap);
    let min_left = DIRECTORY_TREE_LEFT_MIN_WIDTH.min(max_left);
    (min_left, max_left.max(min_left))
}

fn clamp_directory_tree_left_panel_width(width: f32, viewport_width: f32) -> f32 {
    let (min_left, max_left) = directory_tree_left_panel_width_limits(viewport_width);
    width.clamp(min_left, max_left)
}

fn reconcile_directory_tree_panel_widths(state: &mut DirectoryTreeState, viewport_width: f32) {
    let splitter_w = DIRECTORY_TREE_SPLITTER_GRAB_WIDTH;
    let min_list = DIRECTORY_TREE_RIGHT_MIN_WIDTH;
    let (min_left, max_left) = directory_tree_left_panel_width_limits(viewport_width);

    if viewport_width <= splitter_w {
        state.left_panel_width = 0.0;
        state.image_list_panel_width = 0.0;
        return;
    }

    let available = viewport_width - splitter_w;
    let mut left_w = state.left_panel_width.clamp(min_left, max_left);
    let mut list_w = state.image_list_panel_width.max(min_list);

    if left_w + list_w > available {
        list_w = (available - left_w).max(min_list);
    } else if left_w + list_w < available {
        list_w = available - left_w;
    }

    list_w = list_w.clamp(min_list, (available - min_left).max(0.0));
    left_w = (available - list_w).clamp(min_left, max_left);
    list_w = available - left_w;

    state.left_panel_width = left_w;
    state.image_list_panel_width = list_w;
}

fn draw_directory_tree_top_panels(
    ui: &mut egui::Ui,
    state: &mut DirectoryTreeState,
    command_tx: &Sender<DirectoryTreeCommand>,
    root_wake: Option<&crate::app::RootRedrawWake>,
    palette: &ThemePalette,
    panel_size: egui::Vec2,
    embedded: bool,
) {
    let viewport_height = panel_size.y;
    let viewport_width = panel_size.x;
    reconcile_directory_tree_panel_widths(state, viewport_width);

    let left_w = state.left_panel_width;
    let splitter_w = DIRECTORY_TREE_SPLITTER_GRAB_WIDTH;
    let right_w = state.image_list_panel_width;

    let origin = ui.cursor().min;
    let left_rect = egui::Rect::from_min_size(origin, egui::vec2(left_w, viewport_height));
    let splitter_rect = egui::Rect::from_min_size(
        origin + egui::vec2(left_w, 0.0),
        egui::vec2(splitter_w, viewport_height),
    );
    let right_rect = egui::Rect::from_min_size(
        origin + egui::vec2(left_w + splitter_w, 0.0),
        egui::vec2(right_w, viewport_height),
    );

    ui.scope_builder(egui::UiBuilder::new().max_rect(left_rect), |ui| {
        ui.set_clip_rect(left_rect);
        ui.set_width(left_w);
        draw_folder_panel(ui, state, command_tx, root_wake, palette);
    });

    ui.scope_builder(egui::UiBuilder::new().max_rect(right_rect), |ui| {
        ui.set_clip_rect(right_rect);
        ui.set_width(right_w);
        draw_image_file_list(ui, state, command_tx, palette, embedded);
    });

    let splitter_id = ui.id().with("directory_tree_splitter");
    let splitter_response = ui.interact(splitter_rect, splitter_id, egui::Sense::drag());
    if splitter_response.dragged() {
        state.left_panel_width = clamp_directory_tree_left_panel_width(
            state.left_panel_width + splitter_response.drag_delta().x,
            viewport_width,
        );
        reconcile_directory_tree_panel_widths(state, viewport_width);
        ui.ctx().request_repaint();
    }
    if splitter_response.hovered() || splitter_response.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
    let splitter_stroke = if splitter_response.dragged() {
        ui.style().visuals.widgets.active.fg_stroke
    } else if splitter_response.hovered() {
        ui.style().visuals.widgets.hovered.fg_stroke
    } else {
        ui.style().visuals.widgets.noninteractive.bg_stroke
    };
    ui.painter().vline(
        splitter_rect.center().x,
        splitter_rect.y_range(),
        splitter_stroke,
    );
    if embedded {
        state.embedded_nav_panel_width = viewport_width;
    }
}

fn preview_texture_contain_rect(
    cell: egui::Rect,
    texture_width: f32,
    texture_height: f32,
) -> egui::Rect {
    if texture_width <= 0.0 || texture_height <= 0.0 {
        return cell;
    }
    let scale = (cell.width() / texture_width).min(cell.height() / texture_height);
    let size = egui::vec2(texture_width * scale, texture_height * scale);
    let offset = (cell.size() - size) * 0.5;
    egui::Rect::from_min_size(cell.min + offset, size)
}

fn paint_image_list_thumbnail(
    painter: &egui::Painter,
    palette: &ThemePalette,
    thumb_rect: egui::Rect,
    texture: Option<&egui::TextureHandle>,
    logical_size: Option<(u32, u32)>,
) {
    let inner = thumb_rect.shrink(2.0);
    let mut drew_texture = false;
    if let Some(texture) = texture {
        let tex_size = texture.size();
        let texture_w = tex_size[0] as f32;
        let texture_h = tex_size[1] as f32;
        let aspect_ok = logical_size.is_none_or(|(logical_w, logical_h)| {
            preview_aspect_matches_logical(texture_w as u32, texture_h as u32, logical_w, logical_h)
        });
        if aspect_ok && texture_w > 0.0 && texture_h > 0.0 {
            painter.rect_filled(inner, 1.0, palette.widget_bg);
            let image_rect = preview_texture_contain_rect(inner, texture_w, texture_h);
            painter.image(
                texture.id(),
                image_rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
            drew_texture = true;
        }
    }
    if !drew_texture {
        painter.rect_filled(inner, 1.0, palette.widget_bg);
    }
}

fn draw_folder_panel(
    ui: &mut egui::Ui,
    state: &mut DirectoryTreeState,
    command_tx: &Sender<DirectoryTreeCommand>,
    root_wake: Option<&crate::app::RootRedrawWake>,
    palette: &ThemePalette,
) {
    let scroll_to_selected = state.scroll_folder_to_selected;
    directory_tree_scroll_area("directory_tree_folders", ui, |ui| {
        if !state.places_loaded {
            return;
        }
        let mut scrolled = false;
        for entry in &state.known_folders {
            scrolled |= draw_directory_node(
                ui,
                state,
                command_tx,
                root_wake,
                palette,
                &entry.tree_path,
                0,
                scroll_to_selected,
            );
        }
        scrolled |= draw_directory_node(
            ui,
            state,
            command_tx,
            root_wake,
            palette,
            &this_pc_tree_path(),
            0,
            scroll_to_selected,
        );
        if state.network_visible {
            scrolled |= draw_directory_node(
                ui,
                state,
                command_tx,
                root_wake,
                palette,
                &network_tree_path(),
                0,
                scroll_to_selected,
            );
        }
        if scrolled {
            state.scroll_folder_to_selected = false;
        }
    });
}

fn directory_tree_scroll_area(
    id_salt: &'static str,
    ui: &mut egui::Ui,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    let scroll_height = ui.available_height();
    egui::ScrollArea::vertical()
        .id_salt(id_salt)
        .auto_shrink([false, false])
        .max_height(scroll_height)
        .show(ui, add_contents);
}

fn draw_directory_node(
    ui: &mut egui::Ui,
    state: &DirectoryTreeState,
    command_tx: &Sender<DirectoryTreeCommand>,
    root_wake: Option<&crate::app::RootRedrawWake>,
    palette: &ThemePalette,
    path: &Path,
    depth: usize,
    scroll_to_selected: bool,
) -> bool {
    let Some(node) = state.nodes.get(path).cloned() else {
        return false;
    };

    let mut scrolled = false;

    let row_width = ui.available_width();
    ui.allocate_ui_with_layout(
        egui::vec2(row_width, DIRECTORY_TREE_ROW_HEIGHT),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_min_width(row_width);
            ui.add_space(depth as f32 * DIRECTORY_TREE_INDENT);

            if node.loading {
                ui.add_sized(
                    [DIRECTORY_TREE_EXPAND_ICON_WIDTH, DIRECTORY_TREE_ROW_HEIGHT],
                    egui::Spinner::new(),
                );
            } else {
                let expand_response = ui.allocate_response(
                    egui::vec2(DIRECTORY_TREE_EXPAND_ICON_WIDTH, DIRECTORY_TREE_ROW_HEIGHT),
                    egui::Sense::click(),
                );
                paint_tree_expand_icon(ui, node.expanded, &expand_response);
                if expand_response.clicked() {
                    let _ =
                        command_tx.send(DirectoryTreeCommand::ToggleExpanded(path.to_path_buf()));
                }
            }

            let folder_rect = ui.allocate_exact_size(
                egui::vec2(DIRECTORY_TREE_FOLDER_ICON_WIDTH, DIRECTORY_TREE_ROW_HEIGHT),
                egui::Sense::hover(),
            );
            paint_tree_folder_icon(ui, folder_rect.0);

            let selected = state
                .selected_dir
                .as_deref()
                .is_some_and(|selected| selected == node.browse_path.as_path());
            let name_width = ui.available_width().max(1.0);
            let (name_rect, name_response) = ui.allocate_exact_size(
                egui::vec2(name_width, DIRECTORY_TREE_ROW_HEIGHT),
                egui::Sense::click(),
            );
            paint_directory_tree_folder_name(
                ui,
                name_rect,
                selected,
                name_response.hovered(),
                node.display_name.as_str(),
                palette,
            );
            let name_response = name_response.on_hover_text(node.browse_path.to_string_lossy());
            if scroll_to_selected && selected {
                name_response.scroll_to_me(Some(egui::Align::Center));
                scrolled = true;
            }
            if name_response.clicked() {
                if is_places_sentinel_path(path) {
                    let _ =
                        command_tx.send(DirectoryTreeCommand::ToggleExpanded(path.to_path_buf()));
                } else {
                    let browse_path = node.browse_path.clone();
                    let _ = command_tx.send(DirectoryTreeCommand::SelectDirectory(browse_path));
                }
                if let Some(wake) = root_wake {
                    wake();
                }
                ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
                ui.ctx().request_repaint();
            }
        },
    );

    if let Some(error) = node.error.as_deref() {
        ui.horizontal(|ui| {
            ui.add_space((depth + 1) as f32 * DIRECTORY_TREE_INDENT);
            ui.label(
                egui::RichText::new(t!("directory_tree.read_failed", err = error).to_string())
                    .color(ui.visuals().error_fg_color),
            );
        });
    }

    if node.expanded {
        for child in node.children {
            scrolled |= draw_directory_node(
                ui,
                state,
                command_tx,
                root_wake,
                palette,
                &child,
                depth + 1,
                scroll_to_selected,
            );
        }
    }

    scrolled
}

fn draw_image_file_list(
    ui: &mut egui::Ui,
    state: &mut DirectoryTreeState,
    command_tx: &Sender<DirectoryTreeCommand>,
    palette: &ThemePalette,
    embedded: bool,
) {
    let panel_rect = ui.max_rect();
    let list_focus_id = ui.id().with("directory_tree_image_list");
    let list_enabled = !state.scanning || !state.image_rows.is_empty();
    if list_enabled {
        let panel_response = ui.interact(panel_rect, list_focus_id, egui::Sense::click());
        if panel_response.clicked() {
            panel_response.request_focus();
            state.image_list_keyboard_active = true;
        }
    }

    if state.image_rows.is_empty() && !state.scanning {
        ui.label(egui::RichText::new(t!("directory_tree.no_images")).weak());
        return;
    }

    let status_height = if state.scanning && state.image_rows.is_empty() {
        DIRECTORY_TREE_ROW_HEIGHT
    } else {
        0.0
    };
    let row_height = DIRECTORY_TREE_IMAGE_ROW_HEIGHT;
    let row_spacing = ui.spacing().item_spacing.y;
    let row_height_with_spacing = row_height + row_spacing;
    let body_font = egui::FontId::proportional(ui.style().text_styles[&egui::TextStyle::Body].size);
    state.ensure_image_list_column_widths(
        ui.painter(),
        &body_font,
        &t!("directory_tree.col_size"),
        &t!("directory_tree.col_modified"),
    );
    let column_layout = image_list_column_layout(
        ui.available_width(),
        ui.spacing().item_spacing.x,
        state.image_list_col_size_w,
        state.image_list_col_modified_w,
    );

    draw_image_details_header(ui, state, &column_layout, palette, command_tx);

    let interaction_enabled = image_list_interaction_enabled(state);
    let viewport_height = (ui.available_height() - status_height).max(row_height_with_spacing);

    if interaction_enabled {
        try_handle_image_list_arrow_keys(ui, state, list_focus_id, command_tx, embedded);
    }

    let mut pending_scroll_offset = None;
    if list_enabled && state.scroll_image_list_to_current && !state.image_rows.is_empty() {
        pending_scroll_offset = min_scroll_offset_to_show_row(
            state.current_index,
            row_height_with_spacing,
            row_height,
            viewport_height,
            state.image_list_scroll_offset_y,
        )
        .map(|offset| offset.max(0.0));
        state.scroll_image_list_to_current = false;
    }

    ui.add_enabled_ui(list_enabled && interaction_enabled, |ui| {
        let mut scroll = egui::ScrollArea::vertical()
            .id_salt("directory_tree_images")
            .auto_shrink([false, false])
            .max_height(viewport_height);

        if let Some(offset) = pending_scroll_offset {
            scroll = scroll.vertical_scroll_offset(offset);
        }

        let total_rows = state.image_rows.len();
        let current_index = state.current_index;
        let scroll_output = scroll.show_rows(ui, row_height, total_rows, |ui, row_range| {
            state.image_list_visible_row_range = Some((row_range.start, row_range.end));
            for row_index in row_range {
                let Some(row) = state.image_rows.get(row_index) else {
                    continue;
                };
                let clicked = draw_image_details_row(
                    ui,
                    row,
                    row_index,
                    row_index == current_index,
                    &column_layout,
                    state.preview_textures.get(&row_index),
                    state.preview_logical_sizes.get(&row_index).copied(),
                    command_tx,
                    list_enabled && interaction_enabled,
                    palette,
                );
                if clicked {
                    ui.memory_mut(|mem| mem.request_focus(list_focus_id));
                    state.image_list_keyboard_active = true;
                }
            }
        });
        state.image_list_scroll_offset_y = scroll_output.state.offset.y;
    });

    if state.scanning && state.image_rows.is_empty() {
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), status_height),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.spinner();
                ui.label(egui::RichText::new(state.scan_status.as_str()).weak());
            },
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ImageListColumnLayout {
    size_w: f32,
    modified_w: f32,
}

const IMAGE_LIST_COL_CELL_PADDING: f32 = 4.0;
/// Fixed `YYYY/MM/DD HH:MM:SS` cell sample; all modified cells use this format.
const IMAGE_LIST_MODIFIED_CELL_SAMPLE: &str = "2000/01/01 00:00:00";

fn measure_image_list_content_column_widths(
    painter: &egui::Painter,
    body_font: &egui::FontId,
    header_size: &str,
    header_modified: &str,
    rows: &[DirectoryTreeFileRow],
) -> (f32, f32) {
    let measure = |text: &str| {
        painter
            .layout_no_wrap(
                text.to_owned(),
                body_font.clone(),
                egui::Color32::PLACEHOLDER,
            )
            .size()
            .x
    };
    let mut size_w = measure(header_size);
    for row in rows {
        size_w = size_w.max(measure(&format_file_size(row.size_bytes)));
    }
    let modified_w = measure(header_modified)
        .max(measure(IMAGE_LIST_MODIFIED_CELL_SAMPLE))
        .max(measure("-"));
    (
        size_w + IMAGE_LIST_COL_CELL_PADDING,
        modified_w + IMAGE_LIST_COL_CELL_PADDING,
    )
}

fn image_list_column_layout(
    row_width: f32,
    spacing_x: f32,
    ideal_size_w: f32,
    ideal_modified_w: f32,
) -> ImageListColumnLayout {
    let thumb_w = DIRECTORY_TREE_COL_THUMB_WIDTH;
    let gutters = spacing_x * 4.0;
    let ideal_fixed =
        thumb_w + ideal_size_w + ideal_modified_w + gutters + DIRECTORY_TREE_COL_NAME_MIN_WIDTH;
    if row_width >= ideal_fixed {
        return ImageListColumnLayout {
            size_w: ideal_size_w,
            modified_w: ideal_modified_w,
        };
    }

    let available_for_right_cols =
        (row_width - gutters - thumb_w - DIRECTORY_TREE_COL_NAME_MIN_WIDTH).max(0.0);
    let mut modified_w = (available_for_right_cols * 0.62).clamp(
        DIRECTORY_TREE_COL_MODIFIED_MIN_WIDTH.min(available_for_right_cols),
        ideal_modified_w,
    );
    let mut size_w = (available_for_right_cols - modified_w).clamp(0.0, ideal_size_w);
    if size_w < DIRECTORY_TREE_COL_SIZE_MIN_WIDTH && available_for_right_cols > 0.0 {
        size_w = available_for_right_cols
            .min(ideal_size_w)
            .min(DIRECTORY_TREE_COL_SIZE_MIN_WIDTH);
        modified_w = (available_for_right_cols - size_w).max(0.0);
    }
    ImageListColumnLayout { size_w, modified_w }
}

fn image_list_thumb_column(row_rect: egui::Rect, spacing_x: f32) -> egui::Rect {
    let left = row_rect.left() + spacing_x;
    egui::Rect::from_min_max(
        egui::pos2(left, row_rect.top()),
        egui::pos2(left + DIRECTORY_TREE_COL_THUMB_WIDTH, row_rect.bottom()),
    )
}

fn image_list_modified_column(
    row_rect: egui::Rect,
    columns: &ImageListColumnLayout,
    spacing_x: f32,
) -> egui::Rect {
    let right = row_rect.right() - spacing_x;
    let left = (right - columns.modified_w).max(row_rect.left());
    egui::Rect::from_min_max(
        egui::pos2(left, row_rect.top()),
        egui::pos2(right, row_rect.bottom()),
    )
}

fn image_list_size_column(
    row_rect: egui::Rect,
    columns: &ImageListColumnLayout,
    spacing_x: f32,
) -> egui::Rect {
    let modified = image_list_modified_column(row_rect, columns, spacing_x);
    let right = (modified.left() - spacing_x).max(row_rect.left());
    let left = (right - columns.size_w).max(row_rect.left());
    egui::Rect::from_min_max(
        egui::pos2(left, row_rect.top()),
        egui::pos2(right, row_rect.bottom()),
    )
}

fn image_list_name_column(
    row_rect: egui::Rect,
    columns: &ImageListColumnLayout,
    spacing_x: f32,
) -> egui::Rect {
    let thumb = image_list_thumb_column(row_rect, spacing_x);
    let size = image_list_size_column(row_rect, columns, spacing_x);
    let left = thumb.right() + spacing_x;
    let right = (size.left() - spacing_x).max(left);
    egui::Rect::from_min_max(
        egui::pos2(left, row_rect.top()),
        egui::pos2(right, row_rect.bottom()),
    )
}

fn paint_clipped_galley(
    painter: &egui::Painter,
    galley: std::sync::Arc<egui::Galley>,
    column: egui::Rect,
    color: egui::Color32,
    halign: egui::Align,
) {
    let x = match halign {
        egui::Align::RIGHT => column.right() - galley.size().x,
        egui::Align::Center => column.center().x - galley.size().x * 0.5,
        _ => column.left(),
    };
    let y = column.center().y - galley.size().y * 0.5;
    painter
        .with_clip_rect(column)
        .galley(egui::pos2(x, y), galley, color);
}

fn truncate_single_line_text(
    painter: &egui::Painter,
    text: &str,
    font: &egui::FontId,
    max_width: f32,
) -> String {
    let measure = |value: &str| {
        painter
            .layout_no_wrap(value.to_string(), font.clone(), egui::Color32::PLACEHOLDER)
            .size()
            .x
    };
    if max_width <= 0.0 {
        return String::from('…');
    }
    if measure(text) <= max_width {
        return text.to_string();
    }
    let mut lo = 0usize;
    let mut hi = text.chars().count();
    while lo < hi {
        let mid = (lo + hi + 1) / 2;
        let mut candidate = text.chars().take(mid).collect::<String>();
        candidate.push('…');
        if measure(&candidate) <= max_width {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    if lo == 0 {
        return String::from('…');
    }
    let mut out = text.chars().take(lo).collect::<String>();
    out.push('…');
    out
}

fn image_list_interaction_enabled(state: &DirectoryTreeState) -> bool {
    !state.scanning && !state.image_list_reordering
}

fn image_list_sorting_available(state: &DirectoryTreeState) -> bool {
    image_list_interaction_enabled(state) && !state.image_rows.is_empty()
}

fn image_list_sort_order(
    len: usize,
    column: ImageListSortColumn,
    ascending: bool,
    paths: &[PathBuf],
    sizes: &[u64],
    modified: &[Option<i64>],
) -> Vec<usize> {
    let mut order: Vec<usize> = (0..len).collect();
    order.sort_by(|&left, &right| {
        let ordering = compare_image_list_sort_keys(left, right, column, paths, sizes, modified);
        let primary = if ascending {
            ordering
        } else {
            ordering.reverse()
        };
        primary.then_with(|| {
            if ascending {
                left.cmp(&right)
            } else {
                right.cmp(&left)
            }
        })
    });
    order
}

fn compare_image_list_sort_keys(
    left: usize,
    right: usize,
    column: ImageListSortColumn,
    paths: &[PathBuf],
    sizes: &[u64],
    modified: &[Option<i64>],
) -> Ordering {
    match column {
        ImageListSortColumn::Name => paths[left]
            .file_name()
            .map(|name| name.to_ascii_lowercase())
            .cmp(
                &paths[right]
                    .file_name()
                    .map(|name| name.to_ascii_lowercase()),
            ),
        ImageListSortColumn::Size => sizes
            .get(left)
            .copied()
            .unwrap_or(0)
            .cmp(&sizes.get(right).copied().unwrap_or(0)),
        ImageListSortColumn::Modified => compare_optional_unix_time(
            modified.get(left).copied().flatten(),
            modified.get(right).copied().flatten(),
        ),
    }
}

fn compare_optional_unix_time(left: Option<i64>, right: Option<i64>) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => left.cmp(&right),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn image_list_sort_indicator(
    column: ImageListSortColumn,
    state: &DirectoryTreeState,
) -> &'static str {
    if !state.image_list_sort_active || state.image_list_sort_column != column {
        return "";
    }
    if state.image_list_sort_ascending {
        " ▲"
    } else {
        " ▼"
    }
}

fn draw_image_details_header(
    ui: &mut egui::Ui,
    state: &DirectoryTreeState,
    columns: &ImageListColumnLayout,
    palette: &ThemePalette,
    command_tx: &Sender<DirectoryTreeCommand>,
) {
    let header_width = ui.available_width();
    let header_rect = egui::Rect::from_min_size(
        ui.cursor().min,
        egui::vec2(header_width, DIRECTORY_TREE_HEADER_HEIGHT),
    );
    ui.allocate_exact_size(
        egui::vec2(header_width, DIRECTORY_TREE_HEADER_HEIGHT),
        egui::Sense::hover(),
    );
    let spacing_x = ui.spacing().item_spacing.x;
    let header_font =
        egui::FontId::proportional(ui.style().text_styles[&egui::TextStyle::Body].size);
    let weak = palette.text_muted;
    let sorting_enabled = image_list_sorting_available(state);

    let paint_header =
        |column: ImageListSortColumn, label: String, rect: egui::Rect, halign: egui::Align| {
            let text = format!("{}{}", label, image_list_sort_indicator(column, state));
            let galley = ui.painter().layout_no_wrap(text, header_font.clone(), weak);
            paint_clipped_galley(ui.painter(), galley, rect, weak, halign);
            if sorting_enabled {
                let response = ui.interact(
                    rect,
                    ui.id().with(("image_list_sort", column)),
                    egui::Sense::click(),
                );
                if response.clicked() {
                    let _ = command_tx.send(DirectoryTreeCommand::SortImageList(column));
                }
            }
        };

    paint_header(
        ImageListSortColumn::Name,
        t!("directory_tree.col_name").to_string(),
        image_list_name_column(header_rect, columns, spacing_x),
        egui::Align::LEFT,
    );
    paint_header(
        ImageListSortColumn::Size,
        t!("directory_tree.col_size").to_string(),
        image_list_size_column(header_rect, columns, spacing_x),
        egui::Align::RIGHT,
    );
    paint_header(
        ImageListSortColumn::Modified,
        t!("directory_tree.col_modified").to_string(),
        image_list_modified_column(header_rect, columns, spacing_x),
        egui::Align::LEFT,
    );
    ui.separator();
}

fn min_scroll_offset_to_show_row(
    row_index: usize,
    row_height_with_spacing: f32,
    row_height: f32,
    viewport_height: f32,
    scroll_offset_y: f32,
) -> Option<f32> {
    let row_top = row_index as f32 * row_height_with_spacing;
    let row_bottom = row_top + row_height;
    let view_top = scroll_offset_y;
    let view_bottom = scroll_offset_y + viewport_height;

    if row_top >= view_top && row_bottom <= view_bottom {
        return None;
    }
    if row_top < view_top {
        return Some(row_top);
    }
    if row_bottom > view_bottom {
        return Some(row_bottom - viewport_height);
    }
    None
}

fn wrapped_image_list_index(current: usize, delta: i32, len: usize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let next = match delta {
        1 => (current + 1) % len,
        -1 => {
            if current == 0 {
                len - 1
            } else {
                current - 1
            }
        }
        _ => return None,
    };
    if next == current { None } else { Some(next) }
}

fn try_handle_image_list_arrow_keys(
    ui: &mut egui::Ui,
    state: &mut DirectoryTreeState,
    list_focus_id: egui::Id,
    command_tx: &Sender<DirectoryTreeCommand>,
    embedded: bool,
) {
    if !ImageViewerApp::directory_tree_list_accepts_keyboard_input(ui.ctx(), embedded) {
        return;
    }

    let list_has_focus = ui.memory(|mem| mem.has_focus(list_focus_id));
    if !(state.image_list_keyboard_active || list_has_focus)
        || state.image_rows.is_empty()
        || !image_list_interaction_enabled(state)
    {
        return;
    }

    let current = state.current_index;
    let len = state.image_rows.len();
    let mut next = None;
    ui.input(|input| {
        if input.key_pressed(egui::Key::ArrowDown) {
            next = wrapped_image_list_index(current, 1, len);
        } else if input.key_pressed(egui::Key::ArrowUp) {
            next = wrapped_image_list_index(current, -1, len);
        }
    });
    let Some(index) = next else {
        return;
    };

    ui.input_mut(|input| {
        input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp);
        input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown);
    });
    ui.memory_mut(|mem| mem.request_focus(list_focus_id));
    state.image_list_keyboard_active = true;
    state.current_index = index;
    state.scroll_image_list_to_current = true;
    let _ = command_tx.send(DirectoryTreeCommand::SelectImage(index));
}

fn draw_image_details_row(
    ui: &mut egui::Ui,
    row: &DirectoryTreeFileRow,
    row_index: usize,
    selected: bool,
    columns: &ImageListColumnLayout,
    texture: Option<&egui::TextureHandle>,
    logical_size: Option<(u32, u32)>,
    command_tx: &Sender<DirectoryTreeCommand>,
    list_enabled: bool,
    palette: &ThemePalette,
) -> bool {
    let row_width = ui.available_width();
    let (row_rect, response) = ui.allocate_exact_size(
        egui::vec2(row_width, DIRECTORY_TREE_IMAGE_ROW_HEIGHT),
        egui::Sense::click(),
    );
    if ui.is_rect_visible(row_rect) {
        if selected {
            ui.painter()
                .rect_filled(row_rect, 0.0, directory_tree_row_selected_fill(palette));
        } else if response.hovered() {
            ui.painter()
                .rect_filled(row_rect, 0.0, palette.widget_hover);
        }

        let spacing_x = ui.spacing().item_spacing.x;
        let thumb_column = image_list_thumb_column(row_rect, spacing_x);
        paint_image_list_thumbnail(ui.painter(), palette, thumb_column, texture, logical_size);

        let text_color = if selected {
            directory_tree_row_selected_text(palette)
        } else {
            palette.text_normal
        };
        let body_font =
            egui::FontId::proportional(ui.style().text_styles[&egui::TextStyle::Body].size);
        let size_text = format_file_size(row.size_bytes);
        let modified_text = row
            .modified_unix
            .map(format_file_modified)
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| String::from("-"));

        let name_column = image_list_name_column(row_rect, columns, spacing_x);
        let size_column = image_list_size_column(row_rect, columns, spacing_x);
        let modified_column = image_list_modified_column(row_rect, columns, spacing_x);

        let name_text =
            truncate_single_line_text(ui.painter(), &row.name, &body_font, name_column.width());
        let name_galley = ui
            .painter()
            .layout_no_wrap(name_text, body_font.clone(), text_color);
        paint_clipped_galley(
            ui.painter(),
            name_galley,
            name_column,
            text_color,
            egui::Align::LEFT,
        );

        let size_galley = ui
            .painter()
            .layout_no_wrap(size_text, body_font.clone(), text_color);
        paint_clipped_galley(
            ui.painter(),
            size_galley,
            size_column,
            text_color,
            egui::Align::RIGHT,
        );

        let modified_galley = ui
            .painter()
            .layout_no_wrap(modified_text, body_font, text_color);
        paint_clipped_galley(
            ui.painter(),
            modified_galley,
            modified_column,
            text_color,
            egui::Align::LEFT,
        );
    }

    if list_enabled && response.clicked() {
        let _ = command_tx.send(DirectoryTreeCommand::SelectImage(row_index));
        return true;
    }
    response.on_hover_text(row.path.to_string_lossy());
    false
}

fn directory_display_name(path: &Path) -> String {
    if is_places_sentinel_path(path) {
        return String::new();
    }
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

fn should_expand_this_pc_for_path(selected: &Path, known_folders: &[KnownFolderEntry]) -> bool {
    if is_unc_path(selected) {
        return false;
    }
    if known_folders.iter().any(|entry| {
        selected == entry.filesystem_path.as_path() || selected.starts_with(&entry.filesystem_path)
    }) {
        return false;
    }
    let Some(root) = volume_root_for_path(selected) else {
        return false;
    };
    #[cfg(windows)]
    {
        let _ = root;
        return true;
    }
    #[cfg(not(windows))]
    {
        root.components().count() > 1 || root.as_os_str() == "/"
    }
}

fn filesystem_ancestor_chain(target: &Path) -> Vec<PathBuf> {
    if let Some(root) = volume_root_for_path(target) {
        if target == root.as_path() {
            return vec![root];
        }
        let mut chain = vec![root.clone()];
        if let Ok(relative) = target.strip_prefix(&root) {
            let mut current = root;
            for component in relative.components() {
                current.push(component);
                chain.push(current.clone());
            }
        } else {
            chain.push(target.to_path_buf());
        }
        return chain;
    }

    let mut chain = vec![target.to_path_buf()];
    let mut current = target.to_path_buf();
    while current.pop() {
        chain.push(current.clone());
    }
    chain.reverse();
    chain
}

fn volume_root_for_path(path: &Path) -> Option<PathBuf> {
    if let Some(share_root) = unc_share_root(path) {
        return Some(share_root);
    }

    #[cfg(windows)]
    {
        let text = path.to_string_lossy();
        let bytes = text.as_bytes();
        if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
            return Some(PathBuf::from(format!("{}:\\", bytes[0] as char)));
        }
        return None;
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(rest) = path.strip_prefix("/Volumes") {
            if let Some(name) = rest.components().next() {
                return Some(PathBuf::from("/Volumes").join(name.as_os_str()));
            }
        }
        return None;
    }

    #[cfg(target_os = "linux")]
    {
        for prefix in ["/media", "/mnt"] {
            if let Ok(rest) = path.strip_prefix(prefix) {
                if let Some(name) = rest.components().next() {
                    return Some(PathBuf::from(prefix).join(name.as_os_str()));
                }
            }
        }
        if path.has_root() {
            return Some(PathBuf::from("/"));
        }
        return None;
    }

    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    path.parent().map(|parent| parent.to_path_buf())
}

fn unc_share_root(path: &Path) -> Option<PathBuf> {
    if !is_unc_path(path) {
        return None;
    }
    let text = path.to_string_lossy();
    let trimmed = text.trim_start_matches(r"\\").trim_start_matches("//");
    let mut parts = trimmed.split(['\\', '/']).filter(|part| !part.is_empty());
    let server = parts.next()?;
    let share = parts.next()?;
    Some(PathBuf::from(format!(r"\\{server}\{share}")))
}

fn unc_share_display_name(share_root: &Path) -> String {
    let text = share_root.to_string_lossy();
    text.trim_start_matches(r"\\")
        .trim_start_matches("//")
        .to_string()
}

fn directory_ancestor_chain(root: &Path, target: &Path) -> Vec<PathBuf> {
    if target == root {
        return vec![root.to_path_buf()];
    }
    if !target.starts_with(root) {
        return vec![target.to_path_buf()];
    }

    let mut chain = vec![root.to_path_buf()];
    if let Ok(relative) = target.strip_prefix(root) {
        let mut current = root.to_path_buf();
        for component in relative.components() {
            current.push(component);
            chain.push(current.clone());
        }
    }
    chain
}

#[cfg(test)]
mod tests {
    use super::*;
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
        assert!(is_non_browsable_system_directory(Path::new(
            r"F:\$RECYCLE.BIN"
        )));
        assert!(is_non_browsable_system_directory(Path::new(
            r"C:\System Volume Information"
        )));
        assert!(!is_non_browsable_system_directory(Path::new(r"F:\photos")));
    }

    #[test]
    fn apply_children_result_ignores_stale_generation() {
        let root = PathBuf::from("/tmp/siv-dir-tree-test-root");
        let child = PathBuf::from("/tmp/siv-dir-tree-test-child");

        let mut state = DirectoryTreeState {
            places_loaded: true,
            selected_dir: Some(root.clone()),
            generation: 2,
            ..DirectoryTreeState::default()
        };
        state.nodes.insert(
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
        );

        state.apply_children_result(DirectoryChildrenResult {
            tree_path: root.clone(),
            generation: 1,
            result: Ok(vec![child.clone()]),
        });

        let node = state.nodes.get(&root).expect("root node");
        assert!(node.loading);
        assert!(!node.children_loaded);
        assert!(node.children.is_empty());
        assert!(!state.nodes.contains_key(&child));
    }

    #[test]
    fn apply_children_result_merges_children_and_clears_loading() {
        let root = PathBuf::from("/tmp/siv-dir-tree-test-root-2");
        let child = PathBuf::from("/tmp/siv-dir-tree-test-child-2");

        let mut state = DirectoryTreeState {
            places_loaded: true,
            selected_dir: Some(root.clone()),
            generation: 1,
            ..DirectoryTreeState::default()
        };
        state.nodes.insert(
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
        );

        state.apply_children_result(DirectoryChildrenResult {
            tree_path: root.clone(),
            generation: 1,
            result: Ok(vec![child.clone()]),
        });

        let node = state.nodes.get(&root).expect("root node");
        assert!(!node.loading);
        assert!(node.children_loaded);
        assert_eq!(node.children, vec![child.clone()]);
        assert!(state.nodes.contains_key(&child));
    }

    #[test]
    fn apply_children_result_records_read_error() {
        let root = PathBuf::from("/tmp/siv-dir-tree-test-missing");

        let mut state = DirectoryTreeState {
            places_loaded: true,
            selected_dir: Some(root.clone()),
            generation: 1,
            ..DirectoryTreeState::default()
        };
        state.nodes.insert(
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
        );

        state.apply_children_result(DirectoryChildrenResult {
            tree_path: root.clone(),
            generation: 1,
            result: Err("permission denied".to_string()),
        });

        let node = state.nodes.get(&root).expect("root node");
        assert!(!node.loading);
        assert!(node.children_loaded);
        assert!(node.children.is_empty());
        assert_eq!(node.error.as_deref(), Some("permission denied"));
    }

    #[test]
    fn apply_metadata_result_ignores_stale_generation() {
        let mut state = DirectoryTreeState::default();
        state.image_list_generation = 2;
        state.image_rows = vec![DirectoryTreeFileRow {
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

        assert!(state.image_rows[0].modified_unix.is_none());
    }

    #[test]
    fn apply_metadata_result_updates_modified_times() {
        let mut state = DirectoryTreeState::default();
        state.image_list_generation = 1;
        state.image_rows = vec![
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

        assert_eq!(state.image_rows[0].modified_unix, Some(1_700_000_000));
        assert!(state.image_rows[1].modified_unix.is_none());
    }

    #[test]
    fn left_panel_width_limits_stay_ordered_on_narrow_viewport() {
        let (min, max) = directory_tree_left_panel_width_limits(364.0);
        assert!(min <= max);
        assert_eq!(min, 174.0);
        assert_eq!(clamp_directory_tree_left_panel_width(340.0, 364.0), 174.0);
    }

    #[test]
    fn reconcile_directory_tree_panel_widths_honors_saved_split() {
        let mut state = DirectoryTreeState::default();
        state.left_panel_width = 280.0;
        state.image_list_panel_width = 320.0;
        reconcile_directory_tree_panel_widths(&mut state, 640.0);
        assert_eq!(state.left_panel_width, 280.0);
        assert_eq!(state.image_list_panel_width, 350.0);
    }

    #[test]
    fn reconcile_directory_tree_panel_widths_shrinks_when_viewport_is_narrow() {
        let mut state = DirectoryTreeState::default();
        state.left_panel_width = 340.0;
        state.image_list_panel_width = 400.0;
        reconcile_directory_tree_panel_widths(&mut state, 364.0);
        assert!(state.left_panel_width + state.image_list_panel_width <= 354.0);
        assert!(state.image_list_panel_width >= DIRECTORY_TREE_RIGHT_MIN_WIDTH);
    }

    #[test]
    fn visible_cold_strip_indices_skips_stale_range_while_scroll_pending() {
        assert!(ImageViewerApp::visible_cold_strip_indices(Some((100, 110)), true, 200).is_empty());
        assert_eq!(
            ImageViewerApp::visible_cold_strip_indices(Some((100, 110)), false, 200),
            (100..110).collect::<Vec<_>>()
        );
    }

    #[test]
    fn sync_images_marks_list_scroll_when_current_index_changes() {
        let (metadata_tx, _metadata_rx) = crossbeam_channel::unbounded();
        let paths = vec![PathBuf::from("/tmp/a.avif"), PathBuf::from("/tmp/b.avif")];
        let mut state = DirectoryTreeState::default();
        state.image_rows = paths
            .iter()
            .map(|path| DirectoryTreeFileRow {
                path: path.clone(),
                name: directory_display_name(path),
                size_bytes: 0,
                modified_unix: None,
            })
            .collect();
        state.current_index = 0;
        state.scroll_image_list_to_current = false;

        state.sync_images(
            &paths,
            &[0, 0],
            &[None, None],
            1,
            false,
            String::new(),
            &metadata_tx,
        );

        assert_eq!(state.current_index, 1);
        assert!(state.scroll_image_list_to_current);
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
            egui::vec2(320.0, DIRECTORY_TREE_IMAGE_ROW_HEIGHT),
        );
        let columns = image_list_column_layout(row_rect.width(), 4.0, 72.0, 148.0);
        let spacing = 4.0;
        let thumb = image_list_thumb_column(row_rect, spacing);
        let name = image_list_name_column(row_rect, &columns, spacing);
        let size = image_list_size_column(row_rect, &columns, spacing);
        let modified = image_list_modified_column(row_rect, &columns, spacing);
        assert!(thumb.right() <= name.left());
        assert!(name.right() <= size.left());
        assert!(size.right() <= modified.left());
    }

    #[test]
    fn image_list_columns_use_content_widths_when_panel_is_wide() {
        let columns = image_list_column_layout(640.0, 4.0, 72.0, 148.0);
        assert_eq!(columns.size_w, 72.0);
        assert_eq!(columns.modified_w, 148.0);
    }

    #[test]
    fn image_list_thumb_column_has_fixed_width() {
        let row_rect = egui::Rect::from_min_size(
            egui::pos2(10.0, 0.0),
            egui::vec2(400.0, DIRECTORY_TREE_IMAGE_ROW_HEIGHT),
        );
        let thumb = image_list_thumb_column(row_rect, 4.0);
        assert!((thumb.width() - DIRECTORY_TREE_COL_THUMB_WIDTH).abs() < f32::EPSILON);
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
            this_pc_label: "This PC".to_string(),
            network_label: "Network".to_string(),
        };

        let mut state = DirectoryTreeState::default();
        state.initialize_places(places);
        state.set_selected_dir(PathBuf::from("//192.168.2.1/pictures/2024"));

        assert!(state.network_visible);
        let network = state.nodes.get(&network_tree_path()).expect("network node");
        assert_eq!(
            network.children,
            vec![PathBuf::from("//192.168.2.1/pictures")]
        );
        assert!(
            state
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
            this_pc_label: "This PC".to_string(),
            network_label: "Network".to_string(),
        };

        let mut state = DirectoryTreeState::default();
        state.initialize_places(places.clone());
        assert_eq!(state.generation, 1);
        assert!(state.places_loaded);
        assert!(state.nodes.contains_key(&this_pc_tree_path()));
        assert!(!state.network_visible);
        assert!(!state.nodes.contains_key(&network_tree_path()));

        state.nodes.insert(
            PathBuf::from("/tmp/siv-dir-tree-stale"),
            directory_tree_node("stale", PathBuf::from("/tmp/siv-dir-tree-stale")),
        );

        state.initialize_places(places);
        assert_eq!(state.generation, 2);
        assert_eq!(state.nodes.len(), 1);
        assert!(state.nodes.contains_key(&this_pc_tree_path()));
        assert!(!state.network_visible);
        assert!(!state.nodes.contains_key(&network_tree_path()));
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
            this_pc_label: "This PC".to_string(),
            network_label: "Network".to_string(),
        };

        let mut state = DirectoryTreeState::default();
        state.initialize_places(places);
        state.set_selected_dir(docs_fs);
        let _requests = state.reveal_selected_dir();
        assert!(
            !state
                .nodes
                .get(&this_pc_tree_path())
                .is_some_and(|node| node.expanded)
        );
    }
}
