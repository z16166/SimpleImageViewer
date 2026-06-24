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

//! Directory tree: shell namespace paths vs filesystem paths.
//!
//! - [`DirectoryTreeNode`] and node arena keys use `namespace_path` (`\\?\siv-tree\...`).
//! - `fs_path` is the on-disk / UNC location passed to OS directory APIs.
//! - `children` lists child node `namespace_path` values only.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::thread::JoinHandle;
use std::time::Duration;

use arc_swap::ArcSwap;
use crossbeam_channel::{Receiver, Sender, TrySendError};
use eframe::egui;
use parking_lot::Mutex;
use rust_i18n::t;

use crate::directory_tree_places::{DirectoryTreePlaces, KnownFolderEntry};
use crate::path_location::is_unc_path;
use crate::ui::osd::{format_file_modified, format_file_size};

pub(super) const MAX_DIRECTORY_TREE_NODES: usize = 8192;
/// Maximum ancestor segments expanded in one reveal; avoids flooding workers on deep paths.
pub(super) const MAX_DIRECTORY_TREE_REVEAL_DEPTH: usize = 512;
/// Max frames to defer file-list sync while tree/list mutexes are contended.
pub(super) const DIRECTORY_TREE_SYNC_MAX_DEFER_FRAMES: u32 = 120;

pub(super) const DIRECTORY_TREE_VIEWPORT_ID: &str = "siv_directory_tree_viewport";
pub(super) const DIRECTORY_TREE_EMBEDDED_SIDE_PANEL_ID: &str = "siv_directory_tree_embedded";
pub(super) const DIRECTORY_TREE_EMBEDDED_LOADING_PANEL_ID: &str =
    "siv_directory_tree_embedded_loading";
pub(super) const DIRECTORY_TREE_NAV_WHEEL_BLOCK_RECT_ID: &str =
    "siv_directory_tree_nav_wheel_block_rect";
pub(super) const DIRECTORY_TREE_EMBEDDED_DEFAULT_WIDTH: f32 = 380.0;
pub(super) const DIRECTORY_TREE_EMBEDDED_MIN_WIDTH: f32 = 320.0;
pub(super) const DIRECTORY_TREE_MIN_WIDTH: f32 = 640.0;
pub(super) const DIRECTORY_TREE_MIN_HEIGHT: f32 = 420.0;
pub(super) const DIRECTORY_TREE_LEFT_WIDTH: f32 = 340.0;
/// Minimum folder-tree width when dragging the center splitter (0 allows maximizing the file list).
pub(super) const DIRECTORY_TREE_LEFT_MIN_WIDTH: f32 = 0.0;
pub(super) const DIRECTORY_TREE_RIGHT_MIN_WIDTH: f32 = 180.0;
pub(super) const DIRECTORY_TREE_SPLITTER_GRAB_WIDTH: f32 = 10.0;
pub(super) const DIRECTORY_TREE_IMAGE_ROW_HEIGHT_COMPACT: f32 = 22.0;
pub(super) const DIRECTORY_TREE_COLD_NEIGHBOR_RADIUS: usize = 20;
pub(super) const MAX_COLD_STRIP_GENERATES_PER_FRAME: usize = 2;
pub(super) const MAX_STRIP_GENERATE_INFLIGHT: usize = 2;
/// Higher strip cold-gen limits while the file list is first filling after a scan.
pub(super) const MAX_COLD_STRIP_GENERATES_PER_FRAME_BOOTSTRAP: usize = 4;
pub(super) const MAX_STRIP_GENERATE_INFLIGHT_BOOTSTRAP: usize = 4;
/// Max list rows scanned per frame when bootstrap runs before the UI reports a visible range.
pub(super) const BOOTSTRAP_STRIP_VISIBLE_ROW_CAP: usize = 64;
/// End bootstrap high-throughput mode if the list never publishes a visible row range.
pub(super) const MAX_DIRECTORY_TREE_STRIP_BOOTSTRAP_FRAMES: u32 = 120;
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
pub(super) const DIRECTORY_TREE_UI_STROKE_WIDTH: f32 = 1.15;
pub(super) const DIRECTORY_TREE_NODE_ICON_DRAW_RATIO: f32 = 0.78;
pub(super) const DIRECTORY_TREE_DOWNLOADS_TRAY_HEIGHT_RATIO: f32 = 0.34;
/// Share of narrow-panel width allocated to the modified-time column (remainder is size).
pub(super) const DIRECTORY_TREE_IMAGE_LIST_MODIFIED_COL_WEIGHT: f32 = 0.62;
pub(super) const DIRECTORY_TREE_PLACES_LOAD_TIMEOUT: Duration = Duration::from_secs(60);
pub(super) use namespace::{NETWORK_NAMESPACE_PATH, THIS_PC_NAMESPACE_PATH};
/// Linux root path; used by `namespace_path_for_fs_path` fallback when `/` is a Places mount.
const UNIX_FS_ROOT: &str = "/";

pub(super) fn this_pc_namespace_path() -> PathBuf {
    PathBuf::from(THIS_PC_NAMESPACE_PATH)
}

pub(super) fn network_namespace_path() -> PathBuf {
    PathBuf::from(NETWORK_NAMESPACE_PATH)
}

pub(super) fn is_this_pc_namespace_path(path: &Path) -> bool {
    path.as_os_str() == OsStr::new(THIS_PC_NAMESPACE_PATH)
}

pub(super) fn is_network_namespace_path(path: &Path) -> bool {
    path.as_os_str() == OsStr::new(NETWORK_NAMESPACE_PATH)
}

pub(super) fn is_places_sentinel_namespace_path(path: &Path) -> bool {
    namespace::is_places_sentinel_namespace_path(path)
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
    SelectDirectory {
        namespace_path: PathBuf,
        fs_path: PathBuf,
    },
    ToggleExpanded(PathBuf),
    SelectImage(usize),
    SelectImageAndHideNav(usize),
    SortImageList(ImageListSortColumn),
    CloseWindow,
}

