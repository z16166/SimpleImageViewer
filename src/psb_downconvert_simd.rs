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

//! SIMD down-conversion of PSD/PSB big-endian planar samples to 8-bit.
//!
//! 16-bit: take the high byte of each BE u16 (equivalent to `v >> 8`).
//! 32-bit: interpret BE IEEE-754 floats, clamp to [0, 1], scale to 0..255.

const U16_BYTES: usize = 2;
const F32_BYTES: usize = 4;
#[cfg(target_arch = "x86_64")]
const SSE_U16_SAMPLES: usize = 8;
#[cfg(target_arch = "x86_64")]
const AVX2_U16_SAMPLES: usize = 16;
#[cfg(target_arch = "x86_64")]
const SSE_F32_SAMPLES: usize = 4;
#[cfg(target_arch = "x86_64")]
const AVX2_F32_SAMPLES: usize = 8;
#[cfg(target_arch = "aarch64")]
const NEON_U16_SAMPLES: usize = 8;
#[cfg(target_arch = "aarch64")]
const NEON_F32_SAMPLES: usize = 4;

/// Convert `dst.len()` big-endian u16 samples in `src` to 8-bit (high byte).
pub fn u16be_to_u8(dst: &mut [u8], src: &[u8]) {
    let n = dst.len();
    let convertible = n.min(src.len() / U16_BYTES);
    let (head, tail) = dst.split_at_mut(convertible);
    let src_head = &src[..convertible * U16_BYTES];

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            unsafe {
                u16be_to_u8_avx2(head, src_head);
            }
            tail.fill(0);
            return;
        }
        if is_x86_feature_detected!("sse2") {
            unsafe {
                u16be_to_u8_sse2(head, src_head);
            }
            tail.fill(0);
            return;
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        unsafe {
            u16be_to_u8_neon(head, src_head);
        }
        tail.fill(0);
        return;
    }

    #[cfg(not(target_arch = "aarch64"))]
    {
        u16be_to_u8_scalar(head, src_head);
        tail.fill(0);
    }
}

/// Convert `dst.len()` big-endian f32 samples in `src` to 8-bit display values.
pub fn f32be_to_u8(dst: &mut [u8], src: &[u8]) {
    let n = dst.len();
    let convertible = n.min(src.len() / F32_BYTES);
    let (head, tail) = dst.split_at_mut(convertible);
    let src_head = &src[..convertible * F32_BYTES];

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            unsafe {
                f32be_to_u8_avx2(head, src_head);
            }
            tail.fill(0);
            return;
        }
        if is_x86_feature_detected!("sse2") {
            unsafe {
                f32be_to_u8_sse2(head, src_head);
            }
            tail.fill(0);
            return;
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        unsafe {
            f32be_to_u8_neon(head, src_head);
        }
        tail.fill(0);
        return;
    }

    #[cfg(not(target_arch = "aarch64"))]
    {
        f32be_to_u8_scalar(head, src_head);
        tail.fill(0);
    }
}

#[inline]
fn u16be_to_u8_scalar(dst: &mut [u8], src: &[u8]) {
    for (i, out) in dst.iter_mut().enumerate() {
        let off = i * U16_BYTES;
        // On LE hosts, BE bytes [hi, lo] load as u16 with low byte == hi.
        *out = src[off];
    }
}

