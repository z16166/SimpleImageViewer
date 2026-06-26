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

use image::imageops::{FilterType, resize};

use crate::loader::DecodedImage;

/// Downsample `decoded` so its long edge fits within `max_side`.
///
/// When this [`DecodedImage`] holds the only [`Arc`] reference to the pixel buffer,
/// [`DecodedImage::into_rgba8_image`] extracts the buffer without copying (zero-copy).
/// If the buffer is shared, the data is cloned once — never twice.
pub(crate) fn downsample_decoded_for_strip(
    decoded: DecodedImage,
    max_side: u32,
) -> Result<DecodedImage, String> {
    let w = decoded.width;
    let h = decoded.height;
    let max_dim = w.max(h);
    if max_dim <= max_side {
        return Ok(decoded);
    }
    // Zero-copy when this DecodedImage holds the only Arc reference.
    // Falls back to cloning the pixel data when the Arc is shared.
    let src = decoded.into_rgba8_image()?;
    let scale = max_side as f32 / max_dim as f32;
    let out_w = ((w as f32 * scale).round() as u32).max(1);
    let out_h = ((h as f32 * scale).round() as u32).max(1);
    let resized = resize(&src, out_w, out_h, FilterType::Triangle);
    Ok(DecodedImage::from(resized))
}
