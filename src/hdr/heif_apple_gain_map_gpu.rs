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

//! Apple HEIC HDR gain-map GPU path.
//!
//! Decode attaches deferred planes via [`attach_apple_heic_gpu_deferred`] (encoded primary + gain map
//! only). The HDR renderer runs **`cs_compose_apple_gain`** at upload via
//! [`crate::hdr::renderer::apple_compose_gpu`]: primary rows are uploaded in strips that fit
//! `max_storage_buffer_binding_size`, gain map is sampled from a texture, output is written
//! directly to the display texture. Never show encoded primary for deferred HEIC.

use crate::hdr::gain_map::validate_gain_map_rgba_len;
use crate::hdr::heif_apple_gain_map::apple_gain_map_display_weight;
use crate::hdr::heif_apple_gain_map_compose_simd::compose_apple_gain_map_pixels;
use crate::hdr::types::{
    AppleHeicGainMapGpuSource, HdrColorSpace, HdrGainMapMetadata, HdrImageBuffer, HdrImageMetadata,
};
use std::sync::Arc;

pub(crate) fn validate_apple_deferred_planes(
    hdr: &HdrImageBuffer,
    gain_w: u32,
    gain_h: u32,
    gain_rgba: &[u8],
) -> Result<(), String> {
    let pixel_count = hdr.width as usize * hdr.height as usize * 4;
    if hdr.rgba_f32.len() != pixel_count {
        return Err(format!(
            "Apple deferred primary rgba_f32 length mismatch: got {}, expected {pixel_count} for {}x{}",
            hdr.rgba_f32.len(),
            hdr.width,
            hdr.height
        ));
    }
    validate_gain_map_rgba_len(gain_rgba, gain_w, gain_h)
}

/// Color space pushed into the Apple GPU compose uniform — must match CPU
/// [`linear_primary_to_linear_srgb`](crate::hdr::decode::linear_primary_to_linear_srgb) inputs.
pub(crate) fn apple_heic_compose_effective_color_space(
    color_space: HdrColorSpace,
    metadata: &HdrImageMetadata,
) -> HdrColorSpace {
    match color_space {
        HdrColorSpace::LinearScRgb => HdrColorSpace::LinearSrgb,
        HdrColorSpace::Unknown => metadata.color_space_hint(),
        other => other,
    }
}

/// CPU compose when GPU strip compose in [`crate::hdr::renderer::apple_compose_gpu`] is unavailable.
pub(crate) fn compose_apple_heic_deferred_cpu_pixels(
    image: &HdrImageBuffer,
    hdr_target_capacity: f32,
) -> Result<Vec<f32>, String> {
    let deferred = apple_heic_deferred_from_metadata(&image.metadata)
        .ok_or_else(|| "Apple HEIC deferred metadata missing".to_string())?;
    let weight = apple_gain_map_display_weight(hdr_target_capacity, deferred.stops);
    let pixel_count = image.width as usize * image.height as usize * 4;
    let mut composed = vec![0.0_f32; pixel_count];
    compose_apple_gain_map_pixels(
        crate::hdr::heif_apple_gain_map_compose_simd::AppleGainMapComposePixels {
            base_pixels: image.rgba_f32.as_slice(),
            composed_pixels: &mut composed,
            width: image.width,
            height: image.height,
            gain_rgba: deferred.gain_rgba.as_slice(),
            gain_w: deferred.gain_width,
            gain_h: deferred.gain_height,
            color_space: image.color_space,
            transfer: image.metadata.transfer_function,
            metadata: &image.metadata,
            headroom_span: deferred.headroom_span,
            weight,
            force_scalar: false,
        },
    );
    Ok(composed)
}

#[cfg(test)]
pub(crate) fn compose_apple_heic_deferred_to_scene_linear(
    image: &HdrImageBuffer,
    hdr_target_capacity: f32,
) -> Option<Vec<f32>> {
    compose_apple_heic_deferred_cpu_pixels(image, hdr_target_capacity).ok()
}

