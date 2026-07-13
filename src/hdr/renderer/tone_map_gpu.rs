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

//! Offscreen GPU tone-map + readback for large SDR previews (>128px).
//!
//! Uses a dedicated compute pass with `textureLoad` (nearest/exact texels) so preview bytes
//! match the CPU scalar path. Strip-sized previews stay on the CPU SIMD path.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use super::HdrRenderOutputMode;
use super::tone_map_uniform::{
    ImageToneMapUniformParams, ToneMapCommonParams, ToneMapUniform, image_tone_map_uniform,
    libavif_tone_map_native_display_scale,
};

// WGSL `ToneMapSettings` is mirrored by `ToneMapUniform` (Pod + 128-byte layout assert).
const _: () = assert!(std::mem::size_of::<ToneMapUniform>() == 128);

const PREVIEW_READBACK_POLL_SLICE: Duration = Duration::from_millis(1);
const PREVIEW_READBACK_MAX_WAIT: Duration = Duration::from_secs(120);
use super::CallbackUpload;
use super::upload::{upload_callback_image, wgpu_copy_bytes_per_row, write_rgba32f_to_texture};
use crate::hdr::decode::{hdr_to_sdr_rgba8_strip_preview, hdr_to_sdr_rgba8_with_tone_settings};
use crate::hdr::types::{HdrImageBuffer, HdrToneMapSettings, HdrTransferFunction};
use eframe::egui;

const PREVIEW_GPU_OUTPUT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Directory-tree strips and smaller previews use CPU SIMD; larger SDR previews prefer GPU.
pub(crate) const PREVIEW_STRIP_MAX_SIDE: u32 = 128;

