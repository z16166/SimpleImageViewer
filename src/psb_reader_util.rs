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

//! PSD/PSB reader utility functions extracted from `psb_reader` to keep that
//! module under the 2000-line threshold (checklist #12).
//!
//! Contains: blank / zero-information detection (SIMD), ICC / thumbnail
//! resource extraction, PackBits decompression, sample conversion, CMYK→RGB,
//! row interleave, dimension validation, seek/read helpers, and memory
//! estimation. All are re-exported through `psb_reader`.

use std::io::{Read, Seek, SeekFrom};
use std::sync::atomic::AtomicBool;

use simple_image_viewer::simd_swizzle;

use crate::psb_reader::PsbComposite;

// Re-export the `check_decode_cancel` from the loader.
pub(crate) use crate::loader::check_decode_cancel;

// ---------------------------------------------------------------------------
// Constants (moved from psb_reader to keep the main module focused)
// ---------------------------------------------------------------------------

/// Photoshop Image Data / channel compression: raw (uncompressed).
pub(crate) const PSD_COMPRESSION_RAW: u16 = 0;
/// Photoshop PackBits RLE compression.
pub(crate) const PSD_COMPRESSION_RLE: u16 = 1;
/// Photoshop ZIP (zlib) compression without prediction.
pub(crate) const PSD_COMPRESSION_ZIP: u16 = 2;
/// Photoshop ZIP with horizontal difference prediction.
pub(crate) const PSD_COMPRESSION_ZIP_PREDICTION: u16 = 3;

/// Photoshop color mode: Grayscale.
pub(crate) const PSD_COLOR_MODE_GRAYSCALE: u16 = 1;
/// Photoshop color mode: RGB.
pub(crate) const PSD_COLOR_MODE_RGB: u16 = 3;
/// Photoshop color mode: CMYK.
pub(crate) const PSD_COLOR_MODE_CMYK: u16 = 4;
/// Photoshop color mode: Bitmap (1-bit per pixel, packed 8-per-byte).
pub(crate) const PSD_COLOR_MODE_BITMAP: u16 = 0;
/// Photoshop color mode: Indexed (8-bit palette lookup).
pub(crate) const PSD_COLOR_MODE_INDEXED: u16 = 2;
/// Photoshop color mode: Multichannel (arbitrary channels, no standard transform).
pub(crate) const PSD_COLOR_MODE_MULTICHANNEL: u16 = 7;
/// Photoshop color mode: Duotone (stored as single grayscale channel).
pub(crate) const PSD_COLOR_MODE_DUOTONE: u16 = 8;
/// Photoshop color mode: Lab (CIE L*a*b*).
pub(crate) const PSD_COLOR_MODE_LAB: u16 = 9;

/// Layer channel ID: transparency mask (alpha).
pub(crate) const PSD_CHANNEL_ID_ALPHA: i16 = -1;
/// Layer channel ID: user-supplied layer mask.
pub(crate) const PSD_CHANNEL_ID_USER_MASK: i16 = -2;
/// Layer channel ID: real user mask (vector + raster combined).
pub(crate) const PSD_CHANNEL_ID_REAL_USER_MASK: i16 = -3;
/// Inclusive upper bound for color-channel IDs (0=R/C/Gray, 1=G/M, 2=B/Y, 3=K).
pub(crate) const PSD_CHANNEL_ID_COLOR_MAX: i16 = 3;

/// Adobe Photoshop PSD/PSB maximum canvas dimension (pixels per side).
pub(crate) const PSD_MAX_DIMENSION: u32 = 300_000;
/// Hard cap on document canvas total pixels for P1 / P2 / HDR *full-canvas*
/// allocations ([`checked_pixel_count`], HDR flat decode, layer budgets).
///
/// `PSD_MAX_DIMENSION` alone still allows `300_000 x 300_000`. This matches the
/// per-layer pixel budget so a single canvas allocation cannot reserve tens of
/// GB before any pixel data is read.
///
/// Do **not** apply this in [`validate_psd_dimensions`]: Hubble-class PSB
/// documents exceed this cap but must still parse so `load_psd` can open them
/// via on-demand disk tiling instead of a full RGBA buffer.
pub(crate) const MAX_DOCUMENT_PIXELS: u64 = 1024 * 1024 * 1024;
/// Adobe Photoshop PSD/PSB maximum channel count.
const PSD_MAX_CHANNELS: u32 = 56;
/// Absolute cap on a single ZIP Image Data inflate (all planar channels).
///
/// Matches [`crate::psb_layer_composite::MAX_COMPOSITE_DECODED_BYTES`] so a
/// 32-bit multi-channel document cannot allocate unbounded zlib output when a
/// caller skips the RAM precheck that uses [`estimate_memory_from_bytes`].
pub(crate) const MAX_ZIP_PLANAR_INFLATE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
/// Bytes per RGBA pixel when assembling the composite image.
pub(crate) const RGBA_BYTES_PER_PIXEL: usize = 4;
/// Bytes per display-linear HDR RGBA f32 pixel.
pub(crate) const HDR_RGBA_F32_BYTES_PER_PIXEL: u64 = 16;
/// Photoshop Image Resource IDs for embedded JPEG thumbnails.
const IR_THUMBNAIL_PS4: u16 = 1033;
const IR_THUMBNAIL_PS5: u16 = 1036;
/// Photoshop IR 1033/1036 thumbnail resource header length (bytes before JPEG payload).
/// Layout: format(4) + width(4) + height(4) + widthbytes(4) + size(4) + compressed(4)
/// + bits/pixel(2) + planes(2) = 28.
pub(crate) const IR_JPEG_THUMBNAIL_HEADER_LEN: usize = 28;
/// Offset of "Size after compression" (u32 BE) in the IR 1033/1036 thumbnail header.
pub(crate) const IR_JPEG_THUMBNAIL_COMPRESSED_SIZE_OFFSET: usize = 20;
/// Photoshop Image Resource: ICC Profile Settings (raw ICC bytes).
const IR_ICC_PROFILE: u16 = 1039;
/// Pixel-index mask for cancel polling in RGBA8 full-buffer scans (~every 256 KiB).
const RGBA8_CANCEL_POLL_MASK: usize = 0x3_FFFF;
/// Row-count index mask for cancel polling while reading RLE byte counts (~every 1024 rows).
pub(crate) const RLE_ROW_COUNT_CANCEL_POLL_INTERVAL: usize = 0x3FF;
/// Row-index mask for cancel polling during RLE per-row decode / skip / interleave (~every 64 rows).
pub(crate) const RLE_ROW_DECODE_CANCEL_POLL_INTERVAL: usize = 0x3F;

/// Maximum PackBits NO-OP operations per RLE row (safety limit).
pub(crate) const PACKBITS_MAX_NOOPS_PER_ROW: usize = 4096;
/// Error message when PackBits produces too many NO-OPs.
pub(crate) const PACKBITS_TOO_MANY_NOOPS: &str =
    "PackBits RLE: too many NO-OP operations in one row";
