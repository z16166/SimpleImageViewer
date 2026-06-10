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

pub(crate) fn validate_rgba32f_len(
    width: u32,
    height: u32,
    actual_len: usize,
) -> Result<(), String> {
    let expected_len = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .map(|len| len as usize)
        .ok_or_else(|| format!("HDR tiled source dimensions overflow: {width}x{height}"))?;

    if actual_len != expected_len {
        return Err(format!(
            "Malformed HDR tiled source: expected {expected_len} floats for {width}x{height} RGBA, got {actual_len}",
        ));
    }

    Ok(())
}

#[allow(dead_code)]
pub(crate) fn validate_tile_bounds(
    image_width: u32,
    image_height: u32,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
) -> Result<(), String> {
    if width == 0 || height == 0 {
        return Err(format!(
            "HDR tile requires non-zero dimensions, got {width}x{height}"
        ));
    }

    let end_x = x
        .checked_add(width)
        .ok_or_else(|| format!("HDR tile x range overflows: x={x}, width={width}"))?;
    let end_y = y
        .checked_add(height)
        .ok_or_else(|| format!("HDR tile y range overflows: y={y}, height={height}"))?;

    if end_x > image_width || end_y > image_height {
        return Err(format!(
            "HDR tile {x},{y} {width}x{height} exceeds image bounds {image_width}x{image_height}",
        ));
    }

    Ok(())
}
