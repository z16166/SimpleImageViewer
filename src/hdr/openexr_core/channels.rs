// Simple Image Viewer - A high-performance, cross-platform image viewer
// Copyright (C) 2024-2026 Simple Image Viewer Contributors
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

#![allow(dead_code)]

use std::ffi::CStr;
use std::sync::Arc;

use openexr_core_sys as sys;

use super::read_context::OpenExrCoreReadContext;
use super::types::{
    ChannelRole, OpenExrCoreChannelInfo, OpenExrCoreDecodedChunk, OpenExrCoreDecodedChunkKey,
};
use super::{
    DEFAULT_DECODED_CHUNK_CACHE_BYTES, MAX_DECODED_CHUNK_CACHE_BYTES,
    SCANLINE_BOOTSTRAP_PREVIEW_MAX_SIDE, SCANLINE_BOOTSTRAP_PREVIEW_SOURCE_ROW_BUDGET,
    SCANLINE_REFINED_PREVIEW_SOURCE_ROW_BUDGET,
};
use std::time::Instant;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct OpenExrCoreChunkDecodeTiming {
    pub(crate) decode_ms: f64,
    pub(crate) copy_ms: f64,
    pub(crate) cache_hit: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct OpenExrCoreDecodedChunkFetch {
    pub(crate) decoded: Arc<OpenExrCoreDecodedChunk>,
    pub(crate) decode_ms: f64,
    pub(crate) cache_hit: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OpenExrCoreTileGrid {
    pub(crate) tile_width: u32,
    pub(crate) tile_height: u32,
    pub(crate) count_x: u32,
    pub(crate) count_y: u32,
}

pub(crate) struct DecodePipelineGuard {
    pub(crate) context: sys::ExrConstContext,
    pub(crate) pipeline: *mut sys::ExrDecodePipeline,
}

impl Drop for DecodePipelineGuard {
    fn drop(&mut self) {
        let _ = unsafe { sys::exr_decoding_destroy(self.context, self.pipeline) };
    }
}

impl Drop for OpenExrCoreReadContext {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            let _ = unsafe { sys::exr_finish(&mut self.raw) };
        }
    }
}

pub(crate) fn extent_from_window_axis(min: i32, max: i32, label: &str) -> Result<u32, String> {
    let extent = i64::from(max) - i64::from(min) + 1;
    u32::try_from(extent).map_err(|_| format!("EXR data window {label} is invalid: {min}..={max}"))
}

pub(crate) fn validate_tile_bounds(
    image_width: u32,
    image_height: u32,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
) -> Result<(), String> {
    if width == 0 || height == 0 {
        return Err("EXR tile dimensions must be non-zero".to_string());
    }
    if x >= image_width || y >= image_height {
        return Err("EXR tile origin is outside the image".to_string());
    }
    if x.checked_add(width).is_none_or(|right| right > image_width)
        || y.checked_add(height)
            .is_none_or(|bottom| bottom > image_height)
    {
        return Err("EXR tile bounds exceed image dimensions".to_string());
    }
    Ok(())
}

pub(crate) fn decoded_chunk_key(
    part_index: i32,
    chunk: &sys::ExrChunkInfo,
    origin: (u32, u32),
) -> Result<OpenExrCoreDecodedChunkKey, String> {
    let width = u32::try_from(chunk.width)
        .map_err(|_| "OpenEXRCore chunk width is negative".to_string())?;
    let height = u32::try_from(chunk.height)
        .map_err(|_| "OpenEXRCore chunk height is negative".to_string())?;
    Ok(OpenExrCoreDecodedChunkKey {
        part_index,
        chunk_index: chunk.idx,
        origin,
        size: (width, height),
    })
}

