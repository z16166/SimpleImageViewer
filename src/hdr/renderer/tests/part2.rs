use super::*;

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
    let uniform = ToneMapUniform::from_settings(
        HdrToneMapSettings::default(),
        5,
        0.25,
        HdrRenderOutputMode::SdrToneMapped,
        wgpu::TextureFormat::Bgra8Unorm,
        HdrColorSpace::LinearSrgb,
        HdrTransferFunction::Linear,
        HdrReference::Unknown,
        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
        1.0,
        None,
        None,
    );

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
    let uniform = ToneMapUniform::from_settings(
        HdrToneMapSettings::default(),
        0,
        1.0,
        HdrRenderOutputMode::NativeHdr,
        wgpu::TextureFormat::Bgra8Unorm,
        HdrColorSpace::Rec2020Linear,
        HdrTransferFunction::Pq,
        HdrReference::DisplayReferred,
        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
        1.0,
        None,
        None,
    );

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
    // scRGB native HDR is linear; γ2.2 inflates shadows and destroys SDR contrast on
    // physically SDR displays advertising HDR support (conformance `bench_oriented_brg`).
    assert!(HDR_IMAGE_PLANE_SHADER.contains("fn encode_native_hdr"));
    assert!(
        !HDR_IMAGE_PLANE_SHADER.contains("let sdr_base ="),
        "encode_native_hdr must not γ-encode for scRGB output"
    );
    assert!(
        !HDR_IMAGE_PLANE_SHADER.contains("return max(sdr_base, exposed);"),
        "encode_native_hdr must return exposed linear value, no γ-blend"
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

#[test]
