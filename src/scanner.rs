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
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        is_offline_meta(&meta)
    } else {
        false
    }
}

/// Returns true when a directory must not be descended into during scans.
///
/// Uses [`std::fs::symlink_metadata`] fields only (no [`std::fs::read_link`]):
/// symlinks via [`std::fs::FileType::is_symlink`], Windows junctions via
/// [`FILE_ATTRIBUTE_REPARSE_POINT`](https://learn.microsoft.com/en-us/windows/win32/fileio/file-attribute-constants).
pub fn is_directory_traversal_boundary_metadata(metadata: &Metadata) -> bool {
    let file_type = metadata.file_type();
    if !file_type.is_dir() {
        return false;
    }
    if file_type.is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;

        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    false
}

/// Skip descending into a directory using [`DirEntry::file_type`] when possible.
/// On Windows, pass directory [`Metadata`] from the same [`read_dir`] entry to detect junctions.
pub fn skip_directory_traversal_entry(
    file_type: &std::fs::FileType,
    metadata: Option<&Metadata>,
) -> bool {
    if !file_type.is_dir() {
        return file_type.is_symlink();
    }
    if file_type.is_symlink() {
        return true;
    }
    metadata.is_some_and(is_directory_traversal_boundary_metadata)
}

#[cfg(target_os = "windows")]
fn is_offline_meta(metadata: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    // OneDrive / cloud placeholder files: detect via attribute flags only (no read_link,
    // no reparse-tag IO). See FILE_ATTRIBUTE_OFFLINE and recall-on-access bits.
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

/// Returns the file size and modified time for a regular local file metadata probe.
fn validated_metadata(metadata: &Metadata) -> Option<(u64, Option<i64>)> {
    let len = metadata.len();
    if len == 0 || !metadata.is_file() || is_offline_meta(metadata) {
        return None;
    }
    use std::time::UNIX_EPOCH;
    let modified_unix = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs() as i64);
    Some((len, modified_unix))
}

/// Messages sent from the scan thread to the UI thread.
pub enum ScanMessage {
    /// An incremental batch of discovered files (already filtered, not yet globally sorted).
    /// Tuple is `(path, byte length, modified unix seconds)` from the same [`std::fs::metadata`]
    /// probe used during scan, so the UI thread can budget preloads without additional syscalls.
    Batch {
        generation: u64,
        files: Vec<(PathBuf, u64, Option<i64>)>,
    },
    /// Scanning is complete. No more batches will follow.
    Done { generation: u64 },
}

/// Number of files to accumulate before sending a batch to the UI.
const BATCH_SIZE: usize = 200;

