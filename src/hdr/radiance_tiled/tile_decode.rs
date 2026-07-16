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
use super::rle::{read_scanline, rgbe_pixels_to_rgba32f};

use std::io::Cursor;
use std::sync::Arc;

use rayon::prelude::*;

use crate::hdr::tiled::validate_tile_bounds;
use crate::hdr::types::{HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat};

const PARALLEL_ROW_THRESHOLD: u32 = 8;

#[derive(Clone, Copy)]
struct RadiancePreviewRowContext<'a> {
    mmap: &'a [u8],
    scanline_offsets: &'a [usize],
    inner_len: usize,
    preview_width: u32,
    preview_height: u32,
    logical_width: u32,
    logical_height: u32,
}

#[derive(Clone, Copy)]
pub(crate) struct RadianceTileWindow {
    pub(crate) tile_x: u32,
    pub(crate) tile_y: u32,
    pub(crate) tile_w: u32,
    pub(crate) tile_h: u32,
}

pub(crate) fn decode_radiance_tile_window(
    mmap: &[u8],
    raster: RadianceRasterLayout,
    params: crate::hdr::decode::RadianceHeaderParams,
    scanline_offsets: &[usize],
    window: RadianceTileWindow,
) -> Result<Vec<f32>, String> {
    let RadianceTileWindow {
        tile_x,
        tile_y,
        tile_w,
        tile_h,
    } = window;
    validate_tile_bounds(raster.width, raster.height, tile_x, tile_y, tile_w, tile_h)?;
    validate_scanline_offsets(raster.outer_len, scanline_offsets)?;
    let mut reader = Cursor::new(mmap);

    let mut scanline = vec![Rgbe8Pixel::default(); raster.inner_len as usize];
    let mut rgba = vec![0.0f32; tile_w as usize * tile_h as usize * 4];

    if raster.is_row_major_top_left() {
        let first_row = tile_y;
        let last_row_exclusive = tile_y + tile_h;
        let inner_len = raster.inner_len as usize;

        if tile_h >= PARALLEL_ROW_THRESHOLD {
            let rows: Result<Vec<Vec<f32>>, String> = (first_row..last_row_exclusive)
                .into_par_iter()
                .map(|row| {
                    decode_row_major_tile_row(
                        mmap,
                        scanline_offsets,
                        inner_len,
                        tile_x,
                        tile_w,
                        row,
                    )
                })
                .collect();
            for (out_row, row_rgba) in rows?.into_iter().enumerate() {
                let base = out_row * tile_w as usize * 4;
                rgba[base..base + row_rgba.len()].copy_from_slice(&row_rgba);
            }
        } else {
            for row in first_row..last_row_exclusive {
                let row_rgba = decode_row_major_tile_row(
                    mmap,
                    scanline_offsets,
                    inner_len,
                    tile_x,
                    tile_w,
                    row,
                )?;
                let out_row = row - tile_y;
                let base = out_row as usize * tile_w as usize * 4;
                rgba[base..base + row_rgba.len()].copy_from_slice(&row_rgba);
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

#[derive(Clone, Copy)]
pub(crate) struct RadiancePreviewRequest<'a> {
    pub(crate) mmap: &'a [u8],
    pub(crate) logical_width: u32,
    pub(crate) logical_height: u32,
    pub(crate) raster: RadianceRasterLayout,
    pub(crate) params: crate::hdr::decode::RadianceHeaderParams,
    pub(crate) scanline_offsets: &'a [usize],
    pub(crate) max_w: u32,
    pub(crate) max_h: u32,
}

pub(crate) fn decode_radiance_sdr_preview(
    request: RadiancePreviewRequest<'_>,
) -> Result<(u32, u32, Vec<u8>), String> {
    let RadiancePreviewRequest {
        mmap,
        logical_width,
        logical_height,
        raster,
        params,
        scanline_offsets,
        max_w,
        max_h,
    } = request;
    let (preview_width, preview_height) =
        preview_dimensions(logical_width, logical_height, max_w, max_h);
    if preview_width == 0 || preview_height == 0 {
        return Err("Radiance SDR preview dimensions must be non-zero".to_string());
    }

    validate_scanline_offsets(raster.outer_len, scanline_offsets)?;
    let metadata = HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb);
    let pixel_count = crate::constants::checked_rgba_buffer_len(
        preview_width as usize,
        preview_height as usize,
    )
    .ok_or_else(|| format!("Radiance SDR preview buffer size overflow for {preview_width}x{preview_height}"))?;
    let mut pixels = Vec::with_capacity(pixel_count);

    if raster.is_row_major_top_left() {
        let inner_len = raster.inner_len as usize;
        let row_ctx = RadiancePreviewRowContext {
            mmap,
            scanline_offsets,
            inner_len,
            preview_width,
            preview_height,
            logical_width,
            logical_height,
        };
        if preview_height >= PARALLEL_ROW_THRESHOLD {
            let rows: Result<Vec<Vec<u8>>, String> = (0..preview_height)
                .into_par_iter()
                .map(|preview_y| {
                    let mut row_rgba = decode_row_major_preview_row(row_ctx, preview_y)?;
                    params.apply_to_pixels(&mut row_rgba);
                    crate::hdr::tiled::tone_map_linear_rgba_f32_row_to_sdr_u8(
                        preview_width,
                        &row_rgba,
                        HdrColorSpace::LinearSrgb,
                        &metadata,
                    )
                })
                .collect();
            for row in rows? {
                pixels.extend_from_slice(&row);
            }
        } else {
            for preview_y in 0..preview_height {
                let mut row_rgba = decode_row_major_preview_row(row_ctx, preview_y)?;
                params.apply_to_pixels(&mut row_rgba);
                let row_u8 = crate::hdr::tiled::tone_map_linear_rgba_f32_row_to_sdr_u8(
                    preview_width,
                    &row_rgba,
                    HdrColorSpace::LinearSrgb,
                    &metadata,
                )?;
                pixels.extend_from_slice(&row_u8);
            }
        }
    } else {
        let inner_len = raster.inner_len as usize;
        let row_ctx = RadiancePreviewRowContext {
            mmap,
            scanline_offsets,
            inner_len,
            preview_width,
            preview_height,
            logical_width,
            logical_height,
        };
        if preview_height >= PARALLEL_ROW_THRESHOLD {
            let rows: Result<Vec<Vec<u8>>, String> = (0..preview_height)
                .into_par_iter()
                .map(|preview_y| {
                    let mut row_rgba =
                        decode_non_row_major_preview_row(row_ctx, raster, preview_y)?;
                    params.apply_to_pixels(&mut row_rgba);
                    crate::hdr::tiled::tone_map_linear_rgba_f32_row_to_sdr_u8(
                        preview_width,
                        &row_rgba,
                        HdrColorSpace::LinearSrgb,
                        &metadata,
                    )
                })
                .collect();
            for row in rows? {
                pixels.extend_from_slice(&row);
            }
        } else {
            for preview_y in 0..preview_height {
                let mut row_rgba = decode_non_row_major_preview_row(row_ctx, raster, preview_y)?;
                params.apply_to_pixels(&mut row_rgba);
                let row_u8 = crate::hdr::tiled::tone_map_linear_rgba_f32_row_to_sdr_u8(
                    preview_width,
                    &row_rgba,
                    HdrColorSpace::LinearSrgb,
                    &metadata,
                )?;
                pixels.extend_from_slice(&row_u8);
            }
        }
    }

    crate::hdr::tiled::finalize_sdr_preview_pixels(preview_width, preview_height, pixels)
}

pub(crate) fn decode_radiance_hdr_preview(
    request: RadiancePreviewRequest<'_>,
) -> Result<HdrImageBuffer, String> {
    let RadiancePreviewRequest {
        mmap,
        logical_width,
        logical_height,
        raster,
        params,
        scanline_offsets,
        max_w,
        max_h,
    } = request;
    let (preview_width, preview_height) =
        preview_dimensions(logical_width, logical_height, max_w, max_h);
    if preview_width == 0 || preview_height == 0 {
        return Err("Radiance HDR preview dimensions must be non-zero".to_string());
    }

    validate_scanline_offsets(raster.outer_len, scanline_offsets)?;
    let rgba_len = crate::constants::checked_rgba_buffer_len(
        preview_width as usize,
        preview_height as usize,
    )
    .ok_or_else(|| format!("Radiance HDR preview buffer size overflow for {preview_width}x{preview_height}"))?;
    let mut rgba = vec![0.0f32; rgba_len];

    if raster.is_row_major_top_left() {
        let inner_len = raster.inner_len as usize;
        let row_ctx = RadiancePreviewRowContext {
            mmap,
            scanline_offsets,
            inner_len,
            preview_width,
            preview_height,
            logical_width,
            logical_height,
        };
        if preview_height >= PARALLEL_ROW_THRESHOLD {
            let rows: Result<Vec<Vec<f32>>, String> = (0..preview_height)
                .into_par_iter()
                .map(|preview_y| decode_row_major_preview_row(row_ctx, preview_y))
                .collect();
            for (preview_y, row_rgba) in rows?.into_iter().enumerate() {
                let base = preview_y * preview_width as usize * 4;
                rgba[base..base + row_rgba.len()].copy_from_slice(&row_rgba);
            }
        } else {
            for preview_y in 0..preview_height {
                let row_rgba = decode_row_major_preview_row(row_ctx, preview_y)?;
                let base = preview_y as usize * preview_width as usize * 4;
                rgba[base..base + row_rgba.len()].copy_from_slice(&row_rgba);
            }
        }
    } else {
        let inner_len = raster.inner_len as usize;
        let row_ctx = RadiancePreviewRowContext {
            mmap,
            scanline_offsets,
            inner_len,
            preview_width,
            preview_height,
            logical_width,
            logical_height,
        };
        if preview_height >= PARALLEL_ROW_THRESHOLD {
            let rows: Result<Vec<Vec<f32>>, String> = (0..preview_height)
                .into_par_iter()
                .map(|preview_y| decode_non_row_major_preview_row(row_ctx, raster, preview_y))
                .collect();
            for (preview_y, row_rgba) in rows?.into_iter().enumerate() {
                let base = preview_y * preview_width as usize * 4;
                rgba[base..base + row_rgba.len()].copy_from_slice(&row_rgba);
            }
        } else {
            for preview_y in 0..preview_height {
                let row_rgba = decode_non_row_major_preview_row(row_ctx, raster, preview_y)?;
                let base = preview_y as usize * preview_width as usize * 4;
                rgba[base..base + row_rgba.len()].copy_from_slice(&row_rgba);
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

fn decode_row_major_preview_row(
    ctx: RadiancePreviewRowContext<'_>,
    preview_y: u32,
) -> Result<Vec<f32>, String> {
    let RadiancePreviewRowContext {
        mmap,
        scanline_offsets,
        inner_len,
        preview_width,
        preview_height,
        logical_width,
        logical_height,
    } = ctx;
    let source_y = preview_sample_coord(preview_y, preview_height, logical_height);
    let mut reader = Cursor::new(mmap);
    reader.set_position(scanline_offsets[source_y as usize] as u64);
    let mut scanline = vec![Rgbe8Pixel::default(); inner_len];
    read_scanline(&mut reader, &mut scanline)?;

    let mut row_rgba = vec![0.0f32; preview_width as usize * 4];
    for preview_x in 0..preview_width {
        let source_x = preview_sample_coord(preview_x, preview_width, logical_width) as usize;
        let rgb = scanline[source_x].to_rgb_f32();
        let base = preview_x as usize * 4;
        row_rgba[base..base + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 1.0]);
    }
    Ok(row_rgba)
}

fn decode_non_row_major_preview_row(
    ctx: RadiancePreviewRowContext<'_>,
    raster: RadianceRasterLayout,
    preview_y: u32,
) -> Result<Vec<f32>, String> {
    let RadiancePreviewRowContext {
        mmap,
        scanline_offsets,
        inner_len,
        preview_width,
        preview_height,
        logical_width,
        logical_height,
    } = ctx;
    let mut reader = Cursor::new(mmap);
    let mut scanline = vec![Rgbe8Pixel::default(); inner_len];
    let mut last_outer_a: Option<u32> = None;
    let mut row_rgba = vec![0.0f32; preview_width as usize * 4];

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
        let base = preview_x as usize * 4;
        row_rgba[base..base + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 1.0]);
    }
    Ok(row_rgba)
}

fn decode_row_major_tile_row(
    mmap: &[u8],
    scanline_offsets: &[usize],
    inner_len: usize,
    tile_x: u32,
    tile_w: u32,
    row: u32,
) -> Result<Vec<f32>, String> {
    let mut reader = Cursor::new(mmap);
    reader.set_position(scanline_offsets[row as usize] as u64);
    let mut scanline = vec![Rgbe8Pixel::default(); inner_len];
    read_scanline(&mut reader, &mut scanline)?;

    let start = tile_x as usize;
    let end = start + tile_w as usize;
    let mut row_rgba = vec![0.0f32; tile_w as usize * 4];
    rgbe_pixels_to_rgba32f(&scanline[start..end], &mut row_rgba);
    Ok(row_rgba)
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
