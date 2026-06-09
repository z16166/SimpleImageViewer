// Simple Image Viewer - A high-performance, cross-platform image viewer
// Copyright (C) 2024-2026 Simple Image Viewer Contributors
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

#![allow(dead_code)]

use std::ffi::{CString, c_void};
use std::path::Path;
use std::ptr;

use memmap2::Mmap;
use openexr_core_sys as sys;

use super::channels::validate_tile_bounds;
use super::types::OpenExrCorePartInfo;

fn imf_mmap_for_path(path: &Path) -> Result<Mmap, String> {
    crate::mmap_util::map_file(path)
}

fn imf_debug_name_cstr(path: &Path) -> Option<CString> {
    sys::imf_io::path_utf8_cstr(path).ok()
}

pub(crate) fn imf_exr_chromaticities_from_path(path: &Path) -> Option<[f32; 8]> {
    let mmap = imf_mmap_for_path(path).ok()?;
    let debug = imf_debug_name_cstr(path);
    let mut out = [0.0_f32; 8];
    let code = unsafe {
        sys::siv_imf_input_file_chromaticities_f32_bytes(
            mmap.as_ptr().cast::<c_void>(),
            mmap.len(),
            debug.as_ref().map_or(ptr::null(), |label| label.as_ptr()),
            out.as_mut_ptr(),
        )
    };
    (code == 0).then_some(out)
}

pub(crate) fn hdr_color_space_from_chromaticities_xy(ch: &[f32; 8]) -> crate::hdr::types::HdrColorSpace {
    if chromaticities_looks_like_aces_ap0(ch) {
        crate::hdr::types::HdrColorSpace::Aces2065_1
    } else {
        crate::hdr::types::HdrColorSpace::LinearSrgb
    }
}

/// OpenEXR `Imf::RGBtoXYZ` + `RgbaYca::computeYw`: weights for `Y = wr*R + wg*G + wb*B` from file
/// `chromaticities` (xy for R, G, B, white). Required for correct Y/Ry/By → RGB when primaries ≠ Rec.709.
pub(crate) fn openexr_luminance_weights_from_chromaticities_xy(ch: &[f32; 8]) -> Option<[f32; 3]> {
    let (rx, ry) = (ch[0], ch[1]);
    let (gx, gy) = (ch[2], ch[3]);
    let (bx, by) = (ch[4], ch[5]);
    let (wx, wy) = (ch[6], ch[7]);
    let y_white = 1.0_f32;

    if wy.abs() <= 1.0 && (wx * y_white).abs() >= wy.abs() * f32::MAX {
        return None;
    }

    let x = wx * y_white / wy;
    let z = (1.0 - wx - wy) * y_white / wy;

    let d = rx * (by - gy) + bx * (gy - ry) + gx * (ry - by);

    let sr_n = x * (by - gy) - gx * (y_white * (by - 1.0) + by * (x + z))
        + bx * (y_white * (gy - 1.0) + gy * (x + z));
    let sg_n = x * (ry - by) + rx * (y_white * (by - 1.0) + by * (x + z))
        - bx * (y_white * (ry - 1.0) + ry * (x + z));
    let sb_n = x * (gy - ry) - rx * (y_white * (gy - 1.0) + gy * (x + z))
        + gx * (y_white * (ry - 1.0) + ry * (x + z));

    if d.abs() < 1.0
        && (sr_n.abs() >= d.abs() * f32::MAX
            || sg_n.abs() >= d.abs() * f32::MAX
            || sb_n.abs() >= d.abs() * f32::MAX)
    {
        return None;
    }

    let sr = sr_n / d;
    let sg = sg_n / d;
    let sb = sb_n / d;

    let m01 = sr * ry;
    let m11 = sg * gy;
    let m21 = sb * by;
    let sum = m01 + m11 + m21;
    if !sum.is_finite() || sum.abs() < f32::EPSILON {
        return None;
    }
    Some([m01 / sum, m11 / sum, m21 / sum])
}

/// Heuristic match for ACES 2065-1 / AP0-style primaries (e.g. OpenEXR `Carrots.exr`).
pub(crate) fn chromaticities_looks_like_aces_ap0(ch: &[f32; 8]) -> bool {
    let (rx, ry, gx, gy, bx, by) = (ch[0], ch[1], ch[2], ch[3], ch[4], ch[5]);
    let green_on_spectral_locus = gy > 0.85 && gx.abs() < 0.06;
    let red_to_the_right = rx > 0.55 && ry < 0.45;
    let blue_slot = (bx < 0.2 && by.abs() < 0.25) || (bx.abs() < 0.05 && by < 0.15);
    green_on_spectral_locus && red_to_the_right && blue_slot
}

