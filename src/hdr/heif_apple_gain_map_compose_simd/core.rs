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
//! Parallelism: rows compose in parallel via rayon; within each row, x86_64/aarch64 SIMD applies.
//!
//! ## Gain-map math (must stay bit-identical to legacy)
//!
//! Per primary pixel the reference order is **`bt709( bilinear_sample_nonlinear(gain) )`** — bilinear
//! interpolation on encoded gain-map texels, then BT.709 OETF⁻¹ per channel.
//!
//! Per-row pipeline: cached bilinear upsample on encoded gain → SIMD BT.709 linearize → SIMD compose.
//! Same math as legacy (`bt709(bilinear(sample))`); only execution order and vectorization differ.
//!
//! Bilinear uses row scanline pointers plus **`x0` texel caching**: when several primary
//! pixels share the same gain-map column (`x0`), the four corner texels are reused and only
//! `tx`/`ty` lerps are recomputed (typical when the gain map is smaller than the primary).
//! Do **not** skip bilinear when gain and primary dimensions match: center-aligned coords can still
//! fall between texels for some columns due to floating-point placement.
//!
//! **Do not** BT.709-decode the small gain map first and bilinear-filter in linear space
//! (`lerp(bt709(c))`); that is faster but **not equivalent** to legacy and must not ship as default.
//!
//! On x86_64, `load_rgb_interleaved4_sse41` / `store_rgb_interleaved4_sse41` replace scalar gather/scatter
//! for RGB-interleaved gain rows (aarch64 already uses `vld3q_f32` / `vst3q_f32`).

use crate::hdr::decode::{
    bt709_nonlinear_channel_to_linear, decode_transfer_to_display_linear,
    linear_primary_to_linear_srgb,
};
#[cfg(target_arch = "aarch64")]
use crate::hdr::simd_fast_pow::pow4_neon;
#[cfg(target_arch = "x86_64")]
use crate::hdr::simd_fast_pow::pow4_sse41;
use crate::hdr::types::{HdrColorProfile, HdrColorSpace, HdrImageMetadata, HdrTransferFunction};

#[cfg(target_arch = "aarch64")]
use core::arch::aarch64::*;
#[cfg(target_arch = "x86_64")]
use core::arch::x86_64::*;

/// Minimum row width before the SIMD kernel runs (scalar tail handles the remainder).
pub(crate) const SIMD_PIXELS_PER_STEP: u32 = 4;

#[derive(Clone, Copy)]
pub(crate) struct ComposeRowTransform<'a> {
    pub(crate) path: ComposeFastPath,
    pub(crate) color_space: HdrColorSpace,
    pub(crate) transfer: HdrTransferFunction,
    pub(crate) metadata: &'a HdrImageMetadata,
    pub(crate) headroom_span: f32,
    pub(crate) weight: f32,
}

pub(crate) const SRGB_LINEAR_SEGMENT_END: f32 = 0.04045;
pub(crate) const SRGB_DIVISOR: f32 = 12.92;
pub(crate) const SRGB_OFFSET: f32 = 0.055;
pub(crate) const SRGB_SCALE: f32 = 1.055;
pub(crate) const SRGB_GAMMA: f32 = 2.4;

pub(crate) const BT709_LINEAR_SEGMENT_BREAK: f32 = 0.018 * 4.5;
pub(crate) const BT709_DIVISOR: f32 = 4.5;
pub(crate) const BT709_OFFSET: f32 = 0.099;
pub(crate) const BT709_SCALE: f32 = 1.099;
pub(crate) const BT709_GAMMA: f32 = 1.0 / 0.45;

