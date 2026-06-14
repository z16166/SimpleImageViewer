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
    scene_color_scale: vec4<f32>,
};

@group(0) @binding(0) var raw_pixels_texture: texture_2d<u32>;
@group(0) @binding(1) var<uniform> uniforms: DemosaicUniforms;
@group(0) @binding(2) var green_plane_write: texture_storage_2d<r32float, write>;
@group(0) @binding(4) var green_plane_read: texture_2d<f32>;
@group(0) @binding(3) var output_texture: texture_storage_2d<rgba32float, write>;

const RAW16_MAX: i32 = 65535;
const WG_SIZE: u32 = 16u;
const WG_TOTAL: u32 = WG_SIZE * WG_SIZE;
const CFA_HALO: u32 = 3u;
// 22x22 tile: WG_SIZE + 2*CFA_HALO halo for PPG +/-3 neighborhood.
const CFA_TILE: u32 = WG_SIZE + 2u * CFA_HALO;
const CFA_TILE_ELS: u32 = CFA_TILE * CFA_TILE;

var<workgroup> wg_tile_origin: vec2<i32>;
var<workgroup> wg_cfa_tile: array<f32, 484>;
var<workgroup> wg_green_tile: array<f32, 484>;

fn load_cfa_pixel(x: i32, y: i32) -> f32 {
    return f32(textureLoad(raw_pixels_texture, vec2<i32>(x, y), 0).r);
}

fn mirror_coord(c: i32, limit: i32) -> i32 {
    var x = c;
    if (x < 0) {
        x = -x;
    }
    if (x >= limit) {
        let limit_minus_1 = limit - 1;
        let diff = x - limit_minus_1;
        x = limit_minus_1 - diff;
    }
    return clamp(x, 0, limit - 1);
}

fn scaled_cfa_at(x: i32, y: i32) -> f32 {
    // Host CFA extract already applied black-level subtract and CFA scale.
    return load_cfa_pixel(x, y);
}

fn get_bayer_color(phase: i32) -> u32 {
    return uniforms.bayer_pattern[phase];
}

fn get_black_level(color_idx: u32) -> f32 {
    return uniforms.black_level[color_idx];
}

fn get_cfa_scale(color_idx: u32) -> f32 {
    return uniforms.cfa_scale[color_idx];
}

fn read_cfa(c: i32, r: i32) -> f32 {
    let w = i32(uniforms.width);
    let h = i32(uniforms.height);
    let x = mirror_coord(c, w);
    let y = mirror_coord(r, h);
    return scaled_cfa_at(x, y);
}

fn read_cfa_wg(c: i32, r: i32) -> f32 {
    let origin = wg_tile_origin;
    let tx = u32(c - origin.x);
    let ty = u32(r - origin.y);
    return wg_cfa_tile[ty * CFA_TILE + tx];
}

fn coop_load_cfa_tile(wg: vec2<i32>, lane: u32) {
    let tile_origin = wg * i32(WG_SIZE) - i32(CFA_HALO);
    if (lane == 0u) {
        wg_tile_origin = tile_origin;
    }
    let w = i32(uniforms.width);
    let h = i32(uniforms.height);
    var i = lane;
    while (i < CFA_TILE_ELS) {
        let ty = i32(i / CFA_TILE);
        let tx = i32(i % CFA_TILE);
        let gx = mirror_coord(tile_origin.x + tx, w);
        let gy = mirror_coord(tile_origin.y + ty, h);
        wg_cfa_tile[i] = load_cfa_pixel(gx, gy);
        i += WG_TOTAL;
    }
    workgroupBarrier();
}

fn read_green_stored_wg(c: i32, r: i32) -> f32 {
    let origin = wg_tile_origin;
    let tx = u32(c - origin.x);
    let ty = u32(r - origin.y);
    return wg_green_tile[ty * CFA_TILE + tx];
}

fn coop_load_green_tile(lane: u32) {
    let tile_origin = wg_tile_origin;
    let w = i32(uniforms.width);
    let h = i32(uniforms.height);
    var i = lane;
    while (i < CFA_TILE_ELS) {
        let ty = i32(i / CFA_TILE);
        let tx = i32(i % CFA_TILE);
        let gx = clamp(tile_origin.x + tx, 0, w - 1);
        let gy = clamp(tile_origin.y + ty, 0, h - 1);
        wg_green_tile[i] = textureLoad(green_plane_read, vec2<i32>(gx, gy), 0).r;
        i += WG_TOTAL;
    }
    workgroupBarrier();
}

fn get_color_channel(c: i32, r: i32) -> u32 {
    let x = c & 1;
    let y = r & 1;
    return get_bayer_color(y * 2 + x);
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
    return vec3<f32>(
        uniforms.rgb_cam0.x * rgb.r + uniforms.rgb_cam0.y * rgb.g + uniforms.rgb_cam0.z * rgb.b,
        uniforms.rgb_cam1.x * rgb.r + uniforms.rgb_cam1.y * rgb.g + uniforms.rgb_cam1.z * rgb.b,
        uniforms.rgb_cam2.x * rgb.r + uniforms.rgb_cam2.y * rgb.g + uniforms.rgb_cam2.z * rgb.b,
    );
}

fn libraw_clip_channel(v: f32) -> f32 {
    return f32(clamp(i32(v), 0, RAW16_MAX));
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

    let use_vertical = h_diff > v_diff;
    let v_result = ulim(
        v_guess / 4.0,
        read_green_plane(col, row + 1),
        read_green_plane(col, row - 1),
    );
    let h_result = ulim(
        h_guess / 4.0,
        read_green_plane(col + 1, row),
        read_green_plane(col - 1, row),
    );
    return select(h_result, v_result, use_vertical);
}

fn read_green_plane_wg(c: i32, r: i32) -> f32 {
    let fc = get_color_channel(c, r);
    if (fc == 1u || fc == 3u) {
        return read_cfa_wg(c, r);
    }
    return 0.0;
}

