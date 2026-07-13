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
use std::sync::Arc;

use crate::loader::{DecodedImage, ImageData};

#[inline]
fn sample_bytes_span(idx: usize, bps: u16) -> Option<(usize, usize)> {
    let (offset, len) = match bps {
        8 => (idx, 1),
        16 => (idx.checked_mul(2)?, 2),
        24 => (idx.checked_mul(3)?, 3),
        32 => (idx.checked_mul(4)?, 4),
        64 => (idx.checked_mul(8)?, 8),
        _ => return None,
    };
    Some((offset, len))
}

#[inline]
fn sample_bytes_in_buf(buf: &[u8], idx: usize, bps: u16) -> bool {
    match sample_bytes_span(idx, bps) {
        Some((offset, len)) => offset.saturating_add(len) <= buf.len(),
        None => false,
    }
}

/// Minimum byte length of one scanline buffer for contiguous planar configuration.
pub(crate) fn tiff_min_contig_scanline_bytes(width: u32, spp: u16, bps: u16) -> Option<usize> {
    let sample_count = (width as usize).checked_mul(spp as usize)?;
    if bps >= 8 {
        let bytes_per_sample = (bps as usize) / 8;
        if bytes_per_sample == 0 {
            return None;
        }
        sample_count.checked_mul(bytes_per_sample)
    } else {
        sample_count
            .checked_mul(bps as usize)?
            .checked_add(7)
            .map(|bits| bits / 8)
    }
}

/// Minimum byte length of one scanline buffer for separate planar configuration (one plane).
pub(crate) fn tiff_min_separate_scanline_bytes(width: u32, bps: u16) -> Option<usize> {
    if bps >= 8 {
        let bytes_per_sample = (bps as usize) / 8;
        if bytes_per_sample == 0 {
            return None;
        }
        (width as usize).checked_mul(bytes_per_sample)
    } else {
        (width as usize)
            .checked_mul(bps as usize)?
            .checked_add(7)
            .map(|bits| bits / 8)
    }
}

pub(crate) fn ensure_tiff_scanline_size(
    scanline_size: i64,
    width: u32,
    spp: u16,
    bps: u16,
    config: u16,
    context: &str,
) -> Result<(), String> {
    if scanline_size <= 0 {
        return Err(format!("{context}: invalid scanline size"));
    }
    let required = match config {
        CONFIG_CONTIG => tiff_min_contig_scanline_bytes(width, spp, bps),
        CONFIG_SEPARATE => tiff_min_separate_scanline_bytes(width, bps),
        _ => {
            return Err(format!(
                "{context}: unsupported PlanarConfiguration {config}"
            ));
        }
    };
    let Some(required) = required else {
        return Err(format!("{context}: scanline size calculation overflow"));
    };
    if (scanline_size as usize) < required {
        return Err(format!(
            "{context}: TIFFScanlineSize={scanline_size} smaller than required {required} \
             (width={width}, spp={spp}, bps={bps}, config={config})"
        ));
    }
    Ok(())
}

fn checked_rgba32f_len(width: u32, height: u32) -> Result<usize, String> {
    (width as u64)
        .checked_mul(height as u64)
        .and_then(|p| p.checked_mul(4))
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| format!("TIFF output buffer size overflow for {width}x{height}"))
}