const HDR_PREVIEW_TONE_MAP_SHADER: &str = concat!(
    r#"
const MAX_FINITE_HDR_VALUE: f32 = 65504.0;
const INVERSE_DISPLAY_GAMMA: f32 = 1.0 / 2.2;
const INPUT_REFERENCE_SCENE_LINEAR: u32 = 0u;

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
    sdr_grade_clamp: u32,
    uv_min: vec2<f32>,
    uv_max: vec2<f32>,
    apple_compose: u32,
    headroom_span: f32,
    weight: f32,
    gain_width: u32,
    gain_height: u32,
    primary_width: u32,
    primary_height: u32,
    _apple_pad: u32,
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

fn reinhard_tone_map(rgb: vec3<f32>) -> vec3<f32> {
    return rgb / (vec3<f32>(1.0) + rgb);
}

fn sanitize_hdr_rgb(rgb: vec3<f32>) -> vec3<f32> {
    var safe = rgb;
    if (safe.r != safe.r) { safe.r = 0.0; }
    if (safe.g != safe.g) { safe.g = 0.0; }
    if (safe.b != safe.b) { safe.b = 0.0; }
    return clamp(
        safe,
        vec3<f32>(-MAX_FINITE_HDR_VALUE),
        vec3<f32>(MAX_FINITE_HDR_VALUE),
    );
}

fn linear_srgb_scalar_to_encoded_srgb(linear: f32) -> f32 {
    let c = clamp(linear, 0.0, 1.0);
    if c <= 0.0031308 {
        return c * 12.92;
    }
    return 1.055 * pow(c, 1.0 / 2.4) - 0.055;
}

fn sanitize_scalar_for_linear_srgb_encode(value: f32) -> f32 {
    if value != value {
        return 0.0;
    }
    if value <= 0.0 {
        return 0.0;
    }
    return min(value, MAX_FINITE_HDR_VALUE);
}

fn encode_sdr(rgb: vec3<f32>, settings: ToneMapSettings) -> vec3<f32> {
    let exposure_scale = exp2(settings.exposure_ev);
    let use_piecewise_srgb = settings.input_transfer_function == INPUT_TRANSFER_SRGB &&
        settings.input_reference != INPUT_REFERENCE_SCENE_LINEAR;
    let manual_oetf = settings.sdr_manual_srgb_encode != 0u;

    if use_piecewise_srgb {
        let lr = sanitize_scalar_for_linear_srgb_encode(rgb.r * exposure_scale);
        let lg = sanitize_scalar_for_linear_srgb_encode(rgb.g * exposure_scale);
        let lb = sanitize_scalar_for_linear_srgb_encode(rgb.b * exposure_scale);
        let linear_clamped = clamp(vec3<f32>(lr, lg, lb), vec3<f32>(0.0), vec3<f32>(1.0));
        if (!manual_oetf) {
            return linear_clamped;
        }
        return vec3<f32>(
            linear_srgb_scalar_to_encoded_srgb(linear_clamped.r),
            linear_srgb_scalar_to_encoded_srgb(linear_clamped.g),
            linear_srgb_scalar_to_encoded_srgb(linear_clamped.b),
        );
    }

    let display_scale = settings.sdr_white_nits / max(settings.max_display_nits, settings.sdr_white_nits);
    let exposed = sanitize_hdr_rgb(rgb * exposure_scale * display_scale);
    let mapped = reinhard_tone_map(exposed);
    let clamped = clamp(mapped, vec3<f32>(0.0), vec3<f32>(1.0));
    if (!manual_oetf) {
        return clamped;
    }
    return pow(clamped, vec3<f32>(INVERSE_DISPLAY_GAMMA));
}

fn tone_map_texel_to_sdr(hdr: vec4<f32>, settings: ToneMapSettings) -> vec4<f32> {
    let src_a = clamp(hdr.a, 0.0, 1.0);
    let display_referred_srgb = settings.input_transfer_function == INPUT_TRANSFER_SRGB &&
        settings.input_reference != INPUT_REFERENCE_SCENE_LINEAR;
    var src_rgb = hdr.rgb;
    if (settings.sdr_grade_clamp != 0u) {
        src_rgb = clamp(src_rgb, vec3<f32>(0.0), vec3<f32>(1.0));
        src_rgb = src_rgb * src_a;
    }
    let decoded_rgb = decode_input_transfer(src_rgb, settings.input_transfer_function, settings);
    var source_rgb = convert_input_to_linear_srgb(decoded_rgb, settings.input_color_space);
    if (src_a <= 0.0) {
        source_rgb = vec3<f32>(0.0);
    } else if (settings.sdr_grade_clamp != 0u) {
        source_rgb = source_rgb / src_a;
    }
    if (display_referred_srgb) {
        let exposure_scale = exp2(settings.exposure_ev);
        source_rgb = sanitize_hdr_rgb(source_rgb * exposure_scale);
        if (src_a <= 0.0) {
            source_rgb = vec3<f32>(0.0);
        }
    }
    let rgb = encode_sdr(source_rgb, settings);
    return vec4<f32>(rgb, src_a * settings.alpha);
}

@group(0) @binding(0) var hdr_texture: texture_2d<f32>;
@group(0) @binding(1) var<uniform> tone_map: ToneMapSettings;
@group(0) @binding(2) var preview_output: texture_storage_2d<rgba8unorm, write>;

@compute @workgroup_size(16, 16, 1)
fn cs_preview_tone_map(@builtin(global_invocation_id) gid: vec3<u32>) {
    let dims = textureDimensions(hdr_texture);
    if (gid.x >= dims.x || gid.y >= dims.y) {
        return;
    }
    let hdr = textureLoad(hdr_texture, vec2<i32>(i32(gid.x), i32(gid.y)), 0);
    let out = tone_map_texel_to_sdr(hdr, tone_map);
    textureStore(preview_output, vec2<i32>(i32(gid.x), i32(gid.y)), out);
}
"#
);

struct ToneMapPreviewGpuCache {
    bind_group_layout: wgpu::BindGroupLayout,
    pipeline: wgpu::ComputePipeline,
}

/// Per-device-epoch GPU pipeline cache; stale epochs are dropped when the device changes.
static PREVIEW_GPU_CACHE: OnceLock<Mutex<HashMap<u64, ToneMapPreviewGpuCache>>> = OnceLock::new();

