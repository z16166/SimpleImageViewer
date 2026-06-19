// Worker threads and filesystem reads for the directory tree.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
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
            let _ = children_result_tx.send(DirectoryChildrenResult {
                tree_path: request.tree_path,
                generation: request.generation,
                result,
            });
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
}

static READ_DIR_HELPERS_INFLIGHT: AtomicUsize = AtomicUsize::new(0);

fn read_child_directories_with_timeout(path: &Path) -> Result<Vec<PathBuf>, String> {
    if READ_DIR_HELPERS_INFLIGHT.load(AtomicOrdering::Relaxed) >= MAX_READ_DIR_HELPERS_INFLIGHT {
        log::warn!(
            "[DirectoryTree] read_dir helper cap ({MAX_READ_DIR_HELPERS_INFLIGHT}) reached; skipping {}",
            path.display()
        );
        return Err(t!("directory_tree.read_timeout").to_string());
    }

    let (tx, rx) = crossbeam_channel::bounded(1);
    let path_buf = path.to_path_buf();
    READ_DIR_HELPERS_INFLIGHT.fetch_add(1, AtomicOrdering::Relaxed);
    let helper_index = READ_DIR_HELPERS_INFLIGHT.load(AtomicOrdering::Relaxed);
    if std::thread::Builder::new()
        .name(format!("siv-dir-tree-read-dir-{helper_index}"))
        .spawn(move || {
            struct InflightGuard;
            impl Drop for InflightGuard {
                fn drop(&mut self) {
                    READ_DIR_HELPERS_INFLIGHT.fetch_sub(1, AtomicOrdering::Relaxed);
                }
            }
            let _guard = InflightGuard;
            let _ = tx.send(read_child_directories(&path_buf));
        })
        .is_err()
    {
        READ_DIR_HELPERS_INFLIGHT.fetch_sub(1, AtomicOrdering::Relaxed);
        return Err(t!("directory_tree.read_failed", err = "thread spawn failed").to_string());
    }
    match rx.recv_timeout(DIRECTORY_TREE_READ_DIR_TIMEOUT) {
        Ok(result) => result,
        Err(_) => {
            log::warn!(
                "[DirectoryTree] read_dir timed out after {}s: {}",
                DIRECTORY_TREE_READ_DIR_TIMEOUT.as_secs(),
                path.display()
            );
            Err(t!("directory_tree.read_timeout").to_string())
        }
    }
}

#[cfg(target_os = "windows")]
pub(super) fn strip_worker_com_initialized() -> bool {
    std::thread_local! {
        static STRIP_COM_OK: bool = crate::wic::ComGuard::new().is_ok();
    }
    STRIP_COM_OK.with(|ok| *ok)
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

pub(super) fn read_child_directories(path: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = std::fs::read_dir(path).map_err(|err| err.to_string())?;
    let mut dirs = Vec::new();
    for entry in entries.flatten() {
        let child = entry.path();
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let file_type = metadata.file_type();
        if !file_type.is_dir() {
            continue;
        }
        if crate::scanner::skip_directory_traversal_entry(&child, &file_type, Some(&metadata)) {
            continue;
        }
        if crate::scanner::is_non_browsable_system_directory(&child) {
            continue;
        }
        dirs.push(child);
    }
    dirs.sort();
    Ok(dirs)
}
