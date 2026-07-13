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

//! SIMD planar HDR sample interleave for PSD/PSB flattened Image Data.
//!
//! Converts big-endian planar channels into interleaved RGBA f32:
//! - 16-bit: BE u16 / 65535.0 (normalized [0, 1])
//! - 32-bit: BE IEEE-754 float (Photoshop linear light, values may exceed 1.0)
//!
//! Alpha is clamped to [0, 1]. Missing color planes read as 0.0; missing alpha
//! reads as 1.0. Transfer decoding is left to the caller.

const U16_BYTES: usize = 2;
const F32_BYTES: usize = 4;
const RGBA_F32: usize = 4;
const INV_U16: f32 = 1.0 / 65535.0;

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
const PIXELS_PER_SSE_OR_NEON: usize = 4;
#[cfg(target_arch = "x86_64")]
const PIXELS_PER_AVX2: usize = 8;

/// Planar BE u16 RGB(+A) -> interleaved RGBA f32 (normalized).
pub fn interleave_planar_u16be_rgba_f32(
    r: Option<&[u8]>,
    g: Option<&[u8]>,
    b: Option<&[u8]>,
    a: Option<&[u8]>,
    dst: &mut [f32],
    pixel_count: usize,
) {
    if pixel_count == 0 || dst.len() < pixel_count * RGBA_F32 {
        return;
    }
    let dst = &mut dst[..pixel_count * RGBA_F32];

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            unsafe {
                interleave_u16be_avx2(r, g, b, a, dst, pixel_count);
            }
            return;
        }
        // `_mm_shuffle_epi8` needs SSSE3; `_mm_cvtepu16_epi32` needs SSE4.1.
        if is_x86_feature_detected!("sse4.1") && is_x86_feature_detected!("ssse3") {
            unsafe {
                interleave_u16be_sse41(r, g, b, a, dst, pixel_count);
            }
            return;
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        unsafe {
            interleave_u16be_neon(r, g, b, a, dst, pixel_count);
        }
        return;
    }

    #[cfg(not(target_arch = "aarch64"))]
    {
        interleave_u16be_scalar(r, g, b, a, dst, pixel_count);
    }
}

/// Planar BE f32 RGB(+A) -> interleaved RGBA f32 (endian-swapped passthrough).
pub fn interleave_planar_f32be_rgba_f32(
    r: Option<&[u8]>,
    g: Option<&[u8]>,
    b: Option<&[u8]>,
    a: Option<&[u8]>,
    dst: &mut [f32],
    pixel_count: usize,
) {
    if pixel_count == 0 || dst.len() < pixel_count * RGBA_F32 {
        return;
    }
    let dst = &mut dst[..pixel_count * RGBA_F32];

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            unsafe {
                interleave_f32be_avx2(r, g, b, a, dst, pixel_count);
            }
            return;
        }
        // `_mm_shuffle_epi8` (BE->LE) requires SSSE3 in addition to SSE4.1.
        if is_x86_feature_detected!("sse4.1") && is_x86_feature_detected!("ssse3") {
            unsafe {
                interleave_f32be_sse41(r, g, b, a, dst, pixel_count);
            }
            return;
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        unsafe {
            interleave_f32be_neon(r, g, b, a, dst, pixel_count);
        }
        return;
    }

    #[cfg(not(target_arch = "aarch64"))]
    {
        interleave_f32be_scalar(r, g, b, a, dst, pixel_count);
    }
}

/// Planar BE u16 gray(+A) -> interleaved RGBA f32 (gray replicated to RGB).
pub fn interleave_planar_u16be_gray_f32(
    gray: Option<&[u8]>,
    a: Option<&[u8]>,
    dst: &mut [f32],
    pixel_count: usize,
) {
    interleave_planar_u16be_rgba_f32(gray, gray, gray, a, dst, pixel_count);
}

/// Planar BE f32 gray(+A) -> interleaved RGBA f32 (gray replicated to RGB).
pub fn interleave_planar_f32be_gray_f32(
    gray: Option<&[u8]>,
    a: Option<&[u8]>,
    dst: &mut [f32],
    pixel_count: usize,
) {
    interleave_planar_f32be_rgba_f32(gray, gray, gray, a, dst, pixel_count);
}

