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

use super::tile_cache::hdr_tile_key_bytes;
use super::tone_map_uniform::tile_tone_map_uniform;
use super::upload::{
    pack_rows_for_texture_copy, rgba32f_as_bytes, validate_rgba8_upload_layout,
    validate_tile_upload_layout,
};
use super::*;
use crate::hdr::renderer::hdr_image_binding_is_eviction_candidate;
use crate::hdr::tiled::HdrTileBuffer;
use crate::hdr::types::{
    HdrColorSpace, HdrGainMapMetadata, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat,
    HdrReference, HdrToneMapSettings, HdrTransferFunction,
};
use std::sync::Arc;

#[test]
fn renderer_starts_without_uploaded_image_state() {
    let renderer = HdrImageRenderer::new();

    assert!(renderer.uploaded_image().is_none());
    assert_eq!(
        HDR_IMAGE_PLANE_TEXTURE_FORMAT,
        wgpu::TextureFormat::Rgba32Float
    );
}

#[test]
fn keep_resident_binding_expires_when_not_refreshed() {
    let last_use = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_millis(250))
        .expect("instant arithmetic");

    assert!(hdr_image_binding_is_eviction_candidate(
        true,
        last_use,
        std::time::Instant::now(),
    ));
}

#[test]
fn upload_layout_matches_rgba32f_rows() {
    let image = hdr_image(3, 2, HdrPixelFormat::Rgba32Float, vec![0.0; 3 * 2 * 4]);

    let layout = validate_upload_layout(&image, 4096).expect("valid upload layout");

    assert_eq!(layout.size.width, 3);
    assert_eq!(layout.size.height, 2);
    assert_eq!(
        layout.bytes_per_row,
        wgpu::util::align_to(
            3 * 4 * std::mem::size_of::<f32>() as u32,
            wgpu::COPY_BYTES_PER_ROW_ALIGNMENT
        )
    );
    assert_eq!(layout.format, wgpu::TextureFormat::Rgba32Float);
}

#[test]
fn rgba8_upload_layout_aligns_row_pitch_to_wgpu_copy_requirement() {
    let width = 3024;
    let height = 4032;
    let layout = validate_rgba8_upload_layout(
        width,
        height,
        width as usize * height as usize * 4,
        8192,
        "HEIC base upload",
    )
    .expect("valid rgba8 upload layout");

    assert_eq!(
        layout.bytes_per_row,
        wgpu::util::align_to(width * 4, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
    );
    assert_eq!(layout.bytes_per_row % wgpu::COPY_BYTES_PER_ROW_ALIGNMENT, 0);
}

#[test]
fn pack_rows_for_texture_copy_inserts_row_padding_when_required() {
    let width = 3024;
    let height = 2;
    let unpadded = (width * 4) as usize;
    let mut tight = vec![0u8; unpadded * height as usize];
    for y in 0..height {
        tight[y as usize * unpadded] = 100 + y as u8;
    }

    let (padded, bytes_per_row) =
        pack_rows_for_texture_copy(&tight, width, height, 4).expect("pack rows");

    assert_eq!(
        bytes_per_row,
        wgpu::util::align_to(width * 4, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
    );
    assert_eq!(padded.len(), bytes_per_row as usize * height as usize);
    assert_eq!(padded[0], 100);
    assert_eq!(padded[bytes_per_row as usize], 101);
    assert!(matches!(padded, Cow::Owned(_)));
}

#[test]
fn pack_rows_for_texture_copy_borrows_when_already_aligned() {
    let width = 64;
    let height = 2;
    let tight = vec![0u8; width as usize * height as usize * 4];
    let (packed, bytes_per_row) =
        pack_rows_for_texture_copy(&tight, width, height, 4).expect("pack rows");
    assert_eq!(bytes_per_row, width * 4);
    assert!(matches!(packed, Cow::Borrowed(_)));
    assert!(std::ptr::eq(packed.as_ptr(), tight.as_ptr()));
}

#[test]
fn pack_rows_rgba32f_round_trip_preserves_data() {
    // bytes_per_pixel=16 ->unpadded row = width * 16 bytes.
    // Test widths that are / are not multiples of 16 (alignment boundary).
    for &(width, height) in &[(13, 7), (16, 4), (17, 11), (64, 64), (4033, 3)] {
        let pixel_count = width as usize * height as usize * 4;
        let original: Vec<f32> = (0..pixel_count)
            .map(|i| (i as f32 * 0.12345 + 0.001).sin() * 2.0)
            .collect();
        let tight: &[u8] = bytemuck::cast_slice(&original);
        assert_eq!(tight.len(), width as usize * height as usize * 16);

        let (padded, bytes_per_row) =
            pack_rows_for_texture_copy(tight, width, height, 16).expect("pack rows");

        assert_eq!(bytes_per_row % wgpu::COPY_BYTES_PER_ROW_ALIGNMENT, 0);
        assert!(bytes_per_row >= width * 16);
        assert_eq!(padded.len(), bytes_per_row as usize * height as usize);

        // Simulate what `write_texture` does: extract each row's data (width * 16 bytes)
        // from the padded buffer, skipping padding.
        let unpadded_row = (width * 16) as usize;
        let mut unpacked = Vec::with_capacity(tight.len());
        for y in 0..height as usize {
            let src_start = y * bytes_per_row as usize;
            unpacked.extend_from_slice(&padded[src_start..src_start + unpadded_row]);
        }
        assert_eq!(unpacked, tight, "round-trip failed for {width}x{height}");
    }
}

#[test]
fn upload_layout_rejects_zero_dimensions() {
    let image = hdr_image(0, 1, HdrPixelFormat::Rgba32Float, Vec::new());

    let err = validate_upload_layout(&image, 4096).expect_err("reject zero-width upload");

    assert!(err.contains("non-zero"));
}

#[test]
fn upload_layout_rejects_malformed_buffer_length() {
    let image = hdr_image(2, 1, HdrPixelFormat::Rgba32Float, vec![0.0; 7]);

    let err = validate_upload_layout(&image, 4096).expect_err("reject malformed upload");

    assert!(err.contains("expected 8 floats"));
    assert!(err.contains("got 7"));
}

#[test]
fn upload_layout_rejects_unsupported_cpu_format() {
    let image = hdr_image(1, 1, HdrPixelFormat::Rgba16Float, vec![0.0; 4]);

    let err = validate_upload_layout(&image, 4096).expect_err("reject unsupported format");

    assert!(err.contains("Rgba32Float"));
}

#[test]
fn upload_layout_rejects_device_texture_limit_overflow() {
    let image = hdr_image(
        2049,
        1024,
        HdrPixelFormat::Rgba32Float,
        vec![0.0; 2049 * 1024 * 4],
    );

    let err = validate_upload_layout(&image, 2048).expect_err("reject texture limit overflow");

    assert!(err.contains("exceed"));
    assert!(err.contains("2048"));
}

#[test]
fn tile_upload_layout_matches_rgba32f_rows() {
    let tile = hdr_tile(7, 5, vec![0.0; 7 * 5 * 4]);

    let layout = validate_tile_upload_layout(&tile, 4096).expect("valid tile upload layout");

    assert_eq!(layout.size.width, 7);
    assert_eq!(layout.size.height, 5);
    assert_eq!(
        layout.bytes_per_row,
        wgpu::util::align_to(
            7 * 4 * std::mem::size_of::<f32>() as u32,
            wgpu::COPY_BYTES_PER_ROW_ALIGNMENT
        )
    );
    assert_eq!(layout.format, wgpu::TextureFormat::Rgba32Float);
}

#[test]
fn tile_upload_layout_rejects_malformed_buffer_length() {
    let tile = hdr_tile(2, 2, vec![0.0; 15]);

    let err = validate_tile_upload_layout(&tile, 4096).expect_err("reject malformed tile");

    assert!(err.contains("expected 16 floats"));
    assert!(err.contains("got 15"));
}

#[test]
fn hdr_tile_plane_callback_returns_paint_callback_shape() {
    let tile = Arc::new(hdr_tile(1, 1, vec![1.0, 1.0, 1.0, 1.0]));
    let shape = hdr_tile_plane_callback(
        egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(10.0, 10.0)),
        tile,
        HdrToneMapSettings::default(),
        wgpu::TextureFormat::Rgba16Float,
        HdrRenderOutputMode::NativeHdr,
        0,
        1.0,
    );

    assert!(matches!(shape, egui::Shape::Callback(_)));
}

#[test]
fn tone_map_uniform_byte_size_matches_wgpu_shader() {
    assert_eq!(std::mem::size_of::<ToneMapUniform>(), 128);
}

#[test]
fn libavif_tone_map_native_display_scale_matches_encode_sdr_peak_scaler() {
    let mut metadata = HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb);
    metadata.gain_map = Some(HdrGainMapMetadata {
        source: "AVIF",
        target_hdr_capacity: Some(4.0),
        diagnostic: String::new(),
        capped_display_referred: true,
        apple_heic_deferred: None,
        iso_deferred: None,
    });
    let tone = HdrToneMapSettings {
        sdr_white_nits: 203.0,
        max_display_nits: 1000.0,
        ..HdrToneMapSettings::default()
    };
    let s = libavif_tone_map_native_display_scale(&metadata, HdrColorSpace::LinearSrgb, &tone);
    assert!((s - 203.0 / 1000.0).abs() < 1e-5);
}

