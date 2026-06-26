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

//! SIMD-accelerated RGBA8 box-filter (area-averaging) downsampling.
//!
//! Follows the same dispatch pattern as [`crate::simd_swizzle`]: runtime feature
//! detection on x86_64 (AVX2 → SSE4.1 → scalar), NEON on aarch64, scalar fallback.

#[cfg(target_arch = "x86_64")]
use core::arch::x86_64::*;

#[cfg(target_arch = "aarch64")]
use core::arch::aarch64::*;

/// Box-filter (area-averaging) RGBA8 downsample.
///
/// Each output pixel is the average of all source pixels whose centres fall within
/// its footprint.  Operates on a borrowed `&[u8]` slice — zero-copy of the source
/// buffer.
///
/// # Contract
///
/// This function only supports **downscaling** (or same-size). Passing `dst_w > src_w`
/// or `dst_h > src_h` is a logic error — output pixels may have empty source footprints,
/// leading to division by zero in the scalar fallback.
///
/// # Panics
///
/// Panics in debug if any dimension is zero or if `dst` exceeds `src` in either axis.
pub fn downsample_rgba8_box(
    src: &[u8],
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
) -> Vec<u8> {
    // Zero dimensions would divide-by-zero in the scalar path and UB in SIMD
    // paths via get_unchecked.  Return empty — all callers guarantee non-zero
    // but this is a public API.
    if src_w == 0 || src_h == 0 || dst_w == 0 || dst_h == 0 {
        return Vec::new();
    }
    debug_assert!(
        dst_w <= src_w && dst_h <= src_h,
        "downsample_rgba8_box does not support upscaling (src {src_w}x{src_h}, dst {dst_w}x{dst_h})"
    );
    let mut dst = vec![0_u8; dst_w as usize * dst_h as usize * 4];

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            // SAFETY: AVX2 detected via runtime feature check.
            unsafe {
                downsample_rgba8_box_avx2(src, src_w, src_h, &mut dst, dst_w, dst_h);
            }
            return dst;
        }
        if is_x86_feature_detected!("sse4.1") {
            // SAFETY: SSE4.1 detected via runtime feature check.
            unsafe {
                downsample_rgba8_box_sse41(src, src_w, src_h, &mut dst, dst_w, dst_h);
            }
            return dst;
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        // SAFETY: NEON is always available on aarch64.
        unsafe {
            downsample_rgba8_box_neon(src, src_w, src_h, &mut dst, dst_w, dst_h);
        }
        return dst;
    }

    downsample_rgba8_box_scalar(src, src_w, src_h, &mut dst, dst_w, dst_h);
    dst
}

// ── Scalar fallback ───────────────────────────────────────────────────────────

fn downsample_rgba8_box_scalar(
    pixels: &[u8],
    src_w: u32,
    src_h: u32,
    dst: &mut [u8],
    dst_w: u32,
    dst_h: u32,
) {
    let row_stride = src_w as usize * 4;

    for dst_y in 0..dst_h {
        let src_y0 = (dst_y as u64 * src_h as u64) / dst_h as u64;
        let src_y1 = ((dst_y + 1) as u64 * src_h as u64 + dst_h as u64 - 1) / dst_h as u64;
        let src_y1 = src_y1.min(src_h as u64);

        for dst_x in 0..dst_w {
            let src_x0 = (dst_x as u64 * src_w as u64) / dst_w as u64;
            let src_x1 =
                ((dst_x + 1) as u64 * src_w as u64 + dst_w as u64 - 1) / dst_w as u64;
            let src_x1 = src_x1.min(src_w as u64);

            let mut sum_r: u64 = 0;
            let mut sum_g: u64 = 0;
            let mut sum_b: u64 = 0;
            let mut sum_a: u64 = 0;
            let mut count: u64 = 0;

            for sy in src_y0..src_y1 {
                let row_off = sy as usize * row_stride;
                for sx in src_x0..src_x1 {
                    let i = row_off + sx as usize * 4;
                    sum_r += pixels[i] as u64;
                    sum_g += pixels[i + 1] as u64;
                    sum_b += pixels[i + 2] as u64;
                    sum_a += pixels[i + 3] as u64;
                    count += 1;
                }
            }

            let di = (dst_y as usize * dst_w as usize + dst_x as usize) * 4;
            if count > 0 {
                dst[di] = (sum_r / count) as u8;
                dst[di + 1] = (sum_g / count) as u8;
                dst[di + 2] = (sum_b / count) as u8;
                dst[di + 3] = (sum_a / count) as u8;
            }
        }
    }
}