/// Absolute blank barrier for P1 flattened composites (RGBA8).
///
/// Returns true when the buffer is semantically empty:
/// - every alpha byte is 0 (fully transparent), or
/// - (Gray / RGB only) every RGB triple is (0,0,0) (absolute pure black).
///
/// For other color modes (CMYK, Lab, Indexed, ...), only the all-alpha-0
/// rule applies. Those modes can yield RGB-0 after conversion (or incomplete
/// channel mapping) for non-empty content, so treating RGB-0 as blank would
/// false-positive and skip a valid P1 flat.
///
/// Structural decode success alone is not enough; this is an O(N) SIMD scan
/// with early exit once a nonzero sample that disproves blank is found.
/// Polls `cancel` on large buffers when provided.
pub fn rgba8_is_absolutely_blank_with_cancel(
    pixels: &[u8],
    cancel: Option<&AtomicBool>,
    color_mode: u16,
) -> Result<bool, crate::loader::DecodeError> {
    if pixels.len() < 4 {
        return Ok(true);
    }
    let n = pixels.len() - (pixels.len() % 4);
    if n == 0 {
        return Ok(true);
    }
    let pixels = &pixels[..n];
    let use_rgb0 = color_mode_uses_rgb0_blank(color_mode);

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            return unsafe { rgba8_absolutely_blank_avx2(pixels, cancel, use_rgb0) };
        }
        if is_x86_feature_detected!("sse2") {
            return unsafe { rgba8_absolutely_blank_sse2(pixels, cancel, use_rgb0) };
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        return unsafe { rgba8_absolutely_blank_neon(pixels, cancel, use_rgb0) };
    }

    #[cfg(not(target_arch = "aarch64"))]
    {
        rgba8_absolutely_blank_scalar(pixels, cancel, use_rgb0)
    }
}

/// Gray and RGB: all-RGB-0 is a reliable empty-flat signal.
#[inline]
pub(crate) fn color_mode_uses_rgb0_blank(color_mode: u16) -> bool {
    matches!(
        color_mode,
        PSD_COLOR_MODE_GRAYSCALE
            | PSD_COLOR_MODE_RGB
            | PSD_COLOR_MODE_BITMAP
            | PSD_COLOR_MODE_INDEXED
            | PSD_COLOR_MODE_DUOTONE
            | PSD_COLOR_MODE_LAB
    )
}

/// Scan RGBA8; returns `(any_nonzero_rgb, any_nonzero_alpha)`.
fn rgba8_any_rgb_alpha_scalar(
    pixels: &[u8],
    cancel: Option<&AtomicBool>,
    mut any_rgb: bool,
    mut any_a: bool,
    use_rgb0: bool,
) -> Result<(bool, bool), crate::loader::DecodeError> {
    let mut i = 0usize;
    while i + 4 <= pixels.len() {
        if i & RGBA8_CANCEL_POLL_MASK == 0 {
            check_decode_cancel(cancel)?;
        }
        if use_rgb0 && (pixels[i] | pixels[i + 1] | pixels[i + 2]) != 0 {
            any_rgb = true;
        }
        if pixels[i + 3] != 0 {
            any_a = true;
        }
        if any_a && (!use_rgb0 || any_rgb) {
            return Ok((any_rgb, true));
        }
        i += 4;
    }
    Ok((any_rgb, any_a))
}

#[cfg(not(target_arch = "aarch64"))]
fn rgba8_absolutely_blank_scalar(
    pixels: &[u8],
    cancel: Option<&AtomicBool>,
    use_rgb0: bool,
) -> Result<bool, crate::loader::DecodeError> {
    let (any_rgb, any_a) = rgba8_any_rgb_alpha_scalar(pixels, cancel, false, false, use_rgb0)?;
    Ok(if use_rgb0 { !any_rgb || !any_a } else { !any_a })
}