#[test]
fn tile_tone_map_uniform_carries_rotation() {
    let uniform = tile_tone_map_uniform(
        ToneMapCommonParams {
            settings: HdrToneMapSettings::default(),
            rotation_steps: 6,
            alpha: 0.5,
            output_mode: HdrRenderOutputMode::NativeHdr,
            framebuffer_format: wgpu::TextureFormat::Rgba16Float,
            uv_rect: egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
            native_display_scale: 1.0,
        },
        ToneMapInputMetadata {
            color_space: HdrColorSpace::LinearSrgb,
            transfer_function: HdrTransferFunction::Linear,
            reference: HdrReference::Unknown,
        },
    );

    assert_eq!(uniform.rotation_steps, 2);
    assert_eq!(uniform.alpha, 0.5);
    assert_eq!(uniform.output_mode, HdrRenderOutputMode::NativeHdr as u32);
}

#[test]
fn tile_tone_map_uniform_carries_uv_subrect() {
    let uniform = tile_tone_map_uniform(
        ToneMapCommonParams {
            settings: HdrToneMapSettings::default(),
            rotation_steps: 0,
            alpha: 1.0,
            output_mode: HdrRenderOutputMode::NativeHdr,
            framebuffer_format: wgpu::TextureFormat::Rgba16Float,
            uv_rect: egui::Rect::from_min_max(
                egui::Pos2::new(0.25, 0.5),
                egui::Pos2::new(0.75, 1.0),
            ),
            native_display_scale: 1.0,
        },
        ToneMapInputMetadata {
            color_space: HdrColorSpace::LinearSrgb,
            transfer_function: HdrTransferFunction::Linear,
            reference: HdrReference::Unknown,
        },
    );

    assert_eq!(uniform.uv_min, [0.25, 0.5]);
    assert_eq!(uniform.uv_max, [0.75, 1.0]);
}

