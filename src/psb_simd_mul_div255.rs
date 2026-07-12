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

//! Shared exact `a * b / 255` SIMD helpers for planar / interleaved u8 paths.
//!
//! Uses `(prod * 0x8081) >> 23` so results match scalar truncated integer divides
//! for products in `0..=65025`.

/// Exact `c * k / 255` for 8 low lanes of `c` and `k` (SSE4.1).
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
pub(crate) unsafe fn mul_div255_u8x8(
    c: core::arch::x86_64::__m128i,
    k: core::arch::x86_64::__m128i,
) -> core::arch::x86_64::__m128i {
    use core::arch::x86_64::*;
    let c16 = _mm_cvtepu8_epi16(c);
    let k16 = _mm_cvtepu8_epi16(k);
    let prod = _mm_mullo_epi16(c16, k16);
    let prod_lo = _mm_cvtepu16_epi32(prod);
    let prod_hi = _mm_cvtepu16_epi32(_mm_srli_si128(prod, 8));
    let magic = _mm_set1_epi32(0x8081);
    let q_lo = _mm_srli_epi32(_mm_mullo_epi32(prod_lo, magic), 23);
    let q_hi = _mm_srli_epi32(_mm_mullo_epi32(prod_hi, magic), 23);
    let q16 = _mm_packus_epi32(q_lo, q_hi);
    _mm_packus_epi16(q16, _mm_setzero_si128())
}

/// Exact `c * k / 255` for 16 lanes of `c` and `k` (AVX2).
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
    let magic = _mm256_set1_epi32(0x8081);
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