pub(crate) const DISPLAY_P3_TO_LINEAR_SRGB: [[f32; 3]; 3] = [
    [1.2249401, -0.2249402, 0.0],
    [-0.0420569, 1.0420571, 0.0],
    [-0.0196376, -0.0786507, 1.0982884],
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ComposeFastPath {
    SrgbLinearSrgb,
    SrgbDisplayP3,
    Bt709LinearSrgb,
    Bt709DisplayP3,
    LinearLinearSrgb,
    LinearDisplayP3,
    Scalar,
}

pub(crate) struct GainRowLinear {
    /// Encoded bilinear samples for one row (reused each row).
    pub(crate) encoded: Vec<f32>,
    pub(crate) rgb: Vec<f32>,
}

impl GainRowLinear {
    pub(crate) fn ensure_capacity(&mut self, width: usize) {
        let needed = width * 3;
        self.encoded.resize(needed, 0.0);
        self.rgb.resize(needed, 0.0);
    }
}

/// Bilinear upsample four encoded RGB taps (0–1, not BT.709-linear yet).
#[inline]
fn bilinear_rgb_taps(
    c00: [f32; 3],
    c10: [f32; 3],
    c01: [f32; 3],
    c11: [f32; 3],
    tx: f32,
    ty: f32,
) -> [f32; 3] {
    let mut out = [0.0; 3];
    for channel in 0..3 {
        let top = lerp(c00[channel], c10[channel], tx);
        let bottom = lerp(c01[channel], c11[channel], tx);
        out[channel] = lerp(top, bottom, ty);
    }
    out
}

#[inline]
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Read encoded RGB from one gain-map texel (0–1).
#[inline]
fn gain_map_rgb_at_row(row: &[u8], x: u32) -> [f32; 3] {
    let index = x as usize * 4;
    [
        f32::from(row[index]) / 255.0,
        f32::from(row[index + 1]) / 255.0,
        f32::from(row[index + 2]) / 255.0,
    ]
}

/// Bilinear upsample one gain-map row to primary width (encoded 0–1, not BT.709-linear yet).
///
/// **Keep in sync** with [`gain_map_bilinear_coords`](crate::hdr::gain_map::gain_map_bilinear_coords):
/// the per-pixel `gx`/`gy` → `(x0, x1, y0, y1, tx, ty)` mapping must match exactly
/// (row path hoists `y0`/`y1`/`ty` once per row). `precompute_gain_row_matches_legacy_reference` catches drift.
fn sample_gain_map_row_nonlinear(
    gain_rgba: &[u8],
    gain_w: u32,
    gain_h: u32,
    y: u32,
    width: u32,
    height: u32,
    out: &mut [f32],
) {
    if gain_w == 0 || gain_h == 0 || width == 0 || height == 0 {
        return;
    }

    let gy = ((y as f32 + 0.5) * gain_h as f32 / height as f32 - 0.5)
        .clamp(0.0, gain_h.saturating_sub(1) as f32);
    let y0 = gy.floor() as u32;
    let y1 = (y0 + 1).min(gain_h - 1);
    let ty = gy - y0 as f32;
    let row_stride = gain_w as usize * 4;
    let row0 = &gain_rgba[y0 as usize * row_stride..][..row_stride];
    let row1 = &gain_rgba[y1 as usize * row_stride..][..row_stride];

    let mut cache_x0 = u32::MAX;
    let mut c00 = [0.0; 3];
    let mut c10 = [0.0; 3];
    let mut c01 = [0.0; 3];
    let mut c11 = [0.0; 3];

    for x in 0..width {
        let gx = ((x as f32 + 0.5) * gain_w as f32 / width as f32 - 0.5)
            .clamp(0.0, gain_w.saturating_sub(1) as f32);
        let x0 = gx.floor() as u32;
        let tx = gx - x0 as f32;

        if x0 != cache_x0 {
            let x1 = (x0 + 1).min(gain_w - 1);
            c00 = gain_map_rgb_at_row(row0, x0);
            c10 = gain_map_rgb_at_row(row0, x1);
            c01 = gain_map_rgb_at_row(row1, x0);
            c11 = gain_map_rgb_at_row(row1, x1);
            cache_x0 = x0;
        }

        let sampled = bilinear_rgb_taps(c00, c10, c01, c11, tx, ty);
        let base = x as usize * 3;
        out[base..base + 3].copy_from_slice(&sampled);
    }
}

pub(crate) fn precompute_gain_row_linear(
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
    sample_gain_map_row_nonlinear(
        gain_rgba,
        gain_w,
        gain_h,
        y,
        width,
        height,
        &mut out.encoded,
    );
    bt709_linearize_gain_row(&out.encoded, &mut out.rgb, width);
}

pub(crate) fn classify_fast_path(
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

pub(crate) fn compose_pixel_scalar(
    row_in: &[f32],
    row_out: &mut [f32],
    x: u32,
    gain_rgb: &[f32],
    transform: ComposeRowTransform<'_>,
) {
    let idx = x as usize * 4;
    let r_code = row_in[idx];
    let g_code = row_in[idx + 1];
    let b_code = row_in[idx + 2];
    let a = row_in[idx + 3];

    let rgb_display_linear = decode_transfer_to_display_linear(
        [r_code, g_code, b_code],
        transform.transfer,
        crate::hdr::types::DEFAULT_SDR_WHITE_NITS,
    );
    let rgb_linear_srgb = linear_primary_to_linear_srgb(
        rgb_display_linear,
        transform.color_space,
        transform.metadata,
    );

    let gain_base = x as usize * 3;
    let gain_linear = [
        gain_rgb[gain_base],
        gain_rgb[gain_base + 1],
        gain_rgb[gain_base + 2],
    ];

    row_out[idx] = (rgb_linear_srgb[0]
        * (1.0 + transform.headroom_span * gain_linear[0] * transform.weight))
        .max(0.0);
    row_out[idx + 1] = (rgb_linear_srgb[1]
        * (1.0 + transform.headroom_span * gain_linear[1] * transform.weight))
        .max(0.0);
    row_out[idx + 2] = (rgb_linear_srgb[2]
        * (1.0 + transform.headroom_span * gain_linear[2] * transform.weight))
        .max(0.0);
    row_out[idx + 3] = a;
}

pub(crate) fn compose_row_scalar(
    row_in: &[f32],
    row_out: &mut [f32],
    width: u32,
    gain_rgb: &[f32],
    transform: ComposeRowTransform<'_>,
) {
    for x in 0..width {
        compose_pixel_scalar(row_in, row_out, x, gain_rgb, transform);
    }
}

pub(crate) fn path_applies_display_p3_matrix(path: ComposeFastPath) -> bool {
    matches!(
        path,
        ComposeFastPath::SrgbDisplayP3
            | ComposeFastPath::Bt709DisplayP3
            | ComposeFastPath::LinearDisplayP3
    )
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

/// SSE4.1 `_MM_SHUFFLE(z,y,x,w)` => out[0]=a[w], out[1]=a[x], out[2]=b[y], out[3]=b[z]
#[cfg(target_arch = "x86_64")]
const SHUF_SSE_ALL_LANE0: i32 = 0x00;
#[cfg(target_arch = "x86_64")]
const SHUF_SSE_ALL_LANE1: i32 = 0x55;
#[cfg(target_arch = "x86_64")]
const SHUF_SSE_ALL_LANE2: i32 = 0xAA;
#[cfg(target_arch = "x86_64")]
const SHUF_SSE_ALL_LANE3: i32 = 0xFF;

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
#[cfg(target_arch = "x86_64")]
pub(crate) unsafe fn load_rgb_interleaved4_sse41(src: *const f32) -> (__m128, __m128, __m128) {
    unsafe {
        let v0 = _mm_loadu_ps(src);
        let v1 = _mm_loadu_ps(src.add(4));
        let v2 = _mm_loadu_ps(src.add(8));

        // v0=[r0,g0,b0,r1] v1=[g1,b1,r2,g2] v2=[b2,r3,g3,b3]
        let r = _mm_blend_ps(
            _mm_shuffle_ps(v0, v1, 0x2C), // (0,2,3,0) => [r0,r1,r2,g1]
            _mm_shuffle_ps(v2, v2, SHUF_SSE_ALL_LANE1),
            0b1000,
        );
        let g_partial = _mm_shuffle_ps(v0, v1, 0xC1); // (3,0,0,1) => [g0,r0,g1,g2]
        let g = _mm_blend_ps(
            _mm_shuffle_ps(g_partial, g_partial, 0xB8), // (2,3,2,0) => [g0,g1,g2,g1]
            _mm_shuffle_ps(v2, v2, SHUF_SSE_ALL_LANE2),
            0b1000,
        );
        let b_partial = _mm_shuffle_ps(v0, v1, 0x56); // (1,1,1,2) => [b0,g0,b1,b1]
        let b_reordered = _mm_shuffle_ps(b_partial, b_partial, 0xB8); // (2,3,2,0) => [b0,b1,b1,b1]
        let b = _mm_blend_ps(
            _mm_blend_ps(
                b_reordered,
                _mm_shuffle_ps(v2, v2, SHUF_SSE_ALL_LANE0),
                0b0100,
            ),
            _mm_shuffle_ps(v2, v2, SHUF_SSE_ALL_LANE3),
            0b1000,
        );
        (r, g, b)
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
#[cfg(target_arch = "x86_64")]
pub(crate) unsafe fn store_rgb_interleaved4_sse41(dst: *mut f32, r: __m128, g: __m128, b: __m128) {
    unsafe {
        let rg_lo = _mm_unpacklo_ps(r, g);
        let v0 = _mm_blend_ps(
            _mm_blend_ps(rg_lo, _mm_shuffle_ps(b, b, SHUF_SSE_ALL_LANE0), 0b0100),
            _mm_shuffle_ps(r, r, SHUF_SSE_ALL_LANE1),
            0b1000,
        );
        let gb_lo = _mm_unpacklo_ps(
            _mm_shuffle_ps(g, g, SHUF_SSE_ALL_LANE1),
            _mm_shuffle_ps(b, b, SHUF_SSE_ALL_LANE1),
        );
        let v1 = _mm_blend_ps(
            _mm_blend_ps(gb_lo, _mm_shuffle_ps(r, r, SHUF_SSE_ALL_LANE2), 0b0100),
            _mm_shuffle_ps(g, g, SHUF_SSE_ALL_LANE2),
            0b1000,
        );
        let br_lo = _mm_unpacklo_ps(
            _mm_shuffle_ps(b, b, SHUF_SSE_ALL_LANE2),
            _mm_shuffle_ps(r, r, SHUF_SSE_ALL_LANE3),
        );
        let v2 = _mm_blend_ps(
            _mm_blend_ps(br_lo, _mm_shuffle_ps(g, g, SHUF_SSE_ALL_LANE3), 0b0100),
            _mm_shuffle_ps(b, b, SHUF_SSE_ALL_LANE3),
            0b1000,
        );
        _mm_storeu_ps(dst, v0);
        _mm_storeu_ps(dst.add(4), v1);
        _mm_storeu_ps(dst.add(8), v2);
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn gather_gain_rgb4_sse41(
    gain_rgb: *const f32,
    pixel_offset: usize,
) -> (__m128, __m128, __m128) {
    unsafe { load_rgb_interleaved4_sse41(gain_rgb.add(pixel_offset * 3)) }
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
struct Sse41Gain4 {
    linear_r: __m128,
    linear_g: __m128,
    linear_b: __m128,
    alpha: __m128,
    gain_r: __m128,
    gain_g: __m128,
    gain_b: __m128,
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn compose_gain4_sse41(
    inputs: Sse41Gain4,
    transform: ComposeRowTransform<'_>,
) -> (__m128, __m128, __m128, __m128) {
    let Sse41Gain4 {
        linear_r,
        linear_g,
        linear_b,
        alpha,
        gain_r,
        gain_g,
        gain_b,
    } = inputs;
    let one = _mm_set1_ps(1.0);
    let span = _mm_set1_ps(transform.headroom_span);
    let w = _mm_set1_ps(transform.weight);
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
pub(crate) unsafe fn compose_row_sse41(
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
        while x + SIMD_PIXELS_PER_STEP <= width {
            let offset = x as usize * 4;
            let (r, g, b, a) = load_rgba_pixel4_sse41(in_ptr, offset);
            let (mut lr, mut lg, mut lb) = apply_transfer4_sse41(r, g, b, transform.path);
            if path_applies_display_p3_matrix(transform.path) {
                (lr, lg, lb) = apply_display_p3_matrix4_sse41(lr, lg, lb);
            }
            let (gain_r, gain_g, gain_b) = gather_gain_rgb4_sse41(gain_ptr, x as usize);
            let (out_r, out_g, out_b, out_a) = compose_gain4_sse41(
                Sse41Gain4 {
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
            store_rgba_pixel4_sse41(out_ptr, offset, out_r, out_g, out_b, out_a);
            x += SIMD_PIXELS_PER_STEP;
        }
        while x < width {
            compose_pixel_scalar(row_in, row_out, x, gain_rgb, transform);
            x += 1;
        }
    }
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
    let high = unsafe { pow4_neon(adjusted, SRGB_GAMMA) };
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
    let high = unsafe { pow4_neon(adjusted, BT709_GAMMA) };
    vbslq_f32(low_mask, low, high)
}

fn bt709_linearize_gain_row(nonlinear: &[f32], out: &mut [f32], width: u32) {
    let expected = width as usize * 3;
    if nonlinear.len() != expected || out.len() < expected {
        return;
    }

    let mut x = 0_u32;
    #[cfg(target_arch = "x86_64")]
    {
        if std::arch::is_x86_feature_detected!("sse4.1") {
            unsafe {
                bt709_linearize_gain_row_sse41(nonlinear.as_ptr(), out.as_mut_ptr(), width, &mut x);
            }
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        unsafe {
            bt709_linearize_gain_row_neon(nonlinear.as_ptr(), out.as_mut_ptr(), width, &mut x);
        }
    }
    while x < width {
        let base = x as usize * 3;
        out[base] = bt709_nonlinear_channel_to_linear(nonlinear[base]);
        out[base + 1] = bt709_nonlinear_channel_to_linear(nonlinear[base + 1]);
        out[base + 2] = bt709_nonlinear_channel_to_linear(nonlinear[base + 2]);
        x += 1;
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn store_rgb_pixel4_interleaved_sse41(
    dst: *mut f32,
    pixel_offset: usize,
    r: __m128,
    g: __m128,
    b: __m128,
) {
    unsafe {
        store_rgb_interleaved4_sse41(dst.add(pixel_offset * 3), r, g, b);
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn bt709_linearize_gain_row_sse41(src: *const f32, dst: *mut f32, width: u32, x: &mut u32) {
    unsafe {
        while *x + SIMD_PIXELS_PER_STEP <= width {
            let (r, g, b) = gather_gain_rgb4_sse41(src, *x as usize);
            let lr = bt709_to_linear4_sse41(r);
            let lg = bt709_to_linear4_sse41(g);
            let lb = bt709_to_linear4_sse41(b);
            store_rgb_pixel4_interleaved_sse41(dst, *x as usize, lr, lg, lb);
            *x += SIMD_PIXELS_PER_STEP;
        }
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn bt709_linearize_gain_row_neon(src: *const f32, dst: *mut f32, width: u32, x: &mut u32) {
    unsafe {
        while *x + SIMD_PIXELS_PER_STEP <= width {
            let offset = *x as usize * 3;
            let encoded = vld3q_f32(src.add(offset));
            let linear = float32x4x3_t(
                bt709_to_linear4_neon(encoded.0),
                bt709_to_linear4_neon(encoded.1),
                bt709_to_linear4_neon(encoded.2),
            );
            vst3q_f32(dst.add(offset), linear);
            *x += SIMD_PIXELS_PER_STEP;
        }
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn load_rgba_pixel4_neon(
    row: *const f32,
    pixel_offset: usize,
) -> (float32x4_t, float32x4_t, float32x4_t, float32x4_t) {
    unsafe {
        let res = vld4q_f32(row.add(pixel_offset));
        (res.0, res.1, res.2, res.3)
    }
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
    unsafe {
        vst4q_f32(row.add(pixel_offset), float32x4x4_t(r, g, b, a));
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn gather_gain_rgb4_neon(
    gain_rgb: *const f32,
    pixel_offset: usize,
) -> (float32x4_t, float32x4_t, float32x4_t) {
    unsafe {
        let res = vld3q_f32(gain_rgb.add(pixel_offset * 3));
        (res.0, res.1, res.2)
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn apply_display_p3_matrix4_neon(
    r: float32x4_t,
    g: float32x4_t,
    b: float32x4_t,
) -> (float32x4_t, float32x4_t, float32x4_t) {
    let m = DISPLAY_P3_TO_LINEAR_SRGB;
    let lr = vfmaq_f32(
        vfmaq_f32(
            vmulq_f32(r, vdupq_n_f32(m[0][0])),
            g, vdupq_n_f32(m[0][1]),
        ),
        b, vdupq_n_f32(m[0][2]),
    );
    let lg = vfmaq_f32(
        vfmaq_f32(
            vmulq_f32(r, vdupq_n_f32(m[1][0])),
            g, vdupq_n_f32(m[1][1]),
        ),
        b, vdupq_n_f32(m[1][2]),
    );
    let lb = vfmaq_f32(
        vfmaq_f32(
            vmulq_f32(r, vdupq_n_f32(m[2][0])),
            g, vdupq_n_f32(m[2][1]),
        ),
        b, vdupq_n_f32(m[2][2]),
    );
    (lr, lg, lb)
}

#[cfg(target_arch = "aarch64")]
struct NeonGain4 {
    linear_r: float32x4_t,
    linear_g: float32x4_t,
    linear_b: float32x4_t,
    alpha: float32x4_t,
    gain_r: float32x4_t,
    gain_g: float32x4_t,
    gain_b: float32x4_t,
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn compose_gain4_neon(
    inputs: NeonGain4,
    transform: ComposeRowTransform<'_>,
) -> (float32x4_t, float32x4_t, float32x4_t, float32x4_t) {
    let NeonGain4 {
        linear_r,
        linear_g,
        linear_b,
        alpha,
        gain_r,
        gain_g,
        gain_b,
    } = inputs;
    let one = vdupq_n_f32(1.0);
    let span = vdupq_n_f32(transform.headroom_span);
    let w = vdupq_n_f32(transform.weight);
    let zero = vdupq_n_f32(0.0);
    let scale_r = vfmaq_f32(one, vmulq_f32(gain_r, w), span);
    let scale_g = vfmaq_f32(one, vmulq_f32(gain_g, w), span);
    let scale_b = vfmaq_f32(one, vmulq_f32(gain_b, w), span);
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
pub(crate) unsafe fn compose_row_neon(
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
        while x + SIMD_PIXELS_PER_STEP <= width {
            let offset = x as usize * 4;
            let (r, g, b, a) = load_rgba_pixel4_neon(in_ptr, offset);
            let (mut lr, mut lg, mut lb) = apply_transfer4_neon(r, g, b, transform.path);
            if path_applies_display_p3_matrix(transform.path) {
                (lr, lg, lb) = apply_display_p3_matrix4_neon(lr, lg, lb);
            }
            let (gain_r, gain_g, gain_b) = gather_gain_rgb4_neon(gain_ptr, x as usize);
            let (out_r, out_g, out_b, out_a) = compose_gain4_neon(
                NeonGain4 {
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
            store_rgba_pixel4_neon(out_ptr, offset, out_r, out_g, out_b, out_a);
            x += SIMD_PIXELS_PER_STEP;
        }
        while x < width {
            compose_pixel_scalar(row_in, row_out, x, gain_rgb, transform);
            x += 1;
        }
    }
}