// ── SSE4.1 kernel (4 lanes) ───────────────────────────────────────────────────

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn downsample_rgba8_box_sse41(
    src: &[u8],
    src_w: u32,
    src_h: u32,
    dst: &mut [u8],
    dst_w: u32,
    dst_h: u32,
) {
    // SAFETY: SSE4.1 enabled via #[target_feature]. Caller must ensure support.
    unsafe {
        let src_w_u = src_w as usize;
        let dst_w_u = dst_w as usize;
        let row_stride = src_w_u * 4;

        let mut x0 = vec![0_u32; dst_w_u];
        let mut x1 = vec![0_u32; dst_w_u];
        for dx in 0..dst_w_u {
            x0[dx] = ((dx as u64 * src_w as u64) / dst_w as u64) as u32;
            x1[dx] = (((dx + 1) as u64 * src_w as u64 + dst_w as u64 - 1) / dst_w as u64)
                .min(src_w as u64) as u32;
        }

        let simd_w: usize = 4;
        let blocks = (dst_w_u / simd_w) as isize;

        for dy in 0..dst_h as usize {
            let y0 = ((dy as u64 * src_h as u64) / dst_h as u64) as u32;
            let y1 = (((dy + 1) as u64 * src_h as u64 + dst_h as u64 - 1) / dst_h as u64)
                .min(src_h as u64) as u32;

            for bx in 0..blocks {
                let base_x = bx as usize * simd_w;

                let mut acc_r = _mm_setzero_si128();
                let mut acc_g = _mm_setzero_si128();
                let mut acc_b = _mm_setzero_si128();
                let mut acc_a = _mm_setzero_si128();
                let mut acc_cnt = _mm_setzero_si128();

                let x0_v = _mm_loadu_si128(x0.as_ptr().add(base_x) as *const __m128i);
                let x1_v = _mm_loadu_si128(x1.as_ptr().add(base_x) as *const __m128i);

                let merged_x0 = core::cmp::min(
                    core::cmp::min(x0[base_x], x0[base_x + 1]),
                    core::cmp::min(x0[base_x + 2], x0[base_x + 3]),
                );
                let merged_x1 = core::cmp::max(
                    core::cmp::max(x1[base_x], x1[base_x + 1]),
                    core::cmp::max(x1[base_x + 2], x1[base_x + 3]),
                );

                for sy in y0..y1 {
                    let row_off = sy as usize * row_stride;
                    for sx in merged_x0..merged_x1 {
                        let sp = row_off + sx as usize * 4;
                        let px = u32::from_le_bytes([
                            *src.get_unchecked(sp),
                            *src.get_unchecked(sp + 1),
                            *src.get_unchecked(sp + 2),
                            *src.get_unchecked(sp + 3),
                        ]);

                        let sx_v = _mm_set1_epi32(sx as i32);
                        // Flip the sign bit to use signed comparison intrinsics
                        // (`_mm_cmpgt_epi32`) for unsigned u32 values.  Without the
                        // flip, coordinates ≥ 2^31 would be treated as negative.
                        let sign_bit128 = _mm_set1_epi32(i32::MIN);
                        let x0_v_u = _mm_xor_si128(x0_v, sign_bit128);
                        let x1_v_u = _mm_xor_si128(x1_v, sign_bit128);
                        let sx_v_u = _mm_xor_si128(sx_v, sign_bit128);
                        // sx >= x0  →  NOT(x0 > sx)
                        let mask_ge =
                            _mm_andnot_si128(_mm_cmpgt_epi32(x0_v_u, sx_v_u), _mm_set1_epi32(!0));
                        // sx < x1  →  x1 > sx
                        let mask_lt = _mm_cmpgt_epi32(x1_v_u, sx_v_u);
                        let active = _mm_and_si128(mask_ge, mask_lt);

                        if _mm_testz_si128(active, active) != 0 {
                            continue;
                        }

                        let r_v =
                            _mm_and_si128(_mm_set1_epi32((px & 0xFF) as i32), active);
                        let g_v = _mm_and_si128(
                            _mm_set1_epi32(((px >> 8) & 0xFF) as i32),
                            active,
                        );
                        let b_v = _mm_and_si128(
                            _mm_set1_epi32(((px >> 16) & 0xFF) as i32),
                            active,
                        );
                        let a_v = _mm_and_si128(
                            _mm_set1_epi32(((px >> 24) & 0xFF) as i32),
                            active,
                        );

                        acc_r = _mm_add_epi32(acc_r, r_v);
                        acc_g = _mm_add_epi32(acc_g, g_v);
                        acc_b = _mm_add_epi32(acc_b, b_v);
                        acc_a = _mm_add_epi32(acc_a, a_v);
                        acc_cnt = _mm_sub_epi32(acc_cnt, active);
                    }
                }

                // SAFETY (extract closure): all lane indices are compile-time
                // constants 0..3 inside the match arms below.
                let extract = |v: __m128i, lane: usize| -> u32 {
                    match lane {
                        0 => _mm_extract_epi32::<0>(v) as u32,
                        1 => _mm_extract_epi32::<1>(v) as u32,
                        2 => _mm_extract_epi32::<2>(v) as u32,
                        3 => _mm_extract_epi32::<3>(v) as u32,
                        _ => 0,
                    }
                };
                for i in 0..simd_w {
                    let cnt = extract(acc_cnt, i);
                    if cnt > 0 {
                        let di = (dy * dst_w_u + base_x + i) * 4;
                        *dst.get_unchecked_mut(di) = (extract(acc_r, i) / cnt) as u8;
                        *dst.get_unchecked_mut(di + 1) = (extract(acc_g, i) / cnt) as u8;
                        *dst.get_unchecked_mut(di + 2) = (extract(acc_b, i) / cnt) as u8;
                        *dst.get_unchecked_mut(di + 3) = (extract(acc_a, i) / cnt) as u8;
                    }
                }
            }

            // Scalar tail columns.
            let tail_start = (blocks as usize) * simd_w;
            for dx in tail_start..dst_w_u {
                let src_x0 = x0[dx];
                let src_x1 = x1[dx];
                let mut sum_r: u64 = 0;
                let mut sum_g: u64 = 0;
                let mut sum_b: u64 = 0;
                let mut sum_a: u64 = 0;
                let mut count: u64 = 0;
                for sy in y0..y1 {
                    let row_off = sy as usize * row_stride;
                    for sx in src_x0..src_x1 {
                        let i = row_off + sx as usize * 4;
                        sum_r += *src.get_unchecked(i) as u64;
                        sum_g += *src.get_unchecked(i + 1) as u64;
                        sum_b += *src.get_unchecked(i + 2) as u64;
                        sum_a += *src.get_unchecked(i + 3) as u64;
                        count += 1;
                    }
                }
                let di = (dy * dst_w_u + dx) * 4;
                if count > 0 {
                    *dst.get_unchecked_mut(di) = (sum_r / count) as u8;
                    *dst.get_unchecked_mut(di + 1) = (sum_g / count) as u8;
                    *dst.get_unchecked_mut(di + 2) = (sum_b / count) as u8;
                    *dst.get_unchecked_mut(di + 3) = (sum_a / count) as u8;
                }
            }
        }
    }
}

