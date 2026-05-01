// Simple Image Viewer - A high-performance, cross-platform image viewer
// Copyright (C) 2024-2026 Simple Image Viewer Contributors
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

#![allow(dead_code)]

use std::ffi::{CStr, CString, c_char};
use std::path::{Path, PathBuf};
use std::ptr;

use openexr_core_sys as sys;

#[derive(Debug)]
pub(crate) struct OpenExrCoreReadContext {
    path: PathBuf,
    raw: sys::ExrContext,
    part_count: usize,
}

// OpenEXRCore read contexts parse headers up front and are documented as safe
// for concurrent chunk requests when each thread uses its own decode pipeline.
unsafe impl Send for OpenExrCoreReadContext {}
unsafe impl Sync for OpenExrCoreReadContext {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OpenExrCorePartInfo {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) data_window_min: (i32, i32),
    pub(crate) data_window_max: (i32, i32),
    pub(crate) storage: i32,
    pub(crate) chunk_count: u32,
    pub(crate) channels: Vec<OpenExrCoreChannelInfo>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OpenExrCoreChannelInfo {
    pub(crate) name: String,
    pub(crate) pixel_type: i32,
    pub(crate) x_sampling: i32,
    pub(crate) y_sampling: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OpenExrCoreRgbaTile {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) rgba: Vec<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChannelRole {
    Red,
    Green,
    Blue,
    Alpha,
}

impl OpenExrCoreReadContext {
    pub(crate) fn open(path: &Path) -> Result<Self, String> {
        let filename = CString::new(path.to_string_lossy().as_bytes())
            .map_err(|_| format!("EXR path contains an interior NUL: {}", path.display()))?;
        let mut raw = ptr::null_mut();
        exr_result(unsafe { sys::exr_start_read(&mut raw, filename.as_ptr(), ptr::null()) })?;
        if raw.is_null() {
            return Err(format!(
                "OpenEXRCore returned a null context for {}",
                path.display()
            ));
        }

        let mut part_count = 0;
        if let Err(err) =
            exr_result(unsafe { sys::exr_get_count(raw.cast_const(), &mut part_count) })
        {
            let _ = unsafe { sys::exr_finish(&mut raw) };
            return Err(err);
        }
        let part_count = usize::try_from(part_count)
            .map_err(|_| "OpenEXRCore reported a negative part count".to_string())?;

        Ok(Self {
            path: path.to_path_buf(),
            raw,
            part_count,
        })
    }

    pub(crate) fn part_count(&self) -> usize {
        self.part_count
    }

    pub(crate) fn part(&self, part_index: usize) -> Result<OpenExrCorePartInfo, String> {
        if part_index >= self.part_count {
            return Err(format!(
                "EXR part index {part_index} is out of range for {} part(s) in {}",
                self.part_count,
                self.path.display()
            ));
        }

        let part_index =
            i32::try_from(part_index).map_err(|_| "EXR part index exceeds i32".to_string())?;
        let mut storage = 0;
        exr_result(unsafe {
            sys::exr_get_storage(self.raw.cast_const(), part_index, &mut storage)
        })?;

        let mut data_window = sys::ExrAttrBox2i::default();
        exr_result(unsafe {
            sys::exr_get_data_window(self.raw.cast_const(), part_index, &mut data_window)
        })?;
        let width = extent_from_window_axis(data_window.min.x, data_window.max.x, "width")?;
        let height = extent_from_window_axis(data_window.min.y, data_window.max.y, "height")?;

        let mut chunk_count = 0_i32;
        exr_result(unsafe {
            sys::exr_get_chunk_count(self.raw.cast_const(), part_index, &mut chunk_count)
        })?;
        let chunk_count = u32::try_from(chunk_count)
            .map_err(|_| "OpenEXRCore reported a negative chunk count".to_string())?;

        let mut chlist = ptr::null();
        exr_result(unsafe {
            sys::exr_get_channels(self.raw.cast_const(), part_index, &mut chlist)
        })?;
        let channels = copy_channels(chlist)?;

        Ok(OpenExrCorePartInfo {
            width,
            height,
            data_window_min: (data_window.min.x, data_window.min.y),
            data_window_max: (data_window.max.x, data_window.max.y),
            storage,
            chunk_count,
            channels,
        })
    }

    pub(crate) fn extract_scanline_rgba32f_tile(
        &self,
        part_index: usize,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    ) -> Result<OpenExrCoreRgbaTile, String> {
        let part = self.part(part_index)?;
        validate_tile_bounds(part.width, part.height, x, y, width, height)?;

        let part_index =
            i32::try_from(part_index).map_err(|_| "EXR part index exceeds i32".to_string())?;
        let mut rgba = vec![0.0_f32; width as usize * height as usize * 4];
        for alpha in rgba.chunks_exact_mut(4).map(|pixel| &mut pixel[3]) {
            *alpha = 1.0;
        }

        match part.storage {
            sys::EXR_STORAGE_SCANLINE => {
                let mut decoded_starts = std::collections::BTreeSet::new();
                for source_y in y..y + height {
                    let mut chunk = sys::ExrChunkInfo::default();
                    exr_result(unsafe {
                        sys::exr_read_scanline_chunk_info(
                            self.raw.cast_const(),
                            part_index,
                            i32::try_from(source_y)
                                .map_err(|_| "EXR scanline y exceeds i32".to_string())?
                                + part.data_window_min.1,
                            &mut chunk,
                        )
                    })?;
                    if !decoded_starts.insert(chunk.start_y) {
                        continue;
                    }
                    if chunk.height <= 0 || chunk.width <= 0 {
                        continue;
                    }

                    let chunk_origin_x = u32::try_from(chunk.start_x - part.data_window_min.0)
                        .map_err(|_| {
                            "OpenEXRCore chunk start_x is outside data window".to_string()
                        })?;
                    let chunk_origin_y = u32::try_from(chunk.start_y - part.data_window_min.1)
                        .map_err(|_| {
                            "OpenEXRCore chunk start_y is outside data window".to_string()
                        })?;
                    self.decode_chunk_to_tile(
                        part_index,
                        &chunk,
                        (chunk_origin_x, chunk_origin_y),
                        (x, y, width, height),
                        &mut rgba,
                    )?;
                }
            }
            sys::EXR_STORAGE_TILED => {
                let tile_grid = self.tile_grid(part_index)?;
                let start_tile_x = x / tile_grid.tile_width;
                let end_tile_x = (x + width - 1) / tile_grid.tile_width;
                let start_tile_y = y / tile_grid.tile_height;
                let end_tile_y = (y + height - 1) / tile_grid.tile_height;

                for tile_y_index in start_tile_y..=end_tile_y {
                    for tile_x_index in start_tile_x..=end_tile_x {
                        if tile_x_index >= tile_grid.count_x || tile_y_index >= tile_grid.count_y {
                            continue;
                        }
                        let mut chunk = sys::ExrChunkInfo::default();
                        exr_result(unsafe {
                            sys::exr_read_tile_chunk_info(
                                self.raw.cast_const(),
                                part_index,
                                i32::try_from(tile_x_index)
                                    .map_err(|_| "EXR tile x index exceeds i32".to_string())?,
                                i32::try_from(tile_y_index)
                                    .map_err(|_| "EXR tile y index exceeds i32".to_string())?,
                                0,
                                0,
                                &mut chunk,
                            )
                        })?;
                        if chunk.height <= 0 || chunk.width <= 0 {
                            continue;
                        }
                        self.decode_chunk_to_tile(
                            part_index,
                            &chunk,
                            (
                                tile_x_index * tile_grid.tile_width,
                                tile_y_index * tile_grid.tile_height,
                            ),
                            (x, y, width, height),
                            &mut rgba,
                        )?;
                    }
                }
            }
            _ => {
                return Err(
                    "OpenEXRCore tile extraction supports only flat scanline or tiled EXR"
                        .to_string(),
                );
            }
        }

        Ok(OpenExrCoreRgbaTile {
            width,
            height,
            rgba,
        })
    }

    fn tile_grid(&self, part_index: i32) -> Result<OpenExrCoreTileGrid, String> {
        let mut tile_width = 0_i32;
        let mut tile_height = 0_i32;
        exr_result(unsafe {
            sys::exr_get_tile_sizes(
                self.raw.cast_const(),
                part_index,
                0,
                0,
                &mut tile_width,
                &mut tile_height,
            )
        })?;
        let mut count_x = 0_i32;
        let mut count_y = 0_i32;
        exr_result(unsafe {
            sys::exr_get_tile_counts(
                self.raw.cast_const(),
                part_index,
                0,
                0,
                &mut count_x,
                &mut count_y,
            )
        })?;

        Ok(OpenExrCoreTileGrid {
            tile_width: u32::try_from(tile_width)
                .map_err(|_| "OpenEXRCore tile width is invalid".to_string())?,
            tile_height: u32::try_from(tile_height)
                .map_err(|_| "OpenEXRCore tile height is invalid".to_string())?,
            count_x: u32::try_from(count_x)
                .map_err(|_| "OpenEXRCore tile count x is invalid".to_string())?,
            count_y: u32::try_from(count_y)
                .map_err(|_| "OpenEXRCore tile count y is invalid".to_string())?,
        })
    }

    fn decode_chunk_to_tile(
        &self,
        part_index: i32,
        chunk: &sys::ExrChunkInfo,
        chunk_origin: (u32, u32),
        tile: (u32, u32, u32, u32),
        rgba: &mut [f32],
    ) -> Result<(), String> {
        let (tile_x, tile_y, tile_width, tile_height) = tile;
        let chunk_width = usize::try_from(chunk.width)
            .map_err(|_| "OpenEXRCore chunk width is negative".to_string())?;
        let chunk_height = usize::try_from(chunk.height)
            .map_err(|_| "OpenEXRCore chunk height is negative".to_string())?;
        let sample_count = chunk_width
            .checked_mul(chunk_height)
            .ok_or_else(|| "OpenEXRCore chunk sample count overflowed".to_string())?;

        let mut pipeline = sys::ExrDecodePipeline::default();
        exr_result(unsafe {
            sys::exr_decoding_initialize(self.raw.cast_const(), part_index, chunk, &mut pipeline)
        })?;
        let _pipeline_guard = DecodePipelineGuard {
            context: self.raw.cast_const(),
            pipeline: &mut pipeline,
        };

        let (roles, buffers) = {
            let channels = decode_pipeline_channels(&mut pipeline)?;
            let roles: Vec<_> = channels
                .iter()
                .map(|channel| channel_name_to_role(channel.channel_name))
                .collect();
            let mut buffers = vec![Vec::<f32>::new(); channels.len()];
            for (index, channel) in channels.iter_mut().enumerate() {
                if roles[index].is_none() {
                    channel.decode_to_ptr = ptr::null_mut();
                    continue;
                }

                buffers[index] = vec![0.0_f32; sample_count];
                channel.user_bytes_per_element = 4;
                channel.user_data_type = sys::EXR_PIXEL_FLOAT;
                channel.user_pixel_stride = 4;
                channel.user_line_stride = i32::try_from(chunk_width * 4)
                    .map_err(|_| "OpenEXRCore chunk line stride exceeds i32".to_string())?;
                channel.decode_to_ptr = buffers[index].as_mut_ptr().cast::<u8>();
            }
            (roles, buffers)
        };

        exr_result(unsafe {
            sys::exr_decoding_choose_default_routines(
                self.raw.cast_const(),
                part_index,
                &mut pipeline,
            )
        })?;
        exr_result(unsafe {
            sys::exr_decoding_run(self.raw.cast_const(), part_index, &mut pipeline)
        })?;

        let tile_right = tile_x + tile_width;
        let tile_bottom = tile_y + tile_height;
        let (chunk_x, chunk_y) = chunk_origin;
        let copy_start_x = chunk_x.max(tile_x);
        let copy_end_x = (chunk_x + chunk.width as u32).min(tile_right);
        let copy_start_y = chunk_y.max(tile_y);
        let copy_end_y = (chunk_y + chunk.height as u32).min(tile_bottom);

        if copy_start_x >= copy_end_x || copy_start_y >= copy_end_y {
            return Ok(());
        }

        for (channel_index, role) in roles.iter().enumerate() {
            let Some(role) = role else {
                continue;
            };
            let buffer = &buffers[channel_index];
            for source_y in copy_start_y..copy_end_y {
                let src_row = (source_y - chunk_y) as usize;
                let dst_row = (source_y - tile_y) as usize;
                for source_x in copy_start_x..copy_end_x {
                    let src_col = (source_x - chunk_x) as usize;
                    let dst_col = (source_x - tile_x) as usize;
                    let sample = buffer[src_row * chunk_width + src_col];
                    let dest = (dst_row * tile_width as usize + dst_col) * 4;
                    match *role {
                        ChannelRole::Red => rgba[dest] = sample,
                        ChannelRole::Green => rgba[dest + 1] = sample,
                        ChannelRole::Blue => rgba[dest + 2] = sample,
                        ChannelRole::Alpha => rgba[dest + 3] = sample,
                    }
                }
            }
        }

        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OpenExrCoreTileGrid {
    tile_width: u32,
    tile_height: u32,
    count_x: u32,
    count_y: u32,
}

struct DecodePipelineGuard {
    context: sys::ExrConstContext,
    pipeline: *mut sys::ExrDecodePipeline,
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

fn extent_from_window_axis(min: i32, max: i32, label: &str) -> Result<u32, String> {
    let extent = i64::from(max) - i64::from(min) + 1;
    u32::try_from(extent).map_err(|_| format!("EXR data window {label} is invalid: {min}..={max}"))
}

fn validate_tile_bounds(
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

fn decode_pipeline_channels(
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

fn channel_name_to_role(name: *const c_char) -> Option<ChannelRole> {
    if name.is_null() {
        return None;
    }
    let name = unsafe { CStr::from_ptr(name) }.to_string_lossy();
    if name.eq_ignore_ascii_case("R") {
        Some(ChannelRole::Red)
    } else if name.eq_ignore_ascii_case("G") {
        Some(ChannelRole::Green)
    } else if name.eq_ignore_ascii_case("B") {
        Some(ChannelRole::Blue)
    } else if name.eq_ignore_ascii_case("A") {
        Some(ChannelRole::Alpha)
    } else {
        None
    }
}

fn copy_channels(chlist: *const sys::ExrAttrChlist) -> Result<Vec<OpenExrCoreChannelInfo>, String> {
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

fn exr_attr_string_to_string(value: sys::ExrAttrString) -> Result<String, String> {
    if value.str_.is_null() {
        return Ok(String::new());
    }
    let len = usize::try_from(value.length)
        .map_err(|_| "OpenEXRCore returned a negative string length".to_string())?;
    let bytes = unsafe { std::slice::from_raw_parts(value.str_.cast::<u8>(), len) };
    String::from_utf8(bytes.to_vec()).map_err(|err| err.to_string())
}

fn exr_result(result: sys::ExrResult) -> Result<(), String> {
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

#[cfg(test)]
mod tests {
    fn write_test_exr(width: u32, height: u32, suffix: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "simple_image_viewer_openexr_core_{}_{}.exr",
            std::process::id(),
            suffix
        ));
        let pixels: Vec<f32> = (0..width * height)
            .flat_map(|index| {
                let value = index as f32;
                [value, value + 0.1, value + 0.2, 1.0]
            })
            .collect();
        let img = image::ImageBuffer::<image::Rgba<f32>, Vec<f32>>::from_raw(width, height, pixels)
            .expect("build test EXR image");
        image::DynamicImage::ImageRgba32F(img)
            .save_with_format(&path, image::ImageFormat::OpenExr)
            .expect("write test EXR");
        path
    }

    #[test]
    fn openexr_core_read_context_reports_metadata_for_simple_scanline_file() {
        let path = write_test_exr(3, 2, "metadata");

        let context = super::OpenExrCoreReadContext::open(&path).expect("open with OpenEXRCore");
        let part = context.part(0).expect("read first part metadata");

        assert_eq!(context.part_count(), 1);
        assert_eq!(part.width, 3);
        assert_eq!(part.height, 2);
        assert!(part.chunk_count > 0);
        assert!(part.channels.iter().any(|channel| channel.name == "R"));
        assert!(part.channels.iter().any(|channel| channel.name == "G"));
        assert!(part.channels.iter().any(|channel| channel.name == "B"));
        assert!(part.channels.iter().any(|channel| channel.name == "A"));
    }

    #[test]
    fn openexr_core_read_context_extracts_rgba32f_tile_from_simple_scanline_file() {
        let path = write_test_exr(4, 3, "tile");

        let context = super::OpenExrCoreReadContext::open(&path).expect("open with OpenEXRCore");
        let tile = context
            .extract_scanline_rgba32f_tile(0, 1, 1, 2, 2)
            .expect("extract tile with OpenEXRCore");

        assert_eq!(tile.width, 2);
        assert_eq!(tile.height, 2);
        assert_eq!(tile.rgba.len(), 16);

        let expected_indices = [5.0_f32, 6.0, 9.0, 10.0];
        for (pixel, expected) in tile.rgba.chunks_exact(4).zip(expected_indices) {
            assert_eq!(pixel, [expected, expected + 0.1, expected + 0.2, 1.0]);
        }
    }
}