// ---- RGBA blank-detection masks (isolate RGB / Alpha in each u32 pixel) ----
const RGBA_RGB_MASK: u32 = 0x00FF_FFFF;
const RGBA_ALPHA_MASK: u32 = 0xFF00_0000;

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn rgba8_absolutely_blank_sse2(
    pixels: &[u8],
    cancel: Option<&AtomicBool>,
    use_rgb0: bool,
) -> Result<bool, crate::loader::DecodeError> {
    use core::arch::x86_64::*;
    let mut any_rgb = false;
    let mut any_a = false;
    let n = pixels.len();
    let mut i = 0usize;
    let rgb_mask = _mm_set1_epi32(RGBA_RGB_MASK as i32);
    let a_mask = _mm_set1_epi32(RGBA_ALPHA_MASK as i32);
    let zero = _mm_setzero_si128();
    while i + 16 <= n {
        if i & RGBA8_CANCEL_POLL_MASK == 0 {
            check_decode_cancel(cancel)?;
        }
        let v = unsafe { _mm_loadu_si128(pixels.as_ptr().add(i).cast()) };
        if use_rgb0 {
            let rgb = _mm_and_si128(v, rgb_mask);
            if _mm_movemask_epi8(_mm_cmpeq_epi8(rgb, zero)) != 0xFFFF {
                any_rgb = true;
            }
        }
        let alpha = _mm_and_si128(v, a_mask);
        if _mm_movemask_epi8(_mm_cmpeq_epi8(alpha, zero)) != 0xFFFF {
            any_a = true;
        }
        if any_a && (!use_rgb0 || any_rgb) {
            return Ok(false);
        }
        i += 16;
    }
    let (any_rgb, any_a) =
        rgba8_any_rgb_alpha_scalar(&pixels[i..], cancel, any_rgb, any_a, use_rgb0)?;
    Ok(if use_rgb0 { !any_rgb || !any_a } else { !any_a })
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn rgba8_absolutely_blank_avx2(
    pixels: &[u8],
    cancel: Option<&AtomicBool>,
    use_rgb0: bool,
) -> Result<bool, crate::loader::DecodeError> {
    use core::arch::x86_64::*;
    let mut any_rgb = false;
    let mut any_a = false;
    let n = pixels.len();
    let mut i = 0usize;
    let rgb_mask = _mm256_set1_epi32(RGBA_RGB_MASK as i32);
    let a_mask = _mm256_set1_epi32(RGBA_ALPHA_MASK as i32);
    let zero = _mm256_setzero_si256();
    while i + 32 <= n {
        if i & RGBA8_CANCEL_POLL_MASK == 0 {
            check_decode_cancel(cancel)?;
        }
        let v = unsafe { _mm256_loadu_si256(pixels.as_ptr().add(i).cast()) };
        if use_rgb0 {
            let rgb = _mm256_and_si256(v, rgb_mask);
            if _mm256_movemask_epi8(_mm256_cmpeq_epi8(rgb, zero)) != -1 {
                any_rgb = true;
            }
        }
        let alpha = _mm256_and_si256(v, a_mask);
        if _mm256_movemask_epi8(_mm256_cmpeq_epi8(alpha, zero)) != -1 {
            any_a = true;
        }
        if any_a && (!use_rgb0 || any_rgb) {
            return Ok(false);
        }
        i += 32;
    }
    let (any_rgb, any_a) =
        rgba8_any_rgb_alpha_scalar(&pixels[i..], cancel, any_rgb, any_a, use_rgb0)?;
    Ok(if use_rgb0 { !any_rgb || !any_a } else { !any_a })
}

#[cfg(target_arch = "aarch64")]
unsafe fn rgba8_absolutely_blank_neon(
    pixels: &[u8],
    cancel: Option<&AtomicBool>,
    use_rgb0: bool,
) -> Result<bool, crate::loader::DecodeError> {
    unsafe {
        use core::arch::aarch64::*;
        let mut any_rgb = false;
        let mut any_a = false;
        let n = pixels.len();
        let mut i = 0usize;
        let rgb_mask = vdupq_n_u32(RGBA_RGB_MASK);
        let a_mask = vdupq_n_u32(RGBA_ALPHA_MASK);
        while i + 16 <= n {
            if i & RGBA8_CANCEL_POLL_MASK == 0 {
                check_decode_cancel(cancel)?;
            }
            let v = vld1q_u8(pixels.as_ptr().add(i));
            let vu = vreinterpretq_u32_u8(v);
            if use_rgb0 {
                let rgb = vandq_u32(vu, rgb_mask);
                if vmaxvq_u32(rgb) != 0 {
                    any_rgb = true;
                }
            }
            let alpha = vandq_u32(vu, a_mask);
            if vmaxvq_u32(alpha) != 0 {
                any_a = true;
            }
            if any_a && (!use_rgb0 || any_rgb) {
                return Ok(false);
            }
            i += 16;
        }
        let (any_rgb, any_a) =
            rgba8_any_rgb_alpha_scalar(&pixels[i..], cancel, any_rgb, any_a, use_rgb0)?;
        Ok(if use_rgb0 { !any_rgb || !any_a } else { !any_a })
    }
}

/// Zero-information barrier for P2 strict layer composites (RGBA8).
///
/// Returns true when the buffer has no visual information content:
/// - every alpha byte is 0 (fully transparent), or
/// - every RGB triple is identical (solid fill; variance / range is 0).
///
/// Unlike P1 absolute blank (all-RGB-0 only), any solid color fails here
/// (white, gray, etc.). Early-exits once a nonzero alpha and an RGB that
/// differs from the first pixel are both observed. Polls `cancel` on large
/// buffers when provided.
///
/// SDR compares u8 lanes exactly. HDR's matching barrier
/// (`psb_hdr_main::rgba_f32_is_zero_information_with_cancel`) uses a small
/// f32 EPS for the same transparent-or-solid-RGB rule, because float
/// composites accumulate blend/ICC noise that exact equality would mis-reject.
pub fn rgba8_is_zero_information_with_cancel(
    pixels: &[u8],
    cancel: Option<&AtomicBool>,
) -> Result<bool, crate::loader::DecodeError> {
    if pixels.len() < 4 {
        return Ok(true);
    }
    let n = pixels.len() - (pixels.len() % 4);
    if n == 0 {
        return Ok(true);
    }
    let pixels = &pixels[..n];

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            return unsafe { rgba8_zero_information_avx2(pixels, cancel) };
        }
        if is_x86_feature_detected!("sse2") {
            return unsafe { rgba8_zero_information_sse2(pixels, cancel) };
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        return unsafe { rgba8_zero_information_neon(pixels, cancel) };
    }

    #[cfg(not(target_arch = "aarch64"))]
    {
        rgba8_zero_information_scalar(pixels, cancel)
    }
}

/// Scan RGBA8 against `ref_rgb`; returns `(rgb_varies, any_nonzero_alpha)`.
fn rgba8_rgb_varies_any_alpha_scalar(
    pixels: &[u8],
    cancel: Option<&AtomicBool>,
    ref_r: u8,
    ref_g: u8,
    ref_b: u8,
    mut rgb_varies: bool,
    mut any_a: bool,
) -> Result<(bool, bool), crate::loader::DecodeError> {
    let mut i = 0usize;
    while i + 4 <= pixels.len() {
        if i & RGBA8_CANCEL_POLL_MASK == 0 {
            check_decode_cancel(cancel)?;
        }
        if pixels[i] != ref_r || pixels[i + 1] != ref_g || pixels[i + 2] != ref_b {
            rgb_varies = true;
        }
        if pixels[i + 3] != 0 {
            any_a = true;
        }
        if rgb_varies && any_a {
            return Ok((true, true));
        }
        i += 4;
    }
    Ok((rgb_varies, any_a))
}

