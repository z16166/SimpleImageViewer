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
use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::settings::PairedRawJpegHandling;

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
    paired_raw_jpeg_handling: PairedRawJpegHandling,
    tx: Sender<ScanMessage>,
    cancel: Arc<AtomicBool>,
) {
    std::thread::spawn(move || {
        if recursive {
            let mut files: Vec<(PathBuf, u64)> = Vec::new();
            let mut batch: Vec<(PathBuf, u64)> = Vec::with_capacity(BATCH_SIZE);

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
                            if paired_raw_jpeg_handling.needs_pair_index() {
                                files.push((path, len));
                            } else {
                                batch.push((path, len));
                                if batch.len() >= BATCH_SIZE {
                                    send_scan_batch(&mut batch, &tx);
                                }
                            }
                        }
                    }
                }
            }

            if paired_raw_jpeg_handling.needs_pair_index() {
                // RAW/JPEG pairing needs a complete same-directory stem index. Sending recursive
                // batches before the scan finishes could expose a file that should be skipped
                // because its pair appears later.
                send_scanned_files(files, paired_raw_jpeg_handling, &tx);
            } else {
                send_scan_batch(&mut batch, &tx);
            }
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
            send_scanned_files(files, paired_raw_jpeg_handling, &tx);
        }

        let _ = tx.send(ScanMessage::Done);
    });
}

fn send_scan_batch(batch: &mut Vec<(PathBuf, u64)>, tx: &Sender<ScanMessage>) {
    if batch.is_empty() {
        return;
    }
    batch.sort_by(|a, b| a.0.cmp(&b.0));
    let _ = tx.send(ScanMessage::Batch(std::mem::take(batch)));
    batch.reserve(BATCH_SIZE);
}

fn send_scanned_files(
    mut files: Vec<(PathBuf, u64)>,
    paired_raw_jpeg_handling: PairedRawJpegHandling,
    tx: &Sender<ScanMessage>,
) {
    if paired_raw_jpeg_handling.needs_pair_index() {
        filter_raw_jpeg_pairs(&mut files, paired_raw_jpeg_handling);
    }

    // Keep global ordering stable across batches. This holds all paths until the scan finishes,
    // which costs more memory than sorting per batch, but per-batch sorting is not globally sorted.
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut batch = Vec::with_capacity(BATCH_SIZE);
    for file in files {
        batch.push(file);
        if batch.len() >= BATCH_SIZE {
            let _ = tx.send(ScanMessage::Batch(std::mem::take(&mut batch)));
            batch.reserve(BATCH_SIZE);
        }
    }
    if !batch.is_empty() {
        let _ = tx.send(ScanMessage::Batch(batch));
    }
}

fn filter_raw_jpeg_pairs(files: &mut Vec<(PathBuf, u64)>, handling: PairedRawJpegHandling) {
    match handling {
        PairedRawJpegHandling::ShowBoth => {}
        PairedRawJpegHandling::SkipRaw => {
            let jpeg_stems = jpeg_stems_by_parent(files);
            files.retain(|(path, _)| !is_raw_path(path) || !has_matching_stem(path, &jpeg_stems));
        }
        PairedRawJpegHandling::SkipJpeg => {
            let raw_stems = raw_stems_by_parent(files);
            files.retain(|(path, _)| !is_jpeg_path(path) || !has_matching_stem(path, &raw_stems));
        }
    }
}

fn jpeg_stems_by_parent(files: &[(PathBuf, u64)]) -> HashMap<PathBuf, HashSet<OsString>> {
    let mut by_parent: HashMap<PathBuf, HashSet<OsString>> = HashMap::new();
    for key in files.iter().filter_map(|(path, _)| jpeg_stem_key(path)) {
        by_parent
            .entry(key.parent)
            .or_default()
            .insert(key.stem_lower);
    }
    by_parent
}

fn jpeg_stem_key(path: &Path) -> Option<StemKey> {
    if !is_jpeg_path(path) {
        return None;
    }
    owned_stem_key(path)
}

fn raw_stems_by_parent(files: &[(PathBuf, u64)]) -> HashMap<PathBuf, HashSet<OsString>> {
    let mut by_parent: HashMap<PathBuf, HashSet<OsString>> = HashMap::new();
    for key in files.iter().filter_map(|(path, _)| raw_stem_key(path)) {
        by_parent
            .entry(key.parent)
            .or_default()
            .insert(key.stem_lower);
    }
    by_parent
}