pub fn scan_directory(
    dir: PathBuf,
    recursive: bool,
    paired_raw_jpeg_handling: PairedRawJpegHandling,
    generation: u64,
    tx: Sender<ScanMessage>,
    cancel: Arc<AtomicBool>,
    wake_ui: Option<Arc<dyn Fn() + Send + Sync>>,
) {
    std::thread::spawn(move || {
        #[cfg(feature = "preload-debug")]
        let scan_started = std::time::Instant::now();
        crate::preload_debug!(
            "[PreloadDebug][Scan] thread start: dir={} recursive={} paired={:?} needs_pair_index={}",
            dir.display(),
            recursive,
            paired_raw_jpeg_handling,
            paired_raw_jpeg_handling.needs_pair_index()
        );
        if recursive {
            let mut files: Vec<(PathBuf, u64, Option<i64>)> = Vec::new();
            let mut batch: Vec<(PathBuf, u64, Option<i64>)> = Vec::with_capacity(BATCH_SIZE);
            #[cfg(feature = "preload-debug")]
            let mut walk_entries = 0usize;
            #[cfg(feature = "preload-debug")]
            let mut walk_files = 0usize;
            #[cfg(feature = "preload-debug")]
            let mut walk_ext_probes = 0usize;
            #[cfg(feature = "preload-debug")]
            let mut walk_images = 0usize;

            for entry in jwalk::WalkDir::new(&dir)
                .follow_links(false)
                .process_read_dir(|_depth, _path, _state, children| {
                    for child in children.iter_mut() {
                        if let Ok(entry) = child {
                            if !entry.file_type.is_dir() {
                                continue;
                            }
                            let skip = if entry.file_type.is_symlink() {
                                true
                            } else {
                                #[cfg(windows)]
                                {
                                    entry.metadata().ok().is_some_and(|meta| {
                                        is_directory_traversal_boundary_metadata(&meta)
                                    })
                                }
                                #[cfg(not(windows))]
                                {
                                    false
                                }
                            };
                            if skip {
                                entry.read_children_path = None;
                            }
                        }
                    }
                })
                .into_iter()
                .flatten()
            {
                #[cfg(feature = "preload-debug")]
                {
                    walk_entries += 1;
                }
                if cancel.load(Ordering::Relaxed) {
                    log::info!("[Scanner] Scan cancelled for {:?}", dir);
                    return;
                }

                // 1. [Cheapest] Check file_type from directory entry (no syscall on most OSs)
                if entry.file_type().is_file() {
                    #[cfg(feature = "preload-debug")]
                    {
                        walk_files += 1;
                    }
                    // 2. [Cheap] Check extension without constructing full PathBuf
                    let is_img = Path::new(entry.file_name())
                        .extension()
                        .map(|ext| {
                            #[cfg(feature = "preload-debug")]
                            {
                                walk_ext_probes += 1;
                            }
                            is_supported_extension(ext)
                        })
                        .unwrap_or(false);

                    if is_img {
                        #[cfg(feature = "preload-debug")]
                        {
                            walk_images += 1;
                        }
                        // 3. [Expensive] Syscall (metadata) and Path construction only for candidates
                        let path = entry.path();
                        let Ok(meta) = entry.metadata() else {
                            continue;
                        };
                        if let Some((len, modified_unix)) = validated_metadata(&meta) {
                            if paired_raw_jpeg_handling.needs_pair_index() {
                                files.push((path, len, modified_unix));
                            } else {
                                batch.push((path, len, modified_unix));
                                if batch.len() >= BATCH_SIZE {
                                    send_scan_batch(&mut batch, generation, &tx, wake_ui.as_ref());
                                }
                            }
                        }
                    }
                }
            }

            #[cfg(feature = "preload-debug")]
            crate::preload_debug!(
                "[PreloadDebug][Scan] recursive walk done: entries={} files={} ext_probes={} images={} walk_ms={}",
                walk_entries,
                walk_files,
                walk_ext_probes,
                walk_images,
                crate::preload_debug::elapsed_ms(scan_started)
            );

            if paired_raw_jpeg_handling.needs_pair_index() {
                // RAW/JPEG pairing needs a complete same-directory stem index. Sending recursive
                // batches before the scan finishes could expose a file that should be skipped
                // because its pair appears later.
                send_scanned_files(
                    files,
                    paired_raw_jpeg_handling,
                    generation,
                    &tx,
                    wake_ui.as_ref(),
                );
            } else {
                send_scan_batch(&mut batch, generation, &tx, wake_ui.as_ref());
            }
        } else if let Ok(entries) = std::fs::read_dir(&dir) {
            #[cfg(feature = "preload-debug")]
            let mut dir_entries = 0usize;
            #[cfg(feature = "preload-debug")]
            let mut ext_probes = 0usize;
            #[cfg(feature = "preload-debug")]
            let mut image_candidates = 0usize;
            if paired_raw_jpeg_handling.needs_pair_index() {
                let mut files: Vec<(PathBuf, u64, Option<i64>)> = Vec::new();
                for e in entries.flatten() {
                    #[cfg(feature = "preload-debug")]
                    {
                        dir_entries += 1;
                    }
                    if cancel.load(Ordering::Relaxed) {
                        log::info!("[Scanner] Scan (non-recursive) cancelled for {:?}", dir);
                        return;
                    }

                    let is_supported = Path::new(&e.file_name())
                        .extension()
                        .map(|ext| {
                            #[cfg(feature = "preload-debug")]
                            {
                                ext_probes += 1;
                            }
                            is_supported_extension(ext)
                        })
                        .unwrap_or(false);

                    if is_supported {
                        #[cfg(feature = "preload-debug")]
                        {
                            image_candidates += 1;
                        }
                        let Ok(file_type) = e.file_type() else {
                            continue;
                        };
                        if file_type.is_symlink() {
                            continue;
                        }
                        let p = e.path();
                        let Ok(meta) = e.metadata() else {
                            continue;
                        };
                        if let Some((len, modified_unix)) = validated_metadata(&meta) {
                            files.push((p, len, modified_unix));
                        }
                    }
                }
                #[cfg(feature = "preload-debug")]
                crate::preload_debug!(
                    "[PreloadDebug][Scan] read_dir done (pair index): entries={} ext_probes={} images={} read_ms={}",
                    dir_entries,
                    ext_probes,
                    image_candidates,
                    crate::preload_debug::elapsed_ms(scan_started)
                );
                send_scanned_files(
                    files,
                    paired_raw_jpeg_handling,
                    generation,
                    &tx,
                    wake_ui.as_ref(),
                );
            } else {
                let mut batch: Vec<(PathBuf, u64, Option<i64>)> = Vec::with_capacity(BATCH_SIZE);
                for e in entries.flatten() {
                    #[cfg(feature = "preload-debug")]
                    {
                        dir_entries += 1;
                    }
                    if cancel.load(Ordering::Relaxed) {
                        log::info!("[Scanner] Scan (non-recursive) cancelled for {:?}", dir);
                        return;
                    }

                    let is_supported = Path::new(&e.file_name())
                        .extension()
                        .map(|ext| {
                            #[cfg(feature = "preload-debug")]
                            {
                                ext_probes += 1;
                            }
                            is_supported_extension(ext)
                        })
                        .unwrap_or(false);

                    if is_supported {
                        #[cfg(feature = "preload-debug")]
                        {
                            image_candidates += 1;
                        }
                        let Ok(file_type) = e.file_type() else {
                            continue;
                        };
                        if file_type.is_symlink() {
                            continue;
                        }
                        let p = e.path();
                        let Ok(meta) = e.metadata() else {
                            continue;
                        };
                        if let Some((len, modified_unix)) = validated_metadata(&meta) {
                            batch.push((p, len, modified_unix));
                            if batch.len() >= BATCH_SIZE {
                                send_scan_batch(&mut batch, generation, &tx, wake_ui.as_ref());
                            }
                        }
                    }
                }
                #[cfg(feature = "preload-debug")]
                crate::preload_debug!(
                    "[PreloadDebug][Scan] read_dir done: entries={} ext_probes={} images={} read_ms={}",
                    dir_entries,
                    ext_probes,
                    image_candidates,
                    crate::preload_debug::elapsed_ms(scan_started)
                );
                send_scan_batch(&mut batch, generation, &tx, wake_ui.as_ref());
            }
        } else {
            crate::preload_debug!(
                "[PreloadDebug][Scan] read_dir failed: dir={}",
                dir.display()
            );
        }

        let _ = tx.send(ScanMessage::Done { generation });
        if let Some(wake) = wake_ui.as_ref() {
            wake();
        }
        crate::preload_debug!(
            "[PreloadDebug][Scan] thread done sent: dir={} total_ms={}",
            dir.display(),
            crate::preload_debug::elapsed_ms(scan_started)
        );
    });
}

