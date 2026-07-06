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

use super::constants::{checked_rgba_byte_index, checked_rgba_byte_len};

pub(crate) fn apply_orientation_buffer_f32(
    pixels: Vec<f32>,
    w: u32,
    h: u32,
    orientation: u16,
) -> (u32, u32, Vec<f32>) {
    if orientation <= 1 {
        return (w, h, pixels);
    }

    let (out_w, out_h) = if (5..=8).contains(&orientation) {
        (h, w)
    } else {
        (w, h)
    };
    let Some(out_len) = checked_rgba_byte_len(out_w, out_h) else {
        log::warn!(
            "orientation f32 buffer overflow for {out_w}x{out_h}; keeping original orientation"
        );
        return (w, h, pixels);
    };
    let mut out = vec![0.0_f32; out_len];

    for y in 0..h {
        for x in 0..w {
            let (nx, ny) = match orientation {
                2 => (w - 1 - x, y),
                3 => (w - 1 - x, h - 1 - y),
                4 => (x, h - 1 - y),
                5 => (y, x),
                6 => (h - 1 - y, x),
                7 => (h - 1 - y, w - 1 - x),
                8 => (y, w - 1 - x),
                _ => (x, y),
            };
            let Some(src_idx) = checked_rgba_byte_index(y, x, w) else {
                continue;
            };
            let Some(dst_idx) = checked_rgba_byte_index(ny, nx, out_w) else {
                continue;
            };
            if dst_idx + 4 <= out.len() && src_idx + 4 <= pixels.len() {
                out[dst_idx..dst_idx + 4].copy_from_slice(&pixels[src_idx..src_idx + 4]);
            }
        }
    }
    (out_w, out_h, out)
}
fn apply_orientation_rgba8_inner(
    pixels: &[u8],
    w: u32,
    h: u32,
    orientation: u16,
) -> (u32, u32, Vec<u8>) {
    let (out_w, out_h) = if (5..=8).contains(&orientation) {
        (h, w)
    } else {
        (w, h)
    };
    let Some(out_len) = checked_rgba_byte_len(out_w, out_h) else {
        log::warn!(
            "orientation RGBA8 buffer overflow for {out_w}x{out_h}; keeping original orientation"
        );
        return (w, h, pixels.to_vec());
    };
    let mut out = vec![0u8; out_len];

    for y in 0..h {
        for x in 0..w {
            let (nx, ny) = match orientation {
                2 => (w - 1 - x, y),
                3 => (w - 1 - x, h - 1 - y),
                4 => (x, h - 1 - y),
                5 => (y, x),
                6 => (h - 1 - y, x),
                7 => (h - 1 - y, w - 1 - x),
                8 => (y, w - 1 - x),
                _ => (x, y),
            };
            let Some(src_idx) = checked_rgba_byte_index(y, x, w) else {
                continue;
            };
            let Some(dst_idx) = checked_rgba_byte_index(ny, nx, out_w) else {
                continue;
            };
            if dst_idx + 4 <= out.len() && src_idx + 4 <= pixels.len() {
                out[dst_idx..dst_idx + 4].copy_from_slice(&pixels[src_idx..src_idx + 4]);
            }
        }
    }
    (out_w, out_h, out)
}

/// Rotate/flip RGBA8 pixels read from `pixels` without cloning the source buffer.
pub(crate) fn apply_orientation_buffer_from_slice(
    pixels: &[u8],
    w: u32,
    h: u32,
    orientation: u16,
) -> (u32, u32, Vec<u8>) {
    if orientation <= 1 {
        return (w, h, pixels.to_vec());
    }
    apply_orientation_rgba8_inner(pixels, w, h, orientation)
}

pub(crate) fn apply_orientation_buffer(
    pixels: Vec<u8>,
    w: u32,
    h: u32,
    orientation: u16,
) -> (u32, u32, Vec<u8>) {
    if orientation <= 1 {
        return (w, h, pixels);
    }
    apply_orientation_rgba8_inner(&pixels, w, h, orientation)
}
