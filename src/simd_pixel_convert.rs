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

//! SIMD-accelerated pixel lane unpack and u16 -> f32 normalization.
//!
//! Dispatch follows [`crate::simd_swizzle`]: runtime feature detection on x86_64
//! (AVX2 -> SSE4.1 -> scalar), NEON on aarch64, scalar fallback.

#[cfg(target_arch = "x86_64")]
use core::arch::x86_64::*;

#[cfg(target_arch = "aarch64")]
use core::arch::aarch64::*;

const LANES_PER_AVX2_STEP: usize = 16;
const LANES_PER_SSE41_STEP: usize = 8;
const F32S_PER_SSE41_STEP: usize = 4;

#[cfg(target_arch = "aarch64")]
const LANES_PER_NEON_STEP: usize = 8;

#[cfg(target_arch = "aarch64")]
const F32S_PER_NEON_STEP: usize = 4;

/// Zero-extend packed u8 samples into u16 lanes (one lane per channel sample).
pub fn unpack_u8_to_u16_lanes(dst: &mut [u16], src: &[u8]) {
    if dst.len() != src.len() {
        return;
    }
    let mut i = 0;

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            unsafe {
                unpack_u8_to_u16_lanes_avx2(dst, src, &mut i);
            }
        } else if is_x86_feature_detected!("sse4.1") {
            unsafe {
                unpack_u8_to_u16_lanes_sse41(dst, src, &mut i);
            }
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        unsafe {
            unpack_u8_to_u16_lanes_neon(dst, src, &mut i);
        }
    }

    while i < src.len() {
        dst[i] = src[i] as u16;
        i += 1;
    }
}

/// Copy little-endian u16 pairs from `src` bytes into `dst` lanes.
pub fn copy_le_u16_lanes(dst: &mut [u16], src: &[u8]) {
    if dst.len().saturating_mul(2) != src.len() {
        return;
    }
    let mut i = 0;

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            unsafe {
                copy_le_u16_lanes_avx2(dst, src, &mut i);
            }
        } else if is_x86_feature_detected!("sse4.1") {
            unsafe {
                copy_le_u16_lanes_sse41(dst, src, &mut i);
            }
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        unsafe {
            copy_le_u16_lanes_neon(dst, src, &mut i);
        }
    }

    while i < dst.len() {
        let byte = i * 2;
        dst[i] = u16::from_le_bytes([src[byte], src[byte + 1]]);
        i += 1;
    }
}

/// Normalize u16 lanes to f32 in `[0, 1]` via `lane * inv_scale`.
pub fn u16_lanes_to_f32(dst: &mut [f32], src: &[u16], inv_scale: f32) {
    if dst.len() != src.len() {
        return;
    }
    let mut i = 0;

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("sse4.1") {
            unsafe {
                u16_lanes_to_f32_sse41(dst, src, inv_scale, &mut i);
            }
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        unsafe {
            u16_lanes_to_f32_neon(dst, src, inv_scale, &mut i);
        }
    }

    while i < src.len() {
        dst[i] = src[i] as f32 * inv_scale;
        i += 1;
    }
}

/// Normalize one 16-bit RGB/RGBA scanline into scene-linear RGBA f32 row pixels.
///
/// `src` is contiguous sample data (`spp` u16 samples per pixel, native endian).
/// `dst` receives `width * 4` floats. Alpha is `1.0` when `spp < 4`.
pub fn normalize_uint16_rgb_scanline_to_rgba32f(
    src: &[u8],
    dst: &mut [f32],
    width: usize,
    spp: usize,
    smin: f32,
    inv_range: f32,
) {
    if !matches!(spp, 3 | 4) || dst.len() != width * 4 || src.len() < width * spp * 2 {
        return;
    }

    let mut x = 0;
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") && spp == 3 {
            unsafe {
                normalize_uint16_rgb3_scanline_avx2(src, dst, width, smin, inv_range, &mut x);
            }
        } else if is_x86_feature_detected!("sse4.1") && spp == 3 {
            unsafe {
                normalize_uint16_rgb3_scanline_sse41(src, dst, width, smin, inv_range, &mut x);
            }
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        if spp == 3 {
            unsafe {
                normalize_uint16_rgb3_scanline_neon(src, dst, width, smin, inv_range, &mut x);
            }
        }
    }

    while x < width {
        let src_base = x * spp * 2;
        let dst_base = x * 4;
        for c in 0..3 {
            let sample = read_native_u16(src, src_base + c * 2);
            dst[dst_base + c] = ((sample as f32 - smin) * inv_range).clamp(0.0, 1.0);
        }
        dst[dst_base + 3] = if spp >= 4 {
            let sample = read_native_u16(src, src_base + 6);
            ((sample as f32 - smin) * inv_range).clamp(0.0, 1.0)
        } else {
            1.0
        };
        x += 1;
    }
}

