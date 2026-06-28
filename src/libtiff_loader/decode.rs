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
use crate::hdr::types::{
    HdrColorProfile, HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat, HdrReference,
    HdrToneMapSettings, HdrTransferFunction,
};

use super::constants::*;
use libtiff_viewer as lib;
use std::os::raw::c_void;
use std::path::Path;
use std::sync::Arc;

use crate::loader::{DecodedImage, ImageData};

pub(crate) fn get_raw_value(buf: &[u8], idx: usize, bps: u16, format: u16) -> f64 {
    match (bps, format) {
        (16, _) => unsafe {
            std::ptr::read_unaligned(buf.as_ptr().add(idx * 2) as *const u16) as f64
        },
        (32, FORMAT_UINT) => unsafe {
            std::ptr::read_unaligned(buf.as_ptr().add(idx * 4) as *const u32) as f64
        },
        (32, FORMAT_INT) => unsafe {
            std::ptr::read_unaligned(buf.as_ptr().add(idx * 4) as *const i32) as f64
        },
        (32, FORMAT_IEEEFP) => unsafe {
            f32::from_bits(std::ptr::read_unaligned(
                buf.as_ptr().add(idx * 4) as *const u32
            )) as f64
        },
        (64, FORMAT_UINT) => unsafe {
            std::ptr::read_unaligned(buf.as_ptr().add(idx * 8) as *const u64) as f64
        },
        (64, FORMAT_IEEEFP) => unsafe {
            f64::from_bits(std::ptr::read_unaligned(
                buf.as_ptr().add(idx * 8) as *const u64
            ))
        },
        _ => 0.0,
    }
}

#[derive(Clone, Copy)]
pub(crate) struct TiffSampleDecodeParams {
    pub(crate) bps: u16,
    pub(crate) photo: u16,
    pub(crate) format: u16,
    pub(crate) swapped: bool,
    pub(crate) smin: f64,
    pub(crate) smax: f64,
}

#[derive(Clone, Copy)]
pub(crate) struct TiffPaletteMaps {
    pub(crate) r_map: *mut u16,
    pub(crate) g_map: *mut u16,
    pub(crate) b_map: *mut u16,
}

pub(crate) fn process_scanline_contig(
    buf: &[u8],
    rgba_row: &mut [u8],
    width: u32,
    spp: u16,
    params: TiffSampleDecodeParams,
    palette: TiffPaletteMaps,
) {
    let TiffSampleDecodeParams { photo, .. } = params;
    let is_palette = photo == PHOTO_PALETTE;
    for x in 0..width as usize {
        let dst_idx = x * 4;
        let src_sample_offset = x * spp as usize;

        let mut samples = [0u32; 4];
        for (s, sample) in samples.iter_mut().enumerate().take((spp as usize).min(4)) {
            *sample = get_sample_value(buf, src_sample_offset + s, params, is_palette);
        }

        match photo {
            PHOTO_MINISWHITE | PHOTO_MINISBLACK => {
                let v = (if photo == PHOTO_MINISWHITE {
                    255 - samples[0].min(255)
                } else {
                    samples[0].min(255)
                }) as u8;
                rgba_row[dst_idx] = v;
                rgba_row[dst_idx + 1] = v;
                rgba_row[dst_idx + 2] = v;
            }
            PHOTO_RGB => {
                rgba_row[dst_idx] = samples[0] as u8;
                rgba_row[dst_idx + 1] = samples[1] as u8;
                rgba_row[dst_idx + 2] = samples[2] as u8;
                if spp >= 4 {
                    rgba_row[dst_idx + 3] = samples[3] as u8;
                }
            }
            PHOTO_SEPARATED => {
                // Separated (CMYK)
                let c = samples[0] as u8;
                let m = if spp > 1 { samples[1] as u8 } else { 0 };
                let y = if spp > 2 { samples[2] as u8 } else { 0 };
                let k = if spp > 3 { samples[3] as u8 } else { 0 };
                rgba_row[dst_idx] = ((255 - c) as u16 * (255 - k) as u16 / 255) as u8;
                rgba_row[dst_idx + 1] = ((255 - m) as u16 * (255 - k) as u16 / 255) as u8;
                rgba_row[dst_idx + 2] = ((255 - y) as u16 * (255 - k) as u16 / 255) as u8;
                rgba_row[dst_idx + 3] = 255;
            }
            PHOTO_PALETTE => {
                let idx = get_sample_value(buf, x, params, true) as usize;
                unsafe {
                    let r_ptr = palette.r_map.add(idx);
                    let g_ptr = palette.g_map.add(idx);
                    let b_ptr = palette.b_map.add(idx);
                    rgba_row[dst_idx] = (*r_ptr >> 8) as u8;
                    rgba_row[dst_idx + 1] = (*g_ptr >> 8) as u8;
                    rgba_row[dst_idx + 2] = (*b_ptr >> 8) as u8;
                    rgba_row[dst_idx + 3] = 255;
                }
            }
            _ => {}
        }
    }
}