pub(crate) fn copy_decoded_chunk_to_tile(
    decoded: &OpenExrCoreDecodedChunk,
    tile: (u32, u32, u32, u32),
    rgba: &mut [f32],
) -> Result<f64, String> {
    let (tile_x, tile_y, tile_width, tile_height) = tile;
    let tile_right = tile_x + tile_width;
    let tile_bottom = tile_y + tile_height;
    let (chunk_x, chunk_y) = decoded.origin;
    let copy_start_x = chunk_x.max(tile_x);
    let copy_end_x = (chunk_x + decoded.width).min(tile_right);
    let copy_start_y = chunk_y.max(tile_y);
    let copy_end_y = (chunk_y + decoded.height).min(tile_bottom);

    if copy_start_x >= copy_end_x || copy_start_y >= copy_end_y {
        return Ok(0.0);
    }

    let expected_len = tile_width as usize * tile_height as usize * 4;
    if rgba.len() != expected_len {
        return Err("EXR destination tile buffer has unexpected length".to_string());
    }

    let copy_start = Instant::now();
    let copy_width = (copy_end_x - copy_start_x) as usize;
    let chunk_width = decoded.width as usize;
    let tile_width = tile_width as usize;
    for source_y in copy_start_y..copy_end_y {
        let src_row = (source_y - chunk_y) as usize;
        let dst_row = (source_y - tile_y) as usize;
        let src_col = (copy_start_x - chunk_x) as usize;
        let dst_col = (copy_start_x - tile_x) as usize;
        let src_start = (src_row * chunk_width + src_col) * 4;
        let src_end = src_start + copy_width * 4;
        let dst_start = (dst_row * tile_width + dst_col) * 4;
        let dst_end = dst_start + copy_width * 4;
        rgba[dst_start..dst_end].copy_from_slice(&decoded.rgba[src_start..src_end]);
    }

    Ok(copy_start.elapsed().as_secs_f64() * 1000.0)
}

pub(crate) fn sample_decoded_scanline_chunk_into_preview(
    decoded: &OpenExrCoreDecodedChunk,
    source_width: u32,
    preview_width: u32,
    preview_height: u32,
    preview_rows: &[(u32, u32)],
    rgba: &mut [f32],
) -> Result<(), String> {
    let expected_len = preview_width as usize * preview_height as usize * 4;
    if rgba.len() != expected_len {
        return Err("EXR preview buffer has unexpected length".to_string());
    }

    let (chunk_x, chunk_y) = decoded.origin;
    let chunk_right = chunk_x + decoded.width;
    let chunk_bottom = chunk_y + decoded.height;
    for &(preview_y, source_y) in preview_rows {
        if preview_y >= preview_height {
            return Err("EXR preview row is outside preview bounds".to_string());
        }
        if source_y < chunk_y || source_y >= chunk_bottom {
            continue;
        }
        let source_row = (source_y - chunk_y) as usize;
        for preview_x in 0..preview_width {
            let source_x =
                crate::hdr::tiled::preview_sample_coord(preview_x, preview_width, source_width);
            if source_x < chunk_x || source_x >= chunk_right {
                continue;
            }
            let source_col = (source_x - chunk_x) as usize;
            let source_offset = (source_row * decoded.width as usize + source_col) * 4;
            let dest_offset =
                (preview_y as usize * preview_width as usize + preview_x as usize) * 4;
            rgba[dest_offset..dest_offset + 4]
                .copy_from_slice(&decoded.rgba[source_offset..source_offset + 4]);
        }
    }
    Ok(())
}

pub(crate) fn scanline_preview_dimensions(
    source_width: u32,
    source_height: u32,
    requested_max_w: u32,
    requested_max_h: u32,
) -> (u32, u32) {
    let (max_w, max_h) = if requested_max_w <= crate::constants::DEFAULT_PREVIEW_SIZE
        && requested_max_h <= crate::constants::DEFAULT_PREVIEW_SIZE
    {
        (
            SCANLINE_BOOTSTRAP_PREVIEW_MAX_SIDE,
            SCANLINE_BOOTSTRAP_PREVIEW_MAX_SIDE,
        )
    } else {
        (requested_max_w, requested_max_h)
    };
    crate::hdr::tiled::preview_dimensions(source_width, source_height, max_w, max_h)
}

