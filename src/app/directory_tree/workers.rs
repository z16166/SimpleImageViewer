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

// Worker threads and filesystem reads for the directory tree.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use rust_i18n::t;

use super::{
    DirectoryChildrenRequest, DirectoryChildrenResult, FileMetadataRequest, FileMetadataResult,
};

pub(super) const METADATA_BATCH_SIZE: usize = 200;
pub(super) const DIRECTORY_TREE_READ_DIR_TIMEOUT: Duration = Duration::from_secs(30);
pub(super) const MAX_READ_DIR_HELPERS_INFLIGHT: usize = 4;

fn coalesce_children_requests(
    request: DirectoryChildrenRequest,
    request_rx: &Receiver<DirectoryChildrenRequest>,
) -> Vec<DirectoryChildrenRequest> {
    let mut by_path = HashMap::new();
    by_path.insert(request.tree_path.clone(), request);
    while let Ok(next) = request_rx.try_recv() {
        by_path.insert(next.tree_path.clone(), next);
    }
    by_path.into_values().collect()
}

fn coalesce_metadata_requests(
    request: FileMetadataRequest,
    request_rx: &Receiver<FileMetadataRequest>,
) -> Vec<FileMetadataRequest> {
    let mut by_generation = HashMap::new();
    by_generation.insert(request.generation, request);
    while let Ok(next) = request_rx.try_recv() {
        match by_generation.get_mut(&next.generation) {
            Some(existing) => existing.paths.extend(next.paths),
            None => {
                by_generation.insert(next.generation, next);
            }
        }
    }
    by_generation.into_values().collect()
}

pub(super) fn directory_tree_children_worker_loop(
    request_rx: Receiver<DirectoryChildrenRequest>,
    children_result_tx: Sender<DirectoryChildrenResult>,
) {
    while let Ok(request) = request_rx.recv() {
        for request in coalesce_children_requests(request, &request_rx) {
            let result = read_child_directories_with_timeout(&request.browse_path);
            if children_result_tx
                .send(DirectoryChildrenResult {
                    tree_path: request.tree_path,
                    generation: request.generation,
                    result,
                })
                .is_err()
            {
                log::warn!("[DirectoryTree] children result channel disconnected");
            }
        }
    }
}

pub(super) fn directory_tree_metadata_worker_loop(
    request_rx: Receiver<FileMetadataRequest>,
    metadata_result_tx: Sender<FileMetadataResult>,
) {
    while let Ok(request) = request_rx.recv() {
        for request in coalesce_metadata_requests(request, &request_rx) {
            let mut batch_paths = Vec::with_capacity(METADATA_BATCH_SIZE);
            let mut batch_modified = Vec::with_capacity(METADATA_BATCH_SIZE);

            for path in request.paths {
                batch_paths.push(path.clone());
                batch_modified.push(read_file_modified_unix(&path));

                if batch_paths.len() >= METADATA_BATCH_SIZE {
                    if metadata_result_tx
                        .send(FileMetadataResult {
                            generation: request.generation,
                            paths: batch_paths.split_off(0),
                            modified_unix: batch_modified.split_off(0),
                        })
                        .is_err()
                    {
                        log::warn!("[DirectoryTree] metadata result channel disconnected");
                        return;
                    }
                }
            }

            if !batch_paths.is_empty()
                && metadata_result_tx
                    .send(FileMetadataResult {
                        generation: request.generation,
                        paths: batch_paths,
                        modified_unix: batch_modified,
                    })
                    .is_err()
            {
                log::warn!("[DirectoryTree] metadata result channel disconnected");
            }
        }
    }
}

static READ_DIR_HELPERS_INFLIGHT: AtomicUsize = AtomicUsize::new(0);

struct InflightGuard {
    orphan_flag: Arc<AtomicBool>,
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        if !self.orphan_flag.load(AtomicOrdering::Relaxed) {
            READ_DIR_HELPERS_INFLIGHT.fetch_sub(1, AtomicOrdering::Relaxed);
        }
    }
}

