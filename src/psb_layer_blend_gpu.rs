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

//! Offscreen wgpu compute path for PSD/PSB separable layer blend and clipping groups.
//!
//! Handles Normal, Screen, Linear Dodge, and Multiply for the existing SDR RGBA8
//! display path. PackBits / ICC stay on CPU.
//!
//! # Current GPU shader limitations (as of separable + clip path)
//!
//! 1. **Blend modes:** only the four separable keys (`norm` / `scrn` / `lddg` /
//!    `mul `). Overlay, Soft Light, and other non-separable modes are not in
//!    the shader (unknown keys still map to Normal, matching CPU).
//! 2. **User mask vs clipping:** user/real mask is folded into layer alpha on
//!    CPU before upload (not a separate shader pass; acceptable). Clipping
//!    groups *are* on GPU (`cs_capture_base_alpha` /
//!    `cs_apply_base_alpha_mask` + CPU-side group orchestration mirroring
//!    `OpenClipGroup`). Vector masks / knockout / clip-to-folder remain out
//!    of scope.
//! 3. **Admission fallback:** if any decoded layer is not GPU-separable, the
//!    whole stack falls back to CPU `blend_layers_with_clipping` (all-or-
//!    nothing per document). Same full-CPU fallback on device/OpenGL/size
//!    gate, OOM, cancel, or readback failure.
//!

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use wgpu::util::DeviceExt;

const WORKGROUP: u32 = 16;
const READBACK_MAX_WAIT: Duration = Duration::from_secs(30);

#[cfg(test)]
static COMPUTE_PASS_BEGINS: AtomicU64 = AtomicU64::new(0);

fn begin_psd_compute_pass<'a>(
    encoder: &'a mut wgpu::CommandEncoder,
    label: &'static str,
) -> wgpu::ComputePass<'a> {
    #[cfg(test)]
    COMPUTE_PASS_BEGINS.fetch_add(1, Ordering::Relaxed);
    encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
        label: Some(label),
        timestamp_writes: None,
    })
}

/// Skip GPU when the canvas is small enough that upload/sync dominate.
pub(crate) const GPU_BLEND_MIN_SHORT_SIDE: u32 = 512;
pub(crate) const GPU_BLEND_MIN_PIXELS: u64 = 512 * 512;

/// WGSL `mode` uniform values (must match shader).
pub(crate) const BLEND_MODE_NORMAL: u32 = 0;
pub(crate) const BLEND_MODE_SCREEN: u32 = 1;
pub(crate) const BLEND_MODE_LINEAR_DODGE: u32 = 2;
pub(crate) const BLEND_MODE_MULTIPLY: u32 = 3;

pub(crate) fn separable_blend_mode_u32(blend: &[u8; 4]) -> u32 {
    match blend {
        b"scrn" => BLEND_MODE_SCREEN,
        b"lddg" => BLEND_MODE_LINEAR_DODGE,
        b"mul " => BLEND_MODE_MULTIPLY,
        // Unknown keys already treated as Normal on CPU.
        _ => BLEND_MODE_NORMAL,
    }
}

pub(crate) fn is_gpu_separable_blend(blend: &[u8; 4]) -> bool {
    matches!(blend, b"norm" | b"scrn" | b"lddg" | b"mul ")
}

pub(crate) const PSD_SEPARABLE_BLEND_SHADER: &str = r#"
struct BlendParams {
    canvas_w: u32,
    canvas_h: u32,
    layer_w: u32,
    layer_h: u32,
    layer_left: i32,
    layer_top: i32,
    mode: u32,
    _pad0: u32,
};

@group(0) @binding(0) var target: texture_storage_2d<rgba8unorm, read_write>;
@group(0) @binding(1) var layer_tex: texture_2d<f32>;
@group(0) @binding(2) var<uniform> params: BlendParams;

// One compute pass per layer today. Batching multiple layers into one dispatch
// would cut GPU command overhead for 100+ layer docs, but must preserve
// bottom-to-top order (each layer reads the previous composite).

fn blend_b(mode: u32, cb: f32, cs: f32) -> f32 {
    if (mode == 1u) {
        return cb + cs - cb * cs;
    }
    if (mode == 2u) {
        return min(cb + cs, 1.0);
    }
    if (mode == 3u) {
        return cb * cs;
    }
    return cs;
}

fn blend_rgb(mode: u32, cb: vec3<f32>, cs: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        blend_b(mode, cb.r, cs.r),
        blend_b(mode, cb.g, cs.g),
        blend_b(mode, cb.b, cs.b),
    );
}

