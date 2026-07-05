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

//! GPU compute path for deferred Apple HEIC gain-map composition.

use std::collections::HashMap;

use super::{
    AppleToneMapCompose, HdrImageBinding, HdrRenderOutputMode, ToneMapCommonParams,
    ToneMapInputMetadata, ToneMapUniform, ToneMapUniformParams,
};
use crate::hdr::heif_apple_gain_map_gpu::apple_heic_compose_effective_color_space;
use crate::hdr::types::{AppleHeicGainMapGpuSource, HdrImageBuffer, HdrToneMapSettings};
use eframe::egui;
use wgpu::util::DeviceExt;

/// WebGPU storage-buffer binding size alignment (256 bytes).
const STORAGE_BINDING_ALIGNMENT: u64 = 256;

const COMPOSE_WORKGROUP_SIZE: u32 = 16;

/// Apple HEIC gain-map compose — primary from storage buffer, gain from texture, output to storage texture.
pub(super) const APPLE_GAIN_COMPOSE_SHADER: &str = concat!(
    r#"
struct ToneMapSettings {
    exposure_ev: f32,
    sdr_white_nits: f32,
    max_display_nits: f32,
    native_display_scale: f32,
    rotation_steps: u32,
    alpha: f32,
    output_mode: u32,
    input_color_space: u32,
    input_transfer_function: u32,
    input_reference: u32,
    sdr_manual_srgb_encode: u32,
    _wgsl_pad_before_uv: u32,
    uv_min: vec2<f32>,
    uv_max: vec2<f32>,
    apple_compose: u32,
    headroom_span: f32,
    weight: f32,
    gain_width: u32,
    gain_height: u32,
    primary_width: u32,
    primary_height: u32,
    /// Row offset for strip compose when primary exceeds `max_storage_buffer_binding_size`.
    compose_row_offset: u32,
    ripple_center: vec2<f32>,
    ripple_radius: f32,
    ripple_enabled: u32,
    pixels_per_point: f32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
};
"#,
    crate::hdr::wgsl_color::hdr_wgsl_color_helpers!(),
    r#"

@group(0) @binding(0) var<storage, read> encoded_primary: array<vec4<f32>>;
@group(0) @binding(1) var gain_map_texture: texture_2d<f32>;
@group(0) @binding(2) var<uniform> tone_map: ToneMapSettings;
@group(0) @binding(3) var compose_output: texture_storage_2d<rgba32float, write>;

fn bt709_gain_rgb_to_linear(rgb: vec3<f32>) -> vec3<f32> {
    let encoded = clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0));
    let low = encoded / vec3<f32>(4.5);
    let high = pow((encoded + vec3<f32>(0.099)) / vec3<f32>(1.099), vec3<f32>(1.0 / 0.45));
    return select(high, low, encoded < vec3<f32>(0.081));
}

fn sample_apple_gain_linear_at_primary_pixel(px: i32, py: i32, settings: ToneMapSettings) -> vec3<f32> {
    let gain_dims_f = vec2<f32>(f32(settings.gain_width), f32(settings.gain_height));
    let primary_dims_f = vec2<f32>(f32(settings.primary_width), f32(settings.primary_height));
    let p_coord = vec2<f32>(f32(px), f32(py));
    
    let g_coord = clamp(
        (p_coord + vec2<f32>(0.5)) * gain_dims_f / primary_dims_f - vec2<f32>(0.5),
        vec2<f32>(0.0),
        gain_dims_f - vec2<f32>(1.0)
    );
    
    let xy0 = vec2<i32>(floor(g_coord));
    let xy1 = min(xy0 + vec2<i32>(1), vec2<i32>(i32(settings.gain_width) - 1, i32(settings.gain_height) - 1));
    let t = g_coord - vec2<f32>(xy0);

    let p00 = textureLoad(gain_map_texture, vec2<i32>(xy0.x, xy0.y), 0).rgb;
    let p10 = textureLoad(gain_map_texture, vec2<i32>(xy1.x, xy0.y), 0).rgb;
    let p01 = textureLoad(gain_map_texture, vec2<i32>(xy0.x, xy1.y), 0).rgb;
    let p11 = textureLoad(gain_map_texture, vec2<i32>(xy1.x, xy1.y), 0).rgb;

    let mix_x0 = mix(p00, p10, t.x);
    let mix_x1 = mix(p01, p11, t.x);
    let encoded = mix(mix_x0, mix_x1, t.y);
    return bt709_gain_rgb_to_linear(encoded);
}

