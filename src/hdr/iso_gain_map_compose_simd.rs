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

//! SIMD/NEON ISO gain-map CPU compose fallback (Ultra HDR / AVIF JPEG-R deferred planes).
//!
//! Rows compose in parallel via rayon; each row upsamples the gain map once with
//! [`precompute_gain_map_row_encoded`]. Per-pixel ISO recovery uses [`pow4_sse41`] /
//! [`pow4_neon`] for gain shaping and [`exp2_4_sse41`] / [`exp2_4_neon`] for per-lane
//! `2^log_boost`, matching the scalar reference within the same tolerance band as
//! [`crate::hdr::heif_apple_gain_map_compose_simd`].

use std::cell::RefCell;

use rayon::prelude::*;

use crate::hdr::gain_map::{
    GainMapMetadata, compose_gain_map_pixel, gain_map_weight, precompute_gain_map_row_encoded,
    validate_iso_deferred_planes,
};
#[cfg(target_arch = "aarch64")]
use crate::hdr::simd_fast_pow::{exp2_4_neon, pow4_neon};
#[cfg(target_arch = "x86_64")]
use crate::hdr::simd_fast_pow::{exp2_4_sse41, pow4_sse41};
use crate::hdr::types::IsoGainMapGpuSource;

#[cfg(target_arch = "aarch64")]
use core::arch::aarch64::*;
#[cfg(target_arch = "x86_64")]
use core::arch::x86_64::*;

const SIMD_PIXELS_PER_STEP: u32 = 4;

const SRGB_LINEAR_SEGMENT_END: f32 = 0.04045;
const SRGB_DIVISOR: f32 = 12.92;
const SRGB_OFFSET: f32 = 0.055;
const SRGB_SCALE: f32 = 1.055;
const SRGB_GAMMA: f32 = 2.4;

thread_local! {
    static GAIN_ROW_SCRATCH: RefCell<Vec<f32>> = const { RefCell::new(Vec::new()) };
}

#[derive(Clone, Copy)]
struct IsoComposeConstants {
    metadata: GainMapMetadata,
    target_hdr_capacity: f32,
    gain_weight: f32,
    inv_gamma: [f32; 3],
    gain_span: [f32; 3],
}

impl IsoComposeConstants {
    fn new(metadata: GainMapMetadata, target_hdr_capacity: f32) -> Self {
        let gain_weight = gain_map_weight(metadata, target_hdr_capacity);
        let mut inv_gamma = [0.0_f32; 3];
        let mut gain_span = [0.0_f32; 3];
        for channel in 0..3 {
            inv_gamma[channel] = 1.0 / metadata.gamma[channel].max(f32::MIN_POSITIVE);
            gain_span[channel] = metadata.gain_map_max[channel] - metadata.gain_map_min[channel];
        }
        Self {
            metadata,
            target_hdr_capacity,
            gain_weight,
            inv_gamma,
            gain_span,
        }
    }
}

pub(crate) fn compose_iso_deferred_cpu_pixels_simd(
    width: u32,
    height: u32,
    deferred: &IsoGainMapGpuSource,
    target_hdr_capacity: f32,
) -> Result<Vec<f32>, String> {
    validate_iso_deferred_planes(
        width,
        height,
        deferred.sdr_rgba.as_slice(),
        deferred.gain_width,
        deferred.gain_height,
        deferred.gain_rgba.as_slice(),
    )?;

    let constants = IsoComposeConstants::new(deferred.metadata, target_hdr_capacity);
    let mut rgba_f32 = vec![0.0_f32; width as usize * height as usize * 4];
    let row_stride = width as usize * 4;
    let sdr = deferred.sdr_rgba.as_slice();
    let gain = deferred.gain_rgba.as_slice();

    rgba_f32
        .par_chunks_mut(row_stride)
        .enumerate()
        .for_each(|(y, row_out)| {
            GAIN_ROW_SCRATCH.with(|scratch| {
                let mut gain_row = scratch.borrow_mut();
                let needed = width as usize * 3;
                if gain_row.capacity() < needed {
                    let extra = needed - gain_row.len();
                    gain_row.reserve(extra);
                }
                // SAFETY: capacity >= needed; contents are fully overwritten below.
                unsafe {
                    gain_row.set_len(needed);
                }
                precompute_gain_map_row_encoded(
                    gain,
                    deferred.gain_width,
                    deferred.gain_height,
                    y as u32,
                    width,
                    height,
                    &mut gain_row,
                );
                compose_iso_row(
                    &sdr[y * row_stride..(y + 1) * row_stride],
                    row_out,
                    &gain_row,
                    constants,
                );
            });
        });

    Ok(rgba_f32)
}

