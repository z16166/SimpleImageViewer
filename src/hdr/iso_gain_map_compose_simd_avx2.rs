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

//! AVX2 8-pixel ISO gain-map compose helpers (nested from `iso_gain_map_compose_simd`).

use super::{
    IsoComposeConstants, SRGB_DIVISOR, SRGB_GAMMA, SRGB_LINEAR_SEGMENT_END, SRGB_OFFSET, SRGB_SCALE,
};
use crate::hdr::simd_fast_pow::{exp2_8_avx2, pow8_avx2};
use core::arch::x86_64::*;

pub(super) const SIMD_PIXELS_PER_AVX2_STEP: u32 = 8;

#[target_feature(enable = "avx2", enable = "fma")]
pub(super) unsafe fn compose_iso_row_avx2(
    sdr_row: &[u8],
    row_out: &mut [f32],
    gain_row: &[f32],
    width: u32,
    constants: IsoComposeConstants,
    x: &mut u32,
) {
    unsafe {
        while *x + SIMD_PIXELS_PER_AVX2_STEP <= width {
            let base = *x as usize * 4;
            let xi = *x as usize;
            let (enc_r, enc_g, enc_b) = load_sdr_rgb_encoded8_avx2(sdr_row.as_ptr().add(base));
            let (gain_r, gain_g, gain_b) =
                load_gain_rgb8_avx2(gain_row.as_ptr(), xi, width as usize);
            let (out_r, out_g, out_b) =
                recover_hdr_rgb8_avx2(enc_r, enc_g, enc_b, gain_r, gain_g, gain_b, constants);
            store_rgba8_avx2(
                row_out.as_mut_ptr().add(base),
                sdr_row.as_ptr().add(base),
                out_r,
                out_g,
                out_b,
            );
            *x += SIMD_PIXELS_PER_AVX2_STEP;
        }
    }
}

#[target_feature(enable = "avx2", enable = "fma")]
#[inline]
unsafe fn u8x4_lo_to_f32(packed: __m128i) -> __m128 {
    _mm_cvtepi32_ps(_mm_cvtepu8_epi32(packed))
}

#[target_feature(enable = "avx2", enable = "fma")]
#[inline]
unsafe fn u8x8_lanes_to_f32_avx2(bytes: __m256i) -> __m256 {
    unsafe {
        _mm256_set_m128(
            u8x4_lo_to_f32(_mm256_extracti128_si256(bytes, 1)),
            u8x4_lo_to_f32(_mm256_castsi256_si128(bytes)),
        )
    }
}

#[target_feature(enable = "avx2", enable = "fma")]
unsafe fn load_sdr_rgb_encoded8_avx2(ptr: *const u8) -> (__m256, __m256, __m256) {
    unsafe {
        // Interleaved RGBA8 x8 -> planar R/G/B f32 via per-lane pshufb + widen.
        let rgba = _mm256_loadu_si256(ptr as *const __m256i);
        let scale = _mm256_set1_ps(1.0 / 255.0);
        let shuf_r = _mm256_broadcastsi128_si256(_mm_setr_epi8(
            0, 4, 8, 12, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1,
        ));
        let shuf_g = _mm256_broadcastsi128_si256(_mm_setr_epi8(
            1, 5, 9, 13, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1,
        ));
        let shuf_b = _mm256_broadcastsi128_si256(_mm_setr_epi8(
            2, 6, 10, 14, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1,
        ));
        (
            _mm256_mul_ps(
                u8x8_lanes_to_f32_avx2(_mm256_shuffle_epi8(rgba, shuf_r)),
                scale,
            ),
            _mm256_mul_ps(
                u8x8_lanes_to_f32_avx2(_mm256_shuffle_epi8(rgba, shuf_g)),
                scale,
            ),
            _mm256_mul_ps(
                u8x8_lanes_to_f32_avx2(_mm256_shuffle_epi8(rgba, shuf_b)),
                scale,
            ),
        )
    }
}