pub(crate) fn deep_scanline_flatten_rgba_via_imf(
    path: &Path,
    expected_w: u32,
    expected_h: u32,
) -> Result<Vec<f32>, String> {
    let mmap = imf_mmap_for_path(path)?;
    let debug = imf_debug_name_cstr(path);
    let mut rgba = vec![0.0_f32; expected_w as usize * expected_h as usize * 4];
    let mut w = 0u32;
    let mut h = 0u32;
    let code = unsafe {
        sys::siv_imf_deep_scanline_flatten_rgba_bytes(
            mmap.as_ptr().cast::<c_void>(),
            mmap.len(),
            debug.as_ref().map_or(ptr::null(), |label| label.as_ptr()),
            rgba.as_mut_ptr(),
            rgba.len(),
            &mut w,
            &mut h,
        )
    };
    if code != 0 {
        return Err(format!(
            "IMF deep scanline flatten failed (code {code}) for {}",
            path.display()
        ));
    }
    if w != expected_w || h != expected_h {
        return Err(format!(
            "IMF deep flatten size mismatch: got {w}x{h}, expected {expected_w}x{expected_h} for {}",
            path.display()
        ));
    }
    Ok(rgba)
}

pub(crate) fn is_luminance_chroma_scanline_part(part: &OpenExrCorePartInfo) -> bool {
    if part.storage != sys::EXR_STORAGE_SCANLINE {
        return false;
    }
    let mut has_y = false;
    let mut has_ry = false;
    let mut has_by = false;
    let mut has_rgb = false;
    for channel in &part.channels {
        let name = channel.name.as_str();
        if name.eq_ignore_ascii_case("Y") {
            has_y = true;
        } else if name.eq_ignore_ascii_case("RY") {
            has_ry = true;
        } else if name.eq_ignore_ascii_case("BY") {
            has_by = true;
        } else if name.eq_ignore_ascii_case("R")
            || name.eq_ignore_ascii_case("G")
            || name.eq_ignore_ascii_case("B")
        {
            has_rgb = true;
        }
    }
    has_y && has_ry && has_by && !has_rgb
}

pub(crate) fn rgba_input_scanline_flatten_rgba_via_imf(path: &Path) -> Result<Vec<f32>, String> {
    let mmap = imf_mmap_for_path(path)?;
    let debug = imf_debug_name_cstr(path);
    let mut rgba = vec![0.0_f32; 4];
    let mut w = 0u32;
    let mut h = 0u32;
    let mut code = unsafe {
        sys::siv_imf_rgba_input_scanline_flatten_rgba_bytes(
            mmap.as_ptr().cast::<c_void>(),
            mmap.len(),
            debug.as_ref().map_or(ptr::null(), |label| label.as_ptr()),
            rgba.as_mut_ptr(),
            rgba.len(),
            &mut w,
            &mut h,
        )
    };
    if code == -5 {
        let need = w as usize * h as usize * 4;
        rgba = vec![0.0_f32; need];
        code = unsafe {
            sys::siv_imf_rgba_input_scanline_flatten_rgba_bytes(
                mmap.as_ptr().cast::<c_void>(),
                mmap.len(),
                debug.as_ref().map_or(ptr::null(), |label| label.as_ptr()),
                rgba.as_mut_ptr(),
                rgba.len(),
                &mut w,
                &mut h,
            )
        };
    }
    if code != 0 {
        return Err(format!(
            "IMF RgbaInputFile flatten failed (code {code}) for {}",
            path.display()
        ));
    }
    Ok(rgba)
}

pub(crate) fn extract_rgba32f_tile_from_flat_buffer(
    rgba: &[f32],
    image_width: u32,
    image_height: u32,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
) -> Result<Vec<f32>, String> {
    validate_tile_bounds(image_width, image_height, x, y, width, height)?;
    let row_stride = image_width as usize * 4;
    let mut out = vec![0.0_f32; width as usize * height as usize * 4];
    for row in 0..height {
        let src_y = (y + row) as usize;
        let src_start = src_y * row_stride + x as usize * 4;
        let src_end = src_start + width as usize * 4;
        let dst_start = row as usize * width as usize * 4;
        out[dst_start..dst_start + width as usize * 4].copy_from_slice(&rgba[src_start..src_end]);
    }
    Ok(out)
}