/// Build deferred GPU planes from a pre-compose primary buffer and decoded gain-map RGBA8.
pub(crate) fn attach_apple_heic_gpu_deferred(
    hdr: HdrImageBuffer,
    gain_w: u32,
    gain_h: u32,
    gain_rgba: Vec<u8>,
    headroom_span: f32,
    stops: f32,
    hdr_target_capacity: f32,
) -> Result<HdrImageBuffer, (HdrImageBuffer, String)> {
    if let Err(err) = validate_apple_deferred_planes(&hdr, gain_w, gain_h, &gain_rgba) {
        return Err((hdr, err));
    }

    let gain_rgba = Arc::new(gain_rgba);
    let weight = apple_gain_map_display_weight(hdr_target_capacity, stops);

    let mut metadata = hdr.metadata.clone();
    metadata.gain_map = Some(HdrGainMapMetadata {
        source: "HEIF",
        target_hdr_capacity: Some(hdr_target_capacity),
        diagnostic: format!(
            "Apple HDR Gain Map GPU deferred ({}x{} pixels, stops: {:.2}, weight: {:.2})",
            gain_w, gain_h, stops, weight
        ),
        capped_display_referred: false,
        apple_heic_deferred: Some(AppleHeicGainMapGpuSource {
            gain_rgba: Arc::clone(&gain_rgba),
            gain_width: gain_w,
            gain_height: gain_h,
            headroom_span,
            stops,
        }),
        iso_deferred: None,
    });

    Ok(HdrImageBuffer { metadata, ..hdr })
}