fn load_encoded_primary_pixel(px: i32, local_py: i32, width: u32) -> vec4<f32> {
    let idx = local_py * i32(width) + px;
    return encoded_primary[idx];
}

fn compose_apple_at_primary_pixel(px: i32, py: i32, local_py: i32, settings: ToneMapSettings) -> vec4<f32> {
    let base = load_encoded_primary_pixel(px, local_py, settings.primary_width);
    let display_linear = decode_input_transfer(base.rgb, settings.input_transfer_function, settings);
    let linear_srgb = convert_input_to_linear_srgb(display_linear, settings.input_color_space);
    let gain_linear = sample_apple_gain_linear_at_primary_pixel(px, py, settings);
    let scale = vec3<f32>(1.0) + settings.headroom_span * gain_linear * settings.weight;
    let rgb = max(linear_srgb * scale, vec3<f32>(0.0));
    return vec4<f32>(rgb, base.a);
}

// NOTE: The compose compute shader is run ONLY ONCE when the image is first loaded or when
// target display capacity changes, rather than run every frame during transition.
// Therefore, we must compose the ENTIRE primary image including pixels outside the ripple circle.
// If we were to skip compose for pixels outside the ripple radius here, those pixels would remain
// uncomposed/empty when the ripple radius expands in subsequent frames of the transition animation.
// Discarding fragments outside the circle is instead handled efficiently in the fragment shader `fs_main`.
@compute @workgroup_size(16, 16, 1)
fn cs_compose_apple_gain(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= tone_map.primary_width {
        return;
    }
    let py = i32(gid.y) + i32(tone_map.compose_row_offset);
    if u32(py) >= tone_map.primary_height {
        return;
    }
    let px = i32(gid.x);
    let local_py = i32(gid.y);
    let out = compose_apple_at_primary_pixel(px, py, local_py, tone_map);
    textureStore(compose_output, vec2<i32>(px, py), out);
}
"#
);

pub(super) fn primary_row_stride_bytes(width: u32) -> u64 {
    u64::from(width) * 16
}

fn chunk_row_count(width: u32, max_binding_size: u64) -> u32 {
    let row_bytes = primary_row_stride_bytes(width);
    if row_bytes == 0 {
        return 1;
    }
    if chunk_byte_len(width, 1) > max_binding_size {
        return 1;
    }
    let mut chunk_rows = (max_binding_size / row_bytes).max(1) as u32;
    while chunk_rows > 1 && chunk_byte_len(width, chunk_rows) > max_binding_size {
        chunk_rows -= 1;
    }
    chunk_rows
}

fn chunk_byte_len(width: u32, chunk_rows: u32) -> u64 {
    wgpu::util::align_to(
        primary_row_stride_bytes(width) * u64::from(chunk_rows),
        STORAGE_BINDING_ALIGNMENT,
    )
}

fn max_encoded_primary_strip_bytes(limits: &wgpu::Limits) -> u64 {
    limits
        .max_storage_buffer_binding_size
        .min(limits.max_buffer_size)
}

fn encoded_primary_chunk_rows(width: u32, height: u32, limits: &wgpu::Limits) -> u32 {
    chunk_row_count(width, max_encoded_primary_strip_bytes(limits)).min(height)
}