fn compose_iso_row(
    sdr_row: &[u8],
    row_out: &mut [f32],
    gain_row: &[f32],
    constants: IsoComposeConstants,
) {
    let width = sdr_row.len() / 4;
    let mut x = {
        #[cfg(target_arch = "x86_64")]
        {
            if std::arch::is_x86_feature_detected!("sse4.1") {
                let mut simd_x = 0_u32;
                unsafe {
                    compose_iso_row_sse41(
                        sdr_row,
                        row_out,
                        gain_row,
                        width as u32,
                        constants,
                        &mut simd_x,
                    );
                }
                simd_x as usize
            } else {
                0_usize
            }
        }
        #[cfg(target_arch = "aarch64")]
        {
            let mut simd_x = 0_u32;
            unsafe {
                compose_iso_row_neon(
                    sdr_row,
                    row_out,
                    gain_row,
                    width as u32,
                    constants,
                    &mut simd_x,
                );
            }
            simd_x as usize
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            0_usize
        }
    };
    while x < width {
        compose_iso_pixel_scalar(sdr_row, row_out, gain_row, x as u32, constants);
        x += 1;
    }
}

fn compose_iso_pixel_scalar(
    sdr_row: &[u8],
    row_out: &mut [f32],
    gain_row: &[f32],
    x: u32,
    constants: IsoComposeConstants,
) {
    let sdr_index = x as usize * 4;
    let width = sdr_row.len() / 4;
    let xi = x as usize;
    let pixel = compose_gain_map_pixel(
        [
            sdr_row[sdr_index],
            sdr_row[sdr_index + 1],
            sdr_row[sdr_index + 2],
            sdr_row[sdr_index + 3],
        ],
        [gain_row[xi], gain_row[width + xi], gain_row[2 * width + xi]],
        constants.metadata,
        constants.target_hdr_capacity,
    );
    row_out[sdr_index..sdr_index + 4].copy_from_slice(&pixel);
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn compose_iso_row_sse41(
    sdr_row: &[u8],
    row_out: &mut [f32],
    gain_row: &[f32],
    width: u32,
    constants: IsoComposeConstants,
    x: &mut u32,
) {
    unsafe {
        while *x + SIMD_PIXELS_PER_STEP <= width {
            let base = *x as usize * 4;
            let xi = *x as usize;
            let (enc_r, enc_g, enc_b) = load_sdr_rgb_encoded4_sse41(sdr_row.as_ptr().add(base));
            let (gain_r, gain_g, gain_b) =
                load_gain_rgb4_sse41(gain_row.as_ptr(), xi, width as usize);
            let (out_r, out_g, out_b) =
                recover_hdr_rgb4_sse41(enc_r, enc_g, enc_b, gain_r, gain_g, gain_b, constants);
            store_rgba4_sse41(
                row_out.as_mut_ptr().add(base),
                sdr_row.as_ptr().add(base),
                out_r,
                out_g,
                out_b,
            );
            *x += SIMD_PIXELS_PER_STEP;
        }
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn compose_iso_row_neon(
    sdr_row: &[u8],
    row_out: &mut [f32],
    gain_row: &[f32],
    width: u32,
    constants: IsoComposeConstants,
    x: &mut u32,
) {
    unsafe {
        while *x + SIMD_PIXELS_PER_STEP <= width {
            let base = *x as usize * 4;
            let xi = *x as usize;
            let (enc_r, enc_g, enc_b) = load_sdr_rgb_encoded4_neon(sdr_row.as_ptr().add(base));
            let (gain_r, gain_g, gain_b) =
                load_gain_rgb4_neon(gain_row.as_ptr(), xi, width as usize);
            let (out_r, out_g, out_b) =
                recover_hdr_rgb4_neon(enc_r, enc_g, enc_b, gain_r, gain_g, gain_b, constants);
            store_rgba4_neon(
                row_out.as_mut_ptr().add(base),
                sdr_row.as_ptr().add(base),
                out_r,
                out_g,
                out_b,
            );
            *x += SIMD_PIXELS_PER_STEP;
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
#[inline]
unsafe fn u8x4_lanes_to_f32_sse41(packed: __m128i) -> __m128 {
    _mm_cvtepi32_ps(_mm_cvtepu8_epi32(packed))
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn load_sdr_rgb_encoded4_sse41(ptr: *const u8) -> (__m128, __m128, __m128) {
    unsafe {
        // Interleaved RGBA8 x4 -> planar R/G/B f32 via pshufb + cvtepu8.
        let rgba = _mm_loadu_si128(ptr as *const __m128i);
        let scale = _mm_set1_ps(1.0 / 255.0);
        let r_bytes = _mm_shuffle_epi8(
            rgba,
            _mm_setr_epi8(0, 4, 8, 12, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1),
        );
        let g_bytes = _mm_shuffle_epi8(
            rgba,
            _mm_setr_epi8(1, 5, 9, 13, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1),
        );
        let b_bytes = _mm_shuffle_epi8(
            rgba,
            _mm_setr_epi8(2, 6, 10, 14, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1),
        );
        (
            _mm_mul_ps(u8x4_lanes_to_f32_sse41(r_bytes), scale),
            _mm_mul_ps(u8x4_lanes_to_f32_sse41(g_bytes), scale),
            _mm_mul_ps(u8x4_lanes_to_f32_sse41(b_bytes), scale),
        )
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn load_sdr_rgb_encoded4_neon(ptr: *const u8) -> (float32x4_t, float32x4_t, float32x4_t) {
    unsafe {
        let scale = vdupq_n_f32(1.0 / 255.0);
        let rgbe = vld4_u8(ptr);
        let r = vmulq_f32(
            vcvtq_f32_u32(vmovl_u16(vget_low_u16(vmovl_u8(rgbe.0)))),
            scale,
        );
        let g = vmulq_f32(
            vcvtq_f32_u32(vmovl_u16(vget_low_u16(vmovl_u8(rgbe.1)))),
            scale,
        );
        let b = vmulq_f32(
            vcvtq_f32_u32(vmovl_u16(vget_low_u16(vmovl_u8(rgbe.2)))),
            scale,
        );
        (r, g, b)
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn load_gain_rgb4_sse41(
    gain_row: *const f32,
    x: usize,
    width: usize,
) -> (__m128, __m128, __m128) {
    unsafe {
        // Planar: [R0..Rn | G0..Gn | B0..Bn]
        (
            _mm_loadu_ps(gain_row.add(x)),
            _mm_loadu_ps(gain_row.add(width + x)),
            _mm_loadu_ps(gain_row.add(2 * width + x)),
        )
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn load_gain_rgb4_neon(
    gain_row: *const f32,
    x: usize,
    width: usize,
) -> (float32x4_t, float32x4_t, float32x4_t) {
    unsafe {
        (
            vld1q_f32(gain_row.add(x)),
            vld1q_f32(gain_row.add(width + x)),
            vld1q_f32(gain_row.add(2 * width + x)),
        )
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn srgb_encoded_to_linear4_sse41(v: __m128) -> __m128 {
    unsafe {
        let threshold = _mm_set1_ps(SRGB_LINEAR_SEGMENT_END);
        let low_mask = _mm_cmple_ps(v, threshold);
        let low = _mm_div_ps(v, _mm_set1_ps(SRGB_DIVISOR));
        let adjusted = _mm_div_ps(
            _mm_add_ps(v, _mm_set1_ps(SRGB_OFFSET)),
            _mm_set1_ps(SRGB_SCALE),
        );
        let high = pow4_sse41(adjusted, SRGB_GAMMA);
        _mm_blendv_ps(high, low, low_mask)
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn srgb_encoded_to_linear4_neon(v: float32x4_t) -> float32x4_t {
    unsafe {
        let threshold = vdupq_n_f32(SRGB_LINEAR_SEGMENT_END);
        let low_mask = vcleq_f32(v, threshold);
        let low = vdivq_f32(v, vdupq_n_f32(SRGB_DIVISOR));
        let adjusted = vdivq_f32(
            vaddq_f32(v, vdupq_n_f32(SRGB_OFFSET)),
            vdupq_n_f32(SRGB_SCALE),
        );
        let high = pow4_neon(adjusted, SRGB_GAMMA);
        vbslq_f32(low_mask, low, high)
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn recover_hdr_rgb4_sse41(
    enc_r: __m128,
    enc_g: __m128,
    enc_b: __m128,
    gain_r: __m128,
    gain_g: __m128,
    gain_b: __m128,
    constants: IsoComposeConstants,
) -> (__m128, __m128, __m128) {
    unsafe {
        let weight = _mm_set1_ps(constants.gain_weight);
        let zero = _mm_setzero_ps();
        let lr = srgb_encoded_to_linear4_sse41(enc_r);
        let lg = srgb_encoded_to_linear4_sse41(enc_g);
        let lb = srgb_encoded_to_linear4_sse41(enc_b);

        // Per-channel constants built once (not re-indexed via channel usize).
        let inv_gamma_r = constants.inv_gamma[0];
        let inv_gamma_g = constants.inv_gamma[1];
        let inv_gamma_b = constants.inv_gamma[2];
        let gain_min_r = _mm_set1_ps(constants.metadata.gain_map_min[0]);
        let gain_min_g = _mm_set1_ps(constants.metadata.gain_map_min[1]);
        let gain_min_b = _mm_set1_ps(constants.metadata.gain_map_min[2]);
        let gain_span_r = _mm_set1_ps(constants.gain_span[0]);
        let gain_span_g = _mm_set1_ps(constants.gain_span[1]);
        let gain_span_b = _mm_set1_ps(constants.gain_span[2]);
        let offset_sdr_r = _mm_set1_ps(constants.metadata.offset_sdr[0]);
        let offset_sdr_g = _mm_set1_ps(constants.metadata.offset_sdr[1]);
        let offset_sdr_b = _mm_set1_ps(constants.metadata.offset_sdr[2]);
        let offset_hdr_r = _mm_set1_ps(constants.metadata.offset_hdr[0]);
        let offset_hdr_g = _mm_set1_ps(constants.metadata.offset_hdr[1]);
        let offset_hdr_b = _mm_set1_ps(constants.metadata.offset_hdr[2]);

        let shaped_r = pow4_sse41(gain_r, inv_gamma_r);
        let boost_r = exp2_4_sse41(_mm_add_ps(
            gain_min_r,
            _mm_mul_ps(_mm_mul_ps(gain_span_r, shaped_r), weight),
        ));
        let out_r = _mm_max_ps(
            _mm_sub_ps(
                _mm_mul_ps(_mm_add_ps(lr, offset_sdr_r), boost_r),
                offset_hdr_r,
            ),
            zero,
        );

        let shaped_g = pow4_sse41(gain_g, inv_gamma_g);
        let boost_g = exp2_4_sse41(_mm_add_ps(
            gain_min_g,
            _mm_mul_ps(_mm_mul_ps(gain_span_g, shaped_g), weight),
        ));
        let out_g = _mm_max_ps(
            _mm_sub_ps(
                _mm_mul_ps(_mm_add_ps(lg, offset_sdr_g), boost_g),
                offset_hdr_g,
            ),
            zero,
        );

        let shaped_b = pow4_sse41(gain_b, inv_gamma_b);
        let boost_b = exp2_4_sse41(_mm_add_ps(
            gain_min_b,
            _mm_mul_ps(_mm_mul_ps(gain_span_b, shaped_b), weight),
        ));
        let out_b = _mm_max_ps(
            _mm_sub_ps(
                _mm_mul_ps(_mm_add_ps(lb, offset_sdr_b), boost_b),
                offset_hdr_b,
            ),
            zero,
        );

        (out_r, out_g, out_b)
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn recover_hdr_rgb4_neon(
    enc_r: float32x4_t,
    enc_g: float32x4_t,
    enc_b: float32x4_t,
    gain_r: float32x4_t,
    gain_g: float32x4_t,
    gain_b: float32x4_t,
    constants: IsoComposeConstants,
) -> (float32x4_t, float32x4_t, float32x4_t) {
    unsafe {
        let weight = vdupq_n_f32(constants.gain_weight);
        let zero = vdupq_n_f32(0.0);
        let lr = srgb_encoded_to_linear4_neon(enc_r);
        let lg = srgb_encoded_to_linear4_neon(enc_g);
        let lb = srgb_encoded_to_linear4_neon(enc_b);

        let inv_gamma_r = constants.inv_gamma[0];
        let inv_gamma_g = constants.inv_gamma[1];
        let inv_gamma_b = constants.inv_gamma[2];
        let gain_min_r = vdupq_n_f32(constants.metadata.gain_map_min[0]);
        let gain_min_g = vdupq_n_f32(constants.metadata.gain_map_min[1]);
        let gain_min_b = vdupq_n_f32(constants.metadata.gain_map_min[2]);
        let gain_span_r = vdupq_n_f32(constants.gain_span[0]);
        let gain_span_g = vdupq_n_f32(constants.gain_span[1]);
        let gain_span_b = vdupq_n_f32(constants.gain_span[2]);
        let offset_sdr_r = vdupq_n_f32(constants.metadata.offset_sdr[0]);
        let offset_sdr_g = vdupq_n_f32(constants.metadata.offset_sdr[1]);
        let offset_sdr_b = vdupq_n_f32(constants.metadata.offset_sdr[2]);
        let offset_hdr_r = vdupq_n_f32(constants.metadata.offset_hdr[0]);
        let offset_hdr_g = vdupq_n_f32(constants.metadata.offset_hdr[1]);
        let offset_hdr_b = vdupq_n_f32(constants.metadata.offset_hdr[2]);

        let shaped_r = pow4_neon(gain_r, inv_gamma_r);
        let boost_r = exp2_4_neon(vaddq_f32(
            gain_min_r,
            vmulq_f32(vmulq_f32(gain_span_r, shaped_r), weight),
        ));
        let out_r = vmaxq_f32(
            vsubq_f32(
                vmulq_f32(vaddq_f32(lr, offset_sdr_r), boost_r),
                offset_hdr_r,
            ),
            zero,
        );

        let shaped_g = pow4_neon(gain_g, inv_gamma_g);
        let boost_g = exp2_4_neon(vaddq_f32(
            gain_min_g,
            vmulq_f32(vmulq_f32(gain_span_g, shaped_g), weight),
        ));
        let out_g = vmaxq_f32(
            vsubq_f32(
                vmulq_f32(vaddq_f32(lg, offset_sdr_g), boost_g),
                offset_hdr_g,
            ),
            zero,
        );

        let shaped_b = pow4_neon(gain_b, inv_gamma_b);
        let boost_b = exp2_4_neon(vaddq_f32(
            gain_min_b,
            vmulq_f32(vmulq_f32(gain_span_b, shaped_b), weight),
        ));
        let out_b = vmaxq_f32(
            vsubq_f32(
                vmulq_f32(vaddq_f32(lb, offset_sdr_b), boost_b),
                offset_hdr_b,
            ),
            zero,
        );

        (out_r, out_g, out_b)
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn store_rgba4_sse41(dst: *mut f32, sdr: *const u8, r: __m128, g: __m128, b: __m128) {
    unsafe {
        // Planar R/G/B/A lanes -> interleaved RGBA f32 via transpose (required).
        let scale = _mm_set1_ps(1.0 / 255.0);
        let rgba = _mm_loadu_si128(sdr as *const __m128i);
        let a_bytes = _mm_shuffle_epi8(
            rgba,
            _mm_setr_epi8(3, 7, 11, 15, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1),
        );
        let mut a = _mm_mul_ps(u8x4_lanes_to_f32_sse41(a_bytes), scale);
        let mut r = r;
        let mut g = g;
        let mut b = b;
        _MM_TRANSPOSE4_PS(&mut r, &mut g, &mut b, &mut a);
        _mm_storeu_ps(dst, r);
        _mm_storeu_ps(dst.add(4), g);
        _mm_storeu_ps(dst.add(8), b);
        _mm_storeu_ps(dst.add(12), a);
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn store_rgba4_neon(
    dst: *mut f32,
    sdr: *const u8,
    r: float32x4_t,
    g: float32x4_t,
    b: float32x4_t,
) {
    unsafe {
        let scale = vdupq_n_f32(1.0 / 255.0);
        let rgbe = vld4_u8(sdr);
        let a = vmulq_f32(
            vcvtq_f32_u32(vmovl_u16(vget_low_u16(vmovl_u8(rgbe.3)))),
            scale,
        );
        vst4q_f32(dst, float32x4x4_t(r, g, b, a));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::hdr::gain_map::GainMapMetadata;
    use crate::hdr::jpeg_gain_map_gpu::compose_iso_deferred_cpu_pixels_scalar;

    fn test_metadata() -> GainMapMetadata {
        GainMapMetadata {
            gain_map_min: [0.0; 3],
            gain_map_max: [1.0; 3],
            gamma: [1.0; 3],
            offset_sdr: [0.0; 3],
            offset_hdr: [0.0; 3],
            hdr_capacity_min: 1.0,
            hdr_capacity_max: 4.0,
            backward_direction: false,
        }
    }

    fn assert_iso_compose_near_scalar(scalar: &[f32], simd: &[f32]) {
        assert_eq!(scalar.len(), simd.len());
        for (index, (&got, &expected)) in simd.iter().zip(scalar.iter()).enumerate() {
            let diff = (got - expected).abs();
            let tol = (expected.abs() * 1.0e-5).max(1.0e-5);
            assert!(
                diff <= tol,
                "ISO compose SIMD mismatch at {index}: got={got} expected={expected}"
            );
        }
    }

    #[test]
    fn iso_compose_simd_matches_scalar_reference() {
        let width = 64_u32;
        let height = 48_u32;
        let mut sdr = vec![0_u8; (width * height * 4) as usize];
        let mut gain = vec![0_u8; (8 * 6 * 4) as usize];
        for (index, byte) in sdr.iter_mut().enumerate() {
            *byte = ((index * 17) % 256) as u8;
        }
        for (index, byte) in gain.iter_mut().enumerate() {
            *byte = ((index * 31 + 64) % 256) as u8;
        }
        let deferred = IsoGainMapGpuSource {
            sdr_rgba: Arc::new(sdr),
            gain_rgba: Arc::new(gain),
            gain_width: 8,
            gain_height: 6,
            metadata: test_metadata(),
        };
        let scalar =
            compose_iso_deferred_cpu_pixels_scalar(width, height, &deferred, 2.0).expect("scalar");
        let simd =
            compose_iso_deferred_cpu_pixels_simd(width, height, &deferred, 2.0).expect("simd");
        assert_iso_compose_near_scalar(&scalar, &simd);
    }
}