/// Non-blocking UI -> logic command; drops with a warning if the bounded channel is full.
pub(super) fn send_directory_tree_command(
    command_tx: &crossbeam_channel::Sender<DirectoryTreeCommand>,
    command: DirectoryTreeCommand,
) {
    if let Err(err) = command_tx.try_send(command) {
        match err {
            TrySendError::Full(dropped) => {
                log::warn!("[DirectoryTree] Command channel full; dropped {dropped:?}");
            }
            TrySendError::Disconnected(dropped) => {
                log::debug!("[DirectoryTree] Command channel disconnected; dropped {dropped:?}");
            }
        }
    }
}

#[derive(Debug)]
pub(crate) struct DirectoryChildrenRequest {
    namespace_path: PathBuf,
    fs_path: PathBuf,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DirectoryTreeFileRow {
    path: PathBuf,
    name: String,
    size_bytes: u64,
    modified_unix: Option<i64>,
    /// Cached for list paint; refreshed when size or modified metadata changes.
    size_text: String,
    modified_text: String,
}

/// Legacy scan paths stored milliseconds; values above this are treated as ms and converted to seconds.
const MODIFIED_UNIX_MILLIS_THRESHOLD: i64 = 1_000_000_000_000;

fn modified_unix_for_display(stored: i64) -> i64 {
    if stored > MODIFIED_UNIX_MILLIS_THRESHOLD {
        stored / 1_000
    } else {
        stored
    }
}

impl DirectoryTreeFileRow {
    pub(crate) fn new(
        path: PathBuf,
        name: String,
        size_bytes: u64,
        modified_unix: Option<i64>,
    ) -> Self {
        let mut row = Self {
            path,
            name,
            size_bytes,
            modified_unix,
            size_text: String::new(),
            modified_text: String::new(),
        };
        row.refresh_display_cache();
        row
    }

    pub(crate) fn refresh_display_cache(&mut self) {
        self.size_text = format_file_size(self.size_bytes);
        self.modified_text = self
            .modified_unix
            .map(modified_unix_for_display)
            .map(format_file_modified)
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| t!("directory_tree.modified_unknown").to_string());
    }
}

#[derive(Debug)]
pub(crate) struct DirectoryChildrenResult {
    namespace_path: PathBuf,
    generation: u64,
    result: Result<Vec<PathBuf>, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DirectoryTreeNode {
    display_name: String,
    /// On-disk / UNC path for OS directory APIs.
    fs_path: PathBuf,
    expanded: bool,
    loading: bool,
    children_loaded: bool,
    /// Child node keys in the shell namespace (`\\?\siv-tree\...`).
    children: Vec<PathBuf>,
    error: Option<String>,
}

mod domains;
pub(crate) use domains::{
    DirectoryTreeListSnapshot, DirectoryTreeListState, DirectoryTreePreviewSnapshot,
    DirectoryTreeTreeSnapshot, DirectoryTreeTreeState,
};

/// Combined writer access for tests and legacy call sites (separate runtime mutexes in production).
#[cfg(test)]
pub(crate) struct DirectoryTreeState {
    pub tree: DirectoryTreeTreeState,
    pub list: DirectoryTreeListState,
}

#[cfg(test)]
impl Default for DirectoryTreeState {
    fn default() -> Self {
        Self {
            tree: DirectoryTreeTreeState::default(),
            list: DirectoryTreeListState::default(),
        }
    }
}

#[cfg(test)]
#[allow(dead_code)]
impl DirectoryTreeState {
    pub(crate) fn initialize_places(&mut self, places: DirectoryTreePlaces) {
        self.tree.initialize_places(places);
    }

    pub(crate) fn set_selected_fs_path(&mut self, dir: PathBuf) {
        self.tree.set_selected_fs_path(dir);
    }

    pub(crate) fn restore_tree_selection(
        &mut self,
        dir: PathBuf,
        saved_namespace: Option<PathBuf>,
    ) {
        self.tree.restore_tree_selection(dir, saved_namespace);
    }

    pub(crate) fn expand_requests_for_selection(
        &mut self,
        dir: &Path,
    ) -> Vec<DirectoryChildrenRequest> {
        self.tree.expand_requests_for_selection(dir)
    }

    pub(crate) fn reveal_selected_namespace(&mut self) -> Vec<DirectoryChildrenRequest> {
        self.tree.reveal_selected_namespace()
    }

    pub(crate) fn expand_namespace_node(
        &mut self,
        namespace_path: &Path,
    ) -> Option<DirectoryChildrenRequest> {
        self.tree.expand_namespace_node(namespace_path)
    }

    pub(crate) fn expand_namespace_node_for_fs_path(
        &mut self,
        dir: &Path,
    ) -> Option<DirectoryChildrenRequest> {
        self.tree.expand_namespace_node_for_fs_path(dir)
    }

    pub(crate) fn mark_children_request_failed(&mut self, namespace_path: &Path, error: String) {
        self.tree
            .mark_children_request_failed(namespace_path, error);
    }

    pub(crate) fn sync_images(
        &mut self,
        images: &[PathBuf],
        sizes: &[u64],
        modified: &[Option<i64>],
        current_index: usize,
        scanning: bool,
        scan_status: String,
    ) -> Option<FileMetadataRequest> {
        self.list.sync_images(
            images,
            sizes,
            modified,
            current_index,
            scanning,
            scan_status,
        )
    }

    pub(crate) fn update_image_list_column_widths(&mut self, ctx: &egui::Context) {
        self.list.update_image_list_column_widths(ctx);
    }

    fn apply_children_result(&mut self, result: DirectoryChildrenResult) {
        self.tree.apply_children_result(result);
    }

    fn apply_metadata_result(&mut self, result: FileMetadataResult) {
        self.list.apply_metadata_result(result);
    }

