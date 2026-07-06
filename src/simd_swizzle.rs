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

// SIMD Interleaving Utilities
// Moved from psb_reader.rs to support zero-copy optimizations across loaders.

#[cfg(target_arch = "x86_64")]
use core::arch::x86_64::*;

#[cfg(target_arch = "aarch64")]
use core::arch::aarch64::*;

const RGB_CHANNELS: usize = 3;
const RGBA_CHANNELS: usize = 4;
const MAX_CHANNEL_VALUE: u8 = 255;

/// Interleaves planar R, G, B, A channels into a packed RGBA buffer.
pub fn interleave_rgba(r: &[u8], g: &[u8], b: &[u8], a: &[u8], dst: &mut [u8]) {
    let len = r
        .len()
        .min(g.len())
        .min(b.len())
        .min(a.len())
        .min(dst.len() / RGBA_CHANNELS);
    let mut i = 0;

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            unsafe {
                interleave_rgba_avx2(r, g, b, a, dst, &mut i, len);
            }
        } else if is_x86_feature_detected!("sse4.1") {
            unsafe {
                interleave_rgba_sse41(r, g, b, a, dst, &mut i, len);
            }
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        // NEON is always available on aarch64
        unsafe {
            interleave_rgba_neon(r, g, b, a, dst, &mut i, len);
        }
    }

    // Scalar fallback
    while i < len {
        let base = i * RGBA_CHANNELS;
        if base + (RGBA_CHANNELS - 1) < dst.len() {
            dst[base] = r[i];
            dst[base + 1] = g[i];
            dst[base + 2] = b[i];
            dst[base + 3] = a[i];
        }
        i += 1;
    }
}