pub(crate) fn scanline_preview_source_row_budget(requested_preview_width: u32) -> u32 {
    if requested_preview_width <= crate::constants::DEFAULT_PREVIEW_SIZE {
        SCANLINE_BOOTSTRAP_PREVIEW_SOURCE_ROW_BUDGET
    } else {
        SCANLINE_REFINED_PREVIEW_SOURCE_ROW_BUDGET
    }
}

pub(crate) fn scanline_preview_decode_parallelism(unique_chunks: usize) -> usize {
    let cpuses = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let cap = (cpuses * 3 / 4).clamp(16, 32);
    cap.min(unique_chunks.max(1))
}

pub(crate) fn budgeted_scanline_preview_source_y(
    preview_y: u32,
    preview_height: u32,
    source_height: u32,
    max_source_rows: u32,
) -> u32 {
    if preview_height == 0 || source_height == 0 {
        return 0;
    }
    if max_source_rows == 0 || preview_height <= max_source_rows {
        return crate::hdr::tiled::preview_sample_coord(preview_y, preview_height, source_height);
    }

    let bucket = (u64::from(preview_y) * u64::from(max_source_rows) / u64::from(preview_height))
        .min(u64::from(max_source_rows - 1));
    let bucket_start = bucket * u64::from(preview_height) / u64::from(max_source_rows);
    let bucket_end = ((bucket + 1) * u64::from(preview_height) / u64::from(max_source_rows))
        .min(u64::from(preview_height))
        .max(bucket_start + 1);
    let representative_preview_y = ((bucket_start + bucket_end - 1) / 2) as u32;
    crate::hdr::tiled::preview_sample_coord(representative_preview_y, preview_height, source_height)
}

/// Per-channel dimensions and sampling for one decoded chunk (filled by OpenEXRCore on init).
#[derive(Clone, Copy, Debug)]
pub(crate) struct OpenExrCoreChannelChunkLayout {
    pub(crate) width: i32,
    pub(crate) height: i32,
    pub(crate) x_samples: i32,
    pub(crate) y_samples: i32,
}

/// Map a full-resolution chunk pixel to the flat index in that channel's planar buffer.
pub(crate) fn sampled_channel_flat_index(
    layout: OpenExrCoreChannelChunkLayout,
    chunk_origin: (u32, u32),
    col: u32,
    row: u32,
) -> Option<usize> {
    let w = usize::try_from(layout.width).ok()?;
    let h = usize::try_from(layout.height).ok()?;
    if w == 0 || h == 0 {
        return None;
    }
    let xs = layout.x_samples.max(1) as u32;
    let ys = layout.y_samples.max(1) as u32;
    let sub_x = (chunk_origin.0 + col) / xs;
    let sub_y = (chunk_origin.1 + row) / ys;
    let base_x = chunk_origin.0 / xs;
    let base_y = chunk_origin.1 / ys;
    let lx = sub_x.checked_sub(base_x)? as usize;
    let ly = sub_y.checked_sub(base_y)? as usize;
    if lx >= w || ly >= h {
        return None;
    }
    Some(ly * w + lx)
}

pub(crate) fn channel_sample_f32(
    buffers: &[Vec<f32>],
    layouts: &[Option<OpenExrCoreChannelChunkLayout>],
    channel_index: usize,
    chunk_origin: (u32, u32),
    col: u32,
    row: u32,
) -> f32 {
    channel_sample_f32_filtered(
        buffers,
        layouts,
        channel_index,
        chunk_origin,
        col,
        row,
        false,
    )
}