// ── AVX2 kernel (8 lanes) ─────────────────────────────────────────────────────

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn downsample_rgba8_box_avx2(
    src: &[u8],
    src_w: u32,
    src_h: u32,
    dst: &mut [u8],
    dst_w: u32,
    dst_h: u32,
) {
    // SAFETY: AVX2 enabled via #[target_feature]. Caller must ensure support.
    unsafe {
        let src_w_u = src_w as usize;
        let dst_w_u = dst_w as usize;
        let row_stride = src_w_u * 4;

        let mut x0 = vec![0_u32; dst_w_u];
        let mut x1 = vec![0_u32; dst_w_u];
        for dx in 0..dst_w_u {
            x0[dx] = ((dx as u64 * src_w as u64) / dst_w as u64) as u32;
            x1[dx] = (((dx + 1) as u64 * src_w as u64 + dst_w as u64 - 1) / dst_w as u64)
                .min(src_w as u64) as u32;
        }

        let simd_w: usize = 8;
        let blocks = (dst_w_u / simd_w) as isize;

        for dy in 0..dst_h as usize {
            let y0 = ((dy as u64 * src_h as u64) / dst_h as u64) as u32;
            let y1 = (((dy + 1) as u64 * src_h as u64 + dst_h as u64 - 1) / dst_h as u64)
                .min(src_h as u64) as u32;

            for bx in 0..blocks {
                let base_x = bx as usize * simd_w;

                let mut acc_r = _mm256_setzero_si256();
                let mut acc_g = _mm256_setzero_si256();
                let mut acc_b = _mm256_setzero_si256();
                let mut acc_a = _mm256_setzero_si256();
                let mut acc_cnt = _mm256_setzero_si256();

                let x0_v = _mm256_loadu_si256(x0.as_ptr().add(base_x) as *const __m256i);
                let x1_v = _mm256_loadu_si256(x1.as_ptr().add(base_x) as *const __m256i);

                let merged_x0 =
                    (0..8).fold(u32::MAX, |m, i| core::cmp::min(m, x0[base_x + i]));
                let merged_x1 =
                    (0..8).fold(0_u32, |m, i| core::cmp::max(m, x1[base_x + i]));

                for sy in y0..y1 {
                    let row_off = sy as usize * row_stride;
                    for sx in merged_x0..merged_x1 {
                        let sp = row_off + sx as usize * 4;
                        let px = u32::from_le_bytes([
                            *src.get_unchecked(sp),
                            *src.get_unchecked(sp + 1),
                            *src.get_unchecked(sp + 2),
                            *src.get_unchecked(sp + 3),
                        ]);

                        let sx_v = _mm256_set1_epi32(sx as i32);
                        // Flip the sign bit to use signed comparison intrinsics
                        // (`_mm256_cmpgt_epi32`) for unsigned u32 values.
                        let sign_bit256 = _mm256_set1_epi32(i32::MIN);
                        let x0_v_u = _mm256_xor_si256(x0_v, sign_bit256);
                        let x1_v_u = _mm256_xor_si256(x1_v, sign_bit256);
                        let sx_v_u = _mm256_xor_si256(sx_v, sign_bit256);
                        // sx >= x0  →  NOT(x0 > sx)
                        let mask_ge = _mm256_andnot_si256(
                            _mm256_cmpgt_epi32(x0_v_u, sx_v_u),
                            _mm256_set1_epi32(!0),
                        );
                        // sx < x1  →  x1 > sx
                        let mask_lt = _mm256_cmpgt_epi32(x1_v_u, sx_v_u);
                        let active = _mm256_and_si256(mask_ge, mask_lt);

                        if _mm256_testz_si256(active, active) != 0 {
                            continue;
                        }

                        let r_v = _mm256_and_si256(
                            _mm256_set1_epi32((px & 0xFF) as i32),
                            active,
                        );
                        let g_v = _mm256_and_si256(
                            _mm256_set1_epi32(((px >> 8) & 0xFF) as i32),
                            active,
                        );
                        let b_v = _mm256_and_si256(
                            _mm256_set1_epi32(((px >> 16) & 0xFF) as i32),
                            active,
                        );
                        let a_v = _mm256_and_si256(
                            _mm256_set1_epi32(((px >> 24) & 0xFF) as i32),
                            active,
                        );

                        acc_r = _mm256_add_epi32(acc_r, r_v);
                        acc_g = _mm256_add_epi32(acc_g, g_v);
                        acc_b = _mm256_add_epi32(acc_b, b_v);
                        acc_a = _mm256_add_epi32(acc_a, a_v);
                        acc_cnt = _mm256_sub_epi32(acc_cnt, active);
                    }
                }

                let extract = |v: __m256i, lane: usize| -> u32 {
                    match lane {
                        0 => _mm256_extract_epi32::<0>(v) as u32,
                        1 => _mm256_extract_epi32::<1>(v) as u32,
                        2 => _mm256_extract_epi32::<2>(v) as u32,
                        3 => _mm256_extract_epi32::<3>(v) as u32,
                        4 => _mm256_extract_epi32::<4>(v) as u32,
                        5 => _mm256_extract_epi32::<5>(v) as u32,
                        6 => _mm256_extract_epi32::<6>(v) as u32,
                        7 => _mm256_extract_epi32::<7>(v) as u32,
                        _ => 0,
                    }
                };
                for i in 0..simd_w {
                    let cnt = extract(acc_cnt, i);
                    if cnt > 0 {
                        let di = (dy * dst_w_u + base_x + i) * 4;
                        *dst.get_unchecked_mut(di) = (extract(acc_r, i) / cnt) as u8;
                        *dst.get_unchecked_mut(di + 1) = (extract(acc_g, i) / cnt) as u8;
                        *dst.get_unchecked_mut(di + 2) = (extract(acc_b, i) / cnt) as u8;
                        *dst.get_unchecked_mut(di + 3) = (extract(acc_a, i) / cnt) as u8;
                    }
                }
            }

            // Scalar tail columns.
            let tail_start = (blocks as usize) * simd_w;
            for dx in tail_start..dst_w_u {
                let src_x0 = x0[dx];
                let src_x1 = x1[dx];
                let mut sum_r: u64 = 0;
                let mut sum_g: u64 = 0;
                let mut sum_b: u64 = 0;
                let mut sum_a: u64 = 0;
                let mut count: u64 = 0;
                for sy in y0..y1 {
                    let row_off = sy as usize * row_stride;
                    for sx in src_x0..src_x1 {
                        let i = row_off + sx as usize * 4;
                        sum_r += *src.get_unchecked(i) as u64;
                        sum_g += *src.get_unchecked(i + 1) as u64;
                        sum_b += *src.get_unchecked(i + 2) as u64;
                        sum_a += *src.get_unchecked(i + 3) as u64;
                        count += 1;
                    }
                }
                let di = (dy * dst_w_u + dx) * 4;
                if count > 0 {
                    *dst.get_unchecked_mut(di) = (sum_r / count) as u8;
                    *dst.get_unchecked_mut(di + 1) = (sum_g / count) as u8;
                    *dst.get_unchecked_mut(di + 2) = (sum_b / count) as u8;
                    *dst.get_unchecked_mut(di + 3) = (sum_a / count) as u8;
                }
            }
        }
    }
}