    fn toggle_expanded(&mut self, path: &Path) -> Option<DirectoryChildrenRequest> {
        self.tree.toggle_expanded(path)
    }
}

/// Snapshot of list-preview layout passed from settings into draw/sync paths.
#[derive(Debug, Clone, Copy)]
pub(super) struct DirectoryTreeListPreviewLayout {
    pub show_previews: bool,
    pub thumb_px: f32,
    pub strip_max_side: u32,
}

impl DirectoryTreeListPreviewLayout {
    pub(super) fn from_settings(settings: &crate::settings::Settings) -> Self {
        let size = settings.directory_tree_list_preview_size;
        Self {
            show_previews: settings.directory_tree_show_list_previews,
            thumb_px: if settings.directory_tree_show_list_previews {
                size.thumb_px()
            } else {
                0.0
            },
            strip_max_side: size.strip_max_side(),
        }
    }

    pub(super) fn apply_to_list(self, list: &mut DirectoryTreeListState) {
        list.apply_preview_layout(self);
    }
}

pub(super) fn directory_tree_node(
    display_name: impl Into<String>,
    fs_path: PathBuf,
) -> DirectoryTreeNode {
    DirectoryTreeNode {
        display_name: display_name.into(),
        fs_path: fs_path.clone(),
        expanded: false,
        loading: false,
        children_loaded: false,
        children: Vec::new(),
        error: None,
    }
}

pub(super) fn children_request(
    namespace_path: PathBuf,
    fs_path: PathBuf,
    generation: u64,
) -> DirectoryChildrenRequest {
    DirectoryChildrenRequest {
        namespace_path,
        fs_path,
        generation,
    }
}

pub(crate) struct DirectoryTreeRuntime {
    pub(crate) tree: Arc<Mutex<DirectoryTreeTreeState>>,
    pub(crate) list: Arc<Mutex<DirectoryTreeListState>>,
    pub(crate) tree_snapshot: Arc<ArcSwap<DirectoryTreeTreeSnapshot>>,
    pub(crate) list_snapshot: Arc<ArcSwap<DirectoryTreeListSnapshot>>,
    pub(crate) preview_snapshot: Arc<ArcSwap<DirectoryTreePreviewSnapshot>>,
    pub(crate) view: Arc<ArcSwap<view::DirectoryTreeView>>,
    pub(crate) chrome: Arc<Mutex<view::DirectoryTreeUiChrome>>,
    pub(crate) last_list_publish_at: Mutex<std::time::Instant>,
    pub(crate) command_tx: Sender<DirectoryTreeCommand>,
    pub(crate) command_rx: Receiver<DirectoryTreeCommand>,
    pub(crate) children_request_tx: Sender<DirectoryChildrenRequest>,
    pub(crate) metadata_request_tx: Sender<FileMetadataRequest>,
    pub(crate) result_rx: Receiver<DirectoryChildrenResult>,
    pub(crate) metadata_result_rx: Receiver<FileMetadataResult>,
    /// Raw pointer to the live [`super::ImageViewerApp`], set during ROOT `prepare_directory_tree_file_list_viewport`.
    ///
    /// # Safety contract (UI thread only)
    /// - Written with [`Ordering::Release`] each ROOT `ui()` pass that registers the detached viewport.
    /// - Not cleared between frames so Detached-only repaints can still reach strip GPU upload and
    ///   context-menu handlers without waiting for ROOT `ui()` to run first.
    /// - Read with [`Ordering::Acquire`] only inside the detached viewport paint callback.
    /// - The app outlives all viewport callbacks because it is owned by eframe as `Box<dyn App>`.
    /// - Never dereference from worker threads or from `logic()`; use locked state / snapshots instead.
    pub(crate) viewpaint_app: Arc<std::sync::atomic::AtomicPtr<super::ImageViewerApp>>,
    workers_shutdown: Arc<AtomicBool>,
    children_worker: parking_lot::Mutex<Option<JoinHandle<()>>>,
    metadata_worker: parking_lot::Mutex<Option<JoinHandle<()>>>,
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

        let workers_shutdown = Arc::new(AtomicBool::new(false));
        let children_shutdown = Arc::clone(&workers_shutdown);
        let metadata_shutdown = Arc::clone(&workers_shutdown);

        let mut children_worker_alive = false;
        let children_worker = match std::thread::Builder::new()
            .name("siv-directory-tree-children".to_string())
            .spawn(move || {
                directory_tree_children_worker_loop(
                    children_request_rx,
                    result_tx,
                    children_shutdown,
                );
            }) {
            Ok(handle) => {
                children_worker_alive = true;
                Some(handle)
            }
            Err(err) => {
                log::error!("[DirectoryTree] Failed to spawn children worker: {err}");
                None
            }
        };

        let mut metadata_worker_alive = false;
        let metadata_worker = match std::thread::Builder::new()
            .name("siv-directory-tree-metadata".to_string())
            .spawn(move || {
                directory_tree_metadata_worker_loop(
                    metadata_request_rx,
                    metadata_result_tx,
                    metadata_shutdown,
                );
            }) {
            Ok(handle) => {
                metadata_worker_alive = true;
                Some(handle)
            }
            Err(err) => {
                log::error!("[DirectoryTree] Failed to spawn metadata worker: {err}");
                None
            }
        };

        let workers_available = children_worker_alive && metadata_worker_alive;
        if !workers_available {
            log::error!(
                "[DirectoryTree] Background workers unavailable (children={children_worker_alive} metadata={metadata_worker_alive})"
            );
        }

