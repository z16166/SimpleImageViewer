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

//! AVX2 8-pixel strip tone-map kernels (x86_64).
//!
//! Bit-identical to the SSE4.1 4-pixel path after packing to RGBA8.

use super::super::constants::{INVERSE_DISPLAY_GAMMA, MAX_HDR_TONE_MAP_INPUT};
use super::{
    ACES2065_1_TO_LINEAR_SRGB, BT709_DIVISOR, BT709_GAMMA, BT709_LINEAR_SEGMENT_BREAK,
    BT709_OFFSET, BT709_SCALE, DISPLAY_P3_TO_LINEAR_SRGB, HLG_A, HLG_B, HLG_C,
    REC2020_TO_LINEAR_SRGB, SRGB_ENCODE_GAMMA, SRGB_ENCODE_LINEAR_BREAK, SRGB_ENCODE_SCALE,
    SRGB_ENCODE_SLOPE, SRGB_OFFSET, StripSimdPath, StripToneMapContext, XYZ_TO_LINEAR_SRGB,
};
use crate::hdr::simd_fast_pow::{exp8_avx2, pow8_avx2};
use core::arch::x86_64::*;

pub(super) const PIXELS_PER_AVX2_STEP: usize = 8;

#[target_feature(enable = "avx2", enable = "fma")]
pub(super) unsafe fn tone_map_strip_simd_avx2(
    src: &[f32],
    dst: &mut [u8],
    pixel_count: usize,
    ctx: StripToneMapContext,
    offset: &mut usize,
) {
    unsafe {
        while *offset + PIXELS_PER_AVX2_STEP <= pixel_count {
            let base = *offset * 4;
            let (r, g, b, a) = load_rgba_pixel8_avx2(src.as_ptr(), *offset);
            let (lr, lg, lb) = decode_transfer8_avx2(r, g, b, ctx);
            let (sr, sg, sb) = apply_color_matrix8_avx2(lr, lg, lb, ctx.path);
            store_rgba_u8_pixel8_avx2(dst.as_mut_ptr().add(base), sr, sg, sb, a, ctx);
            *offset += PIXELS_PER_AVX2_STEP;
        }
    }
}

#[target_feature(enable = "avx2", enable = "fma")]
unsafe fn load_rgba_pixel8_avx2(
    row: *const f32,
    pixel_offset: usize,
) -> (__m256, __m256, __m256, __m256) {
    unsafe {
        // Two groups of 4 interleaved RGBA -> planar via SSE transpose, then combine.
        let mut p0 = _mm_loadu_ps(row.add(pixel_offset * 4));
        let mut p1 = _mm_loadu_ps(row.add(pixel_offset * 4 + 4));
        let mut p2 = _mm_loadu_ps(row.add(pixel_offset * 4 + 8));
        let mut p3 = _mm_loadu_ps(row.add(pixel_offset * 4 + 12));
        _MM_TRANSPOSE4_PS(&mut p0, &mut p1, &mut p2, &mut p3);

        let mut q0 = _mm_loadu_ps(row.add(pixel_offset * 4 + 16));
        let mut q1 = _mm_loadu_ps(row.add(pixel_offset * 4 + 20));
        let mut q2 = _mm_loadu_ps(row.add(pixel_offset * 4 + 24));
        let mut q3 = _mm_loadu_ps(row.add(pixel_offset * 4 + 28));
        _MM_TRANSPOSE4_PS(&mut q0, &mut q1, &mut q2, &mut q3);

        let r = _mm256_insertf128_ps(_mm256_castps128_ps256(p0), q0, 1);
        let g = _mm256_insertf128_ps(_mm256_castps128_ps256(p1), q1, 1);
        let b = _mm256_insertf128_ps(_mm256_castps128_ps256(p2), q2, 1);
        let a = _mm256_insertf128_ps(_mm256_castps128_ps256(p3), q3, 1);
        (r, g, b, a)
    }
}

