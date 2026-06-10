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

use super::header::validate_scanline_offsets;
use super::layout::RadianceRasterLayout;
use super::layout::{
    Rgbe8Pixel, inner_range_covering_coord_inclusive, outer_range_covering_coord_inclusive,
};
use super::rle::read_scanline;

use std::io::Cursor;
use std::sync::Arc;

use crate::hdr::tiled::validate_tile_bounds;
use crate::hdr::types::{HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat};

pub(crate) fn decode_radiance_tile_window(
    mmap: &[u8],
    raster: RadianceRasterLayout,
    params: crate::hdr::decode::RadianceHeaderParams,
    scanline_offsets: &[usize],
    tile_x: u32,
    tile_y: u32,
    tile_w: u32,
    tile_h: u32,
) -> Result<Vec<f32>, String> {
    validate_tile_bounds(raster.width, raster.height, tile_x, tile_y, tile_w, tile_h)?;
    validate_scanline_offsets(raster.outer_len, scanline_offsets)?;
    let mut reader = Cursor::new(mmap);

    let mut scanline = vec![Rgbe8Pixel::default(); raster.inner_len as usize];
    let mut rgba = vec![0.0f32; tile_w as usize * tile_h as usize * 4];

    if raster.is_row_major_top_left() {
        let first_row = tile_y;
        let last_row_exclusive = tile_y + tile_h;
        for row in first_row..last_row_exclusive {
            reader.set_position(scanline_offsets[row as usize] as u64);
            read_scanline(&mut reader, &mut scanline)?;

            let start = tile_x as usize;
            let end = start + tile_w as usize;
            let out_row = row - tile_y;
            for (dx, pixel) in scanline[start..end].iter().enumerate() {
                let rgb = pixel.to_rgb_f32();
                let base = (out_row as usize * tile_w as usize + dx) * 4;
                rgba[base..base + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 1.0]);
            }
        }
    } else {
        let plan = raster.stride_plan();
        let tw = tile_w as usize;
        let tx0 = tile_x as i32;
        let ty0 = tile_y as i32;
        let tx1 = tx0 + tile_w as i32 - 1;
        let ty1 = ty0 + tile_h as i32 - 1;

        if plan.outer_major_is_y {
            let y0 = plan.y_start;
            if let Some((oa_lo, oa_hi)) =
                outer_range_covering_coord_inclusive(y0, plan.y_step, raster.outer_len, ty0, ty1)
            {
                for outer_a in oa_lo..=oa_hi {
                    reader.set_position(scanline_offsets[outer_a as usize] as u64);
                    read_scanline(&mut reader, &mut scanline)?;
                    let y = y0 + (outer_a as i32) * plan.y_step;
                    let dy = (y - ty0) as usize;
                    if let Some((imin, imax)) = inner_range_covering_coord_inclusive(
                        plan.x_start,
                        plan.x_step,
                        raster.inner_len,
                        tx0,
                        tx1,
                    ) {
                        for inner_b in imin..=imax {
                            let rgb = scanline[inner_b as usize].to_rgb_f32();
                            let x = plan.x_start + (inner_b as i32) * plan.x_step;
                            let dx = (x - tx0) as usize;
                            let base = (dy * tw + dx) * 4;
                            rgba[base..base + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 1.0]);
                        }
                    }
                }
            }
        } else {
            let x0 = plan.x_start;
            if let Some((oa_lo, oa_hi)) =
                outer_range_covering_coord_inclusive(x0, plan.x_step, raster.outer_len, tx0, tx1)
            {
                for outer_a in oa_lo..=oa_hi {
                    reader.set_position(scanline_offsets[outer_a as usize] as u64);
                    read_scanline(&mut reader, &mut scanline)?;
                    let x = x0 + (outer_a as i32) * plan.x_step;
                    let dx = (x - tx0) as usize;
                    if let Some((imin, imax)) = inner_range_covering_coord_inclusive(
                        plan.y_start,
                        plan.y_step,
                        raster.inner_len,
                        ty0,
                        ty1,
                    ) {
                        for inner_b in imin..=imax {
                            let rgb = scanline[inner_b as usize].to_rgb_f32();
                            let y = plan.y_start + (inner_b as i32) * plan.y_step;
                            let dy = (y - ty0) as usize;
                            let base = (dy * tw + dx) * 4;
                            rgba[base..base + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 1.0]);
                        }
                    }
                }
            }
        }
    }
    params.apply_to_pixels(&mut rgba);

    Ok(rgba)
}

