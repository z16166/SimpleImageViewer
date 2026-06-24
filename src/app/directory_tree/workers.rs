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

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering as AtomicOrdering};
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TrySendError};
use parking_lot::Mutex;
use rust_i18n::t;

use super::{
    DirectoryChildrenRequest, DirectoryChildrenResult, FileMetadataRequest, FileMetadataResult,
};

pub(super) const METADATA_BATCH_SIZE: usize = 200;
pub(super) const DIRECTORY_TREE_READ_DIR_TIMEOUT: Duration = Duration::from_secs(30);
pub(super) const MAX_READ_DIR_HELPERS_INFLIGHT: usize = 4;
pub(super) const DIRECTORY_TREE_WORKER_POLL_INTERVAL: Duration = Duration::from_millis(200);
pub(super) const DIRECTORY_TREE_RESULT_SEND_TIMEOUT: Duration = Duration::from_secs(5);

fn send_worker_result<T>(tx: &Sender<T>, mut msg: T, shutdown: &AtomicBool) -> bool {
    let deadline = std::time::Instant::now() + DIRECTORY_TREE_RESULT_SEND_TIMEOUT;
    loop {
        match tx.try_send(msg) {
            Ok(()) => return true,
            Err(TrySendError::Full(pending)) => {
                if shutdown.load(AtomicOrdering::Acquire) {
                    log::debug!("[DirectoryTree] Dropping worker result: shutting down");
                    return false;
                }
                if std::time::Instant::now() >= deadline {
                    log::warn!("[DirectoryTree] Dropping worker result: result channel full");
                    return false;
                }
                msg = pending;
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(TrySendError::Disconnected(_)) => return false,
        }
    }
}

/// Paths with a helper thread still inside OS `read_dir` (including timed-out orphans).
static READ_DIR_INFLIGHT_PATHS: LazyLock<Mutex<HashSet<PathBuf>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));
static READ_DIR_HELPER_THREAD_ID: AtomicU64 = AtomicU64::new(0);

pub(super) fn coalesce_children_requests(
    request: DirectoryChildrenRequest,
    request_rx: &Receiver<DirectoryChildrenRequest>,
) -> Vec<DirectoryChildrenRequest> {
    let mut by_path = HashMap::new();
    by_path.insert(request.namespace_path.clone(), request);
    while let Ok(next) = request_rx.try_recv() {
        by_path.insert(next.namespace_path.clone(), next);
    }
    by_path.into_values().collect()
}