fn read_child_directories_with_timeout(path: &Path) -> Result<Vec<PathBuf>, String> {
    if READ_DIR_HELPERS_INFLIGHT.load(AtomicOrdering::Relaxed) >= MAX_READ_DIR_HELPERS_INFLIGHT {
        log::warn!(
            "[DirectoryTree] read_dir helper cap ({MAX_READ_DIR_HELPERS_INFLIGHT}) reached; skipping {}",
            path.display()
        );
        return Err(t!("directory_tree.read_busy").to_string());
    }

    let (tx, rx) = crossbeam_channel::bounded(1);
    let path_buf = path.to_path_buf();
    READ_DIR_HELPERS_INFLIGHT.fetch_add(1, AtomicOrdering::Relaxed);
    let helper_index = READ_DIR_HELPERS_INFLIGHT.load(AtomicOrdering::Relaxed);
    // Orphan threads cannot be cancelled on all platforms; the flag only recycles the
    // inflight cap so new reads can proceed while a timed-out helper keeps running.
    let orphan_flag = Arc::new(AtomicBool::new(false));
    let orphan_for_thread = Arc::clone(&orphan_flag);
    if std::thread::Builder::new()
        .name(format!("siv-dir-tree-read-dir-{helper_index}"))
        .spawn(move || {
            let _guard = InflightGuard {
                orphan_flag: orphan_for_thread,
            };
            let _ = tx.send(read_child_directories(&path_buf));
        })
        .is_err()
    {
        READ_DIR_HELPERS_INFLIGHT.fetch_sub(1, AtomicOrdering::Relaxed);
        return Err(t!("directory_tree.read_failed", err = t!("directory_tree.thread_spawn_failed")).to_string());
    }
    match rx.recv_timeout(DIRECTORY_TREE_READ_DIR_TIMEOUT) {
        Ok(result) => result,
        Err(_) => {
            orphan_flag.store(true, AtomicOrdering::Relaxed);
            READ_DIR_HELPERS_INFLIGHT.fetch_sub(1, AtomicOrdering::Relaxed);
            log::warn!(
                "[DirectoryTree] read_dir timed out after {}s: {}",
                DIRECTORY_TREE_READ_DIR_TIMEOUT.as_secs(),
                path.display()
            );
            Err(t!("directory_tree.read_timeout").to_string())
        }
    }
}

pub(super) fn read_child_directories(path: &Path) -> Result<Vec<PathBuf>, String> {
    let mut children = Vec::new();
    let entries = std::fs::read_dir(path).map_err(|err| {
        t!(
            "directory_tree.read_failed",
            err = err.to_string()
        )
        .to_string()
    })?;

    for entry in entries.flatten() {
        let child_path = entry.path();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_dir() || meta.file_type().is_symlink() {
            continue;
        }
        if crate::scanner::skip_directory_traversal_entry(
            &child_path,
            &meta.file_type(),
            Some(&meta),
        ) {
            continue;
        }
        children.push(child_path);
    }

    children.sort();
    Ok(children)
}

fn read_file_modified_unix(path: &Path) -> Option<i64> {
    use std::time::UNIX_EPOCH;
    std::fs::metadata(path)
        .ok()
        .and_then(|meta| meta.modified().ok())
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs() as i64)
}

#[cfg(target_os = "windows")]
pub(super) fn strip_worker_com_initialized() -> bool {
    use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
    use windows::core::HRESULT;

    const RPC_E_CHANGED_MODE: HRESULT = HRESULT(0x8001_0106_u32 as i32);
    unsafe {
        let hr = CoInitializeEx(None, COINIT_MULTITHREADED);
        hr.is_ok() || hr == RPC_E_CHANGED_MODE
    }
}

#[cfg(not(target_os = "windows"))]
pub(super) fn strip_worker_com_initialized() -> bool {
    true
}
