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

#[cfg(test)]
use std::io::{BufRead, Cursor};

use image::{ImageReader, Limits};

use crate::hdr::tiled::HdrTiledSource;

use crate::hdr::types::{
    HdrColorProfile, HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat, HdrReference,
    HdrToneMapSettings, HdrTransferFunction,
};

const HDR_RGBA32F_BYTES_PER_PIXEL: u64 = 4 * std::mem::size_of::<f32>() as u64;
const SDR_RGBA8_BYTES_PER_PIXEL: u64 = 4;
const HDR_FALLBACK_BYTES_PER_PIXEL_WITH_SDR: u64 =
    HDR_RGBA32F_BYTES_PER_PIXEL + SDR_RGBA8_BYTES_PER_PIXEL;
const MAX_HDR_FALLBACK_PIXELS: u64 = 8192 * 8192;
const MAX_HDR_FALLBACK_DECODE_BYTES: u64 = MAX_HDR_FALLBACK_PIXELS * HDR_RGBA32F_BYTES_PER_PIXEL;
const MAX_HDR_FALLBACK_TOTAL_BYTES: u64 =
    MAX_HDR_FALLBACK_PIXELS * HDR_FALLBACK_BYTES_PER_PIXEL_WITH_SDR;
const MAX_HDR_TONE_MAP_INPUT: f32 = f32::MAX;
const INVERSE_DISPLAY_GAMMA: f32 = 1.0 / 2.2;

pub fn is_hdr_candidate_ext(ext: &str) -> bool {
    ext.eq_ignore_ascii_case("exr")
        || ext.eq_ignore_ascii_case("hdr")
        || ext.eq_ignore_ascii_case("pic")
}

pub fn decode_hdr_image(path: &Path) -> Result<HdrImageBuffer, String> {
    if super::paths::is_exr_path(path) {
        return super::exr::decode_exr_display_image(path);
    }
    if super::paths::is_radiance_hdr_path(path) {
        return super::radiance::decode_radiance_hdr_image(path);
    }

    let mmap = crate::mmap_util::map_file(path)?;
    let (width, height) = ImageReader::new(std::io::Cursor::new(&mmap[..]))
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .into_dimensions()
        .map_err(|e| e.to_string())?;
    super::tone_map::validate_hdr_fallback_budget(width, height)?;

    let mut decoder = ImageReader::new(std::io::Cursor::new(&mmap[..]))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut limits = Limits::default();
    limits.max_alloc = Some(MAX_HDR_FALLBACK_DECODE_BYTES);
    decoder.limits(limits);

    let rgba = decoder.decode().map_err(|e| e.to_string())?.into_rgba32f();

    Ok(HdrImageBuffer {
        width,
        height,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::LinearSrgb,
        metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
        rgba_f32: Arc::new(rgba.into_raw()),
    })
}