fn create_encoded_primary_buffer(
    device: &wgpu::Device,
    byte_len: u64,
) -> Result<wgpu::Buffer, String> {
    let oom_scope = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
    let validation_scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("simple-image-viewer-hdr-apple-encoded-primary-buffer"),
        size: byte_len,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let validation_err = pollster::block_on(validation_scope.pop());
    if let Some(err) = validation_err {
        let _ = pollster::block_on(oom_scope.pop());
        return Err(format!("Apple encoded primary buffer validation: {err}"));
    }
    let oom_err = pollster::block_on(oom_scope.pop());
    if let Some(err) = oom_err {
        return Err(format!("Apple encoded primary buffer OOM: {err}"));
    }
    Ok(buffer)
}

pub(super) fn create_compose_compute_resources(
    device: &wgpu::Device,
    pipeline_cache: Option<&wgpu::PipelineCache>,
) -> (wgpu::BindGroupLayout, wgpu::ComputePipeline, wgpu::Buffer) {
    let compose_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("simple-image-viewer-hdr-apple-compose-shader"),
        source: wgpu::ShaderSource::Wgsl(APPLE_GAIN_COMPOSE_SHADER.into()),
    });
    let compose_bind_group_layout =
        device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("simple-image-viewer-hdr-apple-compose-bind-group-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba32Float,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
            ],
        });
    let compose_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("simple-image-viewer-hdr-apple-compose-pipeline-layout"),
        bind_group_layouts: &[Some(&compose_bind_group_layout)],
        immediate_size: 0,
    });
    let compose_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("simple-image-viewer-hdr-apple-compose-pipeline"),
        layout: Some(&compose_pipeline_layout),
        module: &compose_shader,
        entry_point: Some("cs_compose_apple_gain"),
        compilation_options: wgpu::PipelineCompilationOptions::default(),
        cache: pipeline_cache,
    });
    let compose_tone_map_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("simple-image-viewer-hdr-apple-compose-tone-map-buffer"),
        contents: bytemuck::bytes_of(&ToneMapUniform::from_settings(ToneMapUniformParams {
            common: ToneMapCommonParams {
                settings: HdrToneMapSettings::default(),
                rotation_steps: 0,
                alpha: 1.0,
                output_mode: HdrRenderOutputMode::SdrToneMapped,
                framebuffer_format: wgpu::TextureFormat::Rgba8Unorm,
                uv_rect: egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
                native_display_scale: 1.0,
            },
            input: ToneMapInputMetadata {
                color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
                transfer_function: crate::hdr::types::HdrTransferFunction::Linear,
                reference: crate::hdr::types::HdrReference::Unknown,
            },
            apple: None,
            ripple: None,
        })),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });
    (
        compose_bind_group_layout,
        compose_pipeline,
        compose_tone_map_buffer,
    )
}

pub(super) fn ensure_encoded_primary_buffer(
    device: &wgpu::Device,
    binding: &mut HdrImageBinding,
    width: u32,
    height: u32,
) -> Result<(), String> {
    let limits = device.limits();
    let max_strip_bytes = max_encoded_primary_strip_bytes(&limits);
    let chunk_rows = encoded_primary_chunk_rows(width, height, &limits);
    let byte_len = chunk_byte_len(width, chunk_rows);
    if byte_len == 0 || byte_len > limits.max_buffer_size {
        return Err(format!(
            "Apple encoded primary strip size {byte_len} exceeds max_buffer_size {}",
            limits.max_buffer_size
        ));
    }
    if byte_len > max_strip_bytes {
        return Err(format!(
            "Apple encoded primary strip size {byte_len} exceeds max_storage_buffer_binding_size {}",
            max_strip_bytes
        ));
    }
    let needs_new = binding.encoded_primary_buffer_bytes != byte_len as usize;
    if needs_new {
        binding.encoded_primary_buffer = Some(create_encoded_primary_buffer(device, byte_len)?);
        binding.encoded_primary_buffer_bytes = byte_len as usize;
    }
    if binding.encoded_primary_buffer.is_some() {
        Ok(())
    } else {
        Err("Apple encoded primary buffer missing".to_string())
    }
}

