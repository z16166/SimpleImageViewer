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

use super::{HdrCallbackResources, HdrRenderOutputMode, ToneMapUniform};
use crate::hdr::heif_apple_gain_map_gpu::apple_heic_compose_effective_color_space;
use crate::hdr::types::{AppleHeicGainMapGpuSource, HdrImageBuffer, HdrToneMapSettings};
use eframe::egui;
use wgpu::util::DeviceExt;

/// WebGPU storage-buffer binding size alignment (256 bytes).
const STORAGE_BINDING_ALIGNMENT: u64 = 256;

const COMPOSE_WORKGROUP_SIZE: u32 = 16;

/// Apple HEIC gain-map compose — primary from storage buffer, gain from texture, output to storage texture.
pub(super) const APPLE_GAIN_COMPOSE_SHADER: &str = r#"
const INPUT_COLOR_SPACE_REC2020_LINEAR: u32 = 2u;
const INPUT_COLOR_SPACE_ACES2065_1: u32 = 3u;
const INPUT_COLOR_SPACE_XYZ: u32 = 4u;
const INPUT_COLOR_SPACE_DISPLAY_P3_LINEAR: u32 = 6u;
const INPUT_TRANSFER_LINEAR: u32 = 0u;
const INPUT_TRANSFER_SRGB: u32 = 1u;
const INPUT_TRANSFER_PQ: u32 = 2u;
const INPUT_TRANSFER_HLG: u32 = 3u;
const INPUT_TRANSFER_BT709: u32 = 6u;

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

@group(0) @binding(0) var<storage, read> encoded_primary: array<vec4<f32>>;
@group(0) @binding(1) var gain_map_texture: texture_2d<f32>;
@group(0) @binding(2) var<uniform> tone_map: ToneMapSettings;
@group(0) @binding(3) var compose_output: texture_storage_2d<rgba32float, write>;

fn rec2020_to_linear_srgb(rgb: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        1.6605 * rgb.r - 0.5876 * rgb.g - 0.0728 * rgb.b,
        -0.1246 * rgb.r + 1.1329 * rgb.g - 0.0083 * rgb.b,
        -0.0182 * rgb.r - 0.1006 * rgb.g + 1.1187 * rgb.b,
    );
}

fn display_p3_linear_to_linear_srgb(rgb: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        1.2249401 * rgb.r - 0.2249402 * rgb.g,
        -0.0420569 * rgb.r + 1.0420571 * rgb.g,
        -0.0196376 * rgb.r - 0.0786507 * rgb.g + 1.0982884 * rgb.b,
    );
}

fn aces2065_1_to_linear_srgb(rgb: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        2.5216 * rgb.r - 1.1369 * rgb.g - 0.3849 * rgb.b,
        -0.2762 * rgb.r + 1.3697 * rgb.g - 0.0935 * rgb.b,
        -0.0159 * rgb.r - 0.1478 * rgb.g + 1.1638 * rgb.b,
    );
}

fn xyz_to_linear_srgb(xyz: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        3.2404 * xyz.x - 1.5371 * xyz.y - 0.4985 * xyz.z,
        -0.9692 * xyz.x + 1.8760 * xyz.y + 0.0415 * xyz.z,
        0.0556 * xyz.x - 0.2040 * xyz.y + 1.0572 * xyz.z,
    );
}

fn srgb_to_linear(rgb: vec3<f32>) -> vec3<f32> {
    let low = rgb / vec3<f32>(12.92);
    let high = pow((rgb + vec3<f32>(0.055)) / vec3<f32>(1.055), vec3<f32>(2.4));
    return select(high, low, rgb <= vec3<f32>(0.04045));
}

fn bt709_nonlinear_to_linear(rgb: vec3<f32>) -> vec3<f32> {
    let low = rgb / vec3<f32>(4.5);
    let high = pow((rgb + vec3<f32>(0.099)) / vec3<f32>(1.099), vec3<f32>(1.0 / 0.45));
    return select(high, low, rgb < vec3<f32>(0.081));
}

