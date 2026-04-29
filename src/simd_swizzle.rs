// Simple Image Viewer - SIMD Interleaving Utilities
// Moved from psb_reader.rs to support zero-copy optimizations across loaders.

#[cfg(target_arch = "x86_64")]
use core::arch::x86_64::*;

#[cfg(target_arch = "aarch64")]
use core::arch::aarch64::*;

/// Interleaves planar R, G, B, A channels into a packed RGBA buffer.
pub fn interleave_rgba(r: &[u8], g: &[u8], b: &[u8], a: &[u8], dst: &mut [u8]) {
    let len = r.len().min(g.len()).min(b.len()).min(a.len());
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
        let base = i * 4;
        if base + 3 < dst.len() {
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
    let len = r.len().min(g.len()).min(b.len());
    let mut i = 0;

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            unsafe {
                interleave_rgb_avx2(r, g, b, alpha, dst, &mut i, len);
            }
        }
    }

    // Scalar fallback
    while i < len {
        let base = i * 4;
        if base + 3 < dst.len() {
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
    let count = src.len() / 3;
    let mut i = 0;

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            unsafe {
                interleave_rgb_packed_to_rgba_avx2(src, dst, &mut i, count);
            }
        }
    }

    // Scalar fallback
    while i < count {
        let s = i * 3;
        let d = i * 4;
        if s + 2 < src.len() && d + 3 < dst.len() {
            dst[d] = src[s];
            dst[d + 1] = src[s + 1];
            dst[d + 2] = src[s + 2];
            dst[d + 3] = 255;
        }
        i += 1;
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn interleave_rgb_packed_to_rgba_avx2(
    src: &[u8],
    dst: &mut [u8],
    i: &mut usize,
    count: usize,
) {
    while *i + 32 <= count {

        // LLVM handles this loop very well with AVX2 if we hint it correctly.
        // For a more robust implementation, one could use _mm256_shuffle_epi8,
        // but that requires complex masks for 3-to-4 byte expansion across 256-bit lanes.
        for _ in 0..32 {
            let s = *i * 3;
            let d = *i * 4;
            dst[d] = src[s];
            dst[d + 1] = src[s + 1];
            dst[d + 2] = src[s + 2];
            dst[d + 3] = 255;
            *i += 1;
        }
    }
}