fn ppg_green_at_wg(col: i32, row: i32, c: u32) -> f32 {
    let x = read_cfa_wg(col, row);
    let h_guess = (read_green_plane_wg(col - 1, row) + x + read_green_plane_wg(col + 1, row)) * 2.0
        - read_cfa_wg(col - 2, row) - read_cfa_wg(col + 2, row);
    let h_diff = (abs_f(read_cfa_wg(col - 2, row) - x) + abs_f(read_cfa_wg(col + 2, row) - x)
        + abs_f(read_green_plane_wg(col - 1, row) - read_green_plane_wg(col + 1, row))) * 3.0
        + (abs_f(read_green_plane_wg(col + 3, row) - read_green_plane_wg(col + 1, row))
        + abs_f(read_green_plane_wg(col - 3, row) - read_green_plane_wg(col - 1, row))) * 2.0;
    let v_guess = (read_green_plane_wg(col, row - 1) + x + read_green_plane_wg(col, row + 1)) * 2.0
        - read_cfa_wg(col, row - 2) - read_cfa_wg(col, row + 2);
    let v_diff = (abs_f(read_cfa_wg(col, row - 2) - x) + abs_f(read_cfa_wg(col, row + 2) - x)
        + abs_f(read_green_plane_wg(col, row - 1) - read_green_plane_wg(col, row + 1))) * 3.0
        + (abs_f(read_green_plane_wg(col, row + 3) - read_green_plane_wg(col, row + 1))
        + abs_f(read_green_plane_wg(col, row - 3) - read_green_plane_wg(col, row - 1))) * 2.0;

    let use_vertical = h_diff > v_diff;
    let v_result = ulim(
        v_guess / 4.0,
        read_green_plane_wg(col, row + 1),
        read_green_plane_wg(col, row - 1),
    );
    let h_result = ulim(
        h_guess / 4.0,
        read_green_plane_wg(col + 1, row),
        read_green_plane_wg(col - 1, row),
    );
    return select(h_result, v_result, use_vertical);
}

fn read_green_stored(c: i32, r: i32) -> f32 {
    let x = clamp(c, 0, i32(uniforms.width) - 1);
    let y = clamp(r, 0, i32(uniforms.height) - 1);
    return textureLoad(green_plane_read, vec2<i32>(x, y), 0).r;
}

fn read_channel_stored_base(c: i32, r: i32, ch: u32) -> f32 {
    let fc = get_color_channel(c, r);
    if (fc == ch) {
        return read_cfa(c, r);
    }
    if (ch == 1u) {
        return read_green_stored(c, r);
    }
    return 0.0;
}

fn read_channel_stored_base_wg(c: i32, r: i32, ch: u32) -> f32 {
    let fc = get_color_channel(c, r);
    if (fc == ch) {
        return read_cfa_wg(c, r);
    }
    if (ch == 1u) {
        return read_green_stored_wg(c, r);
    }
    return 0.0;
}

fn read_channel_stored_wg(c: i32, r: i32, ch: u32) -> f32 {
    let fc = get_color_channel(c, r);
    if (fc == ch) {
        return read_cfa_wg(c, r);
    }
    if (ch == 1u) {
        return read_green_stored_wg(c, r);
    }
    if (fc == 1u || fc == 3u) {
        let green = read_green_stored_wg(c, r);
        let rgb = ppg_green_site_rgb_wg(c, r, green);
        if (ch == 0u) {
            return rgb.r;
        }
        return rgb.b;
    }
    return 0.0;
}

fn read_channel_stored(c: i32, r: i32, ch: u32) -> f32 {
    let fc = get_color_channel(c, r);
    if (fc == ch) {
        return read_cfa(c, r);
    }
    if (ch == 1u) {
        return read_green_stored(c, r);
    }
    // Inline LibRaw PPG pass 2 at green sites (avoids extra full-frame pass + textures).
    if (fc == 1u || fc == 3u) {
        let green = read_green_stored(c, r);
        let rgb = ppg_green_site_rgb(c, r, green);
        if (ch == 0u) {
            return rgb.r;
        }
        return rgb.b;
    }
    return 0.0;
}

// Pass 1 (LibRaw ppg): write interpolated green plane.
@compute @workgroup_size(WG_SIZE, WG_SIZE, 1)
fn cs_ppg_green(
    @builtin(workgroup_id) wg_id: vec3<u32>,
    @builtin(local_invocation_index) lane: u32,
    @builtin(global_invocation_id) gid: vec3<u32>,
) {
    let wg = vec2<i32>(i32(wg_id.x), i32(wg_id.y));
    coop_load_cfa_tile(wg, lane);
    let col = i32(gid.x);
    let row = i32(gid.y);
    if (col >= i32(uniforms.width) || row >= i32(uniforms.height)) {
        return;
    }
    let fc = get_color_channel(col, row);
    var green: f32;
    if (fc == 1u || fc == 3u) {
        green = read_cfa_wg(col, row);
    } else {
        green = ppg_green_at_wg(col, row, fc);
    }
    textureStore(green_plane_write, vec2<i32>(col, row), vec4<f32>(green));
}

// Pass 2: R/B interpolation using stored green (no recursive green recompute).
fn ppg_green_site_rgb(col: i32, row: i32, green: f32) -> vec3<f32> {
    var rgb = vec3<f32>(0.0, green, 0.0);
    var c = get_color_channel(col + 1, row);
    var v = (read_channel_stored_base(col - 1, row, c) + read_channel_stored_base(col + 1, row, c) + 2.0 * green
        - read_channel_stored_base(col - 1, row, 1u) - read_channel_stored_base(col + 1, row, 1u)) * 0.5;
    if (c == 0u) {
        rgb.r = v;
    } else {
        rgb.b = v;
    }
    c = 2u - c;
    v = (read_channel_stored_base(col, row - 1, c) + read_channel_stored_base(col, row + 1, c) + 2.0 * green
        - read_channel_stored_base(col, row - 1, 1u) - read_channel_stored_base(col, row + 1, 1u)) * 0.5;
    if (c == 0u) {
        rgb.r = v;
    } else {
        rgb.b = v;
    }
    return rgb;
}

