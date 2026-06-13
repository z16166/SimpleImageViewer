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
    output_scale: f32,
    _pad: u32,
    black_level: vec4<f32>,
    cfa_scale: vec4<f32>,
    bayer_pattern: vec4<u32>,
    rgb_cam0: vec4<f32>,
    rgb_cam1: vec4<f32>,
    rgb_cam2: vec4<f32>,
};

@group(0) @binding(0) var raw_pixels_texture: texture_2d<u32>;
@group(0) @binding(1) var<uniform> uniforms: DemosaicUniforms;
@group(0) @binding(2) var green_plane: texture_storage_2d<r32float, read_write>;
@group(0) @binding(3) var output_texture: texture_storage_2d<rgba32float, write>;

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
    let scale = uniforms.cfa_scale[color_idx];

    return max(raw_val - black, 0.0) * scale;
}

fn get_color_channel(c: i32, r: i32) -> u32 {
    let x = (c % 2 + 2) % 2;
    let y = (r % 2 + 2) % 2;
    return uniforms.bayer_pattern[y * 2 + x];
}

fn abs_f(v: f32) -> f32 {
    return max(v, -v);
}

fn ulim(v: f32, lo: f32, hi: f32) -> f32 {
    return clamp(v, min(lo, hi), max(lo, hi));
}

// LibRaw pre_interpolate: G2 copied into green plane [1] at G2 sites.
fn read_green_plane(c: i32, r: i32) -> f32 {
    let fc = get_color_channel(c, r);
    if (fc == 1u || fc == 3u) {
        return read_cfa(c, r);
    }
    return 0.0;
}

fn apply_rgb_cam(rgb: vec3<f32>) -> vec3<f32> {
    return max(
        vec3<f32>(
            uniforms.rgb_cam0.x * rgb.r + uniforms.rgb_cam0.y * rgb.g + uniforms.rgb_cam0.z * rgb.b,
            uniforms.rgb_cam1.x * rgb.r + uniforms.rgb_cam1.y * rgb.g + uniforms.rgb_cam1.z * rgb.b,
            uniforms.rgb_cam2.x * rgb.r + uniforms.rgb_cam2.y * rgb.g + uniforms.rgb_cam2.z * rgb.b,
        ),
        vec3<f32>(0.0),
    );
}

// LibRaw ppg pass 1: fill green plane [1].
fn ppg_green_at(col: i32, row: i32, c: u32) -> f32 {
    let x = read_cfa(col, row);
    let h_guess = (read_green_plane(col - 1, row) + x + read_green_plane(col + 1, row)) * 2.0
        - read_cfa(col - 2, row) - read_cfa(col + 2, row);
    let h_diff = (abs_f(read_cfa(col - 2, row) - x) + abs_f(read_cfa(col + 2, row) - x)
        + abs_f(read_green_plane(col - 1, row) - read_green_plane(col + 1, row))) * 3.0
        + (abs_f(read_green_plane(col + 3, row) - read_green_plane(col + 1, row))
        + abs_f(read_green_plane(col - 3, row) - read_green_plane(col - 1, row))) * 2.0;
    let v_guess = (read_green_plane(col, row - 1) + x + read_green_plane(col, row + 1)) * 2.0
        - read_cfa(col, row - 2) - read_cfa(col, row + 2);
    let v_diff = (abs_f(read_cfa(col, row - 2) - x) + abs_f(read_cfa(col, row + 2) - x)
        + abs_f(read_green_plane(col, row - 1) - read_green_plane(col, row + 1))) * 3.0
        + (abs_f(read_green_plane(col, row + 3) - read_green_plane(col, row + 1))
        + abs_f(read_green_plane(col, row - 3) - read_green_plane(col, row - 1))) * 2.0;

    if (h_diff > v_diff) {
        return ulim(v_guess / 4.0, read_green_plane(col, row + 1), read_green_plane(col, row - 1));
    }
    return ulim(h_guess / 4.0, read_green_plane(col + 1, row), read_green_plane(col - 1, row));
}

fn read_green_stored(c: i32, r: i32) -> f32 {
    let x = clamp(c, 0, i32(uniforms.width) - 1);
    let y = clamp(r, 0, i32(uniforms.height) - 1);
    return textureLoad(green_plane, vec2<i32>(x, y)).r;
}