#[cfg(not(target_arch = "aarch64"))]
fn rgba8_zero_information_scalar(
    pixels: &[u8],
    cancel: Option<&AtomicBool>,
) -> Result<bool, crate::loader::DecodeError> {
    let ref_r = pixels[0];
    let ref_g = pixels[1];
    let ref_b = pixels[2];
    let (rgb_varies, any_a) =
        rgba8_rgb_varies_any_alpha_scalar(pixels, cancel, ref_r, ref_g, ref_b, false, false)?;
    // Zero info when fully transparent or solid RGB (no variance).
    Ok(!any_a || !rgb_varies)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn rgba8_zero_information_sse2(
    pixels: &[u8],
    cancel: Option<&AtomicBool>,
) -> Result<bool, crate::loader::DecodeError> {
    use core::arch::x86_64::*;
    let ref_r = pixels[0];
    let ref_g = pixels[1];
    let ref_b = pixels[2];
    let ref_rgb = _mm_set1_epi32(u32::from_le_bytes([ref_r, ref_g, ref_b, 0]) as i32);
    let rgb_mask = _mm_set1_epi32(RGBA_RGB_MASK as i32);
    let a_mask = _mm_set1_epi32(RGBA_ALPHA_MASK as i32);
    let zero = _mm_setzero_si128();
    let mut rgb_varies = false;
    let mut any_a = false;
    let n = pixels.len();
    let mut i = 0usize;
    while i + 16 <= n {
        if i & RGBA8_CANCEL_POLL_MASK == 0 {
            check_decode_cancel(cancel)?;
        }
        let v = unsafe { _mm_loadu_si128(pixels.as_ptr().add(i).cast()) };
        let rgb = _mm_and_si128(v, rgb_mask);
        let alpha = _mm_and_si128(v, a_mask);
        if _mm_movemask_epi8(_mm_cmpeq_epi8(rgb, ref_rgb)) != 0xFFFF {
            rgb_varies = true;
        }
        if _mm_movemask_epi8(_mm_cmpeq_epi8(alpha, zero)) != 0xFFFF {
            any_a = true;
        }
        if rgb_varies && any_a {
            return Ok(false);
        }
        i += 16;
    }
    let (rgb_varies, any_a) = rgba8_rgb_varies_any_alpha_scalar(
        &pixels[i..],
        cancel,
        ref_r,
        ref_g,
        ref_b,
        rgb_varies,
        any_a,
    )?;
    Ok(!any_a || !rgb_varies)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn rgba8_zero_information_avx2(
    pixels: &[u8],
    cancel: Option<&AtomicBool>,
) -> Result<bool, crate::loader::DecodeError> {
    use core::arch::x86_64::*;
    let ref_r = pixels[0];
    let ref_g = pixels[1];
    let ref_b = pixels[2];
    let ref_rgb = _mm256_set1_epi32(u32::from_le_bytes([ref_r, ref_g, ref_b, 0]) as i32);
    let rgb_mask = _mm256_set1_epi32(RGBA_RGB_MASK as i32);
    let a_mask = _mm256_set1_epi32(RGBA_ALPHA_MASK as i32);
    let zero = _mm256_setzero_si256();
    let mut rgb_varies = false;
    let mut any_a = false;
    let n = pixels.len();
    let mut i = 0usize;
    while i + 32 <= n {
        if i & RGBA8_CANCEL_POLL_MASK == 0 {
            check_decode_cancel(cancel)?;
        }
        let v = unsafe { _mm256_loadu_si256(pixels.as_ptr().add(i).cast()) };
        let rgb = _mm256_and_si256(v, rgb_mask);
        let alpha = _mm256_and_si256(v, a_mask);
        if _mm256_movemask_epi8(_mm256_cmpeq_epi8(rgb, ref_rgb)) != -1 {
            rgb_varies = true;
        }
        if _mm256_movemask_epi8(_mm256_cmpeq_epi8(alpha, zero)) != -1 {
            any_a = true;
        }
        if rgb_varies && any_a {
            return Ok(false);
        }
        i += 32;
    }
    let (rgb_varies, any_a) = rgba8_rgb_varies_any_alpha_scalar(
        &pixels[i..],
        cancel,
        ref_r,
        ref_g,
        ref_b,
        rgb_varies,
        any_a,
    )?;
    Ok(!any_a || !rgb_varies)
}

#[cfg(target_arch = "aarch64")]
unsafe fn rgba8_zero_information_neon(
    pixels: &[u8],
    cancel: Option<&AtomicBool>,
) -> Result<bool, crate::loader::DecodeError> {
    unsafe {
        use core::arch::aarch64::*;
        let ref_r = pixels[0];
        let ref_g = pixels[1];
        let ref_b = pixels[2];
        let ref_rgb = vdupq_n_u32(u32::from_le_bytes([ref_r, ref_g, ref_b, 0]));
        let rgb_mask = vdupq_n_u32(RGBA_RGB_MASK);
        let a_mask = vdupq_n_u32(RGBA_ALPHA_MASK);
        let mut rgb_varies = false;
        let mut any_a = false;
        let n = pixels.len();
        let mut i = 0usize;
        while i + 16 <= n {
            if i & RGBA8_CANCEL_POLL_MASK == 0 {
                check_decode_cancel(cancel)?;
            }
            let v = vld1q_u8(pixels.as_ptr().add(i));
            let vu = vreinterpretq_u32_u8(v);
            let rgb = vandq_u32(vu, rgb_mask);
            let alpha = vandq_u32(vu, a_mask);
            // Any lane differing from ref => RGB variance.
            let eq = vceqq_u32(rgb, ref_rgb);
            if vminvq_u32(eq) == 0 {
                rgb_varies = true;
            }
            if vmaxvq_u32(alpha) != 0 {
                any_a = true;
            }
            if rgb_varies && any_a {
                return Ok(false);
            }
            i += 16;
        }
        let (rgb_varies, any_a) = rgba8_rgb_varies_any_alpha_scalar(
            &pixels[i..],
            cancel,
            ref_r,
            ref_g,
            ref_b,
            rgb_varies,
            any_a,
        )?;
        Ok(!any_a || !rgb_varies)
    }
}

pub fn extract_icc_profile_from_ir(bytes: &[u8], ir_start: u64, ir_end: u64) -> Option<Vec<u8>> {
    for_each_image_resource(bytes, ir_start, ir_end, |rid, data| {
        if rid == IR_ICC_PROFILE && !data.is_empty() {
            Some(data.to_vec())
        } else {
            None
        }
    })
}

pub(crate) fn find_image_resource<'a>(
    bytes: &'a [u8],
    ir_start: u64,
    ir_end: u64,
    rid: u16,
) -> Option<&'a [u8]> {
    let base = bytes.as_ptr() as usize;
    let (start, len) = for_each_image_resource(bytes, ir_start, ir_end, |resource_id, data| {
        if resource_id != rid {
            return None;
        }
        let start = (data.as_ptr() as usize).checked_sub(base)?;
        Some((start, data.len()))
    })?;
    bytes.get(start..start.checked_add(len)?)
}

/// Extract embedded ICC (IR 1039) from a full PSD/PSB byte buffer.
#[cfg(test)]
pub fn extract_embedded_icc_from_psd(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.len() < 30 {
        return None;
    }
    let mut r = std::io::Cursor::new(bytes);
    let mut sig = [0u8; 4];
    r.read_exact(&mut sig).ok()?;
    if &sig != b"8BPS" {
        return None;
    }
    let version = read_u16(&mut r).ok()?;
    if version != 1 && version != 2 {
        return None;
    }
    r.seek(SeekFrom::Current(6)).ok()?;
    let _channels = read_u16(&mut r).ok()?;
    let _height = read_u32(&mut r).ok()?;
    let _width = read_u32(&mut r).ok()?;
    let _depth = read_u16(&mut r).ok()?;
    let _color_mode = read_u16(&mut r).ok()?;
    let cm_len = read_u32(&mut r).ok()? as u64;
    seek_forward_within(&mut r, cm_len, bytes.len() as u64, "color mode data").ok()?;
    let ir_len = read_u32(&mut r).ok()? as u64;
    let ir_start = r.stream_position().ok()?;
    let ir_end = ir_start.saturating_add(ir_len).min(bytes.len() as u64);
    extract_icc_profile_from_ir(bytes, ir_start, ir_end)
}

/// Try to extract Photoshop Image Resource 1033/1036 JPEG thumbnail as RGBA8.
///
/// Prefers [`PsdSectionIndex::parse`] IR bounds when the structural walk
/// succeeds. On structural failure (unsupported color mode, truncated
/// layer/mask, etc.) falls back to an IR-only locate so P3 recovery still
/// works when Image Resources remain readable.
pub fn try_extract_photoshop_thumbnail(bytes: &[u8]) -> Option<PsbComposite> {
    if let Ok(index) = crate::psb_section_index::PsdSectionIndex::parse(bytes) {
        return extract_photoshop_thumbnail_from_ir(bytes, index.ir_start, index.ir_end);
    }
    let (ir_start, ir_end) =
        crate::psb_section_index::PsdSectionIndex::locate_image_resources(bytes).ok()?;
    extract_photoshop_thumbnail_from_ir(bytes, ir_start, ir_end)
}