fn ppg_green_site_rgb_wg(col: i32, row: i32, green: f32) -> vec3<f32> {
    var rgb = vec3<f32>(0.0, green, 0.0);
    var c = get_color_channel(col + 1, row);
    var v = (read_channel_stored_base_wg(col - 1, row, c) + read_channel_stored_base_wg(col + 1, row, c) + 2.0 * green
        - read_channel_stored_base_wg(col - 1, row, 1u) - read_channel_stored_base_wg(col + 1, row, 1u)) * 0.5;
    if (c == 0u) {
        rgb.r = v;
    } else {
        rgb.b = v;
    }
    c = 2u - c;
    v = (read_channel_stored_base_wg(col, row - 1, c) + read_channel_stored_base_wg(col, row + 1, c) + 2.0 * green
        - read_channel_stored_base_wg(col, row - 1, 1u) - read_channel_stored_base_wg(col, row + 1, 1u)) * 0.5;
    if (c == 0u) {
        rgb.r = v;
    } else {
        rgb.b = v;
    }
    return rgb;
}

// LibRaw ppg pass 3: missing chroma at R/B sites.
fn ppg_chroma_at_rb_wg(col: i32, row: i32, fc: u32, green: f32) -> f32 {
    let c = 2u - fc;
    let nd_c = read_channel_stored_wg(col - 1, row - 1, c);
    let pd_c = read_channel_stored_wg(col + 1, row + 1, c);
    let nd_g = read_channel_stored_wg(col - 1, row - 1, 1u);
    let pd_g = read_channel_stored_wg(col + 1, row + 1, 1u);
    let diff0 = abs_f(nd_c - pd_c) + abs_f(nd_g - green) + abs_f(pd_g - green);
    let guess0 = nd_c + pd_c + 2.0 * green - nd_g - pd_g;

    let nw_c = read_channel_stored_wg(col + 1, row - 1, c);
    let se_c = read_channel_stored_wg(col - 1, row + 1, c);
    let nw_g = read_channel_stored_wg(col + 1, row - 1, 1u);
    let se_g = read_channel_stored_wg(col - 1, row + 1, 1u);
    let diff1 = abs_f(nw_c - se_c) + abs_f(nw_g - green) + abs_f(se_g - green);
    let guess1 = nw_c + se_c + 2.0 * green - nw_g - se_g;

    let diff_equal = diff0 == diff1;
    let use_guess1 = diff0 > diff1;
    let result0 = guess0 * 0.5;
    let result1 = guess1 * 0.5;
    let result_equal = (guess0 + guess1) * 0.25;
    return select(select(result1, result0, use_guess1), result_equal, diff_equal);
}

@compute @workgroup_size(WG_SIZE, WG_SIZE, 1)
fn cs_ppg_rgb(
    @builtin(workgroup_id) wg_id: vec3<u32>,
    @builtin(local_invocation_index) lane: u32,
    @builtin(global_invocation_id) gid: vec3<u32>,
) {
    let wg = vec2<i32>(i32(wg_id.x), i32(wg_id.y));
    coop_load_cfa_tile(wg, lane);
    coop_load_green_tile(lane);
    let col = i32(gid.x);
    let row = i32(gid.y);
    if (col >= i32(uniforms.width) || row >= i32(uniforms.height)) {
        return;
    }

    let fc = get_color_channel(col, row);
    let green = read_green_stored_wg(col, row);
    var rgb: vec3<f32>;
    rgb.g = green;

    if (fc == 1u || fc == 3u) {
        let green_site = ppg_green_site_rgb_wg(col, row, green);
        rgb.r = green_site.r;
        rgb.b = green_site.b;
    } else if (fc == 0u) {
        rgb.r = read_cfa_wg(col, row);
        rgb.b = ppg_chroma_at_rb_wg(col, row, fc, green);
    } else {
        rgb.r = ppg_chroma_at_rb_wg(col, row, fc, green);
        rgb.b = read_cfa_wg(col, row);
    }

    rgb = apply_rgb_cam(rgb);
    rgb = vec3<f32>(
        libraw_clip_channel(rgb.r),
        libraw_clip_channel(rgb.g),
        libraw_clip_channel(rgb.b),
    );
    rgb = rgb * uniforms.output_scale * uniforms.scene_color_scale.xyz;
    // Scene-linear HDR: no clamp to 1.0; scene_color_scale is identity (auto_bright disabled on CPU develop too).

    textureStore(output_texture, vec2<i32>(col, row), vec4<f32>(rgb, 1.0));
}
"#;

const RAW16_MAX: f32 = 65535.0;
/// Must match `WG_SIZE` in [`RAW_DEMOSAIC_COMPUTE_SHADER`] (keep in sync manually).
pub const RAW_DEMOSAIC_WORKGROUP_SIZE: u32 = 16;

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
    pub scene_color_scale: [f32; 4],
}

impl RawDemosaicUniform {
    pub fn new(source: &RawGpuSource) -> Self {
        Self {
            width: source.width,
            height: source.height,
            output_scale: 1.0 / RAW16_MAX,
            _pad: 0,
            black_level: source.black_level,
            cfa_scale: source.cfa_scale,
            bayer_pattern: source.bayer_pattern,
            rgb_cam0: source.rgb_cam[0..4].try_into().expect("rgb_cam row 0"),
            rgb_cam1: source.rgb_cam[4..8].try_into().expect("rgb_cam row 1"),
            rgb_cam2: source.rgb_cam[8..12].try_into().expect("rgb_cam row 2"),
            scene_color_scale: [
                source.scene_color_scale[0],
                source.scene_color_scale[1],
                source.scene_color_scale[2],
                0.0,
            ],
        }
    }
}