thread_local! {
    static PREVIEW_GPU_CONTEXT: RefCell<Option<(wgpu::Device, wgpu::Queue, u64)>> =
        const { RefCell::new(None) };
    static PREVIEW_OUTPUT_POOL: RefCell<Option<PreviewOutputResources>> =
        const { RefCell::new(None) };
    static PREVIEW_HDR_TEXTURE_CACHE: RefCell<Option<PreviewHdrTextureCache>> =
        const { RefCell::new(None) };
}

struct PreviewHdrTextureCache {
    device_epoch: u64,
    width: u32,
    height: u32,
    /// Content fingerprint of the last uploaded `rgba_f32` plane (not a raw pointer).
    pixels_sample_hash: u64,
    /// Monotonic generation bumped on every texture alloc or pixel rewrite.
    /// Paired with `pixels_sample_hash` so bind-group reuse cannot false-hit after free/realloc.
    texture_generation: u64,
    upload: CallbackUpload,
}

/// Stable identity for the HDR input texture backing a cached preview bind group.
/// Prefer generation + content hash over `Arc::as_ptr` (address reuse after free/realloc).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PreviewBindGroupHdrKey {
    texture_generation: u64,
    pixels_sample_hash: u64,
    width: u32,
    height: u32,
}

struct PreviewOutputResources {
    device_epoch: u64,
    width: u32,
    height: u32,
    padded_bytes_per_row: u32,
    output_texture: wgpu::Texture,
    output_view: wgpu::TextureView,
    readback_buffer: wgpu::Buffer,
    tone_map_buffer: wgpu::Buffer,
    bind_group: Option<wgpu::BindGroup>,
    bind_group_hdr_key: Option<PreviewBindGroupHdrKey>,
    /// Skip `write_buffer` when tone-map params are unchanged across preview calls.
    last_tone_map_uniform: Option<ToneMapUniform>,
}

struct PreviewGpuScopeGuard {
    previous: Option<(wgpu::Device, wgpu::Queue, u64)>,
}

impl Drop for PreviewGpuScopeGuard {
    fn drop(&mut self) {
        PREVIEW_GPU_CONTEXT.with(|slot| {
            *slot.borrow_mut() = self.previous.take();
        });
    }
}

/// Install loader worker GPU handles for [`hdr_to_sdr_rgba8_for_preview`] on this thread.
pub(crate) fn with_preview_tone_map_gpu<R>(
    device: Option<wgpu::Device>,
    queue: Option<wgpu::Queue>,
    device_epoch: u64,
    f: impl FnOnce() -> R,
) -> R {
    let Some(device) = device else {
        return f();
    };
    let Some(queue) = queue else {
        return f();
    };

    PREVIEW_GPU_CONTEXT.with(|slot| {
        let previous = slot.borrow_mut().replace((device, queue, device_epoch));
        let _guard = PreviewGpuScopeGuard { previous };
        f()
    })
}

fn current_preview_gpu_context() -> Option<(wgpu::Device, wgpu::Queue, u64)> {
    PREVIEW_GPU_CONTEXT.with(|slot| slot.borrow().clone())
}

