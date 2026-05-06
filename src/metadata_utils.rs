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
use std::io::BufReader;
use std::path::Path;

/// Extension-only hint for HEIF container orientation sidecars (`Exif` may not be reachable via
/// [`exif::Reader::read_from_container`] on every writer layout).
fn is_heif_extension(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        matches!(
            e.to_ascii_lowercase().as_str(),
            "heic" | "heif" | "hif"
        )
    })
}

/// [`exif::Reader::read_from_container`] first (TIFF / HEIF BMFF scan), then libheif’s view of
/// embedded `Exif` metadata items when the crate scan misses them (common on some **`.heic`** files).
pub fn get_exif_orientation(path: &Path) -> u16 {
    if let Ok(file) = File::open(path) {
        let mut reader = BufReader::new(file);
        let exifreader = exif::Reader::new();
        if let Ok(exif_data) = exifreader.read_from_container(&mut reader) {
            if let Some(field) = exif_data.get_field(exif::Tag::Orientation, exif::In::PRIMARY) {
                // Some writers store Orientation as BYTE or LONG; Short is most common.
                if let Some(o) = field.value.get_uint(0) {
                    let o = o as u16;
                    if (1..=8).contains(&o) {
                        return o;
                    }
                }
            }
        }
    }
    #[cfg(feature = "heif-native")]
    {
        if is_heif_extension(path) {
            if let Some(o) = crate::hdr::heif::libheif_exif_orientation_tag(path) {
                if (1..=8).contains(&o) {
                    return o;
                }
            }
        }
    }
    1
}
