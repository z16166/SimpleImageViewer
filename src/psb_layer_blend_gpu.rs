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

//! Offscreen wgpu compute path for PSD/PSB Normal (straight-alpha src-over) layer blend.
//!
//! Output is RGBA8 for the existing SDR display path. PackBits / ICC stay on CPU.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use wgpu::util::DeviceExt;

const WORKGROUP: u32 = 16;
const READBACK_MAX_WAIT: Duration = Duration::from_secs(30);
const READBACK_POLL_SLICE: Duration = Duration::from_millis(2);

/// Skip GPU when the canvas is small enough that upload/sync dominate.
pub(crate) const GPU_BLEND_MIN_SHORT_SIDE: u32 = 512;
pub(crate) const GPU_BLEND_MIN_PIXELS: u64 = 512 * 512;

pub(crate) const PSD_NORMAL_BLEND_SHADER: &str = r#"
struct BlendParams {
    canvas_w: u32,
    canvas_h: u32,
    layer_w: u32,
    layer_h: u32,
    layer_left: i32,
    layer_top: i32,
    _pad0: u32,
    _pad1: u32,
};

@group(0) @binding(0) var canvas: texture_storage_2d<rgba8unorm, read_write>;
@group(0) @binding(1) var layer_tex: texture_2d<f32>;
@group(0) @binding(2) var<uniform> params: BlendParams;

// One compute pass per layer today. Batching multiple layers into one dispatch
// would cut GPU command overhead for 100+ layer docs, but must preserve
// bottom-to-top order (each layer reads the previous composite).

@compute @workgroup_size(16, 16, 1)
fn cs_blend_normal(@builtin(global_invocation_id) gid: vec3<u32>) {
    let sx = i32(gid.x);
    let sy = i32(gid.y);
    if (sx >= i32(params.layer_w) || sy >= i32(params.layer_h)) {
        return;
    }
    let dx = params.layer_left + sx;
    let dy = params.layer_top + sy;
    if (dx < 0 || dy < 0 || dx >= i32(params.canvas_w) || dy >= i32(params.canvas_h)) {
        return;
    }

    let src = textureLoad(layer_tex, vec2<i32>(sx, sy), 0);
    let sa = src.a;
    if (sa <= 0.0) {
        return;
    }

    let dst_coord = vec2<i32>(dx, dy);
    // rgba8unorm loads are in [0,1]; sa >= 1.0 means fully opaque, so write
    // alpha = 1.0 explicitly (same as src.a after unorm decode).
    if (sa >= 1.0) {
        textureStore(canvas, dst_coord, vec4<f32>(src.rgb, 1.0));
        return;
    }

    let dst = textureLoad(canvas, dst_coord);
    let da = dst.a;
    let out_a = sa + da * (1.0 - sa);
    if (out_a <= 0.0) {
        textureStore(canvas, dst_coord, vec4<f32>(0.0));
        return;
    }
    let out_rgb = (src.rgb * sa + dst.rgb * da * (1.0 - sa)) / out_a;
    textureStore(canvas, dst_coord, vec4<f32>(out_rgb, out_a));
}
"#;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BlendParamsUniform {
    canvas_w: u32,
    canvas_h: u32,
    layer_w: u32,
    layer_h: u32,
    layer_left: i32,
    layer_top: i32,
    _pad0: u32,
    _pad1: u32,
}

const _: () = assert!(std::mem::size_of::<BlendParamsUniform>() == 32);

/// GPU handles available to PSD composite workers (cloned from the image loader).
#[derive(Clone)]
pub struct PsdGpuContext {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub pipeline_cache: Option<Arc<wgpu::PipelineCache>>,
    pub device_id: u64,
    pub device_id_live: Arc<AtomicU64>,
    pub is_opengl: bool,
}

impl PsdGpuContext {
    pub fn is_device_current(&self) -> bool {
        self.device_id == self.device_id_live.load(Ordering::Acquire)
    }
}

pub(crate) struct DecodedLayerRef<'a> {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    pub rgba: &'a [u8],
}

