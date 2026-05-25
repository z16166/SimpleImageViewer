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

//! SIMD/NEON row composition for Apple HEIC HDR gain maps.
//!
//! Parallelism stays at the [`ImageLoader`] job level only; this module vectorizes work **within**
//! one decode thread (Windows/macOS/Linux, x86_64/aarch64).

use crate::hdr::decode::{
    bt709_nonlinear_channel_to_linear, decode_transfer_to_display_linear,
    linear_primary_to_linear_srgb,
};
use crate::hdr::gain_map::sample_gain_map_rgb;
use crate::hdr::types::{HdrColorProfile, HdrColorSpace, HdrImageMetadata, HdrTransferFunction};

#[cfg(target_arch = "aarch64")]
use core::arch::aarch64::*;
#[cfg(target_arch = "x86_64")]
use core::arch::x86_64::*;

/// Minimum row width before the SIMD kernel runs (scalar tail handles the remainder).
const SIMD_PIXELS_PER_STEP: u32 = 4;

const SRGB_LINEAR_SEGMENT_END: f32 = 0.04045;
const SRGB_DIVISOR: f32 = 12.92;
const SRGB_OFFSET: f32 = 0.055;
const SRGB_SCALE: f32 = 1.055;
const SRGB_GAMMA: f32 = 2.4;

const BT709_LINEAR_SEGMENT_BREAK: f32 = 0.018 * 4.5;
const BT709_DIVISOR: f32 = 4.5;
const BT709_OFFSET: f32 = 0.099;
const BT709_SCALE: f32 = 1.099;
const BT709_GAMMA: f32 = 1.0 / 0.45;

