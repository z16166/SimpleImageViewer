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

use super::*;

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct ToneMapUniform {
    pub(super) exposure_ev: f32,
    pub(super) sdr_white_nits: f32,
    pub(super) max_display_nits: f32,
    pub(super) native_display_scale: f32,
    pub(super) rotation_steps: u32,
    pub(super) alpha: f32,
    pub(super) output_mode: u32,
    pub(super) input_color_space: u32,
    pub(super) input_transfer_function: u32,
    pub(super) input_reference: u32,
    /// See WGSL [`ToneMapSettings::sdr_manual_srgb_encode`].
    pub(super) sdr_manual_srgb_encode: u32,
    /// Matches WGSL uniform layout: `uv_min` starts at byte 48 (8-byte aligned).
    pub(super) _wgsl_pad_before_uv: u32,
    pub(super) uv_min: [f32; 2],
    pub(super) uv_max: [f32; 2],
    pub(super) apple_compose: u32,
    pub(super) headroom_span: f32,
    pub(super) weight: f32,
    pub(super) gain_width: u32,
    pub(super) gain_height: u32,
    pub(super) primary_width: u32,
    pub(super) primary_height: u32,
    pub(super) _apple_pad: u32,
    pub(super) ripple_center: [f32; 2],
    pub(super) ripple_radius: f32,
    pub(super) ripple_enabled: u32,
    pub(super) pixels_per_point: f32,
    pub(super) _pad0: u32,
    pub(super) _pad1: u32,
    pub(super) _pad2: u32,
}

unsafe impl bytemuck::Zeroable for ToneMapUniform {}
unsafe impl bytemuck::Pod for ToneMapUniform {}

const _: () = assert!(std::mem::size_of::<ToneMapUniform>() == 128);

impl ToneMapUniform {
    pub(super) fn from_settings(
        settings: HdrToneMapSettings,
        rotation_steps: u32,
        alpha: f32,
        output_mode: HdrRenderOutputMode,
        framebuffer_format: wgpu::TextureFormat,
        input_color_space: HdrColorSpace,
        input_transfer_function: HdrTransferFunction,
        input_reference: HdrReference,
        uv_rect: egui::Rect,
        native_display_scale: f32,
        apple: Option<(&crate::hdr::types::AppleHeicGainMapGpuSource, u32, u32, f32)>,
        ripple: Option<(egui::Pos2, f32, f32, u32)>,
    ) -> Self {
        let manual_srgb = output_mode == HdrRenderOutputMode::SdrToneMapped
            && hdr_sdr_framebuffer_needs_manual_srgb_oetf(framebuffer_format);
        let (
            apple_compose,
            headroom_span,
            weight,
            gain_width,
            gain_height,
            primary_width,
            primary_height,
        ) = if let Some((deferred, primary_w, primary_h, target_capacity)) = apple {
            (
                1,
                deferred.headroom_span,
                #[cfg(feature = "heif-native")]
                apple_gain_map_display_weight(target_capacity, deferred.stops),
                #[cfg(not(feature = "heif-native"))]
                0.0_f32,
                deferred.gain_width,
                deferred.gain_height,
                primary_w,
                primary_h,
            )
        } else {
            (0, 0.0, 0.0, 0, 0, 0, 0)
        };
        let (ripple_center, ripple_radius, ripple_enabled, pixels_per_point) =
            if let Some((center, radius, ppp, mode)) = ripple {
                ([center.x, center.y], radius, mode, ppp)
            } else {
                ([0.0, 0.0], 0.0, 0u32, 1.0)
            };
        Self {
            exposure_ev: settings.exposure_ev,
            sdr_white_nits: settings.sdr_white_nits,
            max_display_nits: settings.max_display_nits,
            native_display_scale: native_display_scale.clamp(0.0, f32::MAX),
            rotation_steps: rotation_steps % 4,
            alpha: alpha.clamp(0.0, 1.0),
            output_mode: output_mode as u32,
            input_color_space: input_color_space as u32,
            input_transfer_function: input_transfer_function as u32,
            input_reference: input_reference as u32,
            sdr_manual_srgb_encode: manual_srgb as u32,
            _wgsl_pad_before_uv: 0,
            uv_min: [uv_rect.min.x, uv_rect.min.y],
            uv_max: [uv_rect.max.x, uv_rect.max.y],
            apple_compose,
            headroom_span,
            weight,
            gain_width,
            gain_height,
            primary_width,
            primary_height,
            _apple_pad: 0,
            ripple_center,
            ripple_radius,
            ripple_enabled,
            pixels_per_point,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        }
    }
}