        let mut initial_tree = DirectoryTreeTreeState {
            workers_available,
            ..DirectoryTreeTreeState::default()
        };
        initial_tree.snapshot_dirty = true;
        let mut initial_list = DirectoryTreeListState::default();
        initial_list.snapshot_dirty = true;
        let tree_snapshot = Arc::new(ArcSwap::from_pointee(DirectoryTreeTreeSnapshot::default()));
        let list_snapshot = Arc::new(ArcSwap::from_pointee(DirectoryTreeListSnapshot::default()));
        let preview_snapshot = Arc::new(ArcSwap::from_pointee(
            DirectoryTreePreviewSnapshot::default(),
        ));
        let view = Arc::new(ArcSwap::from_pointee(view::DirectoryTreeView::assemble(
            tree_snapshot.load_full(),
            list_snapshot.load_full(),
            preview_snapshot.load_full(),
        )));
        let chrome = Arc::new(Mutex::new(view::DirectoryTreeUiChrome::from_domains(
            &initial_tree,
            &initial_list,
        )));

        Self {
            tree: Arc::new(Mutex::new(initial_tree)),
            list: Arc::new(Mutex::new(initial_list)),
            tree_snapshot,
            list_snapshot,
            preview_snapshot,
            view,
            chrome,
            last_list_publish_at: Mutex::new(std::time::Instant::now()),
            command_tx,
            command_rx,
            children_request_tx,
            metadata_request_tx,
            result_rx,
            metadata_result_rx,
            viewpaint_app: Arc::new(std::sync::atomic::AtomicPtr::new(std::ptr::null_mut())),
            workers_shutdown,
            children_worker: parking_lot::Mutex::new(children_worker),
            metadata_worker: parking_lot::Mutex::new(metadata_worker),
        }
    }

    pub(crate) fn shutdown_workers(&self) {
        self.workers_shutdown.store(true, AtomicOrdering::Release);
    }

    pub(crate) fn join_workers(&mut self) {
        self.shutdown_workers();
        if let Some(handle) = self.children_worker.lock().take()
            && handle.join().is_err()
        {
            log::warn!("[DirectoryTree] Children worker panicked on join");
        }
        if let Some(handle) = self.metadata_worker.lock().take()
            && handle.join().is_err()
        {
            log::warn!("[DirectoryTree] Metadata worker panicked on join");
        }
    }

    /// Re-translate cached list row strings after a locale change.
    pub(crate) fn on_language_changed(&self) {
        let mut list = self.list.lock();
        list.refresh_all_display_caches();
        list.mark_snapshot_dirty();
    }
}

impl DirectoryTreeTreeState {
    fn note_nodes_cap_reached(&mut self, context_path: &Path) {
        log::warn!(
            "[DirectoryTree] Node cap ({MAX_DIRECTORY_TREE_NODES}) reached at {}",
            context_path.display()
        );
        let cap_error = t!("directory_tree.nodes_cap_reached").to_string();
        if let Some(node) = self.nodes.get_mut(context_path) {
            node.error = Some(cap_error);
            self.mark_snapshot_dirty();
            return;
        }
        if let Some(parent) = context_path.parent()
            && let Some(node) = self.nodes.get_mut(parent)
        {
            node.error = Some(cap_error);
            self.mark_snapshot_dirty();
        }
    }

    fn insert_tree_node(&mut self, path: PathBuf, node: DirectoryTreeNode) {
        match self
            .nodes
            .insert(path.clone(), node, MAX_DIRECTORY_TREE_NODES)
        {
            Ok(()) => {}
            Err(node_store::InsertNodeError::CapReached) => {
                self.note_nodes_cap_reached(&path);
            }
            Err(node_store::InsertNodeError::IdOverflow) => {
                log::error!(
                    "[DirectoryTree] Node arena id overflow at {}",
                    path.display()
                );
            }
        }
    }

    fn or_insert_tree_node<F: FnOnce() -> DirectoryTreeNode>(
        &mut self,
        path: PathBuf,
        f: F,
    ) -> bool {
        match self
            .nodes
            .or_insert_with(path.clone(), MAX_DIRECTORY_TREE_NODES, f)
        {
            Ok(_) => true,
            Err(node_store::InsertNodeError::CapReached) => {
                self.note_nodes_cap_reached(&path);
                false
            }
            Err(node_store::InsertNodeError::IdOverflow) => {
                log::error!(
                    "[DirectoryTree] Node arena id overflow at {}",
                    path.display()
                );
                false
            }
        }
    }

    pub(crate) fn initialize_places(&mut self, places: DirectoryTreePlaces) {
        self.generation = self.generation.wrapping_add(1);
        self.mark_snapshot_dirty();
        self.places_loaded = true;
        self.known_folders = places.known_folders;
        self.network_label = places.network_label;
        self.network_visible = false;
        self.nodes.clear();
        self.places_drive_roots = places
            .drives
            .iter()
            .map(|drive| drive.fs_path.clone())
            .collect();
        self.network_share_roots = places
            .network_locations
            .iter()
            .map(|entry| entry.fs_path.clone())
            .collect();

        let drive_namespace_paths: Vec<PathBuf> = places
            .drives
            .iter()
            .map(|drive| namespace::drive_mount_namespace_path(&drive.fs_path))
            .collect();
        self.insert_tree_node(
            this_pc_namespace_path(),
            DirectoryTreeNode {
                display_name: places.this_pc_label,
                fs_path: this_pc_namespace_path(),
                expanded: false,
                loading: false,
                children_loaded: true,
                children: drive_namespace_paths.clone(),
                error: None,
            },
        );

        for entry in self.known_folders.clone() {
            self.insert_tree_node(
                entry.namespace_path.clone(),
                DirectoryTreeNode {
                    display_name: entry.display_name.clone(),
                    fs_path: entry.fs_path.clone(),
                    expanded: false,
                    loading: false,
                    children_loaded: false,
                    children: Vec::new(),
                    error: None,
                },
            );
        }

        for drive in places.drives {
            let namespace_path = namespace::drive_mount_namespace_path(&drive.fs_path);
            self.or_insert_tree_node(namespace_path, || {
                directory_tree_node(drive.display_name, drive.fs_path)
            });
        }

        if !places.network_locations.is_empty() {
            let network_children: Vec<PathBuf> = places
                .network_locations
                .iter()
                .map(|entry| namespace::network_share_namespace_path(&entry.fs_path))
                .collect();
            self.network_visible = true;
            self.insert_tree_node(
                network_namespace_path(),
                DirectoryTreeNode {
                    display_name: self.network_label.clone(),
                    fs_path: network_namespace_path(),
                    expanded: false,
                    loading: false,
                    children_loaded: true,
                    children: network_children,
                    error: None,
                },
            );
            for entry in places.network_locations {
                let namespace_path = namespace::network_share_namespace_path(&entry.fs_path);
                self.or_insert_tree_node(namespace_path, || {
                    directory_tree_node(entry.display_name, entry.fs_path)
                });
            }
        }
    }

