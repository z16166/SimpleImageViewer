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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct JpegTiledUploadKey {
    pub(super) sdr_ptr: usize,
    pub(super) gain_ptr: usize,
}

#[allow(dead_code)]
pub(crate) struct HdrImageBinding {
    pub(super) uploaded_texture: wgpu::Texture,
    pub(super) uploaded_view: wgpu::TextureView,
    pub(super) uploaded_gain_texture: Option<wgpu::Texture>,
    pub(super) uploaded_gain_view: Option<wgpu::TextureView>,
    pub(super) uploaded_sdr_texture: Option<wgpu::Texture>,
    pub(super) uploaded_sdr_view: Option<wgpu::TextureView>,
    pub(super) uploaded_display_storage_view: Option<wgpu::TextureView>,
    pub(super) uploaded_raw_pixels_texture: Option<wgpu::Texture>,
    pub(super) uploaded_raw_pixels_view: Option<wgpu::TextureView>,

    pub(super) baked_jpeg_image_key: Option<HdrImageKey>,
    pub(super) baked_jpeg_weight_bits: Option<u32>,
    pub(super) baked_apple_image_key: Option<HdrImageKey>,
    pub(super) baked_apple_weight_bits: Option<u32>,
    pub(super) baked_raw_demosaic_key: Option<HdrImageKey>,
    pub(super) baked_raw_demosaic_method: Option<crate::settings::RawDemosaicMethod>,

    pub(super) tone_map_buffer: wgpu::Buffer,
    pub(super) jpeg_compose_uniform_buffer: Option<wgpu::Buffer>,
    #[cfg(feature = "heif-native")]
    pub(super) compose_tone_map_buffer: Option<wgpu::Buffer>,
    #[cfg(feature = "heif-native")]
    pub(super) encoded_primary_buffer: Option<wgpu::Buffer>,
    #[cfg(feature = "heif-native")]
    pub(super) encoded_primary_buffer_bytes: usize,
    #[cfg(feature = "heif-native")]
    pub(super) encoded_primary_source_ptr: Option<usize>,

    pub(super) bind_group: Option<wgpu::BindGroup>,
    pub(super) last_use: std::time::Instant,
    pub(super) keep_resident: bool,
}

pub(crate) struct HdrCallbackResources {
    pub(super) target_format: wgpu::TextureFormat,
    pub(super) bind_group_layout: wgpu::BindGroupLayout,
    pub(super) pipeline: wgpu::RenderPipeline,
    #[allow(dead_code)]
    pub(super) dummy_gain_texture: wgpu::Texture,
    pub(super) dummy_gain_view: wgpu::TextureView,
    pub(super) tile_bindings: HdrTileBindings,
    pub(super) image_bindings: HashMap<HdrImageKey, HdrImageBinding>,
    pub(super) failed_jpeg_image_compose: HashSet<(HdrImageKey, u32)>,
    pub(super) failed_apple_image_compose: HashSet<(HdrImageKey, u32)>,
    pub(super) failed_raw_demosaic: HashSet<HdrImageKey>,
    pub(super) jpeg_compose_bind_group_layout: Option<wgpu::BindGroupLayout>,
    pub(super) jpeg_compose_pipeline: Option<wgpu::ComputePipeline>,
    pub(super) jpeg_compose_tile_pipeline: Option<wgpu::ComputePipeline>,
    pub(super) raw_demosaic_bind_group_layout: Option<wgpu::BindGroupLayout>,
    pub(super) raw_demosaic_pipeline: Option<wgpu::ComputePipeline>,
    pub(super) raw_demosaic_uniform_buffer: Option<wgpu::Buffer>,
    /// Single ISO gain-map compose uniform for tiled Ultra HDR via [`HdrTilePlaneCallback`].
    ///
    /// Static deferred JPEG via [`HdrImagePlaneCallback`] uses per-binding buffers
    /// (see `HdrImageBinding::jpeg_compose_uniform_buffer`) to avoid data races in concurrent drawing.
    pub(super) jpeg_compose_uniform_buffer: Option<wgpu::Buffer>,
    pub(super) jpeg_tiled_upload_key: Option<JpegTiledUploadKey>,
    pub(super) jpeg_tiled_sdr_texture: Option<wgpu::Texture>,
    pub(super) jpeg_tiled_sdr_view: Option<wgpu::TextureView>,
    pub(super) jpeg_tiled_gain_texture: Option<wgpu::Texture>,
    pub(super) jpeg_tiled_gain_view: Option<wgpu::TextureView>,
    #[cfg(feature = "heif-native")]
    pub(super) compose_bind_group_layout: Option<wgpu::BindGroupLayout>,
    #[cfg(feature = "heif-native")]
    pub(super) compose_pipeline: Option<wgpu::ComputePipeline>,
}

