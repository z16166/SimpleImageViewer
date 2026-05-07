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

//! HDR/SDR [`ImageData`] assembly (static vs tiled) using [`crate::loader::tiled_sources::MemoryImageSource`].

use crate::loader::{DecodedImage, ImageData, TiledImageSource};
use crate::loader::tiled_sources::MemoryImageSource;
use std::sync::Arc;

pub(crate) fn make_image_data(img: DecodedImage) -> ImageData {
    let pixel_count = img.width as u64 * img.height as u64;
    let max_side = img.width.max(img.height);
    // Use the conservative ABSOLUTE_MAX_TEXTURE_SIDE (8192) for the tiling decision,
    // consistent with WIC, macOS ImageIO, and Linux libtiff paths.
    // Images exceeding 8192 on any side benefit from the tiled preview pipeline
    // (instant EXIF preview + async HQ preview) regardless of GPU capability.
    // The GPU's actual texture limit (often 16384) is used only at the wgpu device
    // level to allow tile textures of any supported size.
    let limit = crate::constants::ABSOLUTE_MAX_TEXTURE_SIDE;
    let tiled_limit = crate::tile_cache::TILED_THRESHOLD.load(std::sync::atomic::Ordering::Relaxed);

    if pixel_count >= tiled_limit || max_side > limit {
        log::info!(
            "[Loader] Image {}x{} ({:.1} MP) exceeds GPU limit ({}) or threshold ({:.1} MP). Using forced tiling.",
            img.width,
            img.height,
            pixel_count as f64 / 1_000_000.0,
            limit,
            tiled_limit as f64 / 1_000_000.0
        );
        ImageData::Tiled(Arc::new(MemoryImageSource::new(
            img.width,
            img.height,
            img.into_arc_pixels(),
        )))
    } else {
        ImageData::Static(img)
    }
}

pub(crate) fn make_hdr_image_data(
    hdr: crate::hdr::types::HdrImageBuffer,
    fallback: DecodedImage,
) -> ImageData {
    make_hdr_image_data_for_limit(hdr, fallback, crate::constants::ABSOLUTE_MAX_TEXTURE_SIDE)
}

pub(crate) fn make_hdr_image_data_for_limit(
    hdr: crate::hdr::types::HdrImageBuffer,
    fallback: DecodedImage,
    max_texture_side: u32,
) -> ImageData {
    let pixel_count = hdr.width as u64 * hdr.height as u64;
    let tiled_limit = crate::tile_cache::TILED_THRESHOLD.load(std::sync::atomic::Ordering::Relaxed);
    let max_side = hdr.width.max(hdr.height);

    if pixel_count >= tiled_limit || max_side > max_texture_side {
        log::info!(
            "[Loader] HDR image {}x{} exceeds callback texture limit ({}) or threshold ({:.1} MP). Using SDR tiled fallback.",
            hdr.width,
            hdr.height,
            max_texture_side,
            tiled_limit as f64 / 1_000_000.0
        );
        let fallback_source = Arc::new(MemoryImageSource::new_with_hdr_sdr_fallback(
            fallback.width,
            fallback.height,
            fallback.into_arc_pixels(),
            true,
        ));

        match crate::hdr::tiled::HdrTiledImageSource::new(hdr) {
            Ok(hdr_source) => {
                let kind = crate::hdr::tiled::HdrTiledSource::source_kind(&hdr_source);
                log::info!(
                    "[Loader] HDR tiled source ready: kind={}, {}x{}",
                    kind.as_str(),
                    fallback_source.width(),
                    fallback_source.height()
                );
                ImageData::HdrTiled {
                    hdr: Arc::new(hdr_source),
                    fallback: fallback_source,
                }
            }
            Err(err) => {
                log::warn!("[Loader] HDR tiled source unavailable; using SDR fallback: {err}");
                ImageData::Tiled(fallback_source)
            }
        }
    } else {
        ImageData::Hdr { hdr, fallback }
    }
}
