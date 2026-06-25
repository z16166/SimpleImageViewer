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

use super::constants::{
    HDR_FALLBACK_BYTES_PER_PIXEL_WITH_SDR, INVERSE_DISPLAY_GAMMA, MAX_HDR_FALLBACK_PIXELS,
    MAX_HDR_FALLBACK_TOTAL_BYTES, MAX_HDR_TONE_MAP_INPUT,
};

use crate::hdr::types::{
    HdrColorProfile, HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrReference,
    HdrToneMapSettings, HdrTransferFunction,
};
pub fn hdr_to_sdr_rgba8(buffer: &HdrImageBuffer, exposure_ev: f32) -> Result<Vec<u8>, String> {
    let mut tone = HdrToneMapSettings::default();
    if let Some(max) = buffer.metadata.luminance.mastering_max_nits {
        if max.is_finite() && max > tone.sdr_white_nits {
            tone.max_display_nits = max;
        }
    }
    hdr_to_sdr_rgba8_with_tone_settings(buffer, exposure_ev, &tone)
}

/// Same as [`hdr_to_sdr_rgba8`] but uses explicit SDR white / peak display nits
/// (e.g. from user tone-map settings) for PQ/HLG peak scaling. Caller-supplied
/// `max_display_nits` is raised by [`HdrImageMetadata::luminance::mastering_max_nits`]
/// when that hint exceeds it (content peak vs display capability), unless the caller
/// pinned peak to SDR white (directory-tree strip previews).
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
    // Directory-tree strip previews pass `max_display_nits == sdr_white_nits`; do not
    // raise from mastering metadata or PQ thumbnails crush to ~20% luminance.
    let strip_preview_pinned = tone.max_display_nits <= tone.sdr_white_nits + 0.5;
    if !strip_preview_pinned {
        if let Some(max) = buffer.metadata.luminance.mastering_max_nits {
            if max.is_finite() && max > tone.sdr_white_nits {
                tone.max_display_nits = tone.max_display_nits.max(max);
            }
        }
    }

    let tf = buffer.metadata.transfer_function;
    let apply_peak_scaler = matches!(tf, HdrTransferFunction::Pq | HdrTransferFunction::Hlg);
    let exposure_scale = 2.0_f32.powf(exposure_ev);
    let peak_scale = if apply_peak_scaler {
        tone.sdr_white_nits / tone.max_display_nits.max(tone.sdr_white_nits)
    } else {
        1.0
    };

    let mut pixels = Vec::with_capacity(expected_len);
    for pixel in buffer.rgba_f32.chunks_exact(4) {
        let rgb_in = [pixel[0], pixel[1], pixel[2]];
        let decoded = decode_transfer_to_display_linear(rgb_in, tf, tone.sdr_white_nits);
        let linear_srgb =
            linear_primary_to_linear_srgb(decoded, buffer.color_space, &buffer.metadata);
        // Display‑referred **IEC 61966‑2‑1 sRGB**: browsers treat unmanaged stills without filmic HDR→SDR
        // roll‑off; **`encode_sdr_rgb8`** Reinhard + ~2.2 crushes mids on those blobs.
        // **PQ / BT.709 / scene‑linear masters** stay on **`encode_sdr_rgb8`** — matches **GPU Reinhard path**
        // for PQ (see WGSL **`encode_sdr`**, `INPUT_TRANSFER_PQ`).
        let encoded = if should_use_iec61966_tone_map_fallback(buffer, tf) {
            encode_linear_display_referred_srgb8(linear_srgb, exposure_scale, peak_scale)
        } else {
            encode_sdr_rgb8(linear_srgb, exposure_scale, peak_scale)
        };
        pixels.extend_from_slice(&[
            encoded[0],
            encoded[1],
            encoded[2],
            float_to_u8(pixel[3].clamp(0.0, 1.0)),
        ]);
    }
    Ok(pixels)
}