    pub(crate) fn ensure_network_visible(&mut self) {
        if self.network_visible {
            return;
        }
        self.network_visible = true;
        self.insert_tree_node(
            network_namespace_path(),
            DirectoryTreeNode {
                display_name: self.network_label.clone(),
                fs_path: network_namespace_path(),
                expanded: false,
                loading: false,
                children_loaded: true,
                children: Vec::new(),
                error: None,
            },
        );
    }

    fn known_folder_for_fs_path(&self, path: &Path) -> Option<&KnownFolderEntry> {
        self.known_folders
            .iter()
            .filter(|entry| path == entry.fs_path.as_path() || path.starts_with(&entry.fs_path))
            .max_by_key(|entry| entry.fs_path.components().count())
    }

    fn places_mount_root_for_path(&self, path: &Path) -> Option<PathBuf> {
        self.places_drive_roots
            .iter()
            .filter(|root| path == root.as_path() || path.starts_with(root.as_path()))
            .max_by_key(|root| root.components().count())
            .cloned()
    }

    fn fs_path_for_namespace_node(&self, tree: &Path) -> Option<PathBuf> {
        // This PC / Network are virtual sentinels; their namespace path doubles as a display anchor.
        if is_this_pc_namespace_path(tree) {
            return Some(this_pc_namespace_path());
        }
        if is_network_namespace_path(tree) {
            return Some(network_namespace_path());
        }
        if let Some(browse) = namespace::fs_path_for_known_folder_namespace(
            tree,
            self.known_folders
                .iter()
                .map(|entry| (entry.namespace_path.clone(), entry.fs_path.clone())),
        ) {
            return Some(browse);
        }
        if let Some(browse) =
            namespace::fs_path_for_mount_namespace(tree, self.places_drive_roots.iter().cloned())
        {
            return Some(browse);
        }
        if let Some(browse) = namespace::fs_path_for_network_share_namespace(
            tree,
            self.network_share_roots.iter().cloned(),
        ) {
            return Some(browse);
        }
        None
    }

    fn filter_children_for_places_mounts(&self, children: Vec<PathBuf>) -> Vec<PathBuf> {
        // Complements `filter_nested_mount_paths` in Places loading: when expanding a filesystem
        // parent (e.g. `/run/media`), omit mount roots that already appear as Places shortcuts
        // under This PC so the same drive is not listed twice in the tree.
        children
            .into_iter()
            .filter(|child| !self.places_drive_roots.contains(child))
            .collect()
    }

    fn dedupe_tree_children(children: &mut Vec<PathBuf>) {
        let mut seen = std::collections::HashSet::new();
        children.retain(|path| seen.insert(path.clone()));
    }

    pub(crate) fn expand_namespace_node(
        &mut self,
        namespace_path: &Path,
    ) -> Option<DirectoryChildrenRequest> {
        let node = self.nodes.get_mut(namespace_path)?;
        node.expanded = true;
        if node.children_loaded || node.loading {
            return None;
        }
        node.loading = true;
        node.error = None;
        let fs_path = node.fs_path.clone();
        Some(children_request(
            namespace_path.to_path_buf(),
            fs_path,
            self.generation,
        ))
    }

    pub(crate) fn expand_namespace_node_for_fs_path(
        &mut self,
        dir: &Path,
    ) -> Option<DirectoryChildrenRequest> {
        let namespace_path = self.namespace_path_for_fs_path(dir)?;
        self.expand_namespace_node(&namespace_path)
    }

    pub(crate) fn set_selected_fs_path(&mut self, dir: PathBuf) {
        if let Some(namespace_path) = self.namespace_path_for_fs_path(&dir) {
            self.set_selected_namespace_node(namespace_path, dir);
        } else {
            self.selected_fs_path = Some(dir);
            self.selected_namespace_path = None;
            self.mark_snapshot_dirty();
        }
    }

    /// Restore tree selection from persisted settings, keeping the saved namespace branch when valid.
    ///
    /// Before Places finishes loading, `fs_path_for_namespace_node` returns `None` and this falls
    /// back to heuristic `set_selected_fs_path`. Startup and Places apply paths call again after
    /// Places load via `reveal_selected_namespace` / `expand_requests_for_selection`.
    pub(crate) fn restore_tree_selection(
        &mut self,
        dir: PathBuf,
        saved_namespace: Option<PathBuf>,
    ) {
        let saved_namespace = saved_namespace.map(namespace::normalize_tree_namespace_path);
        let Some(namespace_path) = saved_namespace else {
            self.set_selected_fs_path(dir);
            return;
        };
        if !namespace::is_tree_namespace_path(&namespace_path) {
            self.set_selected_fs_path(dir);
            return;
        }
        let Some(resolved_fs) = self.fs_path_for_namespace_node(&namespace_path) else {
            self.set_selected_fs_path(dir);
            return;
        };
        if resolved_fs == dir {
            self.set_selected_namespace_node(namespace_path, dir);
            return;
        }
        self.set_selected_fs_path(dir);
    }

