// Simple Image Viewer - A high-performance, cross-platform image viewer
// Copyright (C) 2024-2026 Simple Image Viewer Contributors
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

//! Build [`eframe::egui_wgpu::VulkanHdrMetadata`] from decoded HDR image metadata.
//!
//! This module is **format-agnostic**: every HDR decode path produces
//! [`HdrImageMetadata`] + optional [`HdrImageBuffer`] pixels. Vulkan ST 2086
//! metadata is derived from that unified representation, not from individual
//! file extensions.
//!
//! | Source | MaxCLL / peak hint | MaxFALL | Notes |
//! |--------|-------------------|---------|-------|
//! | AVIF (PQ/HLG) | CLLI `max_cll_nits` | CLLI `max_fall_nits` | Container metadata |
//! | HEIF / HEIC | CICP transfer + optional scan | scan / 0 | PQ/HLG via NCLX |
//! | JPEG XL (float HDR) | `intensity_target` or scan | scan / 0 | libjxl basic info |
//! | Ultra HDR JPEG_R | Gain-map `hdr_capacity_max` or scan | scan / 0 | ISO / XMP headroom |
//! | EXR / Radiance `.hdr` | Pixel scan (scene-linear) | scan / 0 | No container CLLI |
//! | Float / LogLuv TIFF | Pixel scan (scene-linear) | scan / 0 | IEEE float / LogLuv decode |
//! | Tiled HDR | `HdrTiledSource::metadata()` + preview scan | preview scan / 0 | Preview refines peak |

// Linux Vulkan HDR sync loads this API; other targets only reference it via `cfg(test)` here.
#![cfg_attr(
    not(any(test, target_os = "linux")),
    allow(dead_code)
)]

use eframe::egui_wgpu::VulkanHdrMetadata;

use super::decode::{hlg_nonlinear_to_scene_linear, pq_nonlinear_to_absolute_nits};
use super::types::{
    HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrLuminanceMetadata, HdrTransferFunction,
    DEFAULT_SDR_WHITE_NITS,
};

const DEFAULT_MASTERING_MAX_NITS: f32 = 1000.0;
const DEFAULT_MASTERING_MIN_NITS: f32 = 0.005;
const MAX_PEAK_SCAN_SAMPLES: usize = 1_048_576;

/// **SMPTE ST 2084** / **ITU-R BT.2100** PQ system reference luminance (cd/m²).
/// PQ code value 1.0 corresponds to this luminance; an HDR10 PQ link cannot
/// represent absolute content peaks above it.
const ST2084_PQ_REFERENCE_LUMINANCE_NITS: f32 = 10_000.0;

/// Pre-PQ linear cap in the HDR image-plane shader (`MAX_FINITE_HDR_VALUE` in
/// `hdr/renderer.rs`) — same bound used before `display_linear_to_pq`.
const HDR_PLANE_LINEAR_PRE_PQ_CAP: f32 = 65_504.0;

/// ST 2086 **MaxCLL** / **MaxFALL** must be positive, finite cd/m² values that
/// are representable on the HDR10 PQ presentation path (ST 2084 ceiling).
fn is_representable_st2086_content_luminance(nits: f32) -> bool {
    nits.is_finite()
        && nits > 0.0
        && nits <= ST2084_PQ_REFERENCE_LUMINANCE_NITS
}

fn validated_st2086_content_luminance(nits: Option<f32>) -> Option<f32> {
    nits.filter(|value| is_representable_st2086_content_luminance(*value))
}

fn validated_st2086_mastering_luminance(nits: f32, fallback: f32) -> f32 {
    if is_representable_st2086_content_luminance(nits) {
        nits
    } else {
        fallback
    }
}

fn sanitize_hdr_plane_linear(channel: f32) -> f32 {
    if channel != channel {
        0.0
    } else {
        channel.clamp(-HDR_PLANE_LINEAR_PRE_PQ_CAP, HDR_PLANE_LINEAR_PRE_PQ_CAP)
    }
}

/// Resolve MaxCLL (nits) for swap-chain metadata across all HDR formats.
///
/// Priority: container MaxCLL → subsampled pixel peak → container mastering
/// peak (`intensity_target`, gain-map headroom, …) → none.
pub fn content_peak_nits(
    luminance: &HdrLuminanceMetadata,
    buffer: Option<&HdrImageBuffer>,
) -> Option<f32> {
    if let Some(max_cll) = luminance
        .max_cll_nits
        .and_then(|value| validated_st2086_content_luminance(Some(value)))
    {
        return Some(max_cll);
    }

    if let Some(buffer) = buffer {
        if let Some(peak) = estimate_max_cll_nits(buffer) {
            return validated_st2086_content_luminance(Some(peak));
        }
    }

    luminance
        .mastering_max_nits
        .and_then(|value| validated_st2086_content_luminance(Some(value)))
}