fn send_scan_batch(
    batch: &mut Vec<(PathBuf, u64, Option<i64>)>,
    generation: u64,
    tx: &Sender<ScanMessage>,
    wake_ui: Option<&Arc<dyn Fn() + Send + Sync>>,
) {
    if batch.is_empty() {
        return;
    }
    #[cfg(feature = "preload-debug")]
    let send_started = std::time::Instant::now();
    batch.sort_by(|a, b| a.0.cmp(&b.0));
    #[cfg(feature = "preload-debug")]
    let batch_count = batch.len();
    let _ = tx.send(ScanMessage::Batch {
        generation,
        files: std::mem::take(batch),
    });
    if let Some(wake) = wake_ui {
        wake();
    }
    batch.reserve(BATCH_SIZE);
    #[cfg(feature = "preload-debug")]
    crate::preload_debug!(
        "[PreloadDebug][Scan] batch sent: count={} send_ms={}",
        batch_count,
        crate::preload_debug::elapsed_ms(send_started)
    );
}

fn send_scanned_files(
    mut files: Vec<(PathBuf, u64, Option<i64>)>,
    paired_raw_jpeg_handling: PairedRawJpegHandling,
    generation: u64,
    tx: &Sender<ScanMessage>,
    wake_ui: Option<&Arc<dyn Fn() + Send + Sync>>,
) {
    #[cfg(feature = "preload-debug")]
    let send_started = std::time::Instant::now();
    if paired_raw_jpeg_handling.needs_pair_index() {
        filter_raw_jpeg_pairs(&mut files, paired_raw_jpeg_handling);
    }

    // Keep global ordering stable across batches. This holds all paths until the scan finishes,
    // which costs more memory than sorting per batch, but per-batch sorting is not globally sorted.
    files.sort_by(|a, b| a.0.cmp(&b.0));
    #[cfg(feature = "preload-debug")]
    let total_files = files.len();
    let mut batch = Vec::with_capacity(BATCH_SIZE);
    #[cfg(feature = "preload-debug")]
    let mut batches_sent = 0usize;
    for file in files {
        batch.push(file);
        if batch.len() >= BATCH_SIZE {
            let _ = tx.send(ScanMessage::Batch {
                generation,
                files: std::mem::take(&mut batch),
            });
            if let Some(wake) = wake_ui {
                wake();
            }
            batch.reserve(BATCH_SIZE);
            #[cfg(feature = "preload-debug")]
            {
                batches_sent += 1;
            }
        }
    }
    if !batch.is_empty() {
        let _ = tx.send(ScanMessage::Batch {
            generation,
            files: batch,
        });
        if let Some(wake) = wake_ui {
            wake();
        }
        #[cfg(feature = "preload-debug")]
        {
            batches_sent += 1;
        }
    }
    #[cfg(feature = "preload-debug")]
    crate::preload_debug!(
        "[PreloadDebug][Scan] send_scanned_files: files={} batches={} ms={}",
        total_files,
        batches_sent,
        crate::preload_debug::elapsed_ms(send_started)
    );
}

