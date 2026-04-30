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

use super::types::{HdrImageBuffer, HdrPixelFormat, HdrToneMapSettings};
use eframe::{
    egui,
    egui_wgpu::{self, CallbackResources, CallbackTrait},
};
use std::sync::Arc;
use wgpu::util::DeviceExt;

pub const HDR_IMAGE_PLANE_TEXTURE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba32Float;

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HdrRenderOutputMode {
    SdrToneMapped = 0,
    NativeHdr = 1,
}

impl HdrRenderOutputMode {
    pub fn for_target_format(target_format: wgpu::TextureFormat) -> Self {
        if crate::hdr::surface::is_native_hdr_surface_format(Some(target_format)) {
            Self::NativeHdr
        } else {
            Self::SdrToneMapped
        }
    }
}

#[allow(dead_code)]
pub const HDR_IMAGE_PLANE_SHADER: &str = r#"
// Largest finite half-float value; caps extreme HDR values before tone mapping.
const MAX_FINITE_HDR_VALUE: f32 = 65504.0;
// Current SDR fallback approximates standard display gamma encoding.
const INVERSE_DISPLAY_GAMMA: f32 = 1.0 / 2.2;
// Keeps generated UVs inside the texture for the fullscreen triangle edge.
const MAX_UV_CLAMP: f32 = 0.999999;
const OUTPUT_MODE_NATIVE_HDR: u32 = 1u;

struct ToneMapSettings {
    exposure_ev: f32,
    sdr_white_nits: f32,
    max_display_nits: f32,
    rotation_steps: u32,
    alpha: f32,
    output_mode: u32,
    _pad0: u32,
    _pad1: u32,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@group(0) @binding(0) var hdr_texture: texture_2d<f32>;
@group(0) @binding(1) var<uniform> tone_map: ToneMapSettings;

fn reinhard_tone_map(rgb: vec3<f32>) -> vec3<f32> {
    return rgb / (vec3<f32>(1.0) + rgb);
}

fn sanitize_hdr_rgb(rgb: vec3<f32>) -> vec3<f32> {
    let positive = select(vec3<f32>(0.0), rgb, rgb > vec3<f32>(0.0));
    return min(positive, vec3<f32>(MAX_FINITE_HDR_VALUE));
}

fn rotate_uv_for_display(uv: vec2<f32>, rotation_steps: u32) -> vec2<f32> {
    switch rotation_steps % 4u {
        case 1u: {
            return vec2<f32>(uv.y, 1.0 - uv.x);
        }
        case 2u: {
            return vec2<f32>(1.0 - uv.x, 1.0 - uv.y);
        }
        case 3u: {
            return vec2<f32>(1.0 - uv.y, uv.x);
        }
        default: {
            return uv;
        }
    }
}

fn encode_sdr(rgb: vec3<f32>, settings: ToneMapSettings) -> vec3<f32> {
    let exposure_scale = exp2(settings.exposure_ev);
    let display_scale = settings.sdr_white_nits / max(settings.max_display_nits, settings.sdr_white_nits);
    let exposed = sanitize_hdr_rgb(rgb * exposure_scale * display_scale);
    let mapped = reinhard_tone_map(exposed);
    return pow(clamp(mapped, vec3<f32>(0.0), vec3<f32>(1.0)), vec3<f32>(INVERSE_DISPLAY_GAMMA));
}

fn encode_native_hdr(rgb: vec3<f32>, settings: ToneMapSettings) -> vec3<f32> {
    let exposure_scale = exp2(settings.exposure_ev);
    return sanitize_hdr_rgb(rgb * exposure_scale);
}

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    let positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    let uvs = array<vec2<f32>, 3>(
        vec2<f32>(0.0, 1.0),
        vec2<f32>(2.0, 1.0),
        vec2<f32>(0.0, -1.0),
    );