pub(crate) fn apple_heic_deferred_from_metadata(
    metadata: &HdrImageMetadata,
) -> Option<&AppleHeicGainMapGpuSource> {
    metadata
        .gain_map
        .as_ref()
        .and_then(|gm| gm.apple_heic_deferred.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hdr::types::{
        DEFAULT_SDR_WHITE_NITS, HdrColorSpace, HdrPixelFormat, HdrTransferFunction,
    };

    #[test]
    fn attach_deferred_populates_gain_map_metadata() {
        let hdr = HdrImageBuffer {
            width: 2,
            height: 2,
            format: HdrPixelFormat::Rgba32Float,
            color_space: HdrColorSpace::DisplayP3Linear,
            metadata: HdrImageMetadata {
                transfer_function: HdrTransferFunction::Srgb,
                ..Default::default()
            },
            rgba_f32: Arc::new(vec![
                0.5, 0.25, 0.125, 1.0, //
                0.0, 0.0, 0.0, 1.0, //
                1.0, 1.0, 1.0, 1.0, //
                0.25, 0.5, 0.75, 1.0,
            ]),
        };
        let gain = vec![128u8; 2 * 2 * 4];
        let out = attach_apple_heic_gpu_deferred(hdr, 2, 2, gain, 1.0, 2.0, 4.0).expect("attach");
        let deferred = apple_heic_deferred_from_metadata(&out.metadata).expect("deferred");
        assert_eq!(deferred.gain_width, 2);
    }

    #[test]
    fn compose_effective_color_space_resolves_unknown_via_metadata() {
        use crate::hdr::types::{HdrColorProfile, HdrReference};
        let metadata = HdrImageMetadata {
            transfer_function: HdrTransferFunction::Srgb,
            reference: HdrReference::Unknown,
            color_profile: HdrColorProfile::Cicp {
                color_primaries: 12,
                transfer_characteristics: 13,
                matrix_coefficients: 0,
                full_range: true,
            },
            ..Default::default()
        };
        assert_eq!(
            apple_heic_compose_effective_color_space(HdrColorSpace::Unknown, &metadata),
            HdrColorSpace::DisplayP3Linear,
        );
    }

    #[test]
    fn compose_deferred_matches_apply_path_pixel_exact() {
        let primary = vec![
            0.5, 0.25, 0.125, 1.0, //
            0.0, 0.0, 0.0, 1.0, //
            1.0, 1.0, 1.0, 1.0, //
            0.25, 0.5, 0.75, 1.0,
        ];
        let hdr = HdrImageBuffer {
            width: 2,
            height: 2,
            format: HdrPixelFormat::Rgba32Float,
            color_space: HdrColorSpace::DisplayP3Linear,
            metadata: HdrImageMetadata {
                transfer_function: HdrTransferFunction::Srgb,
                ..Default::default()
            },
            rgba_f32: Arc::new(primary.clone()),
        };
        let gain = vec![128u8; 2 * 2 * 4];
        let headroom_span = 1.0;
        let stops = 2.0;
        let target_capacity = 4.0;

        // Old decode-time path
        let headroom_params = crate::hdr::heif_apple_gain_map::AppleHdrHeadroomParams {
            hdr_headroom: 0.0,
            hdr_gain: 0.0,
            stops,
            linear_headroom: headroom_span + 1.0,
        };
        let old_composed = crate::hdr::heif_apple_gain_map::apply_apple_gain_map_composition(
            HdrImageBuffer {
                width: 2,
                height: 2,
                format: HdrPixelFormat::Rgba32Float,
                color_space: HdrColorSpace::DisplayP3Linear,
                metadata: HdrImageMetadata {
                    transfer_function: HdrTransferFunction::Srgb,
                    ..Default::default()
                },
                rgba_f32: Arc::new(primary),
            },
            2,
            2,
            &gain,
            &headroom_params,
            target_capacity,
        );

        // New deferred path
        let deferred =
            attach_apple_heic_gpu_deferred(hdr, 2, 2, gain, headroom_span, stops, target_capacity)
                .expect("attach");
        let new_composed = compose_apple_heic_deferred_to_scene_linear(&deferred, target_capacity)
            .expect("composed");

        assert_eq!(old_composed.rgba_f32.len(), new_composed.len());
        for (i, (a, b)) in old_composed
            .rgba_f32
            .iter()
            .zip(new_composed.iter())
            .enumerate()
        {
            assert!((a - b).abs() < 1e-6, "pixel {i}: old={a} new={b}");
        }
    }

    // ── WGSL GPU compute shader logic replicated in Rust ──────────────────

    /// Exact replica of the WGSL `bt709_gain_channel_to_linear` from
    /// `APPLE_GAIN_COMPOSE_SHADER`.
    fn wgsl_bt709_gain_channel_to_linear(c: f32) -> f32 {
        let encoded = c.clamp(0.0, 1.0);
        if encoded < 0.081 {
            encoded / 4.5
        } else {
            ((encoded + 0.099) / 1.099).powf(1.0 / 0.45)
        }
    }

    /// Exact replica of WGSL `decode_input_transfer` (used inside the compose shader).
    fn wgsl_decode_input_transfer(
        r: f32,
        g: f32,
        b: f32,
        input_transfer_function: u32,
    ) -> [f32; 3] {
        match input_transfer_function {
            1 => {
                // INPUT_TRANSFER_SRGB
                let f = |c: f32| -> f32 {
                    let c = c.clamp(0.0, 1.0);
                    if c <= 0.04045 {
                        c / 12.92
                    } else {
                        ((c + 0.055) / 1.055).powf(2.4)
                    }
                };
                [f(r), f(g), f(b)]
            }
            6 => {
                // INPUT_TRANSFER_BT709
                let f = |c: f32| -> f32 {
                    let c = c.clamp(0.0, 1.0);
                    if c < 0.081 {
                        c / 4.5
                    } else {
                        ((c + 0.099) / 1.099).powf(1.0 / 0.45)
                    }
                };
                [f(r), f(g), f(b)]
            }
            2 => {
                // INPUT_TRANSFER_PQ — same formula as WGSL `pq_to_display_linear`
                // using DEFAULT_SDR_WHITE_NITS (same as compute shader uniform).
                let m1 = crate::constants::PQ_M1;
                let m2 = crate::constants::PQ_M2;
                let c1 = crate::constants::PQ_C1;
                let c2 = crate::constants::PQ_C2;
                let c3 = crate::constants::PQ_C3;
                let sdr_white = DEFAULT_SDR_WHITE_NITS;
                let f = |c: f32| -> f32 {
                    let c = c.clamp(0.0, 1.0);
                    let code = c.powf(1.0 / m2);
                    let num = (code - c1).max(0.0);
                    let den = (c2 - c3 * code).max(0.000001);
                    10000.0 * (num / den).powf(1.0 / m1) / sdr_white.max(1.0)
                };
                [f(r), f(g), f(b)]
            }
            _ => [r, g, b],
        }
    }

    /// Exact replica of WGSL `convert_input_to_linear_srgb`.
    fn wgsl_convert_input_to_linear_srgb(
        r: f32,
        g: f32,
        b: f32,
        input_color_space: u32,
    ) -> [f32; 3] {
        match input_color_space {
            2 => {
                // INPUT_COLOR_SPACE_REC2020_LINEAR
                [
                    1.6605 * r - 0.5876 * g - 0.0728 * b,
                    -0.1246 * r + 1.1329 * g - 0.0083 * b,
                    -0.0182 * r - 0.1006 * g + 1.1187 * b,
                ]
            }
            6 => {
                // INPUT_COLOR_SPACE_DISPLAY_P3_LINEAR
                [
                    1.2249401 * r - 0.2249402 * g,
                    -0.0420569 * r + 1.0420571 * g,
                    -0.0196376 * r - 0.0786507 * g + 1.0982884 * b,
                ]
            }
            _ => [r, g, b],
        }
    }

    /// Exact replica of the WGSL compute shader's
    /// `sample_apple_gain_encoded_at_primary_pixel` → bilinear upsample.
    fn wgsl_sample_gain_encoded(
        gain_rgba: &[u8],
        gain_w: u32,
        gain_h: u32,
        primary_w: u32,
        primary_h: u32,
        px: u32,
        py: u32,
    ) -> [f32; 3] {
        if gain_w == 0 || gain_h == 0 || primary_w == 0 || primary_h == 0 {
            return [0.0, 0.0, 0.0];
        }
        let gx = ((px as f32 + 0.5) * gain_w as f32 / primary_w as f32 - 0.5)
            .clamp(0.0, gain_w.saturating_sub(1) as f32);
        let gy = ((py as f32 + 0.5) * gain_h as f32 / primary_h as f32 - 0.5)
            .clamp(0.0, gain_h.saturating_sub(1) as f32);
        let x0 = gx.floor() as u32;
        let y0 = gy.floor() as u32;
        let x1 = (x0 + 1).min(gain_w - 1);
        let y1 = (y0 + 1).min(gain_h - 1);
        let tx = gx - x0 as f32;
        let ty = gy - y0 as f32;

        let load = |x: u32, y: u32| -> [f32; 3] {
            let idx = (y * gain_w + x) as usize * 4;
            [
                gain_rgba[idx] as f32 / 255.0,
                gain_rgba[idx + 1] as f32 / 255.0,
                gain_rgba[idx + 2] as f32 / 255.0,
            ]
        };
        let p00 = load(x0, y0);
        let p10 = load(x1, y0);
        let p01 = load(x0, y1);
        let p11 = load(x1, y1);

        let mix_x0 = [
            p00[0] + (p10[0] - p00[0]) * tx,
            p00[1] + (p10[1] - p00[1]) * tx,
            p00[2] + (p10[2] - p00[2]) * tx,
        ];
        let mix_x1 = [
            p01[0] + (p11[0] - p01[0]) * tx,
            p01[1] + (p11[1] - p01[1]) * tx,
            p01[2] + (p11[2] - p01[2]) * tx,
        ];
        [
            mix_x0[0] + (mix_x1[0] - mix_x0[0]) * ty,
            mix_x0[1] + (mix_x1[1] - mix_x0[1]) * ty,
            mix_x0[2] + (mix_x1[2] - mix_x0[2]) * ty,
        ]
    }

    /// Simulate the WGSL compute shader `cs_compose_apple_gain` output for the
    /// entire image.  Returns a flat `Vec<f32>` in RGBA order, same layout as
    /// the CPU SIMD `composed_pixels`.
    fn wgsl_compose_apple_gain(
        primary_pixels: &[f32],
        primary_w: u32,
        primary_h: u32,
        gain_rgba: &[u8],
        gain_w: u32,
        gain_h: u32,
        input_color_space: u32,
        input_transfer_function: u32,
        headroom_span: f32,
        weight: f32,
    ) -> Vec<f32> {
        let pixel_count = primary_w as usize * primary_h as usize * 4;
        let mut out = vec![0.0_f32; pixel_count];
        for py in 0..primary_h {
            let row_base = py as usize * primary_w as usize * 4;
            for px in 0..primary_w {
                let idx = row_base + px as usize * 4;
                let r = primary_pixels[idx];
                let g = primary_pixels[idx + 1];
                let b = primary_pixels[idx + 2];
                let a = primary_pixels[idx + 3];

                let [dr, dg, db] = wgsl_decode_input_transfer(r, g, b, input_transfer_function);
                let [lr, lg, lb] = wgsl_convert_input_to_linear_srgb(dr, dg, db, input_color_space);
                let [gr, gg, gb] = wgsl_sample_gain_encoded(
                    gain_rgba, gain_w, gain_h, primary_w, primary_h, px, py,
                );
                let glr = wgsl_bt709_gain_channel_to_linear(gr);
                let glg = wgsl_bt709_gain_channel_to_linear(gg);
                let glb = wgsl_bt709_gain_channel_to_linear(gb);

                out[idx] = (lr * (1.0 + headroom_span * glr * weight)).max(0.0);
                out[idx + 1] = (lg * (1.0 + headroom_span * glg * weight)).max(0.0);
                out[idx + 2] = (lb * (1.0 + headroom_span * glb * weight)).max(0.0);
                out[idx + 3] = a;
            }
        }
        out
    }

    #[test]
    fn gpu_shader_matches_cpu_simd_non_trivial_gain_map() {
        // 8×6 primary with 4×3 gain map — exercises bilinear with different dims.
        const PW: u32 = 8;
        const PH: u32 = 6;
        const GW: u32 = 4;
        const GH: u32 = 3;
        let mut primary = Vec::with_capacity(PW as usize * PH as usize * 4);
        for y in 0..PH {
            for x in 0..PW {
                let fx = x as f32 / (PW - 1).max(1) as f32;
                let fy = y as f32 / (PH - 1).max(1) as f32;
                primary.extend_from_slice(&[fx, fy, 0.5, 1.0]);
            }
        }
        let gain: Vec<u8> = (0..GW * GH * 4)
            .map(|i| ((i as u32 * 37 + 17) % 251) as u8)
            .collect();

        let color_space = HdrColorSpace::DisplayP3Linear;
        let transfer = HdrTransferFunction::Srgb;
        let headroom_span = 1.2;
        let weight = 0.8;

        let metadata = HdrImageMetadata::default();

        // CPU SIMD path
        let mut cpu_out = vec![0.0_f32; PW as usize * PH as usize * 4];
        compose_apple_gain_map_pixels(
            crate::hdr::heif_apple_gain_map_compose_simd::AppleGainMapComposePixels {
                base_pixels: &primary,
                composed_pixels: &mut cpu_out,
                width: PW,
                height: PH,
                gain_rgba: &gain,
                gain_w: GW,
                gain_h: GH,
                color_space,
                transfer,
                metadata: &metadata,
                headroom_span,
                weight,
                force_scalar: false,
            },
        );

        // GPU shader simulation
        let gpu_out = wgsl_compose_apple_gain(
            &primary,
            PW,
            PH,
            &gain,
            GW,
            GH,
            color_space as u32,
            transfer as u32,
            headroom_span,
            weight,
        );

        assert_eq!(cpu_out.len(), gpu_out.len());
        let mut max_diff = 0.0_f32;
        for (i, (a, b)) in cpu_out.iter().zip(gpu_out.iter()).enumerate() {
            let diff = (a - b).abs();
            if diff > max_diff {
                max_diff = diff;
            }
            assert!(
                diff < 1e-5,
                "pixel {} (x={}, y={}, ch={}): cpu={a:.8} gpu={b:.8} diff={diff:.2e}",
                i,
                (i / 4) % PW as usize,
                i / (PW as usize * 4),
                i % 4,
            );
        }
        // Sanity: output is not all zeros.
        let sum: f32 = gpu_out.iter().sum();
        assert!(sum.abs() > 0.01, "composed output is unexpectedly dark");
    }

    #[test]
    fn gpu_shader_matches_cpu_simd_linear_srgb_identity() {
        // LinearSrgb + Linear transfer — both color-space and transfer are identity.
        const PW: u32 = 10;
        const PH: u32 = 4;
        const GW: u32 = 5;
        const GH: u32 = 2;
        let mut primary = Vec::with_capacity(PW as usize * PH as usize * 4);
        for i in 0..PW * PH {
            let v = (i as f32 % 100.0) / 100.0;
            primary.extend_from_slice(&[v, v * 0.8, v * 0.6, 1.0]);
        }
        let gain: Vec<u8> = (0..GW * GH * 4)
            .map(|i| ((i as u32 * 53 + 31) % 241) as u8)
            .collect();

        let metadata = HdrImageMetadata::default();
        let headroom_span = 0.8;
        let weight = 0.5;

        let mut cpu_out = vec![0.0_f32; PW as usize * PH as usize * 4];
        compose_apple_gain_map_pixels(
            crate::hdr::heif_apple_gain_map_compose_simd::AppleGainMapComposePixels {
                base_pixels: &primary,
                composed_pixels: &mut cpu_out,
                width: PW,
                height: PH,
                gain_rgba: &gain,
                gain_w: GW,
                gain_h: GH,
                color_space: HdrColorSpace::LinearSrgb,
                transfer: HdrTransferFunction::Linear,
                metadata: &metadata,
                headroom_span,
                weight,
                force_scalar: false,
            },
        );

        let gpu_out = wgsl_compose_apple_gain(
            &primary,
            PW,
            PH,
            &gain,
            GW,
            GH,
            HdrColorSpace::LinearSrgb as u32,
            HdrTransferFunction::Linear as u32,
            headroom_span,
            weight,
        );

        assert_eq!(cpu_out.len(), gpu_out.len());
        for (i, (a, b)) in cpu_out.iter().zip(gpu_out.iter()).enumerate() {
            assert!((a - b).abs() < 1e-5, "pixel {i}: cpu={a:.8} gpu={b:.8}");
        }
    }

    #[test]
    fn gpu_shader_matches_cpu_simd_same_size_gain_map() {
        // Same-size gain map — edge case where bilinear coordinates are integer.
        const N: u32 = 6;
        let mut primary = Vec::with_capacity(N as usize * N as usize * 4);
        for y in 0..N {
            for x in 0..N {
                primary.extend_from_slice(&[
                    (x as f32 + 1.0) / (N as f32 + 1.0),
                    (y as f32 + 1.0) / (N as f32 + 1.0),
                    0.5,
                    1.0,
                ]);
            }
        }
        let gain: Vec<u8> = primary
            .iter()
            .map(|v| (v * 255.0).clamp(0.0, 255.0) as u8)
            .collect();

        let metadata = HdrImageMetadata::default();
        let headroom_span = 1.5;
        let weight = 0.6;

        let mut cpu_out = vec![0.0_f32; N as usize * N as usize * 4];
        compose_apple_gain_map_pixels(
            crate::hdr::heif_apple_gain_map_compose_simd::AppleGainMapComposePixels {
                base_pixels: &primary,
                composed_pixels: &mut cpu_out,
                width: N,
                height: N,
                gain_rgba: &gain,
                gain_w: N,
                gain_h: N,
                color_space: HdrColorSpace::DisplayP3Linear,
                transfer: HdrTransferFunction::Srgb,
                metadata: &metadata,
                headroom_span,
                weight,
                force_scalar: false,
            },
        );

        let gpu_out = wgsl_compose_apple_gain(
            &primary,
            N,
            N,
            &gain,
            N,
            N,
            HdrColorSpace::DisplayP3Linear as u32,
            HdrTransferFunction::Srgb as u32,
            headroom_span,
            weight,
        );

        assert_eq!(cpu_out.len(), gpu_out.len());
        for (i, (a, b)) in cpu_out.iter().zip(gpu_out.iter()).enumerate() {
            assert!((a - b).abs() < 1e-5, "pixel {i}: cpu={a:.8} gpu={b:.8}");
        }
    }
}