@compute @workgroup_size(16, 16, 1)
fn cs_blend_separable(@builtin(global_invocation_id) gid: vec3<u32>) {
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
    // rgba8unorm loads are in [0,1]; only Normal can skip destination reads.
    if (params.mode == 0u && sa >= 1.0) {
        textureStore(target, dst_coord, vec4<f32>(src.rgb, 1.0));
        return;
    }

    let dst = textureLoad(target, dst_coord);
    let da = dst.a;
    let out_a = sa + da * (1.0 - sa);
    let blended = blend_rgb(params.mode, dst.rgb, src.rgb);
    let co = sa * (1.0 - da) * src.rgb + sa * da * blended + da * (1.0 - sa) * dst.rgb;
    let out_rgb = co / out_a;
    textureStore(target, dst_coord, vec4<f32>(out_rgb, out_a));
}

@compute @workgroup_size(16, 16, 1)
fn cs_capture_base_alpha(@builtin(global_invocation_id) gid: vec3<u32>) {
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

    let base = textureLoad(layer_tex, vec2<i32>(sx, sy), 0);
    textureStore(target, vec2<i32>(dx, dy), vec4<f32>(base.a, 0.0, 0.0, 0.0));
}

@compute @workgroup_size(16, 16, 1)
fn cs_apply_base_alpha_mask(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.canvas_w || gid.y >= params.canvas_h) {
        return;
    }

    let coord = vec2<i32>(i32(gid.x), i32(gid.y));
    let mask = textureLoad(layer_tex, coord, 0).r;
    if (mask <= 0.0) {
        textureStore(target, coord, vec4<f32>(0.0));
        return;
    }

    if (mask >= 1.0) {
        return;
    }

    let group = textureLoad(target, coord);
    // Match CPU's u8 alpha math before deciding whether RGB survives.
    let a_u = u32(round(group.a * 255.0));
    let m_u = u32(round(mask * 255.0));
    let out_a_u = (a_u * m_u) / 255u;
    if (out_a_u == 0u) {
        textureStore(target, coord, vec4<f32>(0.0));
        return;
    }
    textureStore(target, coord, vec4<f32>(group.rgb, f32(out_a_u) / 255.0));
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
    mode: u32,
    _pad0: u32,
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
    pub blend: [u8; 4],
    pub clipping: u8,
    pub rgba: &'a [u8],
}

struct PsdBlendPipeline {
    device_id: u64,
    blend_pipeline: wgpu::ComputePipeline,
    capture_base_alpha_pipeline: wgpu::ComputePipeline,
    apply_base_alpha_mask_pipeline: wgpu::ComputePipeline,
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

pub(crate) fn psd_separable_blend_compute_supported(
    device: &wgpu::Device,
    is_opengl: bool,
) -> bool {
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

#[allow(dead_code)]
pub(crate) fn psd_normal_blend_compute_supported(device: &wgpu::Device, is_opengl: bool) -> bool {
    psd_separable_blend_compute_supported(device, is_opengl)
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

/// Prewarm the PSD separable blend compute pipeline (also populates on-disk pipeline cache).
pub fn prewarm_psd_separable_blend_pipeline(
    device: &wgpu::Device,
    device_id: u64,
    is_opengl: bool,
    pipeline_cache: Option<&wgpu::PipelineCache>,
) {
    if !psd_separable_blend_compute_supported(device, is_opengl) {
        return;
    }
    if let Err(e) = get_or_create_pipeline(device, device_id, pipeline_cache) {
        log::warn!("[PSD] GPU separable blend pipeline prewarm failed: {e}");
    }
}

/// Backward-compatible alias for callers still prewarming only Normal PSD blend.
pub fn prewarm_psd_normal_blend_pipeline(
    device: &wgpu::Device,
    device_id: u64,
    is_opengl: bool,
    pipeline_cache: Option<&wgpu::PipelineCache>,
) {
    prewarm_psd_separable_blend_pipeline(device, device_id, is_opengl, pipeline_cache);
}

fn create_pipeline(
    device: &wgpu::Device,
    device_id: u64,
    pipeline_cache: Option<&wgpu::PipelineCache>,
) -> Result<PsdBlendPipeline, String> {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("simple-image-viewer-psd-separable-blend-shader"),
        source: wgpu::ShaderSource::Wgsl(PSD_SEPARABLE_BLEND_SHADER.into()),
    });
    let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("simple-image-viewer-psd-separable-blend-bgl"),
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
        label: Some("simple-image-viewer-psd-separable-blend-pll"),
        bind_group_layouts: &[Some(&bind_group_layout)],
        immediate_size: 0,
    });
    let blend_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("simple-image-viewer-psd-separable-blend-pipeline"),
        layout: Some(&pipeline_layout),
        module: &shader,
        entry_point: Some("cs_blend_separable"),
        compilation_options: wgpu::PipelineCompilationOptions::default(),
        cache: pipeline_cache,
    });
    let capture_base_alpha_pipeline =
        device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("simple-image-viewer-psd-capture-base-alpha-pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("cs_capture_base_alpha"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: pipeline_cache,
        });
    let apply_base_alpha_mask_pipeline =
        device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("simple-image-viewer-psd-apply-base-alpha-mask-pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("cs_apply_base_alpha_mask"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: pipeline_cache,
        });
    Ok(PsdBlendPipeline {
        device_id,
        blend_pipeline,
        capture_base_alpha_pipeline,
        apply_base_alpha_mask_pipeline,
        bind_group_layout,
    })
}

