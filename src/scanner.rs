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

use crossbeam_channel::Sender;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

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

/// Returns the file size when the path is a valid local regular file (>0 bytes, not offline).
/// This performs an `metadata` syscall and should only run after cheap extension filtering.
fn validated_byte_len_if_file(path: &Path) -> Option<u64> {
    let meta = std::fs::metadata(path).ok()?;
    let len = meta.len();
    if len == 0 || !meta.is_file() || is_offline_meta(&meta) {
        return None;
    }
    Some(len)
}

/// Messages sent from the scan thread to the UI thread.
pub enum ScanMessage {
    /// An incremental batch of discovered files (already filtered, not yet globally sorted).
    /// The `u64` is the byte length from the same [`std::fs::metadata`] probe used during scan,
    /// so the UI thread can budget preloads without additional syscalls.
    Batch(Vec<(PathBuf, u64)>),
    /// Scanning is complete. No more batches will follow.
    Done,
}

/// Number of files to accumulate before sending a batch to the UI.
const BATCH_SIZE: usize = 200;

pub fn scan_directory(
    dir: PathBuf,
    recursive: bool,
    skip_raw_if_jpeg_exists: bool,
    tx: Sender<ScanMessage>,
    cancel: Arc<AtomicBool>,
) {
    std::thread::spawn(move || {
        if recursive {
            let mut files: Vec<(PathBuf, u64)> = Vec::new();

            for entry in jwalk::WalkDir::new(&dir)
                .follow_links(false)
                .into_iter()
                .flatten()
            {
                if cancel.load(Ordering::Relaxed) {
                    log::info!("[Scanner] Scan cancelled for {:?}", dir);
                    return;
                }

                // 1. [Cheapest] Check file_type from directory entry (no syscall on most OSs)
                if entry.file_type().is_file() {
                    // 2. [Cheap] Check extension without constructing full PathBuf
                    let is_img = Path::new(entry.file_name())
                        .extension()
                        .map(|ext| is_supported_extension(ext))
                        .unwrap_or(false);

                    if is_img {
                        // 3. [Expensive] Syscall (metadata) and Path construction only for candidates
                        let path = entry.path();
                        if let Some(len) = validated_byte_len_if_file(&path) {
                            files.push((path, len));
                        }
                    }
                }
            }

            send_scanned_files(files, skip_raw_if_jpeg_exists, &tx);
        } else if let Ok(entries) = std::fs::read_dir(&dir) {
            let mut files: Vec<(PathBuf, u64)> = Vec::new();
            for e in entries.flatten() {
                if cancel.load(Ordering::Relaxed) {
                    log::info!("[Scanner] Scan (non-recursive) cancelled for {:?}", dir);
                    return;
                }

                // Use the same tiered filtering pattern
                let is_supported = Path::new(&e.file_name())
                    .extension()
                    .map(|ext| is_supported_extension(ext))
                    .unwrap_or(false);

                if is_supported {
                    let p = e.path();
                    if let Some(len) = validated_byte_len_if_file(&p) {
                        files.push((p, len));
                    }
                }
            }
            send_scanned_files(files, skip_raw_if_jpeg_exists, &tx);
        }

        let _ = tx.send(ScanMessage::Done);
    });
}

fn send_scanned_files(
    mut files: Vec<(PathBuf, u64)>,
    skip_raw_if_jpeg_exists: bool,
    tx: &Sender<ScanMessage>,
) {
    if skip_raw_if_jpeg_exists {
        filter_raw_files_with_matching_jpeg(&mut files);
    }

    files.sort_by(|a, b| a.0.cmp(&b.0));
    for batch in files.chunks(BATCH_SIZE) {
        let _ = tx.send(ScanMessage::Batch(batch.to_vec()));
    }
}

fn filter_raw_files_with_matching_jpeg(files: &mut Vec<(PathBuf, u64)>) {
    let jpeg_stems: HashSet<PathBuf> = files
        .iter()
        .filter_map(|(path, _)| jpeg_stem_key(path))
        .collect();

    files.retain(|(path, _)| {
        !is_raw_path(path)
            || raw_stem_key(path).is_none_or(|stem_key| !jpeg_stems.contains(&stem_key))
    });
}

fn jpeg_stem_key(path: &Path) -> Option<PathBuf> {
    let ext = path.extension()?.to_str()?;
    if !ext.eq_ignore_ascii_case("jpg") && !ext.eq_ignore_ascii_case("jpeg") {
        return None;
    }
    stem_key(path)
}

fn raw_stem_key(path: &Path) -> Option<PathBuf> {
    is_raw_path(path).then(|| stem_key(path)).flatten()
}

fn is_raw_path(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(crate::raw_processor::is_raw_extension)
}

fn stem_key(path: &Path) -> Option<PathBuf> {
    let mut key = path.parent().map(Path::to_path_buf).unwrap_or_default();
    key.push(path.file_stem()?);
    Some(key)
}

#[cfg(test)]
mod tests {
    use super::{ScanMessage, scan_directory};
    use crossbeam_channel::unbounded;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempScanDir {
        path: PathBuf,
    }

    impl TempScanDir {
        fn new() -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "simple_image_viewer_scanner_test_{}_{}",
                std::process::id(),
                unique
            ));
            std::fs::create_dir(&path).expect("create temp scan directory");
            Self { path }
        }

        fn touch(&self, name: &str) {
            std::fs::write(self.path.join(name), b"image").expect("write test file");
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempScanDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn scan_names(dir: &Path, skip_raw_if_jpeg_exists: bool) -> Vec<String> {
        let (tx, rx) = unbounded();
        scan_directory(
            dir.to_path_buf(),
            false,
            skip_raw_if_jpeg_exists,
            tx,
            Arc::new(AtomicBool::new(false)),
        );

        let mut files = Vec::new();
        loop {
            match rx.recv().expect("scan message") {
                ScanMessage::Batch(batch) => {
                    files.extend(batch.into_iter().map(|(path, _)| {
                        path.file_name()
                            .expect("file name")
                            .to_string_lossy()
                            .into_owned()
                    }));
                }
                ScanMessage::Done => break,
            }
        }
        files.sort();
        files
    }

    #[test]
    fn skips_raw_files_when_matching_jpeg_exists() {
        let dir = TempScanDir::new();
        dir.touch("DSC08268.ARW");
        dir.touch("DSC08268.JPG");
        dir.touch("DSC08269.arw");
        dir.touch("DSC08269.JPEG");
        dir.touch("RAW_ONLY.ARW");
        dir.touch("JPEG_ONLY.JPG");

        assert_eq!(
            scan_names(dir.path(), true),
            vec![
                "DSC08268.JPG",
                "DSC08269.JPEG",
                "JPEG_ONLY.JPG",
                "RAW_ONLY.ARW",
            ]
        );
    }

    #[test]
    fn keeps_paired_raw_files_when_filter_is_disabled() {
        let dir = TempScanDir::new();
        dir.touch("DSC08268.ARW");
        dir.touch("DSC08268.JPG");

        assert_eq!(
            scan_names(dir.path(), false),
            vec!["DSC08268.ARW", "DSC08268.JPG"]
        );
    }
}
