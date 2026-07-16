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
//! Rows compose in parallel via rayon; each row applies Y interpolation between two
//! pre-X-upsampled gain rows from [`precompute_gain_map_x_upsampled`]. Per-pixel ISO recovery uses AVX2 (`pow8_avx2` /
//! `exp2_8_avx2`, 8 pixels/step), SSE4.1 (`pow4_sse41` / `exp2_4_sse41`), or NEON
//! (`pow4_neon` / `exp2_4_neon`) for gain shaping and per-lane `2^log_boost`, matching
//! the scalar reference within the same tolerance band as
//! [`crate::hdr::heif_apple_gain_map_compose_simd`].

use std::cell::UnsafeCell;

use rayon::prelude::*;

use crate::hdr::gain_map::{
    GainMapMetadata, compose_gain_map_pixel, gain_map_weight, precompute_gain_map_x_upsampled,
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

#[cfg(target_arch = "x86_64")]
#[path = "iso_gain_map_compose_simd_avx2.rs"]
mod avx2;

const SIMD_PIXELS_PER_STEP: u32 = 4;

const SRGB_LINEAR_SEGMENT_END: f32 = 0.04045;
const SRGB_DIVISOR: f32 = 12.92;
const SRGB_OFFSET: f32 = 0.055;
const SRGB_SCALE: f32 = 1.055;
const SRGB_GAMMA: f32 = 2.4;

thread_local! {
    static GAIN_ROW_SCRATCH: UnsafeCell<Vec<f32>> = const { UnsafeCell::new(Vec::new()) };
}

/// SAFETY: called from within a single rayon thread (`thread_local!`), single-borrow,
/// never re-entered because `compose_iso_row` and its callees do not recurse into
/// [`compose_iso_deferred_cpu_pixels_simd`].
///
/// We use `UnsafeCell` rather than `RefCell` (cf. Apple HEIF path) because no borrow-check
/// is needed at runtime — the access pattern is strictly non-reentrant within one rayon
/// worker. This avoids the (negligible) runtime cost of `RefCell` bookkeeping and keeps the
/// hot row loop free of panicking paths.
fn with_gain_scratch<R>(f: impl FnOnce(&mut Vec<f32>) -> R) -> R {
    GAIN_ROW_SCRATCH.with(|cell| {
        // SAFETY: thread-local, no other borrow exists at this call site.
        let vec = unsafe { &mut *cell.get() };
        f(vec)
    })
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

/// Flat scalar pack pre-built once and passed by reference to every SIMD
/// inner loop — eliminates per-step struct traversal through
/// [`GainMapMetadata`] + per-channel indexing in hot functions such as
/// [`recover_hdr_rgb4_sse41`], [`recover_hdr_rgb4_neon`], and
/// [`avx2::recover_hdr_rgb8_avx2`].
#[derive(Clone, Copy)]
struct IsoComposeSimdPack {
    weight: f32,
    inv_gamma_r: f32,
    inv_gamma_g: f32,
    inv_gamma_b: f32,
    gain_min_r: f32,
    gain_min_g: f32,
    gain_min_b: f32,
    gain_span_r: f32,
    gain_span_g: f32,
    gain_span_b: f32,
    offset_sdr_r: f32,
    offset_sdr_g: f32,
    offset_sdr_b: f32,
    offset_hdr_r: f32,
    offset_hdr_g: f32,
    offset_hdr_b: f32,
}

impl IsoComposeConstants {
    /// Pre-compute a flat scalar pack so SIMD inner loops see plain fields
    /// instead of reaching through [`GainMapMetadata`] + per-channel indexing.
    fn to_simd_pack(&self) -> IsoComposeSimdPack {
        IsoComposeSimdPack {
            weight: self.gain_weight,
            inv_gamma_r: self.inv_gamma[0],
            inv_gamma_g: self.inv_gamma[1],
            inv_gamma_b: self.inv_gamma[2],
            gain_min_r: self.metadata.gain_map_min[0],
            gain_min_g: self.metadata.gain_map_min[1],
            gain_min_b: self.metadata.gain_map_min[2],
            gain_span_r: self.gain_span[0],
            gain_span_g: self.gain_span[1],
            gain_span_b: self.gain_span[2],
            offset_sdr_r: self.metadata.offset_sdr[0],
            offset_sdr_g: self.metadata.offset_sdr[1],
            offset_sdr_b: self.metadata.offset_sdr[2],
            offset_hdr_r: self.metadata.offset_hdr[0],
            offset_hdr_g: self.metadata.offset_hdr[1],
            offset_hdr_b: self.metadata.offset_hdr[2],
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
    let simd_pack = constants.to_simd_pack();
    let rgba_len = crate::constants::checked_rgba_buffer_len(width as usize, height as usize)
        .ok_or_else(|| format!("ISO gain map compose buffer size overflow for {width}x{height}"))?;
    let mut rgba_f32 = vec![0.0_f32; rgba_len];
    let row_stride = width as usize * 4;
    let sdr = deferred.sdr_rgba.as_slice();

    // Precompute gain-map X-upsample once (decouples X from Y interpolation),
    // eliminating per-row re-sampling of gain map bytes when the gain map
    // is much smaller than the primary image.
    let gain_pre_x = precompute_gain_map_x_upsampled(
        deferred.gain_rgba.as_slice(),
        deferred.gain_width,
        deferred.gain_height,
        width,
    );
    let gain_h = deferred.gain_height as usize;

    rgba_f32
        .par_chunks_mut(row_stride)
        .enumerate()
        .for_each(|(y, row_out)| {
            with_gain_scratch(|gain_row| {
                let needed = width as usize * 3;
                if gain_row.capacity() < needed {
                    let extra = needed - gain_row.len();
                    gain_row.reserve(extra);
                }
                // SAFETY: capacity >= needed; contents are fully overwritten below.
                unsafe {
                    gain_row.set_len(needed);
                }

                // Y-interpolate from precomputed X-upsampled rows.
                let gy = ((y as f32 + 0.5) * gain_h as f32 / height as f32 - 0.5)
                    .clamp(0.0, gain_h.saturating_sub(1) as f32);
                let y0 = gy.floor() as usize;
                let y1 = (y0 + 1).min(gain_h.saturating_sub(1));
                let ty = gy - y0 as f32;
                let w = width as usize;
                let row0 = &gain_pre_x[y0 * w * 3..];
                let row1 = &gain_pre_x[y1 * w * 3..];
                for ch in 0..3 {
                    let base = ch * w;
                    let dst = &mut gain_row[base..base + w];
                    let src0 = &row0[base..base + w];
                    let src1 = &row1[base..base + w];
                    for xi in 0..w {
                        dst[xi] = src0[xi] + (src1[xi] - src0[xi]) * ty;
                    }
                }

                compose_iso_row(
                    &sdr[y * row_stride..(y + 1) * row_stride],
                    row_out,
                    gain_row,
                    constants,
                    &simd_pack,
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
    c: &IsoComposeSimdPack,
) {
    let width = sdr_row.len() / 4;
    let mut x = {
        #[cfg(target_arch = "x86_64")]
        {
            if std::arch::is_x86_feature_detected!("avx2")
                && std::arch::is_x86_feature_detected!("fma")
            {
                let mut simd_x = 0_u32;
                unsafe {
                    avx2::compose_iso_row_avx2(
                        sdr_row,
                        row_out,
                        gain_row,
                        width as u32,
                        c,
                        &mut simd_x,
                    );
                }
                simd_x as usize
            } else if std::arch::is_x86_feature_detected!("sse4.1") {
                let mut simd_x = 0_u32;
                unsafe {
                    compose_iso_row_sse41(sdr_row, row_out, gain_row, width as u32, c, &mut simd_x);
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
                compose_iso_row_neon(sdr_row, row_out, gain_row, width as u32, c, &mut simd_x);
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
    c: &IsoComposeSimdPack,
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
                recover_hdr_rgb4_sse41(enc_r, enc_g, enc_b, gain_r, gain_g, gain_b, c);
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
    c: &IsoComposeSimdPack,
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
                recover_hdr_rgb4_neon(enc_r, enc_g, enc_b, gain_r, gain_g, gain_b, c);
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
        // Load 4 RGBA pixels (16 bytes) — safe: only reads 4 pixels.
        // vld4_u8 would read 8 pixels (32 bytes) and overrun the row.
        let rgba = vld1q_u8(ptr);
        // Extract R (bytes 0,4,8,12), G (1,5,9,13), B (2,6,10,14) via lookup.
        // vcreate_u8(u64) interprets as 8 little-endian bytes.
        let r_u8 = vqtbl1_u8(rgba, vcreate_u8(0x0C080400));
        let g_u8 = vqtbl1_u8(rgba, vcreate_u8(0x0D090501));
        let b_u8 = vqtbl1_u8(rgba, vcreate_u8(0x0E0A0602));
        let r = vmulq_f32(
            vcvtq_f32_u32(vmovl_u16(vget_low_u16(vmovl_u8(r_u8)))),
            scale,
        );
        let g = vmulq_f32(
            vcvtq_f32_u32(vmovl_u16(vget_low_u16(vmovl_u8(g_u8)))),
            scale,
        );
        let b = vmulq_f32(
            vcvtq_f32_u32(vmovl_u16(vget_low_u16(vmovl_u8(b_u8)))),
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
    c: &IsoComposeSimdPack,
) -> (__m128, __m128, __m128) {
    unsafe {
        let weight = _mm_set1_ps(c.weight);
        let zero = _mm_setzero_ps();
        let lr = srgb_encoded_to_linear4_sse41(enc_r);
        let lg = srgb_encoded_to_linear4_sse41(enc_g);
        let lb = srgb_encoded_to_linear4_sse41(enc_b);

        let gain_min_r = _mm_set1_ps(c.gain_min_r);
        let gain_min_g = _mm_set1_ps(c.gain_min_g);
        let gain_min_b = _mm_set1_ps(c.gain_min_b);
        let gain_span_r = _mm_set1_ps(c.gain_span_r);
        let gain_span_g = _mm_set1_ps(c.gain_span_g);
        let gain_span_b = _mm_set1_ps(c.gain_span_b);
        let offset_sdr_r = _mm_set1_ps(c.offset_sdr_r);
        let offset_sdr_g = _mm_set1_ps(c.offset_sdr_g);
        let offset_sdr_b = _mm_set1_ps(c.offset_sdr_b);
        let offset_hdr_r = _mm_set1_ps(c.offset_hdr_r);
        let offset_hdr_g = _mm_set1_ps(c.offset_hdr_g);
        let offset_hdr_b = _mm_set1_ps(c.offset_hdr_b);

        let shaped_r = pow4_sse41(gain_r, c.inv_gamma_r);
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

        let shaped_g = pow4_sse41(gain_g, c.inv_gamma_g);
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

        let shaped_b = pow4_sse41(gain_b, c.inv_gamma_b);
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
    c: &IsoComposeSimdPack,
) -> (float32x4_t, float32x4_t, float32x4_t) {
    unsafe {
        let weight = vdupq_n_f32(c.weight);
        let zero = vdupq_n_f32(0.0);
        let lr = srgb_encoded_to_linear4_neon(enc_r);
        let lg = srgb_encoded_to_linear4_neon(enc_g);
        let lb = srgb_encoded_to_linear4_neon(enc_b);

        let gain_min_r = vdupq_n_f32(c.gain_min_r);
        let gain_min_g = vdupq_n_f32(c.gain_min_g);
        let gain_min_b = vdupq_n_f32(c.gain_min_b);
        let gain_span_r = vdupq_n_f32(c.gain_span_r);
        let gain_span_g = vdupq_n_f32(c.gain_span_g);
        let gain_span_b = vdupq_n_f32(c.gain_span_b);
        let offset_sdr_r = vdupq_n_f32(c.offset_sdr_r);
        let offset_sdr_g = vdupq_n_f32(c.offset_sdr_g);
        let offset_sdr_b = vdupq_n_f32(c.offset_sdr_b);
        let offset_hdr_r = vdupq_n_f32(c.offset_hdr_r);
        let offset_hdr_g = vdupq_n_f32(c.offset_hdr_g);
        let offset_hdr_b = vdupq_n_f32(c.offset_hdr_b);

        let shaped_r = pow4_neon(gain_r, c.inv_gamma_r);
        let scaled_r = vmulq_f32(gain_span_r, shaped_r);
        let boost_r = exp2_4_neon(vfmaq_f32(gain_min_r, scaled_r, weight));
        let out_r = vmaxq_f32(
            vsubq_f32(
                vmulq_f32(vaddq_f32(lr, offset_sdr_r), boost_r),
                offset_hdr_r,
            ),
            zero,
        );

        let shaped_g = pow4_neon(gain_g, c.inv_gamma_g);
        let scaled_g = vmulq_f32(gain_span_g, shaped_g);
        let boost_g = exp2_4_neon(vfmaq_f32(gain_min_g, scaled_g, weight));
        let out_g = vmaxq_f32(
            vsubq_f32(
                vmulq_f32(vaddq_f32(lg, offset_sdr_g), boost_g),
                offset_hdr_g,
            ),
            zero,
        );

        let shaped_b = pow4_neon(gain_b, c.inv_gamma_b);
        let scaled_b = vmulq_f32(gain_span_b, shaped_b);
        let boost_b = exp2_4_neon(vfmaq_f32(gain_min_b, scaled_b, weight));
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
        // Load 4 RGBA pixels (16 bytes) — safe: only reads 4 pixels.
        // vld4_u8 would read 8 pixels (32 bytes) and overrun the row.
        let rgba = vld1q_u8(sdr);
        // Extract A at bytes 3, 7, 11, 15 via lookup.
        let a_u8 = vqtbl1_u8(rgba, vcreate_u8(0x0F0B0703));
        let a = vmulq_f32(
            vcvtq_f32_u32(vmovl_u16(vget_low_u16(vmovl_u8(a_u8)))),
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

    #[test]
    fn iso_compose_simd_non_aligned_widths_matches_scalar() {
        // Regression: cover widths not divisible by 4 or 8 so the scalar tail
        // (0-3 pixels for SSE4.1/NEON, 0-7 for AVX2) is exercised on all paths.
        let widths: &[u32] = &[1, 2, 3, 5, 6, 7, 9];
        let height = 48_u32;
        let gain_w = 8_u32;
        let gain_h = 6_u32;
        let metadata = test_metadata();
        let mut gain = vec![0_u8; (gain_w * gain_h * 4) as usize];
        for (index, byte) in gain.iter_mut().enumerate() {
            *byte = ((index * 31 + 64) % 256) as u8;
        }
        for &width in widths {
            let mut sdr = vec![0_u8; (width * height * 4) as usize];
            for (index, byte) in sdr.iter_mut().enumerate() {
                *byte = ((index * 17) % 256) as u8;
            }
            let deferred = IsoGainMapGpuSource {
                sdr_rgba: Arc::new(sdr),
                gain_rgba: Arc::new(gain.clone()),
                gain_width: gain_w,
                gain_height: gain_h,
                metadata: metadata.clone(),
            };
            let scalar = compose_iso_deferred_cpu_pixels_scalar(width, height, &deferred, 2.0)
                .expect("scalar");
            let simd =
                compose_iso_deferred_cpu_pixels_simd(width, height, &deferred, 2.0).expect("simd");
            assert_iso_compose_near_scalar(&scalar, &simd);
        }
    }
}
