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

//! CPU reference compose for Ultra HDR JPEG (tests and tiled fallback).

use std::sync::Arc;

use crate::hdr::gain_map::{
    GainMapMetadata, append_hdr_pixel_from_sdr_and_gain, sample_gain_map_rgb,
};
use crate::hdr::types::{HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat};

#[allow(dead_code)] // tests via `decode_ultra_hdr_jpeg_bytes_with_cpu_compose`; future tiled GPU path
pub(crate) fn compose_ultra_hdr_cpu(
    width: u32,
    height: u32,
    sdr_rgba: &[u8],
    gain_rgba: &[u8],
    gain_width: u32,
    gain_height: u32,
    metadata: GainMapMetadata,
    image_metadata: HdrImageMetadata,
    target_hdr_capacity: f32,
) -> HdrImageBuffer {
    let mut rgba_f32 = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            let sdr_index = (y as usize * width as usize + x as usize) * 4;
            let gain_value =
                sample_gain_map_rgb(gain_rgba, gain_width, gain_height, x, y, width, height);
            append_hdr_pixel_from_sdr_and_gain(
                &mut rgba_f32,
                &sdr_rgba[sdr_index..sdr_index + 4],
                gain_value,
                metadata,
                target_hdr_capacity,
            );
        }
    }

    HdrImageBuffer {
        width,
        height,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::LinearSrgb,
        metadata: image_metadata,
        rgba_f32: Arc::new(rgba_f32),
    }
}
