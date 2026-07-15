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

//! AVX2 8-pixel Apple HEIF gain-map compose (sibling of `core` SSE4.1 / NEON paths).

use super::core::{
    BT709_DIVISOR, BT709_GAMMA, BT709_LINEAR_SEGMENT_BREAK, BT709_OFFSET, BT709_SCALE,
    ComposeFastPath, ComposeRowTransform, DISPLAY_P3_TO_LINEAR_SRGB, SRGB_DIVISOR, SRGB_GAMMA,
    SRGB_LINEAR_SEGMENT_END, SRGB_OFFSET, SRGB_SCALE, compose_pixel_scalar,
    load_rgb_interleaved4_sse41, path_applies_display_p3_matrix,
};
use crate::hdr::simd_fast_pow::pow8_avx2;
use core::arch::x86_64::*;

pub(crate) const SIMD_PIXELS_PER_AVX2_STEP: u32 = 8;

#[target_feature(enable = "avx2,sse4.1")]
unsafe fn load_rgba_pixel8_avx2(
    row: *const f32,
    pixel_offset: usize,
) -> (__m256, __m256, __m256, __m256) {
    unsafe {
        // Two groups of 4 interleaved RGBA -> planar via SSE transpose, then combine.
        let mut p0 = _mm_loadu_ps(row.add(pixel_offset));
        let mut p1 = _mm_loadu_ps(row.add(pixel_offset + 4));
        let mut p2 = _mm_loadu_ps(row.add(pixel_offset + 8));
        let mut p3 = _mm_loadu_ps(row.add(pixel_offset + 12));
        _MM_TRANSPOSE4_PS(&mut p0, &mut p1, &mut p2, &mut p3);

        let mut q0 = _mm_loadu_ps(row.add(pixel_offset + 16));
        let mut q1 = _mm_loadu_ps(row.add(pixel_offset + 20));
        let mut q2 = _mm_loadu_ps(row.add(pixel_offset + 24));
        let mut q3 = _mm_loadu_ps(row.add(pixel_offset + 28));
        _MM_TRANSPOSE4_PS(&mut q0, &mut q1, &mut q2, &mut q3);

        let r = _mm256_insertf128_ps(_mm256_castps128_ps256(p0), q0, 1);
        let g = _mm256_insertf128_ps(_mm256_castps128_ps256(p1), q1, 1);
        let b = _mm256_insertf128_ps(_mm256_castps128_ps256(p2), q2, 1);
        let a = _mm256_insertf128_ps(_mm256_castps128_ps256(p3), q3, 1);
        (r, g, b, a)
    }
}

#[target_feature(enable = "avx2,sse4.1")]
unsafe fn store_rgba_pixel8_avx2(
    row: *mut f32,
    pixel_offset: usize,
    r: __m256,
    g: __m256,
    b: __m256,
    a: __m256,
) {
    unsafe {
        let r_lo = _mm256_castps256_ps128(r);
        let r_hi = _mm256_extractf128_ps(r, 1);
        let g_lo = _mm256_castps256_ps128(g);
        let g_hi = _mm256_extractf128_ps(g, 1);
        let b_lo = _mm256_castps256_ps128(b);
        let b_hi = _mm256_extractf128_ps(b, 1);
        let a_lo = _mm256_castps256_ps128(a);
        let a_hi = _mm256_extractf128_ps(a, 1);

        let rg_lo = _mm_unpacklo_ps(r_lo, g_lo);
        let rg_hi = _mm_unpackhi_ps(r_lo, g_lo);
        let ba_lo = _mm_unpacklo_ps(b_lo, a_lo);
        let ba_hi = _mm_unpackhi_ps(b_lo, a_lo);
        let p0 = _mm_movelh_ps(rg_lo, ba_lo);
        let p1 = _mm_movehl_ps(ba_lo, rg_lo);
        let p2 = _mm_movelh_ps(rg_hi, ba_hi);
        let p3 = _mm_movehl_ps(ba_hi, rg_hi);

        let rg2_lo = _mm_unpacklo_ps(r_hi, g_hi);
        let rg2_hi = _mm_unpackhi_ps(r_hi, g_hi);
        let ba2_lo = _mm_unpacklo_ps(b_hi, a_hi);
        let ba2_hi = _mm_unpackhi_ps(b_hi, a_hi);
        let p4 = _mm_movelh_ps(rg2_lo, ba2_lo);
        let p5 = _mm_movehl_ps(ba2_lo, rg2_lo);
        let p6 = _mm_movelh_ps(rg2_hi, ba2_hi);
        let p7 = _mm_movehl_ps(ba2_hi, rg2_hi);

        _mm_storeu_ps(row.add(pixel_offset), p0);
        _mm_storeu_ps(row.add(pixel_offset + 4), p1);
        _mm_storeu_ps(row.add(pixel_offset + 8), p2);
        _mm_storeu_ps(row.add(pixel_offset + 12), p3);
        _mm_storeu_ps(row.add(pixel_offset + 16), p4);
        _mm_storeu_ps(row.add(pixel_offset + 20), p5);
        _mm_storeu_ps(row.add(pixel_offset + 24), p6);
        _mm_storeu_ps(row.add(pixel_offset + 28), p7);
    }
}