/// Try GPU separable blend. Returns `None` to signal CPU fallback (unsupported, error, cancel).
pub(crate) fn try_blend_layers_gpu(
    ctx: &PsdGpuContext,
    canvas_w: u32,
    canvas_h: u32,
    initial_canvas: &[u8],
    layers: &[DecodedLayerRef<'_>],
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Option<Vec<u8>> {
    if !ctx.is_device_current()
        || !psd_separable_blend_compute_supported(&ctx.device, ctx.is_opengl)
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
                log::debug!("[PSD] GPU separable blend pipeline unavailable: {e}");
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
            log::debug!("[PSD] GPU separable blend fell back to CPU: {e}");
            None
        }
    }
}

struct GpuBlendResources {
    textures: Vec<wgpu::Texture>,
    uniform_buffers: Vec<wgpu::Buffer>,
    bind_groups: Vec<wgpu::BindGroup>,
}

impl GpuBlendResources {
    fn with_capacity(layer_count: usize) -> Self {
        Self {
            textures: Vec::with_capacity(layer_count.saturating_mul(2)),
            uniform_buffers: Vec::with_capacity(layer_count.saturating_mul(2)),
            bind_groups: Vec::with_capacity(layer_count.saturating_mul(2)),
        }
    }
}

struct MaterializedClipGroup {
    group_texture: wgpu::Texture,
    group_view: wgpu::TextureView,
    base_alpha_texture: wgpu::Texture,
    base_alpha_view: wgpu::TextureView,
}

fn validate_gpu_layer(layer: &DecodedLayerRef<'_>) -> Result<(), crate::loader::DecodeError> {
    if !is_gpu_separable_blend(&layer.blend) {
        return Err("layer is not eligible for GPU separable blend".into());
    }
    let need = (layer.width as usize)
        .checked_mul(layer.height as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| "layer size overflow".to_string())?;
    if layer.rgba.len() != need {
        return Err("layer rgba length mismatch".into());
    }
    Ok(())
}

fn create_uploaded_layer_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layer: &DecodedLayerRef<'_>,
) -> Result<(wgpu::Texture, wgpu::TextureView), crate::loader::DecodeError> {
    let layer_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("psd-separable-blend-layer"),
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
    Ok((layer_tex, layer_view))
}

fn create_zeroed_full_canvas_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    label: &'static str,
    canvas_w: u32,
    canvas_h: u32,
    zero_canvas: &[u8],
) -> Result<(wgpu::Texture, wgpu::TextureView), crate::loader::DecodeError> {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
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
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        zero_canvas,
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
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    Ok((texture, view))
}