pub(super) fn create_raw_demosaic_compute_resources(
    device: &wgpu::Device,
    pipeline_cache: Option<&wgpu::PipelineCache>,
) -> (
    wgpu::BindGroupLayout,
    wgpu::BindGroupLayout,
    wgpu::ComputePipeline,
    wgpu::ComputePipeline,
    wgpu::Buffer,
) {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("simple-image-viewer-raw-demosaic-shader"),
        source: wgpu::ShaderSource::Wgsl(RAW_DEMOSAIC_COMPUTE_SHADER.into()),
    });

    let green_bind_group_layout =
        device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("simple-image-viewer-raw-demosaic-green-bind-group-layout"),
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
                        access: wgpu::StorageTextureAccess::WriteOnly,
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

    let rgb_bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("simple-image-viewer-raw-demosaic-rgb-bind-group-layout"),
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
                binding: 4,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
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

    let green_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("simple-image-viewer-raw-demosaic-green-pipeline-layout"),
        bind_group_layouts: &[Some(&green_bind_group_layout)],
        immediate_size: 0,
    });

    let rgb_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("simple-image-viewer-raw-demosaic-rgb-pipeline-layout"),
        bind_group_layouts: &[Some(&rgb_bind_group_layout)],
        immediate_size: 0,
    });

    let green_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("simple-image-viewer-raw-demosaic-green-pipeline"),
        layout: Some(&green_pipeline_layout),
        module: &shader,
        entry_point: Some("cs_ppg_green"),
        compilation_options: wgpu::PipelineCompilationOptions::default(),
        cache: pipeline_cache,
    });

    let rgb_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("simple-image-viewer-raw-demosaic-rgb-pipeline"),
        layout: Some(&rgb_pipeline_layout),
        module: &shader,
        entry_point: Some("cs_ppg_rgb"),
        compilation_options: wgpu::PipelineCompilationOptions::default(),
        cache: pipeline_cache,
    });

    let dummy_uniform = RawDemosaicUniform {
        width: 1,
        height: 1,
        output_scale: 1.0 / RAW16_MAX,
        _pad: 0,
        black_level: [0.0; 4],
        cfa_scale: [1.0; 4],
        bayer_pattern: [0, 1, 1, 2], // Standard RGGB
        rgb_cam0: [1.0, 0.0, 0.0, 0.0],
        rgb_cam1: [0.0, 1.0, 0.0, 0.0],
        rgb_cam2: [0.0, 0.0, 1.0, 0.0],
        scene_color_scale: [1.0, 1.0, 1.0, 0.0],
    };

    let uniforms_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("simple-image-viewer-raw-demosaic-uniforms-buffer"),
        contents: bytemuck::bytes_of(&dummy_uniform),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });

    (
        green_bind_group_layout,
        rgb_bind_group_layout,
        green_pipeline,
        rgb_pipeline,
        uniforms_buffer,
    )
}

pub(crate) fn encode_raw_demosaic_compute_pass(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    green_bind_group_layout: &wgpu::BindGroupLayout,
    rgb_bind_group_layout: &wgpu::BindGroupLayout,
    green_pipeline: &wgpu::ComputePipeline,
    rgb_pipeline: &wgpu::ComputePipeline,
    source: &RawGpuSource,
    raw_pixels_view: &wgpu::TextureView,
    green_plane_write_view: &wgpu::TextureView,
    green_plane_read_view: &wgpu::TextureView,
    output_view: &wgpu::TextureView,
    uniform_buffer: &wgpu::Buffer,
) -> wgpu::CommandBuffer {
    // `uniform_buffer` is shared across HDR callback resources; egui-wgpu prepare is single-threaded.
    let uniform = RawDemosaicUniform::new(source);
    queue.write_buffer(uniform_buffer, 0, bytemuck::bytes_of(&uniform));

    let green_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("simple-image-viewer-raw-demosaic-green-bind-group"),
        layout: green_bind_group_layout,
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
                resource: wgpu::BindingResource::TextureView(green_plane_write_view),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(output_view),
            },
        ],
    });

    let rgb_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("simple-image-viewer-raw-demosaic-rgb-bind-group"),
        layout: rgb_bind_group_layout,
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
                binding: 4,
                resource: wgpu::BindingResource::TextureView(green_plane_read_view),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(output_view),
            },
        ],
    });

    let workgroups_x = source.width.div_ceil(RAW_DEMOSAIC_WORKGROUP_SIZE);
    let workgroups_y = source.height.div_ceil(RAW_DEMOSAIC_WORKGROUP_SIZE);

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("simple-image-viewer-raw-demosaic-encoder"),
    });
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("simple-image-viewer-raw-demosaic-green-pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(green_pipeline);
        pass.set_bind_group(0, &green_bind_group, &[]);
        pass.dispatch_workgroups(workgroups_x, workgroups_y, 1);
    }
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("simple-image-viewer-raw-demosaic-rgb-pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(rgb_pipeline);
        pass.set_bind_group(0, &rgb_bind_group, &[]);
        pass.dispatch_workgroups(workgroups_x, workgroups_y, 1);
    }
    encoder.finish()
}

/// CPU mirror of WGSL PPG demosaic (camera RGB before `convert_to_rgb`).
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

fn cpu_fc(source: &RawGpuSource, col: i32, row: i32) -> u32 {
    let x = ((col % 2) + 2) % 2;
    let y = ((row % 2) + 2) % 2;
    source.bayer_pattern[(y * 2 + x) as usize]
}

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

fn cpu_read_green_stored(plane: &[f32], source: &RawGpuSource, col: i32, row: i32) -> f32 {
    let w = source.width as usize;
    let x = col.clamp(0, source.width as i32 - 1) as usize;
    let y = row.clamp(0, source.height as i32 - 1) as usize;
    plane[y * w + x]
}

fn cpu_read_channel_stored(
    source: &RawGpuSource,
    read_cfa: &impl Fn(i32, i32) -> f32,
    green_plane: &[f32],
    rb_plane: &[(f32, f32)],
    col: i32,
    row: i32,
    ch: u32,
) -> f32 {
    let fc = cpu_fc(source, col, row);
    if fc == ch {
        return read_cfa(col, row);
    }
    if ch == 1 {
        return cpu_read_green_stored(green_plane, source, col, row);
    }
    if fc == 1 || fc == 3 {
        let w = source.width as usize;
        let x = col.clamp(0, source.width as i32 - 1) as usize;
        let y = row.clamp(0, source.height as i32 - 1) as usize;
        let (r, b) = rb_plane[y * w + x];
        if ch == 0 {
            return r;
        }
        if ch == 2 {
            return b;
        }
    }
    0.0
}

