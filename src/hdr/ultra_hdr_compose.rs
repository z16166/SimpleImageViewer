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

//! CPU reference compose for Ultra HDR JPEG (tests and tiled preview).

use std::sync::Arc;

use crate::hdr::gain_map::{
    GainMapMetadata, append_hdr_pixel_from_sdr_and_gain, sample_gain_map_rgb,
};
use crate::hdr::types::{HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat};

pub(crate) struct UltraHdrComposeInput<'a> {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) sdr_rgba: &'a [u8],
    pub(crate) gain_rgba: &'a [u8],
    pub(crate) gain_width: u32,
    pub(crate) gain_height: u32,
    pub(crate) metadata: GainMapMetadata,
    pub(crate) image_metadata: HdrImageMetadata,
    pub(crate) target_hdr_capacity: f32,
}

#[allow(dead_code)] // tests and tiled preview reference path
pub(crate) fn compose_ultra_hdr_cpu(input: UltraHdrComposeInput<'_>) -> HdrImageBuffer {
    let UltraHdrComposeInput {
        width,
        height,
        sdr_rgba,
        gain_rgba,
        gain_width,
        gain_height,
        metadata,
        image_metadata,
        target_hdr_capacity,
    } = input;
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

pub(crate) struct UltraHdrTileRegionCompose<'a, F> {
    pub(crate) tile_width: u32,
    pub(crate) tile_height: u32,
    pub(crate) origin_x: u32,
    pub(crate) origin_y: u32,
    pub(crate) physical_width: u32,
    pub(crate) physical_height: u32,
    pub(crate) orientation: u16,
    pub(crate) sdr_rgba: &'a [u8],
    pub(crate) gain_rgba: &'a [u8],
    pub(crate) gain_width: u32,
    pub(crate) gain_height: u32,
    pub(crate) metadata: GainMapMetadata,
    pub(crate) target_hdr_capacity: f32,
    pub(crate) display_to_physical: F,
}

#[allow(dead_code)] // loader/ultra_hdr tests and tiled preview reference path
pub(crate) fn compose_ultra_hdr_tile_region_cpu<F>(
    input: UltraHdrTileRegionCompose<'_, F>,
) -> Vec<f32>
where
    F: Fn(u32, u32, u32, u32, u16) -> (u32, u32),
{
    let UltraHdrTileRegionCompose {
        tile_width,
        tile_height,
        origin_x,
        origin_y,
        physical_width,
        physical_height,
        orientation,
        sdr_rgba,
        gain_rgba,
        gain_width,
        gain_height,
        metadata,
        target_hdr_capacity,
        display_to_physical,
    } = input;
    let mut rgba_f32 = Vec::with_capacity(tile_width as usize * tile_height as usize * 4);
    for dy in 0..tile_height {
        for dx in 0..tile_width {
            let display_x = origin_x + dx;
            let display_y = origin_y + dy;
            let (physical_x, physical_y) = display_to_physical(
                display_x,
                display_y,
                physical_width,
                physical_height,
                orientation,
            );
            let sdr_index =
                (physical_y as usize * physical_width as usize + physical_x as usize) * 4;
            let gain_value = sample_gain_map_rgb(
                gain_rgba,
                gain_width,
                gain_height,
                physical_x,
                physical_y,
                physical_width,
                physical_height,
            );
            append_hdr_pixel_from_sdr_and_gain(
                &mut rgba_f32,
                &sdr_rgba[sdr_index..sdr_index + 4],
                gain_value,
                metadata,
                target_hdr_capacity,
            );
        }
    }
    rgba_f32
}