fn raw_stem_key(path: &Path) -> Option<StemKey> {
    if !is_raw_path(path) {
        return None;
    }
    owned_stem_key(path)
}

fn has_matching_stem(path: &Path, stems: &HashMap<PathBuf, HashSet<OsString>>) -> bool {
    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    let Some(stem_lower) = path.file_stem().map(OsStr::to_ascii_lowercase) else {
        return false;
    };
    stems
        .get(parent)
        .is_some_and(|stems| stems.contains(&stem_lower))
}

fn is_jpeg_path(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|ext| ext.eq_ignore_ascii_case("jpg") || ext.eq_ignore_ascii_case("jpeg"))
}

fn is_raw_path(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(crate::raw_processor::is_raw_extension)
}

#[derive(Debug, Eq, Hash, PartialEq)]
struct StemKey {
    parent: PathBuf,
    stem_lower: OsString,
}

fn owned_stem_key(path: &Path) -> Option<StemKey> {
    Some(StemKey {
        // Include the parent directory so recursive scans only pair files from the same folder.
        parent: path.parent().map(Path::to_path_buf).unwrap_or_default(),
        // Camera sidecars commonly differ only by ASCII case (e.g. IMG001.ARW/img001.JPG).
        stem_lower: path.file_stem()?.to_ascii_lowercase(),
    })
}

#[cfg(test)]
mod tests {
    use super::{ScanMessage, scan_directory};
    use crate::settings::PairedRawJpegHandling;
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
            for attempt in 0..100 {
                let unique = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("system time")
                    .as_nanos();
                let path = std::env::temp_dir().join(format!(
                    "simple_image_viewer_scanner_test_{}_{}_{}",
                    std::process::id(),
                    unique,
                    attempt
                ));
                if std::fs::create_dir(&path).is_ok() {
                    return Self { path };
                }
            }
            panic!("create temp scan directory");
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

    fn scan_names(dir: &Path, paired_raw_jpeg_handling: PairedRawJpegHandling) -> Vec<String> {
        let (tx, rx) = unbounded();
        scan_directory(
            dir.to_path_buf(),
            false,
            paired_raw_jpeg_handling,
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
    fn skips_raw_files_when_matching_jpeg_stem_differs_by_case() {
        let dir = TempScanDir::new();
        dir.touch("IMG001.ARW");
        dir.touch("img001.JPG");

        assert_eq!(
            scan_names(dir.path(), PairedRawJpegHandling::SkipRaw),
            vec!["img001.JPG"]
        );
    }

    #[test]
    fn paired_raw_jpeg_handling_can_skip_raw_files() {
        let dir = TempScanDir::new();
        dir.touch("DSC08268.ARW");
        dir.touch("DSC08268.JPG");
        dir.touch("DSC08269.arw");
        dir.touch("DSC08269.JPEG");
        dir.touch("RAW_ONLY.ARW");
        dir.touch("JPEG_ONLY.JPG");

        assert_eq!(
            scan_names(dir.path(), PairedRawJpegHandling::SkipRaw),
            vec![
                "DSC08268.JPG",
                "DSC08269.JPEG",
                "JPEG_ONLY.JPG",
                "RAW_ONLY.ARW",
            ]
        );
    }

    #[test]
    fn paired_raw_jpeg_handling_can_skip_jpeg_files() {
        let dir = TempScanDir::new();
        dir.touch("DSC08268.ARW");
        dir.touch("DSC08268.JPG");
        dir.touch("DSC08269.arw");
        dir.touch("DSC08269.JPEG");
        dir.touch("RAW_ONLY.ARW");
        dir.touch("JPEG_ONLY.JPG");

        assert_eq!(
            scan_names(dir.path(), PairedRawJpegHandling::SkipJpeg),
            vec![
                "DSC08268.ARW",
                "DSC08269.arw",
                "JPEG_ONLY.JPG",
                "RAW_ONLY.ARW"
            ]
        );
    }

    #[test]
    fn paired_raw_jpeg_handling_keeps_both_by_default() {
        let dir = TempScanDir::new();
        dir.touch("DSC08268.ARW");
        dir.touch("DSC08268.JPG");

        assert_eq!(
            scan_names(dir.path(), PairedRawJpegHandling::ShowBoth),
            vec!["DSC08268.ARW", "DSC08268.JPG"]
        );
    }
}