#[test]
fn image_and_tile_uniforms_share_transform_output_and_color_space_logic() {
    let settings = HdrToneMapSettings {
        exposure_ev: 1.0,
        sdr_white_nits: 203.0,
        max_display_nits: 1000.0,
    };

    let image = HdrImageBuffer {
        width: 1,
        height: 1,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::Rec2020Linear,
        metadata: HdrImageMetadata {
            transfer_function: HdrTransferFunction::Linear,
            reference: HdrReference::Unknown,
            ..HdrImageMetadata::from_color_space(HdrColorSpace::Rec2020Linear)
        },
        rgba_f32: Arc::new(vec![1.0, 0.0, 0.0, 1.0]),
    };

    let image_uniform = image_tone_map_uniform(
        &image,
        ImageToneMapUniformParams {
            common: ToneMapCommonParams {
                settings,
                rotation_steps: 5,
                alpha: 0.75,
                output_mode: HdrRenderOutputMode::SdrToneMapped,
                framebuffer_format: wgpu::TextureFormat::Bgra8UnormSrgb,
                uv_rect: egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
                native_display_scale: 1.0,
            },
            gpu_composed_scene_linear: false,
            ripple: None,
        },
    );
    let tile_uniform = tile_tone_map_uniform(
        ToneMapCommonParams {
            settings,
            rotation_steps: 5,
            alpha: 0.75,
            output_mode: HdrRenderOutputMode::SdrToneMapped,
            framebuffer_format: wgpu::TextureFormat::Bgra8UnormSrgb,
            uv_rect: egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
            native_display_scale: 1.0,
        },
        ToneMapInputMetadata {
            color_space: HdrColorSpace::Rec2020Linear,
            transfer_function: HdrTransferFunction::Linear,
            reference: HdrReference::Unknown,
        },
    );

    assert_eq!(image_uniform.rotation_steps, tile_uniform.rotation_steps);
    assert_eq!(image_uniform.alpha, tile_uniform.alpha);
    assert_eq!(image_uniform.output_mode, tile_uniform.output_mode);
    assert_eq!(
        image_uniform.input_color_space,
        tile_uniform.input_color_space
    );
    assert_eq!(
        image_uniform.output_mode,
        HdrRenderOutputMode::SdrToneMapped as u32
    );
    assert_eq!(image_uniform.sdr_manual_srgb_encode, 0);
    assert_eq!(tile_uniform.sdr_manual_srgb_encode, 0);
}

#[test]
fn image_tone_map_uniform_marks_deferred_gpu_compose_as_scene_linear() {
    let mut metadata = HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb);
    metadata.transfer_function = HdrTransferFunction::Srgb;
    metadata.reference = HdrReference::SdrGainMapBase;
    metadata.gain_map = Some(HdrGainMapMetadata {
        source: "JPEG XL",
        target_hdr_capacity: Some(4.0),
        diagnostic: "test".to_string(),
        capped_display_referred: false,
        apple_heic_deferred: None,
        iso_deferred: None,
    });
    let image = HdrImageBuffer {
        width: 1,
        height: 1,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::LinearSrgb,
        metadata,
        rgba_f32: Arc::new(Vec::new()),
    };
    let uniform = image_tone_map_uniform(
        &image,
        ImageToneMapUniformParams {
            common: ToneMapCommonParams {
                settings: HdrToneMapSettings::default(),
                rotation_steps: 0,
                alpha: 1.0,
                output_mode: HdrRenderOutputMode::SdrToneMapped,
                framebuffer_format: wgpu::TextureFormat::Bgra8UnormSrgb,
                uv_rect: egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
                native_display_scale: 1.0,
            },
            gpu_composed_scene_linear: true,
            ripple: None,
        },
    );
    assert_eq!(
        uniform.input_transfer_function,
        HdrTransferFunction::Linear as u32
    );
    assert_eq!(uniform.input_reference, HdrReference::Unknown as u32);
}

#[test]
fn tone_map_manual_srgb_oetf_plain_unorm_only() {
    assert!(
        crate::hdr::renderer::hdr_sdr_framebuffer_needs_manual_srgb_oetf(
            wgpu::TextureFormat::Bgra8Unorm
        )
    );
    assert!(
        crate::hdr::renderer::hdr_sdr_framebuffer_needs_manual_srgb_oetf(
            wgpu::TextureFormat::Rgba8Unorm
        )
    );
    assert!(
        !crate::hdr::renderer::hdr_sdr_framebuffer_needs_manual_srgb_oetf(
            wgpu::TextureFormat::Bgra8UnormSrgb
        )
    );
    assert!(
        !crate::hdr::renderer::hdr_sdr_framebuffer_needs_manual_srgb_oetf(
            wgpu::TextureFormat::Rgba8UnormSrgb
        )
    );
}

#[test]
fn render_output_diagnostics_distinguish_native_hdr_and_sdr_tone_mapping() {
    assert_eq!(
        hdr_render_output_diagnostics(Some(wgpu::TextureFormat::Rgba16Float)),
        [
            "[HDR] render_target_format=Some(Rgba16Float)",
            "[HDR] shader_output_mode=native_hdr",
        ]
    );
    assert_eq!(
        hdr_render_output_diagnostics(Some(wgpu::TextureFormat::Bgra8Unorm)),
        [
            "[HDR] render_target_format=Some(Bgra8Unorm)",
            "[HDR] shader_output_mode=sdr_tone_mapped",
        ]
    );
    assert_eq!(
        hdr_render_output_diagnostics(None),
        [
            "[HDR] render_target_format=None",
            "[HDR] shader_output_mode=unknown",
        ]
    );
}

#[test]
fn egui_overlay_diagnostics_report_linear_sdr_ui_on_hdr_float_target() {
    assert_eq!(
        hdr_egui_overlay_diagnostics(Some(wgpu::TextureFormat::Rgba16Float)),
        [
            "[HDR] egui_overlay_target_format=Some(Rgba16Float)",
            "[HDR] egui_overlay_framebuffer_shader=fs_main_linear_framebuffer",
        ]
    );
    assert_eq!(
        hdr_egui_overlay_diagnostics(Some(wgpu::TextureFormat::Bgra8Unorm)),
        [
            "[HDR] egui_overlay_target_format=Some(Bgra8Unorm)",
            "[HDR] egui_overlay_framebuffer_shader=fs_main_gamma_framebuffer",
        ]
    );
}

