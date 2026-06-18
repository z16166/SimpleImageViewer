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

use crossbeam_channel::{Receiver, Sender};
use eframe::egui;
use parking_lot::Mutex;
use rust_i18n::t;

use crate::app::ImageViewerApp;
use crate::settings::BrowseMode;
use crate::ui::osd::{format_file_modified, format_file_size};

const DIRECTORY_TREE_VIEWPORT_ID: &str = "siv_directory_tree_viewport";
const DIRECTORY_TREE_MIN_WIDTH: f32 = 640.0;
const DIRECTORY_TREE_MIN_HEIGHT: f32 = 420.0;
const DIRECTORY_TREE_DEFAULT_WIDTH: f32 = 820.0;
const DIRECTORY_TREE_DEFAULT_HEIGHT: f32 = 560.0;
const DIRECTORY_TREE_LEFT_WIDTH: f32 = 280.0;
const DIRECTORY_TREE_LEFT_MIN_WIDTH: f32 = 200.0;
const DIRECTORY_TREE_RIGHT_MIN_WIDTH: f32 = 180.0;
const DIRECTORY_TREE_SPLITTER_GRAB_WIDTH: f32 = 10.0;
const DIRECTORY_TREE_LEFT_MAX_WIDTH_RATIO: f32 = 0.55;
const DIRECTORY_TREE_EXPAND_ICON_WIDTH: f32 = 18.0;
const DIRECTORY_TREE_FOLDER_ICON_WIDTH: f32 = 18.0;
const DIRECTORY_TREE_ROW_HEIGHT: f32 = 24.0;
const DIRECTORY_TREE_HEADER_HEIGHT: f32 = 22.0;
const DIRECTORY_TREE_COL_SIZE_WIDTH: f32 = 88.0;
const DIRECTORY_TREE_COL_MODIFIED_WIDTH: f32 = 172.0;
const DIRECTORY_TREE_INDENT: f32 = 16.0;

#[derive(Debug)]
pub(crate) enum DirectoryTreeCommand {
    SelectDirectory(PathBuf),
    ToggleExpanded(PathBuf),
    SelectImage(usize),
    CloseWindow,
}

#[derive(Debug)]
pub(crate) struct DirectoryChildrenRequest {
    path: PathBuf,
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
    path: PathBuf,
    generation: u64,
    result: Result<Vec<PathBuf>, String>,
}

#[derive(Debug, Clone)]
pub(crate) struct DirectoryTreeNode {
    display_name: String,
    expanded: bool,
    loading: bool,
    children_loaded: bool,
    children: Vec<PathBuf>,
    error: Option<String>,
}

#[derive(Debug)]
pub(crate) struct DirectoryTreeState {
    root: Option<PathBuf>,
    selected_dir: Option<PathBuf>,
    nodes: HashMap<PathBuf, DirectoryTreeNode>,
    generation: u64,
    image_list_generation: u64,
    image_rows: Vec<DirectoryTreeFileRow>,
    current_index: usize,
    scanning: bool,
    scan_status: String,
    left_panel_width: f32,
    scroll_image_list_to_current: bool,
    scroll_folder_to_selected: bool,
    image_list_scroll_offset_y: f32,
    image_list_keyboard_active: bool,
}

