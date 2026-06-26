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

//! Shared downsample helper for directory-tree strip thumbnails.

use std::sync::Arc;

use image::RgbaImage;
use image::imageops::{FilterType, resize};

use crate::loader::DecodedImage;

/// Downsample `decoded` so its long edge fits within `max_side`.
///
/// When this [`DecodedImage`] holds the only [`Arc`] reference to the pixel buffer,
/// [`image::imageops::resize`] receives the buffer without copying (zero-copy).
/// If the buffer is shared, a box-filter downsample reads the borrowed slice
/// without cloning the full-resolution pixel data.
pub(crate) fn downsample_decoded_for_strip(
    decoded: DecodedImage,
    max_side: u32,
) -> Result<DecodedImage, String> {
    let w = decoded.width;
    let h = decoded.height;
    let max_dim = w.max(h);
    if max_dim <= max_side {
        return Ok(decoded);
    }
    let scale = max_side as f32 / max_dim as f32;
    let out_w = ((w as f32 * scale).round() as u32).max(1);
    let out_h = ((h as f32 * scale).round() as u32).max(1);

    let arc = decoded.into_arc_pixels();
    match Arc::try_unwrap(arc) {
        // Unique reference: zero-copy into image::imageops::resize (Triangle filter).
        Ok(vec) => {
            let src = RgbaImage::from_raw(w, h, vec).ok_or_else(|| {
                format!(
                    "DecodedImage dimensions {w}x{h} do not match RGBA buffer size"
                )
            })?;
            let resized = resize(&src, out_w, out_h, FilterType::Triangle);
            Ok(DecodedImage::from(resized))
        }
        // Shared reference: box-filter on the borrowed slice — no clone of the full buffer.
        Err(arc) => {
            let pixels = downsample_rgba8_box(arc.as_slice(), w, h, out_w, out_h);
            Ok(DecodedImage::new(out_w, out_h, pixels))
        }
    }
}

/// Box-filter (area-averaging) RGBA8 downsample.
///
/// Each output pixel is the average of all source pixels whose centres fall within
/// its footprint. Operates on a borrowed `&[u8]` slice so it never copies the
/// source buffer — suitable for shared-[`Arc`] fallback paths where the caller
/// cannot give up its reference.
fn downsample_rgba8_box(
    pixels: &[u8],
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
) -> Vec<u8> {
    debug_assert!(src_w > 0 && src_h > 0 && dst_w > 0 && dst_h > 0);
    let row_stride = src_w as usize * 4;
    let mut out = vec![0_u8; dst_w as usize * dst_h as usize * 4];

    for dst_y in 0..dst_h {
        let src_y0 = (dst_y as u64 * src_h as u64) / dst_h as u64;
        let src_y1 =
            ((dst_y + 1) as u64 * src_h as u64 + dst_h as u64 - 1) / dst_h as u64;
        let src_y1 = src_y1.min(src_h as u64);

        for dst_x in 0..dst_w {
            let src_x0 = (dst_x as u64 * src_w as u64) / dst_w as u64;
            let src_x1 =
                ((dst_x + 1) as u64 * src_w as u64 + dst_w as u64 - 1) / dst_w as u64;
            let src_x1 = src_x1.min(src_w as u64);

            let mut sum_r: u64 = 0;
            let mut sum_g: u64 = 0;
            let mut sum_b: u64 = 0;
            let mut sum_a: u64 = 0;
            let mut count: u64 = 0;

            for sy in src_y0..src_y1 {
                let row_off = sy as usize * row_stride;
                for sx in src_x0..src_x1 {
                    let i = row_off + sx as usize * 4;
                    sum_r += pixels[i] as u64;
                    sum_g += pixels[i + 1] as u64;
                    sum_b += pixels[i + 2] as u64;
                    sum_a += pixels[i + 3] as u64;
                    count += 1;
                }
            }

            let di = (dst_y as usize * dst_w as usize + dst_x as usize) * 4;
            out[di] = (sum_r / count) as u8;
            out[di + 1] = (sum_g / count) as u8;
            out[di + 2] = (sum_b / count) as u8;
            out[di + 3] = (sum_a / count) as u8;
        }
    }
    out
}