    var output: VertexOutput;
    output.position = vec4<f32>(positions[vertex_index], 0.0, 1.0);
    output.uv = uvs[vertex_index];
    return output;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let texture_size = vec2<f32>(textureDimensions(hdr_texture));
    let rotated_uv = rotate_uv_for_display(input.uv, tone_map.rotation_steps);
    let clamped_uv = clamp(rotated_uv, vec2<f32>(0.0), vec2<f32>(MAX_UV_CLAMP));
    let texel = vec2<i32>(clamped_uv * texture_size);
    let hdr = textureLoad(hdr_texture, texel, 0);
    let rgb = if tone_map.output_mode == OUTPUT_MODE_NATIVE_HDR {
        encode_native_hdr(hdr.rgb, tone_map)
    } else {
        encode_sdr(hdr.rgb, tone_map)
    };
    return vec4<f32>(
        rgb,
        clamp(hdr.a, 0.0, 1.0) * tone_map.alpha,
    );
}
"#;

#[allow(dead_code)]
pub struct UploadedHdrImage {
    pub size: wgpu::Extent3d,
    pub format: wgpu::TextureFormat,
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub sampler: wgpu::Sampler,
}

pub struct HdrImageRenderer {
    pub tone_map: HdrToneMapSettings,
    uploaded_image: Option<UploadedHdrImage>,
}

impl HdrImageRenderer {
    pub fn new() -> Self {
        Self {
            tone_map: HdrToneMapSettings::default(),
            uploaded_image: None,
        }
    }

    #[allow(dead_code)]
    pub fn uploaded_image(&self) -> Option<&UploadedHdrImage> {
        self.uploaded_image.as_ref()
    }

    #[allow(dead_code)]
    pub fn clear_uploaded_image(&mut self) {
        self.uploaded_image = None;
    }

    #[allow(dead_code)]
    pub fn upload_image(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        image: &HdrImageBuffer,
    ) -> Result<(), String> {
        let layout = validate_upload_layout(image, device.limits().max_texture_dimension_2d)?;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("simple-image-viewer-hdr-image-plane"),
            size: layout.size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: layout.format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let bytes = rgba32f_as_bytes(image.rgba_f32.as_slice());

        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(layout.bytes_per_row),
                rows_per_image: Some(layout.size.height),
            },
            layout.size,
        );

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("simple-image-viewer-hdr-image-plane-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        self.uploaded_image = Some(UploadedHdrImage {
            size: layout.size,
            format: layout.format,
            texture,
            view,
            sampler,
        });

        Ok(())
    }
}

pub fn hdr_image_plane_callback(
    rect: egui::Rect,
    image: Arc<HdrImageBuffer>,
    tone_map: HdrToneMapSettings,
    target_format: wgpu::TextureFormat,
    rotation_steps: u32,
    alpha: f32,
) -> egui::Shape {
    egui::Shape::Callback(egui_wgpu::Callback::new_paint_callback(
        rect,
        HdrImagePlaneCallback {
            image,
            tone_map,
            target_format,
            rotation_steps: rotation_steps % 4,
            alpha,
        },
    ))
}

#[allow(dead_code)]
pub fn hdr_tile_plane_callback(
    rect: egui::Rect,
    tile: Arc<crate::hdr::tiled::HdrTileBuffer>,
    tone_map: HdrToneMapSettings,
    target_format: wgpu::TextureFormat,
    alpha: f32,
) -> egui::Shape {
    egui::Shape::Callback(egui_wgpu::Callback::new_paint_callback(
        rect,
        HdrTilePlaneCallback {
            tile,
            tone_map,
            target_format,
            alpha,
        },
    ))
}

struct HdrImagePlaneCallback {
    image: Arc<HdrImageBuffer>,
    tone_map: HdrToneMapSettings,
    target_format: wgpu::TextureFormat,
    rotation_steps: u32,
    alpha: f32,
}

#[allow(dead_code)]
struct HdrTilePlaneCallback {
    tile: Arc<crate::hdr::tiled::HdrTileBuffer>,
    tone_map: HdrToneMapSettings,
    target_format: wgpu::TextureFormat,
    alpha: f32,
}