pub(crate) fn get_raw_value(buf: &[u8], idx: usize, bps: u16, format: u16) -> f64 {
    if !sample_bytes_in_buf(buf, idx, bps) {
        return 0.0;
    }
    match (bps, format) {
        (16, FORMAT_IEEEFP) => {
            // SAFETY: `sample_bytes_in_buf` verified the sample span lies in `buf`.
            let bits = unsafe { std::ptr::read_unaligned(buf.as_ptr().add(idx * 2) as *const u16) };
            half::f16::from_bits(bits).to_f64()
        }
        (16, _) => {
            // SAFETY: `sample_bytes_in_buf` verified `idx * 2 .. idx * 2 + 2` lies in `buf`.
            // Unaligned read is required because TIFF sample offsets are not guaranteed aligned.
            unsafe { std::ptr::read_unaligned(buf.as_ptr().add(idx * 2) as *const u16) as f64 }
        }
        (32, FORMAT_UINT) => {
            // SAFETY: bounds checked above; unaligned read for packed TIFF samples.
            unsafe { std::ptr::read_unaligned(buf.as_ptr().add(idx * 4) as *const u32) as f64 }
        }
        (32, FORMAT_INT) => {
            // SAFETY: bounds checked above; unaligned read for packed TIFF samples.
            unsafe { std::ptr::read_unaligned(buf.as_ptr().add(idx * 4) as *const i32) as f64 }
        }
        (32, FORMAT_IEEEFP) => {
            // SAFETY: bounds checked above; unaligned read for packed IEEE float samples.
            unsafe {
                f32::from_bits(std::ptr::read_unaligned(
                    buf.as_ptr().add(idx * 4) as *const u32
                )) as f64
            }
        }
        (64, FORMAT_UINT) => {
            // SAFETY: bounds checked above; unaligned read for packed TIFF samples.
            unsafe { std::ptr::read_unaligned(buf.as_ptr().add(idx * 8) as *const u64) as f64 }
        }
        (64, FORMAT_IEEEFP) => {
            // SAFETY: bounds checked above; unaligned read for packed IEEE double samples.
            unsafe {
                f64::from_bits(std::ptr::read_unaligned(
                    buf.as_ptr().add(idx * 8) as *const u64
                ))
            }
        }
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
    /// Number of palette entries (`1 << bps` for indexed color).
    pub(crate) entries: usize,
}

pub(crate) struct TiffLinearScratchStats<'a> {
    pub(crate) actual_min: &'a mut f64,
    pub(crate) actual_max: &'a mut f64,
}

impl TiffLinearScratchStats<'_> {
    #[inline]
    fn record(&mut self, value: f64) {
        *self.actual_min = self.actual_min.min(value);
        *self.actual_max = self.actual_max.max(value);
    }
}