pub(crate) fn decode_radiance_sdr_preview(
    mmap: &[u8],
    logical_width: u32,
    logical_height: u32,
    raster: RadianceRasterLayout,
    params: crate::hdr::decode::RadianceHeaderParams,
    scanline_offsets: &[usize],
    max_w: u32,
    max_h: u32,
) -> Result<(u32, u32, Vec<u8>), String> {
    let preview = decode_radiance_hdr_preview(
        mmap,
        logical_width,
        logical_height,
        raster,
        params,
        scanline_offsets,
        max_w,
        max_h,
    )?;
    let pixels = crate::hdr::decode::hdr_to_sdr_rgba8(&preview, 0.0)?;
    Ok((preview.width, preview.height, pixels))
}

pub(crate) fn decode_radiance_hdr_preview(
    mmap: &[u8],
    logical_width: u32,
    logical_height: u32,
    raster: RadianceRasterLayout,
    params: crate::hdr::decode::RadianceHeaderParams,
    scanline_offsets: &[usize],
    max_w: u32,
    max_h: u32,
) -> Result<HdrImageBuffer, String> {
    let (preview_width, preview_height) =
        preview_dimensions(logical_width, logical_height, max_w, max_h);
    if preview_width == 0 || preview_height == 0 {
        return Err("Radiance HDR preview dimensions must be non-zero".to_string());
    }

    validate_scanline_offsets(raster.outer_len, scanline_offsets)?;
    let mut reader = Cursor::new(mmap);

    let mut scanline = vec![Rgbe8Pixel::default(); raster.inner_len as usize];
    let mut rgba = vec![0.0f32; preview_width as usize * preview_height as usize * 4];

    if raster.is_row_major_top_left() {
        for preview_y in 0..preview_height {
            let source_y = preview_sample_coord(preview_y, preview_height, logical_height);
            reader.set_position(scanline_offsets[source_y as usize] as u64);
            read_scanline(&mut reader, &mut scanline)?;

            for preview_x in 0..preview_width {
                let source_x =
                    preview_sample_coord(preview_x, preview_width, logical_width) as usize;
                let rgb = scanline[source_x].to_rgb_f32();
                let base = (preview_y as usize * preview_width as usize + preview_x as usize) * 4;
                rgba[base..base + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 1.0]);
            }
        }
    } else {
        let mut last_outer_a: Option<u32> = None;
        for preview_y in 0..preview_height {
            for preview_x in 0..preview_width {
                let lx = preview_sample_coord(preview_x, preview_width, logical_width);
                let ly = preview_sample_coord(preview_y, preview_height, logical_height);
                let (outer_a, inner_b) = raster.file_indices_for_logical_xy(lx, ly);

                if last_outer_a != Some(outer_a) {
                    reader.set_position(scanline_offsets[outer_a as usize] as u64);
                    read_scanline(&mut reader, &mut scanline)?;
                    last_outer_a = Some(outer_a);
                }

                let rgb = scanline[inner_b as usize].to_rgb_f32();
                let base = (preview_y as usize * preview_width as usize + preview_x as usize) * 4;
                rgba[base..base + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 1.0]);
            }
        }
    }

    params.apply_to_pixels(&mut rgba);

    Ok(HdrImageBuffer {
        width: preview_width,
        height: preview_height,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::LinearSrgb,
        metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
        rgba_f32: Arc::new(rgba),
    })
}

fn preview_dimensions(width: u32, height: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    if width == 0 || height == 0 || max_w == 0 || max_h == 0 {
        return (0, 0);
    }

    let scale = (max_w as f32 / width as f32)
        .min(max_h as f32 / height as f32)
        .min(1.0);
    let preview_width = ((width as f32 * scale).round() as u32).clamp(1, max_w);
    let preview_height = ((height as f32 * scale).round() as u32).clamp(1, max_h);
    (preview_width, preview_height)
}

fn preview_sample_coord(preview_coord: u32, preview_extent: u32, source_extent: u32) -> u32 {
    if preview_extent <= 1 {
        return 0;
    }

    ((u64::from(preview_coord) * u64::from(source_extent - 1)) / u64::from(preview_extent - 1))
        as u32
}
