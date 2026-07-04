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
use std::sync::Arc;

use crate::hdr::tiled::HdrTiledSource;

use crate::hdr::types::{HdrImageBuffer, HdrPixelFormat};

pub(crate) fn decode_exr_display_image(path: &Path) -> Result<HdrImageBuffer, String> {
    let mmap = Arc::new(crate::mmap_util::map_file(path)?);
    decode_exr_display_image_from_mmap(path, mmap)
}

pub(crate) fn decode_exr_display_image_from_mmap(
    path: &Path,
    mmap: Arc<memmap2::Mmap>,
) -> Result<HdrImageBuffer, String> {
    let source = crate::hdr::exr_tiled::ExrTiledImageSource::open_from_mmap(path, mmap)?;
    let (width, height) = (source.width(), source.height());
    super::tone_map::validate_hdr_fallback_budget(width, height)?;
    let tile = source.extract_tile_rgba32f_arc(0, 0, width, height)?;

    Ok(HdrImageBuffer {
        width,
        height,
        format: HdrPixelFormat::Rgba32Float,
        color_space: tile.color_space,
        metadata: tile.metadata.clone(),
        rgba_f32: Arc::clone(&tile.rgba_f32),
    })
}