fn read_channel_stored(c: i32, r: i32, ch: u32) -> f32 {
    let fc = get_color_channel(c, r);
    if (fc == ch) {
        return read_cfa(c, r);
    }
    if (ch == 1u) {
        return read_green_stored(c, r);
    }
    return 0.0;
}

// Pass 1 (LibRaw ppg): write interpolated green plane.
@compute @workgroup_size(16, 16, 1)
fn cs_ppg_green(@builtin(global_invocation_id) gid: vec3<u32>) {
    let col = i32(gid.x);
    let row = i32(gid.y);
    if (col >= i32(uniforms.width) || row >= i32(uniforms.height)) {
        return;
    }
    let fc = get_color_channel(col, row);
    // LibRaw ppg pass 1: measured green at G1/G2 sites, interpolate elsewhere.
    var green: f32;
    if (fc == 1u || fc == 3u) {
        green = read_cfa(col, row);
    } else {
        green = ppg_green_at(col, row, fc);
    }
    textureStore(green_plane, vec2<i32>(col, row), vec4<f32>(green));
}

// Pass 2: R/B interpolation using stored green (no recursive green recompute).
fn ppg_green_site_rgb(col: i32, row: i32, green: f32) -> vec3<f32> {
    var rgb = vec3<f32>(0.0, green, 0.0);
    var c = get_color_channel(col + 1, row);
    var v = (read_channel_stored(col - 1, row, c) + read_channel_stored(col + 1, row, c) + 2.0 * green
        - read_channel_stored(col - 1, row, 1u) - read_channel_stored(col + 1, row, 1u)) * 0.5;
    if (c == 0u) {
        rgb.r = v;
    } else {
        rgb.b = v;
    }
    c = 2u - c;
    v = (read_channel_stored(col, row - 1, c) + read_channel_stored(col, row + 1, c) + 2.0 * green
        - read_channel_stored(col, row - 1, 1u) - read_channel_stored(col, row + 1, 1u)) * 0.5;
    if (c == 0u) {
        rgb.r = v;
    } else {
        rgb.b = v;
    }
    return rgb;
}

// LibRaw ppg pass 3: missing chroma at R/B sites.
fn ppg_chroma_at_rb(col: i32, row: i32, fc: u32, green: f32) -> f32 {
    let c = 2u - fc;
    let nd_c = read_channel_stored(col - 1, row - 1, c);
    let pd_c = read_channel_stored(col + 1, row + 1, c);
    let nd_g = read_channel_stored(col - 1, row - 1, 1u);
    let pd_g = read_channel_stored(col + 1, row + 1, 1u);
    let diff0 = abs_f(nd_c - pd_c) + abs_f(nd_g - green) + abs_f(pd_g - green);
    let guess0 = nd_c + pd_c + 2.0 * green - nd_g - pd_g;

    let nw_c = read_channel_stored(col + 1, row - 1, c);
    let se_c = read_channel_stored(col - 1, row + 1, c);
    let nw_g = read_channel_stored(col + 1, row - 1, 1u);
    let se_g = read_channel_stored(col - 1, row + 1, 1u);
    let diff1 = abs_f(nw_c - se_c) + abs_f(nw_g - green) + abs_f(se_g - green);
    let guess1 = nw_c + se_c + 2.0 * green - nw_g - se_g;

    if (diff0 != diff1) {
        if (diff0 > diff1) {
            return guess1 * 0.5;
        }
        return guess0 * 0.5;
    }
    return (guess0 + guess1) * 0.25;
}

@compute @workgroup_size(16, 16, 1)
fn cs_ppg_rgb(@builtin(global_invocation_id) gid: vec3<u32>) {
    let col = i32(gid.x);
    let row = i32(gid.y);
    if (col >= i32(uniforms.width) || row >= i32(uniforms.height)) {
        return;
    }

    let fc = get_color_channel(col, row);
    let green = read_green_stored(col, row);
    var rgb: vec3<f32>;

    if (fc == 0u) {
        rgb.r = read_cfa(col, row);
        rgb.g = green;
        rgb.b = ppg_chroma_at_rb(col, row, fc, green);
    } else if (fc == 2u) {
        rgb.b = read_cfa(col, row);
        rgb.g = green;
        rgb.r = ppg_chroma_at_rb(col, row, fc, green);
    } else {
        rgb = ppg_green_site_rgb(col, row, green);
    }

    rgb = apply_rgb_cam(rgb) * uniforms.output_scale;
    rgb = clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0));

    textureStore(output_texture, vec2<i32>(col, row), vec4<f32>(rgb, 1.0));
}
"#;