#[target_feature(enable = "avx2", enable = "fma")]
unsafe fn decode_transfer8_avx2(
    r: __m256,
    g: __m256,
    b: __m256,
    ctx: StripToneMapContext,
) -> (__m256, __m256, __m256) {
    unsafe {
        match ctx.path {
            StripSimdPath::ReinhardPqRec2020
            | StripSimdPath::ReinhardPqDisplayP3
            | StripSimdPath::ReinhardPqLinearSrgb
            | StripSimdPath::ReinhardPqAces
            | StripSimdPath::ReinhardPqXyz => (
                pq_to_display_linear8_avx2(r, ctx.sdr_white_nits),
                pq_to_display_linear8_avx2(g, ctx.sdr_white_nits),
                pq_to_display_linear8_avx2(b, ctx.sdr_white_nits),
            ),
            StripSimdPath::ReinhardHlgRec2020
            | StripSimdPath::ReinhardHlgDisplayP3
            | StripSimdPath::ReinhardHlgLinearSrgb
            | StripSimdPath::ReinhardHlgAces
            | StripSimdPath::ReinhardHlgXyz => (
                hlg_to_scene_linear8_avx2(r),
                hlg_to_scene_linear8_avx2(g),
                hlg_to_scene_linear8_avx2(b),
            ),
            StripSimdPath::ReinhardBt709Rec2020
            | StripSimdPath::ReinhardBt709DisplayP3
            | StripSimdPath::ReinhardBt709LinearSrgb
            | StripSimdPath::ReinhardBt709Aces
            | StripSimdPath::ReinhardBt709Xyz => (
                bt709_to_linear8_avx2(r),
                bt709_to_linear8_avx2(g),
                bt709_to_linear8_avx2(b),
            ),
            StripSimdPath::ReinhardLinearRec2020
            | StripSimdPath::ReinhardLinearDisplayP3
            | StripSimdPath::ReinhardLinearLinearSrgb
            | StripSimdPath::ReinhardLinearAces
            | StripSimdPath::ReinhardLinearXyz
            | StripSimdPath::Iec61966SrgbLinearSrgb => (r, g, b),
            StripSimdPath::Scalar => (r, g, b),
        }
    }
}

#[target_feature(enable = "avx2", enable = "fma")]
unsafe fn apply_color_matrix8_avx2(
    r: __m256,
    g: __m256,
    b: __m256,
    path: StripSimdPath,
) -> (__m256, __m256, __m256) {
    unsafe {
        match path {
            StripSimdPath::ReinhardPqRec2020
            | StripSimdPath::ReinhardHlgRec2020
            | StripSimdPath::ReinhardBt709Rec2020
            | StripSimdPath::ReinhardLinearRec2020 => {
                apply_matrix8_avx2(r, g, b, &REC2020_TO_LINEAR_SRGB)
            }
            StripSimdPath::ReinhardPqDisplayP3
            | StripSimdPath::ReinhardHlgDisplayP3
            | StripSimdPath::ReinhardBt709DisplayP3
            | StripSimdPath::ReinhardLinearDisplayP3 => {
                apply_matrix8_avx2(r, g, b, &DISPLAY_P3_TO_LINEAR_SRGB)
            }
            StripSimdPath::ReinhardPqAces
            | StripSimdPath::ReinhardHlgAces
            | StripSimdPath::ReinhardBt709Aces
            | StripSimdPath::ReinhardLinearAces => {
                apply_matrix8_avx2(r, g, b, &ACES2065_1_TO_LINEAR_SRGB)
            }
            StripSimdPath::ReinhardPqXyz
            | StripSimdPath::ReinhardHlgXyz
            | StripSimdPath::ReinhardBt709Xyz
            | StripSimdPath::ReinhardLinearXyz => apply_matrix8_avx2(r, g, b, &XYZ_TO_LINEAR_SRGB),
            _ => (r, g, b),
        }
    }
}

#[target_feature(enable = "avx2", enable = "fma")]
unsafe fn apply_matrix8_avx2(
    r: __m256,
    g: __m256,
    b: __m256,
    m: &[[f32; 3]; 3],
) -> (__m256, __m256, __m256) {
    let sr = _mm256_fmadd_ps(
        b, _mm256_set1_ps(m[0][2]),
        _mm256_fmadd_ps(
            g, _mm256_set1_ps(m[0][1]),
            _mm256_mul_ps(r, _mm256_set1_ps(m[0][0])),
        ),
    );
    let sg = _mm256_fmadd_ps(
        b, _mm256_set1_ps(m[1][2]),
        _mm256_fmadd_ps(
            g, _mm256_set1_ps(m[1][1]),
            _mm256_mul_ps(r, _mm256_set1_ps(m[1][0])),
        ),
    );
    let sb = _mm256_fmadd_ps(
        b, _mm256_set1_ps(m[2][2]),
        _mm256_fmadd_ps(
            g, _mm256_set1_ps(m[2][1]),
            _mm256_mul_ps(r, _mm256_set1_ps(m[2][0])),
        ),
    );
    (sr, sg, sb)
}