/// Parse Photoshop Image Resource 1033/1036 JPEG thumbnail into RGBA8.
pub(crate) fn extract_photoshop_thumbnail_from_ir(
    bytes: &[u8],
    ir_start: u64,
    ir_end: u64,
) -> Option<PsbComposite> {
    for_each_image_resource(bytes, ir_start, ir_end, |rid, data| {
        if (rid == IR_THUMBNAIL_PS4 || rid == IR_THUMBNAIL_PS5)
            && data.len() >= IR_JPEG_THUMBNAIL_HEADER_LEN
        {
            decode_photoshop_thumbnail_resource(data)
        } else {
            None
        }
    })
}

/// Walk Photoshop Image Resources (8BIM), invoking `on_resource` for each.
/// Returns the first `Some` from the callback.
pub(crate) fn for_each_image_resource<T>(
    bytes: &[u8],
    ir_start: u64,
    ir_end: u64,
    mut on_resource: impl FnMut(u16, &[u8]) -> Option<T>,
) -> Option<T> {
    let mut pos = ir_start as usize;
    let end = (ir_end as usize).min(bytes.len());
    while pos + 12 <= end {
        let sig = &bytes[pos..pos + 4];
        // Only 8BIM is a valid Image Resource signature. 8B64 (with u64 length
        // in PSB) belongs to Additional Layer Information and is simply an
        // unrecognised signature here — it cleanly stops the walk.
        if sig != b"8BIM" {
            break;
        }
        pos += 4;
        if pos + 2 > end {
            break;
        }
        let rid = u16::from_be_bytes([bytes[pos], bytes[pos + 1]]);
        pos += 2;
        if pos >= end {
            break;
        }
        let name_len = bytes[pos] as usize;
        pos += 1;
        pos = pos.checked_add(name_len)?;
        if (name_len + 1) % 2 == 1 {
            pos = pos.checked_add(1)?;
        }
        if pos + 4 > end {
            break;
        }
        let size = u32::from_be_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
            as usize;
        pos += 4;
        let data_end = pos.checked_add(size)?;
        if data_end > end {
            // Truncated IR: stop walking rather than reading past the section.
            // Remaining resources are unavailable; callers that needed a specific
            // rid simply observe None (ICC / thumbnail missing).
            log::warn!(
                "PSD/PSB image resource {rid} size {size} exceeds remaining IR bytes (pos={pos}, end={end}); stopping walk"
            );
            break;
        }
        if let Some(found) = on_resource(rid, &bytes[pos..data_end]) {
            return Some(found);
        }
        pos = data_end;
        if size % 2 == 1 {
            pos = pos.checked_add(1)?;
        }
    }
    None
}

pub(crate) fn decode_photoshop_thumbnail_resource(data: &[u8]) -> Option<PsbComposite> {
    if data.len() < IR_JPEG_THUMBNAIL_HEADER_LEN {
        return None;
    }
    let format = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
    // 1 = JPEG RGB in current Photoshop thumbnail resources.
    if format != 1 {
        return None;
    }
    let width = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
    let height = u32::from_be_bytes([data[8], data[9], data[10], data[11]]);
    let compressed_off = IR_JPEG_THUMBNAIL_COMPRESSED_SIZE_OFFSET;
    let compressed = u32::from_be_bytes([
        data[compressed_off],
        data[compressed_off + 1],
        data[compressed_off + 2],
        data[compressed_off + 3],
    ]) as usize;
    if width == 0 || height == 0 || compressed == 0 {
        return None;
    }
    let jpeg_start = IR_JPEG_THUMBNAIL_HEADER_LEN;
    // Declared size past the resource payload is truncated/malformed -- do not
    // feed a partial JPEG into the decoder.
    let jpeg_end = jpeg_start.checked_add(compressed)?;
    if jpeg_end > data.len() {
        return None;
    }
    if jpeg_end <= jpeg_start {
        return None;
    }
    let jpeg = &data[jpeg_start..jpeg_end];
    let img = image::load_from_memory(jpeg).ok()?.to_rgba8();
    if img.width() == 0 || img.height() == 0 {
        return None;
    }
    Some(PsbComposite {
        width: img.width(),
        height: img.height(),
        pixels: img.into_raw(),
    })
}

/// PackBits RLE decompression (Macintosh PackBits variant) into an existing buffer.
///
/// Fail-closed: truncated literals/runs or early EOF before `expected_len` return
/// [`Err`] (empty `result`). Exceeding [`PACKBITS_MAX_NOOPS_PER_ROW`] also
/// returns [`Err`]. Trailing bytes after a successful fill are ignored (Photoshop
/// row payloads may be padded).
pub(crate) fn unpack_bits_into(
    result: &mut Vec<u8>,
    data: &[u8],
    expected_len: usize,
) -> Result<(), String> {
    result.clear();
    if result.capacity() < expected_len {
        result.reserve(expected_len - result.capacity());
    }

    let fail = |result: &mut Vec<u8>, msg: &str| -> Result<(), String> {
        result.clear();
        Err(msg.to_string())
    };

    let mut i = 0;
    let mut noop_count = 0usize;
    while i < data.len() && result.len() < expected_len {
        let n = data[i] as i8;
        i += 1;
        if n >= 0 {
            // Copy next (n+1) bytes literally
            let count = n as usize + 1;
            if i + count > data.len() {
                return fail(result, "PSD/PSB PackBits truncated literal run");
            }
            let remaining = expected_len - result.len();
            let take = count.min(remaining);
            result.extend_from_slice(&data[i..i + take]);
            i += count;
        } else if n > -128 {
            // Repeat next byte (1-n) times
            let count = (1 - n as i16) as usize;
            if i >= data.len() {
                return fail(result, "PSD/PSB PackBits repeat run missing value byte");
            }
            let val = data[i];
            i += 1;
            let remaining = expected_len - result.len();
            let actual_count = count.min(remaining);
            if actual_count > 0 {
                let start = result.len();
                result.resize(start + actual_count, 0);
                crate::psb_packbits_simd::fill_bytes(&mut result[start..], val);
            }
        } else {
            // n == -128: PackBits no-op (consumes the control byte only).
            noop_count += 1;
            if noop_count > PACKBITS_MAX_NOOPS_PER_ROW {
                return fail(result, PACKBITS_TOO_MANY_NOOPS);
            }
        }
    }
    if result.len() < expected_len {
        return fail(result, "PSD/PSB PackBits output shorter than expected");
    }
    Ok(())
}

// -- Helpers ---------------------------------------------------------

pub(crate) fn bytes_per_sample(depth: u16) -> Result<usize, String> {
    match depth {
        1 | 8 => Ok(1), // depth=1: Bitmap packed bits, same raw byte slot
        16 => Ok(2),
        32 => Ok(4),
        _ => Err(format!(
            "Unsupported PSD/PSB bit depth {depth} (supported: 8, 16, 32)"
        )),
    }
}

