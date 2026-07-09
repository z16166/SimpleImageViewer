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

use std::fs::File;
use std::path::Path;

/// User-facing error for empty or sub-header-sized image paths.
pub(crate) fn image_file_too_small_error(len: u64) -> String {
    if len == 0 {
        rust_i18n::t!("error.empty_image_file").to_string()
    } else {
        rust_i18n::t!(
            "error.image_file_too_small",
            bytes = len,
            min = crate::constants::MIN_IMAGE_FILE_BYTES
        )
        .to_string()
    }
}

fn reject_len_below_image_minimum(len: u64) -> Result<(), String> {
    if len < crate::constants::MIN_IMAGE_FILE_BYTES {
        Err(image_file_too_small_error(len))
    } else {
        Ok(())
    }
}

/// Cheap metadata probe before mmap/decode scheduling (one stat, no worker spawn).
pub(crate) fn reject_if_image_file_too_small(path: &Path) -> Result<(), String> {
    let len = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    reject_len_below_image_minimum(len)
}

/// Memory-map an existing file for read-only decoding paths (checklist: avoid `read_to_end` duplication).
///
/// Returns `(mmap, len)` from a single `metadata()` call so callers need not re-stat for size checks.
pub(crate) fn map_file(path: &Path) -> Result<(memmap2::Mmap, u64), String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let len = file.metadata().map_err(|e| e.to_string())?.len();
    reject_len_below_image_minimum(len)?;
    let mmap = unsafe { memmap2::Mmap::map(&file).map_err(|e| e.to_string())? };
    Ok((mmap, len))
}

#[cfg(test)]
mod tests {
    use super::{image_file_too_small_error, map_file, reject_if_image_file_too_small};
    use crate::constants::MIN_IMAGE_FILE_BYTES;
    use std::fs;
    use std::path::PathBuf;

    fn temp_image_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("siv-mmap-util-{name}-{}", std::process::id()))
    }

    #[test]
    fn image_file_too_small_error_distinguishes_empty_and_sub_header() {
        let empty = image_file_too_small_error(0);
        assert!(empty.contains("empty") || empty.contains("空"));
        let tiny = image_file_too_small_error(4);
        assert!(tiny.contains('4') || tiny.contains("4"));
    }

    #[test]
    fn reject_if_image_file_too_small_blocks_empty_and_sub_header_files() {
        let path = temp_image_path("reject");
        let _ = fs::remove_file(&path);

        fs::write(&path, []).unwrap();
        assert!(reject_if_image_file_too_small(&path).is_err());

        fs::write(&path, [0u8; 4]).unwrap();
        assert!(reject_if_image_file_too_small(&path).is_err());

        fs::write(&path, vec![0u8; MIN_IMAGE_FILE_BYTES as usize]).unwrap();
        assert!(reject_if_image_file_too_small(&path).is_ok());

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn map_file_returns_len_and_rejects_tiny_files() {
        let path = temp_image_path("map");
        let _ = fs::remove_file(&path);

        fs::write(&path, [0u8; 4]).unwrap();
        assert!(map_file(&path).is_err());

        let bytes = vec![0u8; MIN_IMAGE_FILE_BYTES as usize];
        fs::write(&path, &bytes).unwrap();
        let (mmap, len) = map_file(&path).expect("map");
        assert_eq!(len, bytes.len() as u64);
        assert_eq!(mmap.len(), bytes.len());

        let _ = fs::remove_file(&path);
    }
}
