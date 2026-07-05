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
use crate::hdr::types::{HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat};
use std::collections::HashMap;
use std::sync::Arc;

use rayon::prelude::*;

use crate::hdr::renderer::hdr_to_sdr_rgba8_for_preview;

use super::kind::HdrTiledSource;
use super::validate::validate_rgba32f_len;

const PARALLEL_PREVIEW_ROW_THRESHOLD: u32 = 8;

#[derive(Clone, Copy)]
struct HdrImagePreviewRowSample<'a> {
    rgba_f32: &'a [f32],
    source_width: u32,
    source_height: u32,
    preview_width: u32,
    preview_height: u32,
    color_space: HdrColorSpace,
    metadata: &'a HdrImageMetadata,
}

pub(crate) fn downsample_hdr_image_nearest(
    image: &HdrImageBuffer,
    max_w: u32,
    max_h: u32,
) -> Result<HdrImageBuffer, String> {
    validate_rgba32f_len(image.width, image.height, image.rgba_f32.len())?;
    let (width, height) = preview_dimensions(image.width, image.height, max_w, max_h);
    if width == 0 || height == 0 {
        return Err("HDR preview dimensions must be non-zero".to_string());
    }

    let mut rgba_f32 = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        let src_y = preview_sample_coord(y, height, image.height) as usize;
        for x in 0..width {
            let src_x = preview_sample_coord(x, width, image.width) as usize;
            let offset = (src_y * image.width as usize + src_x) * 4;
            rgba_f32.extend_from_slice(&image.rgba_f32[offset..offset + 4]);
        }
    }

    Ok(HdrImageBuffer {
        width,
        height,
        format: HdrPixelFormat::Rgba32Float,
        color_space: image.color_space,
        metadata: image.metadata.clone(),
        rgba_f32: Arc::new(rgba_f32),
    })
}

pub(crate) fn hdr_preview_from_tiled_source_nearest<S: HdrTiledSource + ?Sized>(
    source: &S,
    max_w: u32,
    max_h: u32,
) -> Result<HdrImageBuffer, String> {
    let (width, height) = preview_dimensions(source.width(), source.height(), max_w, max_h);
    if width == 0 || height == 0 {
        return Err("HDR tiled preview dimensions must be non-zero".to_string());
    }

    let src_ys: Vec<u32> = (0..height)
        .map(|preview_y| preview_sample_coord(preview_y, height, source.height()))
        .collect();
    let source_rows = fetch_tiled_preview_source_rows(source, &src_ys)?;

    let mut rgba_f32 = Vec::with_capacity(width as usize * height as usize * 4);
    for preview_y in 0..height {
        let src_y = src_ys[preview_y as usize];
        let (strip, row_in_strip) = &source_rows[&src_y];
        let row = sample_preview_row_from_source_strip(strip, *row_in_strip, width, source.width());
        rgba_f32.extend(row);
    }

    Ok(HdrImageBuffer {
        width,
        height,
        format: HdrPixelFormat::Rgba32Float,
        color_space: source.color_space(),
        metadata: source.metadata(),
        rgba_f32: Arc::new(rgba_f32),
    })
}

fn fetch_tiled_preview_source_rows<S: HdrTiledSource + ?Sized>(
    source: &S,
    src_ys: &[u32],
) -> Result<HashMap<u32, (Arc<super::buffer::HdrTileBuffer>, u32)>, String> {
    if src_ys.is_empty() {
        return Ok(HashMap::new());
    }

    let mut unique = Vec::<u32>::new();
    for &src_y in src_ys {
        if unique.last().copied() != Some(src_y) {
            unique.push(src_y);
        }
    }

    let mut rows = HashMap::with_capacity(unique.len());
    let mut index = 0;
    while index < unique.len() {
        let start_y = unique[index];
        let mut end_y = start_y;
        while index + 1 < unique.len() && unique[index + 1] == end_y + 1 {
            index += 1;
            end_y = unique[index];
        }
        let strip_height = end_y - start_y + 1;
        let strip = source.extract_tile_rgba32f_arc(0, start_y, source.width(), strip_height)?;
        for src_y in start_y..=end_y {
            rows.insert(src_y, (Arc::clone(&strip), src_y - start_y));
        }
        index += 1;
    }
    Ok(rows)
}

fn sample_preview_row_from_source_strip(
    strip: &super::buffer::HdrTileBuffer,
    row_in_strip: u32,
    preview_width: u32,
    source_width: u32,
) -> Vec<f32> {
    let row_offset = row_in_strip as usize * source_width as usize * 4;
    let row_len = source_width as usize * 4;
    let row = &strip.rgba_f32[row_offset..row_offset + row_len];
    let mut rgba_f32 = Vec::with_capacity(preview_width as usize * 4);
    for preview_x in 0..preview_width {
        let src_x = preview_sample_coord(preview_x, preview_width, source_width) as usize;
        let offset = src_x * 4;
        rgba_f32.extend_from_slice(&row[offset..offset + 4]);
    }
    rgba_f32
}