fn cpu_build_rb_at_green_plane(
    source: &RawGpuSource,
    read_cfa: &impl Fn(i32, i32) -> f32,
    green_plane: &[f32],
) -> Vec<(f32, f32)> {
    let w = source.width as usize;
    let h = source.height as usize;
    let mut plane = vec![(0.0f32, 0.0f32); w * h];
    for row in 0..source.height as i32 {
        for col in 0..source.width as i32 {
            let fc = cpu_fc(source, col, row);
            if fc != 1 && fc != 3 {
                continue;
            }
            let green = cpu_read_green_stored(green_plane, source, col, row);
            let rgb = cpu_ppg_green_site_rgb(source, read_cfa, green_plane, col, row, green);
            plane[row as usize * w + col as usize] = (rgb[0], rgb[2]);
        }
    }
    plane
}

fn cpu_ppg_green_site_rgb(
    source: &RawGpuSource,
    read_cfa: &impl Fn(i32, i32) -> f32,
    green_plane: &[f32],
    col: i32,
    row: i32,
    green: f32,
) -> [f32; 3] {
    let mut rgb = [0.0f32, green, 0.0];
    let mut c = cpu_fc(source, col + 1, row);
    let mut v = (cpu_read_channel_stored(source, read_cfa, green_plane, &[], col - 1, row, c)
        + cpu_read_channel_stored(source, read_cfa, green_plane, &[], col + 1, row, c)
        + 2.0 * green
        - cpu_read_channel_stored(source, read_cfa, green_plane, &[], col - 1, row, 1)
        - cpu_read_channel_stored(source, read_cfa, green_plane, &[], col + 1, row, 1))
        * 0.5;
    if c == 0 {
        rgb[0] = v;
    } else {
        rgb[2] = v;
    }
    c = 2 - c;
    v = (cpu_read_channel_stored(source, read_cfa, green_plane, &[], col, row - 1, c)
        + cpu_read_channel_stored(source, read_cfa, green_plane, &[], col, row + 1, c)
        + 2.0 * green
        - cpu_read_channel_stored(source, read_cfa, green_plane, &[], col, row - 1, 1)
        - cpu_read_channel_stored(source, read_cfa, green_plane, &[], col, row + 1, 1))
        * 0.5;
    if c == 0 {
        rgb[0] = v;
    } else {
        rgb[2] = v;
    }
    rgb
}

