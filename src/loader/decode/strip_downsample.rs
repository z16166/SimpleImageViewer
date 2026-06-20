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

use std::borrow::Cow;

use image::imageops::{FilterType, resize};

use crate::loader::DecodedImage;

pub(crate) fn downsample_decoded_for_strip<'a>(
    decoded: &'a DecodedImage,
    max_side: u32,
) -> Result<Cow<'a, DecodedImage>, String> {
    let max_dim = decoded.width.max(decoded.height);
    if max_dim <= max_side {
        return Ok(Cow::Borrowed(decoded));
    }
    let src = decoded.clone().into_rgba8_image()?;
    let scale = max_side as f32 / max_dim as f32;
    let out_w = ((decoded.width as f32 * scale).round() as u32).max(1);
    let out_h = ((decoded.height as f32 * scale).round() as u32).max(1);
    let resized = resize(&src, out_w, out_h, FilterType::Triangle);
    Ok(Cow::Owned(DecodedImage::from(resized)))
}