pub(crate) fn process_scanline_separate(
    buf: &[u8],
    rgba_row: &mut [u8],
    width: u32,
    sample_idx: usize,
    params: TiffSampleDecodeParams,
) {
    let TiffSampleDecodeParams { photo, .. } = params;
    let is_palette = photo == PHOTO_PALETTE;
    for x in 0..width as usize {
        let dst_idx = x * 4;
        let val = get_sample_value(buf, x, params, is_palette);

        match photo {
            PHOTO_MINISWHITE | PHOTO_MINISBLACK => {
                let v = if photo == PHOTO_MINISWHITE {
                    255 - val
                } else {
                    val
                } as u8;
                rgba_row[dst_idx] = v;
                rgba_row[dst_idx + 1] = v;
                rgba_row[dst_idx + 2] = v;
            }
            PHOTO_RGB => {
                if sample_idx < 4 {
                    rgba_row[dst_idx + sample_idx] = val as u8;
                }
            }
            PHOTO_SEPARATED => {
                // Separated (CMYK)
                // In planar mode, we need a way to store samples 0,1,2,3 without overwriting RGBA yet.
                // We'll use a trick: since we know we are in manual_decode_scanline, we can assume
                // the caller might have a temporary buffer, but here we only have rgba_row.
                // Let's use the actual samples from get_sample_value.
                // To avoid corruption, we only convert to RGB on the LAST sample.
                // But where to store C, M, Y while waiting for K?
                // Actually, the current manual_decode_scanline for Separate (planar) reads ONE channel
                // for the WHOLE image before moving to the next channel.
                // So when s=0, we write ALL Cyan values to rgba_row[0].
                // This means rgba_row is actually shared across all pixels.
                // Wait! rgba_row is just for ONE scanline.
                // So s=0: writes C to all rgba_row[x*4+0].
                // s=1: writes M to all rgba_row[x*4+1].
                // s=3: reads C,M,Y from rgba_row and converts.
                // THIS WORKS FINE as long as val is stored as u8.
                rgba_row[dst_idx + sample_idx] = val as u8;
                if sample_idx == 3 {
                    let c = rgba_row[dst_idx];
                    let m = rgba_row[dst_idx + 1];
                    let y = rgba_row[dst_idx + 2];
                    let k = rgba_row[dst_idx + 3];
                    rgba_row[dst_idx] = ((255 - c) as u16 * (255 - k) as u16 / 255) as u8;
                    rgba_row[dst_idx + 1] = ((255 - m) as u16 * (255 - k) as u16 / 255) as u8;
                    rgba_row[dst_idx + 2] = ((255 - y) as u16 * (255 - k) as u16 / 255) as u8;
                    rgba_row[dst_idx + 3] = 255;
                }
            }
            PHOTO_LOGL | PHOTO_LOGLUV => {
                // LogL / LogLuv
                let v = val as u8;
                rgba_row[dst_idx] = v;
                rgba_row[dst_idx + 1] = v;
                rgba_row[dst_idx + 2] = v;
                rgba_row[dst_idx + 3] = 255;
            }
            _ => {}
        }
    }
}