#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct RawDemosaicUniform {
    pub width: u32,
    pub height: u32,
    pub output_scale: f32,
    pub _pad: u32,
    pub black_level: [f32; 4],
    pub cfa_scale: [f32; 4],
    pub bayer_pattern: [u32; 4],
    pub rgb_cam0: [f32; 4],
    pub rgb_cam1: [f32; 4],
    pub rgb_cam2: [f32; 4],
}

impl RawDemosaicUniform {
    pub fn new(source: &RawGpuSource) -> Self {
        Self {
            width: source.width,
            height: source.height,
            output_scale: 1.0 / 65535.0,
            _pad: 0,
            black_level: source.black_level,
            cfa_scale: source.cfa_scale,
            bayer_pattern: source.bayer_pattern,
            rgb_cam0: source.rgb_cam[0..4].try_into().expect("rgb_cam row 0"),
            rgb_cam1: source.rgb_cam[4..8].try_into().expect("rgb_cam row 1"),
            rgb_cam2: source.rgb_cam[8..12].try_into().expect("rgb_cam row 2"),
        }
    }
}

pub(super) fn create_raw_demosaic_compute_resources(
    device: &wgpu::Device,
) -> (
    wgpu::BindGroupLayout,
    wgpu::ComputePipeline,
    wgpu::ComputePipeline,
    wgpu::Buffer,
) {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("simple-image-viewer-raw-demosaic-shader"),
        source: wgpu::ShaderSource::Wgsl(RAW_DEMOSAIC_COMPUTE_SHADER.into()),
    });

    let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("simple-image-viewer-raw-demosaic-bind-group-layout"),
        entries: &[
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
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::StorageTexture {
                    access: wgpu::StorageTextureAccess::ReadWrite,
                    format: wgpu::TextureFormat::R32Float,
                    view_dimension: wgpu::TextureViewDimension::D2,
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

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("simple-image-viewer-raw-demosaic-pipeline-layout"),
        bind_group_layouts: &[Some(&bind_group_layout)],
        immediate_size: 0,
    });

    let green_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("simple-image-viewer-raw-demosaic-green-pipeline"),
        layout: Some(&pipeline_layout),
        module: &shader,
        entry_point: Some("cs_ppg_green"),
        compilation_options: wgpu::PipelineCompilationOptions::default(),
        cache: None,
    });

    let rgb_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("simple-image-viewer-raw-demosaic-rgb-pipeline"),
        layout: Some(&pipeline_layout),
        module: &shader,
        entry_point: Some("cs_ppg_rgb"),
        compilation_options: wgpu::PipelineCompilationOptions::default(),
        cache: None,
    });

    let dummy_uniform = RawDemosaicUniform {
        width: 1,
        height: 1,
        output_scale: 1.0 / 65535.0,
        _pad: 0,
        black_level: [0.0; 4],
        cfa_scale: [1.0; 4],
        bayer_pattern: [0; 4],
        rgb_cam0: [1.0, 0.0, 0.0, 0.0],
        rgb_cam1: [0.0, 1.0, 0.0, 0.0],
        rgb_cam2: [0.0, 0.0, 1.0, 0.0],
    };

    let uniforms_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("simple-image-viewer-raw-demosaic-uniforms-buffer"),
        contents: bytemuck::bytes_of(&dummy_uniform),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });

    (bind_group_layout, green_pipeline, rgb_pipeline, uniforms_buffer)
}

