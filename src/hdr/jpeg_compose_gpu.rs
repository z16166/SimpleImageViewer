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

//! GPU compute path for deferred Ultra HDR / ISO 21496 JPEG gain-map composition.

use super::HdrCallbackResources;
use crate::hdr::gain_map::gain_map_weight;
use crate::hdr::types::{
    HdrImageBuffer, HdrToneMapSettings, IsoDeferredTileContext, IsoGainMapGpuSource,
};
use wgpu::util::DeviceExt;

const COMPOSE_WORKGROUP_SIZE: u32 = 16;

/// ISO / Adobe gain-map compose — baseline SDR + gain textures → linear HDR storage texture.
pub(super) const JPEG_GAIN_COMPOSE_SHADER: &str = r#"
struct JpegGainMapComposeSettings {
    gain_map_min: vec3<f32>,
    _pad0: f32,
    gain_map_max: vec3<f32>,
    _pad1: f32,
    gamma: vec3<f32>,
    _pad2: f32,
    offset_sdr: vec3<f32>,
    _pad3: f32,
    offset_hdr: vec3<f32>,
    gain_weight: f32,
    gain_width: u32,
    gain_height: u32,
    physical_width: u32,
    physical_height: u32,
    tile_origin_x: u32,
    tile_origin_y: u32,
    tile_width: u32,
    tile_height: u32,
    orientation: u32,
    _pad4: u32,
    _pad5: u32,
    _pad6: u32,
};

@group(0) @binding(0) var sdr_texture: texture_2d<f32>;
@group(0) @binding(1) var gain_map_texture: texture_2d<f32>;
@group(0) @binding(2) var<uniform> settings: JpegGainMapComposeSettings;
@group(0) @binding(3) var compose_output: texture_storage_2d<rgba32float, write>;

fn srgb_to_linear(channel: f32) -> f32 {
    let c = clamp(channel, 0.0, 1.0);
    let low = c / 12.92;
    let high = pow((c + 0.055) / 1.055, 2.4);
    return select(high, low, c <= 0.04045);
}

fn sample_gain_map_rgb(px: i32, py: i32) -> vec3<f32> {
    let gain_dims_f = vec2<f32>(f32(settings.gain_width), f32(settings.gain_height));
    let primary_dims_f = vec2<f32>(f32(settings.physical_width), f32(settings.physical_height));
    let p_coord = vec2<f32>(f32(px), f32(py));
    let g_coord = clamp(
        (p_coord + vec2<f32>(0.5)) * gain_dims_f / primary_dims_f - vec2<f32>(0.5),
        vec2<f32>(0.0),
        gain_dims_f - vec2<f32>(1.0),
    );
    let xy0 = vec2<i32>(i32(floor(g_coord.x)), i32(floor(g_coord.y)));
    let xy1 = min(
        xy0 + vec2<i32>(1, 1),
        vec2<i32>(i32(settings.gain_width) - 1, i32(settings.gain_height) - 1),
    );
    let t = g_coord - vec2<f32>(xy0);

    let p00 = textureLoad(gain_map_texture, xy0, 0).rgb;
    let p10 = textureLoad(gain_map_texture, vec2<i32>(xy1.x, xy0.y), 0).rgb;
    let p01 = textureLoad(gain_map_texture, vec2<i32>(xy0.x, xy1.y), 0).rgb;
    let p11 = textureLoad(gain_map_texture, xy1, 0).rgb;
    let mix_x0 = mix(p00, p10, t.x);
    let mix_x1 = mix(p01, p11, t.x);
    return mix(mix_x0, mix_x1, t.y);
}

fn recover_hdr_channel(
    sdr_channel: f32,
    gain_value: f32,
    channel_index: u32,
) -> f32 {
    var gain_map_min = settings.gain_map_min;
    var gain_map_max = settings.gain_map_max;
    var gamma = settings.gamma;
    var offset_sdr = settings.offset_sdr;
    var offset_hdr = settings.offset_hdr;
    let gi = min(channel_index, 2u);
    let g = max(gamma[gi], 1e-20);
    let log_boost = gain_map_min[gi]
        + (gain_map_max[gi] - gain_map_min[gi]) * pow(gain_value, 1.0 / g) * settings.gain_weight;
    let boost = pow(2.0, log_boost);
    let linear_sdr = srgb_to_linear(sdr_channel);
    return max((linear_sdr + offset_sdr[gi]) * boost - offset_hdr[gi], 0.0);
}

