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

use std::path::Path;

/// AVIF-related `ftyp` brands (MIAF). `avis` denotes image sequence in the container, not a filename suffix.
pub(crate) fn is_avif_brand(brand: &[u8]) -> bool {
    matches!(brand, b"avif" | b"avis")
}

/// True when the container major brand is `avis` (animated image sequence track).
#[cfg(feature = "avif-native")]
pub(crate) fn bytes_is_avif_image_sequence(bytes: &[u8]) -> bool {
    matches!(
        super::decode::avif_ftyp_major_brand(bytes),
        Some(brand) if &brand == b"avis"
    )
}

/// True when the container major brand is `avis` (animated image sequence track).
#[cfg(feature = "avif-native")]
pub(crate) fn path_is_avif_image_sequence(path: &Path) -> bool {
    let Ok((mmap, _)) = crate::mmap_util::map_file(path) else {
        return false;
    };
    bytes_is_avif_image_sequence(mmap.as_ref())
}

#[cfg(not(feature = "avif-native"))]
pub(crate) fn path_is_avif_image_sequence(_path: &Path) -> bool {
    false
}