#[test]
fn hdr_tile_keys_distinguish_equal_size_tile_buffers() {
    let first = hdr_tile(1, 1, vec![1.0, 0.0, 0.0, 1.0]);
    let second = hdr_tile(1, 1, vec![0.0, 1.0, 0.0, 1.0]);

    assert_ne!(
        HdrTileKey::from_tile(&first),
        HdrTileKey::from_tile(&second)
    );
}

#[test]
fn hdr_tile_keys_distinguish_logical_tiles_even_when_rgba_allocation_matches() {
    let rgba = Arc::new(vec![1.0, 0.0, 0.0, 1.0]);
    let first = HdrTileBuffer::new(1, 1, HdrColorSpace::LinearSrgb, Arc::clone(&rgba));
    let second = HdrTileBuffer::new(1, 1, HdrColorSpace::LinearSrgb, rgba);

    assert_ne!(
        HdrTileKey::from_tile(&first),
        HdrTileKey::from_tile(&second)
    );
}

#[test]
fn hdr_tile_keys_distinguish_uv_subrects() {
    let tile = hdr_tile(2, 2, vec![1.0; 2 * 2 * 4]);
    let full = HdrTileKey::from_tile_with_uv(
        &tile,
        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
    );
    let clipped = HdrTileKey::from_tile_with_uv(
        &tile,
        egui::Rect::from_min_max(egui::Pos2::new(0.5, 0.0), egui::Pos2::new(1.0, 1.0)),
    );

    assert_ne!(full, clipped);
}

#[test]
fn callback_resources_store_independent_tile_bind_groups() {
    let first = HdrTileKey::from_tile(&hdr_tile(1, 1, vec![1.0, 0.0, 0.0, 1.0]));
    let second = HdrTileKey::from_tile(&hdr_tile(1, 1, vec![0.0, 1.0, 0.0, 1.0]));
    let mut resources = HdrTileBindings::default();

    resources.insert_placeholder(first);
    resources.insert_placeholder(second);

    assert!(resources.contains(first));
    assert!(resources.contains(second));
    assert_eq!(resources.len(), 2);
}

#[test]
fn callback_resources_evict_lru_tile_bindings_when_over_budget() {
    let first = HdrTileKey::from_tile(&hdr_tile(1, 1, vec![1.0, 0.0, 0.0, 1.0]));
    let second = HdrTileKey::from_tile(&hdr_tile(1, 1, vec![0.0, 1.0, 0.0, 1.0]));
    let third = HdrTileKey::from_tile(&hdr_tile(1, 1, vec![0.0, 0.0, 1.0, 1.0]));
    let mut resources = HdrTileBindings::with_budget(2 * hdr_tile_key_bytes(first));

    resources.insert_placeholder(first);
    resources.insert_placeholder(second);
    resources.insert_placeholder(third);

    assert!(!resources.contains(first));
    assert!(resources.contains(second));
    assert!(resources.contains(third));
    assert_eq!(resources.len(), 2);
    assert!(resources.current_bytes() <= 2 * hdr_tile_key_bytes(first));
}

#[test]
fn callback_resources_keep_recently_prepared_tile_bindings_over_budget() {
    let first = HdrTileKey::from_tile(&hdr_tile(1, 1, vec![1.0, 0.0, 0.0, 1.0]));
    let second = HdrTileKey::from_tile(&hdr_tile(1, 1, vec![0.0, 1.0, 0.0, 1.0]));
    let third = HdrTileKey::from_tile(&hdr_tile(1, 1, vec![0.0, 0.0, 1.0, 1.0]));
    let mut resources = HdrTileBindings::with_budget(2 * hdr_tile_key_bytes(first));

    resources.insert_protected_placeholder(first);
    resources.insert_protected_placeholder(second);
    resources.insert_protected_placeholder(third);

    assert!(resources.contains(first));
    assert!(resources.contains(second));
    assert!(resources.contains(third));
    assert_eq!(resources.len(), 3);
    assert!(resources.current_bytes() > 2 * hdr_tile_key_bytes(first));
}

#[test]
fn callback_resources_refresh_lru_on_existing_tile_binding() {
    let first = HdrTileKey::from_tile(&hdr_tile(1, 1, vec![1.0, 0.0, 0.0, 1.0]));
    let second = HdrTileKey::from_tile(&hdr_tile(1, 1, vec![0.0, 1.0, 0.0, 1.0]));
    let third = HdrTileKey::from_tile(&hdr_tile(1, 1, vec![0.0, 0.0, 1.0, 1.0]));
    let mut resources = HdrTileBindings::with_budget(2 * hdr_tile_key_bytes(first));

    resources.insert_placeholder(first);
    resources.insert_placeholder(second);
    assert!(resources.contains(first));
    resources.insert_placeholder(third);

    assert!(resources.contains(first));
    assert!(!resources.contains(second));
    assert!(resources.contains(third));
}

#[test]
fn shader_sanitizes_non_finite_hdr_rgb_before_tone_mapping() {
    assert!(HDR_IMAGE_PLANE_SHADER.contains("fn sanitize_hdr_rgb"));
    assert!(HDR_IMAGE_PLANE_SHADER.contains("safe.r != safe.r"));
    assert!(HDR_IMAGE_PLANE_SHADER.contains("const MAX_FINITE_HDR_VALUE: f32"));
    assert!(HDR_IMAGE_PLANE_SHADER.contains("clamp("));
}