impl CallbackTrait for HdrImagePlaneCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        callback_resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let needs_resources = callback_resources
            .get::<HdrCallbackResources>()
            .map_or(true, |resources| {
                resources.target_format != self.target_format
            });
        if needs_resources {
            callback_resources.insert(create_callback_resources(device, self.target_format));
        }

        let Some(resources) = callback_resources.get_mut::<HdrCallbackResources>() else {
            return Vec::new();
        };

        let uniform = ToneMapUniform::from_settings(
            self.tone_map,
            self.rotation_steps,
            self.alpha,
            HdrRenderOutputMode::for_target_format(self.target_format),
        );
        queue.write_buffer(&resources.tone_map_buffer, 0, bytemuck::bytes_of(&uniform));

        let image_key = HdrImageKey::from_image(&self.image);
        if resources.uploaded_image_key != Some(image_key) {
            match upload_callback_image(device, queue, &self.image, &resources.bind_group_layout) {
                Ok(uploaded) => {
                    resources.uploaded_image_key = Some(image_key);
                    resources.uploaded_tile_key = None;
                    resources.uploaded_texture = Some(uploaded.texture);
                    resources.uploaded_view = Some(uploaded.view);
                    resources.bind_group = Some(device.create_bind_group(
                        &wgpu::BindGroupDescriptor {
                            label: Some("simple-image-viewer-hdr-image-plane-bind-group"),
                            layout: &resources.bind_group_layout,
                            entries: &[
                                wgpu::BindGroupEntry {
                                    binding: 0,
                                    resource: wgpu::BindingResource::TextureView(
                                        resources.uploaded_view.as_ref().unwrap(),
                                    ),
                                },
                                wgpu::BindGroupEntry {
                                    binding: 1,
                                    resource: resources.tone_map_buffer.as_entire_binding(),
                                },
                            ],
                        },
                    ));
                }
                Err(err) => {
                    log::warn!("[HDR] Skipping HDR image plane upload: {err}");
                    resources.uploaded_image_key = None;
                    resources.uploaded_texture = None;
                    resources.uploaded_view = None;
                    resources.bind_group = None;
                }
            }
        }

        Vec::new()
    }

    fn paint(
        &self,
        info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        callback_resources: &CallbackResources,
    ) {
        let Some(resources) = callback_resources.get::<HdrCallbackResources>() else {
            return;
        };
        let Some(bind_group) = resources.bind_group.as_ref() else {
            return;
        };

        // egui-wgpu already sets this viewport before invoking callbacks; repeat it
        // here so the fullscreen triangle is explicitly scoped to the image rect.
        let viewport = info.viewport_in_pixels();
        render_pass.set_viewport(
            viewport.left_px as f32,
            viewport.top_px as f32,
            viewport.width_px as f32,
            viewport.height_px as f32,
            0.0,
            1.0,
        );
        let scissor = info.clip_rect_in_pixels();
        render_pass.set_scissor_rect(
            scissor.left_px.max(0) as u32,
            scissor.top_px.max(0) as u32,
            scissor.width_px.max(0) as u32,
            scissor.height_px.max(0) as u32,
        );
        render_pass.set_pipeline(&resources.pipeline);
        render_pass.set_bind_group(0, bind_group, &[]);
        render_pass.draw(0..3, 0..1);
    }
}