fn compose_at_primary_pixel(px: i32, py: i32) -> vec4<f32> {
    let sdr = textureLoad(sdr_texture, vec2<i32>(px, py), 0);
    let gain = sample_gain_map_rgb(px, py);
    let rgb = vec3<f32>(
        recover_hdr_channel(sdr.r, gain.r, 0u),
        recover_hdr_channel(sdr.g, gain.g, 1u),
        recover_hdr_channel(sdr.b, gain.b, 2u),
    );
    return vec4<f32>(rgb, sdr.a);
}

fn display_to_physical_pixel(
    display_x: u32,
    display_y: u32,
    physical_width: u32,
    physical_height: u32,
    orientation: u32,
) -> vec2<i32> {
    switch orientation {
        case 2u: {
            return vec2<i32>(i32(physical_width - 1u - display_x), i32(display_y));
        }
        case 3u: {
            return vec2<i32>(
                i32(physical_width - 1u - display_x),
                i32(physical_height - 1u - display_y),
            );
        }
        case 4u: {
            return vec2<i32>(i32(display_x), i32(physical_height - 1u - display_y));
        }
        case 5u: {
            return vec2<i32>(i32(display_y), i32(display_x));
        }
        case 6u: {
            return vec2<i32>(i32(display_y), i32(physical_height - 1u - display_x));
        }
        case 7u: {
            return vec2<i32>(
                i32(physical_width - 1u - display_y),
                i32(physical_height - 1u - display_x),
            );
        }
        case 8u: {
            return vec2<i32>(i32(physical_width - 1u - display_y), i32(display_x));
        }
        default: {
            return vec2<i32>(i32(display_x), i32(display_y));
        }
    }
}

// NOTE: The compose compute shader is run ONLY ONCE when the image is first loaded or when
// target display capacity changes, rather than run every frame during transition.
// Therefore, we must compose the ENTIRE primary image including pixels outside the ripple circle.
// If we were to skip compose for pixels outside the ripple radius here, those pixels would remain
// uncomposed/empty when the ripple radius expands in subsequent frames of the transition animation.
// Discarding fragments outside the circle is instead handled efficiently in the fragment shader `fs_main`.
@compute @workgroup_size(16, 16, 1)
fn cs_compose_jpeg_gain(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= settings.physical_width || gid.y >= settings.physical_height {
        return;
    }
    let px = i32(gid.x);
    let py = i32(gid.y);
    let out = compose_at_primary_pixel(px, py);
    textureStore(compose_output, vec2<i32>(px, py), out);
}

@compute @workgroup_size(16, 16, 1)
fn cs_compose_jpeg_gain_tile(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= settings.tile_width || gid.y >= settings.tile_height {
        return;
    }
    let display_x = settings.tile_origin_x + gid.x;
    let display_y = settings.tile_origin_y + gid.y;
    let physical = display_to_physical_pixel(
        display_x,
        display_y,
        settings.physical_width,
        settings.physical_height,
        settings.orientation,
    );
    let out = compose_at_primary_pixel(physical.x, physical.y);
    textureStore(compose_output, vec2<i32>(i32(gid.x), i32(gid.y)), out);
}
"#;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct JpegGainMapComposeUniform {
    gain_map_min: [f32; 3],
    _pad0: f32,
    gain_map_max: [f32; 3],
    _pad1: f32,
    gamma: [f32; 3],
    _pad2: f32,
    offset_sdr: [f32; 3],
    _pad3: f32,
    offset_hdr: [f32; 3],
    gain_weight: f32,
    gain_width: u32,
    gain_height: u32,
    physical_width: u32,
    physical_height: u32,
    tile_origin_x: u32,
    tile_origin_y: u32,
    tile_width: u32,
    tile_height: u32,
    orientation: u32,
    _pad4: u32,
    _pad5: u32,
    _pad6: u32,
}

const _: () = assert!(std::mem::size_of::<JpegGainMapComposeUniform>() == 128);