fn get_sample_value(
    buf: &[u8],
    idx: usize,
    params: TiffSampleDecodeParams,
    is_palette: bool,
) -> u32 {
    let TiffSampleDecodeParams {
        bps,
        photo,
        format,
        swapped: _swapped,
        smin,
        smax,
    } = params;
    let range = smax - smin;

    // Handle packed bitstreams (1, 2, 4, 6, 10, 12, 14)
    if bps < 16 && bps != 8 && format == FORMAT_UINT {
        let bit_offset = idx * bps as usize;
        let byte_idx = bit_offset / 8;
        let bit_in_byte = bit_offset % 8;

        let mut val: u32 = (buf[byte_idx] as u32) << 16;
        if byte_idx + 1 < buf.len() {
            val |= (buf[byte_idx + 1] as u32) << 8;
        }
        if byte_idx + 2 < buf.len() {
            val |= buf[byte_idx + 2] as u32;
        }

        let shift = 24 - bit_in_byte - bps as usize;
        let mask = (1u32 << bps) - 1;
        let res = (val >> shift) & mask;

        if is_palette {
            return res;
        }

        let linear = if smax > smin && (smin != 0.0 || smax != 1.0) {
            ((res as f64 - smin) / range).clamp(0.0, 1.0) as f32
        } else {
            (res as f32) / ((1u32 << bps) - 1) as f32
        };

        return if photo != PHOTO_SEPARATED {
            to_srgb_8(linear) as u32
        } else {
            (linear * 255.0) as u32
        };
    }

    // High bit depth or floats.
    // CRITICAL: libtiff ALWAYS returns 16, 32, and 64-bit samples in the host's NATIVE byte order!
    // We MUST NOT perform manual swap_bytes on them.
    let f_val: f64 = match (bps, format) {
        (8, _) => buf[idx] as f64,
        (16, _) => unsafe {
            std::ptr::read_unaligned(buf.as_ptr().add(idx * 2) as *const u16) as f64
        },
        (24, _) => {
            // libtiff doesn't natively swap 24-bit arrays. We must swap based on _swapped flag.
            let b0 = buf[idx * 3] as u32;
            let b1 = buf[idx * 3 + 1] as u32;
            let b2 = buf[idx * 3 + 2] as u32;
            let val = if _swapped {
                (b0 << 16) | (b1 << 8) | b2
            } else {
                (b2 << 16) | (b1 << 8) | b0
            };
            val as f64
        }
        (32, 1) => unsafe {
            std::ptr::read_unaligned(buf.as_ptr().add(idx * 4) as *const u32) as f64
        },
        (32, 2) => unsafe {
            std::ptr::read_unaligned(buf.as_ptr().add(idx * 4) as *const i32) as f64
        },
        (32, 3) => unsafe {
            f32::from_bits(std::ptr::read_unaligned(
                buf.as_ptr().add(idx * 4) as *const u32
            )) as f64
        },
        (64, 1) => unsafe {
            std::ptr::read_unaligned(buf.as_ptr().add(idx * 8) as *const u64) as f64
        },
        (64, 3) => unsafe {
            f64::from_bits(std::ptr::read_unaligned(
                buf.as_ptr().add(idx * 8) as *const u64
            ))
        },
        _ => 0.0,
    };

    if (bps == 8 || bps == 16) && is_palette {
        return f_val as u32;
    }

    // Default max for integers if smax is 1.0
    let effective_max = if smax <= 1.0 && format != FORMAT_IEEEFP {
        match bps {
            64 => 18446744073709551615.0,
            32 => 4294967295.0,
            24 => 16777215.0,
            16 => 65535.0,
            8 => 255.0,
            _ => 1.0,
        }
    } else {
        smax
    };

    let effective_range = effective_max - smin;
    let linear = if effective_range > 0.0 {
        ((f_val - smin) / effective_range).clamp(0.0, 1.0) as f32
    } else {
        (f_val - smin).clamp(0.0, 1.0) as f32
    };

    if photo == PHOTO_SEPARATED {
        (linear * 255.0) as u32
    } else {
        to_srgb_8(linear) as u32
    }
}

fn to_srgb_8(linear: f32) -> u8 {
    // Simple sRGB / Gamma 2.2 mapping
    let l = linear.clamp(0.0, 1.0);
    let s = if l <= 0.0031308 {
        12.92 * l
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    };
    (s * 255.0) as u8
}

pub(crate) fn tiff_ieee_scene_linear_eligible(
    sample_format: u16,
    bps: u16,
    photo: u16,
    spp: u16,
) -> bool {
    if sample_format != lib::SAMPLEFORMAT_IEEEFP || !matches!(bps, 16 | 32 | 64) {
        return false;
    }
    match photo {
        PHOTO_RGB => spp == 3 || spp == 4,
        PHOTO_MINISBLACK | PHOTO_MINISWHITE => spp == 1,
        _ => false,
    }
}

