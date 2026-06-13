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

//! GPU RAW demosaicing compute shader implementation.

use crate::hdr::types::RawGpuSource;
use wgpu::util::DeviceExt;

pub(super) const RAW_DEMOSAIC_COMPUTE_SHADER: &str = r#"
struct DemosaicUniforms {
    width: u32,
    height: u32,
    maximum: f32,
    _pad: u32,
    black_level: vec4<f32>,
    cam_mul: vec4<f32>,
    bayer_pattern: vec4<u32>,
};

@group(0) @binding(0) var raw_pixels_texture: texture_2d<u32>;
@group(0) @binding(1) var<uniform> uniforms: DemosaicUniforms;
@group(0) @binding(2) var output_texture: texture_storage_2d<rgba32float, write>;

fn read_cfa(c: i32, r: i32) -> f32 {
    var x = c;
    if (x < 0) {
        x = -x;
    }
    if (x >= i32(uniforms.width)) {
        let w_minus_1 = i32(uniforms.width) - 1;
        let diff = x - w_minus_1;
        x = w_minus_1 - diff;
    }
    
    var y = r;
    if (y < 0) {
        y = -y;
    }
    if (y >= i32(uniforms.height)) {
        let h_minus_1 = i32(uniforms.height) - 1;
        let diff = y - h_minus_1;
        y = h_minus_1 - diff;
    }
    
    x = clamp(x, 0, i32(uniforms.width) - 1);
    y = clamp(y, 0, i32(uniforms.height) - 1);

    let raw_val = f32(textureLoad(raw_pixels_texture, vec2<i32>(x, y), 0).r);
    
    let phase = (y % 2) * 2 + (x % 2);
    let color_idx = uniforms.bayer_pattern[phase];
    let black = uniforms.black_level[color_idx];
    
    return max(raw_val - black, 0.0);
}

fn get_color_channel(c: i32, r: i32) -> u32 {
    let x = (c % 2 + 2) % 2;
    let y = (r % 2 + 2) % 2;
    return uniforms.bayer_pattern[y * 2 + x];
}

@compute @workgroup_size(16, 16, 1)
fn cs_demosaic(@builtin(global_invocation_id) gid: vec3<u32>) {
    let col = i32(gid.x);
    let row = i32(gid.y);
    if (col >= i32(uniforms.width) || row >= i32(uniforms.height)) {
        return;
    }

    let p02 = read_cfa(col, row - 2);
    let p11 = read_cfa(col - 1, row - 1);
    let p12 = read_cfa(col, row - 1);
    let p13 = read_cfa(col + 1, row - 1);
    let p20 = read_cfa(col - 2, row);
    let p21 = read_cfa(col - 1, row);
    let p22 = read_cfa(col, row);
    let p23 = read_cfa(col + 1, row);
    let p24 = read_cfa(col + 2, row);
    let p31 = read_cfa(col - 1, row + 1);
    let p32 = read_cfa(col, row + 1);
    let p33 = read_cfa(col + 1, row + 1);
    let p42 = read_cfa(col, row + 2);

    let color = get_color_channel(col, row);
    var rgb = vec3<f32>(0.0);

    if (color == 0u) {
        // Red site
        let r_val = p22;
        let g_val = (4.0 * p22 + 2.0 * (p12 + p21 + p23 + p32) - (p02 + p42 + p20 + p24)) / 8.0;
        let b_val = (4.0 * (p11 + p13 + p31 + p33) + 12.0 * p22 - 3.0 * (p02 + p42 + p20 + p24)) / 16.0;
        rgb = vec3<f32>(r_val, g_val, b_val);
    } else if (color == 2u) {
        // Blue site
        let b_val = p22;
        let g_val = (4.0 * p22 + 2.0 * (p12 + p21 + p23 + p32) - (p02 + p42 + p20 + p24)) / 8.0;
        let r_val = (4.0 * (p11 + p13 + p31 + p33) + 12.0 * p22 - 3.0 * (p02 + p42 + p20 + p24)) / 16.0;
        rgb = vec3<f32>(r_val, g_val, b_val);
    } else {
        // Green site
        let row_phase = row % 2;
        let c0 = uniforms.bayer_pattern[row_phase * 2 + 0];
        let c1 = uniforms.bayer_pattern[row_phase * 2 + 1];
        let is_red_row = (c0 == 0u || c1 == 0u);

        if (is_red_row) {
            // G1 (horizontal R, vertical B)
            let g_val = p22;
            let r_val = (p20 + p24 - 2.0 * (p11 + p31 + p02 + p42 + p13 + p33) + 6.0 * (p21 + p23) + 10.0 * p22) / 16.0;
            let b_val = (p02 + p42 - 2.0 * (p11 + p13 + p20 + p24 + p31 + p33) + 6.0 * (p12 + p32) + 10.0 * p22) / 16.0;
            rgb = vec3<f32>(r_val, g_val, b_val);
        } else {
            // G2 (horizontal B, vertical R)
            let g_val = p22;
            let b_val = (p20 + p24 - 2.0 * (p11 + p31 + p02 + p42 + p13 + p33) + 6.0 * (p21 + p23) + 10.0 * p22) / 16.0;
            let r_val = (p02 + p42 - 2.0 * (p11 + p13 + p20 + p24 + p31 + p33) + 6.0 * (p12 + p32) + 10.0 * p22) / 16.0;
            rgb = vec3<f32>(r_val, g_val, b_val);
        }
    }

    // Apply White Balance (uniforms.cam_mul) and scale to [0.0, 1.0]
    let rgb_indices = array<u32, 3>(0u, 1u, 2u);
    for (var c = 0u; c < 3u; c++) {
        let color_idx = rgb_indices[c];
        let black = uniforms.black_level[color_idx];
        let wb = uniforms.cam_mul[color_idx];
        let den = max(uniforms.maximum - black, 1.0);
        rgb[c] = clamp(rgb[c] * wb / den, 0.0, 1.0);
    }

    textureStore(output_texture, vec2<i32>(col, row), vec4<f32>(rgb, 1.0));
}
"#;

