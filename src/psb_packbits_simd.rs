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

//! SIMD helpers for PackBits RLE expand (repeat-byte fills).
//!
//! PackBits control flow stays scalar; large repeat runs are filled with
//! SSE2 / AVX2 / NEON stores. Bit-identical to `slice.fill(val)`.

/// Fill `dst` with `val` using the widest available SIMD path.
#[inline]
pub fn fill_bytes(dst: &mut [u8], val: u8) {
    if dst.is_empty() {
        return;
    }

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            unsafe {
                fill_bytes_avx2(dst, val);
            }
            return;
        }
        if is_x86_feature_detected!("sse2") {
            unsafe {
                fill_bytes_sse2(dst, val);
            }
            return;
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        unsafe {
            fill_bytes_neon(dst, val);
        }
        return;
    }

    dst.fill(val);
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn fill_bytes_sse2(dst: &mut [u8], val: u8) {
    use core::arch::x86_64::*;
    let mut i = 0usize;
    let n = dst.len();
    let pattern = _mm_set1_epi8(val as i8);
    while i + 16 <= n {
        unsafe {
            _mm_storeu_si128(dst.as_mut_ptr().add(i).cast(), pattern);
        }
        i += 16;
    }
    if i < n {
        dst[i..].fill(val);
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn fill_bytes_avx2(dst: &mut [u8], val: u8) {
    use core::arch::x86_64::*;
    let mut i = 0usize;
    let n = dst.len();
    let pattern = _mm256_set1_epi8(val as i8);
    while i + 32 <= n {
        unsafe {
            _mm256_storeu_si256(dst.as_mut_ptr().add(i).cast(), pattern);
        }
        i += 32;
    }
    if i + 16 <= n {
        let pattern16 = _mm_set1_epi8(val as i8);
        unsafe {
            _mm_storeu_si128(dst.as_mut_ptr().add(i).cast(), pattern16);
        }
        i += 16;
    }
    if i < n {
        dst[i..].fill(val);
    }
}

#[cfg(target_arch = "aarch64")]
unsafe fn fill_bytes_neon(dst: &mut [u8], val: u8) {
    use core::arch::aarch64::*;
    let mut i = 0usize;
    let n = dst.len();
    let pattern = vdupq_n_u8(val);
    while i + 16 <= n {
        unsafe {
            vst1q_u8(dst.as_mut_ptr().add(i), pattern);
        }
        i += 16;
    }
    if i < n {
        dst[i..].fill(val);
    }
}

#[cfg(test)]
mod tests {
    use super::fill_bytes;

    #[test]
    fn fill_bytes_matches_slice_fill() {
        for len in [0usize, 1, 15, 16, 17, 31, 32, 33, 64, 1000] {
            for &val in &[0u8, 1, 127, 128, 255] {
                let mut a = vec![7u8; len];
                let mut b = vec![7u8; len];
                fill_bytes(&mut a, val);
                b.fill(val);
                assert_eq!(a, b, "len={len} val={val}");
            }
        }
    }
}
