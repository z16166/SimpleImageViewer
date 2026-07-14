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

//! Shared exact `a * b / 255` helpers for planar / interleaved u8 paths.
//!
//! Uses `(prod * 0x8081) >> 23` so results match scalar truncated integer divides
//! for products in `0..=65025`.
//! Pixel buffers may be unaligned, so callers use unaligned loads and stores
//! (`loadu` / `storeu` on x86 and `vld1` / `vld1q` on NEON).

/// Multiply-by-255-reciprocal magic constant: `(x * 0x8081) >> 23` = `x / 255` for `x ≤ 65025`.
const DIV255_MAGIC: u32 = 0x8081;

/// Exact `x / 255` for `x` in 0..=65025 via `(x * DIV255_MAGIC) >> 23`.
#[inline]
pub(crate) fn div255_u16_exact(x: u16) -> u8 {
    (((x as u32) * DIV255_MAGIC) >> 23) as u8
}

/// Exact `c * k / 255` for 8 low lanes of `c` and `k` (SSE2).
///
/// Uses unpack + `_mm_mul_epu32` instead of SSE4.1 `cvtepu*` / `mullo_epi32` /
/// `packus_epi32`, so pre-Nehalem / pre-Bulldozer x86_64 CPUs still hit SIMD.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
pub(crate) unsafe fn mul_div255_u8x8(
    c: core::arch::x86_64::__m128i,
    k: core::arch::x86_64::__m128i,
) -> core::arch::x86_64::__m128i {
    use core::arch::x86_64::*;
    let zero = _mm_setzero_si128();
    let c16 = _mm_unpacklo_epi8(c, zero);
    let k16 = _mm_unpacklo_epi8(k, zero);
    let prod = _mm_mullo_epi16(c16, k16);
    let magic = _mm_set1_epi32(DIV255_MAGIC as i32);

    // `(prod * 0x8081) >> 23` via 64-bit `mul_epu32` on even/odd dwords.
    // For products in 0..=65025 the 64-bit product fits in 32 bits, so the
    // high dword after `>> 23` is zero and interleaving with OR is safe.
    let p32_lo = _mm_unpacklo_epi16(prod, zero);
    let q_even_lo = _mm_srli_epi64(_mm_mul_epu32(p32_lo, magic), 23);
    let q_odd_lo = _mm_srli_epi64(_mm_mul_epu32(_mm_srli_si128(p32_lo, 4), magic), 23);
    let q_lo = _mm_or_si128(q_even_lo, _mm_slli_si128(q_odd_lo, 4));

    let p32_hi = _mm_unpackhi_epi16(prod, zero);
    let q_even_hi = _mm_srli_epi64(_mm_mul_epu32(p32_hi, magic), 23);
    let q_odd_hi = _mm_srli_epi64(_mm_mul_epu32(_mm_srli_si128(p32_hi, 4), magic), 23);
    let q_hi = _mm_or_si128(q_even_hi, _mm_slli_si128(q_odd_hi, 4));

    // Values are 0..=255; signed packs_epi32 is fine in that range.
    let q16 = _mm_packs_epi32(q_lo, q_hi);
    _mm_packus_epi16(q16, zero)
}

