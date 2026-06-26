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

//! Shared downsample helper for directory-tree strip thumbnails.

use crate::loader::DecodedImage;
use simple_image_viewer::simd_downsample::downsample_rgba8_box;

/// Downsample `decoded` so its long edge fits within `max_side`.
///
/// Takes `&DecodedImage` to avoid unnecessary [`Arc`] reference-count
/// operations when the caller already holds a reference (e.g. `hdr_fallback`).
/// Uses a SIMD-accelerated box-filter (area-averaging) downsample that
/// operates on the borrowed pixel slice — zero-copy *when downsampling occurs*.
///
/// When the image is already small enough (`max_dim <= max_side`), this
/// returns a cheap [`DecodedImage::clone`] (an [`Arc`] ref-count bump, not a
/// pixel buffer copy) rather than performing a no-op downsample.
pub(crate) fn downsample_decoded_for_strip(
    decoded: &DecodedImage,
    max_side: u32,
) -> Result<DecodedImage, String> {
    let w = decoded.width;
    let h = decoded.height;
    let max_dim = w.max(h);
    if max_dim <= max_side {
        return Ok(decoded.clone());
    }
    let scale = max_side as f32 / max_dim as f32;
    let out_w = ((w as f32 * scale).round() as u32).max(1);
    let out_h = ((h as f32 * scale).round() as u32).max(1);
    let pixels = downsample_rgba8_box(decoded.rgba(), w, h, out_w, out_h);
    Ok(DecodedImage::new(out_w, out_h, pixels))
}
