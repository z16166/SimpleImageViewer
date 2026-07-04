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

//! Path/extension helpers and optional asset smoke tests.

use std::path::{Path, PathBuf};

use crate::loader::ImageData;
use crate::loader::decode::modern::{
    is_avif_path, is_hdr_capable_modern_format_path, is_heif_path, is_jxl_path,
};
use crate::loader::decode::raster::load_psd;

#[test]
fn modern_hdr_format_path_helpers_detect_supported_extensions() {
    assert!(is_avif_path(Path::new("sample.avif")));
    assert!(is_avif_path(Path::new("sample.avifs")));
    assert!(is_heif_path(Path::new("sample.HEIC")));
    assert!(is_jxl_path(Path::new("sample.jxl")));
    assert!(is_hdr_capable_modern_format_path(Path::new("sample.heif")));
    assert!(!is_hdr_capable_modern_format_path(Path::new("sample.png")));
}

/// Set `SIV_PSD_SAMPLES_DIR` to a folder that contains `colors.psd` and `seine.psd`
/// (for example `libavif/tests/data/sources` inside a libavif source checkout) to regression-test the `psd` crate composite
/// path: it must not unwind (historical `psd_channel` index OOB panics).
///
/// When the variable is unset or files are missing, this test is a no-op so CI stays green.
#[test]
fn optional_psd_libavif_sources_load_without_panic() {
    let Some(dir) = std::env::var("SIV_PSD_SAMPLES_DIR")
        .ok()
        .filter(|p| Path::new(p).is_dir())
    else {
        return;
    };
    let dir = PathBuf::from(dir);
    for name in ["colors.psd", "seine.psd"] {
        let path = dir.join(name);
        if !path.is_file() {
            continue;
        }
        let outcome =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| load_psd(&path, None)));
        assert!(
            outcome.is_ok(),
            "load_psd must not panic for {}",
            path.display()
        );
        match outcome.unwrap() {
            Ok(data) => match &data {
                ImageData::Static(img) => {
                    assert!(img.width > 0 && img.height > 0, "{name}: static dims");
                }
                ImageData::Tiled(src) => {
                    assert!(src.width() > 0 && src.height() > 0, "{name}: tiled dims");
                }
                _ => panic!("{name}: unexpected PSD ImageData shape"),
            },
            Err(_msg) => {
                // OOM guard, `psd` parse error, or composite `Err` after catch_unwind — all OK.
            }
        }
    }
}