struct PsdBlendPipeline {
    device_id: u64,
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

static PIPELINE_CACHE: Mutex<Option<Arc<PsdBlendPipeline>>> = Mutex::new(None);

fn tight_rgba8_bytes_per_row(width: u32) -> Result<u32, String> {
    width
        .checked_mul(4)
        .ok_or_else(|| format!("row byte count overflows for width {width}"))
}

fn padded_copy_bytes_per_row(unpadded_bytes_per_row: u32) -> u32 {
    wgpu::util::align_to(unpadded_bytes_per_row, wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
}

pub(crate) fn gpu_blend_worthwhile(width: u32, height: u32) -> bool {
    if width == 0 || height == 0 {
        return false;
    }
    let short = width.min(height);
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    short >= GPU_BLEND_MIN_SHORT_SIDE && pixels >= GPU_BLEND_MIN_PIXELS
}

pub(crate) fn psd_normal_blend_compute_supported(device: &wgpu::Device, is_opengl: bool) -> bool {
    if is_opengl {
        return false;
    }
    let limits = device.limits();
    if limits.max_compute_invocations_per_workgroup < WORKGROUP * WORKGROUP {
        return false;
    }
    let features = wgpu::TextureFormat::Rgba8Unorm.guaranteed_format_features(device.features());
    features
        .flags
        .contains(wgpu::TextureFormatFeatureFlags::STORAGE_READ_WRITE)
}

fn get_or_create_pipeline(
    device: &wgpu::Device,
    device_id: u64,
    pipeline_cache: Option<&wgpu::PipelineCache>,
) -> Result<Arc<PsdBlendPipeline>, String> {
    {
        let guard = PIPELINE_CACHE.lock();
        if let Some(existing) = guard.as_ref().filter(|p| p.device_id == device_id) {
            return Ok(Arc::clone(existing));
        }
    }
    let created = Arc::new(create_pipeline(device, device_id, pipeline_cache)?);
    let mut guard = PIPELINE_CACHE.lock();
    *guard = Some(Arc::clone(&created));
    Ok(created)
}

/// Prewarm the PSD Normal blend compute pipeline (also populates on-disk pipeline cache).
pub fn prewarm_psd_normal_blend_pipeline(
    device: &wgpu::Device,
    device_id: u64,
    is_opengl: bool,
    pipeline_cache: Option<&wgpu::PipelineCache>,
) {
    if !psd_normal_blend_compute_supported(device, is_opengl) {
        return;
    }
    if let Err(e) = get_or_create_pipeline(device, device_id, pipeline_cache) {
        log::warn!("[PSD] GPU Normal blend pipeline prewarm failed: {e}");
    }
}

fn create_pipeline(
    device: &wgpu::Device,
    device_id: u64,
    pipeline_cache: Option<&wgpu::PipelineCache>,
) -> Result<PsdBlendPipeline, String> {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("simple-image-viewer-psd-normal-blend-shader"),
        source: wgpu::ShaderSource::Wgsl(PSD_NORMAL_BLEND_SHADER.into()),
    });
    let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("simple-image-viewer-psd-normal-blend-bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::StorageTexture {
                    access: wgpu::StorageTextureAccess::ReadWrite,
                    format: wgpu::TextureFormat::Rgba8Unorm,
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
        ],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("simple-image-viewer-psd-normal-blend-pll"),
        bind_group_layouts: &[Some(&bind_group_layout)],
        immediate_size: 0,
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("simple-image-viewer-psd-normal-blend-pipeline"),
        layout: Some(&pipeline_layout),
        module: &shader,
        entry_point: Some("cs_blend_normal"),
        compilation_options: wgpu::PipelineCompilationOptions::default(),
        cache: pipeline_cache,
    });
    Ok(PsdBlendPipeline {
        device_id,
        pipeline,
        bind_group_layout,
    })
}