/// Sample a channel at full-resolution `(col, row)`. Subsampled chroma uses bilinear
/// upsampling (OpenEXR stores RY/BY at half resolution; nearest-neighbor skews hue).
pub(crate) fn channel_sample_f32_filtered(
    buffers: &[Vec<f32>],
    layouts: &[Option<OpenExrCoreChannelChunkLayout>],
    channel_index: usize,
    chunk_origin: (u32, u32),
    col: u32,
    row: u32,
    bilinear_subsampled: bool,
) -> f32 {
    let Some(layout) = layouts[channel_index] else {
        return 0.0;
    };
    let w = usize::try_from(layout.width).unwrap_or(0);
    let h = usize::try_from(layout.height).unwrap_or(0);
    if w == 0 || h == 0 {
        return 0.0;
    }
    let xs = layout.x_samples.max(1) as f32;
    let ys = layout.y_samples.max(1) as f32;
    if !bilinear_subsampled || (xs == 1.0 && ys == 1.0) {
        let Some(sample_index) = sampled_channel_flat_index(layout, chunk_origin, col, row) else {
            return 0.0;
        };
        return buffers[channel_index]
            .get(sample_index)
            .copied()
            .unwrap_or(0.0);
    }

    let abs_x = chunk_origin.0 as f32 + col as f32 + 0.5;
    let abs_y = chunk_origin.1 as f32 + row as f32 + 0.5;
    let lx = abs_x / xs - chunk_origin.0 as f32 / xs - 0.5;
    let ly = abs_y / ys - chunk_origin.1 as f32 / ys - 0.5;

    let x0 = lx.floor().max(0.0) as usize;
    let y0 = ly.floor().max(0.0) as usize;
    let x1 = (x0 + 1).min(w.saturating_sub(1));
    let y1 = (y0 + 1).min(h.saturating_sub(1));
    let tx = (lx - x0 as f32).clamp(0.0, 1.0);
    let ty = (ly - y0 as f32).clamp(0.0, 1.0);

    let buf = &buffers[channel_index];
    let at = |x: usize, y: usize| buf.get(y * w + x).copied().unwrap_or(0.0);
    let top = at(x0, y0) * (1.0 - tx) + at(x1, y0) * tx;
    let bot = at(x0, y1) * (1.0 - tx) + at(x1, y1) * tx;
    top * (1.0 - ty) + bot * ty
}

pub(crate) fn decode_pipeline_channels(
    pipeline: &mut sys::ExrDecodePipeline,
) -> Result<&mut [sys::ExrCodingChannelInfo], String> {
    let count = usize::try_from(pipeline.channel_count)
        .map_err(|_| "OpenEXRCore reported a negative decode channel count".to_string())?;
    if count == 0 {
        return Ok(&mut []);
    }
    if pipeline.channels.is_null() {
        return Err("OpenEXRCore returned null decode channel info".to_string());
    }
    Ok(unsafe { std::slice::from_raw_parts_mut(pipeline.channels, count) })
}

pub(crate) fn storage_name(storage: i32) -> &'static str {
    match storage {
        sys::EXR_STORAGE_SCANLINE => "scanline",
        sys::EXR_STORAGE_TILED => "tiled",
        2 => "deep-scanline",
        3 => "deep-tiled",
        _ => "unknown",
    }
}

pub(crate) fn compression_name(compression: u8) -> &'static str {
    match compression {
        0 => "none",
        1 => "rle",
        2 => "zips",
        3 => "zip",
        4 => "piz",
        5 => "pxr24",
        6 => "b44",
        7 => "b44a",
        8 => "dwaa",
        9 => "dwab",
        10 => "htj2k256",
        11 => "htj2k32",
        _ => "unknown",
    }
}