#[inline]
fn sample_u16be(channel: Option<&[u8]>, i: usize) -> f32 {
    let Some(ch) = channel else {
        return 0.0;
    };
    let off = i * U16_BYTES;
    if off + U16_BYTES > ch.len() {
        return 0.0;
    }
    u16::from_be_bytes([ch[off], ch[off + 1]]) as f32 * INV_U16
}

#[inline]
fn sample_f32be(channel: Option<&[u8]>, i: usize) -> f32 {
    let Some(ch) = channel else {
        return 0.0;
    };
    let off = i * F32_BYTES;
    if off + F32_BYTES > ch.len() {
        return 0.0;
    }
    f32::from_be_bytes([ch[off], ch[off + 1], ch[off + 2], ch[off + 3]])
}

#[inline]
fn alpha_u16be(channel: Option<&[u8]>, i: usize) -> f32 {
    match channel {
        Some(_) => sample_u16be(channel, i).clamp(0.0, 1.0),
        None => 1.0,
    }
}

#[inline]
fn alpha_f32be(channel: Option<&[u8]>, i: usize) -> f32 {
    match channel {
        Some(_) => sample_f32be(channel, i).clamp(0.0, 1.0),
        None => 1.0,
    }
}

fn interleave_u16be_scalar(
    r: Option<&[u8]>,
    g: Option<&[u8]>,
    b: Option<&[u8]>,
    a: Option<&[u8]>,
    dst: &mut [f32],
    pixel_count: usize,
) {
    for i in 0..pixel_count {
        let base = i * RGBA_F32;
        dst[base] = sample_u16be(r, i);
        dst[base + 1] = sample_u16be(g, i);
        dst[base + 2] = sample_u16be(b, i);
        dst[base + 3] = alpha_u16be(a, i);
    }
}

fn interleave_f32be_scalar(
    r: Option<&[u8]>,
    g: Option<&[u8]>,
    b: Option<&[u8]>,
    a: Option<&[u8]>,
    dst: &mut [f32],
    pixel_count: usize,
) {
    for i in 0..pixel_count {
        let base = i * RGBA_F32;
        dst[base] = sample_f32be(r, i);
        dst[base + 1] = sample_f32be(g, i);
        dst[base + 2] = sample_f32be(b, i);
        dst[base + 3] = alpha_f32be(a, i);
    }
}

#[cfg(target_arch = "x86_64")]
#[inline]
unsafe fn load_u16be_f32x4(
    channel: Option<&[u8]>,
    i: usize,
    inv: core::arch::x86_64::__m128,
) -> core::arch::x86_64::__m128 {
    use core::arch::x86_64::*;
    unsafe {
        let Some(ch) = channel else {
            return _mm_setzero_ps();
        };
        let off = i * U16_BYTES;
        if off + PIXELS_PER_SSE_OR_NEON * U16_BYTES > ch.len() {
            return _mm_setzero_ps();
        }
        // Swap bytes within each u16: BE [hi,lo] -> host numeric value.
        let swap = _mm_setr_epi8(1, 0, 3, 2, 5, 4, 7, 6, 9, 8, 11, 10, 13, 12, 15, 14);
        let be = _mm_loadl_epi64(ch.as_ptr().add(off).cast());
        let le = _mm_shuffle_epi8(be, swap);
        let widened = _mm_cvtepu16_epi32(le);
        _mm_mul_ps(_mm_cvtepi32_ps(widened), inv)
    }
}

#[cfg(target_arch = "x86_64")]
#[inline]
unsafe fn load_f32be_x4(channel: Option<&[u8]>, i: usize) -> core::arch::x86_64::__m128 {
    use core::arch::x86_64::*;
    unsafe {
        let Some(ch) = channel else {
            return _mm_setzero_ps();
        };
        let off = i * F32_BYTES;
        if off + PIXELS_PER_SSE_OR_NEON * F32_BYTES > ch.len() {
            return _mm_setzero_ps();
        }
        let swap = _mm_setr_epi8(3, 2, 1, 0, 7, 6, 5, 4, 11, 10, 9, 8, 15, 14, 13, 12);
        let be = _mm_loadu_si128(ch.as_ptr().add(off).cast());
        _mm_castsi128_ps(_mm_shuffle_epi8(be, swap))
    }
}