pub(crate) fn encode_raw_demosaic_compute_pass(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    bind_group_layout: &wgpu::BindGroupLayout,
    green_pipeline: &wgpu::ComputePipeline,
    rgb_pipeline: &wgpu::ComputePipeline,
    source: &RawGpuSource,
    raw_pixels_view: &wgpu::TextureView,
    green_plane_view: &wgpu::TextureView,
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
                resource: wgpu::BindingResource::TextureView(green_plane_view),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(output_view),
            },
        ],
    });

    let workgroups_x = source.width.div_ceil(16);
    let workgroups_y = source.height.div_ceil(16);

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("simple-image-viewer-raw-demosaic-encoder"),
    });
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("simple-image-viewer-raw-demosaic-green-pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(green_pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups(workgroups_x, workgroups_y, 1);
    }
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("simple-image-viewer-raw-demosaic-rgb-pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(rgb_pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups(workgroups_x, workgroups_y, 1);
    }
    encoder.finish()
}

/// CPU mirror of WGSL PPG demosaic (camera RGB before `convert_to_rgb`).
#[cfg(test)]
fn cpu_ppg_helpers(source: &RawGpuSource) -> impl Fn(i32, i32) -> f32 + '_ {
    let w = source.width as usize;
    let raw = source.raw_pixels.as_slice();
    let color_at = |col: i32, row: i32| -> u32 {
        let x = ((col % 2) + 2) % 2;
        let y = ((row % 2) + 2) % 2;
        source.bayer_pattern[(y * 2 + x) as usize]
    };
    move |col: i32, row: i32| -> f32 {
        let mut x = col;
        if x < 0 {
            x = -x;
        }
        if x >= source.width as i32 {
            let w_minus_1 = source.width as i32 - 1;
            let diff = x - w_minus_1;
            x = w_minus_1 - diff;
        }
        let mut y = row;
        if y < 0 {
            y = -y;
        }
        if y >= source.height as i32 {
            let h_minus_1 = source.height as i32 - 1;
            let diff = y - h_minus_1;
            y = h_minus_1 - diff;
        }
        x = x.clamp(0, source.width as i32 - 1);
        y = y.clamp(0, source.height as i32 - 1);
        let raw_val = raw[y as usize * w + x as usize] as f32;
        let ch = color_at(x, y) as usize;
        let black = source.black_level[ch];
        let scale = source.cfa_scale[ch];
        (raw_val - black).max(0.0) * scale
    }
}

#[cfg(test)]
fn cpu_fc(source: &RawGpuSource, col: i32, row: i32) -> u32 {
    let x = ((col % 2) + 2) % 2;
    let y = ((row % 2) + 2) % 2;
    source.bayer_pattern[(y * 2 + x) as usize]
}

#[cfg(test)]
fn cpu_read_green_plane(
    source: &RawGpuSource,
    read_cfa: &impl Fn(i32, i32) -> f32,
    col: i32,
    row: i32,
) -> f32 {
    let fc = cpu_fc(source, col, row);
    if fc == 1 || fc == 3 {
        return read_cfa(col, row);
    }
    0.0
}

#[cfg(test)]
fn cpu_ppg_green_at(
    source: &RawGpuSource,
    read_cfa: &impl Fn(i32, i32) -> f32,
    col: i32,
    row: i32,
) -> f32 {
    let read_gp = |c: i32, r: i32| cpu_read_green_plane(source, read_cfa, c, r);
    let x = read_cfa(col, row);
    let h_guess = (read_gp(col - 1, row) + x + read_gp(col + 1, row)) * 2.0
        - read_cfa(col - 2, row)
        - read_cfa(col + 2, row);
    let h_diff = ((read_cfa(col - 2, row) - x).abs()
        + (read_cfa(col + 2, row) - x).abs()
        + (read_gp(col - 1, row) - read_gp(col + 1, row)).abs())
        * 3.0
        + ((read_gp(col + 3, row) - read_gp(col + 1, row)).abs()
            + (read_gp(col - 3, row) - read_gp(col - 1, row)).abs())
            * 2.0;
    let v_guess = (read_gp(col, row - 1) + x + read_gp(col, row + 1)) * 2.0
        - read_cfa(col, row - 2)
        - read_cfa(col, row + 2);
    let v_diff = ((read_cfa(col, row - 2) - x).abs()
        + (read_cfa(col, row + 2) - x).abs()
        + (read_gp(col, row - 1) - read_gp(col, row + 1)).abs())
        * 3.0
        + ((read_gp(col, row + 3) - read_gp(col, row + 1)).abs()
            + (read_gp(col, row - 3) - read_gp(col, row - 1)).abs())
            * 2.0;
    if h_diff > v_diff {
        let lo = read_gp(col, row + 1);
        let hi = read_gp(col, row - 1);
        return (v_guess / 4.0).clamp(lo.min(hi), lo.max(hi));
    }
    let lo = read_gp(col + 1, row);
    let hi = read_gp(col - 1, row);
    (h_guess / 4.0).clamp(lo.min(hi), lo.max(hi))
}