// Both encode_* paths write `resources.jpeg_compose_uniform_buffer` immediately before
// recording their compute pass. This is safe without a second buffer: static full-frame
// compose and tiled compose are mutually exclusive at the ImageData/callback level, and
// egui-wgpu invokes paint-callback `prepare` sequentially (see CallbackTrait docs).

pub(super) fn create_jpeg_compose_compute_resources(
    device: &wgpu::Device,
) -> (
    wgpu::BindGroupLayout,
    wgpu::ComputePipeline,
    wgpu::ComputePipeline,
    wgpu::Buffer,
) {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("simple-image-viewer-hdr-jpeg-compose-shader"),
        source: wgpu::ShaderSource::Wgsl(JPEG_GAIN_COMPOSE_SHADER.into()),
    });
    let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("simple-image-viewer-hdr-jpeg-compose-bind-group-layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
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
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("simple-image-viewer-hdr-jpeg-compose-pipeline-layout"),
        bind_group_layouts: &[Some(&bind_group_layout)],
        immediate_size: 0,
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("simple-image-viewer-hdr-jpeg-compose-pipeline"),
        layout: Some(&pipeline_layout),
        module: &shader,
        entry_point: Some("cs_compose_jpeg_gain"),
        compilation_options: wgpu::PipelineCompilationOptions::default(),
        cache: None,
    });
    let tile_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("simple-image-viewer-hdr-jpeg-compose-tile-pipeline"),
        layout: Some(&pipeline_layout),
        module: &shader,
        entry_point: Some("cs_compose_jpeg_gain_tile"),
        compilation_options: wgpu::PipelineCompilationOptions::default(),
        cache: None,
    });
    let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("simple-image-viewer-hdr-jpeg-compose-uniform-buffer"),
        contents: bytemuck::bytes_of(&JpegGainMapComposeUniform {
            gain_map_min: [0.0; 3],
            _pad0: 0.0,
            gain_map_max: [0.0; 3],
            _pad1: 0.0,
            gamma: [1.0; 3],
            _pad2: 0.0,
            offset_sdr: [0.0; 3],
            _pad3: 0.0,
            offset_hdr: [0.0; 3],
            gain_weight: 0.0,
            gain_width: 0,
            gain_height: 0,
            physical_width: 0,
            physical_height: 0,
            tile_origin_x: 0,
            tile_origin_y: 0,
            tile_width: 0,
            tile_height: 0,
            orientation: 0,
            _pad4: 0,
            _pad5: 0,
            _pad6: 0,
        }),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });
    (bind_group_layout, pipeline, tile_pipeline, uniform_buffer)
}

fn compose_uniform_fields(
    deferred: &IsoGainMapGpuSource,
    physical_width: u32,
    physical_height: u32,
    target_hdr_capacity: f32,
) -> JpegGainMapComposeUniform {
    let metadata = deferred.metadata;
    JpegGainMapComposeUniform {
        gain_map_min: metadata.gain_map_min,
        _pad0: 0.0,
        gain_map_max: metadata.gain_map_max,
        _pad1: 0.0,
        gamma: metadata.gamma,
        _pad2: 0.0,
        offset_sdr: metadata.offset_sdr,
        _pad3: 0.0,
        offset_hdr: metadata.offset_hdr,
        gain_weight: gain_map_weight(metadata, target_hdr_capacity),
        gain_width: deferred.gain_width,
        gain_height: deferred.gain_height,
        physical_width,
        physical_height,
        tile_origin_x: 0,
        tile_origin_y: 0,
        tile_width: 0,
        tile_height: 0,
        orientation: 0,
        _pad4: 0,
        _pad5: 0,
        _pad6: 0,
    }
}

fn compose_uniform(
    deferred: &IsoGainMapGpuSource,
    image: &HdrImageBuffer,
    target_hdr_capacity: f32,
) -> JpegGainMapComposeUniform {
    compose_uniform_fields(deferred, image.width, image.height, target_hdr_capacity)
}

fn compose_tile_uniform(
    deferred: &IsoGainMapGpuSource,
    tile_ctx: &IsoDeferredTileContext,
    tile_width: u32,
    tile_height: u32,
    target_hdr_capacity: f32,
) -> JpegGainMapComposeUniform {
    let mut uniform = compose_uniform_fields(
        deferred,
        tile_ctx.physical_width,
        tile_ctx.physical_height,
        target_hdr_capacity,
    );
    uniform.tile_origin_x = tile_ctx.origin_x;
    uniform.tile_origin_y = tile_ctx.origin_y;
    uniform.tile_width = tile_width;
    uniform.tile_height = tile_height;
    uniform.orientation = tile_ctx.orientation as u32;
    uniform
}