fn cpu_ppg_chroma_at_rb(
    source: &RawGpuSource,
    read_cfa: &impl Fn(i32, i32) -> f32,
    green_plane: &[f32],
    rb_plane: &[(f32, f32)],
    col: i32,
    row: i32,
    fc: u32,
    green: f32,
) -> f32 {
    let c = 2 - fc;
    let nd_c =
        cpu_read_channel_stored(source, read_cfa, green_plane, rb_plane, col - 1, row - 1, c);
    let pd_c =
        cpu_read_channel_stored(source, read_cfa, green_plane, rb_plane, col + 1, row + 1, c);
    let nd_g =
        cpu_read_channel_stored(source, read_cfa, green_plane, rb_plane, col - 1, row - 1, 1);
    let pd_g =
        cpu_read_channel_stored(source, read_cfa, green_plane, rb_plane, col + 1, row + 1, 1);
    let diff0 = (nd_c - pd_c).abs() + (nd_g - green).abs() + (pd_g - green).abs();
    let guess0 = nd_c + pd_c + 2.0 * green - nd_g - pd_g;

    let nw_c =
        cpu_read_channel_stored(source, read_cfa, green_plane, rb_plane, col + 1, row - 1, c);
    let se_c =
        cpu_read_channel_stored(source, read_cfa, green_plane, rb_plane, col - 1, row + 1, c);
    let nw_g =
        cpu_read_channel_stored(source, read_cfa, green_plane, rb_plane, col + 1, row - 1, 1);
    let se_g =
        cpu_read_channel_stored(source, read_cfa, green_plane, rb_plane, col - 1, row + 1, 1);
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

fn cpu_ppg_camera_rgb_at(
    source: &RawGpuSource,
    read_cfa: &impl Fn(i32, i32) -> f32,
    green_plane: &[f32],
    rb_plane: &[(f32, f32)],
    col: i32,
    row: i32,
) -> [f32; 3] {
    let fc = cpu_fc(source, col, row);
    let green = cpu_read_green_stored(green_plane, source, col, row);
    if fc == 0 {
        [
            read_cfa(col, row),
            green,
            cpu_ppg_chroma_at_rb(source, read_cfa, green_plane, rb_plane, col, row, fc, green),
        ]
    } else if fc == 2 {
        [
            cpu_ppg_chroma_at_rb(source, read_cfa, green_plane, rb_plane, col, row, fc, green),
            green,
            read_cfa(col, row),
        ]
    } else {
        let w = source.width as usize;
        let (r, b) = rb_plane[row as usize * w + col as usize];
        [r, green, b]
    }
}

#[cfg(test)]
#[cfg(test)]
pub(crate) fn cpu_demosaic_ppg_camera_counts(source: &RawGpuSource) -> Vec<f32> {
    let w = source.width as usize;
    let read_cfa = cpu_ppg_helpers(source);
    let green_plane = cpu_build_green_plane(source, &read_cfa);
    let rb_plane = cpu_build_rb_at_green_plane(source, &read_cfa, &green_plane);
    let mut out = vec![0.0f32; w * source.height as usize * 3];
    for row in 0..source.height as i32 {
        for col in 0..source.width as i32 {
            let rgb = cpu_ppg_camera_rgb_at(source, &read_cfa, &green_plane, &rb_plane, col, row);
            let i = (row as usize * w + col as usize) * 3;
            out[i..i + 3].copy_from_slice(&rgb);
        }
    }
    out
}

/// CPU mirror of the GPU PPG demosaic + LibRaw color path (tests / calibration).
pub(crate) fn cpu_demosaic_ppg_scene_linear(source: &RawGpuSource) -> Vec<f32> {
    let w = source.width as usize;
    let output_scale = 1.0f32 / 65535.0;
    let read_cfa = cpu_ppg_helpers(source);
    let green_plane = cpu_build_green_plane(source, &read_cfa);
    let rb_plane = cpu_build_rb_at_green_plane(source, &read_cfa, &green_plane);
    let mut out = vec![0.0f32; w * source.height as usize * 4];
    let apply_rgb_cam = |rgb: [f32; 3]| -> [f32; 3] {
        let m = &source.rgb_cam;
        [
            m[0] * rgb[0] + m[1] * rgb[1] + m[2] * rgb[2],
            m[4] * rgb[0] + m[5] * rgb[1] + m[6] * rgb[2],
            m[8] * rgb[0] + m[9] * rgb[1] + m[10] * rgb[2],
        ]
    };
    let clip = |v: f32| (v as i32).clamp(0, 65535) as f32;
    for row in 0..source.height as i32 {
        for col in 0..source.width as i32 {
            let rgb = cpu_ppg_camera_rgb_at(source, &read_cfa, &green_plane, &rb_plane, col, row);
            let linear = apply_rgb_cam(rgb);
            let i = (row as usize * w + col as usize) * 4;
            out[i] = clip(linear[0]) * output_scale * source.scene_color_scale[0];
            out[i + 1] = clip(linear[1]) * output_scale * source.scene_color_scale[1];
            out[i + 2] = clip(linear[2]) * output_scale * source.scene_color_scale[2];
            out[i + 3] = 1.0;
        }
    }
    out
}

pub(crate) fn scene_linear_center_luma_from_source(source: &RawGpuSource) -> f64 {
    const HALO: i32 = 32;
    let w = source.width as i32;
    let h = source.height as i32;
    if w == 0 || h == 0 {
        return 0.0;
    }
    let cx = w / 2;
    let cy = h / 2;
    let mut x0 = cx - 32 - HALO;
    let mut y0 = cy - 32 - HALO;
    let mut x1 = cx + 32 + HALO;
    let mut y1 = cy + 32 + HALO;
    x0 = (x0 & !1).max(0);
    y0 = (y0 & !1).max(0);
    x1 = x1.min(w - 1);
    y1 = y1.min(h - 1);
    let crop_w = ((x1 - x0 + 1) / 2) * 2;
    let crop_h = ((y1 - y0 + 1) / 2) * 2;
    let crop = crop_raw_gpu_source(source, x0 as u32, y0 as u32, crop_w as u32, crop_h as u32);
    let mut unit = crop;
    unit.scene_color_scale = [1.0, 1.0, 1.0];
    let rgba = cpu_demosaic_ppg_scene_linear(&unit);
    let crop_cx = cx - x0;
    let crop_cy = cy - y0;
    scene_linear_center_luma_sum_at(
        &rgba,
        unit.width,
        unit.height,
        crop_cx as usize,
        crop_cy as usize,
    )
}

fn scene_linear_center_luma_sum_at(
    rgba: &[f32],
    width: u32,
    height: u32,
    center_x: usize,
    center_y: usize,
) -> f64 {
    let w = width as usize;
    let h = height as usize;
    let mut sum = 0.0f64;
    for dy in 0..64 {
        for dx in 0..64 {
            let x = center_x as i32 + dx - 32;
            let y = center_y as i32 + dy - 32;
            if x < 0 || y < 0 || x as usize >= w || y as usize >= h {
                continue;
            }
            let i = (y as usize * w + x as usize) * 4;
            if i + 2 >= rgba.len() {
                continue;
            }
            sum += rgba[i] as f64 + rgba[i + 1] as f64 + rgba[i + 2] as f64;
        }
    }
    sum
}

fn crop_raw_gpu_source(
    source: &RawGpuSource,
    x0: u32,
    y0: u32,
    crop_w: u32,
    crop_h: u32,
) -> RawGpuSource {
    let src_w = source.width as usize;
    let mut pixels = Vec::with_capacity(crop_w as usize * crop_h as usize);
    for y in 0..crop_h {
        let sy = y0 + y;
        for x in 0..crop_w {
            let sx = x0 + x;
            pixels.push(source.raw_pixels[sy as usize * src_w + sx as usize]);
        }
    }
    RawGpuSource {
        raw_width: source.raw_width,
        raw_height: source.raw_height,
        width: crop_w,
        height: crop_h,
        raw_pixels: std::sync::Arc::new(pixels),
        black_level: source.black_level,
        cfa_scale: source.cfa_scale,
        rgb_cam: source.rgb_cam,
        maximum: source.maximum,
        bayer_pattern: source.bayer_pattern,
        demosaic_method: source.demosaic_method,
        scene_color_scale: [1.0, 1.0, 1.0],
        bootstrap_preview: None,
    }
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
    use std::path::PathBuf;

    fn raw_integration_sample_path(subpath: &str) -> PathBuf {
        if let Ok(root) = std::env::var("SIV_RAW_TEST_ROOT") {
            return PathBuf::from(root).join(subpath);
        }
        PathBuf::from(r"F:\win7\raws").join(subpath)
    }

    #[test]
    fn raw_demosaic_compute_shader_parses_as_wgsl() {
        naga::front::wgsl::parse_str(RAW_DEMOSAIC_COMPUTE_SHADER)
            .expect("RAW demosaic compute shader must parse before runtime pipeline creation");
    }

    /// Requires a Canon 5D2 CR2 sample (`SIV_RAW_TEST_ROOT` or dev fallback path).
    #[test]
    fn diff_canon_5d2_ppg_gpu_path_vs_libraw_cpu() {
        let path = raw_integration_sample_path(r"canon\5dm2\RAW_CANON_5DMARK2_PREPROD.CR2");
        if !path.is_file() {
            eprintln!("skip: Canon 5D2 sample not present at {}", path.display());
            return;
        }
        let mut processor = crate::raw_processor::RawProcessor::new().expect("libraw init");
        processor.open(&path).expect("libraw open");
        let source = processor
            .extract_raw_gpu_source(crate::settings::RawDemosaicMethod::Ppg)
            .expect("extract gpu source");
        let mut source = source;
        source.scene_color_scale =
            crate::raw_processor::RawProcessor::estimate_gpu_scene_color_scale(&path);
        eprintln!("canon 5d2 scene_color_scale={:?}", source.scene_color_scale);
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
        let m = &source.rgb_cam;
        eprintln!(
            "canon 5d2 rgb_cam matrix: row0={:?}, row1={:?}, row2={:?}",
            &m[0..4],
            &m[4..8],
            &m[8..12]
        );
        let sample = [13729.0f32, 8838.0, 15838.0];
        let lr = (
            m[0] * sample[0] + m[1] * sample[1] + m[2] * sample[2],
            m[4] * sample[0] + m[5] * sample[1] + m[6] * sample[2],
            m[8] * sample[0] + m[9] * sample[1] + m[10] * sample[2],
        );
        eprintln!(
            "canon 5d2 manual rgb_cam*counts/65535=({:.4}, {:.4}, {:.4})",
            lr.0 / 65535.0,
            lr.1 / 65535.0,
            lr.2 / 65535.0,
        );

        let hdr = {
            let mut develop_processor =
                crate::raw_processor::RawProcessor::new().expect("libraw init");
            develop_processor.open(&path).expect("libraw open");
            develop_processor
                .develop_scene_linear_hdr()
                .expect("develop_scene_linear_hdr")
        };
        let cpu = hdr.rgba_f32.as_slice();
        let (cr, cg, cb) = center_mean_rgba(cpu, w, h);
        let cpu_rb = cr / cb.max(1e-9);

        eprintln!("canon 5d2 gpu-shader center avg=({gr:.4}, {gg:.4}, {gb:.4}) R/B={gpu_rb:.3}");
        eprintln!("canon 5d2 libraw cpu center avg=({cr:.4}, {cg:.4}, {cb:.4}) R/B={cpu_rb:.3}");
        eprintln!(
            "note: both paths use LibRaw PPG demosaic; expect small rgb_cam / auto_bright diff only"
        );
    }

    /// Requires a Canon 5D2 CR2 sample (`SIV_RAW_TEST_ROOT` or dev fallback path).
    #[test]
    fn diff_canon_5d2_ppg_counts_via_libraw_output_color() {
        let path = raw_integration_sample_path(r"canon\5dm2\RAW_CANON_5DMARK2_PREPROD.CR2");
        if !path.is_file() {
            eprintln!("skip: Canon 5D2 sample not present at {}", path.display());
            return;
        }
        let mut processor = crate::raw_processor::RawProcessor::new().expect("libraw init");
        processor.open(&path).expect("libraw open");
        let source = processor
            .extract_raw_gpu_source(crate::settings::RawDemosaicMethod::Ppg)
            .expect("extract gpu source");
        let mut source = source;
        source.scene_color_scale =
            crate::raw_processor::RawProcessor::estimate_gpu_scene_color_scale(&path);
        eprintln!("canon 5d2 scene_color_scale={:?}", source.scene_color_scale);
        let w = source.width as usize;
        let h = source.height as usize;
        let cx = w / 2;
        let cy = h / 2;

        let counts = cpu_demosaic_ppg_camera_counts(&source);
        if let Ok(libraw_counts) = {
            let mut ref_processor = crate::raw_processor::RawProcessor::new().expect("libraw init");
            ref_processor.open(&path).expect("libraw open");
            ref_processor.libraw_ppg_camera_rgb_counts()
        } {
            let ci = (cy * w + cx) * 3;
            eprintln!(
                "canon 5d2 libraw counts at center=({:.0}, {:.0}, {:.0})",
                libraw_counts[ci] as f32,
                libraw_counts[ci + 1] as f32,
                libraw_counts[ci + 2] as f32,
            );
            let mut lr_r = 0.0f64;
            let mut lr_g = 0.0f64;
            let mut lr_b = 0.0f64;
            let mut ours_r = 0.0f64;
            let mut ours_g = 0.0f64;
            let mut ours_b = 0.0f64;
            for dy in 0..64 {
                for dx in 0..64 {
                    let x = cx + dx - 32;
                    let y = cy + dy - 32;
                    if x >= w || y >= h {
                        continue;
                    }
                    let i = (y * w + x) * 3;
                    lr_r += libraw_counts[i] as f64;
                    lr_g += libraw_counts[i + 1] as f64;
                    lr_b += libraw_counts[i + 2] as f64;
                    ours_r += counts[i] as f64;
                    ours_g += counts[i + 1] as f64;
                    ours_b += counts[i + 2] as f64;
                }
            }
            let n = 64.0 * 64.0;
            eprintln!(
                "canon 5d2 libraw PPG camera counts center avg=({:.0}, {:.0}, {:.0})",
                lr_r / n,
                lr_g / n,
                lr_b / n,
            );
            eprintln!(
                "canon 5d2 ours PPG camera counts center avg=({:.0}, {:.0}, {:.0})",
                ours_r / n,
                ours_g / n,
                ours_b / n,
            );
        }
        let gpu_path = cpu_demosaic_ppg_scene_linear(&source);
        let (gr, gg, gb) = center_mean_rgba(&gpu_path, w, h);
        eprintln!(
            "canon 5d2 gpu-shader center avg=({gr:.4}, {gg:.4}, {gb:.4}) R/B={:.3}",
            gr / gb.max(1e-9)
        );

        let hdr = {
            let mut develop_processor =
                crate::raw_processor::RawProcessor::new().expect("libraw init");
            develop_processor.open(&path).expect("libraw open");
            develop_processor
                .develop_scene_linear_hdr()
                .expect("develop_scene_linear_hdr")
        };
        let (cr, cg, cb) = center_mean_rgba(hdr.rgba_f32.as_slice(), w, h);
        eprintln!(
            "canon 5d2 libraw cpu (AHD) center avg=({cr:.4}, {cg:.4}, {cb:.4}) R/B={:.3}",
            cr / cb.max(1e-9)
        );

        let hdr_ppg = {
            let mut p = crate::raw_processor::RawProcessor::new().expect("libraw init");
            p.open(&path).expect("libraw open");
            p.develop_scene_linear_hdr_with_qual(false, 2)
                .expect("develop PPG")
        };
        let (pr, pg, pb) = center_mean_rgba(hdr_ppg.rgba_f32.as_slice(), w, h);
        eprintln!(
            "canon 5d2 libraw PPG center=({pr:.4}, {pg:.4}, {pb:.4}) R/B={:.3}",
            pr / pb.max(1e-9)
        );

        let hdr_ppg_no_ab = {
            let mut p = crate::raw_processor::RawProcessor::new().expect("libraw init");
            p.open(&path).expect("libraw open");
            p.develop_scene_linear_hdr_with_qual(true, 2)
                .expect("develop PPG no auto bright")
        };
        let (pnr, png, pnb) = center_mean_rgba(hdr_ppg_no_ab.rgba_f32.as_slice(), w, h);
        eprintln!(
            "canon 5d2 libraw PPG no_auto_bright=({pnr:.4}, {png:.4}, {pnb:.4}) R/B={:.3}",
            pnr / pnb.max(1e-9)
        );

        let _ = counts;
        eprintln!(
            "note: GPU WGSL uses PPG demosaic; CPU default is AHD — compare no_auto_bright to isolate color matrix vs auto_bright vs demosaic"
        );
    }

    #[test]
    fn diff_canon_40d_ppg_counts_via_libraw_output_color() {
        let path = raw_integration_sample_path(r"canon\40d\RAW_CANON_40D_RAW_V103.CR2");
        if !path.is_file() {
            eprintln!("skip: Canon 40D sample not present at {}", path.display());
            return;
        }
        let mut processor = crate::raw_processor::RawProcessor::new().expect("libraw init");
        processor.open(&path).expect("libraw open");
        let source = processor
            .extract_raw_gpu_source(crate::settings::RawDemosaicMethod::Ppg)
            .expect("extract gpu source");
        let mut source = source;
        source.scene_color_scale = [1.0, 1.0, 1.0];
        eprintln!(
            "canon 40d source meta: maximum={} black_level={:?} cfa_scale={:?} rgb_cam={:?} bayer={:?}",
            source.maximum,
            source.black_level,
            source.cfa_scale,
            source.rgb_cam,
            source.bayer_pattern
        );
        let (left_margin, top_margin) = processor.margins();
        eprintln!(
            "canon 40d margins: left_margin={}, top_margin={}",
            left_margin, top_margin
        );

        let w = source.width as usize;
        let h = source.height as usize;
        let cx = w / 2;
        let cy = h / 2;

        let counts = cpu_demosaic_ppg_camera_counts(&source);
        if let Ok(libraw_counts) = {
            let mut ref_processor = crate::raw_processor::RawProcessor::new().expect("libraw init");
            ref_processor.open(&path).expect("libraw open");
            ref_processor.libraw_ppg_camera_rgb_counts()
        } {
            let mut lr_r = 0.0f64;
            let mut lr_g = 0.0f64;
            let mut lr_b = 0.0f64;
            let mut ours_r = 0.0f64;
            let mut ours_g = 0.0f64;
            let mut ours_b = 0.0f64;
            for dy in 0..64 {
                for dx in 0..64 {
                    let x = cx + dx - 32;
                    let y = cy + dy - 32;
                    if x >= w || y >= h {
                        continue;
                    }
                    let i = (y * w + x) * 3;
                    lr_r += libraw_counts[i] as f64;
                    lr_g += libraw_counts[i + 1] as f64;
                    lr_b += libraw_counts[i + 2] as f64;
                    ours_r += counts[i] as f64;
                    ours_g += counts[i + 1] as f64;
                    ours_b += counts[i + 2] as f64;
                }
            }
            let n = 64.0 * 64.0;
            eprintln!(
                "canon 40d libraw PPG camera counts center avg=({:.0}, {:.0}, {:.0})",
                lr_r / n,
                lr_g / n,
                lr_b / n,
            );
            eprintln!(
                "canon 40d ours PPG camera counts center avg=({:.0}, {:.0}, {:.0})",
                ours_r / n,
                ours_g / n,
                ours_b / n,
            );
            assert!(
                (ours_r / n - lr_r / n).abs() < 0.5
                    && (ours_g / n - lr_g / n).abs() < 0.5
                    && (ours_b / n - lr_b / n).abs() < 0.5,
                "GPU PPG demosaic camera counts must match LibRaw before rgb_cam"
            );
        }
        let gpu_path = cpu_demosaic_ppg_scene_linear(&source);
        let (gr, gg, gb) = center_mean_rgba(&gpu_path, w, h);
        eprintln!(
            "canon 40d gpu-shader (linear baseline) center avg=({gr:.4}, {gg:.4}, {gb:.4}) R/B={:.3}",
            gr / gb.max(1e-9)
        );

        let hdr_ppg = {
            let mut develop_processor =
                crate::raw_processor::RawProcessor::new().expect("libraw init");
            develop_processor.open(&path).expect("libraw open");
            develop_processor
                .develop_scene_linear_hdr()
                .expect("develop PPG no auto_bright")
        };
        let (cr, cg, cb) = center_mean_rgba(hdr_ppg.rgba_f32.as_slice(), w, h);
        eprintln!(
            "canon 40d libraw cpu (PPG no auto_bright) center avg=({cr:.4}, {cg:.4}, {cb:.4}) R/B={:.3}",
            cr / cb.max(1e-9)
        );
        assert!(
            (gr - cr).abs() < 0.01 && (gg - cg).abs() < 0.01 && (gb - cb).abs() < 0.01,
            "GPU PPG path must match LibRaw linear PPG develop at center"
        );
        let hdr_ppg_ab = {
            let mut p = crate::raw_processor::RawProcessor::new().expect("libraw init");
            p.open(&path).expect("libraw open");
            p.develop_scene_linear_hdr_with_qual(false, 2)
                .expect("develop PPG auto_bright")
        };
        let (ar, ag, ab) = center_mean_rgba(hdr_ppg_ab.rgba_f32.as_slice(), w, h);
        eprintln!(
            "canon 40d libraw PPG auto_bright=({ar:.4}, {ag:.4}, {ab:.4}) R/B={:.3}",
            ar / ab.max(1e-9)
        );
    }
}