#[cfg(test)]
fn cpu_build_green_plane(source: &RawGpuSource, read_cfa: &impl Fn(i32, i32) -> f32) -> Vec<f32> {
    let w = source.width as usize;
    let h = source.height as usize;
    let mut plane = vec![0.0f32; w * h];
    for row in 0..source.height as i32 {
        for col in 0..source.width as i32 {
            let fc = cpu_fc(source, col, row);
            let green = if fc == 1 || fc == 3 {
                read_cfa(col, row)
            } else {
                cpu_ppg_green_at(source, read_cfa, col, row)
            };
            plane[row as usize * w + col as usize] = green;
        }
    }
    plane
}

#[cfg(test)]
fn cpu_read_green_stored(plane: &[f32], source: &RawGpuSource, col: i32, row: i32) -> f32 {
    let w = source.width as usize;
    let x = col.clamp(0, source.width as i32 - 1) as usize;
    let y = row.clamp(0, source.height as i32 - 1) as usize;
    plane[y * w + x]
}

#[cfg(test)]
fn cpu_read_channel_stored(
    source: &RawGpuSource,
    read_cfa: &impl Fn(i32, i32) -> f32,
    plane: &[f32],
    col: i32,
    row: i32,
    ch: u32,
) -> f32 {
    let fc = cpu_fc(source, col, row);
    if fc == ch {
        return read_cfa(col, row);
    }
    if ch == 1 {
        return cpu_read_green_stored(plane, source, col, row);
    }
    0.0
}

#[cfg(test)]
fn cpu_ppg_green_site_rgb(
    source: &RawGpuSource,
    read_cfa: &impl Fn(i32, i32) -> f32,
    plane: &[f32],
    col: i32,
    row: i32,
    green: f32,
) -> [f32; 3] {
    let mut rgb = [0.0f32, green, 0.0];
    let mut c = cpu_fc(source, col + 1, row);
    let mut v = (cpu_read_channel_stored(source, read_cfa, plane, col - 1, row, c)
        + cpu_read_channel_stored(source, read_cfa, plane, col + 1, row, c)
        + 2.0 * green
        - cpu_read_channel_stored(source, read_cfa, plane, col - 1, row, 1)
        - cpu_read_channel_stored(source, read_cfa, plane, col + 1, row, 1))
        * 0.5;
    if c == 0 {
        rgb[0] = v;
    } else {
        rgb[2] = v;
    }
    c = 2 - c;
    v = (cpu_read_channel_stored(source, read_cfa, plane, col, row - 1, c)
        + cpu_read_channel_stored(source, read_cfa, plane, col, row + 1, c)
        + 2.0 * green
        - cpu_read_channel_stored(source, read_cfa, plane, col, row - 1, 1)
        - cpu_read_channel_stored(source, read_cfa, plane, col, row + 1, 1))
        * 0.5;
    if c == 0 {
        rgb[0] = v;
    } else {
        rgb[2] = v;
    }
    rgb
}