#[inline]
fn f32be_to_u8_scalar(dst: &mut [u8], src: &[u8]) {
    for (i, out) in dst.iter_mut().enumerate() {
        let off = i * F32_BYTES;
        let bits = u32::from_be_bytes([src[off], src[off + 1], src[off + 2], src[off + 3]]);
        let f = f32::from_bits(bits);
        *out = (f.clamp(0.0, 1.0) * 255.0).round() as u8;
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn u16be_to_u8_sse2(dst: &mut [u8], src: &[u8]) {
    use core::arch::x86_64::*;
    let mut i = 0usize;
    let n = dst.len();
    // BE [hi,lo] on LE: low 8 bits of each u16 lane are the high sample byte.
    let mask = _mm_set1_epi16(0x00FF);
    while i + SSE_U16_SAMPLES <= n {
        unsafe {
            let v = _mm_loadu_si128(src.as_ptr().add(i * U16_BYTES).cast());
            let lo = _mm_and_si128(v, mask);
            let packed = _mm_packus_epi16(lo, _mm_setzero_si128());
            _mm_storel_epi64(dst.as_mut_ptr().add(i).cast(), packed);
        }
        i += SSE_U16_SAMPLES;
    }
    if i < n {
        u16be_to_u8_scalar(&mut dst[i..], &src[i * U16_BYTES..]);
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn u16be_to_u8_avx2(dst: &mut [u8], src: &[u8]) {
    use core::arch::x86_64::*;
    let mut i = 0usize;
    let n = dst.len();
    let mask = _mm256_set1_epi16(0x00FF);
    while i + AVX2_U16_SAMPLES <= n {
        unsafe {
            let v = _mm256_loadu_si256(src.as_ptr().add(i * U16_BYTES).cast());
            let lo = _mm256_and_si256(v, mask);
            // packus is per 128-bit lane: [8B][pad][8B][pad] -> permute to contiguous 16B.
            let packed = _mm256_packus_epi16(lo, _mm256_setzero_si256());
            let ordered = _mm256_permute4x64_epi64::<0xD8>(packed);
            _mm_storeu_si128(
                dst.as_mut_ptr().add(i).cast(),
                _mm256_castsi256_si128(ordered),
            );
        }
        i += AVX2_U16_SAMPLES;
    }
    if i + SSE_U16_SAMPLES <= n {
        unsafe {
            u16be_to_u8_sse2(&mut dst[i..], &src[i * U16_BYTES..]);
        }
    } else if i < n {
        u16be_to_u8_scalar(&mut dst[i..], &src[i * U16_BYTES..]);
    }
}

/// Swap bytes within each u32 lane: BE [b0 b1 b2 b3] -> LE [b3 b2 b1 b0] (SSE2).
#[cfg(target_arch = "x86_64")]
#[inline]
unsafe fn bswap_u32x4_sse2(v: core::arch::x86_64::__m128i) -> core::arch::x86_64::__m128i {
    use core::arch::x86_64::*;
    // 0123 -> 1032 (swap adjacent bytes), then 1032 -> 3210 (swap 16-bit halves).
    let swap8 = unsafe { _mm_or_si128(_mm_slli_epi16(v, 8), _mm_srli_epi16(v, 8)) };
    unsafe { _mm_or_si128(_mm_slli_epi32(swap8, 16), _mm_srli_epi32(swap8, 16)) }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn f32be_to_u8_sse2(dst: &mut [u8], src: &[u8]) {
    use core::arch::x86_64::*;
    let mut i = 0usize;
    let n = dst.len();
    let zero = _mm_setzero_ps();
    let one = _mm_set1_ps(1.0);
    let scale = _mm_set1_ps(255.0);
    // floor(x+0.5) via +0.5 then truncate: matches scalar `.round()` on [0,255].
    let half = _mm_set1_ps(0.5);
    while i + SSE_F32_SAMPLES <= n {
        unsafe {
            let be = _mm_loadu_si128(src.as_ptr().add(i * F32_BYTES).cast());
            let le = bswap_u32x4_sse2(be);
            let f = _mm_castsi128_ps(le);
            // Same order as scalar: clamp to [0,1], then *255, then round.
            let clamped = _mm_min_ps(_mm_max_ps(f, zero), one);
            let scaled = _mm_mul_ps(clamped, scale);
            let i32s = _mm_cvttps_epi32(_mm_add_ps(scaled, half));
            // Values are 0..=255; signed packs_epi32 is fine (SSE2, no packus_epi32).
            let u16s = _mm_packs_epi32(i32s, _mm_setzero_si128());
            let u8s = _mm_packus_epi16(u16s, _mm_setzero_si128());
            let bits = _mm_cvtsi128_si32(u8s) as u32;
            dst[i..i + SSE_F32_SAMPLES].copy_from_slice(&bits.to_le_bytes());
        }
        i += SSE_F32_SAMPLES;
    }
    if i < n {
        f32be_to_u8_scalar(&mut dst[i..], &src[i * F32_BYTES..]);
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn f32be_to_u8_avx2(dst: &mut [u8], src: &[u8]) {
    use core::arch::x86_64::*;
    let mut i = 0usize;
    let n = dst.len();
    let swap = _mm256_setr_epi8(
        3, 2, 1, 0, 7, 6, 5, 4, 11, 10, 9, 8, 15, 14, 13, 12, 3, 2, 1, 0, 7, 6, 5, 4, 11, 10, 9, 8,
        15, 14, 13, 12,
    );
    let zero = _mm256_setzero_ps();
    let one = _mm256_set1_ps(1.0);
    let scale = _mm256_set1_ps(255.0);
    // floor(x+0.5) via +0.5 then truncate: matches scalar `.round()` on [0,255].
    let half = _mm256_set1_ps(0.5);
    while i + AVX2_F32_SAMPLES <= n {
        unsafe {
            let be = _mm256_loadu_si256(src.as_ptr().add(i * F32_BYTES).cast());
            let le = _mm256_shuffle_epi8(be, swap);
            let f = _mm256_castsi256_ps(le);
            // Same order as scalar: clamp to [0,1], then *255, then round.
            let clamped = _mm256_min_ps(_mm256_max_ps(f, zero), one);
            let scaled = _mm256_mul_ps(clamped, scale);
            let i32s = _mm256_cvttps_epi32(_mm256_add_ps(scaled, half));
            // Pack 8 i32 -> 8 u8 across lanes.
            let u16s = _mm256_packus_epi32(i32s, _mm256_setzero_si256());
            let ordered = _mm256_permute4x64_epi64::<0xD8>(u16s);
            let u8s = _mm_packus_epi16(_mm256_castsi256_si128(ordered), _mm_setzero_si128());
            _mm_storel_epi64(dst.as_mut_ptr().add(i).cast(), u8s);
        }
        i += AVX2_F32_SAMPLES;
    }
    // Remainder: reuse SSE2 kernel (AVX2 implies SSE2).
    if i + SSE_F32_SAMPLES <= n {
        unsafe {
            f32be_to_u8_sse2(&mut dst[i..], &src[i * F32_BYTES..]);
        }
    } else if i < n {
        f32be_to_u8_scalar(&mut dst[i..], &src[i * F32_BYTES..]);
    }
}

#[cfg(target_arch = "aarch64")]
unsafe fn u16be_to_u8_neon(dst: &mut [u8], src: &[u8]) {
    use core::arch::aarch64::*;
    let mut i = 0usize;
    let n = dst.len();
    while i + NEON_U16_SAMPLES <= n {
        unsafe {
            let v = vld1q_u16(src.as_ptr().add(i * U16_BYTES).cast());
            // LE view of BE bytes: low byte of each lane is the high sample byte.
            let narrow = vmovn_u16(vandq_u16(v, vdupq_n_u16(0x00FF)));
            vst1_u8(dst.as_mut_ptr().add(i), narrow);
        }
        i += NEON_U16_SAMPLES;
    }
    if i < n {
        u16be_to_u8_scalar(&mut dst[i..], &src[i * U16_BYTES..]);
    }
}

#[cfg(target_arch = "aarch64")]
unsafe fn f32be_to_u8_neon(dst: &mut [u8], src: &[u8]) {
    unsafe {
        use core::arch::aarch64::*;
        let mut i = 0usize;
        let n = dst.len();
        let zero = vdupq_n_f32(0.0);
        let one = vdupq_n_f32(1.0);
        let scale = vdupq_n_f32(255.0);
        while i + NEON_F32_SAMPLES <= n {
            let be_bytes = vld1q_u8(src.as_ptr().add(i * F32_BYTES));
            // Rev 4-byte groups: 0123->3210, 4567->7654, ...
            let le_bytes = vrev32q_u8(be_bytes);
            let f = vreinterpretq_f32_u8(le_bytes);
            // Same order as scalar: clamp to [0,1], then *255, then round.
            let clamped = vminq_f32(vmaxq_f32(f, zero), one);
            let scaled = vmulq_f32(clamped, scale);
            // Ties away from zero: matches scalar `.round()` (not ties-to-even).
            let i32s = vcvtaq_s32_f32(scaled);
            let u16s = vqmovun_s32(i32s);
            let u8s = vqmovn_u16(vcombine_u16(u16s, u16s));
            let mut tmp = [0u8; 8];
            vst1_u8(tmp.as_mut_ptr(), u8s);
            dst[i..i + NEON_F32_SAMPLES].copy_from_slice(&tmp[..NEON_F32_SAMPLES]);
            i += NEON_F32_SAMPLES;
        }
        if i < n {
            f32be_to_u8_scalar(&mut dst[i..], &src[i * F32_BYTES..]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{f32be_to_u8, f32be_to_u8_scalar, u16be_to_u8};

    #[cfg(target_arch = "x86_64")]
    use super::{f32be_to_u8_avx2, f32be_to_u8_sse2};

    #[cfg(target_arch = "aarch64")]
    use super::f32be_to_u8_neon;

    #[test]
    fn u16be_matches_high_byte() {
        let src = [0x12, 0x34, 0xAB, 0xCD, 0x00, 0xFF, 0xFF, 0x00];
        let mut dst = [0u8; 4];
        u16be_to_u8(&mut dst, &src);
        assert_eq!(dst, [0x12, 0xAB, 0x00, 0xFF]);
    }

    #[test]
    fn u16be_handles_short_src() {
        let src = [0x11, 0x22];
        let mut dst = [9u8; 3];
        u16be_to_u8(&mut dst, &src);
        assert_eq!(dst, [0x11, 0, 0]);
    }

    #[test]
    fn u16be_long_row_matches_scalar() {
        let n = 1000usize;
        let mut src = Vec::with_capacity(n * 2);
        for i in 0..n {
            let v = (i * 37) as u16;
            src.extend_from_slice(&v.to_be_bytes());
        }
        let mut simd = vec![0u8; n];
        let mut scalar = vec![0u8; n];
        u16be_to_u8(&mut simd, &src);
        for (i, out) in scalar.iter_mut().enumerate() {
            *out = src[i * 2];
        }
        assert_eq!(simd, scalar);
    }

    #[test]
    fn f32be_clamps_and_scales() {
        let mut src = Vec::new();
        for f in [0.0f32, 0.5, 1.0, 2.0, -1.0] {
            src.extend_from_slice(&f.to_be_bytes());
        }
        let mut dst = [0u8; 5];
        f32be_to_u8(&mut dst, &src);
        assert_eq!(dst[0], 0);
        assert_eq!(dst[1], 128);
        assert_eq!(dst[2], 255);
        assert_eq!(dst[3], 255);
        assert_eq!(dst[4], 0);
    }

    #[test]
    fn f32be_long_row_matches_scalar() {
        let n = 257usize;
        let mut src = Vec::with_capacity(n * 4);
        for i in 0..n {
            let f = (i as f32) / 256.0;
            src.extend_from_slice(&f.to_be_bytes());
        }
        let mut simd = vec![0u8; n];
        let mut scalar = vec![0u8; n];
        f32be_to_u8(&mut simd, &src);
        for (i, out) in scalar.iter_mut().enumerate() {
            let off = i * 4;
            let bits = u32::from_be_bytes([src[off], src[off + 1], src[off + 2], src[off + 3]]);
            *out = (f32::from_bits(bits).clamp(0.0, 1.0) * 255.0).round() as u8;
        }
        assert_eq!(simd, scalar);
    }

    #[test]
    fn f32be_half_ties_match_scalar_round_away() {
        // Values that scale to *.5 where ties-to-even differs from `.round()`.
        let halves = [2.5f32, 4.5, 126.5, 128.5, 254.5];
        let mut src = Vec::with_capacity(halves.len() * 4);
        for scaled in halves {
            src.extend_from_slice(&(scaled / 255.0).to_be_bytes());
        }
        let mut simd = vec![0u8; halves.len()];
        let mut scalar = vec![0u8; halves.len()];
        f32be_to_u8(&mut simd, &src);
        for (i, out) in scalar.iter_mut().enumerate() {
            *out = halves[i].round() as u8;
        }
        assert_eq!(simd, scalar);
        assert_eq!(simd, [3, 5, 127, 129, 255]);
    }

    /// Boundary / out-of-range floats where clamp-then-scale vs scale-then-clamp
    /// (or round-then-clamp) would diverge by +/-1 if the SIMD order were wrong.
    fn f32be_boundary_inputs() -> Vec<f32> {
        let mut vals = vec![
            -100.0,
            -1.0,
            -0.0,
            0.0,
            f32::from_bits(1),           // tiniest +subnormal
            1.0 / 255.0,                 // ~1 after scale+round
            0.5 / 255.0,                 // exact 0.5 tie -> 1
            1.5 / 255.0,                 // exact 1.5 tie -> 2
            254.5 / 255.0,               // tie near top
            1.0,                         // 255/255
            f32::from_bits(0x3F7F_FFFF), // just below 1.0
            1.0,
            1.0 + f32::EPSILON,
            1.002,
            2.0,
            100.0,
            f32::INFINITY,
            f32::NEG_INFINITY,
        ];
        // Dense sweep around 0 and 1 where float mul rounding is sensitive.
        for i in 0..32 {
            vals.push((i as f32) / 255.0);
            vals.push(1.0 - (i as f32) / 255.0);
            vals.push((i as f32 + 0.5) / 255.0);
        }
        vals
    }

    fn encode_f32be(vals: &[f32]) -> Vec<u8> {
        let mut src = Vec::with_capacity(vals.len() * 4);
        for &f in vals {
            src.extend_from_slice(&f.to_be_bytes());
        }
        src
    }

    #[test]
    fn f32be_boundary_public_path_matches_scalar() {
        let vals = f32be_boundary_inputs();
        let src = encode_f32be(&vals);
        let mut got = vec![0u8; vals.len()];
        let mut expect = vec![0u8; vals.len()];
        f32be_to_u8(&mut got, &src);
        f32be_to_u8_scalar(&mut expect, &src);
        assert_eq!(
            got, expect,
            "public f32be_to_u8 diverged from scalar at boundaries"
        );
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn f32be_sse2_and_avx2_match_scalar_at_boundaries() {
        let vals = f32be_boundary_inputs();
        // Pad to a full AVX2 chunk so the SIMD main loop is exercised.
        let mut padded = vals.clone();
        while padded.len() % 8 != 0 {
            padded.push(0.5);
        }
        let src = encode_f32be(&padded);
        let mut expect = vec![0u8; padded.len()];
        f32be_to_u8_scalar(&mut expect, &src);

        if is_x86_feature_detected!("sse2") {
            let mut sse = vec![0u8; padded.len()];
            unsafe {
                f32be_to_u8_sse2(&mut sse, &src);
            }
            assert_eq!(sse, expect, "SSE2 path diverged from scalar");
        }
        if is_x86_feature_detected!("avx2") {
            let mut avx = vec![0u8; padded.len()];
            unsafe {
                f32be_to_u8_avx2(&mut avx, &src);
            }
            assert_eq!(avx, expect, "AVX2 path diverged from scalar");
        }
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn f32be_neon_matches_scalar_at_boundaries() {
        let vals = f32be_boundary_inputs();
        let mut padded = vals;
        while padded.len() % 4 != 0 {
            padded.push(0.5);
        }
        let src = encode_f32be(&padded);
        let mut expect = vec![0u8; padded.len()];
        let mut neon = vec![0u8; padded.len()];
        f32be_to_u8_scalar(&mut expect, &src);
        unsafe {
            f32be_to_u8_neon(&mut neon, &src);
        }
        assert_eq!(neon, expect, "NEON path diverged from scalar");
    }
}