/// Raw (0) and PackBits RLE (1) expose per-row extents, so disk-tiled open can
/// seek/decode one row at a time. ZIP / ZIP+prediction (2|3) store Image Data
/// as a single zlib stream of all planar channels -- there is no independent
/// row offset table, so true row-at-a-time tiling would require a full inflate
/// first (defeating the disk-tiled memory goal). Those modes must use the
/// static full-canvas decode path (or the budgeted HDR ZIP flat helper).
pub(crate) fn tiled_compression_supported(compression: u16) -> Result<(), String> {
    match compression {
        PSD_COMPRESSION_RAW | PSD_COMPRESSION_RLE => Ok(()),
        PSD_COMPRESSION_ZIP | PSD_COMPRESSION_ZIP_PREDICTION => {
            Err("PSD/PSB ZIP Image Data cannot be opened as disk-tiled".into())
        }
        other => Err(format!("Unsupported compression: {other}")),
    }
}

#[inline]
pub(crate) fn channel_is_used(color_mode: u16, ch_idx: u32, channels: u32) -> bool {
    match color_mode {
        PSD_COLOR_MODE_GRAYSCALE => ch_idx <= 1, // Gray, Alpha
        PSD_COLOR_MODE_RGB => ch_idx <= 3,       // R, G, B, Alpha
        PSD_COLOR_MODE_CMYK => ch_idx < 4 || (channels >= 5 && ch_idx == 4), // C,M,Y,K[,A]
        PSD_COLOR_MODE_BITMAP | PSD_COLOR_MODE_INDEXED | PSD_COLOR_MODE_DUOTONE => {
            ch_idx <= 1 // single colour channel + optional alpha
        }
        PSD_COLOR_MODE_MULTICHANNEL => {
            ch_idx < channels // all channels are used; first 3→RGB, rest discarded
        }
        PSD_COLOR_MODE_LAB => ch_idx <= 3, // L, a, b, Alpha
        _ => false,
    }
}

/// Reject Bitmap / Indexed / Lab / Duotone / Multichannel etc. (checklist #15).
///
/// Only Gray / RGB / CMYK have color conversion paths.
pub(crate) fn ensure_supported_color_mode(color_mode: u16) -> Result<(), String> {
    match color_mode {
        PSD_COLOR_MODE_GRAYSCALE
        | PSD_COLOR_MODE_RGB
        | PSD_COLOR_MODE_CMYK
        | PSD_COLOR_MODE_BITMAP
        | PSD_COLOR_MODE_INDEXED
        | PSD_COLOR_MODE_MULTICHANNEL
        | PSD_COLOR_MODE_DUOTONE
        | PSD_COLOR_MODE_LAB => Ok(()),
        _ => Err(rust_i18n::t!("error.psd_unsupported_color_mode", mode = color_mode).to_string()),
    }
}

/// Convert planar samples (8/16/32-bit BE) into 8-bit display samples.
/// `dst.len()` is the sample count; `src` must hold `dst.len() * bps` bytes (or be truncated).
///
/// 16-bit uses the high byte of each BE u16 (`v >> 8`) with no gamma remapping --
/// matching Photoshop's composite storage, not a display-referred tone curve.
/// 8-bit and 16-bit sources therefore stay bit-faithful to the file even when
/// mixed depths appear across layers.
pub(crate) fn downconvert_samples_to_u8(dst: &mut [u8], src: &[u8], bps: usize) {
    let n = dst.len();
    match bps {
        1 => {
            let copy = n.min(src.len());
            dst[..copy].copy_from_slice(&src[..copy]);
            if copy < n {
                dst[copy..].fill(0);
            }
        }
        2 => crate::psb_downconvert_simd::u16be_to_u8(dst, src),
        4 => crate::psb_downconvert_simd::f32be_to_u8(dst, src),
        _ => dst.fill(0),
    }
}

/// Convert one PSD/PSB CMYK sample to approximate display RGB.
///
/// Photoshop stores CMYK channel bytes with **0 = 100% ink** and **255 = 0% ink**
/// (Adobe Photoshop File Formats Specification). The naive "0 = no ink" formula
/// inverts whites to black and turns solid 0xFF M/Y into a red cast.
#[inline]
pub(crate) fn cmyk_to_rgb(c: u8, m: u8, y: u8, k: u8) -> (u8, u8, u8) {
    let c = c as u32;
    let m = m as u32;
    let y = y as u32;
    let k = k as u32;
    let r = (c * k / 255) as u8;
    let g = (m * k / 255) as u8;
    let b = (y * k / 255) as u8;
    (r, g, b)
}