/// Peak scaler for **libavif** `avifImageApplyGainMap` output: display-referred linear in ~0–1,
/// same factor as the first step of `encode_sdr` so Native HDR is not hotter than the SDR path.
pub(super) fn libavif_tone_map_native_display_scale(
    metadata: &HdrImageMetadata,
    color_space: HdrColorSpace,
    tone: &HdrToneMapSettings,
) -> f32 {
    let capped = metadata
        .gain_map
        .as_ref()
        .is_some_and(|g| g.capped_display_referred);
    if !capped {
        return 1.0;
    }
    if metadata.transfer_function != HdrTransferFunction::Linear
        || color_space != HdrColorSpace::LinearSrgb
    {
        return 1.0;
    }
    tone.sdr_white_nits / tone.max_display_nits.max(tone.sdr_white_nits)
}

pub(super) fn hdr_tile_tone_map_uniform(
    settings: HdrToneMapSettings,
    rotation_steps: u32,
    alpha: f32,
    output_mode: HdrRenderOutputMode,
    framebuffer_format: wgpu::TextureFormat,
    tile: &crate::hdr::tiled::HdrTileBuffer,
    uv_rect: egui::Rect,
    native_display_scale: f32,
    jpeg_gpu_composed: bool,
) -> ToneMapUniform {
    if jpeg_gpu_composed {
        return tile_tone_map_uniform(
            settings,
            rotation_steps,
            alpha,
            output_mode,
            framebuffer_format,
            HdrColorSpace::LinearSrgb,
            HdrTransferFunction::Linear,
            HdrReference::Unknown,
            uv_rect,
            native_display_scale,
        );
    }

    tile_tone_map_uniform(
        settings,
        rotation_steps,
        alpha,
        output_mode,
        framebuffer_format,
        tile.metadata.color_space_hint(),
        tile.metadata.transfer_function,
        tile.metadata.reference,
        uv_rect,
        native_display_scale,
    )
}

pub(super) fn tile_tone_map_uniform(
    settings: HdrToneMapSettings,
    rotation_steps: u32,
    alpha: f32,
    output_mode: HdrRenderOutputMode,
    framebuffer_format: wgpu::TextureFormat,
    input_color_space: HdrColorSpace,
    input_transfer_function: HdrTransferFunction,
    input_reference: HdrReference,
    uv_rect: egui::Rect,
    native_display_scale: f32,
) -> ToneMapUniform {
    ToneMapUniform::from_settings(
        settings,
        rotation_steps,
        alpha,
        output_mode,
        framebuffer_format,
        input_color_space,
        input_transfer_function,
        input_reference,
        uv_rect,
        native_display_scale,
        None,
        None,
    )
}

pub(super) fn image_tone_map_uniform(
    image: &HdrImageBuffer,
    settings: HdrToneMapSettings,
    rotation_steps: u32,
    alpha: f32,
    output_mode: HdrRenderOutputMode,
    framebuffer_format: wgpu::TextureFormat,
    uv_rect: egui::Rect,
    native_display_scale: f32,
    apple_gpu_composed: bool,
    ripple: Option<(egui::Pos2, f32, f32, u32)>,
) -> ToneMapUniform {
    if apple_gpu_composed {
        return ToneMapUniform::from_settings(
            settings,
            rotation_steps,
            alpha,
            output_mode,
            framebuffer_format,
            HdrColorSpace::LinearSrgb,
            HdrTransferFunction::Linear,
            HdrReference::Unknown,
            uv_rect,
            native_display_scale,
            None,
            ripple,
        );
    }

    ToneMapUniform::from_settings(
        settings,
        rotation_steps,
        alpha,
        output_mode,
        framebuffer_format,
        image.metadata.color_space_hint(),
        image.metadata.transfer_function,
        image.metadata.reference,
        uv_rect,
        native_display_scale,
        None,
        ripple,
    )
}