    pub(crate) fn folder_reveal_work_pending(&self) -> bool {
        self.scroll_folder_tree_to_selected
            || self.nodes.iter().any(|(_, node)| node.loading)
    }

    pub(crate) fn expand_requests_for_selection(
        &mut self,
        dir: &Path,
    ) -> Vec<DirectoryChildrenRequest> {
        let mut requests = self.reveal_selected_namespace();
        if let Some(namespace_path) = self.selected_namespace_path.clone() {
            if let Some(request) = self.expand_namespace_node(&namespace_path) {
                requests.push(request);
            }
        } else if let Some(request) = self.expand_namespace_node_for_fs_path(dir) {
            requests.push(request);
        }
        requests
    }

    pub(crate) fn set_selected_namespace_node(&mut self, namespace_path: PathBuf, dir: PathBuf) {
        let namespace_path = namespace::normalize_tree_namespace_path(namespace_path);
        if is_unc_path(&dir) {
            self.ensure_network_visible();
            if let Some(share_root) = unc_share_root(&dir) {
                self.ensure_network_share_mounted(&share_root);
            }
        }
        self.selected_fs_path = Some(dir.clone());
        self.selected_namespace_path = Some(namespace_path.clone());
        let display_name = self
            .known_folder_for_fs_path(&dir)
            .filter(|entry| entry.fs_path == dir)
            .map(|entry| entry.display_name.clone())
            .unwrap_or_else(|| directory_display_name(&dir));
        self.or_insert_tree_node(namespace_path, || directory_tree_node(display_name, dir));
        self.mark_snapshot_dirty();
    }

    fn resolve_selected_namespace_path(&self) -> Option<PathBuf> {
        if let Some(tree) = self.selected_namespace_path.clone() {
            return Some(tree);
        }
        self.selected_fs_path
            .as_ref()
            .and_then(|dir| self.namespace_path_for_fs_path(dir))
    }

    fn reveal_ancestor_chain_for_selection(&self) -> Vec<PathBuf> {
        let Some(tree) = self.resolve_selected_namespace_path() else {
            return Vec::new();
        };
        namespace::namespace_path_ancestor_chain(&tree)
    }

    /// Map a filesystem path to a namespace key when no persisted branch is available.
    ///
    /// Priority: known folder, UNC share, deepest Places mount root, then root `/` on Linux.
    /// Preferring the deepest mount avoids flattening mount subtrees under a parent filesystem
    /// expansion (see tests `reveal_mount_path_skips_root_slash_ancestor_chain`).
    fn namespace_path_for_fs_path(&self, dir: &Path) -> Option<PathBuf> {
        if let Some(entry) = self.known_folder_for_fs_path(dir) {
            let chain = namespace::namespace_ancestor_chain(
                &entry.namespace_path,
                &entry.fs_path,
                dir,
                MAX_DIRECTORY_TREE_REVEAL_DEPTH,
            );
            return chain.last().cloned().or(Some(entry.namespace_path.clone()));
        }
        if is_unc_path(dir) {
            if let Some(share) = unc_share_root(dir) {
                let share_tree = namespace::network_share_namespace_path(&share);
                let chain = namespace::namespace_ancestor_chain(
                    &share_tree,
                    &share,
                    dir,
                    MAX_DIRECTORY_TREE_REVEAL_DEPTH,
                );
                return chain.last().cloned().or(Some(share_tree));
            }
        }
        if let Some(mount_root) = self.places_mount_root_for_path(dir) {
            let mount_tree = namespace::drive_mount_namespace_path(&mount_root);
            let chain = namespace::namespace_ancestor_chain(
                &mount_tree,
                &mount_root,
                dir,
                MAX_DIRECTORY_TREE_REVEAL_DEPTH,
            );
            return chain.last().cloned().or(Some(mount_tree));
        }
        if self
            .places_drive_roots
            .iter()
            .any(|root| root.as_os_str() == OsStr::new(UNIX_FS_ROOT))
        {
            let root = PathBuf::from(UNIX_FS_ROOT);
            let root_tree = namespace::drive_mount_namespace_path(&root);
            let chain = namespace::namespace_ancestor_chain(
                &root_tree,
                &root,
                dir,
                MAX_DIRECTORY_TREE_REVEAL_DEPTH,
            );
            return chain.last().cloned().or(Some(root_tree));
        }
        None
    }

    pub(crate) fn reveal_selected_namespace(&mut self) -> Vec<DirectoryChildrenRequest> {
        let Some(selected_tree_key) = self.resolve_selected_namespace_path() else {
            return Vec::new();
        };
        if !self.places_loaded {
            return Vec::new();
        }

        let mut chain = self.reveal_ancestor_chain_for_selection();
        if chain.len() > MAX_DIRECTORY_TREE_REVEAL_DEPTH {
            chain.truncate(MAX_DIRECTORY_TREE_REVEAL_DEPTH);
        }
        let mut requests = Vec::new();

        if should_expand_this_pc_for_namespace_path(&selected_tree_key, &self.known_folders) {
            if let Some(node) = self.nodes.get_mut(&this_pc_namespace_path()) {
                node.expanded = true;
            }
        }

        let selected_browse = self.selected_fs_path.clone();
        if let Some(selected) = selected_browse.as_ref() {
            if is_unc_path(selected) {
                self.ensure_network_visible();
                if let Some(share_root) = unc_share_root(selected) {
                    self.ensure_network_share_mounted(&share_root);
                }
                if let Some(node) = self.nodes.get_mut(&network_namespace_path()) {
                    node.expanded = true;
                }
            }
        }

        for path in chain.iter().take(chain.len().saturating_sub(1)) {
            if is_places_sentinel_namespace_path(path) {
                continue;
            }
            let Some(browse) = self.fs_path_for_namespace_node(path) else {
                log::warn!(
                    "[DirectoryTree] Reveal skipped namespace node without browse path: {}",
                    path.display()
                );
                continue;
            };
            if !self.or_insert_tree_node(path.clone(), || {
                directory_tree_node(directory_display_name(&browse), browse)
            }) {
                continue;
            }
            let Some(node) = self.nodes.get_mut(path) else {
                continue;
            };
            node.expanded = true;
            if !node.children_loaded && !node.loading {
                node.loading = true;
                node.error = None;
                let fs_path = node.fs_path.clone();
                requests.push(children_request(path.clone(), fs_path, self.generation));
            }
        }
        let browse = self
            .fs_path_for_namespace_node(&selected_tree_key)
            .or_else(|| self.selected_fs_path.clone())
            .unwrap_or_else(|| selected_tree_key.clone());
        self.or_insert_tree_node(selected_tree_key.clone(), || {
            directory_tree_node(directory_display_name(&browse), browse)
        });
        self.link_revealed_selection_to_parents(&selected_tree_key, &chain);
        requests
    }