pub(crate) fn interleave_row_rgba8(
    dst_row: &mut [u8],
    planar: &[Option<Vec<u8>>],
    color_mode: u16,
    channels: u32,
    start: usize,
    end: usize,
) {
    let width = end.saturating_sub(start);
    match color_mode {
        // Bitmap (0): packed bits → grayscale.
        // Notes:
        //   - depth=1 Bitmap is handled by an early-exit path in the reader
        //     (psb_reader.rs) that calls `bitmap_bits_row_to_rgba8` directly,
        //     so this branch normally only sees depths > 1 (if any exist).
        //   - We keep it as a defensive fallback for unexpected bit depths.
        PSD_COLOR_MODE_BITMAP => {
            if let Some(src) = planar.first().and_then(|ch| ch.as_deref()) {
                if channels >= 2
                    && let Some(a_row) = planar.get(1).and_then(|ch| ch.as_deref())
                {
                    crate::psb_color_convert::bitmap_bits_row_to_rgba8(dst_row, src, width);
                    // Apply alpha from channel 1.
                    for col in 0..width {
                        dst_row[col * 4 + 3] = a_row.get(start + col).copied().unwrap_or(0xFF);
                    }
                } else {
                    crate::psb_color_convert::bitmap_bits_row_to_rgba8(dst_row, src, width);
                }
            }
        }
        // Indexed (2): palette lookup via pre-processed planar data (expanded to RGB).
        // Duotone (8): stored as single grayscale channel.
        PSD_COLOR_MODE_DUOTONE | PSD_COLOR_MODE_GRAYSCALE => {
            if let Some(g_row) = planar
                .first()
                .and_then(|ch| ch.as_ref())
                .and_then(|d| d.get(start..end))
            {
                if channels >= 2
                    && let Some(a_row) = planar
                        .get(1)
                        .and_then(|ch| ch.as_ref())
                        .and_then(|d| d.get(start..end))
                {
                    simd_swizzle::interleave_rgba(g_row, g_row, g_row, a_row, dst_row);
                } else {
                    simd_swizzle::interleave_rgb_with_alpha(g_row, g_row, g_row, 255, dst_row);
                }
            }
        }
        PSD_COLOR_MODE_RGB | PSD_COLOR_MODE_INDEXED => {
            // RGB / Indexed (pre-processed to RGB planar data)
            let r = planar
                .first()
                .and_then(|ch| ch.as_ref())
                .and_then(|d| d.get(start..end));
            let g = planar
                .get(1)
                .and_then(|ch| ch.as_ref())
                .and_then(|d| d.get(start..end));
            let b = planar
                .get(2)
                .and_then(|ch| ch.as_ref())
                .and_then(|d| d.get(start..end));
            if let (Some(r), Some(g), Some(b)) = (r, g, b) {
                if channels >= 4
                    && let Some(a_row) = planar
                        .get(3)
                        .and_then(|ch| ch.as_ref())
                        .and_then(|d| d.get(start..end))
                {
                    simd_swizzle::interleave_rgba(r, g, b, a_row, dst_row);
                } else {
                    simd_swizzle::interleave_rgb_with_alpha(r, g, b, 255, dst_row);
                }
            }
        }
        PSD_COLOR_MODE_CMYK if channels >= 4 => {
            let c = planar
                .get(0)
                .and_then(|ch| ch.as_ref())
                .and_then(|d| d.get(start..end));
            let m = planar
                .get(1)
                .and_then(|ch| ch.as_ref())
                .and_then(|d| d.get(start..end));
            let y = planar
                .get(2)
                .and_then(|ch| ch.as_ref())
                .and_then(|d| d.get(start..end));
            let k = planar
                .get(3)
                .and_then(|ch| ch.as_ref())
                .and_then(|d| d.get(start..end));
            let a = if channels >= 5 {
                planar
                    .get(4)
                    .and_then(|ch| ch.as_ref())
                    .and_then(|d| d.get(start..end))
            } else {
                None
            };
            if let (Some(c), Some(m), Some(y), Some(k)) = (c, m, y, k) {
                crate::psb_cmyk_simd::cmyk_planes_to_rgba8(c, m, y, k, a, dst_row);
            }
        }
        // Multichannel (7): first 3 channels → RGB.
        PSD_COLOR_MODE_MULTICHANNEL => {
            let ch0 = planar.get(0).and_then(|ch| ch.as_deref());
            let ch1 = planar.get(1).and_then(|ch| ch.as_deref());
            let ch2 = planar.get(2).and_then(|ch| ch.as_deref());
            let a = if channels >= 4 {
                planar.get(3).and_then(|ch| ch.as_deref())
            } else {
                None
            };
            if let (Some(c0), Some(c1), Some(c2)) = (ch0, ch1, ch2) {
                crate::psb_color_convert::multichannel_row_to_rgba8(
                    dst_row, c0, c1, c2, a, start, end,
                );
            }
        }
        // Lab (9): CIE L*a*b* → sRGB.
        PSD_COLOR_MODE_LAB => {
            let l_ch = planar.get(0).and_then(|ch| ch.as_deref());
            let a_ch = planar.get(1).and_then(|ch| ch.as_deref());
            let b_ch = planar.get(2).and_then(|ch| ch.as_deref());
            let alpha = if channels >= 4 {
                planar.get(3).and_then(|ch| ch.as_deref())
            } else {
                None
            };
            if let (Some(l), Some(a), Some(b)) = (l_ch, a_ch, b_ch) {
                crate::psb_color_convert::lab_row_to_rgba8(dst_row, l, a, b, alpha, start, end);
            }
        }
        _ => {}
    }
}

pub(crate) fn validate_psd_dimensions(
    width: u32,
    height: u32,
    channels: u32,
) -> Result<(), String> {
    if width == 0 || height == 0 {
        return Err("PSD/PSB dimensions must be non-zero".into());
    }
    if width > PSD_MAX_DIMENSION || height > PSD_MAX_DIMENSION {
        return Err(format!(
            "PSD/PSB dimensions {width}x{height} exceed maximum {PSD_MAX_DIMENSION}"
        ));
    }
    // Structural only: per-side Photoshop limit + channel count. Total-pixel
    // budget ([`MAX_DOCUMENT_PIXELS`]) applies to full-canvas decode paths, not
    // header/section parsing (disk-tiled Hubble-class PSB exceeds that budget).
    (width as u64)
        .checked_mul(height as u64)
        .ok_or_else(|| "PSD/PSB pixel count overflow".to_string())?;
    if channels == 0 || channels > PSD_MAX_CHANNELS {
        return Err(format!(
            "PSD/PSB channel count {channels} is out of range (1..={PSD_MAX_CHANNELS})"
        ));
    }
    Ok(())
}

/// Returns `width * height` as `usize`, or an error on overflow / document cap.
///
/// On success, any `row * width` with `row < height` also fits in `usize`
/// (including on 32-bit targets). Callers may rely on that instead of
/// repeating `checked_mul` in row loops.
pub(crate) fn checked_pixel_count(width: u32, height: u32) -> Result<usize, String> {
    let pixels = (width as u64)
        .checked_mul(height as u64)
        .ok_or_else(|| "PSD/PSB pixel count overflow".to_string())?;
    if pixels > MAX_DOCUMENT_PIXELS {
        return Err(format!(
            "PSD/PSB dimensions {width}x{height} exceed maximum {MAX_DOCUMENT_PIXELS} pixels"
        ));
    }
    usize::try_from(pixels).map_err(|_| "PSD/PSB pixel count overflow".into())
}

pub(crate) fn checked_rgba_len(pixel_count: usize) -> Result<usize, String> {
    pixel_count
        .checked_mul(RGBA_BYTES_PER_PIXEL)
        .ok_or_else(|| "PSD/PSB RGBA buffer size overflow".into())
}

pub(crate) fn seek_forward(r: &mut impl Seek, len: u64) -> Result<(), String> {
    if len > i64::MAX as u64 {
        return Err(format!(
            "PSD/PSB section length {len} exceeds seekable range"
        ));
    }
    r.seek(SeekFrom::Current(len as i64))
        .map_err(|e| format!("Seek error: {e}"))?;
    Ok(())
}

/// Ensure the next `len` bytes from the current position stay within `file_size`
/// before a subsequent `read_exact`, so OOB errors match [`seek_forward_within`].
pub(crate) fn ensure_readable_within(
    r: &mut impl Seek,
    len: u64,
    file_size: u64,
    label: &str,
) -> Result<(), String> {
    let pos = r
        .stream_position()
        .map_err(|e| format!("Stream position error: {e}"))?;
    let _ = checked_section_end(pos, len, file_size, label)?;
    Ok(())
}

/// Seek forward `len` bytes only when the resulting position stays within
/// `file_size`. `Cursor` allows seeking past EOF, which would otherwise defer
/// failure to a later `read_exact`.
pub(crate) fn seek_forward_within(
    r: &mut impl Seek,
    len: u64,
    file_size: u64,
    label: &str,
) -> Result<(), String> {
    // Bounds come from checked_section_end (always on, not debug_assert).
    ensure_readable_within(r, len, file_size, label)?;
    seek_forward(r, len)
}