/// Exact `c * k / 255` for 16 lanes of `c` and `k` (AVX2).
///
/// Inputs are `__m128i` holding 16 `u8` lanes (not `__m256i`): AVX2
/// `_mm256_cvtepu8_epi16` zero-extends those 16 bytes into a `__m256i` of
/// u16, then the result is packed back to 16 `u8` in a `__m128i`.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
pub(crate) unsafe fn mul_div255_u8x16(
    c: core::arch::x86_64::__m128i,
    k: core::arch::x86_64::__m128i,
) -> core::arch::x86_64::__m128i {
    use core::arch::x86_64::*;
    let c16 = _mm256_cvtepu8_epi16(c);
    let k16 = _mm256_cvtepu8_epi16(k);
    let prod = _mm256_mullo_epi16(c16, k16);
    let magic = _mm256_set1_epi32(DIV255_MAGIC as i32);
    // 8 u16 in each 128-bit half -> 8 u32 each.
    let p0 = _mm256_cvtepu16_epi32(_mm256_castsi256_si128(prod));
    let p1 = _mm256_cvtepu16_epi32(_mm256_extracti128_si256::<1>(prod));
    let q0 = _mm256_srli_epi32(_mm256_mullo_epi32(p0, magic), 23);
    let q1 = _mm256_srli_epi32(_mm256_mullo_epi32(p1, magic), 23);
    // packus_epi32 is per-lane; permute to [q0[0..7], q1[0..7]] as u16.
    let q16 = _mm256_permute4x64_epi64::<0xD8>(_mm256_packus_epi32(q0, q1));
    _mm_packus_epi16(
        _mm256_castsi256_si128(q16),
        _mm256_extracti128_si256::<1>(q16),
    )
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
pub(crate) unsafe fn mul_div255_u16x8_neon(
    prod: core::arch::aarch64::uint16x8_t,
) -> core::arch::aarch64::uint16x8_t {
    use core::arch::aarch64::*;
    let lo = vmovl_u16(vget_low_u16(prod));
    let hi = vmovl_u16(vget_high_u16(prod));
    let magic = vdupq_n_u32(DIV255_MAGIC as u32);
    let q_lo = vshrq_n_u32(vmulq_u32(lo, magic), 23);
    let q_hi = vshrq_n_u32(vmulq_u32(hi, magic), 23);
    vcombine_u16(vmovn_u32(q_lo), vmovn_u32(q_hi))
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
pub(crate) unsafe fn mul_div255_u8x16_neon(
    c: core::arch::aarch64::uint8x16_t,
    k: core::arch::aarch64::uint8x16_t,
) -> core::arch::aarch64::uint8x16_t {
    use core::arch::aarch64::*;
    let prod_lo = vmull_u8(vget_low_u8(c), vget_low_u8(k));
    let prod_hi = vmull_u8(vget_high_u8(c), vget_high_u8(k));
    let q_lo = unsafe { mul_div255_u16x8_neon(prod_lo) };
    let q_hi = unsafe { mul_div255_u16x8_neon(prod_hi) };
    vcombine_u8(vmovn_u16(q_lo), vmovn_u16(q_hi))
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
pub(crate) unsafe fn mul_div255_u8x8_neon(
    c: core::arch::aarch64::uint8x8_t,
    k: core::arch::aarch64::uint8x8_t,
) -> core::arch::aarch64::uint8x8_t {
    use core::arch::aarch64::*;
    let prod = vmull_u8(c, k);
    vmovn_u16(unsafe { mul_div255_u16x8_neon(prod) })
}

#[cfg(all(test, target_arch = "x86_64"))]
mod tests {
    use super::{mul_div255_u8x8, mul_div255_u8x16};
    use core::arch::x86_64::*;

    #[test]
    fn mul_div255_u8x8_matches_scalar() {
        if !is_x86_feature_detected!("sse2") {
            return;
        }
        // Spot-check corners and a dense grid of (c,k) pairs.
        let mut pairs = Vec::new();
        for c in [0u8, 1, 2, 127, 128, 254, 255] {
            for k in [0u8, 1, 2, 127, 128, 254, 255] {
                pairs.push((c, k));
            }
        }
        for c in (0u8..=255).step_by(17) {
            for k in (0u8..=255).step_by(19) {
                pairs.push((c, k));
            }
        }

        for chunk in pairs.chunks(8) {
            let mut c = [0u8; 8];
            let mut k = [0u8; 8];
            let mut expect = [0u8; 8];
            for (i, &(ci, ki)) in chunk.iter().enumerate() {
                c[i] = ci;
                k[i] = ki;
                expect[i] = ((ci as u16 * ki as u16) / 255) as u8;
            }
            let got = unsafe {
                let cv = _mm_loadl_epi64(c.as_ptr().cast());
                let kv = _mm_loadl_epi64(k.as_ptr().cast());
                let out = mul_div255_u8x8(cv, kv);
                let mut buf = [0u8; 8];
                _mm_storel_epi64(buf.as_mut_ptr().cast(), out);
                buf
            };
            assert_eq!(&got[..chunk.len()], &expect[..chunk.len()]);
        }
    }

    #[test]
    fn mul_div255_u8x16_avx2_matches_scalar() {
        if !is_x86_feature_detected!("avx2") {
            return;
        }
        let mut pairs = Vec::new();
        for c in [0u8, 1, 2, 127, 128, 254, 255] {
            for k in [0u8, 1, 2, 127, 128, 254, 255] {
                pairs.push((c, k));
            }
        }
        for c in (0u8..=255).step_by(17) {
            for k in (0u8..=255).step_by(19) {
                pairs.push((c, k));
            }
        }

        for chunk in pairs.chunks(16) {
            let mut c = [0u8; 16];
            let mut k = [0u8; 16];
            let mut expect = [0u8; 16];
            for (i, &(ci, ki)) in chunk.iter().enumerate() {
                c[i] = ci;
                k[i] = ki;
                expect[i] = ((ci as u16 * ki as u16) / 255) as u8;
            }
            let got = unsafe {
                let cv = _mm_loadu_si128(c.as_ptr().cast());
                let kv = _mm_loadu_si128(k.as_ptr().cast());
                let out = mul_div255_u8x16(cv, kv);
                let mut buf = [0u8; 16];
                _mm_storeu_si128(buf.as_mut_ptr().cast(), out);
                buf
            };
            assert_eq!(&got[..chunk.len()], &expect[..chunk.len()]);
        }
    }
}

/// Arch-agnostic coverage for the shared scalar helper used by every SIMD path.
#[cfg(test)]
mod arch_agnostic_tests {
    use super::div255_u16_exact;

    #[test]
    fn div255_u16_exact_matches_integer_div_full_domain() {
        for x in 0u16..=65025 {
            assert_eq!(div255_u16_exact(x), (x / 255) as u8, "x={x}");
        }
    }
}