/// 16-bit integer RGB TIFF (common for Nikon/camera exports). `TIFFReadRGBAImage` crushes these to
/// 8-bit SDR; preserve headroom via scene-linear float when HDR output is requested.
pub(crate) fn tiff_uint16_rgb_scene_linear_eligible(
    sample_format: u16,
    bps: u16,
    photo: u16,
    spp: u16,
    planar_config: u16,
) -> bool {
    sample_format == lib::SAMPLEFORMAT_UINT
        && bps == 16
        && photo == PHOTO_RGB
        && matches!(spp, 3 | 4)
        && planar_config == CONFIG_CONTIG
}

#[inline]
fn read_uint16_sample(buf: &[u8], sample_index: usize) -> u16 {
    unsafe { std::ptr::read_unaligned(buf.as_ptr().add(sample_index * 2) as *const u16) }
}

pub(crate) fn decode_uint16_rgb_scene_linear_rgba32f(
    tif: *mut lib::TIFF,
    width: u32,
    height: u32,
    mut spp: u16,
) -> Result<Vec<f32>, String> {
    if spp == 0 {
        spp = 3;
    }
    if !matches!(spp, 3 | 4) {
        return Err(format!(
            "16-bit RGB TIFF: expected SamplesPerPixel 3 or 4, got {spp}"
        ));
    }

    let scanline_size = unsafe { lib::TIFFScanlineSize(tif) };
    if scanline_size <= 0 {
        return Err("16-bit RGB TIFF: invalid scanline size".to_string());
    }
    let mut buf = vec![0u8; scanline_size as usize];

    let mut smin = 0.0_f64;
    let mut smax = 65535.0_f64;
    let mut smin_provided = false;
    let mut smax_provided = false;
    unsafe {
        let mut smin_v: f64 = 0.0;
        let mut smax_v: f64 = 0.0;
        if lib::TIFFGetField(tif, lib::TIFFTAG_SMINSAMPLEVALUE, &mut smin_v) != 0 {
            smin = smin_v;
            smin_provided = true;
        }
        if lib::TIFFGetField(tif, lib::TIFFTAG_SMAXSAMPLEVALUE, &mut smax_v) != 0 {
            smax = smax_v;
            smax_provided = true;
        }
    }

    if !smax_provided {
        let mut actual_min = f64::MAX;
        let mut actual_max = f64::MIN;
        for y in 0..height {
            if unsafe { lib::TIFFReadScanline(tif, buf.as_mut_ptr() as *mut c_void, y, 0) } <= 0 {
                return Err(format!(
                    "16-bit RGB TIFF: scan failed at row {y} (min/max pass)"
                ));
            }
            for x in 0..width as usize {
                let base = x * spp as usize;
                for c in 0..3 {
                    let val = read_uint16_sample(&buf, base + c) as f64;
                    actual_min = actual_min.min(val);
                    actual_max = actual_max.max(val);
                }
            }
        }
        if actual_max > actual_min {
            if !smin_provided {
                smin = actual_min;
            }
            smax = actual_max;
        }
    }

    let range = (smax - smin).max(1.0);
    let mut out = vec![0.0_f32; width as usize * height as usize * 4];
    for y in 0..height {
        if unsafe { lib::TIFFReadScanline(tif, buf.as_mut_ptr() as *mut c_void, y, 0) } <= 0 {
            return Err(format!("16-bit RGB TIFF: scan failed at row {y}"));
        }
        let row_off = y as usize * width as usize * 4;
        let row = &mut out[row_off..row_off + width as usize * 4];
        for x in 0..width as usize {
            let base = x * spp as usize;
            let dst = x * 4;
            for c in 0..3 {
                let val = read_uint16_sample(&buf, base + c) as f64;
                row[dst + c] = (((val - smin) / range).clamp(0.0, 1.0)) as f32;
            }
            row[dst + 3] = if spp >= 4 {
                (((read_uint16_sample(&buf, base + 3) as f64 - smin) / range).clamp(0.0, 1.0))
                    as f32
            } else {
                1.0
            };
        }
    }
    Ok(out)
}