impl CallbackTrait for HdrTilePlaneCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        callback_resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let needs_resources = callback_resources
            .get::<HdrCallbackResources>()
            .map_or(true, |resources| {
                resources.target_format != self.target_format
            });
        if needs_resources {
            callback_resources.insert(create_callback_resources(device, self.target_format));
        }

        let Some(resources) = callback_resources.get_mut::<HdrCallbackResources>() else {
            return Vec::new();
        };

        let uniform = ToneMapUniform::from_settings(
            self.tone_map,
            0,
            self.alpha,
            HdrRenderOutputMode::for_target_format(self.target_format),
        );
        queue.write_buffer(&resources.tone_map_buffer, 0, bytemuck::bytes_of(&uniform));

        let tile_key = HdrTileKey::from_tile(&self.tile);
        if resources.uploaded_tile_key != Some(tile_key) {
            match upload_callback_tile(device, queue, &self.tile) {
                Ok(uploaded) => {
                    resources.uploaded_image_key = None;
                    resources.uploaded_tile_key = Some(tile_key);
                    resources.uploaded_texture = Some(uploaded.texture);
                    resources.uploaded_view = Some(uploaded.view);
                    resources.bind_group = Some(device.create_bind_group(
                        &wgpu::BindGroupDescriptor {
                            label: Some("simple-image-viewer-hdr-tile-plane-bind-group"),
                            layout: &resources.bind_group_layout,
                            entries: &[
                                wgpu::BindGroupEntry {
                                    binding: 0,
                                    resource: wgpu::BindingResource::TextureView(
                                        resources.uploaded_view.as_ref().unwrap(),
                                    ),
                                },
                                wgpu::BindGroupEntry {
                                    binding: 1,
                                    resource: resources.tone_map_buffer.as_entire_binding(),
                                },
                            ],
                        },
                    ));
                }
                Err(err) => {
                    log::warn!("[HDR] Skipping HDR tile plane upload: {err}");
                    resources.uploaded_tile_key = None;
                    resources.uploaded_texture = None;
                    resources.uploaded_view = None;
                    resources.bind_group = None;
                }
            }
        }

        Vec::new()
    }

    fn paint(
        &self,
        info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        callback_resources: &CallbackResources,
    ) {
        let Some(resources) = callback_resources.get::<HdrCallbackResources>() else {
            return;
        };
        let Some(bind_group) = resources.bind_group.as_ref() else {
            return;
        };

        let viewport = info.viewport_in_pixels();
        render_pass.set_viewport(
            viewport.left_px as f32,
            viewport.top_px as f32,
            viewport.width_px as f32,
            viewport.height_px as f32,
            0.0,
            1.0,
        );
        let scissor = info.clip_rect_in_pixels();
        render_pass.set_scissor_rect(
            scissor.left_px.max(0) as u32,
            scissor.top_px.max(0) as u32,
            scissor.width_px.max(0) as u32,
            scissor.height_px.max(0) as u32,
        );
        render_pass.set_pipeline(&resources.pipeline);
        render_pass.set_bind_group(0, bind_group, &[]);
        render_pass.draw(0..3, 0..1);
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ToneMapUniform {
    exposure_ev: f32,
    sdr_white_nits: f32,
    max_display_nits: f32,
    rotation_steps: u32,
    alpha: f32,
    output_mode: u32,
    _pad0: u32,
    _pad1: u32,
}

unsafe impl bytemuck::Zeroable for ToneMapUniform {}
unsafe impl bytemuck::Pod for ToneMapUniform {}

impl ToneMapUniform {
    fn from_settings(
        settings: HdrToneMapSettings,
        rotation_steps: u32,
        alpha: f32,
        output_mode: HdrRenderOutputMode,
    ) -> Self {
        Self {
            exposure_ev: settings.exposure_ev,
            sdr_white_nits: settings.sdr_white_nits,
            max_display_nits: settings.max_display_nits,
            rotation_steps: rotation_steps % 4,
            alpha: alpha.clamp(0.0, 1.0),
            output_mode: output_mode as u32,
            _pad0: 0,
            _pad1: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HdrImageKey {
    width: u32,
    height: u32,
    format: HdrPixelFormat,
    rgba_ptr: usize,
    rgba_len: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HdrTileKey {
    width: u32,
    height: u32,
    rgba_ptr: usize,
    rgba_len: usize,
}

impl HdrTileKey {
    #[allow(dead_code)]
    fn from_tile(tile: &crate::hdr::tiled::HdrTileBuffer) -> Self {
        Self {
            width: tile.width,
            height: tile.height,
            rgba_ptr: Arc::as_ptr(&tile.rgba_f32) as usize,
            rgba_len: tile.rgba_f32.len(),
        }
    }
}

impl HdrImageKey {
    fn from_image(image: &HdrImageBuffer) -> Self {
        Self {
            width: image.width,
            height: image.height,
            format: image.format,
            rgba_ptr: Arc::as_ptr(&image.rgba_f32) as usize,
            rgba_len: image.rgba_f32.len(),
        }
    }
}

struct HdrCallbackResources {
    target_format: wgpu::TextureFormat,
    bind_group_layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
    tone_map_buffer: wgpu::Buffer,
    uploaded_image_key: Option<HdrImageKey>,
    uploaded_tile_key: Option<HdrTileKey>,
    uploaded_texture: Option<wgpu::Texture>,
    uploaded_view: Option<wgpu::TextureView>,
    bind_group: Option<wgpu::BindGroup>,
}

struct CallbackUpload {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

fn create_callback_resources(
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
    let tone_map_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("simple-image-viewer-hdr-image-plane-tone-map-buffer"),
        contents: bytemuck::bytes_of(&ToneMapUniform::from_settings(
            HdrToneMapSettings::default(),
            0,
            1.0,
            HdrRenderOutputMode::SdrToneMapped,
        )),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });

    HdrCallbackResources {
        target_format,
        bind_group_layout,
        pipeline,
        tone_map_buffer,
        uploaded_image_key: None,
        uploaded_tile_key: None,
        uploaded_texture: None,
        uploaded_view: None,
        bind_group: None,
    }
}

#[allow(dead_code)]
fn upload_callback_tile(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    tile: &crate::hdr::tiled::HdrTileBuffer,
) -> Result<CallbackUpload, String> {
    let layout = validate_tile_upload_layout(tile, device.limits().max_texture_dimension_2d)?;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("simple-image-viewer-hdr-tile-plane-callback-texture"),
        size: layout.size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: layout.format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });

    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        rgba32f_as_bytes(tile.rgba_f32.as_slice()),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(layout.bytes_per_row),
            rows_per_image: Some(layout.size.height),
        },
        layout.size,
    );

    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    Ok(CallbackUpload { texture, view })
}