#[test]
fn shader_names_tone_map_numeric_constants() {
    assert!(HDR_IMAGE_PLANE_SHADER.contains("const INVERSE_DISPLAY_GAMMA: f32"));
    assert!(HDR_IMAGE_PLANE_SHADER.contains("const MAX_UV_CLAMP: f32"));
    assert!(HDR_IMAGE_PLANE_SHADER.contains("vec3<f32>(INVERSE_DISPLAY_GAMMA)"));
    assert!(HDR_IMAGE_PLANE_SHADER.contains("vec2<f32>(MAX_UV_CLAMP)"));
}

#[test]
fn rgba32f_byte_view_does_not_allocate_or_copy() {
    let values = [1.0, -2.5, 0.25, f32::INFINITY];

    let bytes = rgba32f_as_bytes(&values);

    assert_eq!(bytes.len(), values.len() * std::mem::size_of::<f32>());
    assert_eq!(bytes.as_ptr(), values.as_ptr().cast::<u8>());
    assert_eq!(&bytes[0..4], &1.0_f32.to_ne_bytes());
}

#[test]
fn tone_map_uniform_carries_rotation_and_alpha() {
    let uniform = ToneMapUniform::from_settings(ToneMapUniformParams {
        common: ToneMapCommonParams {
            settings: HdrToneMapSettings::default(),
            rotation_steps: 5,
            alpha: 0.25,
            output_mode: HdrRenderOutputMode::SdrToneMapped,
            framebuffer_format: wgpu::TextureFormat::Bgra8Unorm,
            uv_rect: egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
            native_display_scale: 1.0,
        },
        input: ToneMapInputMetadata {
            color_space: HdrColorSpace::LinearSrgb,
            transfer_function: HdrTransferFunction::Linear,
            reference: HdrReference::Unknown,
        },
        apple: None,
        ripple: None,
    });

    assert_eq!(uniform.rotation_steps, 1);
    assert_eq!(uniform.alpha, 0.25);
    assert_eq!(uniform.sdr_manual_srgb_encode, 1);
}

#[test]
fn render_mode_uses_native_hdr_for_float_and_pq_targets() {
    use crate::hdr::monitor::HdrNativeSurfaceEncoding;
    assert_eq!(
        HdrRenderOutputMode::for_target_format(wgpu::TextureFormat::Rgba16Float, None,),
        HdrRenderOutputMode::NativeHdr
    );
    assert_eq!(
        HdrRenderOutputMode::for_target_format(wgpu::TextureFormat::Rgba32Float, None,),
        HdrRenderOutputMode::NativeHdr
    );
    assert_eq!(
        HdrRenderOutputMode::for_target_format(
            wgpu::TextureFormat::Rgb10a2Unorm,
            Some(HdrNativeSurfaceEncoding::PqHdr10),
        ),
        HdrRenderOutputMode::NativeHdrPq
    );
    assert_eq!(
        HdrRenderOutputMode::for_target_format(
            wgpu::TextureFormat::Rgb10a2Unorm,
            Some(HdrNativeSurfaceEncoding::Gamma22Electrical),
        ),
        HdrRenderOutputMode::NativeHdrGamma22
    );
    assert_eq!(
        HdrRenderOutputMode::for_target_format(wgpu::TextureFormat::Rgb10a2Unorm, None,),
        HdrRenderOutputMode::SdrToneMapped
    );
    assert_eq!(
        HdrRenderOutputMode::for_target_format(wgpu::TextureFormat::Bgra8Unorm, None,),
        HdrRenderOutputMode::SdrToneMapped
    );
}

#[test]
fn tone_map_uniform_carries_output_mode() {
    let uniform = ToneMapUniform::from_settings(ToneMapUniformParams {
        common: ToneMapCommonParams {
            settings: HdrToneMapSettings::default(),
            rotation_steps: 0,
            alpha: 1.0,
            output_mode: HdrRenderOutputMode::NativeHdr,
            framebuffer_format: wgpu::TextureFormat::Bgra8Unorm,
            uv_rect: egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
            native_display_scale: 1.0,
        },
        input: ToneMapInputMetadata {
            color_space: HdrColorSpace::Rec2020Linear,
            transfer_function: HdrTransferFunction::Pq,
            reference: HdrReference::DisplayReferred,
        },
        apple: None,
        ripple: None,
    });

    assert_eq!(uniform.output_mode, HdrRenderOutputMode::NativeHdr as u32);
    assert_eq!(uniform.sdr_manual_srgb_encode, 0);
    assert_eq!(
        uniform.input_color_space,
        HdrColorSpace::Rec2020Linear as u32
    );
    assert_eq!(
        uniform.input_transfer_function,
        HdrTransferFunction::Pq as u32
    );
    assert_eq!(
        uniform.input_reference,
        HdrReference::DisplayReferred as u32
    );
}

#[test]
fn shader_converts_rec2020_input_to_linear_srgb() {
    assert!(HDR_IMAGE_PLANE_SHADER.contains("INPUT_COLOR_SPACE_REC2020_LINEAR"));
    assert!(HDR_IMAGE_PLANE_SHADER.contains("INPUT_COLOR_SPACE_DISPLAY_P3_LINEAR"));
    assert!(HDR_IMAGE_PLANE_SHADER.contains("fn convert_input_to_linear_srgb"));
    assert!(HDR_IMAGE_PLANE_SHADER.contains("1.6605"));
}

#[test]
fn shader_converts_aces2065_1_input_to_linear_srgb() {
    assert!(HDR_IMAGE_PLANE_SHADER.contains("INPUT_COLOR_SPACE_ACES2065_1"));
    assert!(HDR_IMAGE_PLANE_SHADER.contains("fn aces2065_1_to_linear_srgb"));
    assert!(HDR_IMAGE_PLANE_SHADER.contains("2.5216"));
}