const DISPLAY_P3_TO_LINEAR_SRGB: [[f32; 3]; 3] = [
    [1.2249401, -0.2249402, 0.0],
    [-0.0420569, 1.0420571, 0.0],
    [-0.0196376, -0.0786507, 1.0982884],
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ComposeFastPath {
    SrgbLinearSrgb,
    SrgbDisplayP3,
    Bt709LinearSrgb,
    Bt709DisplayP3,
    LinearLinearSrgb,
    LinearDisplayP3,
    Scalar,
}

pub(crate) struct GainRowLinear {
    rgb: Vec<f32>,
}

impl GainRowLinear {
    fn ensure_capacity(&mut self, width: usize) {
        let needed = width * 3;
        if self.rgb.len() < needed {
            self.rgb.resize(needed, 0.0);
        }
    }
}

fn classify_fast_path(
    color_space: HdrColorSpace,
    transfer: HdrTransferFunction,
    metadata: &HdrImageMetadata,
) -> ComposeFastPath {
    let effective_space = match color_space {
        HdrColorSpace::LinearScRgb => HdrColorSpace::LinearSrgb,
        HdrColorSpace::Unknown => {
            if matches!(
                metadata.color_profile,
                HdrColorProfile::Cicp {
                    color_primaries: 11,
                    ..
                }
            ) {
                HdrColorSpace::DisplayP3Linear
            } else if matches!(
                metadata.color_profile,
                HdrColorProfile::Cicp {
                    color_primaries: 9,
                    ..
                }
            ) {
                HdrColorSpace::Rec2020Linear
            } else {
                HdrColorSpace::LinearSrgb
            }
        }
        other => other,
    };

    match (transfer, effective_space) {
        (HdrTransferFunction::Srgb, HdrColorSpace::LinearSrgb) => ComposeFastPath::SrgbLinearSrgb,
        (HdrTransferFunction::Srgb, HdrColorSpace::DisplayP3Linear) => {
            ComposeFastPath::SrgbDisplayP3
        }
        (HdrTransferFunction::Bt709, HdrColorSpace::LinearSrgb) => ComposeFastPath::Bt709LinearSrgb,
        (HdrTransferFunction::Bt709, HdrColorSpace::DisplayP3Linear) => {
            ComposeFastPath::Bt709DisplayP3
        }
        (HdrTransferFunction::Linear, HdrColorSpace::LinearSrgb) => {
            ComposeFastPath::LinearLinearSrgb
        }
        (HdrTransferFunction::Linear, HdrColorSpace::DisplayP3Linear) => {
            ComposeFastPath::LinearDisplayP3
        }
        _ => ComposeFastPath::Scalar,
    }
}

fn precompute_gain_row_linear(
    gain_rgba: &[u8],
    gain_w: u32,
    gain_h: u32,
    y: u32,
    width: u32,
    height: u32,
    out: &mut GainRowLinear,
) {
    let w = width as usize;
    out.ensure_capacity(w);
    for x in 0..width {
        let raw = sample_gain_map_rgb(gain_rgba, gain_w, gain_h, x, y, width, height);
        let base = x as usize * 3;
        out.rgb[base] = bt709_nonlinear_channel_to_linear(raw[0]);
        out.rgb[base + 1] = bt709_nonlinear_channel_to_linear(raw[1]);
        out.rgb[base + 2] = bt709_nonlinear_channel_to_linear(raw[2]);
    }
}

fn compose_pixel_scalar(
    row_in: &[f32],
    row_out: &mut [f32],
    x: u32,
    gain_rgb: &[f32],
    color_space: HdrColorSpace,
    transfer: HdrTransferFunction,
    metadata: &HdrImageMetadata,
    headroom_span: f32,
    weight: f32,
) {
    let idx = x as usize * 4;
    let r_code = row_in[idx];
    let g_code = row_in[idx + 1];
    let b_code = row_in[idx + 2];
    let a = row_in[idx + 3];

    let rgb_display_linear = decode_transfer_to_display_linear(
        [r_code, g_code, b_code],
        transfer,
        crate::hdr::types::DEFAULT_SDR_WHITE_NITS,
    );
    let rgb_linear_srgb = linear_primary_to_linear_srgb(rgb_display_linear, color_space, metadata);

    let gain_base = x as usize * 3;
    let gain_linear = [
        gain_rgb[gain_base],
        gain_rgb[gain_base + 1],
        gain_rgb[gain_base + 2],
    ];

    row_out[idx] = (rgb_linear_srgb[0] * (1.0 + headroom_span * gain_linear[0] * weight)).max(0.0);
    row_out[idx + 1] =
        (rgb_linear_srgb[1] * (1.0 + headroom_span * gain_linear[1] * weight)).max(0.0);
    row_out[idx + 2] =
        (rgb_linear_srgb[2] * (1.0 + headroom_span * gain_linear[2] * weight)).max(0.0);
    row_out[idx + 3] = a;
}

fn compose_row_scalar(
    row_in: &[f32],
    row_out: &mut [f32],
    width: u32,
    gain_rgb: &[f32],
    color_space: HdrColorSpace,
    transfer: HdrTransferFunction,
    metadata: &HdrImageMetadata,
    headroom_span: f32,
    weight: f32,
) {
    for x in 0..width {
        compose_pixel_scalar(
            row_in,
            row_out,
            x,
            gain_rgb,
            color_space,
            transfer,
            metadata,
            headroom_span,
            weight,
        );
    }
}

fn path_applies_display_p3_matrix(path: ComposeFastPath) -> bool {
    matches!(
        path,
        ComposeFastPath::SrgbDisplayP3
            | ComposeFastPath::Bt709DisplayP3
            | ComposeFastPath::LinearDisplayP3
    )
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn pow4_sse41(base: __m128, exponent: f32) -> __m128 {
    unsafe {
        let mut lanes = [0.0_f32; 4];
        _mm_storeu_ps(lanes.as_mut_ptr(), base);
        _mm_set_ps(
            lanes[3].powf(exponent),
            lanes[2].powf(exponent),
            lanes[1].powf(exponent),
            lanes[0].powf(exponent),
        )
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn srgb_to_linear4_sse41(v: __m128) -> __m128 {
    unsafe {
        let zero = _mm_setzero_ps();
        let one = _mm_set1_ps(1.0);
        let clamped = _mm_min_ps(_mm_max_ps(v, zero), one);
        let threshold = _mm_set1_ps(SRGB_LINEAR_SEGMENT_END);
        let low_mask = _mm_cmple_ps(clamped, threshold);
        let low = _mm_div_ps(clamped, _mm_set1_ps(SRGB_DIVISOR));
        let adjusted = _mm_div_ps(
            _mm_add_ps(clamped, _mm_set1_ps(SRGB_OFFSET)),
            _mm_set1_ps(SRGB_SCALE),
        );
        let high = pow4_sse41(adjusted, SRGB_GAMMA);
        _mm_blendv_ps(high, low, low_mask)
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn bt709_to_linear4_sse41(v: __m128) -> __m128 {
    unsafe {
        let zero = _mm_setzero_ps();
        let one = _mm_set1_ps(1.0);
        let clamped = _mm_min_ps(_mm_max_ps(v, zero), one);
        let threshold = _mm_set1_ps(BT709_LINEAR_SEGMENT_BREAK);
        let low_mask = _mm_cmplt_ps(clamped, threshold);
        let low = _mm_div_ps(clamped, _mm_set1_ps(BT709_DIVISOR));
        let adjusted = _mm_div_ps(
            _mm_add_ps(clamped, _mm_set1_ps(BT709_OFFSET)),
            _mm_set1_ps(BT709_SCALE),
        );
        let high = pow4_sse41(adjusted, BT709_GAMMA);
        _mm_blendv_ps(high, low, low_mask)
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn load_rgba_pixel4_sse41(
    row: *const f32,
    pixel_offset: usize,
) -> (__m128, __m128, __m128, __m128) {
    unsafe {
        let mut p0 = _mm_loadu_ps(row.add(pixel_offset));
        let mut p1 = _mm_loadu_ps(row.add(pixel_offset + 4));
        let mut p2 = _mm_loadu_ps(row.add(pixel_offset + 8));
        let mut p3 = _mm_loadu_ps(row.add(pixel_offset + 12));
        _MM_TRANSPOSE4_PS(&mut p0, &mut p1, &mut p2, &mut p3);
        (p0, p1, p2, p3)
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn store_rgba_pixel4_sse41(
    row: *mut f32,
    pixel_offset: usize,
    r: __m128,
    g: __m128,
    b: __m128,
    a: __m128,
) {
    unsafe {
        let mut r = r;
        let mut g = g;
        let mut b = b;
        let mut a = a;
        _MM_TRANSPOSE4_PS(&mut r, &mut g, &mut b, &mut a);
        _mm_storeu_ps(row.add(pixel_offset), r);
        _mm_storeu_ps(row.add(pixel_offset + 4), g);
        _mm_storeu_ps(row.add(pixel_offset + 8), b);
        _mm_storeu_ps(row.add(pixel_offset + 12), a);
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn gather_gain_rgb4_sse41(
    gain_rgb: *const f32,
    pixel_offset: usize,
) -> (__m128, __m128, __m128) {
    unsafe {
        let base = pixel_offset * 3;
        let r = _mm_set_ps(
            *gain_rgb.add(base + 9),
            *gain_rgb.add(base + 6),
            *gain_rgb.add(base + 3),
            *gain_rgb.add(base),
        );
        let g = _mm_set_ps(
            *gain_rgb.add(base + 10),
            *gain_rgb.add(base + 7),
            *gain_rgb.add(base + 4),
            *gain_rgb.add(base + 1),
        );
        let b = _mm_set_ps(
            *gain_rgb.add(base + 11),
            *gain_rgb.add(base + 8),
            *gain_rgb.add(base + 5),
            *gain_rgb.add(base + 2),
        );
        (r, g, b)
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn apply_display_p3_matrix4_sse41(
    r: __m128,
    g: __m128,
    b: __m128,
) -> (__m128, __m128, __m128) {
    let m = DISPLAY_P3_TO_LINEAR_SRGB;
    let lr = _mm_add_ps(
        _mm_add_ps(
            _mm_mul_ps(r, _mm_set1_ps(m[0][0])),
            _mm_mul_ps(g, _mm_set1_ps(m[0][1])),
        ),
        _mm_mul_ps(b, _mm_set1_ps(m[0][2])),
    );
    let lg = _mm_add_ps(
        _mm_add_ps(
            _mm_mul_ps(r, _mm_set1_ps(m[1][0])),
            _mm_mul_ps(g, _mm_set1_ps(m[1][1])),
        ),
        _mm_mul_ps(b, _mm_set1_ps(m[1][2])),
    );
    let lb = _mm_add_ps(
        _mm_add_ps(
            _mm_mul_ps(r, _mm_set1_ps(m[2][0])),
            _mm_mul_ps(g, _mm_set1_ps(m[2][1])),
        ),
        _mm_mul_ps(b, _mm_set1_ps(m[2][2])),
    );
    (lr, lg, lb)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn compose_gain4_sse41(
    linear_r: __m128,
    linear_g: __m128,
    linear_b: __m128,
    alpha: __m128,
    gain_r: __m128,
    gain_g: __m128,
    gain_b: __m128,
    headroom_span: f32,
    weight: f32,
) -> (__m128, __m128, __m128, __m128) {
    let one = _mm_set1_ps(1.0);
    let span = _mm_set1_ps(headroom_span);
    let w = _mm_set1_ps(weight);
    let zero = _mm_setzero_ps();
    let scale_r = _mm_add_ps(one, _mm_mul_ps(span, _mm_mul_ps(gain_r, w)));
    let scale_g = _mm_add_ps(one, _mm_mul_ps(span, _mm_mul_ps(gain_g, w)));
    let scale_b = _mm_add_ps(one, _mm_mul_ps(span, _mm_mul_ps(gain_b, w)));
    let out_r = _mm_max_ps(_mm_mul_ps(linear_r, scale_r), zero);
    let out_g = _mm_max_ps(_mm_mul_ps(linear_g, scale_g), zero);
    let out_b = _mm_max_ps(_mm_mul_ps(linear_b, scale_b), zero);
    (out_r, out_g, out_b, alpha)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn apply_transfer4_sse41(
    r: __m128,
    g: __m128,
    b: __m128,
    path: ComposeFastPath,
) -> (__m128, __m128, __m128) {
    unsafe {
        match path {
            ComposeFastPath::SrgbLinearSrgb | ComposeFastPath::SrgbDisplayP3 => (
                srgb_to_linear4_sse41(r),
                srgb_to_linear4_sse41(g),
                srgb_to_linear4_sse41(b),
            ),
            ComposeFastPath::Bt709LinearSrgb | ComposeFastPath::Bt709DisplayP3 => (
                bt709_to_linear4_sse41(r),
                bt709_to_linear4_sse41(g),
                bt709_to_linear4_sse41(b),
            ),
            ComposeFastPath::LinearLinearSrgb | ComposeFastPath::LinearDisplayP3 => (r, g, b),
            ComposeFastPath::Scalar => unreachable!("scalar path uses scalar row loop"),
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn compose_row_sse41(
    row_in: &[f32],
    row_out: &mut [f32],
    width: u32,
    gain_rgb: &[f32],
    path: ComposeFastPath,
    color_space: HdrColorSpace,
    transfer: HdrTransferFunction,
    metadata: &HdrImageMetadata,
    headroom_span: f32,
    weight: f32,
) {
    unsafe {
        let in_ptr = row_in.as_ptr();
        let out_ptr = row_out.as_mut_ptr();
        let gain_ptr = gain_rgb.as_ptr();
        let mut x = 0_u32;
        while x + SIMD_PIXELS_PER_STEP <= width {
            let offset = x as usize * 4;
            let (r, g, b, a) = load_rgba_pixel4_sse41(in_ptr, offset);
            let (mut lr, mut lg, mut lb) = apply_transfer4_sse41(r, g, b, path);
            if path_applies_display_p3_matrix(path) {
                (lr, lg, lb) = apply_display_p3_matrix4_sse41(lr, lg, lb);
            }
            let (gain_r, gain_g, gain_b) = gather_gain_rgb4_sse41(gain_ptr, x as usize);
            let (out_r, out_g, out_b, out_a) =
                compose_gain4_sse41(lr, lg, lb, a, gain_r, gain_g, gain_b, headroom_span, weight);
            store_rgba_pixel4_sse41(out_ptr, offset, out_r, out_g, out_b, out_a);
            x += SIMD_PIXELS_PER_STEP;
        }
        while x < width {
            compose_pixel_scalar(
                row_in,
                row_out,
                x,
                gain_rgb,
                color_space,
                transfer,
                metadata,
                headroom_span,
                weight,
            );
            x += 1;
        }
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn pow4_neon(base: float32x4_t, exponent: f32) -> float32x4_t {
    let mut lanes = [0.0_f32; 4];
    vst1q_f32(lanes.as_mut_ptr(), base);
    let result: [f32; 4] = [
        lanes[0].powf(exponent),
        lanes[1].powf(exponent),
        lanes[2].powf(exponent),
        lanes[3].powf(exponent),
    ];
    vld1q_f32(result.as_ptr())
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn srgb_to_linear4_neon(v: float32x4_t) -> float32x4_t {
    let zero = vdupq_n_f32(0.0);
    let one = vdupq_n_f32(1.0);
    let clamped = vminq_f32(vmaxq_f32(v, zero), one);
    let threshold = vdupq_n_f32(SRGB_LINEAR_SEGMENT_END);
    let low_mask = vcleq_f32(clamped, threshold);
    let low = vdivq_f32(clamped, vdupq_n_f32(SRGB_DIVISOR));
    let adjusted = vdivq_f32(
        vaddq_f32(clamped, vdupq_n_f32(SRGB_OFFSET)),
        vdupq_n_f32(SRGB_SCALE),
    );
    let high = pow4_neon(adjusted, SRGB_GAMMA);
    vbslq_f32(low_mask, low, high)
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn bt709_to_linear4_neon(v: float32x4_t) -> float32x4_t {
    let zero = vdupq_n_f32(0.0);
    let one = vdupq_n_f32(1.0);
    let clamped = vminq_f32(vmaxq_f32(v, zero), one);
    let threshold = vdupq_n_f32(BT709_LINEAR_SEGMENT_BREAK);
    let low_mask = vcltq_f32(clamped, threshold);
    let low = vdivq_f32(clamped, vdupq_n_f32(BT709_DIVISOR));
    let adjusted = vdivq_f32(
        vaddq_f32(clamped, vdupq_n_f32(BT709_OFFSET)),
        vdupq_n_f32(BT709_SCALE),
    );
    let high = pow4_neon(adjusted, BT709_GAMMA);
    vbslq_f32(low_mask, low, high)
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn load_rgba_pixel4_neon(
    row: *const f32,
    pixel_offset: usize,
) -> (float32x4_t, float32x4_t, float32x4_t, float32x4_t) {
    let p0 = vld1q_f32(row.add(pixel_offset));
    let p1 = vld1q_f32(row.add(pixel_offset + 4));
    let p2 = vld1q_f32(row.add(pixel_offset + 8));
    let p3 = vld1q_f32(row.add(pixel_offset + 12));
    let t0 = vtrnq_f32(p0, p1);
    let t1 = vtrnq_f32(p2, p3);
    let r = vcombine_f32(vget_low_f32(t0.0), vget_low_f32(t1.0));
    let g = vcombine_f32(vget_low_f32(t0.1), vget_low_f32(t1.1));
    let b = vcombine_f32(vget_high_f32(t0.0), vget_high_f32(t1.0));
    let a = vcombine_f32(vget_high_f32(t0.1), vget_high_f32(t1.1));
    (r, g, b, a)
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn store_rgba_pixel4_neon(
    row: *mut f32,
    pixel_offset: usize,
    r: float32x4_t,
    g: float32x4_t,
    b: float32x4_t,
    a: float32x4_t,
) {
    let rg0 = vtrn1q_f32(r, g);
    let rg1 = vtrn2q_f32(r, g);
    let ba0 = vtrn1q_f32(b, a);
    let ba1 = vtrn2q_f32(b, a);
    let p0 = vcombine_f32(vget_low_f32(rg0), vget_low_f32(ba0));
    let p1 = vcombine_f32(vget_low_f32(rg1), vget_low_f32(ba1));
    let p2 = vcombine_f32(vget_high_f32(rg0), vget_high_f32(ba0));
    let p3 = vcombine_f32(vget_high_f32(rg1), vget_high_f32(ba1));
    vst1q_f32(row.add(pixel_offset), p0);
    vst1q_f32(row.add(pixel_offset + 4), p1);
    vst1q_f32(row.add(pixel_offset + 8), p2);
    vst1q_f32(row.add(pixel_offset + 12), p3);
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn gather_gain_rgb4_neon(
    gain_rgb: *const f32,
    pixel_offset: usize,
) -> (float32x4_t, float32x4_t, float32x4_t) {
    let base = pixel_offset * 3;
    let r_arr: [f32; 4] = [
        *gain_rgb.add(base),
        *gain_rgb.add(base + 3),
        *gain_rgb.add(base + 6),
        *gain_rgb.add(base + 9),
    ];
    let g_arr: [f32; 4] = [
        *gain_rgb.add(base + 1),
        *gain_rgb.add(base + 4),
        *gain_rgb.add(base + 7),
        *gain_rgb.add(base + 10),
    ];
    let b_arr: [f32; 4] = [
        *gain_rgb.add(base + 2),
        *gain_rgb.add(base + 5),
        *gain_rgb.add(base + 8),
        *gain_rgb.add(base + 11),
    ];
    let r = vld1q_f32(r_arr.as_ptr());
    let g = vld1q_f32(g_arr.as_ptr());
    let b = vld1q_f32(b_arr.as_ptr());
    (r, g, b)
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn apply_display_p3_matrix4_neon(
    r: float32x4_t,
    g: float32x4_t,
    b: float32x4_t,
) -> (float32x4_t, float32x4_t, float32x4_t) {
    let m = DISPLAY_P3_TO_LINEAR_SRGB;
    let lr = vaddq_f32(
        vaddq_f32(
            vmulq_f32(r, vdupq_n_f32(m[0][0])),
            vmulq_f32(g, vdupq_n_f32(m[0][1])),
        ),
        vmulq_f32(b, vdupq_n_f32(m[0][2])),
    );
    let lg = vaddq_f32(
        vaddq_f32(
            vmulq_f32(r, vdupq_n_f32(m[1][0])),
            vmulq_f32(g, vdupq_n_f32(m[1][1])),
        ),
        vmulq_f32(b, vdupq_n_f32(m[1][2])),
    );
    let lb = vaddq_f32(
        vaddq_f32(
            vmulq_f32(r, vdupq_n_f32(m[2][0])),
            vmulq_f32(g, vdupq_n_f32(m[2][1])),
        ),
        vmulq_f32(b, vdupq_n_f32(m[2][2])),
    );
    (lr, lg, lb)
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn compose_gain4_neon(
    linear_r: float32x4_t,
    linear_g: float32x4_t,
    linear_b: float32x4_t,
    alpha: float32x4_t,
    gain_r: float32x4_t,
    gain_g: float32x4_t,
    gain_b: float32x4_t,
    headroom_span: f32,
    weight: f32,
) -> (float32x4_t, float32x4_t, float32x4_t, float32x4_t) {
    let one = vdupq_n_f32(1.0);
    let span = vdupq_n_f32(headroom_span);
    let w = vdupq_n_f32(weight);
    let zero = vdupq_n_f32(0.0);
    let scale_r = vaddq_f32(one, vmulq_f32(span, vmulq_f32(gain_r, w)));
    let scale_g = vaddq_f32(one, vmulq_f32(span, vmulq_f32(gain_g, w)));
    let scale_b = vaddq_f32(one, vmulq_f32(span, vmulq_f32(gain_b, w)));
    let out_r = vmaxq_f32(vmulq_f32(linear_r, scale_r), zero);
    let out_g = vmaxq_f32(vmulq_f32(linear_g, scale_g), zero);
    let out_b = vmaxq_f32(vmulq_f32(linear_b, scale_b), zero);
    (out_r, out_g, out_b, alpha)
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn apply_transfer4_neon(
    r: float32x4_t,
    g: float32x4_t,
    b: float32x4_t,
    path: ComposeFastPath,
) -> (float32x4_t, float32x4_t, float32x4_t) {
    unsafe {
        match path {
            ComposeFastPath::SrgbLinearSrgb | ComposeFastPath::SrgbDisplayP3 => (
                srgb_to_linear4_neon(r),
                srgb_to_linear4_neon(g),
                srgb_to_linear4_neon(b),
            ),
            ComposeFastPath::Bt709LinearSrgb | ComposeFastPath::Bt709DisplayP3 => (
                bt709_to_linear4_neon(r),
                bt709_to_linear4_neon(g),
                bt709_to_linear4_neon(b),
            ),
            ComposeFastPath::LinearLinearSrgb | ComposeFastPath::LinearDisplayP3 => (r, g, b),
            ComposeFastPath::Scalar => unreachable!("scalar path uses scalar row loop"),
        }
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn compose_row_neon(
    row_in: &[f32],
    row_out: &mut [f32],
    width: u32,
    gain_rgb: &[f32],
    path: ComposeFastPath,
    color_space: HdrColorSpace,
    transfer: HdrTransferFunction,
    metadata: &HdrImageMetadata,
    headroom_span: f32,
    weight: f32,
) {
    unsafe {
        let in_ptr = row_in.as_ptr();
        let out_ptr = row_out.as_mut_ptr();
        let gain_ptr = gain_rgb.as_ptr();
        let mut x = 0_u32;
        while x + SIMD_PIXELS_PER_STEP <= width {
            let offset = x as usize * 4;
            let (r, g, b, a) = load_rgba_pixel4_neon(in_ptr, offset);
            let (mut lr, mut lg, mut lb) = apply_transfer4_neon(r, g, b, path);
            if path_applies_display_p3_matrix(path) {
                (lr, lg, lb) = apply_display_p3_matrix4_neon(lr, lg, lb);
            }
            let (gain_r, gain_g, gain_b) = gather_gain_rgb4_neon(gain_ptr, x as usize);
            let (out_r, out_g, out_b, out_a) =
                compose_gain4_neon(lr, lg, lb, a, gain_r, gain_g, gain_b, headroom_span, weight);
            store_rgba_pixel4_neon(out_ptr, offset, out_r, out_g, out_b, out_a);
            x += SIMD_PIXELS_PER_STEP;
        }
        while x < width {
            compose_pixel_scalar(
                row_in,
                row_out,
                x,
                gain_rgb,
                color_space,
                transfer,
                metadata,
                headroom_span,
                weight,
            );
            x += 1;
        }
    }
}

fn compose_row(
    row_in: &[f32],
    row_out: &mut [f32],
    width: u32,
    gain_rgb: &[f32],
    path: ComposeFastPath,
    color_space: HdrColorSpace,
    transfer: HdrTransferFunction,
    metadata: &HdrImageMetadata,
    headroom_span: f32,
    weight: f32,
) {
    if path == ComposeFastPath::Scalar || width < SIMD_PIXELS_PER_STEP {
        compose_row_scalar(
            row_in,
            row_out,
            width,
            gain_rgb,
            color_space,
            transfer,
            metadata,
            headroom_span,
            weight,
        );
        return;
    }

    #[cfg(target_arch = "x86_64")]
    {
        if std::arch::is_x86_feature_detected!("sse4.1") {
            unsafe {
                compose_row_sse41(
                    row_in,
                    row_out,
                    width,
                    gain_rgb,
                    path,
                    color_space,
                    transfer,
                    metadata,
                    headroom_span,
                    weight,
                );
            }
            return;
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        unsafe {
            compose_row_neon(
                row_in,
                row_out,
                width,
                gain_rgb,
                path,
                color_space,
                transfer,
                metadata,
                headroom_span,
                weight,
            );
        }
        return;
    }

    compose_row_scalar(
        row_in,
        row_out,
        width,
        gain_rgb,
        color_space,
        transfer,
        metadata,
        headroom_span,
        weight,
    );
}

pub(crate) fn compose_apple_gain_map_pixels(
    base_pixels: &[f32],
    composed_pixels: &mut [f32],
    width: u32,
    height: u32,
    gain_rgba: &[u8],
    gain_w: u32,
    gain_h: u32,
    color_space: HdrColorSpace,
    transfer: HdrTransferFunction,
    metadata: &HdrImageMetadata,
    headroom_span: f32,
    weight: f32,
) {
    if width == 0 || height == 0 {
        return;
    }

    let path = classify_fast_path(color_space, transfer, metadata);
    let row_stride = width as usize * 4;
    let mut gain_row = GainRowLinear { rgb: Vec::new() };

    for (y, (row_out, row_in)) in composed_pixels
        .chunks_mut(row_stride)
        .zip(base_pixels.chunks(row_stride))
        .enumerate()
    {
        precompute_gain_row_linear(
            gain_rgba,
            gain_w,
            gain_h,
            y as u32,
            width,
            height,
            &mut gain_row,
        );
        compose_row(
            row_in,
            row_out,
            width,
            &gain_row.rgb,
            path,
            color_space,
            transfer,
            metadata,
            headroom_span,
            weight,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hdr::types::HdrImageMetadata;

    fn scalar_reference_row(
        row_in: &[f32],
        row_out: &mut [f32],
        width: u32,
        gain_rgba: &[u8],
        gain_w: u32,
        gain_h: u32,
        y: u32,
        height: u32,
        color_space: HdrColorSpace,
        transfer: HdrTransferFunction,
        metadata: &HdrImageMetadata,
        headroom_span: f32,
        weight: f32,
    ) {
        let mut gain_row = GainRowLinear { rgb: Vec::new() };
        precompute_gain_row_linear(gain_rgba, gain_w, gain_h, y, width, height, &mut gain_row);
        compose_row_scalar(
            row_in,
            row_out,
            width,
            &gain_row.rgb,
            color_space,
            transfer,
            metadata,
            headroom_span,
            weight,
        );
    }

    #[test]
    fn simd_compose_matches_scalar_for_common_heic_paths() {
        const W: u32 = 67;
        const H: u32 = 3;
        let pixel_count = W as usize * H as usize * 4;
        let base_pixels: Vec<f32> = (0..pixel_count)
            .map(|i| ((i * 17 + 3) % 997) as f32 / 997.0)
            .collect();
        let gain_rgba: Vec<u8> = (0..W as usize * H as usize * 4)
            .map(|i| ((i * 13 + 7) % 256) as u8)
            .collect();
        let headroom_span = 3.0;
        let weight = 0.75;

        let cases = [
            (
                HdrColorSpace::LinearSrgb,
                HdrTransferFunction::Srgb,
                HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
            ),
            (
                HdrColorSpace::DisplayP3Linear,
                HdrTransferFunction::Srgb,
                HdrImageMetadata::from_color_space(HdrColorSpace::DisplayP3Linear),
            ),
            (
                HdrColorSpace::LinearSrgb,
                HdrTransferFunction::Bt709,
                HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
            ),
            (
                HdrColorSpace::DisplayP3Linear,
                HdrTransferFunction::Bt709,
                HdrImageMetadata::from_color_space(HdrColorSpace::DisplayP3Linear),
            ),
        ];

        for (color_space, transfer, metadata) in cases {
            let mut scalar = vec![0.0_f32; pixel_count];
            let mut simd = vec![0.0_f32; pixel_count];
            for y in 0..H {
                let row_stride = W as usize * 4;
                let start = y as usize * row_stride;
                let end = start + row_stride;
                scalar_reference_row(
                    &base_pixels[start..end],
                    &mut scalar[start..end],
                    W,
                    &gain_rgba,
                    W,
                    H,
                    y,
                    H,
                    color_space,
                    transfer,
                    &metadata,
                    headroom_span,
                    weight,
                );
            }
            compose_apple_gain_map_pixels(
                &base_pixels,
                &mut simd,
                W,
                H,
                &gain_rgba,
                W,
                H,
                color_space,
                transfer,
                &metadata,
                headroom_span,
                weight,
            );
            assert_eq!(
                scalar, simd,
                "parity failed for {color_space:?} + {transfer:?}"
            );
        }
    }
}