/// Promote display-referred 8-bit RGB TIFF pixels to scene-linear float for the HDR render plane.
fn rgba8_to_scene_linear_hdr_buffer(
    width: u32,
    height: u32,
    pixels: &[u8],
) -> Result<HdrImageBuffer, String> {
    if pixels.len() != width as usize * height as usize * 4 {
        return Err("RGBA8 buffer size mismatch".to_string());
    }
    let mut rgba_f32 = Vec::with_capacity(pixels.len());
    for px in pixels.chunks_exact(4) {
        rgba_f32.push(crate::hdr::decode::srgb_nonlinear_channel_to_linear(
            px[0] as f32 / 255.0,
        ));
        rgba_f32.push(crate::hdr::decode::srgb_nonlinear_channel_to_linear(
            px[1] as f32 / 255.0,
        ));
        rgba_f32.push(crate::hdr::decode::srgb_nonlinear_channel_to_linear(
            px[2] as f32 / 255.0,
        ));
        rgba_f32.push(px[3] as f32 / 255.0);
    }
    Ok(HdrImageBuffer {
        width,
        height,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::LinearSrgb,
        metadata: HdrImageMetadata {
            transfer_function: HdrTransferFunction::Linear,
            reference: HdrReference::SceneLinear,
            color_profile: HdrColorProfile::LinearSrgb,
            ..Default::default()
        },
        rgba_f32: Arc::new(rgba_f32),
    })
}

pub(crate) struct CameraTiffHdrUpgrade<'a> {
    pub(crate) path: &'a Path,
    pub(crate) hdr_target_capacity: f32,
    pub(crate) tone_map: &'a HdrToneMapSettings,
    pub(crate) photo: u16,
    pub(crate) bps: u16,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) pixels: &'a [u8],
}

pub(crate) fn try_camera_tiff_rgb8_hdr_upgrade(
    input: CameraTiffHdrUpgrade<'_>,
) -> Result<Option<ImageData>, String> {
    let CameraTiffHdrUpgrade {
        path,
        hdr_target_capacity,
        tone_map,
        photo,
        bps,
        width,
        height,
        pixels,
    } = input;
    if crate::loader::hdr_display_requests_sdr_preview(hdr_target_capacity)
        || photo != PHOTO_RGB
        || bps != 8
        || !crate::loader::tiff_may_be_camera_raw(path)
        || crate::raw_processor::probe_libraw_can_open(path)
    {
        return Ok(None);
    }
    let hdr = rgba8_to_scene_linear_hdr_buffer(width, height, pixels)?;
    let fallback = DecodedImage::from_hdr_sdr_fallback(
        width,
        height,
        crate::loader::hdr_sdr_fallback_rgba8_eager_or_placeholder(
            &hdr,
            hdr_target_capacity,
            tone_map,
        )?,
    );
    Ok(Some(ImageData::Hdr {
        hdr: Box::new(hdr),
        fallback,
    }))
}

#[inline]
fn read_ieee_sample_f32(buf: &[u8], sample_index: usize, bps: u16) -> f32 {
    let v = match bps {
        16 => {
            let bits = unsafe {
                std::ptr::read_unaligned(buf.as_ptr().add(sample_index * 2) as *const u16)
            };
            half::f16::from_bits(bits).to_f32()
        }
        32 => unsafe {
            f32::from_bits(std::ptr::read_unaligned(
                buf.as_ptr().add(sample_index * 4) as *const u32
            ))
        },
        64 => unsafe {
            f64::from_bits(std::ptr::read_unaligned(
                buf.as_ptr().add(sample_index * 8) as *const u64
            )) as f32
        },
        _ => 0.0_f32,
    };
    if v.is_finite() { v } else { 0.0 }
}

/// `TIFFTAG_SMAXSAMPLEVALUE` — libtiff passes this tag as a `double` out-parameter, not `double**`.
unsafe fn tiff_tag_smax_sample_value_f64(tif: *mut lib::TIFF) -> Option<f64> {
    let mut v: f64 = 0.0;
    unsafe {
        if lib::TIFFGetField(tif, lib::TIFFTAG_SMAXSAMPLEVALUE, &mut v) != 0 && v.is_finite() {
            return Some(v);
        }
    }
    None
}