#[test]
fn shader_converts_xyz_input_to_linear_srgb() {
    assert!(HDR_IMAGE_PLANE_SHADER.contains("INPUT_COLOR_SPACE_XYZ"));
    assert!(HDR_IMAGE_PLANE_SHADER.contains("fn xyz_to_linear_srgb"));
    assert!(HDR_IMAGE_PLANE_SHADER.contains("3.2404"));
}

#[test]
fn shader_decodes_hdr_transfer_functions_before_color_conversion() {
    assert!(HDR_IMAGE_PLANE_SHADER.contains("INPUT_TRANSFER_PQ"));
    assert!(HDR_IMAGE_PLANE_SHADER.contains("INPUT_TRANSFER_HLG"));
    assert!(HDR_IMAGE_PLANE_SHADER.contains("INPUT_TRANSFER_BT709"));
    assert!(HDR_IMAGE_PLANE_SHADER.contains("fn pq_to_display_linear"));
    assert!(HDR_IMAGE_PLANE_SHADER.contains("fn bt709_nonlinear_to_linear"));
    assert!(HDR_IMAGE_PLANE_SHADER.contains("fn hlg_to_scene_linear"));
    assert!(HDR_IMAGE_PLANE_SHADER.contains("fn decode_input_transfer"));
    assert!(HDR_IMAGE_PLANE_SHADER.contains("sdr_manual_srgb_encode"));
    assert!(HDR_IMAGE_PLANE_SHADER.contains("manual_oetf"));
}

#[test]
fn shader_outputs_straight_alpha_for_standard_blending() {
    assert!(HDR_IMAGE_PLANE_SHADER.contains("fn encode_native_hdr"));
    assert!(HDR_IMAGE_PLANE_SHADER.contains("if tone_map.output_mode == OUTPUT_MODE_NATIVE_HDR"));
    assert!(HDR_IMAGE_PLANE_SHADER.contains("src_a = clamp(hdr.a, 0.0, 1.0)"));
    assert!(HDR_IMAGE_PLANE_SHADER.contains("a_out * tone_map.alpha"));
    assert!(!HDR_IMAGE_PLANE_SHADER.contains("encode_sdr(hdr.rgb, tone_map) * tone_map.alpha"));
}

#[test]
fn apple_heic_display_never_uses_per_fragment_compose() {
    assert!(!HDR_IMAGE_PLANE_SHADER.contains("tone_map.apple_compose != 0u"));
    assert!(!HDR_IMAGE_PLANE_SHADER.contains("fn sample_apple_gain_encoded_at_primary_pixel"));
    assert!(HDR_IMAGE_PLANE_SHADER.contains("fn sample_hdr_for_display"));
}

#[test]
#[cfg(feature = "heif-native")]
fn apple_gain_map_gpu_compose_entry_point_exists() {
    use super::apple_compose_gpu::APPLE_GAIN_COMPOSE_SHADER;

    assert!(APPLE_GAIN_COMPOSE_SHADER.contains("fn cs_compose_apple_gain"));
    assert!(APPLE_GAIN_COMPOSE_SHADER.contains("var<storage, read> encoded_primary"));
    assert!(APPLE_GAIN_COMPOSE_SHADER.contains("compose_row_offset"));
    assert!(
        APPLE_GAIN_COMPOSE_SHADER
            .contains("compose_apple_at_primary_pixel(px, py, local_py, tone_map)")
    );
}

#[test]
fn native_hdr_pq_shader_encodes_pq_for_rgb10a2_target() {
    assert!(HDR_IMAGE_PLANE_SHADER.contains("OUTPUT_MODE_NATIVE_HDR_PQ"));
    assert!(HDR_IMAGE_PLANE_SHADER.contains("fn encode_native_hdr_pq"));
    assert!(HDR_IMAGE_PLANE_SHADER.contains("fn display_linear_to_pq"));
    assert!(HDR_IMAGE_PLANE_SHADER.contains("const PQ_REFERENCE_LUMINANCE_NITS: f32 = 10000.0"));
    assert!(HDR_IMAGE_PLANE_SHADER.contains("nits / vec3<f32>(PQ_REFERENCE_LUMINANCE_NITS)"));
    assert!(HDR_IMAGE_PLANE_SHADER.contains("OUTPUT_MODE_NATIVE_HDR_GAMMA22"));
    assert!(HDR_IMAGE_PLANE_SHADER.contains("fn encode_native_hdr_gamma22"));
}

#[test]
fn native_hdr_encoders_share_exposed_linear_rgb() {
    assert!(HDR_IMAGE_PLANE_SHADER.contains("fn exposed_linear_rgb"));
    assert!(HDR_IMAGE_PLANE_SHADER.contains("return exposed_linear_rgb(rgb, settings);"));
    assert!(HDR_IMAGE_PLANE_SHADER.contains("display_linear_to_pq(exposed_linear_rgb"));
    assert!(
        HDR_IMAGE_PLANE_SHADER.contains("exposed_linear_rgb(rgb, settings) * display_scale")
            || HDR_IMAGE_PLANE_SHADER
                .contains("scene_linear_to_display_referred(exposed) * display_scale")
    );
    assert!(!HDR_IMAGE_PLANE_SHADER.contains("fn encode_scene_linear_kwin_gamma22"));
    assert!(!HDR_IMAGE_PLANE_SHADER.contains("fn compress_scene_linear_highlights"));
    assert!(!HDR_IMAGE_PLANE_SHADER.contains("reinhard_tone_map_luminance_preserved"));
    assert!(HDR_IMAGE_PLANE_SHADER.contains("fn scene_linear_to_display_referred"));
    assert!(
        HDR_IMAGE_PLANE_SHADER
            .contains("scene_linear_to_display_referred(exposed) * display_scale")
    );
    assert!(
        HDR_IMAGE_PLANE_SHADER
            .contains("if (settings.input_transfer_function == INPUT_TRANSFER_LINEAR)"),
        "scene-linear needs display-referred mapping before KWin gamma 2.2 OETF"
    );
}