/// Try GPU Normal blend. Returns `None` to signal CPU fallback (unsupported, error, cancel).
pub(crate) fn try_blend_layers_gpu(
    ctx: &PsdGpuContext,
    canvas_w: u32,
    canvas_h: u32,
    initial_canvas: &[u8],
    layers: &[DecodedLayerRef<'_>],
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Option<Vec<u8>> {
    if !ctx.is_device_current()
        || !psd_normal_blend_compute_supported(&ctx.device, ctx.is_opengl)
        || !gpu_blend_worthwhile(canvas_w, canvas_h)
    {
        return None;
    }
    let expected = (canvas_w as usize)
        .checked_mul(canvas_h as usize)?
        .checked_mul(4)?;
    if initial_canvas.len() != expected {
        return None;
    }
    if crate::psb_reader::check_decode_cancel(cancel).is_err() {
        return None;
    }

    let pipe =
        match get_or_create_pipeline(&ctx.device, ctx.device_id, ctx.pipeline_cache.as_deref()) {
            Ok(p) => p,
            Err(e) => {
                log::debug!("[PSD] GPU Normal blend pipeline unavailable: {e}");
                return None;
            }
        };

    match blend_layers_gpu_inner(
        ctx,
        &pipe,
        canvas_w,
        canvas_h,
        initial_canvas,
        layers,
        cancel,
    ) {
        Ok(pixels) => Some(pixels),
        Err(e) => {
            log::debug!("[PSD] GPU Normal blend fell back to CPU: {e}");
            None
        }
    }
}

fn blend_layers_gpu_inner(
    ctx: &PsdGpuContext,
    pipe: &PsdBlendPipeline,
    canvas_w: u32,
    canvas_h: u32,
    initial_canvas: &[u8],
    layers: &[DecodedLayerRef<'_>],
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<Vec<u8>, crate::loader::DecodeError> {
    let device = &ctx.device;
    let queue = &ctx.queue;

    let canvas_texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("psd-normal-blend-canvas"),
        size: wgpu::Extent3d {
            width: canvas_w,
            height: canvas_h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &canvas_texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        initial_canvas,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(tight_rgba8_bytes_per_row(canvas_w)?),
            rows_per_image: Some(canvas_h),
        },
        wgpu::Extent3d {
            width: canvas_w,
            height: canvas_h,
            depth_or_array_layers: 1,
        },
    );
    let canvas_view = canvas_texture.create_view(&wgpu::TextureViewDescriptor::default());

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("psd-normal-blend-encoder"),
    });

    // Keep GPU resources alive until submit.
    let mut layer_textures: Vec<wgpu::Texture> = Vec::with_capacity(layers.len());
    let mut uniform_buffers: Vec<wgpu::Buffer> = Vec::with_capacity(layers.len());
    let mut bind_groups: Vec<wgpu::BindGroup> = Vec::with_capacity(layers.len());

    for layer in layers {
        crate::psb_reader::check_decode_cancel(cancel)?;
        if layer.width == 0 || layer.height == 0 {
            continue;
        }
        let need = (layer.width as usize)
            .checked_mul(layer.height as usize)
            .and_then(|n| n.checked_mul(4))
            .ok_or_else(|| "layer size overflow".to_string())?;
        if layer.rgba.len() != need {
            return Err("layer rgba length mismatch".into());
        }

        let layer_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("psd-normal-blend-layer"),
            size: wgpu::Extent3d {
                width: layer.width,
                height: layer.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &layer_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            layer.rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(tight_rgba8_bytes_per_row(layer.width)?),
                rows_per_image: Some(layer.height),
            },
            wgpu::Extent3d {
                width: layer.width,
                height: layer.height,
                depth_or_array_layers: 1,
            },
        );
        let layer_view = layer_tex.create_view(&wgpu::TextureViewDescriptor::default());

        let params = BlendParamsUniform {
            canvas_w,
            canvas_h,
            layer_w: layer.width,
            layer_h: layer.height,
            layer_left: layer.left,
            layer_top: layer.top,
            _pad0: 0,
            _pad1: 0,
        };
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("psd-normal-blend-params"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("psd-normal-blend-bg"),
            layout: &pipe.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&canvas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&layer_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniform.as_entire_binding(),
                },
            ],
        });

        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("psd-normal-blend-pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipe.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(
                layer.width.div_ceil(WORKGROUP),
                layer.height.div_ceil(WORKGROUP),
                1,
            );
        }

        layer_textures.push(layer_tex);
        uniform_buffers.push(uniform);
        bind_groups.push(bind_group);
    }

    let unpadded_bpr = tight_rgba8_bytes_per_row(canvas_w)?;
    let padded_bpr = padded_copy_bytes_per_row(unpadded_bpr);
    let readback_size = u64::from(padded_bpr)
        .checked_mul(u64::from(canvas_h))
        .ok_or_else(|| "readback size overflow".to_string())?;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("psd-normal-blend-readback"),
        size: readback_size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        canvas_texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bpr),
                rows_per_image: None,
            },
        },
        wgpu::Extent3d {
            width: canvas_w,
            height: canvas_h,
            depth_or_array_layers: 1,
        },
    );

    if !ctx.is_device_current() {
        return Err("wgpu device replaced during PSD blend".into());
    }
    queue.submit(Some(encoder.finish()));
    drop(bind_groups);
    drop(layer_textures);
    drop(uniform_buffers);

    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    readback
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
    wait_for_readback(device, &rx)?;

    let mapped = readback.slice(..).get_mapped_range();
    let mut pixels = Vec::with_capacity(initial_canvas.len());
    for row in mapped.chunks(padded_bpr as usize) {
        pixels.extend_from_slice(&row[..unpadded_bpr as usize]);
    }
    drop(mapped);
    readback.unmap();
    Ok(pixels)
}

fn wait_for_readback(
    device: &wgpu::Device,
    rx: &std::sync::mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>,
) -> Result<(), String> {
    let deadline = Instant::now() + READBACK_MAX_WAIT;
    loop {
        match rx.try_recv() {
            Ok(result) => {
                return result.map_err(|err| format!("PSD blend readback map failed: {err}"));
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                return Err("PSD blend readback channel closed".to_string());
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
        if Instant::now() >= deadline {
            return Err("PSD blend readback timed out".to_string());
        }
        // Wait already polls the device; a prior PollType::Poll was redundant.
        match device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(READBACK_POLL_SLICE),
        }) {
            Ok(_) => {}
            Err(wgpu::PollError::Timeout) => {}
            Err(err) => {
                return Err(format!("PSD blend device poll failed: {err:?}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_blend_worthwhile_rejects_small_canvas() {
        assert!(!gpu_blend_worthwhile(64, 64));
        assert!(!gpu_blend_worthwhile(511, 2000));
        assert!(!gpu_blend_worthwhile(2000, 200));
        assert!(gpu_blend_worthwhile(512, 512));
        assert!(gpu_blend_worthwhile(2048, 1024));
    }
}
