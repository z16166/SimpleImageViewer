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

use super::layout::{RadianceRasterLayout, RadianceScanAxis, RadianceScanSign, Rgbe8Pixel};
use super::rle::{read_scanline, rgbe_pixels_to_rgba32f, skip_scanline};

use std::io::{BufRead, Cursor};
use std::sync::Arc;

use crate::hdr::types::{HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat};

pub(crate) fn build_radiance_scanline_offsets(
    mmap: &[u8],
    data_offset: usize,
    raster: &RadianceRasterLayout,
) -> Result<Vec<usize>, String> {
    let mut reader = Cursor::new(mmap);
    reader.set_position(data_offset as u64);
    let mut offsets = Vec::with_capacity(raster.outer_len as usize);
    for _ in 0..raster.outer_len {
        offsets.push(reader.position() as usize);
        skip_scanline(&mut reader, raster.inner_len as usize)?;
    }
    Ok(offsets)
}

pub(crate) fn validate_scanline_offsets(
    outer_len: u32,
    scanline_offsets: &[usize],
) -> Result<(), String> {
    if scanline_offsets.len() != outer_len as usize {
        return Err(format!(
            "Radiance HDR scanline index has {} chunks, expected {outer_len}",
            scanline_offsets.len()
        ));
    }
    Ok(())
}

pub(crate) fn read_radiance_header<R: BufRead>(
    reader: &mut R,
    params: &mut crate::hdr::decode::RadianceHeaderParams,
) -> Result<RadianceRasterLayout, String> {
    let mut line = String::new();
    reader.read_line(&mut line).map_err(|err| err.to_string())?;
    if line.trim_end() != "#?RADIANCE" {
        return Err("Radiance HDR signature not found".to_string());
    }

    loop {
        line.clear();
        let bytes_read = reader.read_line(&mut line).map_err(|err| err.to_string())?;
        if bytes_read == 0 {
            return Err("EOF in Radiance HDR header".to_string());
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            break;
        }
        if !trimmed.starts_with('#') {
            params.apply_header_line(trimmed);
        }
    }

    line.clear();
    reader.read_line(&mut line).map_err(|err| err.to_string())?;
    parse_radiance_dimensions_line(line.trim())
}

fn parse_axis_size_token(
    tag: &str,
    size_str: &str,
) -> Result<(RadianceScanAxis, RadianceScanSign, u32), String> {
    let b = tag.as_bytes();
    if b.len() < 2 {
        return Err(format!(
            "Invalid Radiance HDR axis token (expected ±X/±Y): {tag}"
        ));
    }
    let sign = match b[0] {
        b'+' => RadianceScanSign::Positive,
        b'-' => RadianceScanSign::Negative,
        _ => {
            return Err(format!("Invalid Radiance HDR axis sign in token: {tag}"));
        }
    };
    let axis = match b[1] {
        b'x' | b'X' => RadianceScanAxis::X,
        b'y' | b'Y' => RadianceScanAxis::Y,
        _ => {
            return Err(format!("Invalid Radiance HDR axis letter in token: {tag}"));
        }
    };
    let size = size_str
        .parse::<u32>()
        .map_err(|err| format!("Invalid Radiance HDR dimension value: {err}"))?;
    if size == 0 {
        return Err("Radiance HDR dimension must be non-zero".to_string());
    }
    Ok((axis, sign, size))
}

/// Four fields: `±Axis size ±Axis size` in any order (`-Y 1024 +X 2048` or `+X 2048 -Y 1024`, etc.).
pub(crate) fn parse_radiance_dimensions_line(line: &str) -> Result<RadianceRasterLayout, String> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() != 4 {
        return Err(format!(
            "Radiance HDR dimensions line must have 4 whitespace-separated fields, got: {line}"
        ));
    }
    let (o_axis, o_sign, o_size) = parse_axis_size_token(parts[0], parts[1])?;
    let (i_axis, i_sign, i_size) = parse_axis_size_token(parts[2], parts[3])?;
    if o_axis == i_axis {
        return Err(format!(
            "Radiance HDR dimensions must use one X and one Y axis: {line}"
        ));
    }
    let (width, height) = match o_axis {
        RadianceScanAxis::X => (o_size, i_size),
        RadianceScanAxis::Y => (i_size, o_size),
    };
    Ok(RadianceRasterLayout {
        width,
        height,
        outer_axis: o_axis,
        outer_sign: o_sign,
        outer_len: o_size,
        inner_axis: i_axis,
        inner_sign: i_sign,
        inner_len: i_size,
    })
}