fn sample_tiled_preview_row<S: HdrTiledSource + ?Sized>(
    source: &S,
    preview_y: u32,
    preview_width: u32,
    preview_height: u32,
    source_rows: &HashMap<u32, (Arc<super::buffer::HdrTileBuffer>, u32)>,
) -> Result<Vec<f32>, String> {
    let src_y = preview_sample_coord(preview_y, preview_height, source.height());
    let (strip, row_in_strip) = source_rows
        .get(&src_y)
        .ok_or_else(|| format!("missing tiled preview source row {src_y}"))?;
    Ok(sample_preview_row_from_source_strip(
        strip,
        *row_in_strip,
        preview_width,
        source.width(),
    ))
}

/// Nearest downsample from an in-memory linear HDR image straight to 8-bit SDR preview pixels.
///
/// Tone-maps row by row so the SDR path never allocates a full `Rgba32Float` preview buffer.
pub(crate) fn sdr_preview_from_hdr_image_nearest(
    image: &HdrImageBuffer,
    max_w: u32,
    max_h: u32,
) -> Result<(u32, u32, Vec<u8>), String> {
    validate_rgba32f_len(image.width, image.height, image.rgba_f32.len())?;
    let (width, height) = preview_dimensions(image.width, image.height, max_w, max_h);
    if width == 0 || height == 0 {
        return Err("SDR preview dimensions must be non-zero".to_string());
    }

    let metadata = image.metadata.clone();
    let color_space = image.color_space;
    let source_width = image.width;
    let source_height = image.height;
    let rgba_f32 = Arc::clone(&image.rgba_f32);
    let row_sample = HdrImagePreviewRowSample {
        rgba_f32: &rgba_f32,
        source_width,
        source_height,
        preview_width: width,
        preview_height: height,
        color_space,
        metadata: &metadata,
    };

    let rows = if height >= PARALLEL_PREVIEW_ROW_THRESHOLD {
        (0..height)
            .into_par_iter()
            .map(|y| sample_hdr_image_preview_row_to_sdr_u8(row_sample, y))
            .collect::<Result<Vec<_>, _>>()?
    } else {
        let mut rows = Vec::with_capacity(height as usize);
        for y in 0..height {
            rows.push(sample_hdr_image_preview_row_to_sdr_u8(row_sample, y)?);
        }
        rows
    };

    let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
    for row in rows {
        pixels.extend_from_slice(&row);
    }
    finalize_sdr_preview_pixels(width, height, pixels)
}

/// Tone-map an already-downsampled linear RGBA f32 buffer to 8-bit SDR preview pixels.
pub(crate) fn sdr_preview_from_linear_rgba32f(
    width: u32,
    height: u32,
    rgba_f32: &[f32],
    color_space: HdrColorSpace,
    metadata: &HdrImageMetadata,
) -> Result<(u32, u32, Vec<u8>), String> {
    validate_rgba32f_len(width, height, rgba_f32.len())?;
    if width == 0 || height == 0 {
        return Err("SDR preview dimensions must be non-zero".to_string());
    }

    let metadata = metadata.clone();
    let rows = if height >= PARALLEL_PREVIEW_ROW_THRESHOLD {
        (0..height)
            .into_par_iter()
            .map(|y| {
                let row_start = y as usize * width as usize * 4;
                let row_end = row_start + width as usize * 4;
                tone_map_linear_rgba_f32_row_to_sdr_u8(
                    width,
                    rgba_f32[row_start..row_end].to_vec(),
                    color_space,
                    &metadata,
                )
            })
            .collect::<Result<Vec<_>, _>>()?
    } else {
        let mut rows = Vec::with_capacity(height as usize);
        for y in 0..height {
            let row_start = y as usize * width as usize * 4;
            let row_end = row_start + width as usize * 4;
            rows.push(tone_map_linear_rgba_f32_row_to_sdr_u8(
                width,
                rgba_f32[row_start..row_end].to_vec(),
                color_space,
                &metadata,
            )?);
        }
        rows
    };

    let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
    for row in rows {
        pixels.extend_from_slice(&row);
    }
    finalize_sdr_preview_pixels(width, height, pixels)
}

