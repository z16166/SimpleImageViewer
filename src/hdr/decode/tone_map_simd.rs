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

//! SIMD/NEON HDR -> SDR tone-map for directory-tree strip previews (<=128px).
//!
//! Bit-identical to [`super::tone_map::hdr_to_sdr_rgba8_with_tone_settings`] on supported fast paths;
//! rare color/transfer combinations fall back to the scalar loop.

use super::constants::{INVERSE_DISPLAY_GAMMA, MAX_HDR_TONE_MAP_INPUT};
use super::tone_map::{
    STRIP_PREVIEW_NITS_PIN_EPSILON, decode_transfer_to_display_linear,
    encode_linear_display_referred_srgb8, encode_sdr_rgb8,
    hdr_to_sdr_rgba8_with_tone_settings_scalar, should_use_iec61966_tone_map_fallback,
};
#[cfg(target_arch = "aarch64")]
use crate::hdr::simd_fast_pow::{exp4_neon, pow4_neon};
#[cfg(target_arch = "x86_64")]
use crate::hdr::simd_fast_pow::{exp4_sse41, pow4_sse41};
use crate::hdr::types::{
    HdrColorProfile, HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrToneMapSettings,
    HdrTransferFunction,
};

#[cfg(target_arch = "aarch64")]
use core::arch::aarch64::*;
#[cfg(target_arch = "x86_64")]
use core::arch::x86_64::*;

const PIXELS_PER_SIMD_STEP: usize = 4;

const SRGB_OFFSET: f32 = 0.055;
const SRGB_ENCODE_LINEAR_BREAK: f32 = 0.0031308;
const SRGB_ENCODE_SLOPE: f32 = 12.92;
const SRGB_ENCODE_SCALE: f32 = 1.055;
const SRGB_ENCODE_GAMMA: f32 = 1.0 / 2.4;

const BT709_LINEAR_SEGMENT_BREAK: f32 = 0.018 * 4.5;
const BT709_DIVISOR: f32 = 4.5;
const BT709_OFFSET: f32 = 0.099;
const BT709_SCALE: f32 = 1.099;
const BT709_GAMMA: f32 = 1.0 / 0.45;

const HLG_A: f32 = 0.17883277;
const HLG_B: f32 = 0.28466892;
#[allow(clippy::excessive_precision)]
const HLG_C: f32 = 0.55991073;

const REC2020_TO_LINEAR_SRGB: [[f32; 3]; 3] = [
    [1.6605, -0.5876, -0.0728],
    [-0.1246, 1.1329, -0.0083],
    [-0.0182, -0.1006, 1.1187],
];