    fn is_known_folder_namespace_path(&self, path: &Path) -> bool {
        self.known_folders
            .iter()
            .any(|entry| entry.namespace_path.as_os_str() == path.as_os_str())
    }

    /// Ensure an expanded, loaded parent lists the selected namespace node for UI rendering.
    ///
    /// Only repairs the selected node's direct parent. Deeper ancestors are refreshed by reveal
    /// expand requests; extend to walk `chain` if intermediate links are missing in practice.
    fn link_revealed_selection_to_parents(
        &mut self,
        selected_tree_key: &Path,
        chain: &[PathBuf],
    ) {
        let selected_is_known_folder = self.is_known_folder_namespace_path(selected_tree_key);
        if chain.len() >= 2 {
            let parent_key = &chain[chain.len() - 2];
            if is_places_sentinel_namespace_path(parent_key) {
                return;
            }
            if is_this_pc_namespace_path(parent_key) && selected_is_known_folder {
                return;
            }
            let Some(parent_node) = self.nodes.get_mut(parent_key) else {
                return;
            };
            if parent_node.expanded
                && parent_node.children_loaded
                && !parent_node
                    .children
                    .iter()
                    .any(|child| child.as_os_str() == selected_tree_key.as_os_str())
            {
                parent_node
                    .children
                    .push(selected_tree_key.to_path_buf());
                Self::dedupe_tree_children(&mut parent_node.children);
                self.mark_snapshot_dirty();
            }
        }
    }

    pub(crate) fn ensure_network_share_mounted(&mut self, share_root: &Path) {
        self.ensure_network_visible();
        let share_path = share_root.to_path_buf();
        let share_tree = namespace::network_share_namespace_path(&share_path);
        self.network_share_roots.insert(share_path.clone());
        if let Some(network) = self.nodes.get_mut(&network_namespace_path()) {
            if !network
                .children
                .iter()
                .any(|child| child.as_os_str() == share_tree.as_os_str())
            {
                network.children.push(share_tree.clone());
                network.children.sort();
            }
        }
        self.or_insert_tree_node(share_tree, || {
            directory_tree_node(unc_share_display_name(&share_path), share_path.clone())
        });
    }

    pub(crate) fn mark_children_request_failed(&mut self, namespace_path: &Path, error: String) {
        let Some(node) = self.nodes.get_mut(namespace_path) else {
            return;
        };
        node.loading = false;
        node.error = Some(error);
        self.mark_snapshot_dirty();
    }
}