/// Full-image maximum of gray IEEE samples (one sample per column). Used for `PHOTO_MINISWHITE`
/// when `SMaxSampleValue` is absent, so inversion uses one file-consistent reference (not per-row).
fn ieee_grayscale_float_global_max_sample(
    tif: *mut lib::TIFF,
    width: u32,
    height: u32,
    bps: u16,
    buf: &mut [u8],
) -> Result<f32, String> {
    let mut gmax = f32::NEG_INFINITY;
    for y in 0..height {
        if unsafe { lib::TIFFReadScanline(tif, buf.as_mut_ptr() as *mut c_void, y, 0) } <= 0 {
            return Err(format!(
                "IEEE TIFF (white-ref scan): TIFFReadScanline failed at row {y}"
            ));
        }
        for x in 0..width as usize {
            let v = read_ieee_sample_f32(buf, x, bps);
            if v.is_finite() {
                gmax = gmax.max(v);
            }
        }
    }
    Ok(if gmax.is_finite() && gmax > 0.0 {
        gmax
    } else {
        1.0
    })
}

fn resolve_miniswhite_float_white_reference(
    tif: *mut lib::TIFF,
    width: u32,
    height: u32,
    bps: u16,
    buf: &mut [u8],
) -> Result<f32, String> {
    if let Some(mx) = unsafe { tiff_tag_smax_sample_value_f64(tif) }
        && mx > 0.0
    {
        return Ok(mx as f32);
    }
    log::debug!(
        "[libtiff_loader] IEEE MINISWHITE float: SMaxSampleValue unset or non-positive; using image-wide maximum as white reference"
    );
    ieee_grayscale_float_global_max_sample(tif, width, height, bps, buf)
}