fn upload_callback_image(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    image: &HdrImageBuffer,
    _bind_group_layout: &wgpu::BindGroupLayout,
) -> Result<CallbackUpload, String> {
    let layout = validate_upload_layout(image, device.limits().max_texture_dimension_2d)?;
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("simple-image-viewer-hdr-image-plane-callback-texture"),
        size: layout.size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: layout.format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });

    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        rgba32f_as_bytes(image.rgba_f32.as_slice()),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(layout.bytes_per_row),
            rows_per_image: Some(layout.size.height),
        },
        layout.size,
    );

    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    Ok(CallbackUpload { texture, view })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HdrUploadLayout {
    size: wgpu::Extent3d,
    bytes_per_row: u32,
    format: wgpu::TextureFormat,
}

fn validate_upload_layout(
    image: &HdrImageBuffer,
    max_texture_dimension_2d: u32,
) -> Result<HdrUploadLayout, String> {
    if image.format != HdrPixelFormat::Rgba32Float {
        return Err(format!(
            "HDR upload currently supports only Rgba32Float buffers, got {:?}",
            image.format
        ));
    }

    validate_rgba32f_upload_layout(
        image.width,
        image.height,
        image.rgba_f32.len(),
        max_texture_dimension_2d,
        "HDR upload",
    )
}

#[allow(dead_code)]
fn validate_tile_upload_layout(
    tile: &crate::hdr::tiled::HdrTileBuffer,
    max_texture_dimension_2d: u32,
) -> Result<HdrUploadLayout, String> {
    validate_rgba32f_upload_layout(
        tile.width,
        tile.height,
        tile.rgba_f32.len(),
        max_texture_dimension_2d,
        "HDR tile upload",
    )
}

fn validate_rgba32f_upload_layout(
    width: u32,
    height: u32,
    actual_len: usize,
    max_texture_dimension_2d: u32,
    label: &str,
) -> Result<HdrUploadLayout, String> {
    if width == 0 || height == 0 {
        return Err(format!(
            "{label} requires non-zero dimensions, got {width}x{height}"
        ));
    }

    if width > max_texture_dimension_2d || height > max_texture_dimension_2d {
        return Err(format!(
            "{label} dimensions {width}x{height} exceed device max_texture_dimension_2d {max_texture_dimension_2d}",
        ));
    }

    let expected_len = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .map(|len| len as usize)
        .ok_or_else(|| format!("{label} dimensions overflow: {width}x{height}"))?;

    if actual_len != expected_len {
        return Err(format!(
            "Malformed {label} buffer: expected {expected_len} floats for {width}x{height} RGBA, got {actual_len}",
        ));
    }

    let bytes_per_row = width
        .checked_mul(4)
        .and_then(|channels| channels.checked_mul(std::mem::size_of::<f32>() as u32))
        .ok_or_else(|| format!("{label} row byte count overflows for width {width}"))?;

    Ok(HdrUploadLayout {
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        bytes_per_row,
        format: HDR_IMAGE_PLANE_TEXTURE_FORMAT,
    })
}