const HDR_COMPOSE_WORKGROUP_SIZE: u32 = 16;
const HDR_COMPOSE_MIN_STORAGE_TEXTURES: u32 = 1;
#[cfg(feature = "heif-native")]
const HDR_COMPOSE_MIN_STORAGE_BUFFERS: u32 = 1;

pub(super) fn iso_gain_map_compose_compute_supported(limits: &wgpu::Limits) -> bool {
    limits.max_compute_invocations_per_workgroup
        >= HDR_COMPOSE_WORKGROUP_SIZE * HDR_COMPOSE_WORKGROUP_SIZE
        && limits.max_compute_workgroup_size_x >= HDR_COMPOSE_WORKGROUP_SIZE
        && limits.max_compute_workgroup_size_y >= HDR_COMPOSE_WORKGROUP_SIZE
        && limits.max_storage_textures_per_shader_stage >= HDR_COMPOSE_MIN_STORAGE_TEXTURES
}

#[cfg(feature = "heif-native")]
pub(super) fn apple_compose_compute_supported(limits: &wgpu::Limits) -> bool {
    iso_gain_map_compose_compute_supported(limits)
        && limits.max_storage_buffers_per_shader_stage >= HDR_COMPOSE_MIN_STORAGE_BUFFERS
}

pub(crate) struct CallbackUpload {
    pub(super) texture: wgpu::Texture,
    pub(super) view: wgpu::TextureView,
    pub(super) storage_view: Option<wgpu::TextureView>,
}

pub(crate) struct ImagePlaneUpload {
    pub(super) base: CallbackUpload,
    pub(super) gain: Option<CallbackUpload>,
    pub(super) sdr_baseline: Option<CallbackUpload>,
    pub(super) raw_pixels: Option<CallbackUpload>,
}

pub(crate) const HDR_APPLE_GAIN_TEXTURE_FORMAT: wgpu::TextureFormat =
    wgpu::TextureFormat::Rgba8Unorm;