#[test]
fn native_hdr_shader_outputs_linear_scrgb_without_gamma_encoding() {
    // scRGB native HDR is linear; 纬2.2 inflates shadows and destroys SDR contrast on
    // physically SDR displays advertising HDR support (conformance `bench_oriented_brg`).
    assert!(HDR_IMAGE_PLANE_SHADER.contains("fn encode_native_hdr"));
    assert!(
        !HDR_IMAGE_PLANE_SHADER.contains("let sdr_base ="),
        "encode_native_hdr must not 纬-encode for scRGB output"
    );
    assert!(
        !HDR_IMAGE_PLANE_SHADER.contains("return max(sdr_base, exposed);"),
        "encode_native_hdr must return exposed linear value, no 纬-blend"
    );
}

#[test]
fn shader_averages_hdr_texels_when_downscaling() {
    assert!(HDR_IMAGE_PLANE_SHADER.contains("fn sample_hdr_for_display"));
    assert!(HDR_IMAGE_PLANE_SHADER.contains("fn bilinear_load_hdr"));
    assert!(HDR_IMAGE_PLANE_SHADER.contains("premultiply_hdr_rgba"));
    assert!(HDR_IMAGE_PLANE_SHADER.contains("HDR_DOWNSCALE_SAMPLE_GRID"));
    assert!(HDR_IMAGE_PLANE_SHADER.contains("dpdx(uv)"));
    assert!(HDR_IMAGE_PLANE_SHADER.contains("sum += premultiply_hdr_rgba"));
}

#[test]
fn shader_uses_wgsl_if_statement_for_output_mode_selection() {
    assert!(
        !HDR_IMAGE_PLANE_SHADER.contains("let rgb = if "),
        "WGSL/Naga rejects Rust-style if expressions in shader code"
    );
    assert!(HDR_IMAGE_PLANE_SHADER.contains("var rgb: vec3<f32>;"));
}

#[test]
fn hdr_image_plane_shader_parses_as_wgsl() {
    naga::front::wgsl::parse_str(HDR_IMAGE_PLANE_SHADER)
        .expect("HDR image plane shader must parse before runtime pipeline creation");
}

fn hdr_image(
    width: u32,
    height: u32,
    format: HdrPixelFormat,
    rgba_f32: Vec<f32>,
) -> HdrImageBuffer {
    HdrImageBuffer {
        width,
        height,
        format,
        color_space: HdrColorSpace::LinearSrgb,
        metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
        rgba_f32: Arc::new(rgba_f32),
    }
}

fn hdr_tile(width: u32, height: u32, rgba_f32: Vec<f32>) -> HdrTileBuffer {
    HdrTileBuffer::new(width, height, HdrColorSpace::LinearSrgb, Arc::new(rgba_f32))
}

fn drain_pending_plane_uploads_for_test(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pending: &Arc<HdrPendingWorkQueues>,
    callback_resources: &mut egui_wgpu::CallbackResources,
    target_format: wgpu::TextureFormat,
    device_id: u64,
) {
    let requests = std::mem::take(&mut *pending.plane_upload_requests.lock());
    for request in requests {
        let key = request.key;
        match upload_image_plane_with_sink(
            device,
            GpuUploadSink::Pending {
                queues: pending.as_ref(),
                stage: HdrGpuUploadStage::PlaneCreate,
            },
            &request.image,
        ) {
            Ok(uploaded) => {
                let _ = pending.flush_staged_writes_for_registration(queue);
                let binding = HdrImageBinding::from_uploaded(
                    device,
                    uploaded,
                    &request.image,
                    request.tone_map,
                    request.target_format,
                    request.output_mode,
                    device_id,
                );
                if let Some(resources) = callback_resources
                    .get_mut::<HdrCallbackResourcesSet>()
                    .and_then(|set| set.get_for_mut(target_format))
                {
                    let _ = resources.register_preuploaded_binding(key, binding, device_id);
                    resources.set_image_binding_keep_resident(key, request.keep_resident);
                }
                pending.clear_plane_upload_inflight(key);
            }
            Err(err) => {
                log::warn!("[HDR] Test plane upload failed: {err}");
                pending.clear_plane_upload_inflight(key);
            }
        }
    }
}

