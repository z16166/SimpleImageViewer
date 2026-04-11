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

use std::path::{Path, PathBuf};
use crossbeam_channel::Sender;
use std::fs::Metadata;
use std::ffi::OsStr;

/// Lightweight check using only the file extension.
pub fn is_supported_extension(ext: &OsStr) -> bool {
    crate::formats::is_supported_extension(ext)
}

/// Helper to check if a file is marked as "offline" (Windows specific).
pub fn is_offline(path: &Path) -> bool {
    if let Ok(meta) = std::fs::metadata(path) {
        is_offline_meta(&meta)
    } else {
        false
    }
}

#[cfg(target_os = "windows")]
fn is_offline_meta(metadata: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    // Attributes indicating the file data is not fully present locally:
    const FILE_ATTRIBUTE_OFFLINE: u32 = 0x1000;
    const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x40000;
    const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x400000;

    const OFFLINE_MASK: u32 = FILE_ATTRIBUTE_OFFLINE 
        | FILE_ATTRIBUTE_RECALL_ON_OPEN 
        | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS;

    let attr = metadata.file_attributes();
    (attr & OFFLINE_MASK) != 0
}

#[cfg(not(target_os = "windows"))]
fn is_offline_meta(_metadata: &Metadata) -> bool {
    false
}

/// Returns true if the file is a legal local file (exists, is a file, size > 0, and not offline).
/// Note: This performs a syscall (metadata) and should be called after extension filtering.
fn is_valid_file(path: &Path) -> bool {
    if let Ok(meta) = std::fs::metadata(path) {
        // Checking size and file type first as they are fundamental.
        meta.len() > 0 && meta.is_file() && !is_offline_meta(&meta)
    } else {
        false
    }
}

/// Messages sent from the scan thread to the UI thread.
pub enum ScanMessage {
    /// An incremental batch of discovered files (already filtered, not yet globally sorted).
    Batch(Vec<PathBuf>),
    /// Scanning is complete. No more batches will follow.
    Done,
}

/// Number of files to accumulate before sending a batch to the UI.
const BATCH_SIZE: usize = 200;

pub fn scan_directory(dir: PathBuf, recursive: bool, tx: Sender<ScanMessage>) {
    std::thread::spawn(move || {
        if recursive {
            let mut batch = Vec::with_capacity(BATCH_SIZE);

            for entry in jwalk::WalkDir::new(&dir)
                .follow_links(false)
                .into_iter()
                .flatten()
            {
                // 1. [Cheapest] Check file_type from directory entry (no syscall on most OSs)
                if entry.file_type().is_file() {
                    // 2. [Cheap] Check extension without constructing full PathBuf
                    let is_img = Path::new(entry.file_name()).extension()
                        .map(|ext| is_supported_extension(ext))
                        .unwrap_or(false);

                    if is_img {
                        // 3. [Expensive] Syscall (metadata) and Path construction only for candidates
                        let path = entry.path();
                        if is_valid_file(&path) {
                            batch.push(path);
                            if batch.len() >= BATCH_SIZE {
                                batch.sort();
                                let _ = tx.send(ScanMessage::Batch(std::mem::take(&mut batch)));
                                batch.reserve(BATCH_SIZE);
                            }
                        }
                    }
                }
            }

            if !batch.is_empty() {
                batch.sort();
                let _ = tx.send(ScanMessage::Batch(batch));
            }
        } else if let Ok(entries) = std::fs::read_dir(&dir) {
            let mut files: Vec<PathBuf> = entries
                .flatten()
                .filter(|e| {
                    // Use the same tiered filtering pattern
                    Path::new(&e.file_name()).extension().map(|ext| is_supported_extension(ext)).unwrap_or(false) 
                        && is_valid_file(&e.path())
                })
                .map(|e| e.path())
                .collect();
            files.sort();
            if !files.is_empty() {
                let _ = tx.send(ScanMessage::Batch(files));
            }
        }

        let _ = tx.send(ScanMessage::Done);
    });
}