/// Full image in logical row-major RGBA32F (for small-buffer / `decode_hdr_image` path).
pub(crate) fn decode_radiance_rgba32f_from_mmap(
    mmap: &[u8],
    params_override: Option<crate::hdr::decode::RadianceHeaderParams>,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<HdrImageBuffer, String> {
    let mut params = params_override.unwrap_or_default();
    let mut reader = Cursor::new(mmap);
    let raster = read_radiance_header(&mut reader, &mut params)?;
    let (width, height) = (raster.width, raster.height);
    crate::hdr::decode::validate_hdr_fallback_budget(width, height)?;
    let data_offset = reader.position() as usize;
    let scanline_offsets = build_radiance_scanline_offsets(mmap, data_offset, &raster)?;
    let n = width as usize * height as usize * 4;
    let mut rgba_f32 = vec![0.0f32; n];
    validate_scanline_offsets(raster.outer_len, &scanline_offsets)?;

    let mut file_reader = Cursor::new(mmap);
    let mut scanline = vec![Rgbe8Pixel::default(); raster.inner_len as usize];
    const CANCEL_POLL_ROWS: u32 = 64;

    if raster.is_row_major_top_left() {
        for ly in 0..height {
            if ly % CANCEL_POLL_ROWS == 0 {
                crate::loader::check_decode_cancel_str(cancel)?;
            }
            file_reader.set_position(scanline_offsets[ly as usize] as u64);
            read_scanline(&mut file_reader, &mut scanline)?;
            let row_off = ly as usize * width as usize * 4;
            let row_pixels = width as usize * 4;
            rgbe_pixels_to_rgba32f(
                &scanline[..width as usize],
                &mut rgba_f32[row_off..row_off + row_pixels],
            );
        }
    } else {
        let plan = raster.stride_plan();
        let w = width as usize;
        if plan.outer_major_is_y {
            let mut y = plan.y_start;
            for outer_i in 0..plan.outer_len {
                if outer_i % CANCEL_POLL_ROWS == 0 {
                    crate::loader::check_decode_cancel_str(cancel)?;
                }
                file_reader.set_position(scanline_offsets[outer_i as usize] as u64);
                read_scanline(&mut file_reader, &mut scanline)?;
                let mut x = plan.x_start;
                let row_off = y as usize * w * 4;
                for pixel in scanline.iter().take(plan.inner_len as usize) {
                    let rgb = pixel.to_rgb_f32();
                    let o = row_off + (x as usize) * 4;
                    rgba_f32[o..o + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 1.0]);
                    x += plan.x_step;
                }
                y += plan.y_step;
            }
        } else {
            let mut x = plan.x_start;
            for outer_i in 0..plan.outer_len {
                if outer_i % CANCEL_POLL_ROWS == 0 {
                    crate::loader::check_decode_cancel_str(cancel)?;
                }
                file_reader.set_position(scanline_offsets[outer_i as usize] as u64);
                read_scanline(&mut file_reader, &mut scanline)?;
                let xu = x as usize;
                let mut y = plan.y_start;
                for pixel in scanline.iter().take(plan.inner_len as usize) {
                    let rgb = pixel.to_rgb_f32();
                    let o = ((y as usize) * w + xu) * 4;
                    rgba_f32[o..o + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 1.0]);
                    y += plan.y_step;
                }
                x += plan.x_step;
            }
        }
    }
    params.apply_to_pixels(&mut rgba_f32);

    Ok(HdrImageBuffer {
        width,
        height,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::LinearSrgb,
        metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
        rgba_f32: Arc::new(rgba_f32),
    })
}