/// Scene-linear RGBA (`HdrImageMetadata` linear / scene) from IEEE float TIFF samples. libtiff returns
/// multi-byte samples in **native** byte order — no endian swap here (matches `get_sample_value` rule).
pub(crate) fn decode_ieee_scene_linear_rgba32f(
    tif: *mut lib::TIFF,
    width: u32,
    height: u32,
    bps: u16,
    mut spp: u16,
    photo: u16,
    config: u16,
) -> Result<Vec<f32>, String> {
    if spp == 0 {
        spp = 1;
    }
    if !matches!(bps, 16 | 32 | 64) {
        return Err(format!("IEEE TIFF: unsupported BitsPerSample {bps}"));
    }
    let bytes_per_sample = (bps / 8) as usize;

    let scanline_size = unsafe { lib::TIFFScanlineSize(tif) };
    if scanline_size <= 0 {
        return Err("IEEE TIFF: invalid scanline size".to_string());
    }
    let mut buf = vec![0u8; scanline_size as usize];

    let miniswhite_ref: Option<f32> = if photo == PHOTO_MINISWHITE {
        Some(resolve_miniswhite_float_white_reference(
            tif, width, height, bps, &mut buf,
        )?)
    } else {
        None
    };

    let mut out = vec![0.0_f32; width as usize * height as usize * 4];

    if config == CONFIG_CONTIG {
        for y in 0..height {
            if unsafe { lib::TIFFReadScanline(tif, buf.as_mut_ptr() as *mut c_void, y, 0) } <= 0 {
                return Err(format!("IEEE TIFF: TIFFReadScanline failed at row {y}"));
            }
            let row_off = y as usize * width as usize * 4;
            let row = &mut out[row_off..row_off + width as usize * 4];
            match photo {
                PHOTO_RGB => {
                    for x in 0..width as usize {
                        let dst = x * 4;
                        let base = x * spp as usize;
                        row[dst] = read_ieee_sample_f32(&buf, base, bps);
                        row[dst + 1] = read_ieee_sample_f32(&buf, base + 1, bps);
                        row[dst + 2] = read_ieee_sample_f32(&buf, base + 2, bps);
                        row[dst + 3] = if spp >= 4 {
                            read_ieee_sample_f32(&buf, base + 3, bps)
                        } else {
                            1.0
                        };
                    }
                }
                PHOTO_MINISBLACK => {
                    for x in 0..width as usize {
                        let dst = x * 4;
                        let v = read_ieee_sample_f32(&buf, x, bps);
                        row[dst] = v;
                        row[dst + 1] = v;
                        row[dst + 2] = v;
                        row[dst + 3] = 1.0;
                    }
                }
                PHOTO_MINISWHITE => {
                    let pivot = miniswhite_ref
                        .expect("IEEE HDR: MINISWHITE white reference must be resolved");
                    for x in 0..width as usize {
                        let dst = x * 4;
                        let v = read_ieee_sample_f32(&buf, x, bps);
                        let g = (pivot - v).max(0.0);
                        row[dst] = g;
                        row[dst + 1] = g;
                        row[dst + 2] = g;
                        row[dst + 3] = 1.0;
                    }
                }
                _ => {
                    return Err(format!(
                        "IEEE TIFF: unsupported PhotometricInterpretation {photo}"
                    ));
                }
            }
        }
    } else if config == CONFIG_SEPARATE {
        // One component per TIFFReadScanline sample index (R then G then B, …).
        let comp_count = match photo {
            PHOTO_RGB => spp.min(4) as usize,
            PHOTO_MINISBLACK | PHOTO_MINISWHITE => 1,
            _ => {
                return Err(format!(
                    "IEEE TIFF: planar unsupported for PhotometricInterpretation {photo}"
                ));
            }
        };
        for c in 0..comp_count {
            for y in 0..height {
                if unsafe {
                    lib::TIFFReadScanline(tif, buf.as_mut_ptr() as *mut c_void, y, c as u16)
                } <= 0
                {
                    return Err(format!(
                        "IEEE TIFF: TIFFReadScanline failed plane {c} row {y}"
                    ));
                }
                let row_off = y as usize * width as usize * 4;
                let row = &mut out[row_off..row_off + width as usize * 4];
                match photo {
                    PHOTO_RGB => {
                        for x in 0..width as usize {
                            let dst = x * 4 + c;
                            row[dst] = read_ieee_sample_f32(&buf, x, bps);
                        }
                    }
                    PHOTO_MINISBLACK => {
                        for x in 0..width as usize {
                            let dst = x * 4;
                            let v = read_ieee_sample_f32(&buf, x, bps);
                            row[dst + c] = v;
                        }
                    }
                    PHOTO_MINISWHITE => {
                        let pivot = miniswhite_ref
                            .expect("IEEE HDR: MINISWHITE white reference must be resolved");
                        for x in 0..width as usize {
                            let dst = x * 4;
                            let v = read_ieee_sample_f32(&buf, x, bps);
                            let g = (pivot - v).max(0.0);
                            row[dst + c] = g;
                        }
                    }
                    _ => unreachable!(),
                }
            }
        }
        if photo == PHOTO_RGB && spp < 4 {
            for y in 0..height as usize {
                for x in 0..width as usize {
                    let i = (y * width as usize + x) * 4 + 3;
                    out[i] = 1.0;
                }
            }
        }
        if matches!(photo, PHOTO_MINISBLACK | PHOTO_MINISWHITE) {
            for y in 0..height as usize {
                for x in 0..width as usize {
                    let i = (y * width as usize + x) * 4;
                    let r = out[i];
                    out[i + 1] = r;
                    out[i + 2] = r;
                    out[i + 3] = 1.0;
                }
            }
        }
    } else {
        return Err(format!(
            "IEEE TIFF: unsupported PlanarConfiguration {config}"
        ));
    }

    let expected_min = width as usize * spp as usize * bytes_per_sample;
    if (photo == PHOTO_RGB || photo == PHOTO_MINISBLACK || photo == PHOTO_MINISWHITE)
        && (scanline_size as usize) < expected_min
    {
        log::warn!(
            "[libtiff_loader] IEEE HDR: TIFFScanlineSize={} smaller than width*spp*bps ({}) — file may be malformed",
            scanline_size,
            expected_min
        );
    }

    Ok(out)
}

pub(crate) fn tiff_logl_logluv_hdr_eligible(photo: u16, planar: u16) -> bool {
    matches!(photo, PHOTO_LOGL | PHOTO_LOGLUV) && planar == CONFIG_CONTIG
}

pub(crate) struct LogLuvDecodeParams {
    pub(crate) tif: *mut lib::TIFF,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) photo: u16,
    pub(crate) compression: u16,
    pub(crate) bps: u16,
    pub(crate) spp: u16,
    pub(crate) sample_format: u16,
}

