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

use crate::loader::{DecodedImage, ImageData, TiledImageSource};
use memmap2::Mmap;
use parking_lot::Mutex;
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tiff::decoder::{Decoder, DecodingResult};
use tiff::tags::Tag;

use core_foundation::array::CFArray;
use core_foundation::base::{CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};
use core_graphics::color_space::CGColorSpace;
use core_graphics::context::CGContext;
use core_graphics::image::CGImage;
use foreign_types::ForeignType;

// External link to ImageIO and CoreServices
#[link(name = "ImageIO", kind = "framework")]
#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
fn apply_orientation_buffer(
    pixels: Vec<u8>,
    w: u32,
    h: u32,
    orientation: u32,
) -> (u32, u32, Vec<u8>) {
    if orientation <= 1 {
        return (w, h, pixels);
    }

    let (out_w, out_h) = if orientation >= 5 && orientation <= 8 {
        (h, w)
    } else {
        (w, h)
    };
    let mut out = vec![0u8; (out_w * out_h * 4) as usize];

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
            let src_idx = (y * w + x) as usize * 4;
            let dst_idx = (ny * out_w + nx) as usize * 4;
            if dst_idx + 4 <= out.len() {
                out[dst_idx..dst_idx + 4].copy_from_slice(&pixels[src_idx..src_idx + 4]);
            }
        }
    }
    (out_w, out_h, out)
}

/// Inverse of [`apply_orientation_buffer`]: maps a **display** (logical) pixel to the
/// corresponding **stored** (physical) pixel. `pw` / `ph` are the bitmap dimensions as stored
/// in the file (before EXIF orientation is applied for viewing).
fn exif_display_to_physical_pixel(
    lx: u32,
    ly: u32,
    orientation: u32,
    pw: u32,
    ph: u32,
) -> Option<(u32, u32)> {
    if pw == 0 || ph == 0 {
        return None;
    }
    let (lw, lh) = if (5..=8).contains(&orientation) {
        (ph, pw)
    } else {
        (pw, ph)
    };
    if lx >= lw || ly >= lh {
        return None;
    }
    let (px, py) = match orientation {
        1 => (lx, ly),
        2 => (pw - 1 - lx, ly),
        3 => (pw - 1 - lx, ph - 1 - ly),
        4 => (lx, ph - 1 - ly),
        5 => (ly, lx),
        6 => (ly, ph - 1 - lx),
        7 => (pw - 1 - ly, ph - 1 - lx),
        8 => (pw - 1 - ly, lx),
        _ => (lx, ly),
    };
    if px < pw && py < ph {
        Some((px, py))
    } else {
        None
    }
}

unsafe fn render_cgimage_to_rgba_sync(
    cg_image: &CGImage,
    orientation: u32,
    lw: u32,
    lh: u32,
) -> DecodedImage {
    unsafe {
        let pw = cg_image.width() as u32;
        let ph = cg_image.height() as u32;
        let color_space = CGColorSpace::create_with_name(
            CFString::wrap_under_get_rule(kCGColorSpaceSRGB).as_concrete_TypeRef(),
        )
        .unwrap_or_else(|| CGColorSpace::create_device_rgb());

        let mut context = CGContext::create_bitmap_context(
            None,
            lw as usize,
            lh as usize,
            8,
            lw as usize * 4,
            &color_space,
            core_graphics::base::kCGImageAlphaPremultipliedLast,
        );

        apply_orientation_ctm(&mut context, orientation, lw as f64, lh as f64);

        let rect = core_graphics::geometry::CGRect::new(
            &core_graphics::geometry::CGPoint::new(0.0, 0.0),
            &core_graphics::geometry::CGSize::new(pw as f64, ph as f64),
        );
        context.draw_image(rect, &cg_image);

        DecodedImage::new(lw, lh, context.data().to_vec())
    }
}