#[target_feature(enable = "avx2", enable = "fma")]
unsafe fn load_gain_rgb8_avx2(
    gain_row: *const f32,
    x: usize,
    width: usize,
) -> (__m256, __m256, __m256) {
    unsafe {
        // Planar: [R0..Rn | G0..Gn | B0..Bn]
        (
            _mm256_loadu_ps(gain_row.add(x)),
            _mm256_loadu_ps(gain_row.add(width + x)),
            _mm256_loadu_ps(gain_row.add(2 * width + x)),
        )
    }
}

#[target_feature(enable = "avx2", enable = "fma")]
unsafe fn srgb_encoded_to_linear8_avx2(v: __m256) -> __m256 {
    unsafe {
        let threshold = _mm256_set1_ps(SRGB_LINEAR_SEGMENT_END);
        let low_mask = _mm256_cmp_ps(v, threshold, _CMP_LE_OQ);
        let low = _mm256_div_ps(v, _mm256_set1_ps(SRGB_DIVISOR));
        let adjusted = _mm256_div_ps(
            _mm256_add_ps(v, _mm256_set1_ps(SRGB_OFFSET)),
            _mm256_set1_ps(SRGB_SCALE),
        );
        let high = pow8_avx2(adjusted, SRGB_GAMMA);
        _mm256_blendv_ps(high, low, low_mask)
    }
}

#[target_feature(enable = "avx2", enable = "fma")]
unsafe fn recover_hdr_rgb8_avx2(
    enc_r: __m256,
    enc_g: __m256,
    enc_b: __m256,
    gain_r: __m256,
    gain_g: __m256,
    gain_b: __m256,
    constants: IsoComposeConstants,
) -> (__m256, __m256, __m256) {
    unsafe {
        let weight = _mm256_set1_ps(constants.gain_weight);
        let zero = _mm256_setzero_ps();
        let lr = srgb_encoded_to_linear8_avx2(enc_r);
        let lg = srgb_encoded_to_linear8_avx2(enc_g);
        let lb = srgb_encoded_to_linear8_avx2(enc_b);

        let inv_gamma_r = constants.inv_gamma[0];
        let inv_gamma_g = constants.inv_gamma[1];
        let inv_gamma_b = constants.inv_gamma[2];
        let gain_min_r = _mm256_set1_ps(constants.metadata.gain_map_min[0]);
        let gain_min_g = _mm256_set1_ps(constants.metadata.gain_map_min[1]);
        let gain_min_b = _mm256_set1_ps(constants.metadata.gain_map_min[2]);
        let gain_span_r = _mm256_set1_ps(constants.gain_span[0]);
        let gain_span_g = _mm256_set1_ps(constants.gain_span[1]);
        let gain_span_b = _mm256_set1_ps(constants.gain_span[2]);
        let offset_sdr_r = _mm256_set1_ps(constants.metadata.offset_sdr[0]);
        let offset_sdr_g = _mm256_set1_ps(constants.metadata.offset_sdr[1]);
        let offset_sdr_b = _mm256_set1_ps(constants.metadata.offset_sdr[2]);
        let offset_hdr_r = _mm256_set1_ps(constants.metadata.offset_hdr[0]);
        let offset_hdr_g = _mm256_set1_ps(constants.metadata.offset_hdr[1]);
        let offset_hdr_b = _mm256_set1_ps(constants.metadata.offset_hdr[2]);

        let shaped_r = pow8_avx2(gain_r, inv_gamma_r);
        let scaled_r = _mm256_mul_ps(gain_span_r, shaped_r);
        let boost_r = exp2_8_avx2(_mm256_fmadd_ps(scaled_r, weight, gain_min_r));
        let out_r = _mm256_max_ps(
            _mm256_fmsub_ps(_mm256_add_ps(lr, offset_sdr_r), boost_r, offset_hdr_r),
            zero,
        );

        let shaped_g = pow8_avx2(gain_g, inv_gamma_g);
        let scaled_g = _mm256_mul_ps(gain_span_g, shaped_g);
        let boost_g = exp2_8_avx2(_mm256_fmadd_ps(scaled_g, weight, gain_min_g));
        let out_g = _mm256_max_ps(
            _mm256_fmsub_ps(_mm256_add_ps(lg, offset_sdr_g), boost_g, offset_hdr_g),
            zero,
        );

        let shaped_b = pow8_avx2(gain_b, inv_gamma_b);
        let scaled_b = _mm256_mul_ps(gain_span_b, shaped_b);
        let boost_b = exp2_8_avx2(_mm256_fmadd_ps(scaled_b, weight, gain_min_b));
        let out_b = _mm256_max_ps(
            _mm256_fmsub_ps(_mm256_add_ps(lb, offset_sdr_b), boost_b, offset_hdr_b),
            zero,
        );

        (out_r, out_g, out_b)
    }
}