#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct RawDemosaicUniform {
    pub width: u32,
    pub height: u32,
    pub maximum: f32,
    pub _pad: u32,
    pub black_level: [f32; 4],
    pub cam_mul: [f32; 4],
    pub bayer_pattern: [u32; 4],
}

impl RawDemosaicUniform {
    pub fn new(source: &RawGpuSource) -> Self {
        Self {
            width: source.width,
            height: source.height,
            maximum: source.maximum.max(1.0),
            _pad: 0,
            black_level: source.black_level,
            cam_mul: source.cam_mul,
            bayer_pattern: source.bayer_pattern,
        }
    }
}

pub(super) fn create_raw_demosaic_compute_resources(
    device: &wgpu::Device,
) -> (wgpu::BindGroupLayout, wgpu::ComputePipeline, wgpu::Buffer) {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("simple-image-viewer-raw-demosaic-shader"),
        source: wgpu::ShaderSource::Wgsl(RAW_DEMOSAIC_COMPUTE_SHADER.into()),
    });

    let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("simple-image-viewer-raw-demosaic-bind-group-layout"),
        entries: &[
            // raw_pixels_texture
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    sample_type: wgpu::TextureSampleType::Uint,
                    view_dimension: wgpu::TextureViewDimension::D2,
                },
                count: None,
            },
            // uniforms
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            // output_texture
            wgpu::BindGroupLayoutEntry {
                binding: 2,
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

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("simple-image-viewer-raw-demosaic-pipeline-layout"),
        bind_group_layouts: &[Some(&bind_group_layout)],
        immediate_size: 0,
    });

    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("simple-image-viewer-raw-demosaic-pipeline"),
        layout: Some(&pipeline_layout),
        module: &shader,
        entry_point: Some("cs_demosaic"),
        compilation_options: wgpu::PipelineCompilationOptions::default(),
        cache: None,
    });

    let dummy_uniform = RawDemosaicUniform {
        width: 1,
        height: 1,
        maximum: 1.0,
        _pad: 0,
        black_level: [0.0; 4],
        cam_mul: [1.0; 4],
        bayer_pattern: [0; 4],
    };

    let uniforms_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("simple-image-viewer-raw-demosaic-uniforms-buffer"),
        contents: bytemuck::bytes_of(&dummy_uniform),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });

    (bind_group_layout, pipeline, uniforms_buffer)
}

pub(crate) fn encode_raw_demosaic_compute_pass(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    bind_group_layout: &wgpu::BindGroupLayout,
    pipeline: &wgpu::ComputePipeline,
    source: &RawGpuSource,
    raw_pixels_view: &wgpu::TextureView,
    output_view: &wgpu::TextureView,
    uniform_buffer: &wgpu::Buffer,
) -> wgpu::CommandBuffer {
    let uniform = RawDemosaicUniform::new(source);
    queue.write_buffer(uniform_buffer, 0, bytemuck::bytes_of(&uniform));

    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("simple-image-viewer-raw-demosaic-bind-group"),
        layout: bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(raw_pixels_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: uniform_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(output_view),
            },
        ],
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("simple-image-viewer-raw-demosaic-encoder"),
    });
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("simple-image-viewer-raw-demosaic-pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &bind_group, &[]);

        let workgroups_x = source.width.div_ceil(16);
        let workgroups_y = source.height.div_ceil(16);
        pass.dispatch_workgroups(workgroups_x, workgroups_y, 1);
    }
    encoder.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_demosaic_compute_shader_parses_as_wgsl() {
        naga::front::wgsl::parse_str(RAW_DEMOSAIC_COMPUTE_SHADER)
            .expect("RAW demosaic compute shader must parse before runtime pipeline creation");
    }
}