/// Interleaves planar R, G, B channels into a packed RGBA buffer with a fixed alpha.
pub fn interleave_rgb_with_alpha(r: &[u8], g: &[u8], b: &[u8], alpha: u8, dst: &mut [u8]) {
    let len = r
        .len()
        .min(g.len())
        .min(b.len())
        .min(dst.len() / RGBA_CHANNELS);
    let mut i = 0;

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            unsafe {
                interleave_rgb_avx2(r, g, b, alpha, dst, &mut i, len);
            }
        } else if is_x86_feature_detected!("sse4.1") {
            unsafe {
                interleave_rgb_sse41(r, g, b, alpha, dst, &mut i, len);
            }
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        // NEON is always available on aarch64
        unsafe {
            interleave_rgb_with_alpha_neon(r, g, b, alpha, dst, &mut i, len);
        }
    }

    // Scalar fallback
    while i < len {
        let base = i * RGBA_CHANNELS;
        if base + (RGBA_CHANNELS - 1) < dst.len() {
            dst[base] = r[i];
            dst[base + 1] = g[i];
            dst[base + 2] = b[i];
            dst[base + 3] = alpha;
        }
        i += 1;
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn interleave_rgba_avx2(
    r: &[u8],
    g: &[u8],
    b: &[u8],
    a: &[u8],
    dst: &mut [u8],
    i: &mut usize,
    len: usize,
) {
    unsafe {
        while *i + 32 <= len {
            let vr = _mm256_loadu_si256(r.as_ptr().add(*i) as *const __m256i);
            let vg = _mm256_loadu_si256(g.as_ptr().add(*i) as *const __m256i);
            let vb = _mm256_loadu_si256(b.as_ptr().add(*i) as *const __m256i);
            let va = _mm256_loadu_si256(a.as_ptr().add(*i) as *const __m256i);

            let rg_lo = _mm256_unpacklo_epi8(vr, vg);
            let rg_hi = _mm256_unpackhi_epi8(vr, vg);
            let ba_lo = _mm256_unpacklo_epi8(vb, va);
            let ba_hi = _mm256_unpackhi_epi8(vb, va);

            let rgba0 = _mm256_unpacklo_epi16(rg_lo, ba_lo);
            let rgba1 = _mm256_unpackhi_epi16(rg_lo, ba_lo);
            let rgba2 = _mm256_unpacklo_epi16(rg_hi, ba_hi);
            let rgba3 = _mm256_unpackhi_epi16(rg_hi, ba_hi);

            let p_dst = dst.as_mut_ptr().add(*i * 4);
            _mm_storeu_si128(p_dst as *mut __m128i, _mm256_extracti128_si256(rgba0, 0));
            _mm_storeu_si128(
                p_dst.add(16) as *mut __m128i,
                _mm256_extracti128_si256(rgba1, 0),
            );
            _mm_storeu_si128(
                p_dst.add(32) as *mut __m128i,
                _mm256_extracti128_si256(rgba2, 0),
            );
            _mm_storeu_si128(
                p_dst.add(48) as *mut __m128i,
                _mm256_extracti128_si256(rgba3, 0),
            );
            _mm_storeu_si128(
                p_dst.add(64) as *mut __m128i,
                _mm256_extracti128_si256(rgba0, 1),
            );
            _mm_storeu_si128(
                p_dst.add(80) as *mut __m128i,
                _mm256_extracti128_si256(rgba1, 1),
            );
            _mm_storeu_si128(
                p_dst.add(96) as *mut __m128i,
                _mm256_extracti128_si256(rgba2, 1),
            );
            _mm_storeu_si128(
                p_dst.add(112) as *mut __m128i,
                _mm256_extracti128_si256(rgba3, 1),
            );
            *i += 32;
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn interleave_rgb_avx2(
    r: &[u8],
    g: &[u8],
    b: &[u8],
    alpha: u8,
    dst: &mut [u8],
    i: &mut usize,
    len: usize,
) {
    unsafe {
        let va = _mm256_set1_epi8(alpha as i8);
        while *i + 32 <= len {
            let vr = _mm256_loadu_si256(r.as_ptr().add(*i) as *const __m256i);
            let vg = _mm256_loadu_si256(g.as_ptr().add(*i) as *const __m256i);
            let vb = _mm256_loadu_si256(b.as_ptr().add(*i) as *const __m256i);

            let rg_lo = _mm256_unpacklo_epi8(vr, vg);
            let rg_hi = _mm256_unpackhi_epi8(vr, vg);
            let ba_lo = _mm256_unpacklo_epi8(vb, va);
            let ba_hi = _mm256_unpackhi_epi8(vb, va);

            let rgba0 = _mm256_unpacklo_epi16(rg_lo, ba_lo);
            let rgba1 = _mm256_unpackhi_epi16(rg_lo, ba_lo);
            let rgba2 = _mm256_unpacklo_epi16(rg_hi, ba_hi);
            let rgba3 = _mm256_unpackhi_epi16(rg_hi, ba_hi);

            let p_dst = dst.as_mut_ptr().add(*i * 4);
            _mm_storeu_si128(p_dst as *mut __m128i, _mm256_extracti128_si256(rgba0, 0));
            _mm_storeu_si128(
                p_dst.add(16) as *mut __m128i,
                _mm256_extracti128_si256(rgba1, 0),
            );
            _mm_storeu_si128(
                p_dst.add(32) as *mut __m128i,
                _mm256_extracti128_si256(rgba2, 0),
            );
            _mm_storeu_si128(
                p_dst.add(48) as *mut __m128i,
                _mm256_extracti128_si256(rgba3, 0),
            );
            _mm_storeu_si128(
                p_dst.add(64) as *mut __m128i,
                _mm256_extracti128_si256(rgba0, 1),
            );
            _mm_storeu_si128(
                p_dst.add(80) as *mut __m128i,
                _mm256_extracti128_si256(rgba1, 1),
            );
            _mm_storeu_si128(
                p_dst.add(96) as *mut __m128i,
                _mm256_extracti128_si256(rgba2, 1),
            );
            _mm_storeu_si128(
                p_dst.add(112) as *mut __m128i,
                _mm256_extracti128_si256(rgba3, 1),
            );
            *i += 32;
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn interleave_rgb_sse41(
    r: &[u8],
    g: &[u8],
    b: &[u8],
    alpha: u8,
    dst: &mut [u8],
    i: &mut usize,
    len: usize,
) {
    unsafe {
        let va = _mm_set1_epi8(alpha as i8);
        while *i + 16 <= len {
            let vr = _mm_loadu_si128(r.as_ptr().add(*i) as *const __m128i);
            let vg = _mm_loadu_si128(g.as_ptr().add(*i) as *const __m128i);
            let vb = _mm_loadu_si128(b.as_ptr().add(*i) as *const __m128i);

            let rg_lo = _mm_unpacklo_epi8(vr, vg);
            let rg_hi = _mm_unpackhi_epi8(vr, vg);
            let ba_lo = _mm_unpacklo_epi8(vb, va);
            let ba_hi = _mm_unpackhi_epi8(vb, va);

            let rgba0 = _mm_unpacklo_epi16(rg_lo, ba_lo);
            let rgba1 = _mm_unpackhi_epi16(rg_lo, ba_lo);
            let rgba2 = _mm_unpacklo_epi16(rg_hi, ba_hi);
            let rgba3 = _mm_unpackhi_epi16(rg_hi, ba_hi);

            let p_dst = dst.as_mut_ptr().add(*i * 4);
            _mm_storeu_si128(p_dst as *mut __m128i, rgba0);
            _mm_storeu_si128(p_dst.add(16) as *mut __m128i, rgba1);
            _mm_storeu_si128(p_dst.add(32) as *mut __m128i, rgba2);
            _mm_storeu_si128(p_dst.add(48) as *mut __m128i, rgba3);
            *i += 16;
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn interleave_rgba_sse41(
    r: &[u8],
    g: &[u8],
    b: &[u8],
    a: &[u8],
    dst: &mut [u8],
    i: &mut usize,
    len: usize,
) {
    unsafe {
        while *i + 16 <= len {
            let vr = _mm_loadu_si128(r.as_ptr().add(*i) as *const __m128i);
            let vg = _mm_loadu_si128(g.as_ptr().add(*i) as *const __m128i);
            let vb = _mm_loadu_si128(b.as_ptr().add(*i) as *const __m128i);
            let va = _mm_loadu_si128(a.as_ptr().add(*i) as *const __m128i);

            let rg_lo = _mm_unpacklo_epi8(vr, vg);
            let rg_hi = _mm_unpackhi_epi8(vr, vg);
            let ba_lo = _mm_unpacklo_epi8(vb, va);
            let ba_hi = _mm_unpackhi_epi8(vb, va);

            let rgba0 = _mm_unpacklo_epi16(rg_lo, ba_lo);
            let rgba1 = _mm_unpackhi_epi16(rg_lo, ba_lo);
            let rgba2 = _mm_unpacklo_epi16(rg_hi, ba_hi);
            let rgba3 = _mm_unpackhi_epi16(rg_hi, ba_hi);

            let p_dst = dst.as_mut_ptr().add(*i * 4);
            _mm_storeu_si128(p_dst as *mut __m128i, rgba0);
            _mm_storeu_si128(p_dst.add(16) as *mut __m128i, rgba1);
            _mm_storeu_si128(p_dst.add(32) as *mut __m128i, rgba2);
            _mm_storeu_si128(p_dst.add(48) as *mut __m128i, rgba3);
            *i += 16;
        }
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn interleave_rgba_neon(
    r: &[u8],
    g: &[u8],
    b: &[u8],
    a: &[u8],
    dst: &mut [u8],
    i: &mut usize,
    len: usize,
) {
    unsafe {
        while *i + 16 <= len {
            let vr = vld1q_u8(r.as_ptr().add(*i));
            let vg = vld1q_u8(g.as_ptr().add(*i));
            let vb = vld1q_u8(b.as_ptr().add(*i));
            let va = vld1q_u8(a.as_ptr().add(*i));

            let res = uint8x16x4_t(vr, vg, vb, va);
            vst4q_u8(dst.as_mut_ptr().add(*i * 4), res);
            *i += 16;
        }
    }
}

/// Interleaves packed RGB (RGBRGB...) into packed RGBA (RGBARGBA...) with a fixed alpha.
pub fn interleave_rgb_packed_to_rgba_packed(src: &[u8], dst: &mut [u8]) {
    let count = (src.len() / RGB_CHANNELS).min(dst.len() / RGBA_CHANNELS);
    let mut i = 0;

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("ssse3") {
            unsafe {
                interleave_rgb_packed_to_rgba_ssse3(src, dst, &mut i, count);
            }
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        // NEON is always available on aarch64
        unsafe {
            interleave_rgb_packed_to_rgba_neon(src, dst, &mut i, count);
        }
    }

    // Scalar fallback
    while i < count {
        let s = i * RGB_CHANNELS;
        let d = i * RGBA_CHANNELS;
        if s + (RGB_CHANNELS - 1) < src.len() && d + (RGBA_CHANNELS - 1) < dst.len() {
            dst[d] = src[s];
            dst[d + 1] = src[s + 1];
            dst[d + 2] = src[s + 2];
            dst[d + 3] = MAX_CHANNEL_VALUE;
        }
        i += 1;
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn interleave_rgb_with_alpha_neon(
    r: &[u8],
    g: &[u8],
    b: &[u8],
    alpha: u8,
    dst: &mut [u8],
    i: &mut usize,
    len: usize,
) {
    unsafe {
        let va = vmovq_n_u8(alpha);
        while *i + 16 <= len {
            let vr = vld1q_u8(r.as_ptr().add(*i));
            let vg = vld1q_u8(g.as_ptr().add(*i));
            let vb = vld1q_u8(b.as_ptr().add(*i));

            let res = uint8x16x4_t(vr, vg, vb, va);
            vst4q_u8(dst.as_mut_ptr().add(*i * 4), res);
            *i += 16;
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "ssse3")]
unsafe fn interleave_rgb_packed_to_rgba_ssse3(
    src: &[u8],
    dst: &mut [u8],
    i: &mut usize,
    count: usize,
) {
    while *i + 32 <= count {
        unsafe {
            let src_ptr = src.as_ptr().add(*i * RGB_CHANNELS);
            let dst_ptr = dst.as_mut_ptr().add(*i * RGBA_CHANNELS);
            interleave_rgb_packed_16_to_rgba_ssse3(src_ptr, dst_ptr);
            interleave_rgb_packed_16_to_rgba_ssse3(
                src_ptr.add(16 * RGB_CHANNELS),
                dst_ptr.add(16 * RGBA_CHANNELS),
            );
        }
        *i += 32;
    }
    while *i + 16 <= count {
        unsafe {
            interleave_rgb_packed_16_to_rgba_ssse3(
                src.as_ptr().add(*i * RGB_CHANNELS),
                dst.as_mut_ptr().add(*i * RGBA_CHANNELS),
            );
        }
        *i += 16;
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "ssse3")]
/// Interleave 16 RGB888 pixels (48 bytes) into 16 RGBA8888 pixels (64 bytes).
///
/// # Safety
///
/// `src` must address at least 48 readable bytes and `dst` at least 64 writable bytes.
/// The two pointers must not alias.
unsafe fn interleave_rgb_packed_16_to_rgba_ssse3(src: *const u8, dst: *mut u8) {
    unsafe {
        let in0 = _mm_loadu_si128(src as *const __m128i);
        let in1 = _mm_loadu_si128(src.add(16) as *const __m128i);
        let in2 = _mm_loadu_si128(src.add(32) as *const __m128i);
        let rgb0 = _mm_setr_epi8(0, 1, 2, -128, 3, 4, 5, -128, 6, 7, 8, -128, 9, 10, 11, -128);
        let rgb_tail = _mm_setr_epi8(
            4, 5, 6, -128, 7, 8, 9, -128, 10, 11, 12, -128, 13, 14, 15, -128,
        );
        let alpha = _mm_setr_epi8(0, 0, 0, -1, 0, 0, 0, -1, 0, 0, 0, -1, 0, 0, 0, -1);

        let out0 = _mm_or_si128(_mm_shuffle_epi8(in0, rgb0), alpha);
        let out1 = _mm_or_si128(_mm_shuffle_epi8(_mm_alignr_epi8(in1, in0, 12), rgb0), alpha);
        let out2 = _mm_or_si128(_mm_shuffle_epi8(_mm_alignr_epi8(in2, in1, 8), rgb0), alpha);
        let out3 = _mm_or_si128(_mm_shuffle_epi8(in2, rgb_tail), alpha);

        _mm_storeu_si128(dst as *mut __m128i, out0);
        _mm_storeu_si128(dst.add(16) as *mut __m128i, out1);
        _mm_storeu_si128(dst.add(32) as *mut __m128i, out2);
        _mm_storeu_si128(dst.add(48) as *mut __m128i, out3);
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn interleave_rgb_packed_to_rgba_neon(
    src: &[u8],
    dst: &mut [u8],
    i: &mut usize,
    count: usize,
) {
    unsafe {
        let va = vmovq_n_u8(MAX_CHANNEL_VALUE);
        while *i + 16 <= count {
            let p_src = src.as_ptr().add(*i * 3);
            let res_rgb = vld3q_u8(p_src);
            let res_rgba = uint8x16x4_t(res_rgb.0, res_rgb.1, res_rgb.2, va);

            let p_dst = dst.as_mut_ptr().add(*i * 4);
            vst4q_u8(p_dst, res_rgba);
            *i += 16;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn interleave_rgba_scalar(r: &[u8], g: &[u8], b: &[u8], a: &[u8], dst: &mut [u8]) {
        let len = r
            .len()
            .min(g.len())
            .min(b.len())
            .min(a.len())
            .min(dst.len() / RGBA_CHANNELS);
        for i in 0..len {
            let base = i * RGBA_CHANNELS;
            dst[base] = r[i];
            dst[base + 1] = g[i];
            dst[base + 2] = b[i];
            dst[base + 3] = a[i];
        }
    }

    fn interleave_rgb_with_alpha_scalar(r: &[u8], g: &[u8], b: &[u8], alpha: u8, dst: &mut [u8]) {
        let len = r
            .len()
            .min(g.len())
            .min(b.len())
            .min(dst.len() / RGBA_CHANNELS);
        for i in 0..len {
            let base = i * RGBA_CHANNELS;
            dst[base] = r[i];
            dst[base + 1] = g[i];
            dst[base + 2] = b[i];
            dst[base + 3] = alpha;
        }
    }

    fn interleave_rgb_packed_to_rgba_packed_scalar(src: &[u8], dst: &mut [u8]) {
        let count = (src.len() / RGB_CHANNELS).min(dst.len() / RGBA_CHANNELS);
        for i in 0..count {
            let s = i * RGB_CHANNELS;
            let d = i * RGBA_CHANNELS;
            dst[d] = src[s];
            dst[d + 1] = src[s + 1];
            dst[d + 2] = src[s + 2];
            dst[d + 3] = MAX_CHANNEL_VALUE;
        }
    }

    fn patterned_channels(len: usize) -> (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>) {
        let r: Vec<u8> = (0..len).map(|i| ((i * 3 + 1) % 256) as u8).collect();
        let g: Vec<u8> = (0..len).map(|i| ((i * 5 + 2) % 256) as u8).collect();
        let b: Vec<u8> = (0..len).map(|i| ((i * 7 + 3) % 256) as u8).collect();
        let a: Vec<u8> = (0..len).map(|i| ((i * 11 + 4) % 256) as u8).collect();
        (r, g, b, a)
    }

    const PARITY_LENGTHS: &[usize] = &[0, 1, 15, 16, 17, 31, 32, 33, 64, 100];

    #[test]
    fn simd_swizzle_interleave_rgba_matches_scalar() {
        for len in PARITY_LENGTHS {
            let (r, g, b, a) = patterned_channels(*len);
            let mut simd_dst = vec![0_u8; len * RGBA_CHANNELS];
            let mut scalar_dst = vec![0_u8; len * RGBA_CHANNELS];
            interleave_rgba(&r, &g, &b, &a, &mut simd_dst);
            interleave_rgba_scalar(&r, &g, &b, &a, &mut scalar_dst);
            assert_eq!(simd_dst, scalar_dst, "len={len}");
        }
    }

    #[test]
    fn simd_swizzle_interleave_rgb_with_alpha_matches_scalar() {
        for len in PARITY_LENGTHS {
            let (r, g, b, _) = patterned_channels(*len);
            let alpha = 200_u8;
            let mut simd_dst = vec![0_u8; len * RGBA_CHANNELS];
            let mut scalar_dst = vec![0_u8; len * RGBA_CHANNELS];
            interleave_rgb_with_alpha(&r, &g, &b, alpha, &mut simd_dst);
            interleave_rgb_with_alpha_scalar(&r, &g, &b, alpha, &mut scalar_dst);
            assert_eq!(simd_dst, scalar_dst, "len={len}");
        }
    }

    #[test]
    fn simd_swizzle_interleave_rgb_packed_to_rgba_matches_scalar() {
        for len in PARITY_LENGTHS {
            let src: Vec<u8> = (0..len * RGB_CHANNELS)
                .map(|i| ((i * 13 + 7) % 256) as u8)
                .collect();
            let mut simd_dst = vec![0_u8; len * RGBA_CHANNELS];
            let mut scalar_dst = vec![0_u8; len * RGBA_CHANNELS];
            interleave_rgb_packed_to_rgba_packed(&src, &mut simd_dst);
            interleave_rgb_packed_to_rgba_packed_scalar(&src, &mut scalar_dst);
            assert_eq!(simd_dst, scalar_dst, "len={len}");
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn simd_swizzle_rgb24_chunk16_expander_matches_scalar() {
        if !is_x86_feature_detected!("ssse3") {
            return;
        }
        let src: Vec<u8> = (0..16 * RGB_CHANNELS)
            .map(|i| ((i * 17 + 11) % 256) as u8)
            .collect();
        let mut simd_dst = vec![0_u8; 16 * RGBA_CHANNELS];
        let mut scalar_dst = vec![0_u8; 16 * RGBA_CHANNELS];

        unsafe {
            interleave_rgb_packed_16_to_rgba_ssse3(src.as_ptr(), simd_dst.as_mut_ptr());
        }
        interleave_rgb_packed_to_rgba_packed_scalar(&src, &mut scalar_dst);

        assert_eq!(simd_dst, scalar_dst);
    }
}