/// Decode full-range RGB **code values** (0–1) per CICP transfer to **display-linear**
/// channels in the same primary space as the codes (matches libavif `gammaToLinear` input).
pub(crate) fn decode_transfer_to_display_linear(
    rgb: [f32; 3],
    tf: HdrTransferFunction,
    sdr_white_nits: f32,
) -> [f32; 3] {
    let clamp01 = |v: f32| v.clamp(0.0, 1.0);
    match tf {
        HdrTransferFunction::Linear => rgb,
        HdrTransferFunction::Srgb => [
            srgb_nonlinear_channel_to_linear(rgb[0]),
            srgb_nonlinear_channel_to_linear(rgb[1]),
            srgb_nonlinear_channel_to_linear(rgb[2]),
        ],
        HdrTransferFunction::Bt709 => [
            bt709_nonlinear_channel_to_linear(rgb[0]),
            bt709_nonlinear_channel_to_linear(rgb[1]),
            bt709_nonlinear_channel_to_linear(rgb[2]),
        ],
        HdrTransferFunction::Pq => [
            pq_nonlinear_to_display_linear(clamp01(rgb[0]), sdr_white_nits),
            pq_nonlinear_to_display_linear(clamp01(rgb[1]), sdr_white_nits),
            pq_nonlinear_to_display_linear(clamp01(rgb[2]), sdr_white_nits),
        ],
        HdrTransferFunction::Hlg => [
            hlg_nonlinear_to_scene_linear(clamp01(rgb[0])),
            hlg_nonlinear_to_scene_linear(clamp01(rgb[1])),
            hlg_nonlinear_to_scene_linear(clamp01(rgb[2])),
        ],
        HdrTransferFunction::Gamma | HdrTransferFunction::Unknown => rgb,
    }
}

/// Inverse **BT.709 / SMPTE 170‑style opto-electronic transfer** (**ITU‑R BT.709** Annex 1 curve).
///
/// Codec / file **nonlinear display code** in 0–1 → nominal **linear‑light RGB** factor (unbounded nominal).
///
/// Distinct from IEC 61966‑2‑1 (`srgb_nonlinear_channel_to_linear`, [`HdrTransferFunction::Srgb`]).
pub(crate) fn bt709_nonlinear_channel_to_linear(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    const BT709_LINEAR_SEGMENT_END: f32 = 0.018;
    let breakpoint = BT709_LINEAR_SEGMENT_END * 4.5;
    if c < breakpoint {
        c / 4.5
    } else {
        ((c + 0.099) / 1.099).powf(1.0 / 0.45)
    }
}

/// Linear sRGB / extended linear where 1.0 is SDR white → nonlinear sRGB 8-bit (ISO gain-map SDR base).
pub(crate) fn linear_srgb_linear_to_srgb_u8(linear: f32) -> u8 {
    let linear = linear.clamp(0.0, 1.0);
    let encoded = if linear <= 0.0031308 {
        linear * 12.92
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    };
    (encoded * 255.0).round().clamp(0.0, 255.0) as u8
}