/// Build an SDR preview by nearest sampling a tiled HDR source row-by-row.
pub(crate) fn sdr_preview_from_tiled_source_nearest<S: HdrTiledSource + ?Sized>(
    source: &S,
    max_w: u32,
    max_h: u32,
) -> Result<(u32, u32, Vec<u8>), String> {
    let (width, height) = preview_dimensions(source.width(), source.height(), max_w, max_h);
    if width == 0 || height == 0 {
        return Err("SDR tiled preview dimensions must be non-zero".to_string());
    }

    let metadata = source.metadata();
    let color_space = source.color_space();
    let src_ys: Vec<u32> = (0..height)
        .map(|preview_y| preview_sample_coord(preview_y, height, source.height()))
        .collect();
    let source_rows = fetch_tiled_preview_source_rows(source, &src_ys)?;
    let rows = if height >= PARALLEL_PREVIEW_ROW_THRESHOLD {
        (0..height)
            .into_par_iter()
            .map(|preview_y| {
                let row_f32 =
                    sample_tiled_preview_row(source, preview_y, width, height, &source_rows)?;
                tone_map_linear_rgba_f32_row_to_sdr_u8(width, row_f32, color_space, &metadata)
            })
            .collect::<Result<Vec<_>, _>>()?
    } else {
        let mut rows = Vec::with_capacity(height as usize);
        for preview_y in 0..height {
            let row_f32 = sample_tiled_preview_row(source, preview_y, width, height, &source_rows)?;
            rows.push(tone_map_linear_rgba_f32_row_to_sdr_u8(
                width,
                row_f32,
                color_space,
                &metadata,
            )?);
        }
        rows
    };

    let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
    for row in rows {
        pixels.extend_from_slice(&row);
    }
    finalize_sdr_preview_pixels(width, height, pixels)
}

fn sample_hdr_image_preview_row_to_sdr_u8(
    sample: HdrImagePreviewRowSample<'_>,
    preview_y: u32,
) -> Result<Vec<u8>, String> {
    let HdrImagePreviewRowSample {
        rgba_f32,
        source_width,
        source_height,
        preview_width,
        preview_height,
        color_space,
        metadata,
    } = sample;
    let src_y = preview_sample_coord(preview_y, preview_height, source_height) as usize;
    let mut row_f32 = Vec::with_capacity(preview_width as usize * 4);
    for x in 0..preview_width {
        let src_x = preview_sample_coord(x, preview_width, source_width) as usize;
        let offset = (src_y * source_width as usize + src_x) * 4;
        row_f32.extend_from_slice(&rgba_f32[offset..offset + 4]);
    }
    tone_map_linear_rgba_f32_row_to_sdr_u8(preview_width, row_f32, color_space, metadata)
}

/// Tone-map one linear RGBA f32 preview row to 8-bit SDR without a full-image HDR buffer.
pub(crate) fn tone_map_linear_rgba_f32_row_to_sdr_u8(
    row_width: u32,
    row_rgba_f32: Vec<f32>,
    color_space: HdrColorSpace,
    metadata: &HdrImageMetadata,
) -> Result<Vec<u8>, String> {
    let row = HdrImageBuffer {
        width: row_width,
        height: 1,
        format: HdrPixelFormat::Rgba32Float,
        color_space,
        metadata: metadata.clone(),
        rgba_f32: Arc::new(row_rgba_f32),
    };
    hdr_to_sdr_rgba8_for_preview(&row, 0.0)
}

pub(crate) fn finalize_sdr_preview_pixels(
    width: u32,
    height: u32,
    mut pixels: Vec<u8>,
) -> Result<(u32, u32, Vec<u8>), String> {
    make_visible_preview_opaque_if_alpha_is_empty(&mut pixels);
    Ok((width, height, pixels))
}

fn make_visible_preview_opaque_if_alpha_is_empty(pixels: &mut [u8]) {
    if pixels.chunks_exact(4).any(|pixel| pixel[3] != 0) {
        return;
    }

    let has_visible_rgb = pixels
        .chunks_exact(4)
        .any(|pixel| pixel[0] != 0 || pixel[1] != 0 || pixel[2] != 0);
    if !has_visible_rgb {
        return;
    }

    for pixel in pixels.chunks_exact_mut(4) {
        if pixel[0] != 0 || pixel[1] != 0 || pixel[2] != 0 {
            pixel[3] = u8::MAX;
        }
    }
}

pub(crate) fn preview_dimensions(width: u32, height: u32, max_w: u32, max_h: u32) -> (u32, u32) {
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

pub(crate) fn preview_sample_coord(
    preview_coord: u32,
    preview_extent: u32,
    source_extent: u32,
) -> u32 {
    if preview_extent <= 1 {
        return 0;
    }
    ((u64::from(preview_coord) * u64::from(source_extent - 1)) / u64::from(preview_extent - 1))
        as u32
}