#[test]
fn test_hdr_renderer_multi_binding_and_lru_eviction() {
    let Some((_instance, _adapter, device, queue)) = pollster::block_on(async {
        let instance = wgpu::Instance::default();
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                force_fallback_adapter: true,
                compatible_surface: None,
            })
            .await
            .ok()?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await
            .ok()?;
        Some((instance, adapter, device, queue))
    }) else {
        log::warn!("Skipping GPU test: no adapter available");
        return;
    };

    let mut callback_resources = CallbackResources::default();
    let target_format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut set = HdrCallbackResourcesSet::default();
    set.insert_format(create_callback_resources(&device, target_format, None));
    callback_resources.insert(set);

    let images: Vec<_> = (1..=9)
        .map(|i| {
            let size = i * 10;
            let pixels = (size * size * 4) as usize;
            Arc::new(hdr_image(
                size,
                size,
                HdrPixelFormat::Rgba32Float,
                vec![1.0; pixels],
            ))
        })
        .collect();

    let screen_desc = egui_wgpu::ScreenDescriptor {
        size_in_pixels: [100, 100],
        pixels_per_point: 1.0,
    };

    let pending_work = HdrPendingWorkQueues::new_shared();
    const TEST_DEVICE_ID: u64 = 0;

    // Prepare eight callbacks (sleeping so LRU timestamps are distinct).
    for (i, img) in images.iter().take(8).enumerate() {
        if i > 0 {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let callback = HdrImagePlaneCallback {
            image: Arc::clone(img),
            tone_map: HdrToneMapSettings::default(),
            target_format,
            output_mode: HdrRenderOutputMode::SdrToneMapped,
            rotation_steps: 0,
            alpha: 1.0,
            uv_rect: egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
            ripple: None,
            keep_resident: false,
            raw_demosaic_baked_notify: None,
            pending_work: Some(Arc::clone(&pending_work)),
            sync_plane_upload_on_cache_miss: false,
        };

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        let cmds = callback.prepare(
            &device,
            &queue,
            &screen_desc,
            &mut encoder,
            &mut callback_resources,
        );
        if !cmds.is_empty() {
            queue.submit(cmds);
        }
        drain_pending_plane_uploads_for_test(
            &device,
            &queue,
            &pending_work,
            &mut callback_resources,
            target_format,
            TEST_DEVICE_ID,
        );
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        let cmds = callback.prepare(
            &device,
            &queue,
            &screen_desc,
            &mut encoder,
            &mut callback_resources,
        );
        if !cmds.is_empty() {
            queue.submit(cmds);
        }
    }

    // Verify that we have exactly eight bindings in resources and they are independent
    {
        let resources = callback_resources
            .get::<HdrCallbackResourcesSet>()
            .and_then(|set| set.get_for(target_format))
            .unwrap();
        assert_eq!(resources.image_bindings.len(), 8);

        let key0 = HdrImageKey::from_image(&images[0]);
        let key1 = HdrImageKey::from_image(&images[1]);
        let key7 = HdrImageKey::from_image(&images[7]);

        let b0 = resources.image_bindings.get(&key0).unwrap();
        let b1 = resources.image_bindings.get(&key1).unwrap();
        let b7 = resources.image_bindings.get(&key7).unwrap();

        assert!(b0.bind_group.is_some());
        assert!(b1.bind_group.is_some());
        assert!(b7.bind_group.is_some());

        assert_eq!(b0.uploaded_texture.width(), 10);
        assert_eq!(b1.uploaded_texture.width(), 20);
        assert_eq!(b7.uploaded_texture.width(), 80);
    }

    // Age out the oldest binding past the eviction-protect window, then insert a ninth image.
    std::thread::sleep(std::time::Duration::from_millis(60));

    // Now prepare the 9th image callback. This should trigger eviction of the oldest (the 1st one)
    {
        let callback = HdrImagePlaneCallback {
            image: Arc::clone(&images[8]),
            tone_map: HdrToneMapSettings::default(),
            target_format,
            output_mode: HdrRenderOutputMode::SdrToneMapped,
            rotation_steps: 0,
            alpha: 1.0,
            uv_rect: egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
            ripple: None,
            keep_resident: false,
            raw_demosaic_baked_notify: None,
            pending_work: Some(Arc::clone(&pending_work)),
            sync_plane_upload_on_cache_miss: false,
        };

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        let cmds = callback.prepare(
            &device,
            &queue,
            &screen_desc,
            &mut encoder,
            &mut callback_resources,
        );
        if !cmds.is_empty() {
            queue.submit(cmds);
        }
        drain_pending_plane_uploads_for_test(
            &device,
            &queue,
            &pending_work,
            &mut callback_resources,
            target_format,
            TEST_DEVICE_ID,
        );
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        let cmds = callback.prepare(
            &device,
            &queue,
            &screen_desc,
            &mut encoder,
            &mut callback_resources,
        );
        if !cmds.is_empty() {
            queue.submit(cmds);
        }
    }

    // Verify that resources has size 8 and images[0] has been evicted
    {
        let resources = callback_resources
            .get::<HdrCallbackResourcesSet>()
            .and_then(|set| set.get_for(target_format))
            .unwrap();
        assert_eq!(resources.image_bindings.len(), 8);

        let key_evicted = HdrImageKey::from_image(&images[0]);
        assert!(!resources.image_bindings.contains_key(&key_evicted));

        for img in images.iter().skip(1) {
            let key = HdrImageKey::from_image(img);
            assert!(resources.image_bindings.contains_key(&key));
        }
    }
}

#[test]
fn gpu_preview_tone_map_matches_cpu_for_linear_srgb() {
    let Some((_instance, _adapter, device, queue)) = pollster::block_on(async {
        let instance = wgpu::Instance::default();
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                force_fallback_adapter: true,
                compatible_surface: None,
            })
            .await
            .ok()?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await
            .ok()?;
        Some((instance, adapter, device, queue))
    }) else {
        log::warn!("Skipping GPU preview tone-map test: no adapter available");
        return;
    };

    let width = 256_u32;
    let height = 256_u32;
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            let v = (x as f32 / width as f32) * 2.0;
            rgba.extend_from_slice(&[v, v * 0.5, v * 0.25, 1.0]);
            let _ = y;
        }
    }
    let buffer = hdr_image(width, height, HdrPixelFormat::Rgba32Float, rgba);
    let tone = HdrToneMapSettings::default();
    let cpu = crate::hdr::decode::hdr_to_sdr_rgba8_with_tone_settings(&buffer, 0.0, &tone)
        .expect("cpu preview tone-map");
    let gpu = with_preview_tone_map_gpu(Some(device), Some(queue), 1, || {
        hdr_to_sdr_rgba8_for_preview(&buffer, 0.0)
    })
    .expect("gpu preview tone-map");

    assert_eq!(cpu.len(), gpu.len());
    for (idx, (c, g)) in cpu.iter().zip(gpu.iter()).enumerate() {
        let delta = i32::from(*c) - i32::from(*g);
        assert!(
            delta.abs() <= 1,
            "preview tone-map mismatch at byte {idx}: cpu={c} gpu={g}"
        );
    }
}