/// ST 2086 metadata for an HDR swap chain when the current view has **no**
/// decoded HDR plane (SDR still / fallback on an HDR10 PQ surface).
pub fn default_vulkan_hdr_metadata_for_sdr_view() -> VulkanHdrMetadata {
    vulkan_hdr_metadata_from_luminance(&HdrLuminanceMetadata::default(), None)
}

/// Build swap-chain ST 2086 metadata from unified decode metadata + optional pixels.
pub fn vulkan_hdr_metadata_for_content(
    image_metadata: &HdrImageMetadata,
    scan_buffer: Option<&HdrImageBuffer>,
) -> VulkanHdrMetadata {
    let peak = content_peak_nits(&image_metadata.luminance, scan_buffer);
    vulkan_hdr_metadata_from_luminance(&image_metadata.luminance, peak)
}

/// Build swap-chain ST 2086 metadata for the current HDR image.
pub fn vulkan_hdr_metadata_from_luminance(
    luminance: &HdrLuminanceMetadata,
    content_peak_nits: Option<f32>,
) -> VulkanHdrMetadata {
    let max_cll = validated_st2086_content_luminance(content_peak_nits)
        .unwrap_or(DEFAULT_MASTERING_MAX_NITS);

    let max_fall = luminance
        .max_fall_nits
        .and_then(|value| validated_st2086_content_luminance(Some(value)))
        .map(|fall| fall.min(max_cll))
        .unwrap_or(0.0);

    let mastering_max = luminance
        .mastering_max_nits
        .map(|value| validated_st2086_mastering_luminance(value, max_cll))
        .unwrap_or_else(|| max_cll.max(DEFAULT_MASTERING_MAX_NITS));

    let min_luminance = luminance
        .mastering_min_nits
        .filter(|value| value.is_finite() && *value >= 0.0)
        .unwrap_or(DEFAULT_MASTERING_MIN_NITS);

    VulkanHdrMetadata {
        mastering_max_luminance_nits: mastering_max,
        max_content_light_level_nits: max_cll,
        max_frame_average_luminance_nits: max_fall,
        min_luminance_nits: min_luminance,
    }
}

/// Estimate MaxCLL (nits) by subsampling decoded pixel values.
pub fn estimate_max_cll_nits(buffer: &HdrImageBuffer) -> Option<f32> {
    let pixel_count = buffer
        .width
        .checked_mul(buffer.height)?
        .min(buffer.rgba_f32.len() as u32 / 4) as usize;
    if pixel_count == 0 {
        return None;
    }

    let stride = (pixel_count / MAX_PEAK_SCAN_SAMPLES).max(1);
    let sdr_white = buffer
        .metadata
        .luminance
        .sdr_white_nits
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(DEFAULT_SDR_WHITE_NITS);
    let transfer = buffer.metadata.transfer_function;

    let mut peak_nits = 0.0_f32;
    for (index, pixel) in buffer.rgba_f32.chunks_exact(4).enumerate() {
        if index % stride != 0 {
            continue;
        }
        let rgb = [pixel[0], pixel[1], pixel[2]];
        let nits = pixel_rgb_to_peak_nits(rgb, buffer.color_space, transfer, sdr_white);
        if nits.is_finite() {
            peak_nits = peak_nits.max(nits);
        }
    }

    validated_st2086_content_luminance(Some(peak_nits))
}

fn pixel_rgb_to_peak_nits(
    rgb: [f32; 3],
    color_space: HdrColorSpace,
    transfer: HdrTransferFunction,
    sdr_white_nits: f32,
) -> f32 {
    match transfer {
        HdrTransferFunction::Pq => rgb
            .map(|code| pq_nonlinear_to_absolute_nits(code.clamp(0.0, 1.0)))
            .into_iter()
            .fold(0.0_f32, f32::max),
        HdrTransferFunction::Hlg => {
            let linear = rgb.map(|code| hlg_nonlinear_to_scene_linear(code.clamp(0.0, 1.0)));
            linear_luminance(linear, color_space) * sdr_white_nits
        }
        HdrTransferFunction::Linear | HdrTransferFunction::Srgb => {
            let linear = if matches!(transfer, HdrTransferFunction::Srgb) {
                rgb.map(srgb_nonlinear_to_linear)
            } else {
                rgb.map(sanitize_hdr_plane_linear)
            };
            // Match HDR plane PQ encode: `display_linear_to_pq` treats linear input as
            // display-referred where 1.0 = SDR white → nits = rgb * sdr_white_nits.
            linear_luminance(linear, color_space) * sdr_white_nits
        }
        HdrTransferFunction::Gamma | HdrTransferFunction::Unknown => {
            linear_luminance(
                rgb.map(sanitize_hdr_plane_linear),
                color_space,
            ) * sdr_white_nits
        }
    }
}