#[target_feature(enable = "avx2", enable = "fma")]
unsafe fn store_rgba_u8_pixel8_avx2(
    dst: *mut u8,
    r: __m256,
    g: __m256,
    b: __m256,
    a: __m256,
    ctx: StripToneMapContext,
) {
    unsafe {
        let (er, eg, eb) = if ctx.path == StripSimdPath::Iec61966SrgbLinearSrgb {
            (
                encode_iec61966_channel8_avx2(r, ctx.combined_scale),
                encode_iec61966_channel8_avx2(g, ctx.combined_scale),
                encode_iec61966_channel8_avx2(b, ctx.combined_scale),
            )
        } else {
            (
                encode_reinhard_channel8_avx2(r, ctx.combined_scale),
                encode_reinhard_channel8_avx2(g, ctx.combined_scale),
                encode_reinhard_channel8_avx2(b, ctx.combined_scale),
            )
        };
        pack_rgba_u8_pixel8_avx2(dst, er, eg, eb, a);
    }
}

#[target_feature(enable = "avx2", enable = "fma")]
unsafe fn sanitize_hdr_rgb8_avx2(v: __m256) -> __m256 {
    let zero = _mm256_setzero_ps();
    let max_input = _mm256_set1_ps(MAX_HDR_TONE_MAP_INPUT);
    let sanitized = _mm256_max_ps(v, zero);
    _mm256_min_ps(sanitized, max_input)
}

#[target_feature(enable = "avx2", enable = "fma")]
unsafe fn pq_to_display_linear8_avx2(code: __m256, sdr_white_nits: f32) -> __m256 {
    unsafe {
        let m2 = crate::constants::PQ_M2;
        let m1 = crate::constants::PQ_M1;
        let c1 = _mm256_set1_ps(crate::constants::PQ_C1);
        let c2 = _mm256_set1_ps(crate::constants::PQ_C2);
        let c3 = _mm256_set1_ps(crate::constants::PQ_C3);
        let zero = _mm256_setzero_ps();
        let one = _mm256_set1_ps(1.0);
        let clamped = _mm256_min_ps(_mm256_max_ps(code, zero), one);
        let code_m2 = pow8_avx2(clamped, 1.0 / m2);
        let numerator = _mm256_max_ps(_mm256_sub_ps(code_m2, c1), zero);
        let denominator = _mm256_max_ps(
            _mm256_fnmadd_ps(c3, code_m2, c2),
            _mm256_set1_ps(0.000001),
        );
        let ratio = _mm256_div_ps(numerator, denominator);
        let nits = _mm256_mul_ps(_mm256_set1_ps(10_000.0), pow8_avx2(ratio, 1.0 / m1));
        _mm256_div_ps(nits, _mm256_set1_ps(sdr_white_nits.max(1.0)))
    }
}