pub(super) fn coalesce_metadata_requests(
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

pub(super) fn split_metadata_request(request: FileMetadataRequest) -> Vec<FileMetadataRequest> {
    if request.paths.len() <= METADATA_BATCH_SIZE {
        return vec![request];
    }
    request
        .paths
        .chunks(METADATA_BATCH_SIZE)
        .map(|chunk| FileMetadataRequest {
            generation: request.generation,
            paths: chunk.to_vec(),
        })
        .collect()
}

pub(super) fn directory_tree_children_worker_loop(
    request_rx: Receiver<DirectoryChildrenRequest>,
    children_result_tx: Sender<DirectoryChildrenResult>,
    shutdown: Arc<AtomicBool>,
) {
    while !shutdown.load(AtomicOrdering::Acquire) {
        match request_rx.recv_timeout(DIRECTORY_TREE_WORKER_POLL_INTERVAL) {
            Ok(request) => {
                for request in coalesce_children_requests(request, &request_rx) {
                    if shutdown.load(AtomicOrdering::Acquire) {
                        return;
                    }
                    let result = read_child_directories_with_timeout(&request.fs_path);
                    if !send_worker_result(
                        &children_result_tx,
                        DirectoryChildrenResult {
                            namespace_path: request.namespace_path,
                            generation: request.generation,
                            result,
                        },
                        &shutdown,
                    ) {
                        return;
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

pub(super) fn directory_tree_metadata_worker_loop(
    request_rx: Receiver<FileMetadataRequest>,
    metadata_result_tx: Sender<FileMetadataResult>,
    shutdown: Arc<AtomicBool>,
) {
    while !shutdown.load(AtomicOrdering::Acquire) {
        match request_rx.recv_timeout(DIRECTORY_TREE_WORKER_POLL_INTERVAL) {
            Ok(request) => {
                for request in coalesce_metadata_requests(request, &request_rx) {
                    for request in split_metadata_request(request) {
                        if shutdown.load(AtomicOrdering::Acquire) {
                            return;
                        }
                        let mut batch_paths = Vec::with_capacity(METADATA_BATCH_SIZE);
                        let mut batch_modified = Vec::with_capacity(METADATA_BATCH_SIZE);

                        for path in request.paths {
                            batch_paths.push(path.clone());
                            batch_modified.push(read_file_modified_unix(&path));

                            if batch_paths.len() >= METADATA_BATCH_SIZE {
                                if !send_worker_result(
                                    &metadata_result_tx,
                                    FileMetadataResult {
                                        generation: request.generation,
                                        paths: std::mem::take(&mut batch_paths),
                                        modified_unix: std::mem::take(&mut batch_modified),
                                    },
                                    &shutdown,
                                ) {
                                    return;
                                }
                            }
                        }

                        if !batch_paths.is_empty()
                            && !send_worker_result(
                                &metadata_result_tx,
                                FileMetadataResult {
                                    generation: request.generation,
                                    paths: batch_paths,
                                    modified_unix: batch_modified,
                                },
                                &shutdown,
                            )
                        {
                            return;
                        }
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

static READ_DIR_HELPERS_INFLIGHT: AtomicUsize = AtomicUsize::new(0);

struct InflightGuard {
    slot_freed: Arc<AtomicBool>,
}

struct ReadDirPathGuard(PathBuf);

impl Drop for ReadDirPathGuard {
    fn drop(&mut self) {
        READ_DIR_INFLIGHT_PATHS.lock().remove(&self.0);
    }
}

fn release_read_dir_helper_slot(slot_freed: &AtomicBool) {
    if !slot_freed.swap(true, AtomicOrdering::SeqCst) {
        READ_DIR_HELPERS_INFLIGHT.fetch_sub(1, AtomicOrdering::Release);
    }
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        release_read_dir_helper_slot(&self.slot_freed);
    }
}

fn read_child_directories_with_timeout(path: &Path) -> Result<Vec<PathBuf>, String> {
    if READ_DIR_INFLIGHT_PATHS.lock().contains(path) {
        return Err(t!("directory_tree.read_busy").to_string());
    }
    loop {
        let current = READ_DIR_HELPERS_INFLIGHT.load(AtomicOrdering::Acquire);
        if current >= MAX_READ_DIR_HELPERS_INFLIGHT {
            log::warn!(
                "[DirectoryTree] read_dir helper cap ({MAX_READ_DIR_HELPERS_INFLIGHT}) reached; skipping {}",
                path.display()
            );
            return Err(t!("directory_tree.read_busy").to_string());
        }
        if READ_DIR_HELPERS_INFLIGHT
            .compare_exchange(
                current,
                current + 1,
                AtomicOrdering::AcqRel,
                AtomicOrdering::Acquire,
            )
            .is_ok()
        {
            break;
        }
    }

    let (tx, rx) = crossbeam_channel::bounded(1);
    let path_buf = path.to_path_buf();
    let helper_id = READ_DIR_HELPER_THREAD_ID.fetch_add(1, AtomicOrdering::Relaxed);
    // Orphan threads cannot be cancelled on all platforms; `slot_freed` recycles the inflight
    // cap exactly once whether the helper or the timeout path wins the race.
    // `READ_DIR_INFLIGHT_PATHS` blocks duplicate reads for the same path until the helper exits.
    let slot_freed = Arc::new(AtomicBool::new(false));
    let slot_freed_for_thread = Arc::clone(&slot_freed);
    if std::thread::Builder::new()
        .name(format!("siv-dir-tree-read-dir-{helper_id}"))
        .spawn(move || {
            let _path_guard = ReadDirPathGuard(path_buf.clone());
            let _guard = InflightGuard {
                slot_freed: slot_freed_for_thread,
            };
            if let Err(err) = tx.send(read_child_directories(&path_buf)) {
                log::warn!("[DirectoryTree] read_dir orphan helper failed to send result: {err}");
            }
        })
        .is_err()
    {
        release_read_dir_helper_slot(&slot_freed);
        return Err(t!(
            "directory_tree.read_failed",
            err = t!("directory_tree.thread_spawn_failed")
        )
        .to_string());
    }
    match rx.recv_timeout(DIRECTORY_TREE_READ_DIR_TIMEOUT) {
        Ok(result) => result,
        Err(RecvTimeoutError::Timeout) => {
            if let Ok(result) = rx.try_recv() {
                return result;
            }
            release_read_dir_helper_slot(&slot_freed);
            log::warn!(
                "[DirectoryTree] read_dir timed out after {}s: {}",
                DIRECTORY_TREE_READ_DIR_TIMEOUT.as_secs(),
                path.display()
            );
            Err(t!("directory_tree.read_timeout").to_string())
        }
        Err(RecvTimeoutError::Disconnected) => Err(t!(
            "directory_tree.read_failed",
            err = t!("directory_tree.thread_spawn_failed")
        )
        .to_string()),
    }
}

pub(super) fn read_child_directories(path: &Path) -> Result<Vec<PathBuf>, String> {
    let mut children = Vec::new();
    let entries = std::fs::read_dir(path)
        .map_err(|err| t!("directory_tree.read_failed", err = err.to_string()).to_string())?;

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
    // UTC seconds, matching scanner.rs `validated_metadata` and DirectoryTreeFileRow.
    std::fs::metadata(path)
        .ok()
        .and_then(|meta| meta.modified().ok())
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs() as i64)
}

#[cfg(target_os = "windows")]
pub(super) fn ensure_strip_worker_com_initialized() -> bool {
    use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
    use windows::core::HRESULT;

    const RPC_E_CHANGED_MODE: HRESULT = HRESULT(0x8001_0106_u32 as i32);
    unsafe {
        let hr = CoInitializeEx(None, COINIT_MULTITHREADED);
        hr.is_ok() || hr == RPC_E_CHANGED_MODE
    }
}
