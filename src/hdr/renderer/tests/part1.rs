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
use super::upload::{validate_rgba8_upload_layout, validate_tile_upload_layout};
use super::*;
use crate::hdr::renderer::image_callback::hdr_image_binding_is_eviction_candidate;
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
    // bytes_per_pixel=16 → unpadded row = width * 16 bytes.
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
        HdrToneMapSettings::default(),
        6,
        0.5,
        HdrRenderOutputMode::NativeHdr,
        wgpu::TextureFormat::Rgba16Float,
        HdrColorSpace::LinearSrgb,
        HdrTransferFunction::Linear,
        HdrReference::Unknown,
        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
        1.0,
    );

    assert_eq!(uniform.rotation_steps, 2);
    assert_eq!(uniform.alpha, 0.5);
    assert_eq!(uniform.output_mode, HdrRenderOutputMode::NativeHdr as u32);
}

#[test]
fn tile_tone_map_uniform_carries_uv_subrect() {
    let uniform = tile_tone_map_uniform(
        HdrToneMapSettings::default(),
        0,
        1.0,
        HdrRenderOutputMode::NativeHdr,
        wgpu::TextureFormat::Rgba16Float,
        HdrColorSpace::LinearSrgb,
        HdrTransferFunction::Linear,
        HdrReference::Unknown,
        egui::Rect::from_min_max(egui::Pos2::new(0.25, 0.5), egui::Pos2::new(0.75, 1.0)),
        1.0,
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
        settings,
        5,
        0.75,
        HdrRenderOutputMode::SdrToneMapped,
        wgpu::TextureFormat::Bgra8UnormSrgb,
        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
        1.0,
        false,
        None,
    );
    let tile_uniform = tile_tone_map_uniform(
        settings,
        5,
        0.75,
        HdrRenderOutputMode::SdrToneMapped,
        wgpu::TextureFormat::Bgra8UnormSrgb,
        HdrColorSpace::Rec2020Linear,
        HdrTransferFunction::Linear,
        HdrReference::Unknown,
        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
        1.0,
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