#[cfg(target_arch = "x86_64")]
#[inline]
fn store_m128_rgb_pixels(dst: &mut [f32], x: usize, r: __m128, g: __m128, b: __m128, count: usize) {
    debug_assert!(count <= 4);
    unsafe {
        let one = _mm_set1_ps(1.0);
        let rg_lo = _mm_unpacklo_ps(r, g);
        let rg_hi = _mm_unpackhi_ps(r, g);
        let ba_lo = _mm_unpacklo_ps(b, one);
        let ba_hi = _mm_unpackhi_ps(b, one);
        if count > 0 {
            _mm_storeu_ps(dst[(x) * 4..].as_mut_ptr(), _mm_movelh_ps(rg_lo, ba_lo));
        }
        if count > 1 {
            _mm_storeu_ps(dst[(x + 1) * 4..].as_mut_ptr(), _mm_movehl_ps(ba_lo, rg_lo));
        }
        if count > 2 {
            _mm_storeu_ps(dst[(x + 2) * 4..].as_mut_ptr(), _mm_movelh_ps(rg_hi, ba_hi));
        }
        if count > 3 {
            _mm_storeu_ps(dst[(x + 3) * 4..].as_mut_ptr(), _mm_movehl_ps(ba_hi, rg_hi));
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[inline]
fn store_m256_rgb_pixels(dst: &mut [f32], x: usize, r: __m256, g: __m256, b: __m256, count: usize) {
    debug_assert!(count <= 4);
    unsafe {
        store_m128_rgb_pixels(
            dst,
            x,
            _mm256_castps256_ps128(r),
            _mm256_castps256_ps128(g),
            _mm256_castps256_ps128(b),
            count,
        );
    }
}

#[inline]
fn read_native_u16(buf: &[u8], byte_offset: usize) -> u16 {
    assert!(byte_offset + 1 < buf.len());
    unsafe { std::ptr::read_unaligned(buf.as_ptr().add(byte_offset) as *const u16) }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn unpack_u8_to_u16_lanes_avx2(dst: &mut [u16], src: &[u8], i: &mut usize) {
    unsafe {
        while *i + LANES_PER_AVX2_STEP <= src.len() {
            let bytes = _mm_loadu_si128(src.as_ptr().add(*i) as *const __m128i);
            let widened = _mm256_cvtepu8_epi16(bytes);
            _mm256_storeu_si256(dst.as_mut_ptr().add(*i) as *mut __m256i, widened);
            *i += LANES_PER_AVX2_STEP;
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn unpack_u8_to_u16_lanes_sse41(dst: &mut [u16], src: &[u8], i: &mut usize) {
    unsafe {
        while *i + LANES_PER_SSE41_STEP <= src.len() {
            let bytes = _mm_loadl_epi64(src.as_ptr().add(*i) as *const __m128i);
            let widened = _mm_cvtepu8_epi16(bytes);
            _mm_storeu_si128(dst.as_mut_ptr().add(*i) as *mut __m128i, widened);
            *i += LANES_PER_SSE41_STEP;
        }
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn unpack_u8_to_u16_lanes_neon(dst: &mut [u16], src: &[u8], i: &mut usize) {
    unsafe {
        while *i + LANES_PER_NEON_STEP <= src.len() {
            let bytes = vld1_u8(src.as_ptr().add(*i));
            let widened = vmovl_u8(bytes);
            vst1q_u16(dst.as_mut_ptr().add(*i), widened);
            *i += LANES_PER_NEON_STEP;
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn copy_le_u16_lanes_avx2(dst: &mut [u16], src: &[u8], i: &mut usize) {
    unsafe {
        while *i + LANES_PER_AVX2_STEP <= dst.len() {
            let chunk = _mm256_loadu_si256(src.as_ptr().add(*i * 2) as *const __m256i);
            _mm256_storeu_si256(dst.as_mut_ptr().add(*i) as *mut __m256i, chunk);
            *i += LANES_PER_AVX2_STEP;
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn copy_le_u16_lanes_sse41(dst: &mut [u16], src: &[u8], i: &mut usize) {
    unsafe {
        while *i + LANES_PER_SSE41_STEP <= dst.len() {
            let chunk = _mm_loadu_si128(src.as_ptr().add(*i * 2) as *const __m128i);
            _mm_storeu_si128(dst.as_mut_ptr().add(*i) as *mut __m128i, chunk);
            *i += LANES_PER_SSE41_STEP;
        }
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn copy_le_u16_lanes_neon(dst: &mut [u16], src: &[u8], i: &mut usize) {
    unsafe {
        while *i + LANES_PER_NEON_STEP <= dst.len() {
            let chunk = vld1q_u16(src.as_ptr().add(*i * 2) as *const u16);
            vst1q_u16(dst.as_mut_ptr().add(*i), chunk);
            *i += LANES_PER_NEON_STEP;
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn u16_lanes_to_f32_sse41(dst: &mut [f32], src: &[u16], inv_scale: f32, i: &mut usize) {
    unsafe {
        let scale = _mm_set1_ps(inv_scale);
        while *i + F32S_PER_SSE41_STEP <= src.len() {
            let lanes = _mm_loadl_epi64(src.as_ptr().add(*i) as *const __m128i);
            let widened = _mm_cvtepu16_epi32(lanes);
            let floats = _mm_mul_ps(_mm_cvtepi32_ps(widened), scale);
            _mm_storeu_ps(dst.as_mut_ptr().add(*i), floats);
            *i += F32S_PER_SSE41_STEP;
        }
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn u16_lanes_to_f32_neon(dst: &mut [f32], src: &[u16], inv_scale: f32, i: &mut usize) {
    unsafe {
        let scale = vdupq_n_f32(inv_scale);
        while *i + F32S_PER_NEON_STEP <= src.len() {
            let lanes = vld1_u16(src.as_ptr().add(*i));
            let widened = vmovl_u16(lanes);
            let floats = vmulq_f32(vcvtq_f32_u32(widened), scale);
            vst1q_f32(dst.as_mut_ptr().add(*i), floats);
            *i += F32S_PER_NEON_STEP;
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn normalize_uint16_rgb3_scanline_avx2(
    src: &[u8],
    dst: &mut [f32],
    width: usize,
    smin: f32,
    inv_range: f32,
    x: &mut usize,
) {
    unsafe {
        let smin_v = _mm256_set1_ps(smin);
        let inv_v = _mm256_set1_ps(inv_range);
        let zero = _mm256_setzero_ps();
        let one = _mm256_set1_ps(1.0);

        while *x + 4 <= width {
            let base = *x * 6;
            let words = src.as_ptr().add(base) as *const u16;
            let r = _mm_setr_epi16(
                *words.add(0) as i16,
                *words.add(3) as i16,
                *words.add(6) as i16,
                *words.add(9) as i16,
                0,
                0,
                0,
                0,
            );
            let g = _mm_setr_epi16(
                *words.add(1) as i16,
                *words.add(4) as i16,
                *words.add(7) as i16,
                *words.add(10) as i16,
                0,
                0,
                0,
                0,
            );
            let b = _mm_setr_epi16(
                *words.add(2) as i16,
                *words.add(5) as i16,
                *words.add(8) as i16,
                *words.add(11) as i16,
                0,
                0,
                0,
                0,
            );

            let rf = _mm256_min_ps(
                _mm256_max_ps(
                    _mm256_mul_ps(
                        _mm256_sub_ps(_mm256_cvtepi32_ps(_mm256_cvtepu16_epi32(r)), smin_v),
                        inv_v,
                    ),
                    zero,
                ),
                one,
            );
            let gf = _mm256_min_ps(
                _mm256_max_ps(
                    _mm256_mul_ps(
                        _mm256_sub_ps(_mm256_cvtepi32_ps(_mm256_cvtepu16_epi32(g)), smin_v),
                        inv_v,
                    ),
                    zero,
                ),
                one,
            );
            let bf = _mm256_min_ps(
                _mm256_max_ps(
                    _mm256_mul_ps(
                        _mm256_sub_ps(_mm256_cvtepi32_ps(_mm256_cvtepu16_epi32(b)), smin_v),
                        inv_v,
                    ),
                    zero,
                ),
                one,
            );

            store_m256_rgb_pixels(dst, *x, rf, gf, bf, 4);
            *x += 4;
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn normalize_uint16_rgb3_scanline_sse41(
    src: &[u8],
    dst: &mut [f32],
    width: usize,
    smin: f32,
    inv_range: f32,
    x: &mut usize,
) {
    unsafe {
        let smin_v = _mm_set1_ps(smin);
        let inv_v = _mm_set1_ps(inv_range);
        let zero = _mm_setzero_ps();
        let one = _mm_set1_ps(1.0);

        while *x + 2 <= width {
            let base = *x * 6;
            let words = src.as_ptr().add(base) as *const u16;
            let r = _mm_setr_epi16(*words.add(0) as i16, *words.add(3) as i16, 0, 0, 0, 0, 0, 0);
            let g = _mm_setr_epi16(*words.add(1) as i16, *words.add(4) as i16, 0, 0, 0, 0, 0, 0);
            let b = _mm_setr_epi16(*words.add(2) as i16, *words.add(5) as i16, 0, 0, 0, 0, 0, 0);

            let rf = _mm_min_ps(
                _mm_max_ps(
                    _mm_mul_ps(
                        _mm_sub_ps(_mm_cvtepi32_ps(_mm_cvtepu16_epi32(r)), smin_v),
                        inv_v,
                    ),
                    zero,
                ),
                one,
            );
            let gf = _mm_min_ps(
                _mm_max_ps(
                    _mm_mul_ps(
                        _mm_sub_ps(_mm_cvtepi32_ps(_mm_cvtepu16_epi32(g)), smin_v),
                        inv_v,
                    ),
                    zero,
                ),
                one,
            );
            let bf = _mm_min_ps(
                _mm_max_ps(
                    _mm_mul_ps(
                        _mm_sub_ps(_mm_cvtepi32_ps(_mm_cvtepu16_epi32(b)), smin_v),
                        inv_v,
                    ),
                    zero,
                ),
                one,
            );

            store_m128_rgb_pixels(dst, *x, rf, gf, bf, 2);
            *x += 2;
        }
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn normalize_uint16_rgb3_scanline_neon(
    src: &[u8],
    dst: &mut [f32],
    width: usize,
    smin: f32,
    inv_range: f32,
    x: &mut usize,
) {
    unsafe {
        let smin_v = vdupq_n_f32(smin);
        let inv_v = vdupq_n_f32(inv_range);
        let zero = vdupq_n_f32(0.0);
        let one = vdupq_n_f32(1.0);

        while *x + 2 <= width {
            let base = *x * 6;
            let words = src.as_ptr().add(base) as *const u16;
            let r_u16 = vcreate_u16(u64::from(*words.add(0)) | (u64::from(*words.add(3)) << 16));
            let g_u16 = vcreate_u16(u64::from(*words.add(1)) | (u64::from(*words.add(4)) << 16));
            let b_u16 = vcreate_u16(u64::from(*words.add(2)) | (u64::from(*words.add(5)) << 16));

            let rf = vminq_f32(
                vmaxq_f32(
                    vmulq_f32(vsubq_f32(vcvtq_f32_u32(vmovl_u16(r_u16)), smin_v), inv_v),
                    zero,
                ),
                one,
            );
            let gf = vminq_f32(
                vmaxq_f32(
                    vmulq_f32(vsubq_f32(vcvtq_f32_u32(vmovl_u16(g_u16)), smin_v), inv_v),
                    zero,
                ),
                one,
            );
            let bf = vminq_f32(
                vmaxq_f32(
                    vmulq_f32(vsubq_f32(vcvtq_f32_u32(vmovl_u16(b_u16)), smin_v), inv_v),
                    zero,
                ),
                one,
            );

            let rg = vzipq_f32(rf, gf);
            let ba = vzipq_f32(bf, one);
            let rgba0 = vcombine_f32(vget_low_f32(rg.0), vget_low_f32(ba.0));
            let rgba1 = vcombine_f32(vget_high_f32(rg.0), vget_high_f32(ba.0));
            vst1q_f32(dst.as_mut_ptr().add(*x * 4), rgba0);
            vst1q_f32(dst.as_mut_ptr().add((*x + 1) * 4), rgba1);
            *x += 2;
        }
    }
}

static SRGB8_TO_LINEAR_LUT: std::sync::LazyLock<[f32; 256]> = std::sync::LazyLock::new(|| {
    let mut lut = [0.0_f32; 256];
    for (i, slot) in lut.iter_mut().enumerate() {
        let c = i as f32 / 255.0;
        *slot = if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        };
    }
    lut
});

static LINEAR_TO_SRGB8_LUT: std::sync::LazyLock<[u8; 256]> = std::sync::LazyLock::new(|| {
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

const INV_255: f32 = 1.0 / 255.0;

/// Display-referred sRGB8 RGBA -> scene-linear RGBA f32 (IEC 61966-2-1 decode on RGB; alpha linear).
pub fn srgb8_rgba_to_scene_linear_f32(src: &[u8], dst: &mut [f32]) {
    if src.len() != dst.len() || !src.len().is_multiple_of(4) {
        return;
    }
    let lut = &*SRGB8_TO_LINEAR_LUT;
    let mut px = 0;
    let pixel_count = src.len() / 4;

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("sse4.1") {
            unsafe {
                srgb8_rgba_to_scene_linear_f32_sse41(src, dst, lut, &mut px);
            }
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        unsafe {
            srgb8_rgba_to_scene_linear_f32_neon(src, dst, lut, &mut px);
        }
    }

    while px < pixel_count {
        let base = px * 4;
        dst[base] = lut[src[base] as usize];
        dst[base + 1] = lut[src[base + 1] as usize];
        dst[base + 2] = lut[src[base + 2] as usize];
        dst[base + 3] = src[base + 3] as f32 * INV_255;
        px += 1;
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn srgb8_rgba_to_scene_linear_f32_sse41(
    src: &[u8],
    dst: &mut [f32],
    lut: &[f32; 256],
    px: &mut usize,
) {
    unsafe {
        let pixel_count = src.len() / 4;
        while *px + 4 <= pixel_count {
            let base = *px * 4;
            let s = src.as_ptr().add(base);
            let d = dst.as_mut_ptr().add(base);
            for p in 0..4 {
                let o = p * 4;
                let r = lut[*s.add(o) as usize];
                let g = lut[*s.add(o + 1) as usize];
                let b = lut[*s.add(o + 2) as usize];
                let a = *s.add(o + 3) as f32 * INV_255;
                _mm_storeu_ps(d.add(o), _mm_set_ps(a, b, g, r));
            }
            *px += 4;
        }
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn srgb8_rgba_to_scene_linear_f32_neon(
    src: &[u8],
    dst: &mut [f32],
    lut: &[f32; 256],
    px: &mut usize,
) {
    unsafe {
        let pixel_count = src.len() / 4;
        while *px + 2 <= pixel_count {
            let base = *px * 4;
            let s = src.as_ptr().add(base);
            let d = dst.as_mut_ptr().add(base);
            for p in 0..2 {
                let o = p * 4;
                let rgba = [
                    lut[*s.add(o) as usize],
                    lut[*s.add(o + 1) as usize],
                    lut[*s.add(o + 2) as usize],
                    *s.add(o + 3) as f32 * INV_255,
                ];
                vst1q_f32(d.add(o), vld1q_f32(rgba.as_ptr()));
            }
            *px += 2;
        }
    }
}

#[inline]
fn finite_f32(v: f32) -> f32 {
    if v.is_finite() { v } else { 0.0 }
}

#[inline]
fn linear_to_srgb8_index(linear: f32) -> u8 {
    let index = (linear.clamp(0.0, 1.0) * 255.0).round() as usize;
    LINEAR_TO_SRGB8_LUT[index.min(255)]
}

/// `(pivot - v).max(0)` on gray stored in every RGBA pixel (stride-4 layout).
pub fn invert_miniswhite_rgba32f(buf: &mut [f32], width: usize, height: usize, pivot: f32) {
    if width == 0 || height == 0 || buf.len() < width * height * 4 {
        return;
    }
    let pivot_v = pivot;
    for y in 0..height {
        let row = &mut buf[y * width * 4..(y + 1) * width * 4];
        invert_miniswhite_rgba32f_row(row, width, pivot_v);
    }
}

fn invert_miniswhite_rgba32f_row(row: &mut [f32], width: usize, pivot: f32) {
    let mut x = 0;

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("sse4.1") {
            unsafe {
                invert_miniswhite_rgba32f_row_sse41(row, width, pivot, &mut x);
            }
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        unsafe {
            invert_miniswhite_rgba32f_row_neon(row, width, pivot, &mut x);
        }
    }

    while x < width {
        let i = x * 4;
        let g = (pivot - row[i]).max(0.0);
        row[i] = g;
        row[i + 1] = g;
        row[i + 2] = g;
        row[i + 3] = 1.0;
        x += 1;
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn invert_miniswhite_rgba32f_row_sse41(
    row: &mut [f32],
    width: usize,
    pivot: f32,
    x: &mut usize,
) {
    unsafe {
        let pivot_v = _mm_set1_ps(pivot);
        let zero = _mm_setzero_ps();
        let one = _mm_set1_ps(1.0);
        while *x + 4 <= width {
            let base = *x * 4;
            let v = _mm_set_ps(row[base + 12], row[base + 8], row[base + 4], row[base]);
            let g = _mm_max_ps(_mm_sub_ps(pivot_v, v), zero);
            store_m128_rgb_pixels(row, *x, g, g, g, 4);
            let dst = row.as_mut_ptr().add(base);
            for p in 0..4 {
                *dst.add(p * 4 + 3) = 1.0;
            }
            *x += 4;
        }
        let _ = one;
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn invert_miniswhite_rgba32f_row_neon(
    row: &mut [f32],
    width: usize,
    pivot: f32,
    x: &mut usize,
) {
    unsafe {
        let pivot_v = vdupq_n_f32(pivot);
        let zero = vdupq_n_f32(0.0);
        while *x + 4 <= width {
            let base = *x * 4;
            let v = vsetq_lane_f32(
                row[base + 12],
                vsetq_lane_f32(
                    row[base + 8],
                    vsetq_lane_f32(
                        row[base + 4],
                        vsetq_lane_f32(row[base], vdupq_n_f32(0.0), 0),
                        1,
                    ),
                    2,
                ),
                3,
            );
            let g = vmaxq_f32(vsubq_f32(pivot_v, v), zero);
            store_f32x4_gray_as_rgba(row, *x, g);
            *x += 4;
        }
    }
}

#[cfg(target_arch = "aarch64")]
#[inline]
unsafe fn store_f32x4_gray_as_rgba(row: &mut [f32], x: usize, g: float32x4_t) {
    unsafe {
        let mut lanes = [0.0_f32; 4];
        vst1q_f32(lanes.as_mut_ptr(), g);
        for p in 0..4 {
            let gp = lanes[p];
            let o = (x + p) * 4;
            row[o] = gp;
            row[o + 1] = gp;
            row[o + 2] = gp;
            row[o + 3] = 1.0;
        }
    }
}

/// One IEEE-f32 gray scanline -> RGBA f32 row. `invert_pivot = Some(p)` applies MINISWHITE inversion.
pub fn ieee_f32_gray_scanline_to_rgba32f(
    src: &[u8],
    dst: &mut [f32],
    width: usize,
    invert_pivot: Option<f32>,
) {
    if dst.len() < width * 4 || src.len() < width * 4 {
        return;
    }
    let mut x = 0;

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("sse4.1") {
            unsafe {
                ieee_f32_gray_scanline_to_rgba32f_sse41(src, dst, width, invert_pivot, &mut x);
            }
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        unsafe {
            ieee_f32_gray_scanline_to_rgba32f_neon(src, dst, width, invert_pivot, &mut x);
        }
    }

    while x < width {
        let raw = f32::from_ne_bytes([src[x * 4], src[x * 4 + 1], src[x * 4 + 2], src[x * 4 + 3]]);
        let v = finite_f32(raw);
        let g = match invert_pivot {
            Some(pivot) => (pivot - v).max(0.0),
            None => v,
        };
        let o = x * 4;
        dst[o] = g;
        dst[o + 1] = g;
        dst[o + 2] = g;
        dst[o + 3] = 1.0;
        x += 1;
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn ieee_f32_gray_scanline_to_rgba32f_sse41(
    src: &[u8],
    dst: &mut [f32],
    width: usize,
    invert_pivot: Option<f32>,
    x: &mut usize,
) {
    unsafe {
        let zero = _mm_setzero_ps();
        let one = _mm_set1_ps(1.0);
        let pivot_v = invert_pivot.map(|p| _mm_set1_ps(p));
        while *x + 4 <= width {
            let v = _mm_loadu_ps(src.as_ptr().add(*x * 4) as *const f32);
            let finite = _mm_and_ps(v, _mm_cmpord_ps(v, v));
            let g = if let Some(pivot) = pivot_v {
                _mm_max_ps(_mm_sub_ps(pivot, finite), zero)
            } else {
                finite
            };
            store_m128_rgb_pixels(dst, *x, g, g, g, 4);
            let dst_ptr = dst.as_mut_ptr().add(*x * 4);
            for p in 0..4 {
                *dst_ptr.add(p * 4 + 3) = 1.0;
            }
            *x += 4;
        }
        let _ = one;
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn ieee_f32_gray_scanline_to_rgba32f_neon(
    src: &[u8],
    dst: &mut [f32],
    width: usize,
    invert_pivot: Option<f32>,
    x: &mut usize,
) {
    unsafe {
        let zero = vdupq_n_f32(0.0);
        let pivot_v = invert_pivot.map(vdupq_n_f32);
        while *x + 4 <= width {
            let v = vld1q_f32(src.as_ptr().add(*x * 4) as *const f32);
            let finite = vbslq_f32(vreinterpretq_u32_f32(vceqq_f32(v, v)), v, zero);
            let g = if let Some(pivot) = pivot_v {
                vmaxq_f32(vsubq_f32(pivot, finite), zero)
            } else {
                finite
            };
            store_f32x4_gray_as_rgba(dst, *x, g);
            *x += 4;
        }
    }
}

/// Contiguous IEEE-f32 RGB (3 or 4 samples per pixel) -> RGBA f32 row.
pub fn ieee_f32_rgb_scanline_to_rgba32f(src: &[u8], dst: &mut [f32], width: usize, spp: usize) {
    if dst.len() < width * 4 {
        return;
    }
    let bytes_per_pixel = spp * 4;
    if src.len() < width * bytes_per_pixel {
        return;
    }
    let mut x = 0;

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("sse4.1") && spp == 3 {
            unsafe {
                ieee_f32_rgb3_scanline_to_rgba32f_sse41(src, dst, width, &mut x);
            }
        }
    }

    while x < width {
        let src_base = x * bytes_per_pixel;
        let dst_base = x * 4;
        for c in 0..3 {
            let raw = f32::from_ne_bytes([
                src[src_base + c * 4],
                src[src_base + c * 4 + 1],
                src[src_base + c * 4 + 2],
                src[src_base + c * 4 + 3],
            ]);
            dst[dst_base + c] = finite_f32(raw);
        }
        dst[dst_base + 3] = if spp >= 4 {
            let raw = f32::from_ne_bytes([
                src[src_base + 12],
                src[src_base + 13],
                src[src_base + 14],
                src[src_base + 15],
            ]);
            finite_f32(raw)
        } else {
            1.0
        };
        x += 1;
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn ieee_f32_rgb3_scanline_to_rgba32f_sse41(
    src: &[u8],
    dst: &mut [f32],
    width: usize,
    x: &mut usize,
) {
    unsafe {
        while *x + 4 <= width {
            let base = *x * 12;
            let s = src.as_ptr().add(base);
            let r = _mm_set_ps(
                finite_f32(f32::from_ne_bytes([
                    *s.add(36),
                    *s.add(37),
                    *s.add(38),
                    *s.add(39),
                ])),
                finite_f32(f32::from_ne_bytes([
                    *s.add(24),
                    *s.add(25),
                    *s.add(26),
                    *s.add(27),
                ])),
                finite_f32(f32::from_ne_bytes([
                    *s.add(12),
                    *s.add(13),
                    *s.add(14),
                    *s.add(15),
                ])),
                finite_f32(f32::from_ne_bytes([
                    *s.add(0),
                    *s.add(1),
                    *s.add(2),
                    *s.add(3),
                ])),
            );
            let g = _mm_set_ps(
                finite_f32(f32::from_ne_bytes([
                    *s.add(40),
                    *s.add(41),
                    *s.add(42),
                    *s.add(43),
                ])),
                finite_f32(f32::from_ne_bytes([
                    *s.add(28),
                    *s.add(29),
                    *s.add(30),
                    *s.add(31),
                ])),
                finite_f32(f32::from_ne_bytes([
                    *s.add(16),
                    *s.add(17),
                    *s.add(18),
                    *s.add(19),
                ])),
                finite_f32(f32::from_ne_bytes([
                    *s.add(4),
                    *s.add(5),
                    *s.add(6),
                    *s.add(7),
                ])),
            );
            let b = _mm_set_ps(
                finite_f32(f32::from_ne_bytes([
                    *s.add(44),
                    *s.add(45),
                    *s.add(46),
                    *s.add(47),
                ])),
                finite_f32(f32::from_ne_bytes([
                    *s.add(32),
                    *s.add(33),
                    *s.add(34),
                    *s.add(35),
                ])),
                finite_f32(f32::from_ne_bytes([
                    *s.add(20),
                    *s.add(21),
                    *s.add(22),
                    *s.add(23),
                ])),
                finite_f32(f32::from_ne_bytes([
                    *s.add(8),
                    *s.add(9),
                    *s.add(10),
                    *s.add(11),
                ])),
            );
            store_m128_rgb_pixels(dst, *x, r, g, b, 4);
            *x += 4;
        }
    }
}

/// Normalize one grayscale scratch row to 8-bit RGBA (MINISBLACK / MINISWHITE).
pub fn finalize_gray_linear_scratch_row_to_rgba8(
    scratch_row: &[f32],
    rgba_row: &mut [u8],
    width: usize,
    smin: f64,
    smax: f64,
    miniswhite: bool,
) {
    if scratch_row.len() < width * 4 || rgba_row.len() < width * 4 {
        return;
    }
    let range = (smax - smin).max(f64::EPSILON);
    let inv_range = 1.0 / range;
    let mut x = 0;

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("sse4.1") {
            unsafe {
                finalize_gray_linear_scratch_row_to_rgba8_sse41(
                    scratch_row,
                    rgba_row,
                    width,
                    smin,
                    inv_range,
                    miniswhite,
                    &mut x,
                );
            }
        }
    }

    while x < width {
        let scratch_idx = x * 4;
        let dst_idx = x * 4;
        let f_val = scratch_row[scratch_idx] as f64;
        let linear = ((f_val - smin) * inv_range).clamp(0.0, 1.0) as f32;
        let sample = linear_to_srgb8_index(linear);
        let v = if miniswhite {
            255_u8.saturating_sub(sample)
        } else {
            sample
        };
        rgba_row[dst_idx] = v;
        rgba_row[dst_idx + 1] = v;
        rgba_row[dst_idx + 2] = v;
        rgba_row[dst_idx + 3] = 255;
        x += 1;
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn finalize_gray_linear_scratch_row_to_rgba8_sse41(
    scratch_row: &[f32],
    rgba_row: &mut [u8],
    width: usize,
    smin: f64,
    inv_range: f64,
    miniswhite: bool,
    x: &mut usize,
) {
    unsafe {
        let smin_v = _mm_set1_ps(smin as f32);
        let inv_v = _mm_set1_ps(inv_range as f32);
        let zero = _mm_setzero_ps();
        let one = _mm_set1_ps(1.0);
        while *x + 4 <= width {
            let base = *x * 4;
            let v = _mm_set_ps(
                scratch_row[base + 12],
                scratch_row[base + 8],
                scratch_row[base + 4],
                scratch_row[base],
            );
            let linear = _mm_min_ps(
                _mm_max_ps(_mm_mul_ps(_mm_sub_ps(v, smin_v), inv_v), zero),
                one,
            );
            let mut lanes = [0.0_f32; 4];
            _mm_storeu_ps(lanes.as_mut_ptr(), linear);
            for (p, &linear) in lanes.iter().enumerate() {
                let sample = linear_to_srgb8_index(linear);
                let byte = if miniswhite {
                    255_u8.saturating_sub(sample)
                } else {
                    sample
                };
                let o = (base + p * 4) as isize;
                *rgba_row.as_mut_ptr().offset(o) = byte;
                *rgba_row.as_mut_ptr().offset(o + 1) = byte;
                *rgba_row.as_mut_ptr().offset(o + 2) = byte;
                *rgba_row.as_mut_ptr().offset(o + 3) = 255;
            }
            *x += 4;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unpack_u8_scalar(dst: &mut [u16], src: &[u8]) {
        for (lane, &byte) in dst.iter_mut().zip(src.iter()) {
            *lane = byte as u16;
        }
    }

    fn copy_le_u16_scalar(dst: &mut [u16], src: &[u8]) {
        for (i, lane) in dst.iter_mut().enumerate() {
            let byte = i * 2;
            *lane = u16::from_le_bytes([src[byte], src[byte + 1]]);
        }
    }

    fn u16_to_f32_scalar(dst: &mut [f32], src: &[u16], inv_scale: f32) {
        for (out, &lane) in dst.iter_mut().zip(src.iter()) {
            *out = lane as f32 * inv_scale;
        }
    }

    fn normalize_rgb3_scalar(src: &[u8], dst: &mut [f32], width: usize, smin: f32, inv_range: f32) {
        for x in 0..width {
            let src_base = x * 6;
            let dst_base = x * 4;
            for c in 0..3 {
                let sample = read_native_u16(src, src_base + c * 2);
                dst[dst_base + c] = ((sample as f32 - smin) * inv_range).clamp(0.0, 1.0);
            }
            dst[dst_base + 3] = 1.0;
        }
    }

    const PARITY_LENGTHS: &[usize] = &[0, 1, 7, 8, 9, 15, 16, 17, 31, 32, 33, 64];

    #[test]
    fn unpack_u8_to_u16_lanes_matches_scalar() {
        for len in PARITY_LENGTHS {
            let src: Vec<u8> = (0..*len).map(|i| ((i * 13 + 7) % 256) as u8).collect();
            let mut simd_dst = vec![0_u16; *len];
            let mut scalar_dst = vec![0_u16; *len];
            unpack_u8_to_u16_lanes(&mut simd_dst, &src);
            unpack_u8_scalar(&mut scalar_dst, &src);
            assert_eq!(simd_dst, scalar_dst, "len={len}");
        }
    }

    #[test]
    fn copy_le_u16_lanes_matches_scalar() {
        for len in PARITY_LENGTHS {
            let src: Vec<u8> = (0..len * 2).map(|i| ((i * 17 + 3) % 256) as u8).collect();
            let mut simd_dst = vec![0_u16; *len];
            let mut scalar_dst = vec![0_u16; *len];
            copy_le_u16_lanes(&mut simd_dst, &src);
            copy_le_u16_scalar(&mut scalar_dst, &src);
            assert_eq!(simd_dst, scalar_dst, "len={len}");
        }
    }

    #[test]
    fn u16_lanes_to_f32_matches_scalar() {
        let inv_scale = 1.0 / 65535.0;
        for len in PARITY_LENGTHS {
            let src: Vec<u16> = (0..*len).map(|i| (i * 997 + 13) as u16).collect();
            let mut simd_dst = vec![0.0_f32; *len];
            let mut scalar_dst = vec![0.0_f32; *len];
            u16_lanes_to_f32(&mut simd_dst, &src, inv_scale);
            u16_to_f32_scalar(&mut scalar_dst, &src, inv_scale);
            assert_eq!(simd_dst, scalar_dst, "len={len}");
        }
    }

    #[test]
    fn normalize_uint16_rgb_scanline_matches_scalar() {
        for width in [0, 1, 2, 3, 4, 7, 8, 9, 16] {
            let src: Vec<u8> = (0..width * 6).map(|i| ((i * 11 + 5) % 256) as u8).collect();
            let mut simd_dst = vec![0.0_f32; width * 4];
            let mut scalar_dst = vec![0.0_f32; width * 4];
            let smin = 100.0_f32;
            let inv_range = 1.0 / 60000.0_f32;
            normalize_uint16_rgb_scanline_to_rgba32f(
                &src,
                &mut simd_dst,
                width,
                3,
                smin,
                inv_range,
            );
            normalize_rgb3_scalar(&src, &mut scalar_dst, width, smin, inv_range);
            assert_eq!(simd_dst, scalar_dst, "width={width}");
        }
    }

    fn srgb8_to_linear_scalar(src: &[u8]) -> Vec<f32> {
        let mut dst = Vec::with_capacity(src.len());
        for px in src.chunks_exact(4) {
            for (i, &byte) in px.iter().enumerate() {
                if i < 3 {
                    let c = byte as f32 / 255.0;
                    dst.push(if c <= 0.04045 {
                        c / 12.92
                    } else {
                        ((c + 0.055) / 1.055).powf(2.4)
                    });
                } else {
                    dst.push(byte as f32 * INV_255);
                }
            }
        }
        dst
    }

    #[test]
    fn srgb8_rgba_to_scene_linear_matches_scalar() {
        for len in [4, 8, 12, 16, 20, 32] {
            let src: Vec<u8> = (0..len).map(|i| ((i * 19 + 7) % 256) as u8).collect();
            let mut simd_dst = vec![0.0_f32; len];
            srgb8_rgba_to_scene_linear_f32(&src, &mut simd_dst);
            let scalar_dst = srgb8_to_linear_scalar(&src);
            assert_eq!(simd_dst, scalar_dst, "len={len}");
        }
    }

    #[test]
    fn invert_miniswhite_rgba32f_matches_scalar() {
        for width in [1, 3, 4, 7, 8, 15] {
            let mut simd_buf: Vec<f32> = (0..width * 4).map(|i| (i as f32 * 0.03) % 2.0).collect();
            let mut scalar_buf = simd_buf.clone();
            let pivot = 1.25_f32;
            invert_miniswhite_rgba32f(&mut simd_buf, width, 1, pivot);
            for x in 0..width {
                let i = x * 4;
                let g = (pivot - scalar_buf[i]).max(0.0);
                scalar_buf[i] = g;
                scalar_buf[i + 1] = g;
                scalar_buf[i + 2] = g;
                scalar_buf[i + 3] = 1.0;
            }
            assert_eq!(simd_buf, scalar_buf, "width={width}");
        }
    }

    #[test]
    fn ieee_f32_gray_scanline_matches_scalar() {
        for width in [1, 4, 7, 8] {
            let src: Vec<u8> = (0..width * 4)
                .flat_map(|i| ((i as f32 * 0.11) as f32).to_ne_bytes())
                .collect();
            let mut simd_dst = vec![0.0_f32; width * 4];
            let mut scalar_dst = vec![0.0_f32; width * 4];
            ieee_f32_gray_scanline_to_rgba32f(&src, &mut simd_dst, width, None);
            for x in 0..width {
                let v = f32::from_ne_bytes([
                    src[x * 4],
                    src[x * 4 + 1],
                    src[x * 4 + 2],
                    src[x * 4 + 3],
                ]);
                let g = if v.is_finite() { v } else { 0.0 };
                let o = x * 4;
                scalar_dst[o..o + 4].copy_from_slice(&[g, g, g, 1.0]);
            }
            assert_eq!(simd_dst, scalar_dst, "width={width}");
        }
    }
}