#[cfg(target_arch = "x86_64")]
#[inline]
unsafe fn store_rgba_f32x4(
    dst: &mut [f32],
    i: usize,
    r: core::arch::x86_64::__m128,
    g: core::arch::x86_64::__m128,
    b: core::arch::x86_64::__m128,
    a: core::arch::x86_64::__m128,
) {
    use core::arch::x86_64::*;
    unsafe {
        let rg_lo = _mm_unpacklo_ps(r, g);
        let rg_hi = _mm_unpackhi_ps(r, g);
        let ba_lo = _mm_unpacklo_ps(b, a);
        let ba_hi = _mm_unpackhi_ps(b, a);
        let out = dst.as_mut_ptr().add(i * RGBA_F32);
        _mm_storeu_ps(out, _mm_movelh_ps(rg_lo, ba_lo));
        _mm_storeu_ps(out.add(4), _mm_movehl_ps(ba_lo, rg_lo));
        _mm_storeu_ps(out.add(8), _mm_movelh_ps(rg_hi, ba_hi));
        _mm_storeu_ps(out.add(12), _mm_movehl_ps(ba_hi, rg_hi));
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1,ssse3")]
unsafe fn interleave_u16be_sse41(
    r: Option<&[u8]>,
    g: Option<&[u8]>,
    b: Option<&[u8]>,
    a: Option<&[u8]>,
    dst: &mut [f32],
    pixel_count: usize,
) {
    use core::arch::x86_64::*;
    let inv = _mm_set1_ps(INV_U16);
    let zero = _mm_setzero_ps();
    let one = _mm_set1_ps(1.0);
    let mut i = 0usize;
    unsafe {
        while i + PIXELS_PER_SSE_OR_NEON <= pixel_count {
            let rv = load_u16be_f32x4(r, i, inv);
            let gv = load_u16be_f32x4(g, i, inv);
            let bv = load_u16be_f32x4(b, i, inv);
            let av = if a.is_some() {
                _mm_min_ps(_mm_max_ps(load_u16be_f32x4(a, i, inv), zero), one)
            } else {
                one
            };
            store_rgba_f32x4(dst, i, rv, gv, bv, av);
            i += PIXELS_PER_SSE_OR_NEON;
        }
    }
    if i < pixel_count {
        interleave_u16be_scalar(
            r.map(|c| &c[i * U16_BYTES..]),
            g.map(|c| &c[i * U16_BYTES..]),
            b.map(|c| &c[i * U16_BYTES..]),
            a.map(|c| &c[i * U16_BYTES..]),
            &mut dst[i * RGBA_F32..],
            pixel_count - i,
        );
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1,ssse3")]
unsafe fn interleave_f32be_sse41(
    r: Option<&[u8]>,
    g: Option<&[u8]>,
    b: Option<&[u8]>,
    a: Option<&[u8]>,
    dst: &mut [f32],
    pixel_count: usize,
) {
    use core::arch::x86_64::*;
    let zero = _mm_setzero_ps();
    let one = _mm_set1_ps(1.0);
    let mut i = 0usize;
    unsafe {
        while i + PIXELS_PER_SSE_OR_NEON <= pixel_count {
            let rv = load_f32be_x4(r, i);
            let gv = load_f32be_x4(g, i);
            let bv = load_f32be_x4(b, i);
            let av = if a.is_some() {
                _mm_min_ps(_mm_max_ps(load_f32be_x4(a, i), zero), one)
            } else {
                one
            };
            store_rgba_f32x4(dst, i, rv, gv, bv, av);
            i += PIXELS_PER_SSE_OR_NEON;
        }
    }
    if i < pixel_count {
        interleave_f32be_scalar(
            r.map(|c| &c[i * F32_BYTES..]),
            g.map(|c| &c[i * F32_BYTES..]),
            b.map(|c| &c[i * F32_BYTES..]),
            a.map(|c| &c[i * F32_BYTES..]),
            &mut dst[i * RGBA_F32..],
            pixel_count - i,
        );
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,sse4.1,ssse3")]
unsafe fn interleave_u16be_avx2(
    r: Option<&[u8]>,
    g: Option<&[u8]>,
    b: Option<&[u8]>,
    a: Option<&[u8]>,
    dst: &mut [f32],
    pixel_count: usize,
) {
    use core::arch::x86_64::*;
    let inv = _mm_set1_ps(INV_U16);
    let zero = _mm_setzero_ps();
    let one = _mm_set1_ps(1.0);
    let mut i = 0usize;
    unsafe {
        while i + PIXELS_PER_AVX2 <= pixel_count {
            for lane in 0..2 {
                let base = i + lane * PIXELS_PER_SSE_OR_NEON;
                let rv = load_u16be_f32x4(r, base, inv);
                let gv = load_u16be_f32x4(g, base, inv);
                let bv = load_u16be_f32x4(b, base, inv);
                let av = if a.is_some() {
                    _mm_min_ps(_mm_max_ps(load_u16be_f32x4(a, base, inv), zero), one)
                } else {
                    one
                };
                store_rgba_f32x4(dst, base, rv, gv, bv, av);
            }
            i += PIXELS_PER_AVX2;
        }
        if i + PIXELS_PER_SSE_OR_NEON <= pixel_count {
            interleave_u16be_sse41(
                r.map(|c| &c[i * U16_BYTES..]),
                g.map(|c| &c[i * U16_BYTES..]),
                b.map(|c| &c[i * U16_BYTES..]),
                a.map(|c| &c[i * U16_BYTES..]),
                &mut dst[i * RGBA_F32..],
                pixel_count - i,
            );
            return;
        }
    }
    if i < pixel_count {
        interleave_u16be_scalar(
            r.map(|c| &c[i * U16_BYTES..]),
            g.map(|c| &c[i * U16_BYTES..]),
            b.map(|c| &c[i * U16_BYTES..]),
            a.map(|c| &c[i * U16_BYTES..]),
            &mut dst[i * RGBA_F32..],
            pixel_count - i,
        );
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,sse4.1,ssse3")]
unsafe fn interleave_f32be_avx2(
    r: Option<&[u8]>,
    g: Option<&[u8]>,
    b: Option<&[u8]>,
    a: Option<&[u8]>,
    dst: &mut [f32],
    pixel_count: usize,
) {
    use core::arch::x86_64::*;
    let zero = _mm_setzero_ps();
    let one = _mm_set1_ps(1.0);
    let mut i = 0usize;
    unsafe {
        while i + PIXELS_PER_AVX2 <= pixel_count {
            for lane in 0..2 {
                let base = i + lane * PIXELS_PER_SSE_OR_NEON;
                let rv = load_f32be_x4(r, base);
                let gv = load_f32be_x4(g, base);
                let bv = load_f32be_x4(b, base);
                let av = if a.is_some() {
                    _mm_min_ps(_mm_max_ps(load_f32be_x4(a, base), zero), one)
                } else {
                    one
                };
                store_rgba_f32x4(dst, base, rv, gv, bv, av);
            }
            i += PIXELS_PER_AVX2;
        }
        if i + PIXELS_PER_SSE_OR_NEON <= pixel_count {
            interleave_f32be_sse41(
                r.map(|c| &c[i * F32_BYTES..]),
                g.map(|c| &c[i * F32_BYTES..]),
                b.map(|c| &c[i * F32_BYTES..]),
                a.map(|c| &c[i * F32_BYTES..]),
                &mut dst[i * RGBA_F32..],
                pixel_count - i,
            );
            return;
        }
    }
    if i < pixel_count {
        interleave_f32be_scalar(
            r.map(|c| &c[i * F32_BYTES..]),
            g.map(|c| &c[i * F32_BYTES..]),
            b.map(|c| &c[i * F32_BYTES..]),
            a.map(|c| &c[i * F32_BYTES..]),
            &mut dst[i * RGBA_F32..],
            pixel_count - i,
        );
    }
}

#[cfg(target_arch = "aarch64")]
#[inline]
unsafe fn load_u16be_f32x4_neon(
    channel: Option<&[u8]>,
    i: usize,
) -> core::arch::aarch64::float32x4_t {
    unsafe {
        use core::arch::aarch64::*;
        let Some(ch) = channel else {
            return vdupq_n_f32(0.0);
        };
        let off = i * U16_BYTES;
        if off + PIXELS_PER_SSE_OR_NEON * U16_BYTES > ch.len() {
            return vdupq_n_f32(0.0);
        }
        let be = vld1_u8(ch.as_ptr().add(off));
        // Rev each 2-byte pair: [hi,lo] -> [lo,hi] then reinterpret as host u16.
        let le_bytes = vrev16_u8(be);
        let u16s = vreinterpret_u16_u8(le_bytes);
        let widened = vmovl_u16(u16s);
        vmulq_n_f32(vcvtq_f32_u32(widened), INV_U16)
    }
}

#[cfg(target_arch = "aarch64")]
#[inline]
unsafe fn load_f32be_x4_neon(channel: Option<&[u8]>, i: usize) -> core::arch::aarch64::float32x4_t {
    unsafe {
        use core::arch::aarch64::*;
        let Some(ch) = channel else {
            return vdupq_n_f32(0.0);
        };
        let off = i * F32_BYTES;
        if off + PIXELS_PER_SSE_OR_NEON * F32_BYTES > ch.len() {
            return vdupq_n_f32(0.0);
        }
        let be = vld1q_u8(ch.as_ptr().add(off));
        let le = vrev32q_u8(be);
        vreinterpretq_f32_u8(le)
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn interleave_u16be_neon(
    r: Option<&[u8]>,
    g: Option<&[u8]>,
    b: Option<&[u8]>,
    a: Option<&[u8]>,
    dst: &mut [f32],
    pixel_count: usize,
) {
    use core::arch::aarch64::*;
    let one = vdupq_n_f32(1.0);
    let zero = vdupq_n_f32(0.0);
    let mut i = 0usize;
    unsafe {
        while i + PIXELS_PER_SSE_OR_NEON <= pixel_count {
            let rv = load_u16be_f32x4_neon(r, i);
            let gv = load_u16be_f32x4_neon(g, i);
            let bv = load_u16be_f32x4_neon(b, i);
            let av = if a.is_some() {
                vminq_f32(vmaxq_f32(load_u16be_f32x4_neon(a, i), zero), one)
            } else {
                one
            };
            let pixels = float32x4x4_t(rv, gv, bv, av);
            vst4q_f32(dst.as_mut_ptr().add(i * RGBA_F32), pixels);
            i += PIXELS_PER_SSE_OR_NEON;
        }
    }
    if i < pixel_count {
        interleave_u16be_scalar(
            r.map(|c| &c[i * U16_BYTES..]),
            g.map(|c| &c[i * U16_BYTES..]),
            b.map(|c| &c[i * U16_BYTES..]),
            a.map(|c| &c[i * U16_BYTES..]),
            &mut dst[i * RGBA_F32..],
            pixel_count - i,
        );
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn interleave_f32be_neon(
    r: Option<&[u8]>,
    g: Option<&[u8]>,
    b: Option<&[u8]>,
    a: Option<&[u8]>,
    dst: &mut [f32],
    pixel_count: usize,
) {
    use core::arch::aarch64::*;
    let one = vdupq_n_f32(1.0);
    let zero = vdupq_n_f32(0.0);
    let mut i = 0usize;
    unsafe {
        while i + PIXELS_PER_SSE_OR_NEON <= pixel_count {
            let rv = load_f32be_x4_neon(r, i);
            let gv = load_f32be_x4_neon(g, i);
            let bv = load_f32be_x4_neon(b, i);
            let av = if a.is_some() {
                vminq_f32(vmaxq_f32(load_f32be_x4_neon(a, i), zero), one)
            } else {
                one
            };
            let pixels = float32x4x4_t(rv, gv, bv, av);
            vst4q_f32(dst.as_mut_ptr().add(i * RGBA_F32), pixels);
            i += PIXELS_PER_SSE_OR_NEON;
        }
    }
    if i < pixel_count {
        interleave_f32be_scalar(
            r.map(|c| &c[i * F32_BYTES..]),
            g.map(|c| &c[i * F32_BYTES..]),
            b.map(|c| &c[i * F32_BYTES..]),
            a.map(|c| &c[i * F32_BYTES..]),
            &mut dst[i * RGBA_F32..],
            pixel_count - i,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn planar_u16be(values: &[u16]) -> Vec<u8> {
        let mut out = Vec::with_capacity(values.len() * 2);
        for v in values {
            out.extend_from_slice(&v.to_be_bytes());
        }
        out
    }

    fn planar_f32be(values: &[f32]) -> Vec<u8> {
        let mut out = Vec::with_capacity(values.len() * 4);
        for v in values {
            out.extend_from_slice(&v.to_be_bytes());
        }
        out
    }

    #[test]
    fn u16be_rgba_matches_scalar_reference() {
        let n = 37usize;
        let r: Vec<u16> = (0..n as u16).map(|i| i.wrapping_mul(17)).collect();
        let g: Vec<u16> = (0..n as u16).map(|i| i.wrapping_mul(31)).collect();
        let b: Vec<u16> = (0..n as u16).map(|i| i.wrapping_mul(47)).collect();
        let a: Vec<u16> = (0..n as u16).map(|i| 0x8000u16.wrapping_add(i)).collect();
        let r_b = planar_u16be(&r);
        let g_b = planar_u16be(&g);
        let b_b = planar_u16be(&b);
        let a_b = planar_u16be(&a);

        let mut simd = vec![0.0f32; n * 4];
        let mut scalar = vec![0.0f32; n * 4];
        interleave_planar_u16be_rgba_f32(
            Some(&r_b),
            Some(&g_b),
            Some(&b_b),
            Some(&a_b),
            &mut simd,
            n,
        );
        interleave_u16be_scalar(
            Some(&r_b),
            Some(&g_b),
            Some(&b_b),
            Some(&a_b),
            &mut scalar,
            n,
        );
        for (i, (s, r)) in simd.iter().zip(scalar.iter()).enumerate() {
            assert!((s - r).abs() < 1e-6, "mismatch at {i}: simd={s} scalar={r}");
        }
    }

    #[test]
    fn u16be_missing_alpha_is_one() {
        let r_b = planar_u16be(&[0, 65535, 32768]);
        let g_b = planar_u16be(&[0, 0, 0]);
        let b_b = planar_u16be(&[0, 0, 0]);
        let mut dst = vec![0.0f32; 12];
        interleave_planar_u16be_rgba_f32(Some(&r_b), Some(&g_b), Some(&b_b), None, &mut dst, 3);
        assert!((dst[3] - 1.0).abs() < 1e-6);
        assert!((dst[7] - 1.0).abs() < 1e-6);
        assert!((dst[11] - 1.0).abs() < 1e-6);
        assert!((dst[4] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn f32be_preserves_hdr_headroom() {
        let r_b = planar_f32be(&[0.0, 1.5, 4.2]);
        let g_b = planar_f32be(&[0.25, 0.5, 0.75]);
        let b_b = planar_f32be(&[1.0, 1.0, 1.0]);
        let mut dst = vec![0.0f32; 12];
        interleave_planar_f32be_rgba_f32(Some(&r_b), Some(&g_b), Some(&b_b), None, &mut dst, 3);
        assert!((dst[4] - 1.5).abs() < 1e-6);
        assert!((dst[8] - 4.2).abs() < 1e-6);
        assert!((dst[11] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn f32be_rgba_matches_scalar_reference() {
        let n = 19usize;
        let r: Vec<f32> = (0..n).map(|i| i as f32 * 0.1).collect();
        let g: Vec<f32> = (0..n).map(|i| i as f32 * 0.05).collect();
        let b: Vec<f32> = (0..n).map(|i| 2.0 - i as f32 * 0.01).collect();
        let a: Vec<f32> = (0..n)
            .map(|i| (i as f32 / n as f32).clamp(0.0, 1.0))
            .collect();
        let r_b = planar_f32be(&r);
        let g_b = planar_f32be(&g);
        let b_b = planar_f32be(&b);
        let a_b = planar_f32be(&a);

        let mut simd = vec![0.0f32; n * 4];
        let mut scalar = vec![0.0f32; n * 4];
        interleave_planar_f32be_rgba_f32(
            Some(&r_b),
            Some(&g_b),
            Some(&b_b),
            Some(&a_b),
            &mut simd,
            n,
        );
        interleave_f32be_scalar(
            Some(&r_b),
            Some(&g_b),
            Some(&b_b),
            Some(&a_b),
            &mut scalar,
            n,
        );
        for (i, (s, r)) in simd.iter().zip(scalar.iter()).enumerate() {
            assert!((s - r).abs() < 1e-6, "mismatch at {i}: simd={s} scalar={r}");
        }
    }
}
