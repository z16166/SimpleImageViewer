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

#[derive(Clone, Copy)]
pub(super) struct ToneMapCommonParams {
    pub(super) settings: HdrToneMapSettings,
    pub(super) rotation_steps: u32,
    pub(super) alpha: f32,
    pub(super) output_mode: HdrRenderOutputMode,
    pub(super) framebuffer_format: wgpu::TextureFormat,
    pub(super) uv_rect: egui::Rect,
    pub(super) native_display_scale: f32,
}

#[derive(Clone, Copy)]
pub(super) struct ToneMapInputMetadata {
    pub(super) color_space: HdrColorSpace,
    pub(super) transfer_function: HdrTransferFunction,
    pub(super) reference: HdrReference,
}

#[derive(Clone, Copy)]
pub(super) struct AppleToneMapCompose<'a> {
    pub(super) deferred: &'a crate::hdr::types::AppleHeicGainMapGpuSource,
    pub(super) primary_w: u32,
    pub(super) primary_h: u32,
    pub(super) target_capacity: f32,
}

#[derive(Clone, Copy)]
pub(super) struct RippleToneMapParams {
    pub(super) center: egui::Pos2,
    pub(super) radius: f32,
    pub(super) pixels_per_point: f32,
    pub(super) mode: u32,
}

pub(super) struct ToneMapUniformParams<'a> {
    pub(super) common: ToneMapCommonParams,
    pub(super) input: ToneMapInputMetadata,
    pub(super) apple: Option<AppleToneMapCompose<'a>>,
    pub(super) ripple: Option<RippleToneMapParams>,
}

impl ToneMapUniform {
    pub(super) fn from_settings(params: ToneMapUniformParams<'_>) -> Self {
        let ToneMapUniformParams {
            common,
            input,
            apple,
            ripple,
        } = params;
        let ToneMapCommonParams {
            settings,
            rotation_steps,
            alpha,
            output_mode,
            framebuffer_format,
            uv_rect,
            native_display_scale,
        } = common;
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
        ) = if let Some(apple) = apple {
            (
                1,
                apple.deferred.headroom_span,
                #[cfg(feature = "heif-native")]
                apple_gain_map_display_weight(apple.target_capacity, apple.deferred.stops),
                #[cfg(not(feature = "heif-native"))]
                0.0_f32,
                apple.deferred.gain_width,
                apple.deferred.gain_height,
                apple.primary_w,
                apple.primary_h,
            )
        } else {
            (0, 0.0, 0.0, 0, 0, 0, 0)
        };
        let (ripple_center, ripple_radius, ripple_enabled, pixels_per_point) =
            if let Some(ripple) = ripple {
                (
                    [ripple.center.x, ripple.center.y],
                    ripple.radius,
                    ripple.mode,
                    ripple.pixels_per_point,
                )
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
            input_color_space: input.color_space as u32,
            input_transfer_function: input.transfer_function as u32,
            input_reference: input.reference as u32,
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

pub(super) struct HdrTileToneMapUniformParams<'a> {
    pub(super) common: ToneMapCommonParams,
    pub(super) tile: &'a crate::hdr::tiled::HdrTileBuffer,
    pub(super) jpeg_gpu_composed: bool,
}

pub(super) fn hdr_tile_tone_map_uniform(params: HdrTileToneMapUniformParams<'_>) -> ToneMapUniform {
    let HdrTileToneMapUniformParams {
        common,
        tile,
        jpeg_gpu_composed,
    } = params;
    if jpeg_gpu_composed {
        return tile_tone_map_uniform(
            common,
            ToneMapInputMetadata {
                color_space: HdrColorSpace::LinearSrgb,
                transfer_function: HdrTransferFunction::Linear,
                reference: HdrReference::Unknown,
            },
        );
    }

    tile_tone_map_uniform(
        common,
        ToneMapInputMetadata {
            color_space: tile.metadata.color_space_hint(),
            transfer_function: tile.metadata.transfer_function,
            reference: tile.metadata.reference,
        },
    )
}

pub(super) fn tile_tone_map_uniform(
    common: ToneMapCommonParams,
    input: ToneMapInputMetadata,
) -> ToneMapUniform {
    ToneMapUniform::from_settings(ToneMapUniformParams {
        common,
        input,
        apple: None,
        ripple: None,
    })
}

pub(super) struct ImageToneMapUniformParams {
    pub(super) common: ToneMapCommonParams,
    pub(super) gpu_composed_scene_linear: bool,
    pub(super) ripple: Option<RippleToneMapParams>,
}

pub(super) fn image_tone_map_uniform(
    image: &HdrImageBuffer,
    params: ImageToneMapUniformParams,
) -> ToneMapUniform {
    let ImageToneMapUniformParams {
        common,
        gpu_composed_scene_linear,
        ripple,
    } = params;
    if gpu_composed_scene_linear {
        return ToneMapUniform::from_settings(ToneMapUniformParams {
            common,
            input: ToneMapInputMetadata {
                color_space: HdrColorSpace::LinearSrgb,
                transfer_function: HdrTransferFunction::Linear,
                reference: HdrReference::Unknown,
            },
            apple: None,
            ripple,
        });
    }

    ToneMapUniform::from_settings(ToneMapUniformParams {
        common,
        input: ToneMapInputMetadata {
            color_space: image.metadata.color_space_hint(),
            transfer_function: image.metadata.transfer_function,
            reference: image.metadata.reference,
        },
        apple: None,
        ripple,
    })
}