#[target_feature(enable = "avx2", enable = "fma")]
unsafe fn hlg_to_scene_linear8_avx2(e_prime: __m256) -> __m256 {
    unsafe {
        let zero = _mm256_setzero_ps();
        let one = _mm256_set1_ps(1.0);
        let clamped = _mm256_min_ps(_mm256_max_ps(e_prime, zero), one);
        let half = _mm256_set1_ps(0.5);
        let low_mask = _mm256_cmp_ps(clamped, half, _CMP_LE_OQ);
        let low = _mm256_div_ps(_mm256_mul_ps(clamped, clamped), _mm256_set1_ps(3.0));
        let adjusted = _mm256_div_ps(
            _mm256_max_ps(_mm256_sub_ps(clamped, _mm256_set1_ps(HLG_C)), zero),
            _mm256_set1_ps(HLG_A),
        );
        let high = _mm256_div_ps(
            _mm256_add_ps(exp8_avx2(adjusted), _mm256_set1_ps(HLG_B)),
            _mm256_set1_ps(12.0),
        );
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
unsafe fn encode_reinhard_channel8_avx2(linear: __m256, scale: f32) -> __m256 {
    unsafe {
        let exposed = sanitize_hdr_rgb8_avx2(_mm256_mul_ps(linear, _mm256_set1_ps(scale)));
        let mapped = _mm256_div_ps(exposed, _mm256_add_ps(_mm256_set1_ps(1.0), exposed));
        let encoded = pow8_avx2(mapped, INVERSE_DISPLAY_GAMMA);
        _mm256_min_ps(
            _mm256_max_ps(encoded, _mm256_setzero_ps()),
            _mm256_set1_ps(1.0),
        )
    }
}

#[target_feature(enable = "avx2", enable = "fma")]
unsafe fn encode_iec61966_channel8_avx2(linear: __m256, scale: f32) -> __m256 {
    unsafe {
        let scaled = _mm256_min_ps(
            _mm256_max_ps(
                _mm256_mul_ps(linear, _mm256_set1_ps(scale)),
                _mm256_setzero_ps(),
            ),
            _mm256_set1_ps(1.0),
        );
        let threshold = _mm256_set1_ps(SRGB_ENCODE_LINEAR_BREAK);
        let low_mask = _mm256_cmp_ps(scaled, threshold, _CMP_LE_OQ);
        let low = _mm256_mul_ps(scaled, _mm256_set1_ps(SRGB_ENCODE_SLOPE));
        let adjusted = _mm256_sub_ps(
            _mm256_mul_ps(
                _mm256_set1_ps(SRGB_ENCODE_SCALE),
                pow8_avx2(scaled, SRGB_ENCODE_GAMMA),
            ),
            _mm256_set1_ps(SRGB_OFFSET),
        );
        _mm256_min_ps(
            _mm256_max_ps(
                _mm256_blendv_ps(adjusted, low, low_mask),
                _mm256_setzero_ps(),
            ),
            _mm256_set1_ps(1.0),
        )
    }
}

#[target_feature(enable = "avx2", enable = "fma")]
#[inline]
unsafe fn pack_i32x8_to_u8x8_avx2(v: __m256i) -> __m128i {
    // packus is lane-wise; permute gathers [lo4, hi4] into the low 128 bits.
    let v16 = _mm256_packus_epi32(v, _mm256_setzero_si256());
    let v16 = _mm256_permute4x64_epi64(v16, 0xD8);
    _mm_packus_epi16(_mm256_castsi256_si128(v16), _mm_setzero_si128())
}

#[target_feature(enable = "avx2", enable = "fma")]
unsafe fn float8_to_u8x8_avx2(v: __m256) -> __m128i {
    let zero = _mm256_setzero_ps();
    let one = _mm256_set1_ps(1.0);
    let half = _mm256_set1_ps(0.5);
    let clamped = _mm256_min_ps(_mm256_max_ps(v, zero), one);
    let scaled = _mm256_mul_ps(clamped, _mm256_set1_ps(255.0));
    // Match SSE / scalar: trunc(v * 255 + 0.5) for non-negative v.
    let rounded = _mm256_cvttps_epi32(_mm256_add_ps(scaled, half));
    unsafe { pack_i32x8_to_u8x8_avx2(rounded) }
}

#[target_feature(enable = "avx2", enable = "fma")]
unsafe fn pack_rgba_u8_pixel8_avx2(dst: *mut u8, r: __m256, g: __m256, b: __m256, a: __m256) {
    unsafe {
        let r8 = float8_to_u8x8_avx2(r);
        let g8 = float8_to_u8x8_avx2(g);
        let b8 = float8_to_u8x8_avx2(b);
        let a8 = float8_to_u8x8_avx2(a);

        let rg = _mm_unpacklo_epi8(r8, g8);
        let ba = _mm_unpacklo_epi8(b8, a8);
        let rgba_lo = _mm_unpacklo_epi16(rg, ba);
        let rgba_hi = _mm_unpackhi_epi16(rg, ba);
        let out = _mm256_set_m128i(rgba_hi, rgba_lo);
        _mm256_storeu_si256(dst as *mut __m256i, out);
    }
}