pub(crate) fn decode_logl_logluv_scene_linear_rgba32f(
    params: LogLuvDecodeParams,
) -> Result<Vec<f32>, String> {
    let LogLuvDecodeParams {
        tif,
        width,
        height,
        photo,
        compression,
        bps,
        spp,
        sample_format,
    } = params;
    let mut out = vec![0.0_f32; width as usize * height as usize * 4];
    let scanline_size = unsafe { lib::TIFFScanlineSize(tif) };
    if scanline_size <= 0 {
        return Err("LogL/LogLuv: invalid TIFFScanlineSize".to_string());
    }
    let mut scanline = vec![0u8; scanline_size as usize];

    if photo == PHOTO_LOGLUV {
        if spp != 1 {
            return Err(format!("LogLuv HDR: expected SamplesPerPixel=1, got {spp}"));
        }
        if bps != 32 {
            return Err(format!("LogLuv HDR: expected BitsPerSample=32, got {bps}"));
        }
        let row_bytes = width as usize * 4;
        if (scanline_size as usize) < row_bytes {
            return Err(format!(
                "LogLuv HDR: scanline too short ({} < {})",
                scanline_size, row_bytes
            ));
        }
        for y in 0..height {
            if unsafe { lib::TIFFReadScanline(tif, scanline.as_mut_ptr() as *mut c_void, y, 0) }
                <= 0
            {
                return Err(format!("LogLuv HDR: TIFFReadScanline failed at row {y}"));
            }
            let row_base = y as usize * width as usize * 4;
            for x in 0..width as usize {
                let word = u32::from_ne_bytes([
                    scanline[x * 4],
                    scanline[x * 4 + 1],
                    scanline[x * 4 + 2],
                    scanline[x * 4 + 3],
                ]);
                let rgba = crate::hdr::logluv_decode::logluv_word_to_linear_rgba(compression, word);
                let o = row_base + x * 4;
                out[o..o + 4].copy_from_slice(&rgba);
            }
        }
        return Ok(out);
    }

    if photo == PHOTO_LOGL {
        if spp != 1 {
            return Err(format!("LogL HDR: expected SamplesPerPixel=1, got {spp}"));
        }
        if bps == 16 {
            let row_bytes = width as usize * 2;
            if (scanline_size as usize) < row_bytes {
                return Err(format!(
                    "LogL HDR: 16-bit scanline too short ({} < {})",
                    scanline_size, row_bytes
                ));
            }
            for y in 0..height {
                if unsafe { lib::TIFFReadScanline(tif, scanline.as_mut_ptr() as *mut c_void, y, 0) }
                    <= 0
                {
                    return Err(format!("LogL HDR: TIFFReadScanline failed at row {y}"));
                }
                let row_base = y as usize * width as usize * 4;
                for x in 0..width as usize {
                    let le = i16::from_ne_bytes([scanline[x * 2], scanline[x * 2 + 1]]);
                    let rgba = crate::hdr::logluv_decode::logl_i16_to_linear_rgba(le);
                    let o = row_base + x * 4;
                    out[o..o + 4].copy_from_slice(&rgba);
                }
            }
            return Ok(out);
        }
        if bps == 32 && sample_format == lib::SAMPLEFORMAT_IEEEFP {
            let row_bytes = width as usize * 4;
            if (scanline_size as usize) < row_bytes {
                return Err(format!(
                    "LogL HDR: float scanline too short ({} < {})",
                    scanline_size, row_bytes
                ));
            }
            for y in 0..height {
                if unsafe { lib::TIFFReadScanline(tif, scanline.as_mut_ptr() as *mut c_void, y, 0) }
                    <= 0
                {
                    return Err(format!("LogL HDR: TIFFReadScanline failed at row {y}"));
                }
                let row_base = y as usize * width as usize * 4;
                for x in 0..width as usize {
                    let yv = f32::from_ne_bytes([
                        scanline[x * 4],
                        scanline[x * 4 + 1],
                        scanline[x * 4 + 2],
                        scanline[x * 4 + 3],
                    ]);
                    let rgba = crate::hdr::logluv_decode::logl_f32_y_to_linear_rgba(yv);
                    let o = row_base + x * 4;
                    out[o..o + 4].copy_from_slice(&rgba);
                }
            }
            return Ok(out);
        }
        return Err(format!(
            "LogL HDR: unsupported bps={bps} SampleFormat={sample_format}"
        ));
    }

    Err("not LogL/LogLuv photometric".to_string())
}