fn linear_luminance(linear_rgb: [f32; 3], color_space: HdrColorSpace) -> f32 {
    let weights = match color_space {
        HdrColorSpace::Rec2020Linear => [0.2627_f32, 0.6780, 0.0593],
        _ => [0.2126_f32, 0.7152, 0.0722],
    };
    (weights[0] * linear_rgb[0] + weights[1] * linear_rgb[1] + weights[2] * linear_rgb[2]).max(0.0)
}

fn srgb_nonlinear_to_linear(channel: f32) -> f32 {
    let channel = channel.clamp(0.0, 1.0);
    if channel <= 0.04045 {
        channel / 12.92
    } else {
        ((channel + 0.055) / 1.055).powf(2.4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hdr::types::{
        HdrColorSpace, HdrImageMetadata, HdrLuminanceMetadata, HdrPixelFormat, HdrReference,
        DEFAULT_SDR_WHITE_NITS,
    };

    #[test]
    fn vulkan_metadata_prefers_container_max_cll_over_estimated_peak() {
        let luminance = HdrLuminanceMetadata {
            max_cll_nits: Some(4000.0),
            max_fall_nits: Some(350.0),
            mastering_max_nits: Some(1000.0),
            ..HdrLuminanceMetadata::default()
        };
        let metadata = vulkan_hdr_metadata_from_luminance(&luminance, Some(800.0));
        assert_eq!(metadata.max_content_light_level_nits, 800.0);
        let peak = content_peak_nits(&luminance, None);
        assert_eq!(peak, Some(4000.0));
        let metadata = vulkan_hdr_metadata_from_luminance(&luminance, peak);
        assert_eq!(metadata.max_content_light_level_nits, 4000.0);
        assert_eq!(metadata.max_frame_average_luminance_nits, 350.0);
        assert_eq!(metadata.mastering_max_luminance_nits, 1000.0);
    }

    #[test]
    fn vulkan_metadata_uses_estimated_peak_when_clli_missing() {
        let luminance = HdrLuminanceMetadata::default();
        let metadata = vulkan_hdr_metadata_from_luminance(&luminance, Some(1800.0));
        assert_eq!(metadata.max_content_light_level_nits, 1800.0);
        assert_eq!(metadata.mastering_max_luminance_nits, 1800.0);
    }

    #[test]
    fn content_peak_falls_back_to_mastering_max_without_buffer() {
        let luminance = HdrLuminanceMetadata {
            mastering_max_nits: Some(2550.0),
            ..HdrLuminanceMetadata::default()
        };
        assert_eq!(content_peak_nits(&luminance, None), Some(2550.0));
    }

    #[test]
    fn estimate_max_cll_from_pq_buffer_finds_brightest_code_value() {
        let mut rgba = vec![0.0_f32; 16];
        rgba[8] = 0.75;
        let buffer = HdrImageBuffer {
            width: 2,
            height: 2,
            format: HdrPixelFormat::Rgba32Float,
            color_space: HdrColorSpace::Rec2020Linear,
            metadata: HdrImageMetadata {
                transfer_function: HdrTransferFunction::Pq,
                reference: HdrReference::DisplayReferred,
                ..HdrImageMetadata::from_color_space(HdrColorSpace::Rec2020Linear)
            },
            rgba_f32: rgba.into(),
        };
        let peak = estimate_max_cll_nits(&buffer).expect("peak");
        assert!(
            peak > 900.0,
            "PQ code 0.75 should map to high nits, got {peak}"
        );
    }

    #[test]
    fn estimate_max_cll_from_scene_linear_radiance_hdr_values() {
        let buffer = HdrImageBuffer {
            width: 1,
            height: 1,
            format: HdrPixelFormat::Rgba32Float,
            color_space: HdrColorSpace::LinearSrgb,
            metadata: HdrImageMetadata {
                transfer_function: HdrTransferFunction::Linear,
                reference: HdrReference::SceneLinear,
                ..HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb)
            },
            rgba_f32: vec![8.0, 8.0, 8.0, 1.0].into(),
        };
        let expected = 8.0 * DEFAULT_SDR_WHITE_NITS;
        assert_eq!(estimate_max_cll_nits(&buffer), Some(expected));
    }

    #[test]
    fn estimate_max_cll_from_linear_display_referred_matches_pq_shader_scale() {
        let buffer = HdrImageBuffer {
            width: 1,
            height: 1,
            format: HdrPixelFormat::Rgba32Float,
            color_space: HdrColorSpace::LinearSrgb,
            metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
            rgba_f32: vec![2.0, 2.0, 2.0, 1.0].into(),
        };
        let peak = estimate_max_cll_nits(&buffer).expect("peak");
        let expected = 2.0 * DEFAULT_SDR_WHITE_NITS;
        assert!(
            (peak - expected).abs() < 0.01,
            "linear 2.0 should map to ~{expected} nits, got {peak}"
        );
    }

    #[test]
    fn default_sdr_view_metadata_uses_conservative_swap_chain_defaults() {
        let metadata = default_vulkan_hdr_metadata_for_sdr_view();
        assert_eq!(metadata.max_content_light_level_nits, 1000.0);
        assert_eq!(metadata.max_frame_average_luminance_nits, 0.0);
    }

    #[test]
    fn sdr_view_default_differs_from_bright_hdr_content_metadata() {
        let sdr = default_vulkan_hdr_metadata_for_sdr_view();
        let luminance = HdrLuminanceMetadata {
            max_cll_nits: Some(4000.0),
            max_fall_nits: Some(350.0),
            ..HdrLuminanceMetadata::default()
        };
        let hdr = vulkan_hdr_metadata_from_luminance(&luminance, Some(4000.0));
        assert_ne!(
            sdr.max_content_light_level_nits,
            hdr.max_content_light_level_nits
        );
    }

    #[test]
    fn extreme_linear_exr_peak_is_not_valid_st2086_content_luminance() {
        let buffer = HdrImageBuffer {
            width: 1,
            height: 1,
            format: HdrPixelFormat::Rgba32Float,
            color_space: HdrColorSpace::LinearSrgb,
            metadata: HdrImageMetadata {
                transfer_function: HdrTransferFunction::Linear,
                reference: HdrReference::SceneLinear,
                ..HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb)
            },
            rgba_f32: vec![f32::MAX, 0.0, 0.0, 1.0].into(),
        };
        assert_eq!(estimate_max_cll_nits(&buffer), None);
        let metadata = vulkan_hdr_metadata_for_content(&buffer.metadata, Some(&buffer));
        assert_eq!(metadata.max_content_light_level_nits, DEFAULT_MASTERING_MAX_NITS);
    }

    #[test]
    fn container_clli_above_st2084_pq_ceiling_is_ignored() {
        let luminance = HdrLuminanceMetadata {
            max_cll_nits: Some(50_000.0),
            ..HdrLuminanceMetadata::default()
        };
        assert_eq!(content_peak_nits(&luminance, None), None);
        let metadata = vulkan_hdr_metadata_from_luminance(&luminance, None);
        assert_eq!(metadata.max_content_light_level_nits, DEFAULT_MASTERING_MAX_NITS);
    }

    #[test]
    fn max_fall_is_clamped_to_max_cll_for_st2086_consistency() {
        let luminance = HdrLuminanceMetadata {
            max_cll_nits: Some(1000.0),
            max_fall_nits: Some(4000.0),
            ..HdrLuminanceMetadata::default()
        };
        let metadata = vulkan_hdr_metadata_from_luminance(&luminance, Some(1000.0));
        assert_eq!(metadata.max_content_light_level_nits, 1000.0);
        assert_eq!(metadata.max_frame_average_luminance_nits, 1000.0);
    }

    #[test]
    fn pq_code_one_maps_to_st2084_reference_luminance() {
        let buffer = HdrImageBuffer {
            width: 1,
            height: 1,
            format: HdrPixelFormat::Rgba32Float,
            color_space: HdrColorSpace::Rec2020Linear,
            metadata: HdrImageMetadata {
                transfer_function: HdrTransferFunction::Pq,
                reference: HdrReference::DisplayReferred,
                ..HdrImageMetadata::from_color_space(HdrColorSpace::Rec2020Linear)
            },
            rgba_f32: vec![1.0, 1.0, 1.0, 1.0].into(),
        };
        let peak = estimate_max_cll_nits(&buffer).expect("peak");
        assert!((peak - ST2084_PQ_REFERENCE_LUMINANCE_NITS).abs() < 1.0);
    }
}