fn pq_to_display_linear(rgb: vec3<f32>, settings: ToneMapSettings) -> vec3<f32> {
    let m1 = 2610.0 / 16384.0;
    let m2 = 2523.0 / 32.0;
    let c1 = 3424.0 / 4096.0;
    let c2 = 2413.0 / 128.0;
    let c3 = 2392.0 / 128.0;
    let code = pow(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)), vec3<f32>(1.0 / m2));
    let numerator = max(code - vec3<f32>(c1), vec3<f32>(0.0));
    let denominator = max(vec3<f32>(c2) - vec3<f32>(c3) * code, vec3<f32>(0.000001));
    let absolute_nits = vec3<f32>(10000.0) * pow(numerator / denominator, vec3<f32>(1.0 / m1));
    return absolute_nits / max(settings.sdr_white_nits, 1.0);
}

fn hlg_to_scene_linear(rgb: vec3<f32>) -> vec3<f32> {
    let a = 0.17883277;
    let b = 0.28466892;
    let c = 0.55991073;
    let low = (rgb * rgb) / vec3<f32>(3.0);
    let high = (exp((rgb - vec3<f32>(c)) / vec3<f32>(a)) + vec3<f32>(b)) / vec3<f32>(12.0);
    return select(high, low, rgb <= vec3<f32>(0.5));
}

fn decode_input_transfer(rgb: vec3<f32>, input_transfer_function: u32, settings: ToneMapSettings) -> vec3<f32> {
    if input_transfer_function == INPUT_TRANSFER_SRGB {
        return srgb_to_linear(rgb);
    }
    if input_transfer_function == INPUT_TRANSFER_BT709 {
        return bt709_nonlinear_to_linear(rgb);
    }
    if input_transfer_function == INPUT_TRANSFER_PQ {
        return pq_to_display_linear(rgb, settings);
    }
    if input_transfer_function == INPUT_TRANSFER_HLG {
        return hlg_to_scene_linear(rgb);
    }
    return rgb;
}

fn convert_input_to_linear_srgb(rgb: vec3<f32>, input_color_space: u32) -> vec3<f32> {
    if input_color_space == INPUT_COLOR_SPACE_REC2020_LINEAR {
        return rec2020_to_linear_srgb(rgb);
    }
    if input_color_space == INPUT_COLOR_SPACE_DISPLAY_P3_LINEAR {
        return display_p3_linear_to_linear_srgb(rgb);
    }
    if input_color_space == INPUT_COLOR_SPACE_ACES2065_1 {
        return aces2065_1_to_linear_srgb(rgb);
    }
    if input_color_space == INPUT_COLOR_SPACE_XYZ {
        return xyz_to_linear_srgb(rgb);
    }
    return rgb;
}

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
"#;

pub(super) fn primary_row_stride_bytes(width: u32) -> u64 {
    u64::from(width) * 16
}

fn chunk_row_count(width: u32, max_binding_size: u64) -> u32 {
    let row_bytes = primary_row_stride_bytes(width);
    if row_bytes == 0 {
        return 1;
    }
    let aligned_row = wgpu::util::align_to(row_bytes, STORAGE_BINDING_ALIGNMENT);
    ((max_binding_size / aligned_row).max(1)) as u32
}

fn chunk_byte_len(width: u32, chunk_rows: u32) -> u64 {
    wgpu::util::align_to(
        primary_row_stride_bytes(width) * u64::from(chunk_rows),
        STORAGE_BINDING_ALIGNMENT,
    )
}