#[allow(clippy::too_many_arguments)]
fn encode_blend_texture(
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    pipe: &PsdBlendPipeline,
    resources: &mut GpuBlendResources,
    target_view: &wgpu::TextureView,
    source_view: &wgpu::TextureView,
    canvas_w: u32,
    canvas_h: u32,
    layer_w: u32,
    layer_h: u32,
    layer_left: i32,
    layer_top: i32,
    mode: u32,
    pass_label: &'static str,
) -> Result<(), crate::loader::DecodeError> {
    if layer_w == 0 || layer_h == 0 {
        return Ok(());
    }
    let params = BlendParamsUniform {
        canvas_w,
        canvas_h,
        layer_w,
        layer_h,
        layer_left,
        layer_top,
        mode,
        _pad0: 0,
    };
    let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("psd-separable-blend-params"),
        contents: bytemuck::bytes_of(&params),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("psd-separable-blend-bg"),
        layout: &pipe.bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(target_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(source_view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: uniform.as_entire_binding(),
            },
        ],
    });
    {
        let mut pass = begin_psd_compute_pass(encoder, pass_label);
        pass.set_pipeline(&pipe.blend_pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups(layer_w.div_ceil(WORKGROUP), layer_h.div_ceil(WORKGROUP), 1);
    }
    resources.uniform_buffers.push(uniform);
    resources.bind_groups.push(bind_group);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn encode_blend_decoded_layer(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    encoder: &mut wgpu::CommandEncoder,
    pipe: &PsdBlendPipeline,
    resources: &mut GpuBlendResources,
    target_view: &wgpu::TextureView,
    canvas_w: u32,
    canvas_h: u32,
    layer: &DecodedLayerRef<'_>,
    mode: u32,
    pass_label: &'static str,
) -> Result<(), crate::loader::DecodeError> {
    validate_gpu_layer(layer)?;
    if layer.width == 0 || layer.height == 0 {
        return Ok(());
    }
    let (layer_tex, layer_view) = create_uploaded_layer_texture(device, queue, layer)?;
    encode_blend_texture(
        device,
        encoder,
        pipe,
        resources,
        target_view,
        &layer_view,
        canvas_w,
        canvas_h,
        layer.width,
        layer.height,
        layer.left,
        layer.top,
        mode,
        pass_label,
    )?;
    resources.textures.push(layer_tex);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn encode_capture_base_alpha(
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    pipe: &PsdBlendPipeline,
    resources: &mut GpuBlendResources,
    base_alpha_view: &wgpu::TextureView,
    base_view: &wgpu::TextureView,
    canvas_w: u32,
    canvas_h: u32,
    base: &DecodedLayerRef<'_>,
) -> Result<(), crate::loader::DecodeError> {
    if base.width == 0 || base.height == 0 {
        return Ok(());
    }
    let params = BlendParamsUniform {
        canvas_w,
        canvas_h,
        layer_w: base.width,
        layer_h: base.height,
        layer_left: base.left,
        layer_top: base.top,
        mode: BLEND_MODE_NORMAL,
        _pad0: 0,
    };
    let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("psd-capture-base-alpha-params"),
        contents: bytemuck::bytes_of(&params),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("psd-capture-base-alpha-bg"),
        layout: &pipe.bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(base_alpha_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(base_view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: uniform.as_entire_binding(),
            },
        ],
    });
    {
        let mut pass = begin_psd_compute_pass(encoder, "psd-capture-base-alpha-pass");
        pass.set_pipeline(&pipe.capture_base_alpha_pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups(
            base.width.div_ceil(WORKGROUP),
            base.height.div_ceil(WORKGROUP),
            1,
        );
    }
    resources.uniform_buffers.push(uniform);
    resources.bind_groups.push(bind_group);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn encode_apply_base_alpha_mask(
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    pipe: &PsdBlendPipeline,
    resources: &mut GpuBlendResources,
    group_view: &wgpu::TextureView,
    base_alpha_view: &wgpu::TextureView,
    canvas_w: u32,
    canvas_h: u32,
) {
    let params = BlendParamsUniform {
        canvas_w,
        canvas_h,
        layer_w: canvas_w,
        layer_h: canvas_h,
        layer_left: 0,
        layer_top: 0,
        mode: BLEND_MODE_NORMAL,
        _pad0: 0,
    };
    let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("psd-apply-base-alpha-mask-params"),
        contents: bytemuck::bytes_of(&params),
        usage: wgpu::BufferUsages::UNIFORM,
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("psd-apply-base-alpha-mask-bg"),
        layout: &pipe.bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(group_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(base_alpha_view),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: uniform.as_entire_binding(),
            },
        ],
    });
    {
        let mut pass = begin_psd_compute_pass(encoder, "psd-apply-base-alpha-mask-pass");
        pass.set_pipeline(&pipe.apply_base_alpha_mask_pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups(
            canvas_w.div_ceil(WORKGROUP),
            canvas_h.div_ceil(WORKGROUP),
            1,
        );
    }
    resources.uniform_buffers.push(uniform);
    resources.bind_groups.push(bind_group);
}

#[allow(clippy::too_many_arguments)]
fn materialize_clip_group(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    encoder: &mut wgpu::CommandEncoder,
    pipe: &PsdBlendPipeline,
    resources: &mut GpuBlendResources,
    canvas_w: u32,
    canvas_h: u32,
    base: &DecodedLayerRef<'_>,
    zero_canvas: &[u8],
) -> Result<MaterializedClipGroup, crate::loader::DecodeError> {
    validate_gpu_layer(base)?;
    let (group_texture, group_view) = create_zeroed_full_canvas_texture(
        device,
        queue,
        "psd-clip-group-texture",
        canvas_w,
        canvas_h,
        zero_canvas,
    )?;
    let (base_alpha_texture, base_alpha_view) = create_zeroed_full_canvas_texture(
        device,
        queue,
        "psd-clip-base-alpha-texture",
        canvas_w,
        canvas_h,
        zero_canvas,
    )?;
    if base.width != 0 && base.height != 0 {
        let (base_texture, base_view) = create_uploaded_layer_texture(device, queue, base)?;
        encode_blend_texture(
            device,
            encoder,
            pipe,
            resources,
            &group_view,
            &base_view,
            canvas_w,
            canvas_h,
            base.width,
            base.height,
            base.left,
            base.top,
            BLEND_MODE_NORMAL,
            "psd-clip-base-normal-pass",
        )?;
        encode_capture_base_alpha(
            device,
            encoder,
            pipe,
            resources,
            &base_alpha_view,
            &base_view,
            canvas_w,
            canvas_h,
            base,
        )?;
        resources.textures.push(base_texture);
    }
    Ok(MaterializedClipGroup {
        group_texture,
        group_view,
        base_alpha_texture,
        base_alpha_view,
    })
}