pub(crate) fn process_scanline_contig(
    buf: &[u8],
    rgba_row: &mut [u8],
    width: u32,
    spp: u16,
    params: TiffSampleDecodeParams,
    palette: TiffPaletteMaps,
) {
    if process_rgb8_scanline_contig_fast(buf, rgba_row, width, spp, params) {
        return;
    }

    let dst_len = width as usize * 4;
    if rgba_row.len() < dst_len {
        #[cfg(debug_assertions)]
        log::warn!(
            "[libtiff_loader] process_scanline_contig: rgba_row buffer too small ({} < {})",
            rgba_row.len(),
            dst_len
        );
        return;
    }

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
                if palette.r_map.is_null()
                    || palette.g_map.is_null()
                    || palette.b_map.is_null()
                    || idx >= palette.entries
                {
                    continue;
                }
                // SAFETY: `idx < palette.entries` checked above; colormap pointers come from libtiff
                // and remain valid for the duration of this scanline decode.
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

fn process_rgb8_scanline_contig_fast(
    buf: &[u8],
    rgba_row: &mut [u8],
    width: u32,
    spp: u16,
    params: TiffSampleDecodeParams,
) -> bool {
    let TiffSampleDecodeParams {
        bps,
        photo,
        format,
        smin,
        smax,
        ..
    } = params;
    if photo != PHOTO_RGB
        || bps != 8
        || format != FORMAT_UINT
        || smin != 0.0
        || smax != 255.0
        || !matches!(spp, 3 | 4)
    {
        return false;
    }

    let src_len = width as usize * spp as usize;
    let dst_len = width as usize * 4;
    if buf.len() < src_len || rgba_row.len() < dst_len {
        return false;
    }

    let src = &buf[..src_len];
    let dst = &mut rgba_row[..dst_len];
    if spp == 3 {
        simple_image_viewer::simd_swizzle::interleave_rgb_packed_to_rgba_packed(src, dst);
    } else {
        dst.copy_from_slice(src);
    }
    true
}

pub(crate) fn process_scanline_separate(
    buf: &[u8],
    rgba_row: &mut [u8],
    width: u32,
    sample_idx: usize,
    params: TiffSampleDecodeParams,
) {
    let dst_len = width as usize * 4;
    if rgba_row.len() < dst_len {
        #[cfg(debug_assertions)]
        log::warn!(
            "[libtiff_loader] process_scanline_separate: rgba_row buffer too small ({} < {})",
            rgba_row.len(),
            dst_len
        );
        return;
    }

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
        if byte_idx >= buf.len() {
            return 0;
        }
        let bits_end = bit_offset + bps as usize;
        let last_byte_needed = bits_end.saturating_sub(1) / 8;
        if last_byte_needed >= buf.len() {
            return 0;
        }
        let bit_in_byte = bit_offset % 8;
        if bit_in_byte + bps as usize > 24 {
            return 0;
        }

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
        (8, _) => {
            if idx >= buf.len() {
                0.0
            } else {
                buf[idx] as f64
            }
        }
        (16, _) => {
            if !sample_bytes_in_buf(buf, idx, bps) {
                0.0
            } else {
                unsafe { std::ptr::read_unaligned(buf.as_ptr().add(idx * 2) as *const u16) as f64 }
            }
        }
        (24, _) => {
            if !sample_bytes_in_buf(buf, idx, bps) {
                0.0
            } else {
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
        }
        (32, 1) => {
            if !sample_bytes_in_buf(buf, idx, bps) {
                0.0
            } else {
                unsafe { std::ptr::read_unaligned(buf.as_ptr().add(idx * 4) as *const u32) as f64 }
            }
        }
        (32, 2) => {
            if !sample_bytes_in_buf(buf, idx, bps) {
                0.0
            } else {
                unsafe { std::ptr::read_unaligned(buf.as_ptr().add(idx * 4) as *const i32) as f64 }
            }
        }
        (32, 3) => {
            if !sample_bytes_in_buf(buf, idx, bps) {
                0.0
            } else {
                unsafe {
                    f32::from_bits(std::ptr::read_unaligned(
                        buf.as_ptr().add(idx * 4) as *const u32
                    )) as f64
                }
            }
        }
        (64, 1) => {
            if !sample_bytes_in_buf(buf, idx, bps) {
                0.0
            } else {
                unsafe { std::ptr::read_unaligned(buf.as_ptr().add(idx * 8) as *const u64) as f64 }
            }
        }
        (64, 3) => {
            if !sample_bytes_in_buf(buf, idx, bps) {
                0.0
            } else {
                unsafe {
                    f64::from_bits(std::ptr::read_unaligned(
                        buf.as_ptr().add(idx * 8) as *const u64
                    ))
                }
            }
        }
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
    static SRGB_ENCODE_LUT: std::sync::LazyLock<[u8; 256]> = std::sync::LazyLock::new(|| {
        let mut lut = [0_u8; 256];
        for (i, slot) in lut.iter_mut().enumerate() {
            let l = i as f32 / 255.0;
            let s = if l <= 0.0031308 {
                12.92 * l
            } else {
                1.055 * l.powf(1.0 / 2.4) - 0.055
            };
            *slot = (s * 255.0).round() as u8;
        }
        lut
    });
    let index = (linear.clamp(0.0, 1.0) * 255.0).round() as usize;
    SRGB_ENCODE_LUT[index.min(255)]
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
    if !sample_bytes_in_buf(buf, sample_index, 16) {
        return 0;
    }
    // SAFETY: `sample_bytes_in_buf` verified `sample_index * 2 .. +2` lies in `buf`.
    // Unaligned read is required because TIFF scanlines are byte-packed.
    unsafe { std::ptr::read_unaligned(buf.as_ptr().add(sample_index * 2) as *const u16) }
}

#[inline]
pub(crate) fn tiff_uint_default_sample_max(bps: u16) -> f64 {
    match bps {
        64 => 18446744073709551615.0,
        32 => 4294967295.0,
        24 => 16777215.0,
        16 => 65535.0,
        8 => 255.0,
        _ => 1.0,
    }
}

/// Re-normalize scene-linear f32 pixels after a provisional full-range decode.
fn rescale_scene_linear_rgba32f(
    out: &mut [f32],
    provisional_smin: f64,
    provisional_smax: f64,
    final_smin: f64,
    final_smax: f64,
    rescale_alpha: bool,
) {
    let provisional_range = (provisional_smax - provisional_smin).max(1.0);
    let final_range = (final_smax - final_smin).max(1.0);
    if (final_smin - provisional_smin).abs() <= 1.0 && (final_smax - provisional_smax).abs() <= 1.0
    {
        return;
    }
    let scale = (provisional_range / final_range) as f32;
    let offset = ((provisional_smin - final_smin) / final_range) as f32;
    let channels = if rescale_alpha { 4 } else { 3 };
    for px in out.chunks_exact_mut(4) {
        for channel in px.iter_mut().take(channels) {
            *channel = (*channel * scale + offset).clamp(0.0, 1.0);
        }
    }
}

pub(crate) fn decode_uint16_rgb_scene_linear_rgba32f(
    tif: *mut lib::TIFF,
    width: u32,
    height: u32,
    mut spp: u16,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<Vec<f32>, String> {
    if spp == 0 {
        spp = 3;
    }
    if !matches!(spp, 3 | 4) {
        return Err(format!(
            "16-bit RGB TIFF: expected SamplesPerPixel 3 or 4, got {spp}"
        ));
    }

    // SAFETY: `tif` is a valid libtiff handle opened by this loader; TIFFScanlineSize is read-only.
    let scanline_size = unsafe { lib::TIFFScanlineSize(tif) };
    ensure_tiff_scanline_size(
        scanline_size,
        width,
        spp,
        16,
        CONFIG_CONTIG,
        "16-bit RGB TIFF",
    )?;
    let mut buf = vec![0u8; scanline_size as usize];

    let mut smin = 0.0_f64;
    let mut smax = 65535.0_f64;
    let mut smin_provided = false;
    let mut smax_provided = false;
    // SAFETY: `tif` is a valid libtiff handle; TIFFGetField only writes tag out-parameters.
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

    let provisional_smin = if smin_provided { smin } else { 0.0 };
    let provisional_smax = if smax_provided {
        smax
    } else {
        tiff_uint_default_sample_max(16)
    };
    let provisional_range = (provisional_smax - provisional_smin).max(1.0);

    let mut actual_min = f64::MAX;
    let mut actual_max = f64::MIN;
    let mut out = vec![0.0_f32; checked_rgba32f_len(width, height)?];
    for y in 0..height {
        super::constants::poll_tiff_scanline_cancel(cancel, y)?;
        // SAFETY: `tif` is valid and exclusive; `buf` is sized to `TIFFScanlineSize(tif)`.
        if unsafe { lib::TIFFReadScanline(tif, buf.as_mut_ptr() as *mut c_void, y, 0) } <= 0 {
            return Err(format!("16-bit RGB TIFF: scan failed at row {y}"));
        }
        let row_off = y as usize * width as usize * 4;
        let row = &mut out[row_off..row_off + width as usize * 4];
        let inv_range = (1.0 / provisional_range) as f32;
        let smin_f32 = provisional_smin as f32;
        if smax_provided {
            simple_image_viewer::simd_pixel_convert::normalize_uint16_rgb_scanline_to_rgba32f(
                &buf,
                row,
                width as usize,
                spp as usize,
                smin_f32,
                inv_range,
            );
        } else {
            for x in 0..width as usize {
                let src_base = x * spp as usize;
                let dst_base = x * 4;
                for c in 0..3 {
                    let sample = read_uint16_sample(&buf, src_base + c);
                    let val = sample as f64;
                    actual_min = actual_min.min(val);
                    actual_max = actual_max.max(val);
                    row[dst_base + c] = ((sample as f32 - smin_f32) * inv_range).clamp(0.0, 1.0);
                }
                row[dst_base + 3] = if spp >= 4 {
                    let sample = read_uint16_sample(&buf, src_base + 3);
                    let val = sample as f64;
                    actual_min = actual_min.min(val);
                    actual_max = actual_max.max(val);
                    ((sample as f32 - smin_f32) * inv_range).clamp(0.0, 1.0)
                } else {
                    1.0
                };
            }
        }
    }

    if !smax_provided && actual_max > actual_min {
        let final_smin = if smin_provided { smin } else { actual_min };
        let final_smax = actual_max;
        rescale_scene_linear_rgba32f(
            &mut out,
            provisional_smin,
            provisional_smax,
            final_smin,
            final_smax,
            spp >= 4,
        );
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
    rgba_f32.resize(pixels.len(), 0.0);
    simple_image_viewer::simd_pixel_convert::srgb8_rgba_to_scene_linear_f32(pixels, &mut rgba_f32);
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
    pub(crate) file_bytes: &'a [u8],
    pub(crate) hdr_target_capacity: f32,
    #[allow(dead_code)]
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
        file_bytes,
        hdr_target_capacity,
        tone_map: _,
        photo,
        bps,
        width,
        height,
        pixels,
    } = input;
    if crate::loader::hdr_display_requests_sdr_preview(hdr_target_capacity)
        || photo != PHOTO_RGB
        || bps != 8
        || !crate::loader::tiff_may_be_camera_raw_bytes(file_bytes)
    {
        return Ok(None);
    }
    if crate::loader::tiff_ifd0_suggests_libraw_raw(file_bytes)
        && crate::raw_processor::probe_libraw_can_open_bytes(file_bytes)
    {
        return Ok(None);
    }
    let hdr = rgba8_to_scene_linear_hdr_buffer(width, height, pixels)?;
    let fallback = DecodedImage::from_hdr_sdr_fallback(
        width,
        height,
        crate::loader::hdr_sdr_fallback_rgba8_or_placeholder(&hdr)?,
    );
    Ok(Some(ImageData::Hdr {
        hdr: Box::new(hdr),
        fallback,
    }))
}

#[inline]
fn read_ieee_sample_f32(buf: &[u8], sample_index: usize, bps: u16) -> f32 {
    if !sample_bytes_in_buf(buf, sample_index, bps) {
        return 0.0;
    }
    let v = match bps {
        16 => {
            // SAFETY: `sample_bytes_in_buf` verified the sample span lies in `buf`.
            let bits = unsafe {
                std::ptr::read_unaligned(buf.as_ptr().add(sample_index * 2) as *const u16)
            };
            half::f16::from_bits(bits).to_f32()
        }
        32 => {
            // SAFETY: bounds checked above; unaligned read for packed IEEE float samples.
            unsafe {
                f32::from_bits(std::ptr::read_unaligned(
                    buf.as_ptr().add(sample_index * 4) as *const u32
                ))
            }
        }
        64 => {
            // SAFETY: bounds checked above; unaligned read for packed IEEE double samples.
            unsafe {
                f64::from_bits(std::ptr::read_unaligned(
                    buf.as_ptr().add(sample_index * 8) as *const u64
                )) as f32
            }
        }
        _ => 0.0_f32,
    };
    if v.is_finite() { v } else { 0.0 }
}

/// `TIFFTAG_SMAXSAMPLEVALUE` — libtiff passes this tag as a `double` out-parameter, not `double**`.
///
/// # Safety
/// `tif` must be a valid libtiff handle.
unsafe fn tiff_tag_smax_sample_value_f64(tif: *mut lib::TIFF) -> Option<f64> {
    let mut v: f64 = 0.0;
    // SAFETY: read-only tag query; `v` is a stack out-parameter.
    unsafe {
        if lib::TIFFGetField(tif, lib::TIFFTAG_SMAXSAMPLEVALUE, &mut v) != 0 && v.is_finite() {
            return Some(v);
        }
    }
    None
}

fn ieee_grayscale_float_white_reference_from_max(gmax: f32) -> f32 {
    if gmax.is_finite() && gmax > 0.0 {
        gmax
    } else {
        1.0
    }
}

/// Apply `(pivot - v).max(0)` after a single I/O pass that stored raw gray samples in `out`.
fn finalize_miniswhite_float_inversion(out: &mut [f32], width: u32, height: u32, pivot: f32) {
    simple_image_viewer::simd_pixel_convert::invert_miniswhite_rgba32f(
        out,
        width as usize,
        height as usize,
        pivot,
    );
}

/// Store raw high-depth samples (integer or IEEE float) in a row of `scratch` (RGBA layout)
/// and track file-wide min/max during the single scanline read pass.
pub(crate) fn write_contig_scanline_linear_scratch(
    buf: &[u8],
    scratch_row: &mut [f32],
    width: u32,
    spp: u16,
    params: TiffSampleDecodeParams,
    stats: &mut TiffLinearScratchStats<'_>,
) {
    let TiffSampleDecodeParams { bps, format, .. } = params;
    for x in 0..width as usize {
        let dst_idx = x * 4;
        for s in 0..(spp as usize).min(4) {
            let idx = x * spp as usize + s;
            let val = get_raw_value(buf, idx, bps, format);
            if val.is_finite() {
                stats.record(val);
                scratch_row[dst_idx + s] = val as f32;
            }
        }
    }
}

/// Store one high-depth plane in a row of `scratch` (RGBA layout) and track file-wide min/max.
pub(crate) fn write_separate_scanline_linear_scratch(
    buf: &[u8],
    scratch_row: &mut [f32],
    width: u32,
    sample_idx: usize,
    params: TiffSampleDecodeParams,
    stats: &mut TiffLinearScratchStats<'_>,
) {
    let TiffSampleDecodeParams { bps, format, .. } = params;
    if sample_idx >= 4 {
        return;
    }
    for x in 0..width as usize {
        let val = get_raw_value(buf, x, bps, format);
        if val.is_finite() {
            stats.record(val);
            scratch_row[x * 4 + sample_idx] = val as f32;
        }
    }
}

/// Normalize deferred linear scratch (single I/O pass) into display RGBA8.
pub(crate) fn finalize_linear_scratch_to_rgba(
    scratch: &[f32],
    rgba: &mut [u8],
    width: u32,
    height: u32,
    spp: u16,
    params: TiffSampleDecodeParams,
) {
    let TiffSampleDecodeParams {
        photo, smin, smax, ..
    } = params;
    let range = (smax - smin).max(f64::EPSILON);
    for y in 0..height as usize {
        let row_scratch = &scratch[y * width as usize * 4..(y + 1) * width as usize * 4];
        let row_rgba = &mut rgba[y * width as usize * 4..(y + 1) * width as usize * 4];
        match photo {
            PHOTO_MINISWHITE | PHOTO_MINISBLACK => {
                simple_image_viewer::simd_pixel_convert::finalize_gray_linear_scratch_row_to_rgba8(
                    row_scratch,
                    row_rgba,
                    width as usize,
                    smin,
                    smax,
                    photo == PHOTO_MINISWHITE,
                );
            }
            PHOTO_RGB => {
                for x in 0..width as usize {
                    let dst_idx = x * 4;
                    let mut samples = [0u32; 4];
                    for s in 0..(spp as usize).min(4) {
                        let f_val = row_scratch[dst_idx + s] as f64;
                        let linear = ((f_val - smin) / range).clamp(0.0, 1.0) as f32;
                        samples[s] = to_srgb_8(linear) as u32;
                    }
                    row_rgba[dst_idx] = samples[0] as u8;
                    row_rgba[dst_idx + 1] = samples[1] as u8;
                    row_rgba[dst_idx + 2] = samples[2] as u8;
                    if spp >= 4 {
                        row_rgba[dst_idx + 3] = samples[3] as u8;
                    } else {
                        row_rgba[dst_idx + 3] = 255;
                    }
                }
            }
            PHOTO_SEPARATED => {
                for x in 0..width as usize {
                    let dst_idx = x * 4;
                    let f_val = row_scratch[dst_idx] as f64;
                    let linear = ((f_val - smin) / range).clamp(0.0, 1.0) as f32;
                    let v = (linear * 255.0) as u8;
                    row_rgba[dst_idx] = v;
                    row_rgba[dst_idx + 1] = v;
                    row_rgba[dst_idx + 2] = v;
                    row_rgba[dst_idx + 3] = 255;
                }
            }
            _ => {}
        }
    }
}

/// Scene-linear RGBA (`HdrImageMetadata` linear / scene) from IEEE float TIFF samples. libtiff returns
/// multi-byte samples in **native** byte order -- no endian swap here (matches `get_sample_value` rule).
pub(crate) struct IeeeSceneLinearDecodeArgs<'a> {
    pub tif: *mut lib::TIFF,
    pub width: u32,
    pub height: u32,
    pub bps: u16,
    pub spp: u16,
    pub photo: u16,
    pub config: u16,
    pub cancel: Option<&'a std::sync::atomic::AtomicBool>,
}

pub(crate) fn decode_ieee_scene_linear_rgba32f(
    args: IeeeSceneLinearDecodeArgs<'_>,
) -> Result<Vec<f32>, String> {
    let IeeeSceneLinearDecodeArgs {
        tif,
        width,
        height,
        bps,
        mut spp,
        photo,
        config,
        cancel,
    } = args;
    if spp == 0 {
        spp = 1;
    }
    if !matches!(bps, 16 | 32 | 64) {
        return Err(format!("IEEE TIFF: unsupported BitsPerSample {bps}"));
    }

    // SAFETY: `tif` is a valid libtiff handle opened by this loader; TIFFScanlineSize is read-only.
    let scanline_size = unsafe { lib::TIFFScanlineSize(tif) };
    if matches!(photo, PHOTO_RGB | PHOTO_MINISBLACK | PHOTO_MINISWHITE) {
        ensure_tiff_scanline_size(scanline_size, width, spp, bps, config, "IEEE TIFF")?;
    } else if scanline_size <= 0 {
        return Err("IEEE TIFF: invalid scanline size".to_string());
    }
    let mut buf = vec![0u8; scanline_size as usize];

    let miniswhite_pivot = if photo == PHOTO_MINISWHITE {
        unsafe { tiff_tag_smax_sample_value_f64(tif) }
            .filter(|&mx| mx > 0.0)
            .map(|mx| mx as f32)
    } else {
        None
    };
    let miniswhite_deferred = photo == PHOTO_MINISWHITE && miniswhite_pivot.is_none();
    if miniswhite_deferred {
        log::debug!(
            "[libtiff_loader] IEEE MINISWHITE float: SMaxSampleValue unset or non-positive; \
             deferring inversion until image-wide maximum is known"
        );
    }
    let mut miniswhite_gmax = f32::NEG_INFINITY;

    let mut out = vec![0.0_f32; checked_rgba32f_len(width, height)?];

    if config == CONFIG_CONTIG {
        for y in 0..height {
            super::constants::poll_tiff_scanline_cancel(cancel, y)?;
            // SAFETY: `tif` is valid and exclusive; `buf` is sized to `TIFFScanlineSize(tif)`.
            if unsafe { lib::TIFFReadScanline(tif, buf.as_mut_ptr() as *mut c_void, y, 0) } <= 0 {
                return Err(format!("IEEE TIFF: TIFFReadScanline failed at row {y}"));
            }
            let row_off = y as usize * width as usize * 4;
            let row = &mut out[row_off..row_off + width as usize * 4];
            match photo {
                PHOTO_RGB => {
                    if bps == 32 {
                        simple_image_viewer::simd_pixel_convert::ieee_f32_rgb_scanline_to_rgba32f(
                            &buf,
                            row,
                            width as usize,
                            spp as usize,
                        );
                    } else {
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
                }
                PHOTO_MINISBLACK => {
                    if bps == 32 {
                        simple_image_viewer::simd_pixel_convert::ieee_f32_gray_scanline_to_rgba32f(
                            &buf,
                            row,
                            width as usize,
                            None,
                        );
                    } else {
                        for x in 0..width as usize {
                            let dst = x * 4;
                            let v = read_ieee_sample_f32(&buf, x, bps);
                            row[dst] = v;
                            row[dst + 1] = v;
                            row[dst + 2] = v;
                            row[dst + 3] = 1.0;
                        }
                    }
                }
                PHOTO_MINISWHITE => {
                    if let Some(pivot) = miniswhite_pivot {
                        if bps == 32 {
                            simple_image_viewer::simd_pixel_convert::ieee_f32_gray_scanline_to_rgba32f(
                                &buf,
                                row,
                                width as usize,
                                Some(pivot),
                            );
                        } else {
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
                    } else {
                        for x in 0..width as usize {
                            let dst = x * 4;
                            let v = read_ieee_sample_f32(&buf, x, bps);
                            if v.is_finite() {
                                miniswhite_gmax = miniswhite_gmax.max(v);
                            }
                            row[dst] = v;
                            row[dst + 1] = v;
                            row[dst + 2] = v;
                            row[dst + 3] = 1.0;
                        }
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
                super::constants::poll_tiff_scanline_cancel(cancel, y)?;
                // SAFETY: `tif` is valid and exclusive; `buf` fits one planar sample row (`sample=c`).
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
                        if let Some(pivot) = miniswhite_pivot {
                            for x in 0..width as usize {
                                let dst = x * 4;
                                let v = read_ieee_sample_f32(&buf, x, bps);
                                let g = (pivot - v).max(0.0);
                                row[dst + c] = g;
                            }
                        } else {
                            for x in 0..width as usize {
                                let dst = x * 4;
                                let v = read_ieee_sample_f32(&buf, x, bps);
                                if v.is_finite() {
                                    miniswhite_gmax = miniswhite_gmax.max(v);
                                }
                                row[dst + c] = v;
                            }
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

    if miniswhite_deferred {
        let pivot = ieee_grayscale_float_white_reference_from_max(miniswhite_gmax);
        finalize_miniswhite_float_inversion(&mut out, width, height, pivot);
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
    cancel: Option<&std::sync::atomic::AtomicBool>,
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
    let mut out = vec![0.0_f32; checked_rgba32f_len(width, height)?];
    // SAFETY: `tif` is a valid libtiff handle opened by this loader; TIFFScanlineSize is read-only.
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
            super::constants::poll_tiff_scanline_cancel(cancel, y)?;
            // SAFETY: `tif` is valid and exclusive; `scanline` is sized to `TIFFScanlineSize(tif)`.
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
                super::constants::poll_tiff_scanline_cancel(cancel, y)?;
                // SAFETY: `tif` is valid and exclusive; `scanline` is sized to `TIFFScanlineSize(tif)`.
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
                super::constants::poll_tiff_scanline_cancel(cancel, y)?;
                // SAFETY: `tif` is valid and exclusive; `scanline` is sized to `TIFFScanlineSize(tif)`.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb8_params() -> TiffSampleDecodeParams {
        TiffSampleDecodeParams {
            bps: 8,
            photo: PHOTO_RGB,
            format: FORMAT_UINT,
            swapped: false,
            smin: 0.0,
            smax: 255.0,
        }
    }

    fn empty_palette() -> TiffPaletteMaps {
        TiffPaletteMaps {
            r_map: std::ptr::null_mut(),
            g_map: std::ptr::null_mut(),
            b_map: std::ptr::null_mut(),
            entries: 0,
        }
    }

    #[test]
    fn rgb8_contig_fast_path_expands_rgb24_to_rgba() {
        let src = [10, 20, 30, 40, 50, 60];
        let mut dst = [0; 8];

        process_scanline_contig(&src, &mut dst, 2, 3, rgb8_params(), empty_palette());

        assert_eq!(dst, [10, 20, 30, 255, 40, 50, 60, 255]);
    }

    #[test]
    fn get_raw_value_reads_ieee_half_float_bits() {
        let bits = half::f16::from_f32(2.5).to_bits();
        let buf = bits.to_ne_bytes();
        let got = get_raw_value(&buf, 0, 16, FORMAT_IEEEFP);
        assert!((got - 2.5).abs() < 1.0e-3);
        let as_uint = get_raw_value(&buf, 0, 16, FORMAT_UINT);
        assert_ne!(as_uint, got);
    }

    #[test]
    fn rgb8_contig_fast_path_copies_rgba32() {
        let src = [10, 20, 30, 128, 40, 50, 60, 64];
        let mut dst = [0; 8];

        process_scanline_contig(&src, &mut dst, 2, 4, rgb8_params(), empty_palette());

        assert_eq!(dst, src);
    }
}