pub(super) fn encode_compose_compute_pass(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    bind_group_layout: &wgpu::BindGroupLayout,
    pipeline: &wgpu::ComputePipeline,
    image: &HdrImageBuffer,
    deferred: &IsoGainMapGpuSource,
    tone_map: &HdrToneMapSettings,
    sdr_view: &wgpu::TextureView,
    gain_view: &wgpu::TextureView,
    display_storage_view: &wgpu::TextureView,
    uniform_buffer: &wgpu::Buffer,
) -> wgpu::CommandBuffer {
    let uniform = compose_uniform(deferred, image, tone_map.target_hdr_capacity());
    queue.write_buffer(uniform_buffer, 0, bytemuck::bytes_of(&uniform));

    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("simple-image-viewer-hdr-jpeg-compose-bind-group"),
        layout: bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(sdr_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(gain_view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: uniform_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(display_storage_view),
            },
        ],
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("simple-image-viewer-hdr-jpeg-compose-encoder"),
    });
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("simple-image-viewer-hdr-jpeg-compose-pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups(
            image.width.div_ceil(COMPOSE_WORKGROUP_SIZE),
            image.height.div_ceil(COMPOSE_WORKGROUP_SIZE),
            1,
        );
    }
    encoder.finish()
}

pub(super) fn encode_tile_compose_compute_pass(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    resources: &HdrCallbackResources,
    deferred: &IsoGainMapGpuSource,
    tile_ctx: &IsoDeferredTileContext,
    tile_width: u32,
    tile_height: u32,
    tone_map: &HdrToneMapSettings,
    sdr_view: &wgpu::TextureView,
    gain_view: &wgpu::TextureView,
    display_storage_view: &wgpu::TextureView,
) -> wgpu::CommandBuffer {
    let uniform = compose_tile_uniform(
        deferred,
        tile_ctx,
        tile_width,
        tile_height,
        tone_map.target_hdr_capacity(),
    );
    queue.write_buffer(
        &resources.jpeg_compose_uniform_buffer,
        0,
        bytemuck::bytes_of(&uniform),
    );

    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("simple-image-viewer-hdr-jpeg-compose-tile-bind-group"),
        layout: &resources.jpeg_compose_bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(sdr_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(gain_view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: resources.jpeg_compose_uniform_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: wgpu::BindingResource::TextureView(display_storage_view),
            },
        ],
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("simple-image-viewer-hdr-jpeg-compose-tile-encoder"),
    });
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("simple-image-viewer-hdr-jpeg-compose-tile-pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&resources.jpeg_compose_tile_pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups(
            tile_width.div_ceil(COMPOSE_WORKGROUP_SIZE),
            tile_height.div_ceil(COMPOSE_WORKGROUP_SIZE),
            1,
        );
    }
    encoder.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jpeg_compose_shader_matches_iso_gain_map_steps() {
        assert!(JPEG_GAIN_COMPOSE_SHADER.contains("fn recover_hdr_channel"));
        assert!(JPEG_GAIN_COMPOSE_SHADER.contains("fn sample_gain_map_rgb"));
        assert!(JPEG_GAIN_COMPOSE_SHADER.contains("fn cs_compose_jpeg_gain"));
        assert!(JPEG_GAIN_COMPOSE_SHADER.contains("fn cs_compose_jpeg_gain_tile"));
        assert!(JPEG_GAIN_COMPOSE_SHADER.contains("fn display_to_physical_pixel"));
        assert!(JPEG_GAIN_COMPOSE_SHADER.contains("fn srgb_to_linear"));
        assert!(!JPEG_GAIN_COMPOSE_SHADER.contains("srgb_u8_to_linear"));
        assert!(!JPEG_GAIN_COMPOSE_SHADER.contains("/ 255.0"));
    }

    #[test]
    fn compose_uniform_struct_size_matches_wgsl() {
        assert_eq!(std::mem::size_of::<JpegGainMapComposeUniform>(), 128);
    }
}