#[allow(clippy::too_many_arguments)]
fn flush_open_clip_group(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    encoder: &mut wgpu::CommandEncoder,
    pipe: &PsdBlendPipeline,
    resources: &mut GpuBlendResources,
    main_canvas_view: &wgpu::TextureView,
    canvas_w: u32,
    canvas_h: u32,
    open_base: Option<&DecodedLayerRef<'_>>,
    materialized_group: Option<MaterializedClipGroup>,
) -> Result<(), crate::loader::DecodeError> {
    let Some(base) = open_base else {
        return Ok(());
    };
    if let Some(group) = materialized_group {
        validate_gpu_layer(base)?;
        encode_apply_base_alpha_mask(
            device,
            encoder,
            pipe,
            resources,
            &group.group_view,
            &group.base_alpha_view,
            canvas_w,
            canvas_h,
        );
        encode_blend_texture(
            device,
            encoder,
            pipe,
            resources,
            main_canvas_view,
            &group.group_view,
            canvas_w,
            canvas_h,
            canvas_w,
            canvas_h,
            0,
            0,
            separable_blend_mode_u32(&base.blend),
            "psd-clip-group-flush-pass",
        )?;
        resources.textures.push(group.group_texture);
        resources.textures.push(group.base_alpha_texture);
        return Ok(());
    }
    encode_blend_decoded_layer(
        device,
        queue,
        encoder,
        pipe,
        resources,
        main_canvas_view,
        canvas_w,
        canvas_h,
        base,
        separable_blend_mode_u32(&base.blend),
        "psd-lone-base-flush-pass",
    )
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
        label: Some("psd-separable-blend-canvas"),
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
        label: Some("psd-separable-blend-encoder"),
    });

    let mut resources = GpuBlendResources::with_capacity(layers.len());
    let mut zero_canvas: Option<Vec<u8>> = None;
    let mut open_base: Option<&DecodedLayerRef<'_>> = None;
    let mut materialized_group: Option<MaterializedClipGroup> = None;

    for layer in layers {
        crate::psb_reader::check_decode_cancel(cancel)?;
        if layer.clipping != 0 {
            let Some(base) = open_base else {
                continue;
            };
            if materialized_group.is_none() {
                let zeros = zero_canvas.get_or_insert_with(|| vec![0u8; initial_canvas.len()]);
                materialized_group = Some(materialize_clip_group(
                    device,
                    queue,
                    &mut encoder,
                    pipe,
                    &mut resources,
                    canvas_w,
                    canvas_h,
                    base,
                    zeros,
                )?);
            }
            let group_view = &materialized_group
                .as_ref()
                .expect("clip group materialized before clip blend")
                .group_view;
            encode_blend_decoded_layer(
                device,
                queue,
                &mut encoder,
                pipe,
                &mut resources,
                group_view,
                canvas_w,
                canvas_h,
                layer,
                separable_blend_mode_u32(&layer.blend),
                "psd-clip-layer-blend-pass",
            )?;
            continue;
        }

        flush_open_clip_group(
            device,
            queue,
            &mut encoder,
            pipe,
            &mut resources,
            &canvas_view,
            canvas_w,
            canvas_h,
            open_base.take(),
            materialized_group.take(),
        )?;
        open_base = Some(layer);
    }

    flush_open_clip_group(
        device,
        queue,
        &mut encoder,
        pipe,
        &mut resources,
        &canvas_view,
        canvas_w,
        canvas_h,
        open_base.take(),
        materialized_group.take(),
    )?;

    let unpadded_bpr = tight_rgba8_bytes_per_row(canvas_w)?;
    let padded_bpr = padded_copy_bytes_per_row(unpadded_bpr);
    let readback_size = u64::from(padded_bpr)
        .checked_mul(u64::from(canvas_h))
        .ok_or_else(|| "readback size overflow".to_string())?;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("psd-separable-blend-readback"),
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
    drop(resources);

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
        let now = Instant::now();
        if now >= deadline {
            return Err("PSD blend readback timed out".to_string());
        }
        // Block until the device signals progress or the overall deadline.
        // map_async completion is delivered on `rx`; Wait avoids a fixed
        // short poll slice (checklist #37).
        match device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(deadline.saturating_duration_since(now)),
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
    use crate::psb_layer_clip::{ClipLayerRef, blend_layers_with_clipping};

    const ACCURACY_CANVAS_W: u32 = 512;
    const ACCURACY_CANVAS_H: u32 = 512;
    const ACCURACY_LAYER_W: u32 = 32;
    const ACCURACY_LAYER_H: u32 = 32;
    const ACCURACY_LAYER_LEFT: i32 = 10;
    const ACCURACY_LAYER_TOP: i32 = 10;
    const CLIPPING_ACCURACY_LAYER_SIZE: u32 = 256;
    const CLIPPING_ACCURACY_OFFSET: i32 = 128;
    const CLIPPING_ACCURACY_OVERLAY_LEFT: i32 = 256;

    #[test]
    fn gpu_blend_worthwhile_rejects_small_canvas() {
        assert!(!gpu_blend_worthwhile(64, 64));
        assert!(!gpu_blend_worthwhile(511, 2000));
        assert!(!gpu_blend_worthwhile(2000, 200));
        assert!(gpu_blend_worthwhile(512, 512));
        assert!(gpu_blend_worthwhile(2048, 1024));
    }

    #[test]
    fn separable_shader_uses_mode_uniform_entry() {
        assert!(PSD_SEPARABLE_BLEND_SHADER.contains("mode: u32"));
        assert!(PSD_SEPARABLE_BLEND_SHADER.contains("fn cs_blend_separable"));
        assert!(PSD_SEPARABLE_BLEND_SHADER.contains("fn blend_b"));
        assert!(PSD_SEPARABLE_BLEND_SHADER.contains("sa * (1.0 - da)"));
    }

    #[test]
    fn clip_shader_declares_capture_and_mask_entries() {
        assert!(PSD_SEPARABLE_BLEND_SHADER.contains("fn cs_capture_base_alpha"));
        assert!(PSD_SEPARABLE_BLEND_SHADER.contains("fn cs_apply_base_alpha_mask"));
    }

    #[test]
    fn clip_shader_quantizes_masked_alpha_like_cpu() {
        assert!(PSD_SEPARABLE_BLEND_SHADER.contains("let a_u = u32(round(group.a * 255.0));"));
        assert!(PSD_SEPARABLE_BLEND_SHADER.contains("let m_u = u32(round(mask * 255.0));"));
        assert!(PSD_SEPARABLE_BLEND_SHADER.contains("let out_a_u = (a_u * m_u) / 255u;"));
        assert!(PSD_SEPARABLE_BLEND_SHADER.contains("if (out_a_u == 0u)"));
    }

    #[test]
    fn separable_mode_u32_mapping() {
        assert_eq!(separable_blend_mode_u32(b"norm"), BLEND_MODE_NORMAL);
        assert_eq!(separable_blend_mode_u32(b"scrn"), BLEND_MODE_SCREEN);
        assert_eq!(separable_blend_mode_u32(b"lddg"), BLEND_MODE_LINEAR_DODGE);
        assert_eq!(separable_blend_mode_u32(b"mul "), BLEND_MODE_MULTIPLY);
        assert_eq!(separable_blend_mode_u32(b"xxxx"), BLEND_MODE_NORMAL);
    }

    fn max_abs_diff(a: &[u8], b: &[u8]) -> u8 {
        assert_eq!(a.len(), b.len());
        a.iter()
            .zip(b)
            .map(|(&left, &right)| left.abs_diff(right))
            .max()
            .unwrap_or(0)
    }

    fn test_canvas() -> Vec<u8> {
        let mut canvas = Vec::with_capacity((ACCURACY_CANVAS_W * ACCURACY_CANVAS_H * 4) as usize);
        for y in 0..ACCURACY_CANVAS_H {
            for x in 0..ACCURACY_CANVAS_W {
                canvas.extend_from_slice(&[
                    ((x * 3 + y) & 0xff) as u8,
                    ((x + y * 5) & 0xff) as u8,
                    ((x * 7 + y * 11) & 0xff) as u8,
                    255,
                ]);
            }
        }
        canvas
    }

    fn test_layer_rgba() -> Vec<u8> {
        let mut layer = Vec::with_capacity((ACCURACY_LAYER_W * ACCURACY_LAYER_H * 4) as usize);
        for y in 0..ACCURACY_LAYER_H {
            for x in 0..ACCURACY_LAYER_W {
                layer.extend_from_slice(&[
                    (40 + ((x * 5) & 0x7f)) as u8,
                    (30 + ((y * 7) & 0x7f)) as u8,
                    (80 + (((x + y) * 3) & 0x7f)) as u8,
                    128,
                ]);
            }
        }
        layer
    }

    fn try_test_psd_gpu_context() -> Option<PsdGpuContext> {
        let (device, queue, is_opengl) = pollster::block_on(async {
            let instance = wgpu::Instance::default();
            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::LowPower,
                    force_fallback_adapter: false,
                    compatible_surface: None,
                })
                .await
                .ok()?;
            let is_opengl = adapter.get_info().backend == wgpu::Backend::Gl;
            let (device, queue) = adapter
                .request_device(&wgpu::DeviceDescriptor::default())
                .await
                .ok()?;
            Some((device, queue, is_opengl))
        })?;
        let device_id_live = Arc::new(AtomicU64::new(1));
        Some(PsdGpuContext {
            device,
            queue,
            pipeline_cache: None,
            device_id: 1,
            device_id_live,
            is_opengl,
        })
    }

    #[test]
    #[ignore]
    fn gpu_separable_modes_match_cpu_within_one() {
        let Some(ctx) = try_test_psd_gpu_context() else {
            eprintln!("Skipping GPU separable blend accuracy test: no wgpu device available");
            return;
        };

        let initial_canvas = test_canvas();
        let layer_rgba = test_layer_rgba();
        for blend in [*b"norm", *b"scrn", *b"lddg", *b"mul "] {
            let mut cpu_canvas = initial_canvas.clone();
            let cpu_layers = [ClipLayerRef {
                left: ACCURACY_LAYER_LEFT,
                top: ACCURACY_LAYER_TOP,
                width: ACCURACY_LAYER_W,
                height: ACCURACY_LAYER_H,
                blend,
                clipping: 0,
                rgba: &layer_rgba,
            }];
            blend_layers_with_clipping(
                &mut cpu_canvas,
                ACCURACY_CANVAS_W,
                ACCURACY_CANVAS_H,
                &cpu_layers,
                None,
            )
            .unwrap();

            let gpu_layers = [DecodedLayerRef {
                left: ACCURACY_LAYER_LEFT,
                top: ACCURACY_LAYER_TOP,
                width: ACCURACY_LAYER_W,
                height: ACCURACY_LAYER_H,
                blend,
                clipping: 0,
                rgba: &layer_rgba,
            }];
            let Some(gpu_canvas) = try_blend_layers_gpu(
                &ctx,
                ACCURACY_CANVAS_W,
                ACCURACY_CANVAS_H,
                &initial_canvas,
                &gpu_layers,
                None,
            ) else {
                eprintln!("Skipping GPU separable blend accuracy test: GPU path unavailable");
                return;
            };

            assert!(
                max_abs_diff(&cpu_canvas, &gpu_canvas) <= 1,
                "blend {:?} exceeded max abs diff 1",
                std::str::from_utf8(&blend).unwrap_or("????")
            );
        }
    }

    fn solid_layer_rgba(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
        let mut layer = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..width * height {
            layer.extend_from_slice(&rgba);
        }
        layer
    }

    #[test]
    #[ignore]
    fn gpu_screen_clipping_matches_cpu_within_one() {
        let Some(ctx) = try_test_psd_gpu_context() else {
            eprintln!("Skipping GPU clipping accuracy test: no wgpu device available");
            return;
        };

        let initial_canvas = vec![0u8; (ACCURACY_CANVAS_W * ACCURACY_CANVAS_H * 4) as usize];
        let base_rgba = solid_layer_rgba(ACCURACY_CANVAS_W, ACCURACY_CANVAS_H, [200, 0, 0, 255]);
        let clip_rgba = solid_layer_rgba(
            CLIPPING_ACCURACY_LAYER_SIZE,
            CLIPPING_ACCURACY_LAYER_SIZE,
            [0, 0, 255, 255],
        );
        let overlay_rgba = solid_layer_rgba(
            CLIPPING_ACCURACY_LAYER_SIZE,
            CLIPPING_ACCURACY_LAYER_SIZE,
            [0, 128, 0, 128],
        );

        let mut cpu_canvas = initial_canvas.clone();
        let cpu_layers = [
            ClipLayerRef {
                left: 0,
                top: 0,
                width: ACCURACY_CANVAS_W,
                height: ACCURACY_CANVAS_H,
                blend: *b"norm",
                clipping: 0,
                rgba: &base_rgba,
            },
            ClipLayerRef {
                left: CLIPPING_ACCURACY_OFFSET,
                top: CLIPPING_ACCURACY_OFFSET,
                width: CLIPPING_ACCURACY_LAYER_SIZE,
                height: CLIPPING_ACCURACY_LAYER_SIZE,
                blend: *b"scrn",
                clipping: 1,
                rgba: &clip_rgba,
            },
            ClipLayerRef {
                left: CLIPPING_ACCURACY_OVERLAY_LEFT,
                top: 0,
                width: CLIPPING_ACCURACY_LAYER_SIZE,
                height: CLIPPING_ACCURACY_LAYER_SIZE,
                blend: *b"norm",
                clipping: 0,
                rgba: &overlay_rgba,
            },
        ];
        blend_layers_with_clipping(
            &mut cpu_canvas,
            ACCURACY_CANVAS_W,
            ACCURACY_CANVAS_H,
            &cpu_layers,
            None,
        )
        .unwrap();

        let gpu_layers = [
            DecodedLayerRef {
                left: 0,
                top: 0,
                width: ACCURACY_CANVAS_W,
                height: ACCURACY_CANVAS_H,
                blend: *b"norm",
                clipping: 0,
                rgba: &base_rgba,
            },
            DecodedLayerRef {
                left: CLIPPING_ACCURACY_OFFSET,
                top: CLIPPING_ACCURACY_OFFSET,
                width: CLIPPING_ACCURACY_LAYER_SIZE,
                height: CLIPPING_ACCURACY_LAYER_SIZE,
                blend: *b"scrn",
                clipping: 1,
                rgba: &clip_rgba,
            },
            DecodedLayerRef {
                left: CLIPPING_ACCURACY_OVERLAY_LEFT,
                top: 0,
                width: CLIPPING_ACCURACY_LAYER_SIZE,
                height: CLIPPING_ACCURACY_LAYER_SIZE,
                blend: *b"norm",
                clipping: 0,
                rgba: &overlay_rgba,
            },
        ];
        let Some(gpu_canvas) = try_blend_layers_gpu(
            &ctx,
            ACCURACY_CANVAS_W,
            ACCURACY_CANVAS_H,
            &initial_canvas,
            &gpu_layers,
            None,
        ) else {
            eprintln!("Skipping GPU clipping accuracy test: GPU path unavailable");
            return;
        };

        assert!(
            max_abs_diff(&cpu_canvas, &gpu_canvas) <= 1,
            "Screen clipping exceeded max abs diff 1"
        );
    }

    #[test]
    #[ignore]
    fn gpu_batch_uses_single_compute_pass() {
        let Some(ctx) = try_test_psd_gpu_context() else {
            eprintln!("Skipping single-pass batch test: no wgpu device available");
            return;
        };

        let initial_canvas = vec![0u8; (ACCURACY_CANVAS_W * ACCURACY_CANVAS_H * 4) as usize];
        let base_rgba = solid_layer_rgba(ACCURACY_CANVAS_W, ACCURACY_CANVAS_H, [200, 0, 0, 255]);
        let clip_rgba = solid_layer_rgba(
            CLIPPING_ACCURACY_LAYER_SIZE,
            CLIPPING_ACCURACY_LAYER_SIZE,
            [0, 0, 255, 255],
        );
        let overlay_rgba = solid_layer_rgba(
            CLIPPING_ACCURACY_LAYER_SIZE,
            CLIPPING_ACCURACY_LAYER_SIZE,
            [0, 128, 0, 128],
        );
        let layer_rgba = test_layer_rgba();

        let gpu_layers = [
            DecodedLayerRef {
                left: 0,
                top: 0,
                width: ACCURACY_CANVAS_W,
                height: ACCURACY_CANVAS_H,
                blend: *b"norm",
                clipping: 0,
                rgba: &base_rgba,
            },
            DecodedLayerRef {
                left: CLIPPING_ACCURACY_OFFSET,
                top: CLIPPING_ACCURACY_OFFSET,
                width: CLIPPING_ACCURACY_LAYER_SIZE,
                height: CLIPPING_ACCURACY_LAYER_SIZE,
                blend: *b"scrn",
                clipping: 1,
                rgba: &clip_rgba,
            },
            DecodedLayerRef {
                left: CLIPPING_ACCURACY_OVERLAY_LEFT,
                top: 0,
                width: CLIPPING_ACCURACY_LAYER_SIZE,
                height: CLIPPING_ACCURACY_LAYER_SIZE,
                blend: *b"mul ",
                clipping: 0,
                rgba: &overlay_rgba,
            },
            DecodedLayerRef {
                left: ACCURACY_LAYER_LEFT,
                top: ACCURACY_LAYER_TOP,
                width: ACCURACY_LAYER_W,
                height: ACCURACY_LAYER_H,
                blend: *b"lddg",
                clipping: 0,
                rgba: &layer_rgba,
            },
        ];

        COMPUTE_PASS_BEGINS.store(0, Ordering::Relaxed);
        let Some(_gpu_canvas) = try_blend_layers_gpu(
            &ctx,
            ACCURACY_CANVAS_W,
            ACCURACY_CANVAS_H,
            &initial_canvas,
            &gpu_layers,
            None,
        ) else {
            eprintln!("Skipping single-pass batch test: GPU path unavailable");
            return;
        };

        let begins = COMPUTE_PASS_BEGINS.load(Ordering::Relaxed);
        assert_eq!(
            begins, 1,
            "expected one compute pass for the whole GPU batch, got {begins}"
        );
    }

    #[test]
    fn blend_params_uniform_keeps_32_byte_layout() {
        let params = BlendParamsUniform {
            canvas_w: 1,
            canvas_h: 2,
            layer_w: 3,
            layer_h: 4,
            layer_left: 5,
            layer_top: 6,
            mode: BLEND_MODE_SCREEN,
            _pad0: 0,
        };
        assert_eq!(std::mem::size_of_val(&params), 32);
        assert_eq!(params.mode, BLEND_MODE_SCREEN);
    }
}