#[target_feature(enable = "avx2", enable = "fma")]
unsafe fn store_rgba8_avx2(dst: *mut f32, sdr: *const u8, r: __m256, g: __m256, b: __m256) {
    unsafe {
        // Planar R/G/B + SDR alpha -> interleaved RGBA f32 for 8 pixels.
        let scale = _mm256_set1_ps(1.0 / 255.0);
        let rgba = _mm256_loadu_si256(sdr as *const __m256i);
        let shuf_a = _mm256_broadcastsi128_si256(_mm_setr_epi8(
            3, 7, 11, 15, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1,
        ));
        let a = _mm256_mul_ps(
            u8x8_lanes_to_f32_avx2(_mm256_shuffle_epi8(rgba, shuf_a)),
            scale,
        );

        let r_lo = _mm256_castps256_ps128(r);
        let r_hi = _mm256_extractf128_ps(r, 1);
        let g_lo = _mm256_castps256_ps128(g);
        let g_hi = _mm256_extractf128_ps(g, 1);
        let b_lo = _mm256_castps256_ps128(b);
        let b_hi = _mm256_extractf128_ps(b, 1);
        let a_lo = _mm256_castps256_ps128(a);
        let a_hi = _mm256_extractf128_ps(a, 1);

        // Pixels 0..3
        let rg_lo = _mm_unpacklo_ps(r_lo, g_lo);
        let rg_hi = _mm_unpackhi_ps(r_lo, g_lo);
        let ba_lo = _mm_unpacklo_ps(b_lo, a_lo);
        let ba_hi = _mm_unpackhi_ps(b_lo, a_lo);
        let p0 = _mm_movelh_ps(rg_lo, ba_lo);
        let p1 = _mm_movehl_ps(ba_lo, rg_lo);
        let p2 = _mm_movelh_ps(rg_hi, ba_hi);
        let p3 = _mm_movehl_ps(ba_hi, rg_hi);

        // Pixels 4..7
        let rg2_lo = _mm_unpacklo_ps(r_hi, g_hi);
        let rg2_hi = _mm_unpackhi_ps(r_hi, g_hi);
        let ba2_lo = _mm_unpacklo_ps(b_hi, a_hi);
        let ba2_hi = _mm_unpackhi_ps(b_hi, a_hi);
        let p4 = _mm_movelh_ps(rg2_lo, ba2_lo);
        let p5 = _mm_movehl_ps(ba2_lo, rg2_lo);
        let p6 = _mm_movelh_ps(rg2_hi, ba2_hi);
        let p7 = _mm_movehl_ps(ba2_hi, rg2_hi);

        _mm_storeu_ps(dst, p0);
        _mm_storeu_ps(dst.add(4), p1);
        _mm_storeu_ps(dst.add(8), p2);
        _mm_storeu_ps(dst.add(12), p3);
        _mm_storeu_ps(dst.add(16), p4);
        _mm_storeu_ps(dst.add(20), p5);
        _mm_storeu_ps(dst.add(24), p6);
        _mm_storeu_ps(dst.add(28), p7);
    }
}