pub(super) fn create_compose_compute_resources(
    device: &wgpu::Device,
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
        cache: None,
    });
    let compose_tone_map_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("simple-image-viewer-hdr-apple-compose-tone-map-buffer"),
        contents: bytemuck::bytes_of(&ToneMapUniform::from_settings(
            HdrToneMapSettings::default(),
            0,
            1.0,
            HdrRenderOutputMode::SdrToneMapped,
            wgpu::TextureFormat::Rgba8Unorm,
            crate::hdr::types::HdrColorSpace::LinearSrgb,
            crate::hdr::types::HdrTransferFunction::Linear,
            crate::hdr::types::HdrReference::Unknown,
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
            1.0,
            None,
            None,
        )),
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
    resources: &mut HdrCallbackResources,
    width: u32,
    max_binding_size: u64,
) -> Result<(), String> {
    let chunk_rows = chunk_row_count(width, max_binding_size);
    let byte_len = chunk_byte_len(width, chunk_rows);
    let needs_new = resources.encoded_primary_buffer_bytes != byte_len as usize;
    if needs_new {
        resources.encoded_primary_buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("simple-image-viewer-hdr-apple-encoded-primary-buffer"),
            size: byte_len,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
        resources.encoded_primary_buffer_bytes = byte_len as usize;
    }
    if resources.encoded_primary_buffer.is_some() {
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
    let mut uniform = ToneMapUniform::from_settings(
        tone_map,
        0,
        1.0,
        HdrRenderOutputMode::SdrToneMapped,
        wgpu::TextureFormat::Rgba8Unorm,
        compose_color_space,
        image.metadata.transfer_function,
        image.metadata.reference,
        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
        1.0,
        Some((
            deferred,
            image.width,
            image.height,
            tone_map.target_hdr_capacity(),
        )),
        None,
    );
    uniform._apple_pad = compose_row_offset;
    uniform
}

pub(super) fn encode_compose_compute_pass(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    resources: &HdrCallbackResources,
    image: &HdrImageBuffer,
    deferred: &AppleHeicGainMapGpuSource,
    tone_map: &HdrToneMapSettings,
    encoded_primary_buffer: &wgpu::Buffer,
    gain_view: &wgpu::TextureView,
    display_storage_view: &wgpu::TextureView,
    upload_primary: bool,
) -> wgpu::CommandBuffer {
    let max_binding = device.limits().max_storage_buffer_binding_size;
    let chunk_rows = chunk_row_count(image.width, max_binding).min(image.height);
    let row_stride_floats = image.width as usize * 4;

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("simple-image-viewer-hdr-apple-compose-encoder"),
    });
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("simple-image-viewer-hdr-apple-compose-pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&resources.compose_pipeline);

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
                &resources.compose_tone_map_buffer,
                0,
                bytemuck::bytes_of(&compose_uniform),
            );

            let binding_size = std::num::NonZeroU64::new(chunk_byte_len(image.width, chunk_height))
                .expect("Apple compose chunk binding size must be non-zero");

            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("simple-image-viewer-hdr-apple-compose-bind-group"),
                layout: &resources.compose_bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: encoded_primary_buffer,
                            offset: 0,
                            size: Some(binding_size),
                        }),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(gain_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: resources.compose_tone_map_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(display_storage_view),
                    },
                ],
            });

            pass.set_bind_group(0, &bind_group, &[]);
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
        let chunk_rows = chunk_row_count(width, DEFAULT_MAX_BINDING);
        assert!(chunk_rows > 0);
        assert!(chunk_rows <= height);
        let chunk_bytes = chunk_byte_len(width, chunk_rows);
        assert!(chunk_bytes <= DEFAULT_MAX_BINDING);
    }

    #[test]
    fn compose_shader_uses_strip_row_offset_and_storage_primary() {
        assert!(APPLE_GAIN_COMPOSE_SHADER.contains("var<storage, read> encoded_primary"));
        assert!(APPLE_GAIN_COMPOSE_SHADER.contains("compose_row_offset"));
        assert!(APPLE_GAIN_COMPOSE_SHADER.contains("fn cs_compose_apple_gain"));
    }
}