fn with_preview_output_resources<R>(
    device: &wgpu::Device,
    device_epoch: u64,
    width: u32,
    height: u32,
    padded_bytes_per_row: u32,
    readback_size: u64,
    f: impl FnOnce(&mut PreviewOutputResources) -> R,
) -> R {
    PREVIEW_OUTPUT_POOL.with(|slot| {
        let mut guard = slot.borrow_mut();
        let needs_alloc = match guard.as_ref() {
            None => true,
            Some(pool) => {
                pool.device_epoch != device_epoch
                    || pool.width != width
                    || pool.height != height
                    || pool.padded_bytes_per_row != padded_bytes_per_row
            }
        };
        if needs_alloc {
            let output_texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("simple-image-viewer-hdr-preview-tone-map-output"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: PREVIEW_GPU_OUTPUT_FORMAT,
                usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let output_view = output_texture.create_view(&wgpu::TextureViewDescriptor::default());
            let readback_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("simple-image-viewer-hdr-preview-tone-map-readback"),
                size: readback_size,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            let tone_map_buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("simple-image-viewer-hdr-preview-tone-map-uniform"),
                size: std::mem::size_of::<ToneMapUniform>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            *guard = Some(PreviewOutputResources {
                device_epoch,
                width,
                height,
                padded_bytes_per_row,
                output_texture,
                output_view,
                readback_buffer,
                tone_map_buffer,
                bind_group: None,
                bind_group_hdr_key: None,
                last_tone_map_uniform: None,
            });
        }
        let pool = guard.as_mut().expect("preview output pool");
        f(pool)
    })
}

fn wait_for_preview_readback(
    device: &wgpu::Device,
    rx: &std::sync::mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>,
) -> Result<(), String> {
    let deadline = Instant::now() + PREVIEW_READBACK_MAX_WAIT;
    loop {
        match rx.try_recv() {
            Ok(result) => {
                return result
                    .map_err(|err| format!("HDR preview tone-map buffer map failed: {err}"));
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                return Err("HDR preview tone-map readback channel closed".to_string());
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
        if Instant::now() >= deadline {
            return Err("HDR preview tone-map readback timed out".to_string());
        }
        device
            .poll(wgpu::PollType::Poll)
            .map_err(|err| format!("HDR preview tone-map device poll failed: {err:?}"))?;
        match device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(PREVIEW_READBACK_POLL_SLICE),
        }) {
            Ok(_) => {}
            Err(wgpu::PollError::Timeout) => {}
            Err(err) => {
                return Err(format!("HDR preview tone-map device poll failed: {err:?}"));
            }
        }
    }
}

fn with_preview_hdr_texture<R>(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    device_epoch: u64,
    buffer: &HdrImageBuffer,
    f: impl FnOnce(&CallbackUpload, PreviewBindGroupHdrKey) -> Result<R, String>,
) -> Result<R, String> {
    let pixels_sample_hash =
        super::image_key::sample_hash_f32_for_preview(buffer.rgba_f32.as_slice());
    PREVIEW_HDR_TEXTURE_CACHE.with(|slot| {
        let mut guard = slot.borrow_mut();
        let reuse_texture = guard.as_ref().is_some_and(|cached| {
            cached.device_epoch == device_epoch
                && cached.width == buffer.width
                && cached.height == buffer.height
        });
        let cache_hit = reuse_texture
            && guard
                .as_ref()
                .is_some_and(|cached| cached.pixels_sample_hash == pixels_sample_hash);

        if !cache_hit {
            if reuse_texture {
                let cached = guard.as_mut().expect("preview HDR texture cache");
                write_rgba32f_to_texture(
                    super::pending_gpu_writes::GpuUploadSink::Immediate(queue),
                    Arc::clone(&cached.upload.texture),
                    buffer.width,
                    buffer.height,
                    Arc::clone(&buffer.rgba_f32),
                )?;
                cached.pixels_sample_hash = pixels_sample_hash;
                cached.texture_generation = cached.texture_generation.wrapping_add(1);
            } else {
                let next_generation = guard
                    .as_ref()
                    .map(|cached| cached.texture_generation.wrapping_add(1))
                    .unwrap_or(1);
                *guard = Some(PreviewHdrTextureCache {
                    device_epoch,
                    width: buffer.width,
                    height: buffer.height,
                    pixels_sample_hash,
                    texture_generation: next_generation,
                    upload: upload_callback_image(
                        device,
                        super::pending_gpu_writes::GpuUploadSink::Immediate(queue),
                        buffer,
                        None,
                    )?,
                });
            }
        }

        let cached = guard.as_ref().expect("preview HDR texture cache");
        let hdr_key = PreviewBindGroupHdrKey {
            texture_generation: cached.texture_generation,
            pixels_sample_hash: cached.pixels_sample_hash,
            width: cached.width,
            height: cached.height,
        };
        f(&cached.upload, hdr_key)
    })
}

fn preview_tone_settings(buffer: &HdrImageBuffer) -> HdrToneMapSettings {
    let mut tone = HdrToneMapSettings::default();
    if let Some(max) = buffer.metadata.luminance.mastering_max_nits
        && max.is_finite()
        && max > tone.sdr_white_nits
    {
        tone.max_display_nits = max;
    }
    tone
}

/// Route HDR -> SDR bytes for downsampled previews: SIMD strip, GPU large, CPU fallback.
pub(crate) fn hdr_to_sdr_rgba8_for_preview(
    buffer: &HdrImageBuffer,
    exposure_ev: f32,
) -> Result<Vec<u8>, String> {
    let max_side = buffer.width.max(buffer.height);
    let tone = preview_tone_settings(buffer);

    if max_side <= PREVIEW_STRIP_MAX_SIDE {
        return hdr_to_sdr_rgba8_strip_preview(buffer, exposure_ev, &tone);
    }

    if let Some((device, queue, device_epoch)) = current_preview_gpu_context() {
        match hdr_to_sdr_rgba8_gpu(&device, &queue, device_epoch, buffer, exposure_ev, &tone) {
            Ok(pixels) => return Ok(pixels),
            Err(err) => {
                log::debug!("[HDR] GPU preview tone-map failed, CPU fallback: {err}");
            }
        }
    }

    hdr_to_sdr_rgba8_with_tone_settings(buffer, exposure_ev, &tone)
}

fn preview_gpu_cache() -> &'static Mutex<HashMap<u64, ToneMapPreviewGpuCache>> {
    PREVIEW_GPU_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn get_or_create_preview_gpu_cache(
    device: &wgpu::Device,
    device_epoch: u64,
) -> parking_lot::MutexGuard<'static, HashMap<u64, ToneMapPreviewGpuCache>> {
    let cache = preview_gpu_cache();
    let mut guard = cache.lock();
    if !guard.contains_key(&device_epoch) {
        guard.clear();
    }
    guard.entry(device_epoch).or_insert_with(|| {
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("simple-image-viewer-hdr-preview-tone-map-bind-group-layout"),
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
                        format: PREVIEW_GPU_OUTPUT_FORMAT,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("simple-image-viewer-hdr-preview-tone-map-pipeline-layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("simple-image-viewer-hdr-preview-tone-map-shader"),
            source: wgpu::ShaderSource::Wgsl(HDR_PREVIEW_TONE_MAP_SHADER.into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("simple-image-viewer-hdr-preview-tone-map-pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("cs_preview_tone_map"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        ToneMapPreviewGpuCache {
            bind_group_layout,
            pipeline,
        }
    });
    guard
}

fn hdr_to_sdr_rgba8_gpu(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    device_epoch: u64,
    buffer: &HdrImageBuffer,
    exposure_ev: f32,
    tone: &HdrToneMapSettings,
) -> Result<Vec<u8>, String> {
    if buffer.rgba_f32.is_empty() {
        return Err("GPU preview tone-map requires non-empty HDR float plane".to_string());
    }

    let width = buffer.width;
    let height = buffer.height;
    let expected_len = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| format!("HDR preview dimensions overflow: {width}x{height}"))?;
    if buffer.rgba_f32.len() != expected_len as usize {
        return Err(format!(
            "Malformed HDR preview buffer: expected {} floats for {}x{}, got {}",
            expected_len,
            width,
            height,
            buffer.rgba_f32.len()
        ));
    }

    let cache = get_or_create_preview_gpu_cache(device, device_epoch);
    let ToneMapPreviewGpuCache {
        bind_group_layout,
        pipeline,
    } = cache
        .get(&device_epoch)
        .ok_or_else(|| "HDR preview GPU cache missing entry".to_string())?;

    with_preview_hdr_texture(device, queue, device_epoch, buffer, |uploaded, hdr_key| {
        let mut tone = *tone;
        tone.exposure_ev = exposure_ev;
        // CPU scalar only applies PQ/HLG peak scaling; pin display nits for other transfers so
        // preview compute `encode_sdr` matches `encode_sdr_rgb8` (live viewer keeps user nits).
        if !matches!(
            buffer.metadata.transfer_function,
            HdrTransferFunction::Pq | HdrTransferFunction::Hlg
        ) {
            tone.max_display_nits = tone.sdr_white_nits;
        }
        let native_display_scale =
            libavif_tone_map_native_display_scale(&buffer.metadata, buffer.color_space, &tone);
        let uniform = image_tone_map_uniform(
            buffer,
            ImageToneMapUniformParams {
                common: ToneMapCommonParams {
                    settings: tone,
                    rotation_steps: 0,
                    alpha: 1.0,
                    output_mode: HdrRenderOutputMode::SdrToneMapped,
                    framebuffer_format: PREVIEW_GPU_OUTPUT_FORMAT,
                    uv_rect: egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
                    native_display_scale,
                },
                gpu_composed_scene_linear: false,
                ripple: None,
            },
        );
        let unpadded_bytes_per_row = width
            .checked_mul(4)
            .ok_or_else(|| format!("preview row bytes overflow for width {width}"))?;
        let padded_bytes_per_row = wgpu_copy_bytes_per_row(unpadded_bytes_per_row);
        let readback_size = padded_bytes_per_row as u64 * u64::from(height);

        with_preview_output_resources(
            device,
            device_epoch,
            width,
            height,
            padded_bytes_per_row,
            readback_size,
            |pool| {
                if pool.last_tone_map_uniform != Some(uniform) {
                    queue.write_buffer(&pool.tone_map_buffer, 0, bytemuck::bytes_of(&uniform));
                    pool.last_tone_map_uniform = Some(uniform);
                }
                if pool.bind_group.is_none() || pool.bind_group_hdr_key != Some(hdr_key) {
                    pool.bind_group = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("simple-image-viewer-hdr-preview-tone-map-bind-group"),
                        layout: bind_group_layout,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: wgpu::BindingResource::TextureView(&uploaded.view),
                            },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: pool.tone_map_buffer.as_entire_binding(),
                            },
                            wgpu::BindGroupEntry {
                                binding: 2,
                                resource: wgpu::BindingResource::TextureView(&pool.output_view),
                            },
                        ],
                    }));
                    pool.bind_group_hdr_key = Some(hdr_key);
                }
                let bind_group = pool
                    .bind_group
                    .as_ref()
                    .expect("preview tone-map bind group");

                let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("simple-image-viewer-hdr-preview-tone-map"),
                });
                {
                    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                        label: Some("simple-image-viewer-hdr-preview-tone-map-pass"),
                        timestamp_writes: None,
                    });
                    pass.set_pipeline(pipeline);
                    pass.set_bind_group(0, bind_group, &[]);
                    pass.dispatch_workgroups(width.div_ceil(16), height.div_ceil(16), 1);
                }
                encoder.copy_texture_to_buffer(
                    pool.output_texture.as_image_copy(),
                    wgpu::TexelCopyBufferInfo {
                        buffer: &pool.readback_buffer,
                        layout: wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(padded_bytes_per_row),
                            rows_per_image: None,
                        },
                    },
                    wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                );

                let (tx, rx) = std::sync::mpsc::sync_channel(1);
                queue.submit(Some(encoder.finish()));
                pool.readback_buffer
                    .slice(..)
                    .map_async(wgpu::MapMode::Read, move |result| {
                        let _ = tx.send(result);
                    });
                wait_for_preview_readback(device, &rx)?;

                let mapped = pool.readback_buffer.slice(..).get_mapped_range();
                let mut pixels = Vec::with_capacity(expected_len as usize);
                for row in mapped.chunks(padded_bytes_per_row as usize) {
                    pixels.extend_from_slice(&row[..unpadded_bytes_per_row as usize]);
                }
                drop(mapped);
                pool.readback_buffer.unmap();
                Ok(pixels)
            },
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_tone_map_shader_parses_as_wgsl() {
        let _ = wgpu::ShaderModuleDescriptor {
            label: Some("hdr-preview-tone-map-shader-parse-test"),
            source: wgpu::ShaderSource::Wgsl(HDR_PREVIEW_TONE_MAP_SHADER.into()),
        };
    }
}