pub(crate) fn assign_channel_roles(
    channels: &[sys::ExrCodingChannelInfo],
) -> Vec<Option<ChannelRole>> {
    let mut roles = vec![None; channels.len()];

    let mut has_r = false;
    let mut has_g = false;
    let mut has_b = false;
    let mut has_y = false;
    let mut has_ry = false;
    let mut has_by = false;
    let mut has_a = false;

    for (i, ch) in channels.iter().enumerate() {
        if ch.channel_name.is_null() {
            continue;
        }
        let name_bytes = unsafe { CStr::from_ptr(ch.channel_name) }.to_bytes();

        let is_suffix = |suffix: &[u8]| {
            name_bytes.len() >= suffix.len()
                && name_bytes[name_bytes.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
                && (name_bytes.len() == suffix.len()
                    || name_bytes[name_bytes.len() - suffix.len() - 1] == b'.')
        };

        if !has_r && (is_suffix(b"R") || is_suffix(b"red")) {
            roles[i] = Some(ChannelRole::Red);
            has_r = true;
        } else if !has_g && (is_suffix(b"G") || is_suffix(b"green")) {
            roles[i] = Some(ChannelRole::Green);
            has_g = true;
        } else if !has_b && (is_suffix(b"B") || is_suffix(b"blue")) {
            roles[i] = Some(ChannelRole::Blue);
            has_b = true;
        } else if !has_y && is_suffix(b"Y") {
            roles[i] = Some(ChannelRole::Luma);
            has_y = true;
        } else if !has_ry && is_suffix(b"RY") {
            roles[i] = Some(ChannelRole::Ry);
            has_ry = true;
        } else if !has_by && is_suffix(b"BY") {
            roles[i] = Some(ChannelRole::By);
            has_by = true;
        } else if !has_a && (is_suffix(b"A") || is_suffix(b"alpha")) {
            roles[i] = Some(ChannelRole::Alpha);
            has_a = true;
        }
    }
    roles
}

pub(crate) fn copy_channels(
    chlist: *const sys::ExrAttrChlist,
) -> Result<Vec<OpenExrCoreChannelInfo>, String> {
    if chlist.is_null() {
        return Err("OpenEXRCore returned a null channel list".to_string());
    }

    let chlist = unsafe { &*chlist };
    let count = usize::try_from(chlist.num_channels)
        .map_err(|_| "OpenEXRCore reported a negative channel count".to_string())?;
    if count == 0 {
        return Ok(Vec::new());
    }
    if chlist.entries.is_null() {
        return Err("OpenEXRCore returned null channel entries".to_string());
    }

    let entries = unsafe { std::slice::from_raw_parts(chlist.entries, count) };
    entries
        .iter()
        .map(|entry| {
            let name = exr_attr_string_to_string(entry.name)?;
            Ok(OpenExrCoreChannelInfo {
                name,
                pixel_type: entry.pixel_type,
                x_sampling: entry.x_sampling,
                y_sampling: entry.y_sampling,
            })
        })
        .collect()
}

pub(crate) fn exr_attr_string_to_string(value: sys::ExrAttrString) -> Result<String, String> {
    if value.str_.is_null() {
        return Ok(String::new());
    }
    let len = usize::try_from(value.length)
        .map_err(|_| "OpenEXRCore returned a negative string length".to_string())?;
    let bytes = unsafe { std::slice::from_raw_parts(value.str_.cast::<u8>(), len) };
    String::from_utf8(bytes.to_vec()).map_err(|err| err.to_string())
}

pub(crate) fn configured_decoded_chunk_cache_max_bytes() -> usize {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    decoded_chunk_cache_budget_for_memory(sys.total_memory() as usize)
}

pub(crate) fn decoded_chunk_cache_budget_for_memory(total_memory_bytes: usize) -> usize {
    (total_memory_bytes / 16).clamp(
        DEFAULT_DECODED_CHUNK_CACHE_BYTES,
        MAX_DECODED_CHUNK_CACHE_BYTES,
    )
}

pub(crate) fn exr_result(result: sys::ExrResult) -> Result<(), String> {
    if result == sys::EXR_ERR_SUCCESS {
        return Ok(());
    }
    let message = unsafe {
        let ptr = sys::exr_get_default_error_message(result);
        if ptr.is_null() {
            "unknown OpenEXRCore error".to_string()
        } else {
            CStr::from_ptr(ptr).to_string_lossy().into_owned()
        }
    };
    Err(format!("OpenEXRCore error {result}: {message}"))
}