pub(super) fn create_dummy_gain_texture(
    device: &wgpu::Device,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("simple-image-viewer-hdr-dummy-gain-texture"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: HDR_APPLE_GAIN_TEXTURE_FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

pub(crate) fn create_callback_resources(
    device: &wgpu::Device,
    target_format: wgpu::TextureFormat,
) -> HdrCallbackResources {
    let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("simple-image-viewer-hdr-image-plane-bind-group-layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    multisampled: false,
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("simple-image-viewer-hdr-image-plane-pipeline-layout"),
        bind_group_layouts: &[Some(&bind_group_layout)],
        immediate_size: 0,
    });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("simple-image-viewer-hdr-image-plane-shader"),
        source: wgpu::ShaderSource::Wgsl(HDR_IMAGE_PLANE_SHADER.into()),
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("simple-image-viewer-hdr-image-plane-pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            polygon_mode: wgpu::PolygonMode::Fill,
            unclipped_depth: false,
            conservative: false,
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: target_format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    });
    let (dummy_gain_texture, dummy_gain_view) = create_dummy_gain_texture(device);
    let adapter_info = device.adapter_info();
    let gl_backend = adapter_info.backend == wgpu::Backend::Gl;

    #[cfg(feature = "heif-native")]
    let (compose_bind_group_layout, compose_pipeline) = if gl_backend {
        log::warn!(
            "[HDR] GPU Apple HEIC gain-map compose disabled on OpenGL backend; using CPU fallback"
        );
        (None, None)
    } else if apple_compose_compute_supported(&device.limits()) {
        let (layout, pipeline, _compose_tone_map_buffer) =
            apple_compose_gpu::create_compose_compute_resources(device);
        (Some(layout), Some(pipeline))
    } else {
        log::warn!(
            "[HDR] GPU Apple HEIC gain-map compose unavailable \
                 (max_compute_invocations_per_workgroup={}, \
                 max_storage_buffers_per_shader_stage={}); using CPU fallback",
            device.limits().max_compute_invocations_per_workgroup,
            device.limits().max_storage_buffers_per_shader_stage
        );
        (None, None)
    };
    let jpeg_compose = if gl_backend {
        log::warn!("[HDR] GPU ISO gain-map compose disabled on OpenGL backend; using CPU fallback");
        None
    } else if iso_gain_map_compose_compute_supported(&device.limits()) {
        Some(jpeg_compose_gpu::create_jpeg_compose_compute_resources(
            device,
        ))
    } else {
        log::warn!(
            "[HDR] GPU ISO gain-map compose unavailable \
             (max_compute_invocations_per_workgroup={}); using CPU fallback",
            device.limits().max_compute_invocations_per_workgroup
        );
        None
    };
    let (
        jpeg_compose_bind_group_layout,
        jpeg_compose_pipeline,
        jpeg_compose_tile_pipeline,
        jpeg_compose_uniform_buffer,
    ) = match jpeg_compose {
        Some((
            jpeg_compose_bind_group_layout,
            jpeg_compose_pipeline,
            jpeg_compose_tile_pipeline,
            jpeg_compose_uniform_buffer,
        )) => (
            Some(jpeg_compose_bind_group_layout),
            Some(jpeg_compose_pipeline),
            Some(jpeg_compose_tile_pipeline),
            Some(jpeg_compose_uniform_buffer),
        ),
        None => (None, None, None, None),
    };

    let raw_demosaic_compute_supported = device.limits().max_compute_invocations_per_workgroup
        >= 256
        && device.limits().max_compute_workgroup_size_x >= 16
        && device.limits().max_compute_workgroup_size_y >= 16;

    let (raw_demosaic_bind_group_layout, raw_demosaic_pipeline, raw_demosaic_uniform_buffer) =
        if gl_backend {
            log::warn!("[HDR] GPU RAW demosaicing disabled on OpenGL backend; using CPU fallback");
            (None, None, None)
        } else if raw_demosaic_compute_supported {
            let (layout, pipeline, buf) =
                crate::hdr::raw_demosaic_gpu::create_raw_demosaic_compute_resources(device);
            (Some(layout), Some(pipeline), Some(buf))
        } else {
            log::warn!(
                "[HDR] GPU RAW demosaicing unavailable \
             (max_compute_invocations_per_workgroup={}); using CPU fallback",
                device.limits().max_compute_invocations_per_workgroup
            );
            (None, None, None)
        };

    HdrCallbackResources {
        target_format,
        bind_group_layout,
        pipeline,
        dummy_gain_texture,
        dummy_gain_view,
        tile_bindings: HdrTileBindings::default(),
        image_bindings: HashMap::new(),
        failed_jpeg_image_compose: HashSet::new(),
        failed_apple_image_compose: HashSet::new(),
        failed_raw_demosaic: HashSet::new(),
        jpeg_compose_bind_group_layout,
        jpeg_compose_pipeline,
        jpeg_compose_tile_pipeline,
        raw_demosaic_bind_group_layout,
        raw_demosaic_pipeline,
        raw_demosaic_uniform_buffer,
        jpeg_compose_uniform_buffer,
        jpeg_tiled_upload_key: None,
        jpeg_tiled_sdr_texture: None,
        jpeg_tiled_sdr_view: None,
        jpeg_tiled_gain_texture: None,
        jpeg_tiled_gain_view: None,
        #[cfg(feature = "heif-native")]
        compose_bind_group_layout,
        #[cfg(feature = "heif-native")]
        compose_pipeline,
    }
}