fn rgba32f_as_bytes(values: &[f32]) -> &[u8] {
    bytemuck::cast_slice(values)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hdr::tiled::HdrTileBuffer;
    use crate::hdr::types::{HdrColorSpace, HdrImageBuffer, HdrPixelFormat};
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
    fn upload_layout_matches_rgba32f_rows() {
        let image = hdr_image(3, 2, HdrPixelFormat::Rgba32Float, vec![0.0; 3 * 2 * 4]);

        let layout = validate_upload_layout(&image, 4096).expect("valid upload layout");

        assert_eq!(layout.size.width, 3);
        assert_eq!(layout.size.height, 2);
        assert_eq!(
            layout.bytes_per_row,
            3 * 4 * std::mem::size_of::<f32>() as u32
        );
        assert_eq!(layout.format, wgpu::TextureFormat::Rgba32Float);
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
            7 * 4 * std::mem::size_of::<f32>() as u32
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
            1.0,
        );

        assert!(matches!(shape, egui::Shape::Callback(_)));
    }

    #[test]
    fn shader_sanitizes_non_finite_hdr_rgb_before_tone_mapping() {
        assert!(HDR_IMAGE_PLANE_SHADER.contains("fn sanitize_hdr_rgb"));
        assert!(HDR_IMAGE_PLANE_SHADER.contains("rgb > vec3<f32>(0.0)"));
        assert!(HDR_IMAGE_PLANE_SHADER.contains("const MAX_FINITE_HDR_VALUE: f32"));
        assert!(HDR_IMAGE_PLANE_SHADER.contains("min(positive, vec3<f32>(MAX_FINITE_HDR_VALUE))"));
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
        );

        assert_eq!(uniform.rotation_steps, 1);
        assert_eq!(uniform.alpha, 0.25);
    }

    #[test]
    fn render_mode_uses_native_hdr_for_float_targets_only() {
        assert_eq!(
            HdrRenderOutputMode::for_target_format(wgpu::TextureFormat::Rgba16Float),
            HdrRenderOutputMode::NativeHdr
        );
        assert_eq!(
            HdrRenderOutputMode::for_target_format(wgpu::TextureFormat::Rgba32Float),
            HdrRenderOutputMode::NativeHdr
        );
        assert_eq!(
            HdrRenderOutputMode::for_target_format(wgpu::TextureFormat::Bgra8Unorm),
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
        );

        assert_eq!(uniform.output_mode, HdrRenderOutputMode::NativeHdr as u32);
    }

    #[test]
    fn shader_outputs_straight_alpha_for_standard_blending() {
        assert!(HDR_IMAGE_PLANE_SHADER.contains("fn encode_native_hdr"));
        assert!(HDR_IMAGE_PLANE_SHADER.contains("if tone_map.output_mode == OUTPUT_MODE_NATIVE_HDR"));
        assert!(HDR_IMAGE_PLANE_SHADER.contains("clamp(hdr.a, 0.0, 1.0) * tone_map.alpha"));
        assert!(!HDR_IMAGE_PLANE_SHADER.contains("encode_sdr(hdr.rgb, tone_map) * tone_map.alpha"));
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
            rgba_f32: Arc::new(rgba_f32),
        }
    }

    fn hdr_tile(width: u32, height: u32, rgba_f32: Vec<f32>) -> HdrTileBuffer {
        HdrTileBuffer {
            width,
            height,
            color_space: HdrColorSpace::LinearSrgb,
            rgba_f32: Arc::new(rgba_f32),
        }
    }
}