#[cfg(test)]
fn cpu_ppg_chroma_at_rb(
    source: &RawGpuSource,
    read_cfa: &impl Fn(i32, i32) -> f32,
    plane: &[f32],
    col: i32,
    row: i32,
    fc: u32,
    green: f32,
) -> f32 {
    let c = 2 - fc;
    let nd_c = cpu_read_channel_stored(source, read_cfa, plane, col - 1, row - 1, c);
    let pd_c = cpu_read_channel_stored(source, read_cfa, plane, col + 1, row + 1, c);
    let nd_g = cpu_read_channel_stored(source, read_cfa, plane, col - 1, row - 1, 1);
    let pd_g = cpu_read_channel_stored(source, read_cfa, plane, col + 1, row + 1, 1);
    let diff0 = (nd_c - pd_c).abs() + (nd_g - green).abs() + (pd_g - green).abs();
    let guess0 = nd_c + pd_c + 2.0 * green - nd_g - pd_g;

    let nw_c = cpu_read_channel_stored(source, read_cfa, plane, col + 1, row - 1, c);
    let se_c = cpu_read_channel_stored(source, read_cfa, plane, col - 1, row + 1, c);
    let nw_g = cpu_read_channel_stored(source, read_cfa, plane, col + 1, row - 1, 1);
    let se_g = cpu_read_channel_stored(source, read_cfa, plane, col - 1, row + 1, 1);
    let diff1 = (nw_c - se_c).abs() + (nw_g - green).abs() + (se_g - green).abs();
    let guess1 = nw_c + se_c + 2.0 * green - nw_g - se_g;

    if diff0 != diff1 {
        if diff0 > diff1 {
            return guess1 * 0.5;
        }
        return guess0 * 0.5;
    }
    (guess0 + guess1) * 0.25
}

#[cfg(test)]
fn cpu_ppg_camera_rgb_at(
    source: &RawGpuSource,
    read_cfa: &impl Fn(i32, i32) -> f32,
    plane: &[f32],
    col: i32,
    row: i32,
) -> [f32; 3] {
    let fc = cpu_fc(source, col, row);
    let green = cpu_read_green_stored(plane, source, col, row);
    if fc == 0 {
        [
            read_cfa(col, row),
            green,
            cpu_ppg_chroma_at_rb(source, read_cfa, plane, col, row, fc, green),
        ]
    } else if fc == 2 {
        [
            cpu_ppg_chroma_at_rb(source, read_cfa, plane, col, row, fc, green),
            green,
            read_cfa(col, row),
        ]
    } else {
        cpu_ppg_green_site_rgb(source, read_cfa, plane, col, row, green)
    }
}

#[cfg(test)]
pub(crate) fn cpu_demosaic_ppg_camera_counts(source: &RawGpuSource) -> Vec<f32> {
    let w = source.width as usize;
    let read_cfa = cpu_ppg_helpers(source);
    let green_plane = cpu_build_green_plane(source, &read_cfa);
    let mut out = vec![0.0f32; w * source.height as usize * 3];
    for row in 0..source.height as i32 {
        for col in 0..source.width as i32 {
            let rgb = cpu_ppg_camera_rgb_at(source, &read_cfa, &green_plane, col, row);
            let i = (row as usize * w + col as usize) * 3;
            out[i..i + 3].copy_from_slice(&rgb);
        }
    }
    out
}

/// CPU mirror of the GPU PPG demosaic + LibRaw color path (tests / diff).
#[cfg(test)]
pub(crate) fn cpu_demosaic_ppg_scene_linear(source: &RawGpuSource) -> Vec<f32> {
    let w = source.width as usize;
    let output_scale = 1.0f32 / 65535.0;
    let read_cfa = cpu_ppg_helpers(source);
    let green_plane = cpu_build_green_plane(source, &read_cfa);
    let mut out = vec![0.0f32; w * source.height as usize * 4];
    let apply_rgb_cam = |rgb: [f32; 3]| -> [f32; 3] {
        let m = &source.rgb_cam;
        [
            (m[0] * rgb[0] + m[1] * rgb[1] + m[2] * rgb[2]).max(0.0),
            (m[4] * rgb[0] + m[5] * rgb[1] + m[6] * rgb[2]).max(0.0),
            (m[8] * rgb[0] + m[9] * rgb[1] + m[10] * rgb[2]).max(0.0),
        ]
    };
    for row in 0..source.height as i32 {
        for col in 0..source.width as i32 {
            let rgb = cpu_ppg_camera_rgb_at(source, &read_cfa, &green_plane, col, row);
            let linear = apply_rgb_cam(rgb);
            let i = (row as usize * w + col as usize) * 4;
            out[i] = (linear[0] * output_scale).clamp(0.0, 1.0);
            out[i + 1] = (linear[1] * output_scale).clamp(0.0, 1.0);
            out[i + 2] = (linear[2] * output_scale).clamp(0.0, 1.0);
            out[i + 3] = 1.0;
        }
    }
    out
}