#[target_feature(enable = "avx2,sse4.1")]
unsafe fn gather_gain_rgb8_avx2(
    gain_rgb: *const f32,
    pixel_offset: usize,
) -> (__m256, __m256, __m256) {
    unsafe {
        let base = gain_rgb.add(pixel_offset * 3);
        let (r0, g0, b0) = load_rgb_interleaved4_sse41(base);
        let (r1, g1, b1) = load_rgb_interleaved4_sse41(base.add(12));
        (
            _mm256_insertf128_ps(_mm256_castps128_ps256(r0), r1, 1),
            _mm256_insertf128_ps(_mm256_castps128_ps256(g0), g1, 1),
            _mm256_insertf128_ps(_mm256_castps128_ps256(b0), b1, 1),
        )
    }
}

#[target_feature(enable = "avx2", enable = "fma")]
unsafe fn srgb_to_linear8_avx2(v: __m256) -> __m256 {
    unsafe {
        let zero = _mm256_setzero_ps();
        let one = _mm256_set1_ps(1.0);
        let clamped = _mm256_min_ps(_mm256_max_ps(v, zero), one);
        let threshold = _mm256_set1_ps(SRGB_LINEAR_SEGMENT_END);
        let low_mask = _mm256_cmp_ps(clamped, threshold, _CMP_LE_OQ);
        let low = _mm256_div_ps(clamped, _mm256_set1_ps(SRGB_DIVISOR));
        let adjusted = _mm256_div_ps(
            _mm256_add_ps(clamped, _mm256_set1_ps(SRGB_OFFSET)),
            _mm256_set1_ps(SRGB_SCALE),
        );
        let high = pow8_avx2(adjusted, SRGB_GAMMA);
        _mm256_blendv_ps(high, low, low_mask)
    }
}

#[target_feature(enable = "avx2", enable = "fma")]
unsafe fn bt709_to_linear8_avx2(v: __m256) -> __m256 {
    unsafe {
        let zero = _mm256_setzero_ps();
        let one = _mm256_set1_ps(1.0);
        let clamped = _mm256_min_ps(_mm256_max_ps(v, zero), one);
        let threshold = _mm256_set1_ps(BT709_LINEAR_SEGMENT_BREAK);
        let low_mask = _mm256_cmp_ps(clamped, threshold, _CMP_LT_OQ);
        let low = _mm256_div_ps(clamped, _mm256_set1_ps(BT709_DIVISOR));
        let adjusted = _mm256_div_ps(
            _mm256_add_ps(clamped, _mm256_set1_ps(BT709_OFFSET)),
            _mm256_set1_ps(BT709_SCALE),
        );
        let high = pow8_avx2(adjusted, BT709_GAMMA);
        _mm256_blendv_ps(high, low, low_mask)
    }
}

#[target_feature(enable = "avx2", enable = "fma")]
unsafe fn apply_display_p3_matrix8_avx2(
    r: __m256,
    g: __m256,
    b: __m256,
) -> (__m256, __m256, __m256) {
    let m = DISPLAY_P3_TO_LINEAR_SRGB;
    let lr = _mm256_fmadd_ps(
        b, _mm256_set1_ps(m[0][2]),
        _mm256_fmadd_ps(
            g, _mm256_set1_ps(m[0][1]),
            _mm256_mul_ps(r, _mm256_set1_ps(m[0][0])),
        ),
    );
    let lg = _mm256_fmadd_ps(
        b, _mm256_set1_ps(m[1][2]),
        _mm256_fmadd_ps(
            g, _mm256_set1_ps(m[1][1]),
            _mm256_mul_ps(r, _mm256_set1_ps(m[1][0])),
        ),
    );
    let lb = _mm256_fmadd_ps(
        b, _mm256_set1_ps(m[2][2]),
        _mm256_fmadd_ps(
            g, _mm256_set1_ps(m[2][1]),
            _mm256_mul_ps(r, _mm256_set1_ps(m[2][0])),
        ),
    );
    (lr, lg, lb)
}

