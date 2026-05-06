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

//! Baseline JPEG and Ultra HDR (JPEG_R).

use crate::hdr::types::HdrToneMapSettings;
use crate::loader::{hdr_gain_map_decode_capacity, hdr_sdr_fallback_rgba8_eager_or_placeholder};
use crate::loader::{DecodedImage, ImageData};
use std::path::PathBuf;
use std::sync::Arc;

use super::assemble::{make_hdr_image_data, make_image_data};
use crate::loader::tiled_sources::MemoryImageSource;

#[cfg(test)]
pub(crate) fn load_jpeg(path: &PathBuf) -> Result<ImageData, String> {
    load_jpeg_with_target_capacity(
        path,
        HdrToneMapSettings::default().target_hdr_capacity(),
        HdrToneMapSettings::default(),
    )
}

pub(crate) fn load_jpeg_with_target_capacity(
    path: &PathBuf,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
) -> Result<ImageData, String> {
    let decode_capacity = hdr_gain_map_decode_capacity(hdr_target_capacity, &hdr_tone_map);
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mmap = unsafe { memmap2::Mmap::map(&file).map_err(|e| e.to_string())? };
    // Sole orientation pass for all JPEG decodes (baseline SDR, **JPEG_R / Ultra HDR**). Do not
    // combine with [`apply_exif_orientation_to_image_data`] — that would double-rotate.
    let orientation = crate::metadata_utils::get_exif_orientation(path);
    // Apply EXIF Orientation per TIFF/EXIF rules (same transform family as Pillow `exif_transpose`).
    // Some reference JPEGs (e.g. libavif `paris_exif_orientation_5.jpg`) store a raster that already
    // looks like a normal landscape before correction; the tag still requests transpose, so the
    // result can differ from viewers that ignore the tag or use heuristics.
    if let Ok(hdr) = crate::hdr::ultra_hdr::decode_ultra_hdr_jpeg_bytes_with_target_capacity(
        &mmap,
        decode_capacity,
    ) {
        let pixel_count = hdr.width as u64 * hdr.height as u64;
        let tiled_limit =
            crate::tile_cache::TILED_THRESHOLD.load(std::sync::atomic::Ordering::Relaxed);
        let max_side = hdr.width.max(hdr.height);
        if pixel_count >= tiled_limit || max_side > crate::constants::ABSOLUTE_MAX_TEXTURE_SIDE {
            let (mut w, mut h, mut pixels) = libjpeg_turbo::decode_to_rgba(&mmap)?;
            if orientation > 1 {
                let oriented =
                    crate::libtiff_loader::apply_orientation_buffer(pixels, w, h, orientation);
                w = oriented.0;
                h = oriented.1;
                pixels = oriented.2;
            }
            if let Ok(hdr_source) =
                crate::hdr::ultra_hdr::UltraHdrTiledImageSource::open_with_target_capacity(
                    path.clone(),
                    orientation,
                    decode_capacity,
                )
            {
                let fallback = Arc::new(MemoryImageSource::new_with_hdr_sdr_fallback(
                    w,
                    h,
                    Arc::new(pixels),
                    true,
                ));
                return Ok(ImageData::HdrTiled {
                    hdr: Arc::new(hdr_source),
                    fallback,
                });
            }
        }

        let hdr = crate::hdr::ultra_hdr::apply_orientation_to_hdr_buffer(hdr, orientation);
        let fallback_pixels = hdr_sdr_fallback_rgba8_eager_or_placeholder(
            &hdr,
            hdr_target_capacity,
            &hdr_tone_map,
        )?;
        let fallback = DecodedImage::new(hdr.width, hdr.height, fallback_pixels);
        return Ok(make_hdr_image_data(hdr, fallback));
    }

    let (mut w, mut h, mut pixels) = libjpeg_turbo::decode_to_rgba(&mmap)?;

    if orientation > 1 {
        let (out_w, out_h, out_pixels) =
            crate::libtiff_loader::apply_orientation_buffer(pixels, w, h, orientation);
        w = out_w;
        h = out_h;
        pixels = out_pixels;
    }

    Ok(make_image_data(DecodedImage::new(w, h, pixels)))
}