#[cfg(test)]
fn center_mean_rgba(pixels: &[f32], width: usize, height: usize) -> (f64, f64, f64) {
    let cx = width / 2;
    let cy = height / 2;
    let mut r_sum = 0.0f64;
    let mut g_sum = 0.0f64;
    let mut b_sum = 0.0f64;
    let mut count = 0u64;
    for dy in 0..64 {
        for dx in 0..64 {
            let x = cx + dx - 32;
            let y = cy + dy - 32;
            if x >= width || y >= height {
                continue;
            }
            let i = (y * width + x) * 4;
            r_sum += pixels[i] as f64;
            g_sum += pixels[i + 1] as f64;
            b_sum += pixels[i + 2] as f64;
            count += 1;
        }
    }
    let n = count as f64;
    (r_sum / n, g_sum / n, b_sum / n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn raw_demosaic_compute_shader_parses_as_wgsl() {
        naga::front::wgsl::parse_str(RAW_DEMOSAIC_COMPUTE_SHADER)
            .expect("RAW demosaic compute shader must parse before runtime pipeline creation");
    }

    /// Requires `F:\win7\raws\canon\5dm2\RAW_CANON_5DMARK2_PREPROD.CR2` on the test machine.
    #[test]
    fn diff_canon_5d2_ppg_gpu_path_vs_libraw_cpu() {
        let path = Path::new(r"F:\win7\raws\canon\5dm2\RAW_CANON_5DMARK2_PREPROD.CR2");
        if !path.is_file() {
            eprintln!("skip: Canon 5D2 sample not present at {}", path.display());
            return;
        }
        let mut processor = crate::raw_processor::RawProcessor::new().expect("libraw init");
        processor.open(path).expect("libraw open");
        let source = processor
            .extract_raw_gpu_source(crate::settings::RawDemosaicMethod::MalvarHeCutler)
            .expect("extract gpu source");
        let w = source.width as usize;
        let h = source.height as usize;

        let counts = cpu_demosaic_ppg_camera_counts(&source);
        let mut cam_r = 0.0f64;
        let mut cam_g = 0.0f64;
        let mut cam_b = 0.0f64;
        let mut cam_n = 0u64;
        let cx = w / 2;
        let cy = h / 2;
        for dy in 0..64 {
            for dx in 0..64 {
                let x = cx + dx - 32;
                let y = cy + dy - 32;
                if x >= w || y >= h {
                    continue;
                }
                let i = (y * w + x) * 3;
                cam_r += counts[i] as f64;
                cam_g += counts[i + 1] as f64;
                cam_b += counts[i + 2] as f64;
                cam_n += 1;
            }
        }
        let cn = cam_n as f64;
        eprintln!(
            "canon 5d2 ppg camera counts center avg=({:.0}, {:.0}, {:.0})",
            cam_r / cn,
            cam_g / cn,
            cam_b / cn,
        );

        let gpu_path = cpu_demosaic_ppg_scene_linear(&source);
        let (gr, gg, gb) = center_mean_rgba(&gpu_path, w, h);
        let gpu_rb = gr / gb.max(1e-9);

        let hdr = processor
            .develop_scene_linear_hdr()
            .expect("develop_scene_linear_hdr");
        let cpu = hdr.rgba_f32.as_slice();
        let (cr, cg, cb) = center_mean_rgba(cpu, w, h);
        let cpu_rb = cr / cb.max(1e-9);

        eprintln!(
            "canon 5d2 gpu-shader center avg=({gr:.4}, {gg:.4}, {gb:.4}) R/B={gpu_rb:.3}"
        );
        eprintln!(
            "canon 5d2 libraw cpu center avg=({cr:.4}, {cg:.4}, {cb:.4}) R/B={cpu_rb:.3}"
        );
        eprintln!(
            "note: GPU WGSL uses LibRaw PPG demosaic; LibRaw CPU default is AHD — expect small diff"
        );
    }
}