struct Avx2Gain8 {
    linear_r: __m256,
    linear_g: __m256,
    linear_b: __m256,
    alpha: __m256,
    gain_r: __m256,
    gain_g: __m256,
    gain_b: __m256,
}

#[target_feature(enable = "avx2", enable = "fma")]
unsafe fn compose_gain8_avx2(
    inputs: Avx2Gain8,
    transform: ComposeRowTransform<'_>,
) -> (__m256, __m256, __m256, __m256) {
    let Avx2Gain8 {
        linear_r,
        linear_g,
        linear_b,
        alpha,
        gain_r,
        gain_g,
        gain_b,
    } = inputs;
    let one = _mm256_set1_ps(1.0);
    let span = _mm256_set1_ps(transform.headroom_span);
    let w = _mm256_set1_ps(transform.weight);
    let zero = _mm256_setzero_ps();
    let scale_r = _mm256_fmadd_ps(_mm256_mul_ps(gain_r, w), span, one);
    let scale_g = _mm256_fmadd_ps(_mm256_mul_ps(gain_g, w), span, one);
    let scale_b = _mm256_fmadd_ps(_mm256_mul_ps(gain_b, w), span, one);
    let out_r = _mm256_max_ps(_mm256_mul_ps(linear_r, scale_r), zero);
    let out_g = _mm256_max_ps(_mm256_mul_ps(linear_g, scale_g), zero);
    let out_b = _mm256_max_ps(_mm256_mul_ps(linear_b, scale_b), zero);
    (out_r, out_g, out_b, alpha)
}

#[target_feature(enable = "avx2", enable = "fma")]
unsafe fn apply_transfer8_avx2(
    r: __m256,
    g: __m256,
    b: __m256,
    path: ComposeFastPath,
) -> (__m256, __m256, __m256) {
    unsafe {
        match path {
            ComposeFastPath::SrgbLinearSrgb | ComposeFastPath::SrgbDisplayP3 => (
                srgb_to_linear8_avx2(r),
                srgb_to_linear8_avx2(g),
                srgb_to_linear8_avx2(b),
            ),
            ComposeFastPath::Bt709LinearSrgb | ComposeFastPath::Bt709DisplayP3 => (
                bt709_to_linear8_avx2(r),
                bt709_to_linear8_avx2(g),
                bt709_to_linear8_avx2(b),
            ),
            ComposeFastPath::LinearLinearSrgb | ComposeFastPath::LinearDisplayP3 => (r, g, b),
            ComposeFastPath::Scalar => unreachable!("scalar path uses scalar row loop"),
        }
    }
}

#[target_feature(enable = "avx2,sse4.1")]
pub(crate) unsafe fn compose_row_avx2(
    row_in: &[f32],
    row_out: &mut [f32],
    width: u32,
    gain_rgb: &[f32],
    transform: ComposeRowTransform<'_>,
) {
    unsafe {
        let in_ptr = row_in.as_ptr();
        let out_ptr = row_out.as_mut_ptr();
        let gain_ptr = gain_rgb.as_ptr();
        let mut x = 0_u32;
        while x + SIMD_PIXELS_PER_AVX2_STEP <= width {
            let offset = x as usize * 4;
            let (r, g, b, a) = load_rgba_pixel8_avx2(in_ptr, offset);
            let (mut lr, mut lg, mut lb) = apply_transfer8_avx2(r, g, b, transform.path);
            if path_applies_display_p3_matrix(transform.path) {
                (lr, lg, lb) = apply_display_p3_matrix8_avx2(lr, lg, lb);
            }
            let (gain_r, gain_g, gain_b) = gather_gain_rgb8_avx2(gain_ptr, x as usize);
            let (out_r, out_g, out_b, out_a) = compose_gain8_avx2(
                Avx2Gain8 {
                    linear_r: lr,
                    linear_g: lg,
                    linear_b: lb,
                    alpha: a,
                    gain_r,
                    gain_g,
                    gain_b,
                },
                transform,
            );
            store_rgba_pixel8_avx2(out_ptr, offset, out_r, out_g, out_b, out_a);
            x += SIMD_PIXELS_PER_AVX2_STEP;
        }
        while x < width {
            compose_pixel_scalar(row_in, row_out, x, gain_rgb, transform);
            x += 1;
        }
    }
}
