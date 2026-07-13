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

/// Radiance HDR magic line (`#?RADIANCE`) used by the official Radiance RGBE format.
const RADIANCE_HDR_MAGIC: &[u8] = b"#?RADIANCE";

pub(crate) fn is_exr_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("exr"))
}

pub(crate) fn is_radiance_hdr_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| matches!(ext.to_ascii_lowercase().as_str(), "hdr" | "pic"))
}

/// Cheap header sniff: true when bytes start with the Radiance RGBE magic.
///
/// Used to skip a full HDR decode attempt when a file only has a `.hdr`/`.pic`
/// extension but is not actually a Radiance container.
pub(crate) fn looks_like_radiance_hdr_bytes(bytes: &[u8]) -> bool {
    bytes.len() >= RADIANCE_HDR_MAGIC.len() && bytes.starts_with(RADIANCE_HDR_MAGIC)
}