fn filter_raw_jpeg_pairs(
    files: &mut Vec<(PathBuf, u64, Option<i64>)>,
    handling: PairedRawJpegHandling,
) {
    match handling {
        PairedRawJpegHandling::ShowBoth => {}
        PairedRawJpegHandling::SkipRaw => {
            let jpeg_stems = jpeg_stems_by_parent(files);
            files
                .retain(|(path, _, _)| !is_raw_path(path) || !has_matching_stem(path, &jpeg_stems));
        }
        PairedRawJpegHandling::SkipJpeg => {
            let raw_stems = raw_stems_by_parent(files);
            files
                .retain(|(path, _, _)| !is_jpeg_path(path) || !has_matching_stem(path, &raw_stems));
        }
    }
}

fn jpeg_stems_by_parent(
    files: &[(PathBuf, u64, Option<i64>)],
) -> HashMap<PathBuf, HashSet<OsString>> {
    let mut by_parent: HashMap<PathBuf, HashSet<OsString>> = HashMap::new();
    for key in files.iter().filter_map(|(path, _, _)| jpeg_stem_key(path)) {
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

fn raw_stems_by_parent(
    files: &[(PathBuf, u64, Option<i64>)],
) -> HashMap<PathBuf, HashSet<OsString>> {
    let mut by_parent: HashMap<PathBuf, HashSet<OsString>> = HashMap::new();
    for key in files.iter().filter_map(|(path, _, _)| raw_stem_key(path)) {
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
            1,
            tx,
            Arc::new(AtomicBool::new(false)),
            None,
        );

        let mut files = Vec::new();
        loop {
            match rx.recv().expect("scan message") {
                ScanMessage::Batch { files: batch, .. } => {
                    files.extend(batch.into_iter().map(|(path, _, _)| {
                        path.file_name()
                            .expect("file name")
                            .to_string_lossy()
                            .into_owned()
                    }));
                }
                ScanMessage::Done { .. } => break,
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

    #[test]
    fn directory_traversal_boundary_skips_symlink_directory() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            let dir = TempScanDir::new();
            let target = dir.path().join("target");
            std::fs::create_dir(&target).expect("create target directory");
            let link = dir.path().join("link");
            symlink(&target, &link).expect("create directory symlink");
            assert!(super::is_directory_traversal_boundary_metadata(
                &std::fs::symlink_metadata(&link).expect("symlink metadata")
            ));
        }
    }
}