// ── NEON kernel (4 lanes) ─────────────────────────────────────────────────────

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn downsample_rgba8_box_neon(
    src: &[u8],
    src_w: u32,
    src_h: u32,
    dst: &mut [u8],
    dst_w: u32,
    dst_h: u32,
) {
    // SAFETY: NEON enabled via #[target_feature]. Caller must ensure support.
    unsafe {
        let src_w_u = src_w as usize;
        let dst_w_u = dst_w as usize;
        let row_stride = src_w_u * 4;

        let mut x0 = vec![0_u32; dst_w_u];
        let mut x1 = vec![0_u32; dst_w_u];
        for dx in 0..dst_w_u {
            x0[dx] = ((dx as u64 * src_w as u64) / dst_w as u64) as u32;
            x1[dx] = (((dx + 1) as u64 * src_w as u64 + dst_w as u64 - 1) / dst_w as u64)
                .min(src_w as u64) as u32;
        }

        let simd_w: usize = 4;
        let blocks = (dst_w_u / simd_w) as isize;

        for dy in 0..dst_h as usize {
            let y0 = ((dy as u64 * src_h as u64) / dst_h as u64) as u32;
            let y1 = (((dy + 1) as u64 * src_h as u64 + dst_h as u64 - 1) / dst_h as u64)
                .min(src_h as u64) as u32;

            for bx in 0..blocks {
                let base_x = bx as usize * simd_w;

                let mut acc_r = vdupq_n_u32(0);
                let mut acc_g = vdupq_n_u32(0);
                let mut acc_b = vdupq_n_u32(0);
                let mut acc_a = vdupq_n_u32(0);
                let mut acc_cnt = vdupq_n_u32(0);

                let x0_v = vld1q_u32(x0.as_ptr().add(base_x));
                let x1_v = vld1q_u32(x1.as_ptr().add(base_x));

                let merged_x0 = core::cmp::min(
                    core::cmp::min(x0[base_x], x0[base_x + 1]),
                    core::cmp::min(x0[base_x + 2], x0[base_x + 3]),
                );
                let merged_x1 = core::cmp::max(
                    core::cmp::max(x1[base_x], x1[base_x + 1]),
                    core::cmp::max(x1[base_x + 2], x1[base_x + 3]),
                );

                for sy in y0..y1 {
                    let row_off = sy as usize * row_stride;
                    for sx in merged_x0..merged_x1 {
                        let sp = row_off + sx as usize * 4;
                        let px = u32::from_le_bytes([
                            *src.get_unchecked(sp),
                            *src.get_unchecked(sp + 1),
                            *src.get_unchecked(sp + 2),
                            *src.get_unchecked(sp + 3),
                        ]);

                        let sx_v = vdupq_n_u32(sx);
                        // sx >= x0  →  NOT(x0 > sx)
                        let mask_ge = vmvnq_u32(vcgtq_u32(x0_v, sx_v));
                        // sx < x1  →  x1 > sx
                        let mask_lt = vcgtq_u32(x1_v, sx_v);
                        let active = vandq_u32(mask_ge, mask_lt);

                        // Check if any lane is active via horizontal OR reduction.
                        let or_low_high = vorr_u32(
                            vget_low_u32(active),
                            vget_high_u32(active),
                        );
                        let any_active = vget_lane_u32::<0>(vorr_u32(
                            or_low_high,
                            vreinterpret_u32_u64(vshr_n_u64::<32>(
                                vreinterpret_u64_u32(or_low_high),
                            )),
                        )) != 0;

                        if !any_active {
                            continue;
                        }

                        let r_v = vandq_u32(vdupq_n_u32(px & 0xFF), active);
                        let g_v =
                            vandq_u32(vdupq_n_u32((px >> 8) & 0xFF), active);
                        let b_v =
                            vandq_u32(vdupq_n_u32((px >> 16) & 0xFF), active);
                        let a_v =
                            vandq_u32(vdupq_n_u32((px >> 24) & 0xFF), active);

                        acc_r = vaddq_u32(acc_r, r_v);
                        acc_g = vaddq_u32(acc_g, g_v);
                        acc_b = vaddq_u32(acc_b, b_v);
                        acc_a = vaddq_u32(acc_a, a_v);
                        acc_cnt = vsubq_u32(acc_cnt, active);
                    }
                }

                // SAFETY (extract closure): all lane indices are compile-time
                // constants 0..3 inside the match arms below.
                let extract = |v: uint32x4_t, lane: usize| -> u32 {
                    match lane {
                        0 => vgetq_lane_u32::<0>(v),
                        1 => vgetq_lane_u32::<1>(v),
                        2 => vgetq_lane_u32::<2>(v),
                        3 => vgetq_lane_u32::<3>(v),
                        _ => 0,
                    }
                };
                for i in 0..simd_w {
                    let cnt = extract(acc_cnt, i);
                    if cnt > 0 {
                        let di = (dy * dst_w_u + base_x + i) * 4;
                        *dst.get_unchecked_mut(di) = (extract(acc_r, i) / cnt) as u8;
                        *dst.get_unchecked_mut(di + 1) = (extract(acc_g, i) / cnt) as u8;
                        *dst.get_unchecked_mut(di + 2) = (extract(acc_b, i) / cnt) as u8;
                        *dst.get_unchecked_mut(di + 3) = (extract(acc_a, i) / cnt) as u8;
                    }
                }
            }

            // Scalar tail columns.
            let tail_start = (blocks as usize) * simd_w;
            for dx in tail_start..dst_w_u {
                let src_x0 = x0[dx];
                let src_x1 = x1[dx];
                let mut sum_r: u64 = 0;
                let mut sum_g: u64 = 0;
                let mut sum_b: u64 = 0;
                let mut sum_a: u64 = 0;
                let mut count: u64 = 0;
                for sy in y0..y1 {
                    let row_off = sy as usize * row_stride;
                    for sx in src_x0..src_x1 {
                        let i = row_off + sx as usize * 4;
                        sum_r += *src.get_unchecked(i) as u64;
                        sum_g += *src.get_unchecked(i + 1) as u64;
                        sum_b += *src.get_unchecked(i + 2) as u64;
                        sum_a += *src.get_unchecked(i + 3) as u64;
                        count += 1;
                    }
                }
                let di = (dy * dst_w_u + dx) * 4;
                if count > 0 {
                    *dst.get_unchecked_mut(di) = (sum_r / count) as u8;
                    *dst.get_unchecked_mut(di + 1) = (sum_g / count) as u8;
                    *dst.get_unchecked_mut(di + 2) = (sum_b / count) as u8;
                    *dst.get_unchecked_mut(di + 3) = (sum_a / count) as u8;
                }
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference scalar implementation — identical to the fallback.
    fn downsample_rgba8_box_reference(
        pixels: &[u8],
        src_w: u32,
        src_h: u32,
        dst_w: u32,
        dst_h: u32,
    ) -> Vec<u8> {
        let mut dst = vec![0_u8; dst_w as usize * dst_h as usize * 4];
        let row_stride = src_w as usize * 4;

        for dst_y in 0..dst_h {
            let src_y0 = (dst_y as u64 * src_h as u64) / dst_h as u64;
            let src_y1 =
                ((dst_y + 1) as u64 * src_h as u64 + dst_h as u64 - 1) / dst_h as u64;
            let src_y1 = src_y1.min(src_h as u64);

            for dst_x in 0..dst_w {
                let src_x0 = (dst_x as u64 * src_w as u64) / dst_w as u64;
                let src_x1 =
                    ((dst_x + 1) as u64 * src_w as u64 + dst_w as u64 - 1) / dst_w as u64;
                let src_x1 = src_x1.min(src_w as u64);

                let mut sum_r: u64 = 0;
                let mut sum_g: u64 = 0;
                let mut sum_b: u64 = 0;
                let mut sum_a: u64 = 0;
                let mut count: u64 = 0;

                for sy in src_y0..src_y1 {
                    let row_off = sy as usize * row_stride;
                    for sx in src_x0..src_x1 {
                        let i = row_off + sx as usize * 4;
                        sum_r += pixels[i] as u64;
                        sum_g += pixels[i + 1] as u64;
                        sum_b += pixels[i + 2] as u64;
                        sum_a += pixels[i + 3] as u64;
                        count += 1;
                    }
                }

                let di = (dst_y as usize * dst_w as usize + dst_x as usize) * 4;
                dst[di] = (sum_r / count) as u8;
                dst[di + 1] = (sum_g / count) as u8;
                dst[di + 2] = (sum_b / count) as u8;
                dst[di + 3] = (sum_a / count) as u8;
            }
        }
        dst
    }

    fn make_patterned_rgba(w: u32, h: u32) -> Vec<u8> {
        let len = (w * h * 4) as usize;
        (0..len)
            .map(|i| ((i.wrapping_mul(13).wrapping_add(7)) % 256) as u8)
            .collect()
    }

    const TEST_SIZES: &[(u32, u32, u32, u32)] = &[
        (8, 8, 4, 4),
        (16, 16, 8, 8),
        (32, 32, 16, 16),
        (64, 48, 32, 24),
        (128, 128, 64, 64),
        (100, 100, 30, 30),
        (256, 128, 128, 64),
        (500, 300, 250, 150),
        // Edge cases
        (1, 100, 1, 50),
        (100, 1, 50, 1),
        (8, 8, 1, 1),
        (8, 8, 8, 1),
        (8, 8, 1, 8),
        // Non-power-of-two
        (7, 7, 3, 3),
        (13, 11, 5, 4),
        (300, 200, 128, 85),
    ];

    #[test]
    fn simd_downsample_box_matches_scalar() {
        for &(sw, sh, dw, dh) in TEST_SIZES {
            let src = make_patterned_rgba(sw, sh);
            let simd_result = downsample_rgba8_box(&src, sw, sh, dw, dh);
            let scalar_result = downsample_rgba8_box_reference(&src, sw, sh, dw, dh);
            assert_eq!(
                simd_result.len(),
                scalar_result.len(),
                "size mismatch: {sw}x{sh} -> {dw}x{dh}"
            );
            assert_eq!(
                simd_result, scalar_result,
                "pixel mismatch: {sw}x{sh} -> {dw}x{dh}"
            );
        }
    }

    #[test]
    fn scalar_fallback_matches_reference() {
        for &(sw, sh, dw, dh) in TEST_SIZES {
            let src = make_patterned_rgba(sw, sh);
            let mut scalar_dst = vec![0_u8; dw as usize * dh as usize * 4];
            downsample_rgba8_box_scalar(&src, sw, sh, &mut scalar_dst, dw, dh);
            let ref_result = downsample_rgba8_box_reference(&src, sw, sh, dw, dh);
            assert_eq!(
                scalar_dst, ref_result,
                "scalar mismatch: {sw}x{sh} -> {dw}x{dh}"
            );
        }
    }

    #[test]
    fn identity_is_noop() {
        let src = make_patterned_rgba(16, 16);
        let result = downsample_rgba8_box(&src, 16, 16, 16, 16);
        assert_eq!(result.len(), 16 * 16 * 4);
        for i in 0..16_usize {
            for j in 0..16_usize {
                let si = (i * 16 + j) * 4;
                let di = (i * 16 + j) * 4;
                assert_eq!(
                    result[di..di + 4],
                    src[si..si + 4],
                    "1:1 mismatch at ({j},{i})"
                );
            }
        }
    }

    #[test]
    fn uniform_input_produces_uniform_output() {
        let src = vec![128_u8; 32 * 32 * 4];
        let result = downsample_rgba8_box(&src, 32, 32, 16, 16);
        assert_eq!(result, vec![128_u8; 16 * 16 * 4]);
    }

    #[test]
    fn black_and_white_averages() {
        let mut src = vec![0_u8; 4 * 4 * 4];
        // Top-left 2x2 is white.
        for y in 0..2_u32 {
            for x in 0..2_u32 {
                let i = (y * 4 + x) as usize * 4;
                src[i] = 255;
                src[i + 1] = 255;
                src[i + 2] = 255;
                src[i + 3] = 255;
            }
        }
        let result = downsample_rgba8_box(&src, 4, 4, 2, 2);
        assert!(result[0] > 0, "top-left should not be pure black");
        assert_eq!(result[4], 0, "bottom-left should be black");
    }

    #[test]
    #[cfg_attr(debug_assertions, should_panic(expected = "upscaling"))]
    fn upscale_panics_in_debug() {
        let src = make_patterned_rgba(10, 10);
        let _result = downsample_rgba8_box(&src, 10, 10, 20, 20);
    }

    #[test]
    fn near_identity_downscale() {
        // 20→10 is a 2× downscale, well within the supported contract.
        let src = make_patterned_rgba(20, 20);
        let result = downsample_rgba8_box(&src, 20, 20, 10, 10);
        assert_eq!(result.len(), 10 * 10 * 4);
    }

    #[test]
    fn scalar_produces_consistent_output() {
        let src = make_patterned_rgba(64, 64);
        let r1 = downsample_rgba8_box(&src, 64, 64, 32, 32);
        let r2 = downsample_rgba8_box(&src, 64, 64, 32, 32);
        assert_eq!(r1, r2);
    }
}
