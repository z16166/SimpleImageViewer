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

//! Cheap gain-map presence probes for directory-tree strip compose upgrade scheduling.

use std::path::Path;

use super::modern::{is_avif_path, is_heif_path, is_jxl_path};

/// True when a file likely needs ISO forward gain-map strip compose (parse/box scan only).
pub(crate) fn path_needs_directory_tree_strip_compose_upgrade(path: &Path) -> bool {
    if !(is_avif_path(path) || is_heif_path(path) || is_jxl_path(path)) {
        return false;
    }
    let Ok(mmap) = crate::mmap_util::map_file(path) else {
        return false;
    };
    let bytes = mmap.as_ref();

    #[cfg(feature = "avif-native")]
    if is_avif_path(path) {
        return matches!(
            crate::hdr::avif::avif_probe_gain_map_strip_kind(bytes),
            Some(crate::hdr::avif::AvifGainMapStripProbe::ForwardIsoGainMap)
        );
    }

    #[cfg(feature = "jpegxl")]
    if is_jxl_path(path) {
        return crate::hdr::jpegxl::jxl_probe_forward_iso_gain_map(bytes);
    }

    #[cfg(feature = "heif-native")]
    if is_heif_path(path) {
        return crate::hdr::heif::heif_probe_forward_iso_gain_map(bytes);
    }

    false
}