fn compose_tone_map_uniform(
    image: &HdrImageBuffer,
    tone_map: HdrToneMapSettings,
    deferred: &AppleHeicGainMapGpuSource,
    compose_row_offset: u32,
) -> ToneMapUniform {
    let compose_color_space =
        apple_heic_compose_effective_color_space(image.color_space, &image.metadata);
    let mut uniform = ToneMapUniform::from_settings(ToneMapUniformParams {
        common: ToneMapCommonParams {
            settings: tone_map,
            rotation_steps: 0,
            alpha: 1.0,
            output_mode: HdrRenderOutputMode::SdrToneMapped,
            framebuffer_format: wgpu::TextureFormat::Rgba8Unorm,
            uv_rect: egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
            native_display_scale: 1.0,
        },
        input: ToneMapInputMetadata {
            color_space: compose_color_space,
            transfer_function: image.metadata.transfer_function,
            reference: image.metadata.reference,
        },
        apple: Some(AppleToneMapCompose {
            deferred,
            primary_w: image.width,
            primary_h: image.height,
            target_capacity: tone_map.target_hdr_capacity(),
        }),
        ripple: None,
    });
    uniform._apple_pad = compose_row_offset;
    uniform
}

pub(super) fn ensure_apple_compose_bind_group<'a>(
    device: &wgpu::Device,
    bind_group_layout: &wgpu::BindGroupLayout,
    bind_groups: &'a mut HashMap<u64, wgpu::BindGroup>,
    binding_size: u64,
    encoded_primary_buffer: &wgpu::Buffer,
    gain_view: &wgpu::TextureView,
    compose_tone_map_buffer: &wgpu::Buffer,
    display_storage_view: &wgpu::TextureView,
) -> &'a wgpu::BindGroup {
    bind_groups.entry(binding_size).or_insert_with(|| {
        super::compose_bind_group::create_compose_bind_group(
            device,
            bind_group_layout,
            "simple-image-viewer-hdr-apple-compose-bind-group",
            super::compose_bind_group::ComposePrimaryBinding::StorageBuffer {
                buffer: encoded_primary_buffer,
                size: binding_size,
            },
            gain_view,
            compose_tone_map_buffer,
            display_storage_view,
        )
    })
}

pub(super) struct AppleComposePass<'a> {
    pub(super) device: &'a wgpu::Device,
    pub(super) queue: &'a wgpu::Queue,
    pub(super) bind_group_layout: &'a wgpu::BindGroupLayout,
    pub(super) pipeline: &'a wgpu::ComputePipeline,
    pub(super) image: &'a HdrImageBuffer,
    pub(super) deferred: &'a AppleHeicGainMapGpuSource,
    pub(super) tone_map: &'a HdrToneMapSettings,
    pub(super) encoded_primary_buffer: &'a wgpu::Buffer,
    pub(super) gain_view: &'a wgpu::TextureView,
    pub(super) display_storage_view: &'a wgpu::TextureView,
    pub(super) upload_primary: bool,
    pub(super) compose_tone_map_buffer: &'a wgpu::Buffer,
    pub(super) apple_compose_bind_groups: &'a mut HashMap<u64, wgpu::BindGroup>,
}