/// Skip one unused RLE channel by summing its precomputed row counts and
/// performing a single bounded seek (instead of `height` per-row seeks).
pub(crate) fn seek_rle_channel_skip(
    r: &mut impl Seek,
    row_counts: &[usize],
    channel_idx: usize,
    height: usize,
    file_size: u64,
    label: &str,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<(), crate::loader::DecodeError> {
    let start = channel_idx
        .checked_mul(height)
        .ok_or_else(|| format!("PSD/PSB {label}: channel row index overflow"))?;
    let end = start
        .checked_add(height)
        .ok_or_else(|| format!("PSD/PSB {label}: channel row range overflow"))?;
    let counts = row_counts
        .get(start..end)
        .ok_or_else(|| format!("PSD/PSB {label}: row counts out of range ({start}..{end})"))?;

    let mut total = 0u64;
    for (i, &len) in counts.iter().enumerate() {
        if i & RLE_ROW_DECODE_CANCEL_POLL_INTERVAL == 0 {
            check_decode_cancel(cancel)?;
        }
        total = total
            .checked_add(len as u64)
            .ok_or_else(|| format!("PSD/PSB {label}: compressed length overflow"))?;
    }
    seek_forward_within(r, total, file_size, label)?;
    Ok(())
}

pub(crate) fn checked_section_end(
    start: u64,
    len: u64,
    file_size: u64,
    label: &str,
) -> Result<u64, String> {
    let end = start
        .checked_add(len)
        .ok_or_else(|| format!("PSD/PSB {label} length overflow"))?;
    if end > file_size {
        return Err(format!(
            "PSD/PSB {label} exceeds file size ({end} > {file_size})"
        ));
    }
    Ok(end)
}

pub(crate) fn validate_rle_total_bytes(row_counts: &[usize], remaining: u64) -> Result<(), String> {
    let total = row_counts.iter().try_fold(0u64, |acc, &len| {
        acc.checked_add(len as u64)
            .ok_or_else(|| "PSD/PSB RLE total length overflow".to_string())
    })?;
    if total > remaining {
        return Err(format!(
            "PSD/PSB RLE compressed data ({total} bytes) exceeds remaining file size ({remaining} bytes)"
        ));
    }
    Ok(())
}

/// PackBits can expand slightly above the raw row size; cap at 2x as a hard DoS bound.
pub(crate) fn max_rle_compressed_row_bytes(row_raw_bytes: usize) -> Result<usize, String> {
    row_raw_bytes
        .checked_mul(2)
        .ok_or_else(|| "PSD/PSB RLE row size limit overflow".to_string())
}

/// Validate each RLE row count against a per-row cap, then the total vs remaining bytes.
pub(crate) fn validate_rle_row_counts(
    row_counts: &[usize],
    remaining: u64,
    max_compressed_row_bytes: usize,
) -> Result<(), String> {
    for (i, &len) in row_counts.iter().enumerate() {
        if len > max_compressed_row_bytes {
            return Err(format!(
                "PSD/PSB RLE row {i} compressed length ({len}) exceeds limit ({max_compressed_row_bytes})"
            ));
        }
    }
    validate_rle_total_bytes(row_counts, remaining)
}

pub(crate) fn read_u16(r: &mut impl Read) -> Result<u16, String> {
    let mut buf = [0u8; 2];
    r.read_exact(&mut buf)
        .map_err(|e| format!("Read u16: {e}"))?;
    Ok(u16::from_be_bytes(buf))
}

pub(crate) fn read_u32(r: &mut impl Read) -> Result<u32, String> {
    let mut buf = [0u8; 4];
    r.read_exact(&mut buf)
        .map_err(|e| format!("Read u32: {e}"))?;
    Ok(u32::from_be_bytes(buf))
}

/// PSD uses u16 RLE row byte counts; PSB uses u32 (Photoshop file format).
#[inline]
pub(crate) fn read_rle_row_count(r: &mut impl Read, is_psb: bool) -> Result<usize, String> {
    if is_psb {
        Ok(read_u32(r)? as usize)
    } else {
        Ok(read_u16(r)? as usize)
    }
}

pub(crate) fn read_u64(r: &mut impl Read) -> Result<u64, String> {
    let mut buf = [0u8; 8];
    r.read_exact(&mut buf)
        .map_err(|e| format!("Read u64: {e}"))?;
    Ok(u64::from_be_bytes(buf))
}

/// Estimate peak resident decode buffers from header bytes.
///
/// Returns `(width, height, channels, estimated_bytes)`.
///
/// - 8-bit SDR: RGBA8 canvas + native-depth planar working set.
/// - 16/32-bit HDR flat: rgba_f32 canvas plus up to two planar copies (ZIP
///   inflate buffer coexists with per-channel `to_vec` copies before the
///   inflate buffer is dropped). Layer composite / clip scratch are not
///   known from the header alone; the RAM precheck must not under-count the
///   flat HDR peak.
pub fn estimate_memory_from_bytes(bytes: &[u8]) -> Result<(u32, u32, u32, u64), String> {
    if bytes.len() < 26 {
        return Err("PSD/PSB header is too short".into());
    }
    if &bytes[0..4] != b"8BPS" {
        return Err("Not a PSD/PSB file (invalid signature)".into());
    }
    let version = u16::from_be_bytes([bytes[4], bytes[5]]);
    if version != 1 && version != 2 {
        return Err(format!("Unknown PSD/PSB version: {version}"));
    }
    let channels = u16::from_be_bytes([bytes[12], bytes[13]]) as u32;
    let height = u32::from_be_bytes([bytes[14], bytes[15], bytes[16], bytes[17]]);
    let width = u32::from_be_bytes([bytes[18], bytes[19], bytes[20], bytes[21]]);
    let depth = u16::from_be_bytes([bytes[22], bytes[23]]);

    validate_psd_dimensions(width, height, channels)?;
    let bps = bytes_per_sample(depth)? as u64;

    let pixels = (width as u64)
        .checked_mul(height as u64)
        .ok_or_else(|| "PSD/PSB memory estimate overflow".to_string())?;
    let planar = pixels
        .checked_mul(channels as u64)
        .and_then(|n| n.checked_mul(bps))
        .ok_or_else(|| "PSD/PSB memory estimate overflow".to_string())?;
    let estimated = if depth >= 16 {
        let rgba_f32 = pixels
            .checked_mul(HDR_RGBA_F32_BYTES_PER_PIXEL)
            .ok_or_else(|| "PSD/PSB memory estimate overflow".to_string())?;
        // ZIP path may keep inflate output and per-channel copies together.
        let planar_peak = planar
            .checked_mul(2)
            .ok_or_else(|| "PSD/PSB memory estimate overflow".to_string())?;
        planar_peak
            .checked_add(rgba_f32)
            .ok_or_else(|| "PSD/PSB memory estimate overflow".to_string())?
    } else {
        let rgba = pixels
            .checked_mul(RGBA_BYTES_PER_PIXEL as u64)
            .ok_or_else(|| "PSD/PSB memory estimate overflow".to_string())?;
        rgba.checked_add(planar)
            .ok_or_else(|| "PSD/PSB memory estimate overflow".to_string())?
    };
    Ok((width, height, channels, estimated))
}