const DISPLAY_P3_TO_LINEAR_SRGB: [[f32; 3]; 3] = [
    [1.2249401, -0.2249402, 0.0],
    [-0.0420569, 1.0420571, 0.0],
    [-0.0196376, -0.0786507, 1.0982884],
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StripSimdPath {
    ReinhardPqRec2020,
    ReinhardPqDisplayP3,
    ReinhardPqLinearSrgb,
    ReinhardHlgRec2020,
    ReinhardHlgDisplayP3,
    ReinhardHlgLinearSrgb,
    ReinhardBt709Rec2020,
    ReinhardBt709DisplayP3,
    ReinhardBt709LinearSrgb,
    ReinhardLinearRec2020,
    ReinhardLinearDisplayP3,
    ReinhardLinearLinearSrgb,
    Iec61966SrgbLinearSrgb,
    Scalar,
}

#[derive(Clone, Copy)]
struct StripToneMapContext {
    exposure_scale: f32,
    peak_scale: f32,
    combined_scale: f32,
    sdr_white_nits: f32,
    path: StripSimdPath,
}

/// CPU HDR -> SDR tone-map: SIMD fast paths when possible, scalar otherwise.
pub fn hdr_to_sdr_rgba8_with_tone_settings(
    buffer: &HdrImageBuffer,
    exposure_ev: f32,
    tone: &HdrToneMapSettings,
) -> Result<Vec<u8>, String> {
    let expected_len = buffer
        .width
        .checked_mul(buffer.height)
        .and_then(|pixels| pixels.checked_mul(4))
        .map(|len| len as usize)
        .ok_or_else(|| {
            format!(
                "HDR buffer dimensions overflow: {}x{}",
                buffer.width, buffer.height
            )
        })?;

    if buffer.rgba_f32.len() != expected_len {
        return Err(format!(
            "Malformed HDR buffer: expected {} floats for {}x{} RGBA, got {}",
            expected_len,
            buffer.width,
            buffer.height,
            buffer.rgba_f32.len()
        ));
    }

    let mut tone = *tone;
    let strip_preview_pinned =
        tone.max_display_nits <= tone.sdr_white_nits + STRIP_PREVIEW_NITS_PIN_EPSILON;
    if !strip_preview_pinned
        && let Some(max) = buffer.metadata.luminance.mastering_max_nits
        && max.is_finite()
        && max > tone.sdr_white_nits
    {
        tone.max_display_nits = tone.max_display_nits.max(max);
    }

    let tf = buffer.metadata.transfer_function;
    let path = classify_strip_simd_path(buffer, tf);
    if path == StripSimdPath::Scalar {
        return hdr_to_sdr_rgba8_with_tone_settings_scalar(buffer, exposure_ev, &tone);
    }

    let apply_peak_scaler = matches!(tf, HdrTransferFunction::Pq | HdrTransferFunction::Hlg);
    let exposure_scale = 2.0_f32.powf(exposure_ev);
    let peak_scale = if apply_peak_scaler {
        tone.sdr_white_nits / tone.max_display_nits.max(tone.sdr_white_nits)
    } else {
        1.0
    };
    let ctx = StripToneMapContext {
        exposure_scale,
        peak_scale,
        combined_scale: exposure_scale * peak_scale,
        sdr_white_nits: tone.sdr_white_nits,
        path,
    };

    let mut pixels = vec![0_u8; expected_len];
    tone_map_strip_simd(&buffer.rgba_f32, &mut pixels, ctx);
    Ok(pixels)
}

/// Directory-tree strip tone-map (same SIMD path as full-image CPU tone-map).
pub(crate) fn hdr_to_sdr_rgba8_strip_preview(
    buffer: &HdrImageBuffer,
    exposure_ev: f32,
    tone: &HdrToneMapSettings,
) -> Result<Vec<u8>, String> {
    hdr_to_sdr_rgba8_with_tone_settings(buffer, exposure_ev, tone)
}

fn classify_strip_simd_path(buffer: &HdrImageBuffer, tf: HdrTransferFunction) -> StripSimdPath {
    if should_use_iec61966_tone_map_fallback(buffer, tf) {
        return match buffer.color_space {
            HdrColorSpace::LinearSrgb | HdrColorSpace::LinearScRgb => {
                StripSimdPath::Iec61966SrgbLinearSrgb
            }
            _ => StripSimdPath::Scalar,
        };
    }

    let cs = effective_strip_color_space(&buffer.metadata, buffer.color_space);
    match (tf, cs) {
        (HdrTransferFunction::Pq, HdrColorSpace::Rec2020Linear) => StripSimdPath::ReinhardPqRec2020,
        (HdrTransferFunction::Pq, HdrColorSpace::DisplayP3Linear) => {
            StripSimdPath::ReinhardPqDisplayP3
        }
        (HdrTransferFunction::Pq, HdrColorSpace::LinearSrgb | HdrColorSpace::LinearScRgb) => {
            StripSimdPath::ReinhardPqLinearSrgb
        }
        (HdrTransferFunction::Hlg, HdrColorSpace::Rec2020Linear) => {
            StripSimdPath::ReinhardHlgRec2020
        }
        (HdrTransferFunction::Hlg, HdrColorSpace::DisplayP3Linear) => {
            StripSimdPath::ReinhardHlgDisplayP3
        }
        (HdrTransferFunction::Hlg, HdrColorSpace::LinearSrgb | HdrColorSpace::LinearScRgb) => {
            StripSimdPath::ReinhardHlgLinearSrgb
        }
        (HdrTransferFunction::Bt709, HdrColorSpace::Rec2020Linear) => {
            StripSimdPath::ReinhardBt709Rec2020
        }
        (HdrTransferFunction::Bt709, HdrColorSpace::DisplayP3Linear) => {
            StripSimdPath::ReinhardBt709DisplayP3
        }
        (HdrTransferFunction::Bt709, HdrColorSpace::LinearSrgb | HdrColorSpace::LinearScRgb) => {
            StripSimdPath::ReinhardBt709LinearSrgb
        }
        (HdrTransferFunction::Linear, HdrColorSpace::Rec2020Linear) => {
            StripSimdPath::ReinhardLinearRec2020
        }
        (HdrTransferFunction::Linear, HdrColorSpace::DisplayP3Linear) => {
            StripSimdPath::ReinhardLinearDisplayP3
        }
        (HdrTransferFunction::Linear, HdrColorSpace::LinearSrgb | HdrColorSpace::LinearScRgb) => {
            StripSimdPath::ReinhardLinearLinearSrgb
        }
        (HdrTransferFunction::Srgb, HdrColorSpace::LinearSrgb | HdrColorSpace::LinearScRgb) => {
            StripSimdPath::Scalar
        }
        _ => StripSimdPath::Scalar,
    }
}

fn effective_strip_color_space(
    meta: &HdrImageMetadata,
    color_space: HdrColorSpace,
) -> HdrColorSpace {
    match color_space {
        HdrColorSpace::Unknown => {
            if matches!(
                meta.color_profile,
                HdrColorProfile::Cicp {
                    color_primaries: 9,
                    ..
                }
            ) {
                HdrColorSpace::Rec2020Linear
            } else if matches!(
                meta.color_profile,
                HdrColorProfile::Cicp {
                    color_primaries: 11,
                    ..
                }
            ) {
                HdrColorSpace::DisplayP3Linear
            } else {
                HdrColorSpace::Unknown
            }
        }
        other => other,
    }
}

fn tone_map_strip_simd(src: &[f32], dst: &mut [u8], ctx: StripToneMapContext) {
    debug_assert_eq!(src.len(), dst.len());
    let pixel_count = src.len() / 4;
    let mut offset = 0_usize;
    #[cfg(target_arch = "x86_64")]
    {
        if std::arch::is_x86_feature_detected!("sse4.1") {
            unsafe {
                tone_map_strip_simd_sse41(src, dst, pixel_count, ctx, &mut offset);
            }
            tone_map_strip_scalar_tail(src, dst, pixel_count, ctx, offset);
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        unsafe {
            tone_map_strip_simd_neon(src, dst, pixel_count, ctx, &mut offset);
        }
        tone_map_strip_scalar_tail(src, dst, pixel_count, ctx, offset);
        return;
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        let _ = offset;
        tone_map_strip_scalar_tail(src, dst, pixel_count, ctx, 0);
    }
}

fn tone_map_strip_scalar_tail(
    src: &[f32],
    dst: &mut [u8],
    pixel_count: usize,
    ctx: StripToneMapContext,
    start_pixel: usize,
) {
    for pixel_idx in start_pixel..pixel_count {
        let base = pixel_idx * 4;
        let pixel = &src[base..base + 4];
        let rgb_in = [pixel[0], pixel[1], pixel[2]];
        let tf = transfer_for_path(ctx.path);
        let decoded = decode_transfer_to_display_linear(rgb_in, tf, ctx.sdr_white_nits);
        let linear_srgb = apply_color_matrix_scalar(decoded, ctx.path);
        let encoded = match ctx.path {
            StripSimdPath::Iec61966SrgbLinearSrgb => encode_linear_display_referred_srgb8(
                linear_srgb,
                ctx.exposure_scale,
                ctx.peak_scale,
            ),
            _ => encode_sdr_rgb8(linear_srgb, ctx.exposure_scale, ctx.peak_scale),
        };
        dst[base..base + 4].copy_from_slice(&[
            encoded[0],
            encoded[1],
            encoded[2],
            float_to_u8_scalar(pixel[3]),
        ]);
    }
}

fn transfer_for_path(path: StripSimdPath) -> HdrTransferFunction {
    match path {
        StripSimdPath::ReinhardPqRec2020
        | StripSimdPath::ReinhardPqDisplayP3
        | StripSimdPath::ReinhardPqLinearSrgb => HdrTransferFunction::Pq,
        StripSimdPath::ReinhardHlgRec2020
        | StripSimdPath::ReinhardHlgDisplayP3
        | StripSimdPath::ReinhardHlgLinearSrgb => HdrTransferFunction::Hlg,
        StripSimdPath::ReinhardBt709Rec2020
        | StripSimdPath::ReinhardBt709DisplayP3
        | StripSimdPath::ReinhardBt709LinearSrgb => HdrTransferFunction::Bt709,
        StripSimdPath::ReinhardLinearRec2020
        | StripSimdPath::ReinhardLinearDisplayP3
        | StripSimdPath::ReinhardLinearLinearSrgb
        | StripSimdPath::Iec61966SrgbLinearSrgb => HdrTransferFunction::Linear,
        StripSimdPath::Scalar => HdrTransferFunction::Unknown,
    }
}

fn apply_color_matrix_scalar(rgb: [f32; 3], path: StripSimdPath) -> [f32; 3] {
    let m = match path {
        StripSimdPath::ReinhardPqRec2020
        | StripSimdPath::ReinhardHlgRec2020
        | StripSimdPath::ReinhardBt709Rec2020
        | StripSimdPath::ReinhardLinearRec2020 => &REC2020_TO_LINEAR_SRGB,
        StripSimdPath::ReinhardPqDisplayP3
        | StripSimdPath::ReinhardHlgDisplayP3
        | StripSimdPath::ReinhardBt709DisplayP3
        | StripSimdPath::ReinhardLinearDisplayP3 => &DISPLAY_P3_TO_LINEAR_SRGB,
        _ => return rgb,
    };
    [
        m[0][0] * rgb[0] + m[0][1] * rgb[1] + m[0][2] * rgb[2],
        m[1][0] * rgb[0] + m[1][1] * rgb[1] + m[1][2] * rgb[2],
        m[2][0] * rgb[0] + m[2][1] * rgb[1] + m[2][2] * rgb[2],
    ]
}

fn float_to_u8_scalar(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn tone_map_strip_simd_sse41(
    src: &[f32],
    dst: &mut [u8],
    pixel_count: usize,
    ctx: StripToneMapContext,
    offset: &mut usize,
) {
    unsafe {
        while *offset + PIXELS_PER_SIMD_STEP <= pixel_count {
            let base = *offset * 4;
            let (r, g, b, a) = load_rgba_pixel4_sse41(src.as_ptr(), *offset);
            let (lr, lg, lb) = decode_transfer4_sse41(r, g, b, ctx);
            let (sr, sg, sb) = apply_color_matrix4_sse41(lr, lg, lb, ctx.path);
            store_rgba_u8_pixel4_sse41(dst.as_mut_ptr().add(base), sr, sg, sb, a, ctx);
            *offset += PIXELS_PER_SIMD_STEP;
        }
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn tone_map_strip_simd_neon(
    src: &[f32],
    dst: &mut [u8],
    pixel_count: usize,
    ctx: StripToneMapContext,
    offset: &mut usize,
) {
    unsafe {
        while *offset + PIXELS_PER_SIMD_STEP <= pixel_count {
            let base = *offset * 4;
            let (r, g, b, a) = load_rgba_pixel4_neon(src.as_ptr(), *offset);
            let (lr, lg, lb) = decode_transfer4_neon(r, g, b, ctx);
            let (sr, sg, sb) = apply_color_matrix4_neon(lr, lg, lb, ctx.path);
            store_rgba_u8_pixel4_neon(dst.as_mut_ptr().add(base), sr, sg, sb, a, ctx);
            *offset += PIXELS_PER_SIMD_STEP;
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn load_rgba_pixel4_sse41(
    row: *const f32,
    pixel_offset: usize,
) -> (__m128, __m128, __m128, __m128) {
    unsafe {
        let mut p0 = _mm_loadu_ps(row.add(pixel_offset * 4));
        let mut p1 = _mm_loadu_ps(row.add(pixel_offset * 4 + 4));
        let mut p2 = _mm_loadu_ps(row.add(pixel_offset * 4 + 8));
        let mut p3 = _mm_loadu_ps(row.add(pixel_offset * 4 + 12));
        _MM_TRANSPOSE4_PS(&mut p0, &mut p1, &mut p2, &mut p3);
        (p0, p1, p2, p3)
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn load_rgba_pixel4_neon(
    row: *const f32,
    pixel_offset: usize,
) -> (float32x4_t, float32x4_t, float32x4_t, float32x4_t) {
    unsafe {
        let res = vld4q_f32(row.add(pixel_offset * 4));
        (res.0, res.1, res.2, res.3)
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn decode_transfer4_sse41(
    r: __m128,
    g: __m128,
    b: __m128,
    ctx: StripToneMapContext,
) -> (__m128, __m128, __m128) {
    unsafe {
        match ctx.path {
            StripSimdPath::ReinhardPqRec2020
            | StripSimdPath::ReinhardPqDisplayP3
            | StripSimdPath::ReinhardPqLinearSrgb => (
                pq_to_display_linear4_sse41(r, ctx.sdr_white_nits),
                pq_to_display_linear4_sse41(g, ctx.sdr_white_nits),
                pq_to_display_linear4_sse41(b, ctx.sdr_white_nits),
            ),
            StripSimdPath::ReinhardHlgRec2020
            | StripSimdPath::ReinhardHlgDisplayP3
            | StripSimdPath::ReinhardHlgLinearSrgb => (
                hlg_to_scene_linear4_sse41(r),
                hlg_to_scene_linear4_sse41(g),
                hlg_to_scene_linear4_sse41(b),
            ),
            StripSimdPath::ReinhardBt709Rec2020
            | StripSimdPath::ReinhardBt709DisplayP3
            | StripSimdPath::ReinhardBt709LinearSrgb => (
                bt709_to_linear4_sse41(r),
                bt709_to_linear4_sse41(g),
                bt709_to_linear4_sse41(b),
            ),
            StripSimdPath::ReinhardLinearRec2020
            | StripSimdPath::ReinhardLinearDisplayP3
            | StripSimdPath::ReinhardLinearLinearSrgb
            | StripSimdPath::Iec61966SrgbLinearSrgb => (r, g, b),
            StripSimdPath::Scalar => (r, g, b),
        }
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn decode_transfer4_neon(
    r: float32x4_t,
    g: float32x4_t,
    b: float32x4_t,
    ctx: StripToneMapContext,
) -> (float32x4_t, float32x4_t, float32x4_t) {
    unsafe {
        match ctx.path {
            StripSimdPath::ReinhardPqRec2020
            | StripSimdPath::ReinhardPqDisplayP3
            | StripSimdPath::ReinhardPqLinearSrgb => (
                pq_to_display_linear4_neon(r, ctx.sdr_white_nits),
                pq_to_display_linear4_neon(g, ctx.sdr_white_nits),
                pq_to_display_linear4_neon(b, ctx.sdr_white_nits),
            ),
            StripSimdPath::ReinhardHlgRec2020
            | StripSimdPath::ReinhardHlgDisplayP3
            | StripSimdPath::ReinhardHlgLinearSrgb => (
                hlg_to_scene_linear4_neon(r),
                hlg_to_scene_linear4_neon(g),
                hlg_to_scene_linear4_neon(b),
            ),
            StripSimdPath::ReinhardBt709Rec2020
            | StripSimdPath::ReinhardBt709DisplayP3
            | StripSimdPath::ReinhardBt709LinearSrgb => (
                bt709_to_linear4_neon(r),
                bt709_to_linear4_neon(g),
                bt709_to_linear4_neon(b),
            ),
            StripSimdPath::ReinhardLinearRec2020
            | StripSimdPath::ReinhardLinearDisplayP3
            | StripSimdPath::ReinhardLinearLinearSrgb
            | StripSimdPath::Iec61966SrgbLinearSrgb => (r, g, b),
            StripSimdPath::Scalar => (r, g, b),
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn apply_color_matrix4_sse41(
    r: __m128,
    g: __m128,
    b: __m128,
    path: StripSimdPath,
) -> (__m128, __m128, __m128) {
    unsafe {
        match path {
            StripSimdPath::ReinhardPqRec2020
            | StripSimdPath::ReinhardHlgRec2020
            | StripSimdPath::ReinhardBt709Rec2020
            | StripSimdPath::ReinhardLinearRec2020 => {
                apply_matrix4_sse41(r, g, b, &REC2020_TO_LINEAR_SRGB)
            }
            StripSimdPath::ReinhardPqDisplayP3
            | StripSimdPath::ReinhardHlgDisplayP3
            | StripSimdPath::ReinhardBt709DisplayP3
            | StripSimdPath::ReinhardLinearDisplayP3 => {
                apply_matrix4_sse41(r, g, b, &DISPLAY_P3_TO_LINEAR_SRGB)
            }
            _ => (r, g, b),
        }
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn apply_color_matrix4_neon(
    r: float32x4_t,
    g: float32x4_t,
    b: float32x4_t,
    path: StripSimdPath,
) -> (float32x4_t, float32x4_t, float32x4_t) {
    unsafe {
        match path {
            StripSimdPath::ReinhardPqRec2020
            | StripSimdPath::ReinhardHlgRec2020
            | StripSimdPath::ReinhardBt709Rec2020
            | StripSimdPath::ReinhardLinearRec2020 => {
                apply_matrix4_neon(r, g, b, &REC2020_TO_LINEAR_SRGB)
            }
            StripSimdPath::ReinhardPqDisplayP3
            | StripSimdPath::ReinhardHlgDisplayP3
            | StripSimdPath::ReinhardBt709DisplayP3
            | StripSimdPath::ReinhardLinearDisplayP3 => {
                apply_matrix4_neon(r, g, b, &DISPLAY_P3_TO_LINEAR_SRGB)
            }
            _ => (r, g, b),
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn apply_matrix4_sse41(
    r: __m128,
    g: __m128,
    b: __m128,
    m: &[[f32; 3]; 3],
) -> (__m128, __m128, __m128) {
    let sr = _mm_add_ps(
        _mm_add_ps(
            _mm_mul_ps(r, _mm_set1_ps(m[0][0])),
            _mm_mul_ps(g, _mm_set1_ps(m[0][1])),
        ),
        _mm_mul_ps(b, _mm_set1_ps(m[0][2])),
    );
    let sg = _mm_add_ps(
        _mm_add_ps(
            _mm_mul_ps(r, _mm_set1_ps(m[1][0])),
            _mm_mul_ps(g, _mm_set1_ps(m[1][1])),
        ),
        _mm_mul_ps(b, _mm_set1_ps(m[1][2])),
    );
    let sb = _mm_add_ps(
        _mm_add_ps(
            _mm_mul_ps(r, _mm_set1_ps(m[2][0])),
            _mm_mul_ps(g, _mm_set1_ps(m[2][1])),
        ),
        _mm_mul_ps(b, _mm_set1_ps(m[2][2])),
    );
    (sr, sg, sb)
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn apply_matrix4_neon(
    r: float32x4_t,
    g: float32x4_t,
    b: float32x4_t,
    m: &[[f32; 3]; 3],
) -> (float32x4_t, float32x4_t, float32x4_t) {
    let sr = vaddq_f32(
        vaddq_f32(
            vmulq_f32(r, vdupq_n_f32(m[0][0])),
            vmulq_f32(g, vdupq_n_f32(m[0][1])),
        ),
        vmulq_f32(b, vdupq_n_f32(m[0][2])),
    );
    let sg = vaddq_f32(
        vaddq_f32(
            vmulq_f32(r, vdupq_n_f32(m[1][0])),
            vmulq_f32(g, vdupq_n_f32(m[1][1])),
        ),
        vmulq_f32(b, vdupq_n_f32(m[1][2])),
    );
    let sb = vaddq_f32(
        vaddq_f32(
            vmulq_f32(r, vdupq_n_f32(m[2][0])),
            vmulq_f32(g, vdupq_n_f32(m[2][1])),
        ),
        vmulq_f32(b, vdupq_n_f32(m[2][2])),
    );
    (sr, sg, sb)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn store_rgba_u8_pixel4_sse41(
    dst: *mut u8,
    r: __m128,
    g: __m128,
    b: __m128,
    a: __m128,
    ctx: StripToneMapContext,
) {
    unsafe {
        let (er, eg, eb) = if ctx.path == StripSimdPath::Iec61966SrgbLinearSrgb {
            (
                encode_iec61966_channel4_sse41(r, ctx.combined_scale),
                encode_iec61966_channel4_sse41(g, ctx.combined_scale),
                encode_iec61966_channel4_sse41(b, ctx.combined_scale),
            )
        } else {
            (
                encode_reinhard_channel4_sse41(r, ctx.combined_scale),
                encode_reinhard_channel4_sse41(g, ctx.combined_scale),
                encode_reinhard_channel4_sse41(b, ctx.combined_scale),
            )
        };
        pack_rgba_u8_pixel4_sse41(dst, er, eg, eb, a);
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn store_rgba_u8_pixel4_neon(
    dst: *mut u8,
    r: float32x4_t,
    g: float32x4_t,
    b: float32x4_t,
    a: float32x4_t,
    ctx: StripToneMapContext,
) {
    unsafe {
        let (er, eg, eb) = if ctx.path == StripSimdPath::Iec61966SrgbLinearSrgb {
            (
                encode_iec61966_channel4_neon(r, ctx.combined_scale),
                encode_iec61966_channel4_neon(g, ctx.combined_scale),
                encode_iec61966_channel4_neon(b, ctx.combined_scale),
            )
        } else {
            (
                encode_reinhard_channel4_neon(r, ctx.combined_scale),
                encode_reinhard_channel4_neon(g, ctx.combined_scale),
                encode_reinhard_channel4_neon(b, ctx.combined_scale),
            )
        };
        pack_rgba_u8_pixel4_neon(dst, er, eg, eb, a);
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn sanitize_hdr_rgb4_sse41(v: __m128) -> __m128 {
    let zero = _mm_setzero_ps();
    let max_input = _mm_set1_ps(MAX_HDR_TONE_MAP_INPUT);
    let sanitized = _mm_max_ps(v, zero);
    _mm_min_ps(sanitized, max_input)
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn sanitize_hdr_rgb4_neon(v: float32x4_t) -> float32x4_t {
    let zero = vdupq_n_f32(0.0);
    let max_input = vdupq_n_f32(MAX_HDR_TONE_MAP_INPUT);
    vminq_f32(vmaxq_f32(v, zero), max_input)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn pq_to_display_linear4_sse41(code: __m128, sdr_white_nits: f32) -> __m128 {
    unsafe {
        let m2 = crate::constants::PQ_M2;
        let m1 = crate::constants::PQ_M1;
        let c1 = _mm_set1_ps(crate::constants::PQ_C1);
        let c2 = _mm_set1_ps(crate::constants::PQ_C2);
        let c3 = _mm_set1_ps(crate::constants::PQ_C3);
        let zero = _mm_setzero_ps();
        let one = _mm_set1_ps(1.0);
        let clamped = _mm_min_ps(_mm_max_ps(code, zero), one);
        let code_m2 = pow4_sse41(clamped, 1.0 / m2);
        let numerator = _mm_max_ps(_mm_sub_ps(code_m2, c1), zero);
        let denominator = _mm_max_ps(
            _mm_sub_ps(c2, _mm_mul_ps(c3, code_m2)),
            _mm_set1_ps(0.000001),
        );
        let ratio = _mm_div_ps(numerator, denominator);
        let nits = _mm_mul_ps(_mm_set1_ps(10_000.0), pow4_sse41(ratio, 1.0 / m1));
        _mm_div_ps(nits, _mm_set1_ps(sdr_white_nits.max(1.0)))
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn pq_to_display_linear4_neon(code: float32x4_t, sdr_white_nits: f32) -> float32x4_t {
    unsafe {
        let m2 = crate::constants::PQ_M2;
        let m1 = crate::constants::PQ_M1;
        let c1 = vdupq_n_f32(crate::constants::PQ_C1);
        let c2 = vdupq_n_f32(crate::constants::PQ_C2);
        let c3 = vdupq_n_f32(crate::constants::PQ_C3);
        let zero = vdupq_n_f32(0.0);
        let one = vdupq_n_f32(1.0);
        let clamped = vminq_f32(vmaxq_f32(code, zero), one);
        let code_m2 = pow4_neon(clamped, 1.0 / m2);
        let numerator = vmaxq_f32(vsubq_f32(code_m2, c1), zero);
        let denominator = vmaxq_f32(vsubq_f32(c2, vmulq_f32(c3, code_m2)), vdupq_n_f32(0.000001));
        let ratio = vdivq_f32(numerator, denominator);
        let nits = vmulq_f32(vdupq_n_f32(10_000.0), pow4_neon(ratio, 1.0 / m1));
        vdivq_f32(nits, vdupq_n_f32(sdr_white_nits.max(1.0)))
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn hlg_to_scene_linear4_sse41(e_prime: __m128) -> __m128 {
    unsafe {
        let zero = _mm_setzero_ps();
        let one = _mm_set1_ps(1.0);
        let clamped = _mm_min_ps(_mm_max_ps(e_prime, zero), one);
        let half = _mm_set1_ps(0.5);
        let low_mask = _mm_cmple_ps(clamped, half);
        let low = _mm_div_ps(_mm_mul_ps(clamped, clamped), _mm_set1_ps(3.0));
        let adjusted = _mm_div_ps(
            _mm_max_ps(_mm_sub_ps(clamped, _mm_set1_ps(HLG_C)), zero),
            _mm_set1_ps(HLG_A),
        );
        let high = _mm_div_ps(
            _mm_add_ps(exp4_sse41(adjusted), _mm_set1_ps(HLG_B)),
            _mm_set1_ps(12.0),
        );
        _mm_blendv_ps(high, low, low_mask)
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn hlg_to_scene_linear4_neon(e_prime: float32x4_t) -> float32x4_t {
    unsafe {
        let zero = vdupq_n_f32(0.0);
        let one = vdupq_n_f32(1.0);
        let clamped = vminq_f32(vmaxq_f32(e_prime, zero), one);
        let half = vdupq_n_f32(0.5);
        let low_mask = vcleq_f32(clamped, half);
        let low = vdivq_f32(vmulq_f32(clamped, clamped), vdupq_n_f32(3.0));
        let adjusted = vdivq_f32(
            vmaxq_f32(vsubq_f32(clamped, vdupq_n_f32(HLG_C)), zero),
            vdupq_n_f32(HLG_A),
        );
        let high = vdivq_f32(
            vaddq_f32(exp4_neon(adjusted), vdupq_n_f32(HLG_B)),
            vdupq_n_f32(12.0),
        );
        vbslq_f32(low_mask, low, high)
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

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn bt709_to_linear4_neon(v: float32x4_t) -> float32x4_t {
    unsafe {
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
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn encode_reinhard_channel4_sse41(linear: __m128, scale: f32) -> __m128 {
    unsafe {
        let exposed = sanitize_hdr_rgb4_sse41(_mm_mul_ps(linear, _mm_set1_ps(scale)));
        let mapped = _mm_div_ps(exposed, _mm_add_ps(_mm_set1_ps(1.0), exposed));
        let encoded = pow4_sse41(mapped, INVERSE_DISPLAY_GAMMA);
        _mm_min_ps(_mm_max_ps(encoded, _mm_setzero_ps()), _mm_set1_ps(1.0))
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn encode_reinhard_channel4_neon(linear: float32x4_t, scale: f32) -> float32x4_t {
    unsafe {
        let exposed = sanitize_hdr_rgb4_neon(vmulq_f32(linear, vdupq_n_f32(scale)));
        let mapped = vdivq_f32(exposed, vaddq_f32(vdupq_n_f32(1.0), exposed));
        let encoded = pow4_neon(mapped, INVERSE_DISPLAY_GAMMA);
        vminq_f32(vmaxq_f32(encoded, vdupq_n_f32(0.0)), vdupq_n_f32(1.0))
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn encode_iec61966_channel4_sse41(linear: __m128, scale: f32) -> __m128 {
    unsafe {
        let scaled = _mm_min_ps(
            _mm_max_ps(_mm_mul_ps(linear, _mm_set1_ps(scale)), _mm_setzero_ps()),
            _mm_set1_ps(1.0),
        );
        let threshold = _mm_set1_ps(SRGB_ENCODE_LINEAR_BREAK);
        let low_mask = _mm_cmple_ps(scaled, threshold);
        let low = _mm_mul_ps(scaled, _mm_set1_ps(SRGB_ENCODE_SLOPE));
        let adjusted = _mm_sub_ps(
            _mm_mul_ps(
                _mm_set1_ps(SRGB_ENCODE_SCALE),
                pow4_sse41(scaled, SRGB_ENCODE_GAMMA),
            ),
            _mm_set1_ps(SRGB_OFFSET),
        );
        _mm_min_ps(
            _mm_max_ps(_mm_blendv_ps(adjusted, low, low_mask), _mm_setzero_ps()),
            _mm_set1_ps(1.0),
        )
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn encode_iec61966_channel4_neon(linear: float32x4_t, scale: f32) -> float32x4_t {
    unsafe {
        let scaled = vminq_f32(
            vmaxq_f32(vmulq_f32(linear, vdupq_n_f32(scale)), vdupq_n_f32(0.0)),
            vdupq_n_f32(1.0),
        );
        let threshold = vdupq_n_f32(SRGB_ENCODE_LINEAR_BREAK);
        let low_mask = vcleq_f32(scaled, threshold);
        let low = vmulq_f32(scaled, vdupq_n_f32(SRGB_ENCODE_SLOPE));
        let adjusted = vsubq_f32(
            vmulq_f32(
                vdupq_n_f32(SRGB_ENCODE_SCALE),
                pow4_neon(scaled, SRGB_ENCODE_GAMMA),
            ),
            vdupq_n_f32(SRGB_OFFSET),
        );
        vminq_f32(
            vmaxq_f32(vbslq_f32(low_mask, low, adjusted), vdupq_n_f32(0.0)),
            vdupq_n_f32(1.0),
        )
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn float4_to_i16x4_sse41(v: __m128) -> __m128i {
    let zero = _mm_setzero_ps();
    let one = _mm_set1_ps(1.0);
    let half = _mm_set1_ps(0.5);
    let clamped = _mm_min_ps(_mm_max_ps(v, zero), one);
    let scaled = _mm_mul_ps(clamped, _mm_set1_ps(255.0));
    let rounded = _mm_cvttps_epi32(_mm_add_ps(scaled, half));
    _mm_packs_epi32(rounded, rounded)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn pack_rgba_u8_pixel4_sse41(dst: *mut u8, r: __m128, g: __m128, b: __m128, a: __m128) {
    unsafe {
        let ri = float4_to_i16x4_sse41(r);
        let gi = float4_to_i16x4_sse41(g);
        let bi = float4_to_i16x4_sse41(b);
        let ai = float4_to_i16x4_sse41(a);

        let rg01 = _mm_unpacklo_epi16(ri, gi);
        let ba01 = _mm_unpacklo_epi16(bi, ai);
        let rgba_i16_01 = _mm_unpacklo_epi32(rg01, ba01);
        let rgba_i16_23 = _mm_unpackhi_epi32(rg01, ba01);
        let rgba01 = _mm_packus_epi16(rgba_i16_01, _mm_setzero_si128());
        let rgba23 = _mm_packus_epi16(rgba_i16_23, _mm_setzero_si128());
        _mm_storel_epi64(dst as *mut __m128i, rgba01);
        _mm_storel_epi64(dst.add(8) as *mut __m128i, rgba23);
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn float4_to_u16x4_neon(v: float32x4_t) -> uint16x4_t {
    let zero = vdupq_n_f32(0.0);
    let one = vdupq_n_f32(1.0);
    let clamped = vminq_f32(vmaxq_f32(v, zero), one);
    let scaled = vmulq_f32(clamped, vdupq_n_f32(255.0));
    let rounded = vrndiq_f32(scaled);
    vmovn_u32(vcvtq_u32_f32(rounded))
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn pack_rgba_u8_pixel4_neon(
    dst: *mut u8,
    r: float32x4_t,
    g: float32x4_t,
    b: float32x4_t,
    a: float32x4_t,
) {
    unsafe {
        let ri = float4_to_u16x4_neon(r);
        let gi = float4_to_u16x4_neon(g);
        let bi = float4_to_u16x4_neon(b);
        let ai = float4_to_u16x4_neon(a);

        let rg01 = vzip1_u16(ri, gi);
        let ba01 = vzip1_u16(bi, ai);
        let rgba01 = vzip1_u32(vreinterpret_u32_u16(rg01), vreinterpret_u32_u16(ba01));
        vst1_u8(dst, vreinterpret_u8_u32(rgba01));

        let rg23 = vzip2_u16(vcombine_u16(ri, vdup_n_u16(0)), vcombine_u16(gi, vdup_n_u16(0)));
        let ba23 = vzip2_u16(vcombine_u16(bi, vdup_n_u16(0)), vcombine_u16(ai, vdup_n_u16(0)));
        let rgba23 = vzip1_u32(
            vreinterpret_u32_u16(vget_low_u16(rg23)),
            vreinterpret_u32_u16(vget_low_u16(ba23)),
        );
        vst1_u8(dst.add(8), vreinterpret_u8_u32(rgba23));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::hdr::types::{
        DEFAULT_SDR_WHITE_NITS, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat,
        HdrToneMapSettings, HdrTransferFunction,
    };

    fn make_buffer(
        width: u32,
        height: u32,
        tf: HdrTransferFunction,
        color_space: HdrColorSpace,
        pixels: Vec<f32>,
    ) -> HdrImageBuffer {
        let mut meta = HdrImageMetadata::from_color_space(color_space);
        meta.transfer_function = tf;
        if color_space == HdrColorSpace::Rec2020Linear {
            meta.color_profile = HdrColorProfile::Cicp {
                color_primaries: 9,
                transfer_characteristics: 16,
                matrix_coefficients: 0,
                full_range: true,
            };
        }
        HdrImageBuffer {
            width,
            height,
            format: HdrPixelFormat::Rgba32Float,
            color_space,
            metadata: meta,
            rgba_f32: Arc::new(pixels),
        }
    }

    fn assert_strip_simd_matches_scalar(buffer: &HdrImageBuffer, tone: &HdrToneMapSettings) {
        let scalar =
            hdr_to_sdr_rgba8_with_tone_settings_scalar(buffer, tone.exposure_ev, tone).expect("scalar");
        let simd = hdr_to_sdr_rgba8_strip_preview(buffer, tone.exposure_ev, tone).expect("simd");
        assert_eq!(scalar, simd, "strip SIMD must match scalar tone-map");
    }

    #[test]
    fn strip_simd_pq_rec2020_matches_scalar() {
        let mut pixels = Vec::new();
        for y in 0..72_u32 {
            for x in 0..128_u32 {
                let t = (x + y) as f32 / 200.0;
                pixels.extend_from_slice(&[0.4 + t * 0.2, 0.35, 0.3, 1.0]);
            }
        }
        let buffer = make_buffer(
            128,
            72,
            HdrTransferFunction::Pq,
            HdrColorSpace::Rec2020Linear,
            pixels,
        );
        let tone = HdrToneMapSettings {
            max_display_nits: DEFAULT_SDR_WHITE_NITS,
            ..HdrToneMapSettings::default()
        };
        assert_strip_simd_matches_scalar(&buffer, &tone);
    }

    #[test]
    fn strip_simd_linear_srgb_matches_scalar() {
        let mut pixels = Vec::new();
        for i in 0..(64 * 48) {
            let v = (i as f32 * 0.01).sin().abs();
            pixels.extend_from_slice(&[v, v * 0.8, v * 0.5, 1.0]);
        }
        let buffer = make_buffer(
            64,
            48,
            HdrTransferFunction::Linear,
            HdrColorSpace::LinearSrgb,
            pixels,
        );
        let tone = HdrToneMapSettings {
            max_display_nits: DEFAULT_SDR_WHITE_NITS,
            ..HdrToneMapSettings::default()
        };
        assert_strip_simd_matches_scalar(&buffer, &tone);
    }

    #[test]
    fn strip_simd_bt709_rec2020_matches_scalar() {
        let pixels: Vec<f32> = (0..32 * 32)
            .flat_map(|i| {
                let v = (i as f32 / 1024.0).clamp(0.0, 1.0);
                [v, v * 0.9, v * 0.7, 1.0]
            })
            .collect();
        let buffer = make_buffer(
            32,
            32,
            HdrTransferFunction::Bt709,
            HdrColorSpace::Rec2020Linear,
            pixels,
        );
        let tone = HdrToneMapSettings {
            max_display_nits: DEFAULT_SDR_WHITE_NITS,
            ..HdrToneMapSettings::default()
        };
        assert_strip_simd_matches_scalar(&buffer, &tone);
    }

    fn pack_rgba_u8_pixel4_scalar(dst: &mut [u8; 16], r: [f32; 4], g: [f32; 4], b: [f32; 4], a: [f32; 4]) {
        for lane in 0..4 {
            dst[lane * 4] = float_to_u8_scalar(r[lane]);
            dst[lane * 4 + 1] = float_to_u8_scalar(g[lane]);
            dst[lane * 4 + 2] = float_to_u8_scalar(b[lane]);
            dst[lane * 4 + 3] = float_to_u8_scalar(a[lane]);
        }
    }

    #[test]
    fn pack_rgba_u8_pixel4_matches_scalar_reference() {
        let r = [0.0_f32, 0.25, 0.5, 1.0];
        let g = [0.1, 0.35, 0.6, 0.9];
        let b = [0.2, 0.45, 0.7, 0.8];
        let a = [1.0_f32; 4];
        let mut expected = [0_u8; 16];
        pack_rgba_u8_pixel4_scalar(&mut expected, r, g, b, a);

        let mut simd = [0_u8; 16];
        #[cfg(target_arch = "x86_64")]
        unsafe {
            if is_x86_feature_detected!("sse4.1") {
                pack_rgba_u8_pixel4_sse41(
                    simd.as_mut_ptr(),
                    _mm_loadu_ps(r.as_ptr()),
                    _mm_loadu_ps(g.as_ptr()),
                    _mm_loadu_ps(b.as_ptr()),
                    _mm_loadu_ps(a.as_ptr()),
                );
                assert_eq!(simd, expected, "SSE4.1 pack must match scalar");
            }
        }
        #[cfg(target_arch = "aarch64")]
        unsafe {
            pack_rgba_u8_pixel4_neon(
                simd.as_mut_ptr(),
                vld1q_f32(r.as_ptr()),
                vld1q_f32(g.as_ptr()),
                vld1q_f32(b.as_ptr()),
                vld1q_f32(a.as_ptr()),
            );
            assert_eq!(simd, expected, "NEON pack must match scalar");
        }
    }
}