pub(super) fn encode_compose_compute_pass(
    pass_params: AppleComposePass<'_>,
) -> wgpu::CommandBuffer {
    let AppleComposePass {
        device,
        queue,
        bind_group_layout,
        pipeline,
        image,
        deferred,
        tone_map,
        encoded_primary_buffer,
        gain_view,
        display_storage_view,
        upload_primary,
        compose_tone_map_buffer,
        apple_compose_bind_groups,
    } = pass_params;
    let limits = device.limits();
    let chunk_rows = encoded_primary_chunk_rows(image.width, image.height, &limits);
    let row_stride_floats = image.width as usize * 4;

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("simple-image-viewer-hdr-apple-compose-encoder"),
    });
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("simple-image-viewer-hdr-apple-compose-pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(pipeline);

        let mut row_start = 0_u32;
        while row_start < image.height {
            let chunk_height = (image.height - row_start).min(chunk_rows);
            if upload_primary {
                let row_start_usize = row_start as usize;
                let chunk_float_offset = row_start_usize * row_stride_floats;
                let chunk_float_count = chunk_height as usize * row_stride_floats;
                let chunk_pixels =
                    &image.rgba_f32[chunk_float_offset..chunk_float_offset + chunk_float_count];
                queue.write_buffer(
                    encoded_primary_buffer,
                    0,
                    bytemuck::cast_slice(chunk_pixels),
                );
            }

            let compose_uniform = compose_tone_map_uniform(image, *tone_map, deferred, row_start);
            queue.write_buffer(
                compose_tone_map_buffer,
                0,
                bytemuck::bytes_of(&compose_uniform),
            );

            let binding_size = std::num::NonZeroU64::new(chunk_byte_len(image.width, chunk_height))
                .expect("Apple compose chunk binding size must be non-zero")
                .get();

            let bind_group = ensure_apple_compose_bind_group(
                device,
                bind_group_layout,
                apple_compose_bind_groups,
                binding_size,
                encoded_primary_buffer,
                gain_view,
                compose_tone_map_buffer,
                display_storage_view,
            );

            pass.set_bind_group(0, bind_group, &[]);
            pass.dispatch_workgroups(
                image.width.div_ceil(COMPOSE_WORKGROUP_SIZE),
                chunk_height.div_ceil(COMPOSE_WORKGROUP_SIZE),
                1,
            );

            row_start += chunk_height;
        }
    }
    encoder.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_rows_fit_binding_limit_for_12mp_primary() {
        const DEFAULT_MAX_BINDING: u64 = 134_217_728;
        let width = 3024_u32;
        let height = 4032_u32;
        let limits = wgpu::Limits {
            max_storage_buffer_binding_size: DEFAULT_MAX_BINDING,
            max_buffer_size: DEFAULT_MAX_BINDING,
            ..wgpu::Limits::default()
        };
        let chunk_rows = encoded_primary_chunk_rows(width, height, &limits);
        assert!(chunk_rows > 0);
        assert!(chunk_rows <= height);
        let chunk_bytes = chunk_byte_len(width, chunk_rows);
        assert!(chunk_bytes <= DEFAULT_MAX_BINDING);
    }

    #[test]
    fn encoded_primary_buffer_size_caps_to_image_height() {
        const DEFAULT_MAX_BINDING: u64 = 134_217_728;
        let width = 3024_u32;
        let height = 100_u32;
        let limits = wgpu::Limits {
            max_storage_buffer_binding_size: DEFAULT_MAX_BINDING,
            max_buffer_size: DEFAULT_MAX_BINDING,
            ..wgpu::Limits::default()
        };
        let chunk_rows = encoded_primary_chunk_rows(width, height, &limits);
        assert_eq!(chunk_rows, height);
        let chunk_bytes = chunk_byte_len(width, chunk_rows);
        let full_strip = chunk_byte_len(width, encoded_primary_chunk_rows(width, 4032, &limits));
        assert!(chunk_bytes < full_strip);
    }

    #[test]
    fn compose_shader_uses_strip_row_offset_and_storage_primary() {
        assert!(APPLE_GAIN_COMPOSE_SHADER.contains("var<storage, read> encoded_primary"));
        assert!(APPLE_GAIN_COMPOSE_SHADER.contains("compose_row_offset"));
        assert!(APPLE_GAIN_COMPOSE_SHADER.contains("fn cs_compose_apple_gain"));
    }

    #[test]
    fn compose_shader_parses_as_wgsl() {
        naga::front::wgsl::parse_str(APPLE_GAIN_COMPOSE_SHADER)
            .expect("Apple gain compose shader WGSL should parse");
    }
}