impl Default for DirectoryTreeState {
    fn default() -> Self {
        Self {
            root: None,
            selected_dir: None,
            nodes: HashMap::new(),
            generation: 0,
            image_list_generation: 0,
            image_rows: Vec::new(),
            current_index: 0,
            scanning: false,
            scan_status: String::new(),
            left_panel_width: DIRECTORY_TREE_LEFT_WIDTH,
            scroll_image_list_to_current: false,
            scroll_folder_to_selected: false,
            image_list_scroll_offset_y: 0.0,
            image_list_keyboard_active: false,
        }
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
    pub(crate) fn set_root(&mut self, root: PathBuf) {
        if self.root.as_ref() == Some(&root) {
            return;
        }

        self.generation = self.generation.wrapping_add(1);
        self.root = Some(root.clone());
        self.selected_dir = Some(root.clone());
        self.nodes.clear();
        self.nodes.insert(
            root.clone(),
            DirectoryTreeNode {
                display_name: directory_display_name(&root),
                expanded: true,
                loading: false,
                children_loaded: false,
                children: Vec::new(),
                error: None,
            },
        );
    }

    pub(crate) fn set_selected_dir(&mut self, dir: PathBuf) {
        self.selected_dir = Some(dir.clone());
        self.nodes
            .entry(dir.clone())
            .or_insert_with(|| DirectoryTreeNode {
                display_name: directory_display_name(&dir),
                expanded: false,
                loading: false,
                children_loaded: false,
                children: Vec::new(),
                error: None,
            });
        self.scroll_folder_to_selected = true;
    }

    pub(crate) fn reveal_selected_dir(&mut self) -> Vec<DirectoryChildrenRequest> {
        let Some(root) = self.root.clone() else {
            return Vec::new();
        };
        let Some(selected) = self.selected_dir.clone() else {
            return Vec::new();
        };

        let chain = directory_ancestor_chain(&root, &selected);
        let mut requests = Vec::new();
        for path in chain.iter().take(chain.len().saturating_sub(1)) {
            self.nodes
                .entry(path.clone())
                .or_insert_with(|| DirectoryTreeNode {
                    display_name: directory_display_name(path),
                    expanded: false,
                    loading: false,
                    children_loaded: false,
                    children: Vec::new(),
                    error: None,
                });
            if let Some(node) = self.nodes.get_mut(path) {
                node.expanded = true;
                if !node.children_loaded && !node.loading {
                    node.loading = true;
                    node.error = None;
                    requests.push(DirectoryChildrenRequest {
                        path: path.clone(),
                        generation: self.generation,
                    });
                }
            }
        }
        self.nodes
            .entry(selected.clone())
            .or_insert_with(|| DirectoryTreeNode {
                display_name: directory_display_name(&selected),
                expanded: false,
                loading: false,
                children_loaded: false,
                children: Vec::new(),
                error: None,
            });
        requests
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
        if node.expanded && !node.children_loaded && !node.loading {
            node.loading = true;
            node.error = None;
            Some(DirectoryChildrenRequest {
                path: path.to_path_buf(),
                generation: self.generation,
            })
        } else {
            None
        }
    }

    fn request_children_if_needed(&mut self, path: &Path) -> Option<DirectoryChildrenRequest> {
        let node = self.nodes.get_mut(path)?;
        if node.children_loaded || node.loading {
            return None;
        }
        node.loading = true;
        node.error = None;
        Some(DirectoryChildrenRequest {
            path: path.to_path_buf(),
            generation: self.generation,
        })
    }

    fn apply_children_result(&mut self, result: DirectoryChildrenResult) {
        if result.generation != self.generation {
            return;
        }

        let Some(node) = self.nodes.get_mut(&result.path) else {
            return;
        };

        node.loading = false;
        node.children_loaded = true;
        match result.result {
            Ok(children) => {
                node.children = children.clone();
                node.error = None;
                for child in children {
                    self.nodes
                        .entry(child.clone())
                        .or_insert_with(|| DirectoryTreeNode {
                            display_name: directory_display_name(&child),
                            expanded: false,
                            loading: false,
                            children_loaded: false,
                            children: Vec::new(),
                            error: None,
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

    pub(crate) fn initialize_directory_tree_root(&mut self, root: PathBuf) {
        self.settings.browse_mode = BrowseMode::Tree;
        self.settings.show_directory_tree_nav = true;
        self.settings.tree_nav_root_dir = Some(root.clone());
        self.settings.tree_nav_selected_dir = Some(root.clone());
        self.settings.last_image_dir = Some(root.clone());

        let runtime = &self.directory_tree;
        let request = {
            let mut state = runtime.state.lock();
            state.set_root(root.clone());
            state.request_children_if_needed(&root)
        };
        if let Some(request) = request {
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
            let viewport_id = egui::ViewportId::from_hash_of(DIRECTORY_TREE_VIEWPORT_ID);
            ctx.request_repaint_of(viewport_id);
        }

        while let Ok(result) = self.directory_tree.metadata_result_rx.try_recv() {
            self.directory_tree
                .state
                .lock()
                .apply_metadata_result(result);
            ctx.request_repaint();
            let viewport_id = egui::ViewportId::from_hash_of(DIRECTORY_TREE_VIEWPORT_ID);
            ctx.request_repaint_of(viewport_id);
        }

        while let Ok(command) = self.directory_tree.command_rx.try_recv() {
            match command {
                DirectoryTreeCommand::SelectDirectory(path) => {
                    self.settings.browse_mode = BrowseMode::Tree;
                    self.settings.show_directory_tree_nav = true;
                    self.settings.tree_nav_selected_dir = Some(path.clone());
                    {
                        let mut state = self.directory_tree.state.lock();
                        state.set_selected_dir(path.clone());
                        state.image_list_keyboard_active = false;
                    }
                    self.load_directory(path);
                    self.queue_save();
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
                    if index < self.image_files.len() {
                        self.navigate_to(index, ctx);
                        let mut state = self.directory_tree.state.lock();
                        state.current_index = index;
                        ctx.request_repaint();
                        let viewport_id =
                            egui::ViewportId::from_hash_of(DIRECTORY_TREE_VIEWPORT_ID);
                        ctx.request_repaint_of(viewport_id);
                    }
                }
                DirectoryTreeCommand::CloseWindow => {
                    self.settings.show_directory_tree_nav = false;
                    self.queue_save();
                    ctx.request_repaint();
                }
            }
        }
    }

    pub(crate) fn draw_directory_tree_viewport(&mut self, ctx: &egui::Context) {
        if self.settings.browse_mode != BrowseMode::Tree
            || !self.settings.show_directory_tree_nav
            || self.directory_tree.state.lock().root.is_none()
        {
            return;
        }

        {
            let mut state = self.directory_tree.state.lock();
            state.sync_images(
                &self.image_files,
                &self.file_byte_len_by_index,
                &self.file_modified_unix_by_index,
                self.current_index,
                self.scanning,
                self.status_message.clone(),
                &self.directory_tree.metadata_request_tx,
            );
        }

        let state = Arc::clone(&self.directory_tree.state);
        let command_tx = self.directory_tree.command_tx.clone();
        let viewport_id = egui::ViewportId::from_hash_of(DIRECTORY_TREE_VIEWPORT_ID);
        let builder = egui::ViewportBuilder::default()
            .with_title(t!("directory_tree.title").to_string())
            .with_inner_size([DIRECTORY_TREE_DEFAULT_WIDTH, DIRECTORY_TREE_DEFAULT_HEIGHT])
            .with_min_inner_size([DIRECTORY_TREE_MIN_WIDTH, DIRECTORY_TREE_MIN_HEIGHT])
            .with_resizable(true)
            .with_close_button(true);

        ctx.show_viewport_deferred(viewport_id, builder, move |ui, _class| {
            if ui.ctx().input(|i| i.viewport().close_requested()) {
                let _ = command_tx.send(DirectoryTreeCommand::CloseWindow);
                return;
            }

            let mut state = state.lock();
            draw_directory_tree_window(ui, &mut state, &command_tx);
        });
    }
}

fn directory_tree_children_worker_loop(
    request_rx: Receiver<DirectoryChildrenRequest>,
    children_result_tx: Sender<DirectoryChildrenResult>,
) {
    while let Ok(request) = request_rx.recv() {
        let result = read_child_directories(&request.path);
        let _ = children_result_tx.send(DirectoryChildrenResult {
            path: request.path,
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
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() || file_type.is_symlink() {
            continue;
        }
        let child = entry.path();
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

fn draw_directory_tree_window(
    ui: &mut egui::Ui,
    state: &mut DirectoryTreeState,
    command_tx: &Sender<DirectoryTreeCommand>,
) {
    ui.visuals_mut().button_frame = false;

    let viewport_height = ui.available_height();
    let viewport_width = ui.available_width();
    let max_left_width = (viewport_width * DIRECTORY_TREE_LEFT_MAX_WIDTH_RATIO)
        .max(DIRECTORY_TREE_LEFT_MIN_WIDTH)
        .min(viewport_width - DIRECTORY_TREE_SPLITTER_GRAB_WIDTH - DIRECTORY_TREE_RIGHT_MIN_WIDTH);
    state.left_panel_width = state
        .left_panel_width
        .clamp(DIRECTORY_TREE_LEFT_MIN_WIDTH, max_left_width);

    let left_w = state.left_panel_width;
    let splitter_w = DIRECTORY_TREE_SPLITTER_GRAB_WIDTH;
    let right_w = (viewport_width - left_w - splitter_w).max(DIRECTORY_TREE_RIGHT_MIN_WIDTH);

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

    ui.allocate_exact_size(
        egui::vec2(viewport_width, viewport_height),
        egui::Sense::hover(),
    );

    ui.allocate_ui_at_rect(left_rect, |ui| {
        ui.set_clip_rect(left_rect);
        ui.set_width(left_w);
        draw_folder_panel(ui, state, command_tx);
    });

    ui.allocate_ui_at_rect(right_rect, |ui| {
        ui.set_clip_rect(right_rect);
        ui.set_width(right_w);
        draw_image_file_list(ui, state, command_tx);
    });

    let splitter_id = ui.id().with("directory_tree_splitter");
    let splitter_response = ui.interact(splitter_rect, splitter_id, egui::Sense::drag());
    if splitter_response.dragged() {
        state.left_panel_width = (state.left_panel_width + splitter_response.drag_delta().x)
            .clamp(DIRECTORY_TREE_LEFT_MIN_WIDTH, max_left_width);
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
}

fn draw_folder_panel(
    ui: &mut egui::Ui,
    state: &mut DirectoryTreeState,
    command_tx: &Sender<DirectoryTreeCommand>,
) {
    let scroll_to_selected = state.scroll_folder_to_selected;
    directory_tree_scroll_area("directory_tree_folders", ui, |ui| {
        if let Some(root) = state.root.clone() {
            let scrolled = draw_directory_node(ui, state, command_tx, &root, 0, scroll_to_selected);
            if scrolled {
                state.scroll_folder_to_selected = false;
            }
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

            let selected = state.selected_dir.as_deref() == Some(path);
            let name_response = ui
                .selectable_label(selected, node.display_name.as_str())
                .on_hover_text(path.to_string_lossy());
            if scroll_to_selected && selected {
                name_response.scroll_to_me(Some(egui::Align::Center));
                scrolled = true;
            }
            if name_response.clicked() {
                let _ = command_tx.send(DirectoryTreeCommand::SelectDirectory(path.to_path_buf()));
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
            scrolled |=
                draw_directory_node(ui, state, command_tx, &child, depth + 1, scroll_to_selected);
        }
    }

    scrolled
}

fn draw_image_file_list(
    ui: &mut egui::Ui,
    state: &mut DirectoryTreeState,
    command_tx: &Sender<DirectoryTreeCommand>,
) {
    let panel_rect = ui.max_rect();
    let list_focus_id = ui.id().with("directory_tree_image_list");
    let list_enabled = !state.scanning;
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

    let status_height = if state.scanning {
        DIRECTORY_TREE_ROW_HEIGHT
    } else {
        0.0
    };
    let row_height = DIRECTORY_TREE_ROW_HEIGHT;
    let row_spacing = ui.spacing().item_spacing.y;
    let row_height_with_spacing = row_height + row_spacing;
    let col_size_w = DIRECTORY_TREE_COL_SIZE_WIDTH;
    let col_modified_w = DIRECTORY_TREE_COL_MODIFIED_WIDTH;

    draw_image_details_header(ui, col_size_w, col_modified_w);

    let viewport_height = (ui.available_height() - status_height).max(row_height_with_spacing);

    try_handle_image_list_arrow_keys(ui, state, list_focus_id, command_tx);

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

    ui.add_enabled_ui(list_enabled, |ui| {
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
            for row_index in row_range {
                let Some(row) = state.image_rows.get(row_index) else {
                    continue;
                };
                let clicked = draw_image_details_row(
                    ui,
                    row,
                    row_index,
                    row_index == current_index,
                    col_size_w,
                    col_modified_w,
                    command_tx,
                    list_enabled,
                );
                if clicked {
                    ui.memory_mut(|mem| mem.request_focus(list_focus_id));
                    state.image_list_keyboard_active = true;
                }
            }
        });
        state.image_list_scroll_offset_y = scroll_output.state.offset.y;
    });

    try_handle_image_list_arrow_keys(ui, state, list_focus_id, command_tx);

    if state.scanning {
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

fn draw_image_details_header(ui: &mut egui::Ui, col_size_w: f32, col_modified_w: f32) {
    let header_width = ui.available_width();
    ui.allocate_ui_with_layout(
        egui::vec2(header_width, DIRECTORY_TREE_HEADER_HEIGHT),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_min_width(header_width);
            ui.label(
                egui::RichText::new(t!("directory_tree.col_name").to_string())
                    .strong()
                    .weak(),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_sized(
                    [col_modified_w, DIRECTORY_TREE_HEADER_HEIGHT],
                    egui::Label::new(
                        egui::RichText::new(t!("directory_tree.col_modified").to_string())
                            .strong()
                            .weak(),
                    ),
                );
                ui.add_sized(
                    [col_size_w, DIRECTORY_TREE_HEADER_HEIGHT],
                    egui::Label::new(
                        egui::RichText::new(t!("directory_tree.col_size").to_string())
                            .strong()
                            .weak(),
                    )
                    .halign(egui::Align::RIGHT),
                );
            });
        },
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

fn try_handle_image_list_arrow_keys(
    ui: &mut egui::Ui,
    state: &mut DirectoryTreeState,
    list_focus_id: egui::Id,
    command_tx: &Sender<DirectoryTreeCommand>,
) {
    let list_has_focus = ui.memory(|mem| mem.has_focus(list_focus_id));
    if !(state.image_list_keyboard_active || list_has_focus)
        || state.image_rows.is_empty()
        || state.scanning
    {
        return;
    }

    let current = state.current_index;
    let last = state.image_rows.len().saturating_sub(1);
    let mut next = None;
    ui.input(|input| {
        if input.key_pressed(egui::Key::ArrowDown) {
            next = Some((current + 1).min(last));
        } else if input.key_pressed(egui::Key::ArrowUp) {
            next = Some(current.saturating_sub(1));
        }
    });
    let Some(index) = next.filter(|&index| index != current) else {
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
    _col_size_w: f32,
    col_modified_w: f32,
    command_tx: &Sender<DirectoryTreeCommand>,
    list_enabled: bool,
) -> bool {
    let row_width = ui.available_width();
    let (row_rect, response) = ui.allocate_exact_size(
        egui::vec2(row_width, DIRECTORY_TREE_ROW_HEIGHT),
        egui::Sense::click(),
    );
    if ui.is_rect_visible(row_rect) {
        if selected {
            ui.painter()
                .rect_filled(row_rect, 0.0, ui.visuals().selection.bg_fill);
        } else if response.hovered() {
            ui.painter()
                .rect_filled(row_rect, 0.0, ui.visuals().widgets.hovered.weak_bg_fill);
        }

        let text_color = if selected {
            ui.visuals().selection.stroke.color
        } else {
            ui.visuals().text_color()
        };
        let body_font =
            egui::FontId::proportional(ui.style().text_styles[&egui::TextStyle::Body].size);
        let size_text = format_file_size(row.size_bytes);
        let modified_text = row
            .modified_unix
            .map(format_file_modified)
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| String::from("-"));

        let name_galley =
            ui.painter()
                .layout_no_wrap(row.name.clone(), body_font.clone(), text_color);
        ui.painter().galley(
            egui::pos2(
                row_rect.left() + ui.spacing().item_spacing.x,
                row_rect.center().y - name_galley.size().y * 0.5,
            ),
            name_galley,
            text_color,
        );

        let size_col_right = row_rect.right() - col_modified_w - ui.spacing().item_spacing.x;
        let size_galley = ui
            .painter()
            .layout_no_wrap(size_text, body_font.clone(), text_color);
        ui.painter().galley(
            egui::pos2(
                size_col_right - size_galley.size().x,
                row_rect.center().y - size_galley.size().y * 0.5,
            ),
            size_galley,
            text_color,
        );

        let modified_galley = ui
            .painter()
            .layout_no_wrap(modified_text, body_font, text_color);
        ui.painter().galley(
            egui::pos2(
                row_rect.right() - ui.spacing().item_spacing.x - modified_galley.size().x,
                row_rect.center().y - modified_galley.size().y * 0.5,
            ),
            modified_galley,
            text_color,
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
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
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
            root: Some(root.clone()),
            selected_dir: Some(root.clone()),
            generation: 2,
            ..DirectoryTreeState::default()
        };
        state.nodes.insert(
            root.clone(),
            DirectoryTreeNode {
                display_name: "root".to_string(),
                expanded: true,
                loading: true,
                children_loaded: false,
                children: Vec::new(),
                error: None,
            },
        );

        state.apply_children_result(DirectoryChildrenResult {
            path: root.clone(),
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
            root: Some(root.clone()),
            selected_dir: Some(root.clone()),
            generation: 1,
            ..DirectoryTreeState::default()
        };
        state.nodes.insert(
            root.clone(),
            DirectoryTreeNode {
                display_name: "root".to_string(),
                expanded: true,
                loading: true,
                children_loaded: false,
                children: Vec::new(),
                error: None,
            },
        );

        state.apply_children_result(DirectoryChildrenResult {
            path: root.clone(),
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
            root: Some(root.clone()),
            selected_dir: Some(root.clone()),
            generation: 1,
            ..DirectoryTreeState::default()
        };
        state.nodes.insert(
            root.clone(),
            DirectoryTreeNode {
                display_name: "root".to_string(),
                expanded: true,
                loading: true,
                children_loaded: false,
                children: Vec::new(),
                error: None,
            },
        );

        state.apply_children_result(DirectoryChildrenResult {
            path: root.clone(),
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
            min_scroll_offset_to_show_row(8, 30.0, 24.0, 260.0, 0.0),
            Some(4.0)
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
    fn set_root_resets_nodes_and_bumps_generation() {
        let first = PathBuf::from("/tmp/siv-dir-tree-first");
        let second = PathBuf::from("/tmp/siv-dir-tree-second");

        let mut state = DirectoryTreeState::default();
        state.set_root(first.clone());
        assert_eq!(state.generation, 1);
        assert_eq!(state.root.as_deref(), Some(first.as_path()));
        assert_eq!(state.selected_dir.as_deref(), Some(first.as_path()));
        assert_eq!(state.nodes.len(), 1);

        state.nodes.insert(
            PathBuf::from("/tmp/siv-dir-tree-stale"),
            DirectoryTreeNode {
                display_name: "stale".to_string(),
                expanded: false,
                loading: false,
                children_loaded: true,
                children: Vec::new(),
                error: None,
            },
        );

        state.set_root(second.clone());
        assert_eq!(state.generation, 2);
        assert_eq!(state.root.as_deref(), Some(second.as_path()));
        assert_eq!(state.nodes.len(), 1);
        assert!(state.nodes.contains_key(&second));
    }
}