pub(crate) fn srgb_nonlinear_channel_to_linear(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// PQ non-linear code value (0–1) → absolute luminance in nits.
///
/// Normative: **ITU-R BT.2100-3** Table 4 (PQ system reference EOTF); same rational coefficients as
/// **SMPTE ST 2084** and the HDR plane WGSL in `renderer.rs`.
pub(crate) fn pq_nonlinear_to_absolute_nits(code: f32) -> f32 {
    let m2 = crate::constants::PQ_M2;
    let c1 = crate::constants::PQ_C1;
    let c2 = crate::constants::PQ_C2;
    let c3 = crate::constants::PQ_C3;
    let code_m2 = code.clamp(0.0, 1.0).powf(1.0 / m2);
    let numerator = (code_m2 - c1).max(0.0);
    let denominator = (c2 - c3 * code_m2).max(0.000001);
    10_000.0 * (numerator / denominator).powf(1.0 / crate::constants::PQ_M1)
}

/// Reference **PQ EOTF** (non-linear code → absolute luminance, then ÷ `sdr_white_nits` for display-relative linear).
pub(crate) fn pq_nonlinear_to_display_linear(code: f32, sdr_white_nits: f32) -> f32 {
    pq_nonlinear_to_absolute_nits(code) / sdr_white_nits.max(1.0)
}

/// BT.2100 HLG OETF inverse (scene linear), matching `hlg_to_scene_linear` in `renderer.rs`.
pub(crate) fn hlg_nonlinear_to_scene_linear(e_prime: f32) -> f32 {
    let a = 0.17883277_f32;
    let b = 0.28466892_f32;
    let c = 0.55991073_f32;
    if e_prime <= 0.5 {
        (e_prime * e_prime) / 3.0
    } else {
        (((e_prime - c).max(0.0) / a).exp() + b) / 12.0
    }
}

pub(crate) fn linear_primary_to_linear_srgb(
    rgb: [f32; 3],
    color_space: HdrColorSpace,
    meta: &HdrImageMetadata,
) -> [f32; 3] {
    match color_space {
        HdrColorSpace::LinearSrgb | HdrColorSpace::LinearScRgb => rgb,
        HdrColorSpace::Rec2020Linear => rec2020_linear_to_linear_srgb(rgb),
        HdrColorSpace::DisplayP3Linear => display_p3_linear_to_linear_srgb(rgb),
        HdrColorSpace::Aces2065_1 => aces2065_1_linear_to_linear_srgb(rgb),
        HdrColorSpace::Xyz => xyz_to_linear_srgb(rgb),
        HdrColorSpace::Unknown => {
            if matches!(
                meta.color_profile,
                HdrColorProfile::Cicp {
                    color_primaries: 9,
                    ..
                }
            ) {
                rec2020_linear_to_linear_srgb(rgb)
            } else if matches!(
                meta.color_profile,
                HdrColorProfile::Cicp {
                    color_primaries: 11,
                    ..
                }
            ) {
                display_p3_linear_to_linear_srgb(rgb)
            } else {
                rgb
            }
        }
    }
}

/// Display P3 (D65) linear RGB → linear sRGB (same white point; matrix from Skia/CSS pipelines).
fn display_p3_linear_to_linear_srgb(rgb: [f32; 3]) -> [f32; 3] {
    [
        1.2249401 * rgb[0] - 0.2249402 * rgb[1],
        -0.0420569 * rgb[0] + 1.0420571 * rgb[1],
        -0.0196376 * rgb[0] - 0.0786507 * rgb[1] + 1.0982884 * rgb[2],
    ]
}

fn rec2020_linear_to_linear_srgb(rgb: [f32; 3]) -> [f32; 3] {
    [
        1.6605 * rgb[0] - 0.5876 * rgb[1] - 0.0728 * rgb[2],
        -0.1246 * rgb[0] + 1.1329 * rgb[1] - 0.0083 * rgb[2],
        -0.0182 * rgb[0] - 0.1006 * rgb[1] + 1.1187 * rgb[2],
    ]
}

fn aces2065_1_linear_to_linear_srgb(rgb: [f32; 3]) -> [f32; 3] {
    [
        2.5216 * rgb[0] - 1.1369 * rgb[1] - 0.3849 * rgb[2],
        -0.2762 * rgb[0] + 1.3697 * rgb[1] - 0.0935 * rgb[2],
        -0.0159 * rgb[0] - 0.1478 * rgb[1] + 1.1638 * rgb[2],
    ]
}

fn xyz_to_linear_srgb(xyz: [f32; 3]) -> [f32; 3] {
    [
        3.2404 * xyz[0] - 1.5371 * xyz[1] - 0.4985 * xyz[2],
        -0.9692 * xyz[0] + 1.8760 * xyz[1] + 0.0415 * xyz[2],
        0.0556 * xyz[0] - 0.2040 * xyz[1] + 1.0572 * xyz[2],
    ]
}

/// Plain **display‑referred linear sRGB** (after transfer + gamut matrices) → 8-bit sRGB codes,
/// matching typical browser unmanaged sRGB pipelines (Chrome-like for HEIC stills).
pub(crate) fn encode_linear_display_referred_srgb8(
    linear_srgb: [f32; 3],
    exposure_scale: f32,
    peak_scale: f32,
) -> [u8; 3] {
    let scale = exposure_scale * peak_scale;
    [
        linear_srgb_linear_to_srgb_u8(sanitize_hdr_rgb(linear_srgb[0]) * scale),
        linear_srgb_linear_to_srgb_u8(sanitize_hdr_rgb(linear_srgb[1]) * scale),
        linear_srgb_linear_to_srgb_u8(sanitize_hdr_rgb(linear_srgb[2]) * scale),
    ]
}

#[inline]
fn use_direct_srgb_sdr_fallback(metadata: &HdrImageMetadata, tf: HdrTransferFunction) -> bool {
    tf == HdrTransferFunction::Srgb && metadata.reference != HdrReference::SceneLinear
}

/// [`use_direct_srgb_sdr_fallback`] only: unmanaged **IEC 61966‑2‑1 display‑referred** sRGB without filmic
/// Reinhard. **PQ / BT.709 / scene-linear** use [`encode_sdr_rgb8`] (matches GPU **`encode_sdr`** for PQ).
#[inline]
fn should_use_iec61966_tone_map_fallback(buffer: &HdrImageBuffer, tf: HdrTransferFunction) -> bool {
    use_direct_srgb_sdr_fallback(&buffer.metadata, tf)
}

pub(crate) fn encode_sdr_rgb8(
    linear_srgb: [f32; 3],
    exposure_scale: f32,
    peak_scale: f32,
) -> [u8; 3] {
    let mut out = [0_u8; 3];
    for i in 0..3 {
        let exposed = clamp_hdr_tone_map_input(
            sanitize_hdr_rgb(linear_srgb[i]) * exposure_scale * peak_scale,
        );
        let mapped = exposed / (1.0 + exposed);
        let encoded = mapped.powf(INVERSE_DISPLAY_GAMMA).clamp(0.0, 1.0);
        out[i] = float_to_u8(encoded);
    }
    out
}

pub(crate) fn validate_hdr_fallback_budget(width: u32, height: u32) -> Result<(), String> {
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(|| format!("HDR image dimensions overflow: {width}x{height}"))?;
    let total_bytes = pixels
        .checked_mul(HDR_FALLBACK_BYTES_PER_PIXEL_WITH_SDR)
        .ok_or_else(|| format!("HDR fallback byte size overflow: {width}x{height}"))?;

    if pixels > MAX_HDR_FALLBACK_PIXELS || total_bytes > MAX_HDR_FALLBACK_TOTAL_BYTES {
        return Err(format!(
            "HDR image {width}x{height} requires {total_bytes} bytes for full-float fallback, \
             exceeds HDR fallback limit of {MAX_HDR_FALLBACK_PIXELS} pixels / \
             {MAX_HDR_FALLBACK_TOTAL_BYTES} bytes"
        ));
    }

    Ok(())
}

fn sanitize_hdr_rgb(value: f32) -> f32 {
    if value.is_nan() || value <= 0.0 {
        0.0
    } else if value.is_infinite() {
        f32::MAX
    } else {
        value
    }
}

fn clamp_hdr_tone_map_input(value: f32) -> f32 {
    if value.is_nan() || value <= 0.0 {
        0.0
    } else if value.is_infinite() {
        MAX_HDR_TONE_MAP_INPUT
    } else {
        value.min(MAX_HDR_TONE_MAP_INPUT)
    }
}

fn float_to_u8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}