impl DirectoryTreeListState {
    pub(crate) fn sync_images(
        &mut self,
        images: &[PathBuf],
        sizes: &[u64],
        modified: &[Option<i64>],
        current_index: usize,
        scanning: bool,
        scan_status: String,
    ) -> Option<FileMetadataRequest> {
        let mut paths_needing_meta = Vec::new();
        let mut queue_metadata = |paths: Vec<PathBuf>| {
            if !paths.is_empty() {
                paths_needing_meta.extend(paths);
            }
        };
        if self.image_list_sort_active {
            let image_set: std::collections::HashSet<&PathBuf> = images.iter().collect();
            let image_index: std::collections::HashMap<&PathBuf, usize> = images
                .iter()
                .enumerate()
                .map(|(index, path)| (path, index))
                .collect();
            self.image_rows.retain(|row| image_set.contains(&row.path));
            for row in &mut self.image_rows {
                let Some(&index) = image_index.get(&row.path) else {
                    continue;
                };
                if let Some(size) = sizes.get(index) {
                    row.size_bytes = *size;
                }
                if let Some(mtime) = modified.get(index) {
                    row.modified_unix = *mtime;
                }
                row.refresh_display_cache();
            }
            // Owned paths: `image_rows.push` below may reallocate, invalidating borrows from rows.
            let existing_paths: std::collections::HashSet<&PathBuf> =
                self.image_rows.iter().map(|row| &row.path).collect();
            let mut new_rows = Vec::new();
            for (index, path) in images.iter().enumerate() {
                if existing_paths.contains(path) {
                    continue;
                }
                let mtime = modified.get(index).copied().flatten();
                if mtime.is_none() && !scanning {
                    paths_needing_meta.push(path.clone());
                }
                new_rows.push(DirectoryTreeFileRow::new(
                    path.clone(),
                    directory_display_name(path),
                    sizes.get(index).copied().unwrap_or(0),
                    mtime,
                ));
            }
            self.image_rows.extend(new_rows);
        } else {
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
                    row.refresh_display_cache();
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
                        self.image_rows.push(DirectoryTreeFileRow::new(
                            path.clone(),
                            directory_display_name(path),
                            sizes.get(index).copied().unwrap_or(0),
                            mtime,
                        ));
                    }
                    if !scanning {
                        queue_metadata(paths_needing_meta);
                    }
                }
            } else {
                self.image_rows = images
                    .iter()
                    .enumerate()
                    .map(|(index, path)| {
                        DirectoryTreeFileRow::new(
                            path.clone(),
                            directory_display_name(path),
                            sizes.get(index).copied().unwrap_or(0),
                            modified.get(index).copied().flatten(),
                        )
                    })
                    .collect();
                if !scanning {
                    queue_metadata(
                        self.image_rows
                            .iter()
                            .filter(|row| row.modified_unix.is_none())
                            .map(|row| row.path.clone())
                            .collect(),
                    );
                }
                self.image_list_scroll_offset_y = 0.0;
            }
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
        let metadata_request = if paths_needing_meta.is_empty() {
            None
        } else {
            self.file_metadata_generation = self.file_metadata_generation.wrapping_add(1);
            Some(FileMetadataRequest {
                generation: self.file_metadata_generation,
                paths: paths_needing_meta,
            })
        };
        self.mark_snapshot_dirty();
        metadata_request
    }

    pub(crate) fn update_image_list_column_widths(&mut self, ctx: &egui::Context) {
        // Fonts are created in `Context::begin_pass` (during `run`/`update`), so measuring
        // from `logic()` before the first UI pass panics ("No fonts available until...").
        if ctx.cumulative_frame_nr() == 0 {
            return;
        }
        let font_size = ctx.global_style().text_styles[&egui::TextStyle::Body].size;
        if !self.image_list_col_widths_dirty
            && (self.image_list_col_widths_font_size - font_size).abs() < f32::EPSILON
        {
            return;
        }
        let body_font = egui::FontId::proportional(font_size);
        let (size_w, modified_w) = ui::measure_image_list_content_column_widths(
            ctx,
            &body_font,
            &t!("directory_tree.col_size"),
            &t!("directory_tree.col_modified"),
        );
        self.image_list_col_size_w = size_w;
        self.image_list_col_modified_w = modified_w;
        self.image_list_col_widths_font_size = font_size;
        self.image_list_col_widths_dirty = false;
        self.mark_snapshot_dirty();
    }

    pub(crate) fn apply_metadata_result(&mut self, result: FileMetadataResult) {
        if result.generation != self.file_metadata_generation {
            return;
        }
        let mut changed = false;
        for (path, modified_unix) in result.paths.into_iter().zip(result.modified_unix) {
            if let Some(row) = self.image_rows.iter_mut().find(|row| row.path == path) {
                row.modified_unix = modified_unix;
                row.refresh_display_cache();
                changed = true;
            }
        }
        if changed {
            self.mark_snapshot_dirty();
        }
    }

    pub(crate) fn refresh_all_display_caches(&mut self) {
        for row in &mut self.image_rows {
            row.refresh_display_cache();
        }
    }
}

impl DirectoryTreeTreeState {
    pub(crate) fn toggle_expanded(&mut self, path: &Path) -> Option<DirectoryChildrenRequest> {
        let node = self.nodes.get_mut(path)?;
        node.expanded = !node.expanded;
        let request = if is_places_sentinel_namespace_path(path) {
            None
        } else if node.expanded && !node.children_loaded && !node.loading {
            node.loading = true;
            node.error = None;
            let fs_path = node.fs_path.clone();
            Some(children_request(
                path.to_path_buf(),
                fs_path,
                self.generation,
            ))
        } else {
            None
        };
        self.mark_snapshot_dirty();
        request
    }

    pub(crate) fn apply_children_result(&mut self, result: DirectoryChildrenResult) {
        if result.generation != self.generation {
            return;
        }

        let namespace_path = result.namespace_path;
        match result.result {
            Ok(children) => {
                let Some(parent_browse) = self
                    .nodes
                    .get(&namespace_path)
                    .map(|node| node.fs_path.clone())
                else {
                    return;
                };
                // Drop Places mount shortcuts from filesystem expansion children (see
                // `filter_children_for_places_mounts`).
                let children = self.filter_children_for_places_mounts(children);
                let mut cap_reached = false;
                let mut loaded_children = Vec::with_capacity(children.len());
                for child_browse in &children {
                    let child_tree = namespace::namespace_child_path(
                        &namespace_path,
                        &parent_browse,
                        child_browse,
                    );
                    match self.nodes.or_insert_with(
                        child_tree.clone(),
                        MAX_DIRECTORY_TREE_NODES,
                        || {
                            directory_tree_node(
                                directory_display_name(child_browse),
                                child_browse.clone(),
                            )
                        },
                    ) {
                        Ok(_) => loaded_children.push(child_tree),
                        Err(node_store::InsertNodeError::CapReached) => {
                            cap_reached = true;
                            break;
                        }
                        Err(node_store::InsertNodeError::IdOverflow) => {
                            log::error!("[DirectoryTree] Node arena id overflow loading children");
                            cap_reached = true;
                            break;
                        }
                    }
                }
                let Some(node) = self.nodes.get_mut(&namespace_path) else {
                    return;
                };
                node.loading = false;
                // Cap is a global arena limit; retry on re-expand would only re-hit the cap.
                node.children_loaded = true;
                node.children = loaded_children;
                Self::dedupe_tree_children(&mut node.children);
                node.error = if cap_reached {
                    Some(t!("directory_tree.nodes_cap_reached").to_string())
                } else {
                    None
                };
            }
            Err(err) => {
                let Some(node) = self.nodes.get_mut(&namespace_path) else {
                    return;
                };
                node.loading = false;
                node.children_loaded = true;
                node.children.clear();
                node.error = Some(err);
            }
        }
        self.mark_snapshot_dirty();
    }
}

mod app;
mod namespace;
mod node_store;
mod sort;
mod strip_previews;
mod ui;
mod view;
mod workers;

use ui::{
    directory_display_name, should_expand_this_pc_for_namespace_path, unc_share_display_name,
    unc_share_root,
};
use workers::{directory_tree_children_worker_loop, directory_tree_metadata_worker_loop};

#[cfg(test)]
mod tests;
