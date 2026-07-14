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
//! GPU-accelerates all 20 separable blend modes (Normal, Screen, Linear Dodge,
//! Multiply, Overlay, Soft Light, Hard Light, Darken, Color Burn, Linear Burn,
//! Lighten, Color Dodge, Vivid Light, Linear Light, Pin Light, Hard Mix,
//! Difference, Exclusion, Subtract, Divide). PackBits / ICC stay on CPU.
//!
//! Modes that remain on CPU because they require cross-channel or per-pixel
//! computation (not trivially vectorisable per-channel in the shader):
//!   - **Non-separable:** Hue (`hue `), Saturation (`sat `), Color (`colr`),
//!     Luminosity (`lum `) — need SetLum/ClipColor across R/G/B together.
//!   - **Per-pixel luminance compare:** Darker Color (`dkCl`), Lighter Color
//!     (`lgCl`) — compare whole-pixel luminance, cannot split per channel.
//!   - **Stochastic / pass-through:** Dissolve (`diss`), Pass Through (`pass`).
//!     Dissolve requires per-pixel randomness; Pass Through is equivalent to
//!     Normal but conceptually a group-level operator.
//!
//! These modes trigger a CPU fallback: `is_gpu_separable_blend` returns false
//! for them, so `gpu_batch_eligible_decoded_bytes` rejects the whole batch,
//! and the compositor falls back to `blend_layers_with_clipping` (CPU).
//!
//! # Clipping groups
//!
//! Clipping groups run on GPU (`cs_capture_base_alpha` /
//! `cs_apply_base_alpha_mask` + CPU-side group orchestration mirroring
//! `OpenClipGroup`). Sequential clip groups reuse one document-scoped
//! scratch texture pair cleared via `cs_clear_storage` (O(1) VRAM; no
//! CPU `vec![0]` full-canvas upload). Peak VRAM is gated by
//! [`crate::psb_layer_decode::gpu_batch_eligible_decoded_bytes`] before
//! this path runs. Vector masks / knockout / clip-to-folder remain out
//! of scope.
//!
//! 1. **Clip base-alpha mask is alpha-only (intentional):**
//!    `cs_apply_base_alpha_mask` scales group alpha (and clears RGB only when
//!    the quantized alpha becomes 0), matching CPU `apply_base_alpha_mask` /
//!    HDR `apply_one_base_alpha_mask`. Group RGB stays straight (unassociated).
//!    That is correct for the PDF separable formula used when flushing with
//!    the base blend (`scrn` / `mul ` / `lddg` / `norm`): coverage is applied
//!    via `sa`, so premultiplying RGB by the mask would double-attenuate.
//! 2. **Admission fallback:** if any decoded layer is not GPU-separable, the
//!    whole stack falls back to CPU `blend_layers_with_clipping` (all-or-
//!    nothing per document). Same full-CPU fallback on device/OpenGL/size
//!    gate, OOM, cancel, or readback failure.
//! 3. **Shader source** lives in [`crate::psb_blend_gpu_shaders`] (split out
//!    to keep this file under the review line limit).

use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::psb_blend_gpu_shaders::*;

const WORKGROUP: u32 = 16;
const READBACK_MAX_WAIT: Duration = Duration::from_secs(30);
/// Short Wait slice so cancel and `device_id_live` can be polled during readback.
/// 1 ms keeps device-replacement detection latency low without busy-waiting.
const READBACK_POLL_SLICE: Duration = Duration::from_millis(1);
/// `BlendParamsUniform` bytes (must match WGSL `BlendParams`).
const BLEND_PARAMS_BYTES: u64 = 32;
/// Upper bound on uniform dispatches per layer (clip clear/capture/apply/flush).
const UNIFORM_SLOTS_PER_LAYER: u32 = 8;
/// Floor on ring slots so tiny stacks still have headroom for clip clears.
const UNIFORM_RING_MIN_SLOTS: u32 = 16;

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

const _: () = assert!(std::mem::size_of::<BlendParamsUniform>() as u64 == BLEND_PARAMS_BYTES);

/// Single UNIFORM+COPY_DST ring; each dispatch writes one aligned slot and
/// binds it via a dynamic offset (avoids per-op `create_buffer_init`).
struct UniformRing {
    buffer: wgpu::Buffer,
    stride: u64,
    slot_count: u32,
    next_slot: u32,
}

impl UniformRing {
    fn with_slots(device: &wgpu::Device, slot_count: u32) -> Self {
        let align = u64::from(device.limits().min_uniform_buffer_offset_alignment.max(1));
        let stride = BLEND_PARAMS_BYTES
            .div_ceil(align)
            .saturating_mul(align)
            .max(align);
        let slot_count = slot_count.max(UNIFORM_RING_MIN_SLOTS);
        let size = stride.saturating_mul(u64::from(slot_count)).max(stride);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("psd-blend-uniform-ring"),
            size,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            buffer,
            stride,
            slot_count,
            next_slot: 0,
        }
    }

    fn push(
        &mut self,
        queue: &wgpu::Queue,
        params: &BlendParamsUniform,
    ) -> Result<u32, crate::loader::DecodeError> {
        if self.next_slot >= self.slot_count {
            return Err("PSD/PSB GPU uniform ring exhausted".to_string().into());
        }
        let offset = u64::from(self.next_slot).saturating_mul(self.stride);
        queue.write_buffer(&self.buffer, offset, bytemuck::bytes_of(params));
        let dyn_offset = u32::try_from(offset)
            .map_err(|_| "PSD/PSB GPU uniform ring offset exceeds u32".to_string())?;
        self.next_slot += 1;
        Ok(dyn_offset)
    }

    fn binding(&self) -> wgpu::BindingResource<'_> {
        wgpu::BindingResource::Buffer(wgpu::BufferBinding {
            buffer: &self.buffer,
            offset: 0,
            size: NonZeroU64::new(BLEND_PARAMS_BYTES),
        })
    }
}

fn uniform_ring_slots_for_layers(layer_count: usize) -> u32 {
    u32::try_from(layer_count)
        .unwrap_or(u32::MAX)
        .saturating_mul(UNIFORM_SLOTS_PER_LAYER)
        .saturating_add(UNIFORM_RING_MIN_SLOTS)
        .max(UNIFORM_RING_MIN_SLOTS)
}

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
    blend_normal_pipeline: wgpu::ComputePipeline,
    blend_screen_pipeline: wgpu::ComputePipeline,
    blend_linear_dodge_pipeline: wgpu::ComputePipeline,
    blend_multiply_pipeline: wgpu::ComputePipeline,
    blend_overlay_pipeline: wgpu::ComputePipeline,
    blend_soft_light_pipeline: wgpu::ComputePipeline,
    blend_hard_light_pipeline: wgpu::ComputePipeline,
    blend_darken_pipeline: wgpu::ComputePipeline,
    blend_color_burn_pipeline: wgpu::ComputePipeline,
    blend_linear_burn_pipeline: wgpu::ComputePipeline,
    blend_lighten_pipeline: wgpu::ComputePipeline,
    blend_color_dodge_pipeline: wgpu::ComputePipeline,
    blend_vivid_light_pipeline: wgpu::ComputePipeline,
    blend_linear_light_pipeline: wgpu::ComputePipeline,
    blend_pin_light_pipeline: wgpu::ComputePipeline,
    blend_hard_mix_pipeline: wgpu::ComputePipeline,
    blend_difference_pipeline: wgpu::ComputePipeline,
    blend_exclusion_pipeline: wgpu::ComputePipeline,
    blend_subtract_pipeline: wgpu::ComputePipeline,
    blend_divide_pipeline: wgpu::ComputePipeline,
    capture_base_alpha_pipeline: wgpu::ComputePipeline,
    apply_base_alpha_mask_pipeline: wgpu::ComputePipeline,
    clear_storage_pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

impl PsdBlendPipeline {
    fn blend_pipeline_for(&self, mode: u32) -> &wgpu::ComputePipeline {
        match mode {
            BLEND_MODE_SCREEN => &self.blend_screen_pipeline,
            BLEND_MODE_LINEAR_DODGE => &self.blend_linear_dodge_pipeline,
            BLEND_MODE_MULTIPLY => &self.blend_multiply_pipeline,
            BLEND_MODE_OVERLAY => &self.blend_overlay_pipeline,
            BLEND_MODE_SOFT_LIGHT => &self.blend_soft_light_pipeline,
            BLEND_MODE_HARD_LIGHT => &self.blend_hard_light_pipeline,
            BLEND_MODE_DARKEN => &self.blend_darken_pipeline,
            BLEND_MODE_COLOR_BURN => &self.blend_color_burn_pipeline,
            BLEND_MODE_LINEAR_BURN => &self.blend_linear_burn_pipeline,
            BLEND_MODE_LIGHTEN => &self.blend_lighten_pipeline,
            BLEND_MODE_COLOR_DODGE => &self.blend_color_dodge_pipeline,
            BLEND_MODE_VIVID_LIGHT => &self.blend_vivid_light_pipeline,
            BLEND_MODE_LINEAR_LIGHT => &self.blend_linear_light_pipeline,
            BLEND_MODE_PIN_LIGHT => &self.blend_pin_light_pipeline,
            BLEND_MODE_HARD_MIX => &self.blend_hard_mix_pipeline,
            BLEND_MODE_DIFFERENCE => &self.blend_difference_pipeline,
            BLEND_MODE_EXCLUSION => &self.blend_exclusion_pipeline,
            BLEND_MODE_SUBTRACT => &self.blend_subtract_pipeline,
            BLEND_MODE_DIVIDE => &self.blend_divide_pipeline,
            _ => &self.blend_normal_pipeline,
        }
    }
}

static PIPELINE_CACHE: Mutex<Option<Arc<PsdBlendPipeline>>> = Mutex::new(None);
static OPENGL_PREWARM_LOG: Once = Once::new();

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
    if is_opengl {
        OPENGL_PREWARM_LOG.call_once(|| {
            log::info!("[PSD] GPU separable blend disabled on OpenGL backend; using CPU");
        });
        return;
    }
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
                    has_dynamic_offset: true,
                    min_binding_size: NonZeroU64::new(BLEND_PARAMS_BYTES),
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
    let blend_normal_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("simple-image-viewer-psd-blend-normal-pipeline"),
        layout: Some(&pipeline_layout),
        module: &shader,
        entry_point: Some("cs_blend_normal"),
        compilation_options: wgpu::PipelineCompilationOptions::default(),
        cache: pipeline_cache,
    });
    let blend_screen_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("simple-image-viewer-psd-blend-screen-pipeline"),
        layout: Some(&pipeline_layout),
        module: &shader,
        entry_point: Some("cs_blend_screen"),
        compilation_options: wgpu::PipelineCompilationOptions::default(),
        cache: pipeline_cache,
    });
    let blend_linear_dodge_pipeline =
        device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("simple-image-viewer-psd-blend-linear-dodge-pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("cs_blend_linear_dodge"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: pipeline_cache,
        });
    let blend_multiply_pipeline =
        device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("simple-image-viewer-psd-blend-multiply-pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("cs_blend_multiply"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: pipeline_cache,
        });
    // ── New modes (17 additional blend shaders) ────────────────────────
    macro_rules! pipe_create {
        ($entry:expr, $label:expr) => {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(concat!(
                    "simple-image-viewer-psd-blend-",
                    $label,
                    "-pipeline"
                )),
                layout: Some(&pipeline_layout),
                module: &shader,
                entry_point: Some($entry),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                cache: pipeline_cache,
            })
        };
    }
    let blend_overlay_pipeline = pipe_create!("cs_blend_overlay", "overlay");
    let blend_soft_light_pipeline = pipe_create!("cs_blend_soft_light", "soft-light");
    let blend_hard_light_pipeline = pipe_create!("cs_blend_hard_light", "hard-light");
    let blend_darken_pipeline = pipe_create!("cs_blend_darken", "darken");
    let blend_color_burn_pipeline = pipe_create!("cs_blend_color_burn", "color-burn");
    let blend_linear_burn_pipeline = pipe_create!("cs_blend_linear_burn", "linear-burn");
    let blend_lighten_pipeline = pipe_create!("cs_blend_lighten", "lighten");
    let blend_color_dodge_pipeline = pipe_create!("cs_blend_color_dodge", "color-dodge");
    let blend_vivid_light_pipeline = pipe_create!("cs_blend_vivid_light", "vivid-light");
    let blend_linear_light_pipeline = pipe_create!("cs_blend_linear_light", "linear-light");
    let blend_pin_light_pipeline = pipe_create!("cs_blend_pin_light", "pin-light");
    let blend_hard_mix_pipeline = pipe_create!("cs_blend_hard_mix", "hard-mix");
    let blend_difference_pipeline = pipe_create!("cs_blend_difference", "difference");
    let blend_exclusion_pipeline = pipe_create!("cs_blend_exclusion", "exclusion");
    let blend_subtract_pipeline = pipe_create!("cs_blend_subtract", "subtract");
    let blend_divide_pipeline = pipe_create!("cs_blend_divide", "divide");
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
    let clear_storage_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("simple-image-viewer-psd-clear-storage-pipeline"),
        layout: Some(&pipeline_layout),
        module: &shader,
        entry_point: Some("cs_clear_storage"),
        compilation_options: wgpu::PipelineCompilationOptions::default(),
        cache: pipeline_cache,
    });
    Ok(PsdBlendPipeline {
        device_id,
        blend_normal_pipeline,
        blend_screen_pipeline,
        blend_linear_dodge_pipeline,
        blend_multiply_pipeline,
        blend_overlay_pipeline,
        blend_soft_light_pipeline,
        blend_hard_light_pipeline,
        blend_darken_pipeline,
        blend_color_burn_pipeline,
        blend_linear_burn_pipeline,
        blend_lighten_pipeline,
        blend_color_dodge_pipeline,
        blend_vivid_light_pipeline,
        blend_linear_light_pipeline,
        blend_pin_light_pipeline,
        blend_hard_mix_pipeline,
        blend_difference_pipeline,
        blend_exclusion_pipeline,
        blend_subtract_pipeline,
        blend_divide_pipeline,
        capture_base_alpha_pipeline,
        apply_base_alpha_mask_pipeline,
        clear_storage_pipeline,
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
        Err(e) if e.is_cancelled() => None,
        Err(e) => {
            log::debug!("[PSD] GPU separable blend fell back to CPU: {e}");
            None
        }
    }
}

struct GpuBlendResources {
    textures: Vec<wgpu::Texture>,
    bind_groups: Vec<wgpu::BindGroup>,
}

impl GpuBlendResources {
    fn with_capacity(layer_count: usize) -> Self {
        Self {
            textures: Vec::with_capacity(layer_count.saturating_mul(2)),
            bind_groups: Vec::with_capacity(layer_count.saturating_mul(2)),
        }
    }
}

/// Document-scoped scratch for clipping groups. Sequential groups reuse the same
/// pair of full-canvas textures (O(1) VRAM); `dirty` forces an in-pass clear
/// before the next materialize.
struct ClipGroupScratch {
    /// Kept so views remain valid until after queue submit.
    _group_texture: wgpu::Texture,
    group_view: wgpu::TextureView,
    _base_alpha_texture: wgpu::Texture,
    base_alpha_view: wgpu::TextureView,
    dirty: bool,
}

/// Soft handle while a clip group is open; textures live in [`ClipGroupScratch`].
struct MaterializedClipGroup {
    group_view: wgpu::TextureView,
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

fn create_full_canvas_storage_texture(
    device: &wgpu::Device,
    label: &'static str,
    canvas_w: u32,
    canvas_h: u32,
) -> (wgpu::Texture, wgpu::TextureView) {
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
        usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

fn create_dummy_layer_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("psd-clear-dummy-layer"),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
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
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &[0u8; 4],
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4),
            rows_per_image: Some(1),
        },
        wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

fn ensure_clip_group_scratch<'a>(
    device: &wgpu::Device,
    canvas_w: u32,
    canvas_h: u32,
    scratch: &'a mut Option<ClipGroupScratch>,
) -> &'a mut ClipGroupScratch {
    scratch.get_or_insert_with(|| {
        let (group_texture, group_view) = create_full_canvas_storage_texture(
            device,
            "psd-clip-group-texture",
            canvas_w,
            canvas_h,
        );
        let (base_alpha_texture, base_alpha_view) = create_full_canvas_storage_texture(
            device,
            "psd-clip-base-alpha-texture",
            canvas_w,
            canvas_h,
        );
        ClipGroupScratch {
            _group_texture: group_texture,
            group_view,
            _base_alpha_texture: base_alpha_texture,
            base_alpha_view,
            // Uninitialized GPU memory -- first materialize always clears.
            dirty: true,
        }
    })
}

#[allow(clippy::too_many_arguments)]
fn encode_params_dispatch(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pass: &mut wgpu::ComputePass<'_>,
    pipe: &PsdBlendPipeline,
    resources: &mut GpuBlendResources,
    ring: &mut UniformRing,
    pipeline: &wgpu::ComputePipeline,
    label: &str,
    target_view: &wgpu::TextureView,
    source_view: &wgpu::TextureView,
    params: BlendParamsUniform,
    dispatch_w: u32,
    dispatch_h: u32,
) -> Result<(), crate::loader::DecodeError> {
    if dispatch_w == 0 || dispatch_h == 0 {
        return Ok(());
    }
    let dyn_offset = ring.push(queue, &params)?;
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(label),
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
                resource: ring.binding(),
            },
        ],
    });
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, &bind_group, &[dyn_offset]);
    pass.dispatch_workgroups(
        dispatch_w.div_ceil(WORKGROUP),
        dispatch_h.div_ceil(WORKGROUP),
        1,
    );
    resources.bind_groups.push(bind_group);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn encode_clear_storage(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pass: &mut wgpu::ComputePass<'_>,
    pipe: &PsdBlendPipeline,
    resources: &mut GpuBlendResources,
    ring: &mut UniformRing,
    target_view: &wgpu::TextureView,
    dummy_layer_view: &wgpu::TextureView,
    canvas_w: u32,
    canvas_h: u32,
) -> Result<(), crate::loader::DecodeError> {
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
    encode_params_dispatch(
        device,
        queue,
        pass,
        pipe,
        resources,
        ring,
        &pipe.clear_storage_pipeline,
        "psd-clear-storage-bg",
        target_view,
        dummy_layer_view,
        params,
        canvas_w,
        canvas_h,
    )
}

#[allow(clippy::too_many_arguments)]
fn encode_blend_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pass: &mut wgpu::ComputePass<'_>,
    pipe: &PsdBlendPipeline,
    resources: &mut GpuBlendResources,
    ring: &mut UniformRing,
    target_view: &wgpu::TextureView,
    source_view: &wgpu::TextureView,
    canvas_w: u32,
    canvas_h: u32,
    layer_w: u32,
    layer_h: u32,
    layer_left: i32,
    layer_top: i32,
    mode: u32,
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
    encode_params_dispatch(
        device,
        queue,
        pass,
        pipe,
        resources,
        ring,
        pipe.blend_pipeline_for(mode),
        "psd-separable-blend-bg",
        target_view,
        source_view,
        params,
        layer_w,
        layer_h,
    )
}

#[allow(clippy::too_many_arguments)]
fn encode_blend_decoded_layer(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pass: &mut wgpu::ComputePass<'_>,
    pipe: &PsdBlendPipeline,
    resources: &mut GpuBlendResources,
    ring: &mut UniformRing,
    target_view: &wgpu::TextureView,
    canvas_w: u32,
    canvas_h: u32,
    layer: &DecodedLayerRef<'_>,
    mode: u32,
) -> Result<(), crate::loader::DecodeError> {
    validate_gpu_layer(layer)?;
    if layer.width == 0 || layer.height == 0 {
        return Ok(());
    }
    let (layer_tex, layer_view) = create_uploaded_layer_texture(device, queue, layer)?;
    encode_blend_texture(
        device,
        queue,
        pass,
        pipe,
        resources,
        ring,
        target_view,
        &layer_view,
        canvas_w,
        canvas_h,
        layer.width,
        layer.height,
        layer.left,
        layer.top,
        mode,
    )?;
    resources.textures.push(layer_tex);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn encode_capture_base_alpha(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pass: &mut wgpu::ComputePass<'_>,
    pipe: &PsdBlendPipeline,
    resources: &mut GpuBlendResources,
    ring: &mut UniformRing,
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
    encode_params_dispatch(
        device,
        queue,
        pass,
        pipe,
        resources,
        ring,
        &pipe.capture_base_alpha_pipeline,
        "psd-capture-base-alpha-bg",
        base_alpha_view,
        base_view,
        params,
        base.width,
        base.height,
    )
}

#[allow(clippy::too_many_arguments)]
fn encode_apply_base_alpha_mask(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pass: &mut wgpu::ComputePass<'_>,
    pipe: &PsdBlendPipeline,
    resources: &mut GpuBlendResources,
    ring: &mut UniformRing,
    group_view: &wgpu::TextureView,
    base_alpha_view: &wgpu::TextureView,
    canvas_w: u32,
    canvas_h: u32,
) -> Result<(), crate::loader::DecodeError> {
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
    encode_params_dispatch(
        device,
        queue,
        pass,
        pipe,
        resources,
        ring,
        &pipe.apply_base_alpha_mask_pipeline,
        "psd-apply-base-alpha-mask-bg",
        group_view,
        base_alpha_view,
        params,
        canvas_w,
        canvas_h,
    )
}

#[allow(clippy::too_many_arguments)]
fn materialize_clip_group(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pass: &mut wgpu::ComputePass<'_>,
    pipe: &PsdBlendPipeline,
    resources: &mut GpuBlendResources,
    ring: &mut UniformRing,
    scratch: &mut ClipGroupScratch,
    dummy_layer_view: &wgpu::TextureView,
    canvas_w: u32,
    canvas_h: u32,
    base: &DecodedLayerRef<'_>,
) -> Result<MaterializedClipGroup, crate::loader::DecodeError> {
    validate_gpu_layer(base)?;
    if scratch.dirty {
        encode_clear_storage(
            device,
            queue,
            pass,
            pipe,
            resources,
            ring,
            &scratch.group_view,
            dummy_layer_view,
            canvas_w,
            canvas_h,
        )?;
        encode_clear_storage(
            device,
            queue,
            pass,
            pipe,
            resources,
            ring,
            &scratch.base_alpha_view,
            dummy_layer_view,
            canvas_w,
            canvas_h,
        )?;
    }
    if base.width != 0 && base.height != 0 {
        let (base_texture, base_view) = create_uploaded_layer_texture(device, queue, base)?;
        encode_blend_texture(
            device,
            queue,
            pass,
            pipe,
            resources,
            ring,
            &scratch.group_view,
            &base_view,
            canvas_w,
            canvas_h,
            base.width,
            base.height,
            base.left,
            base.top,
            BLEND_MODE_NORMAL,
        )?;
        encode_capture_base_alpha(
            device,
            queue,
            pass,
            pipe,
            resources,
            ring,
            &scratch.base_alpha_view,
            &base_view,
            canvas_w,
            canvas_h,
            base,
        )?;
        resources.textures.push(base_texture);
    }
    scratch.dirty = true;
    Ok(MaterializedClipGroup {
        group_view: scratch.group_view.clone(),
        base_alpha_view: scratch.base_alpha_view.clone(),
    })
}

#[allow(clippy::too_many_arguments)]
fn flush_open_clip_group(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pass: &mut wgpu::ComputePass<'_>,
    pipe: &PsdBlendPipeline,
    resources: &mut GpuBlendResources,
    ring: &mut UniformRing,
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
            queue,
            pass,
            pipe,
            resources,
            ring,
            &group.group_view,
            &group.base_alpha_view,
            canvas_w,
            canvas_h,
        )?;
        encode_blend_texture(
            device,
            queue,
            pass,
            pipe,
            resources,
            ring,
            main_canvas_view,
            &group.group_view,
            canvas_w,
            canvas_h,
            canvas_w,
            canvas_h,
            0,
            0,
            separable_blend_mode_u32(&base.blend),
        )?;
        return Ok(());
    }
    encode_blend_decoded_layer(
        device,
        queue,
        pass,
        pipe,
        resources,
        ring,
        main_canvas_view,
        canvas_w,
        canvas_h,
        base,
        separable_blend_mode_u32(&base.blend),
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
    // Kept until after submit so bind groups referencing scratch/dummy/ring stay valid.
    let mut clip_scratch: Option<ClipGroupScratch> = None;
    let mut uniform_ring =
        UniformRing::with_slots(device, uniform_ring_slots_for_layers(layers.len()));
    let (dummy_layer_tex, dummy_layer_view) = create_dummy_layer_texture(device, queue);

    {
        let mut pass = begin_psd_compute_pass(&mut encoder, "psd-separable-blend-batch");
        let mut open_base: Option<&DecodedLayerRef<'_>> = None;
        let mut materialized_group: Option<MaterializedClipGroup> = None;

        for layer in layers {
            crate::psb_reader::check_decode_cancel(cancel)?;
            if layer.clipping != 0 {
                let Some(base) = open_base else {
                    continue;
                };
                if materialized_group.is_none() {
                    let scratch =
                        ensure_clip_group_scratch(device, canvas_w, canvas_h, &mut clip_scratch);
                    materialized_group = Some(materialize_clip_group(
                        device,
                        queue,
                        &mut pass,
                        pipe,
                        &mut resources,
                        &mut uniform_ring,
                        scratch,
                        &dummy_layer_view,
                        canvas_w,
                        canvas_h,
                        base,
                    )?);
                }
                let Some(group) = materialized_group.as_ref() else {
                    return Err("PSD/PSB GPU clip group missing before clip blend"
                        .to_string()
                        .into());
                };
                let group_view = &group.group_view;
                encode_blend_decoded_layer(
                    device,
                    queue,
                    &mut pass,
                    pipe,
                    &mut resources,
                    &mut uniform_ring,
                    group_view,
                    canvas_w,
                    canvas_h,
                    layer,
                    separable_blend_mode_u32(&layer.blend),
                )?;
                continue;
            }

            flush_open_clip_group(
                device,
                queue,
                &mut pass,
                pipe,
                &mut resources,
                &mut uniform_ring,
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
            &mut pass,
            pipe,
            &mut resources,
            &mut uniform_ring,
            &canvas_view,
            canvas_w,
            canvas_h,
            open_base.take(),
            materialized_group.take(),
        )?;
    }

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
    drop(clip_scratch);
    drop(dummy_layer_tex);

    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    readback
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
    wait_for_readback(ctx, device, &rx, cancel)?;

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
    ctx: &PsdGpuContext,
    device: &wgpu::Device,
    rx: &std::sync::mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<(), crate::loader::DecodeError> {
    let deadline = Instant::now() + READBACK_MAX_WAIT;
    loop {
        crate::psb_reader::check_decode_cancel(cancel)?;
        if !ctx.is_device_current() {
            return Err("wgpu device replaced during PSD blend readback".into());
        }
        match rx.try_recv() {
            Ok(result) => {
                return result
                    .map_err(|err| format!("PSD blend readback map failed: {err}").into());
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                return Err("PSD blend readback channel closed".into());
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
        let now = Instant::now();
        if now >= deadline {
            return Err("PSD blend readback timed out".into());
        }
        // Non-blocking poll first: catch already-complete submissions immediately
        // so device-replacement and cancel detection is not delayed by the wait below.
        let _ = device.poll(wgpu::PollType::Poll);

        // Wake periodically so cancel and device replacement can abort before
        // READBACK_MAX_WAIT instead of blocking in a single long Wait.
        let remaining = deadline.saturating_duration_since(now);
        let timeout = remaining.min(READBACK_POLL_SLICE);
        match device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(timeout),
        }) {
            Ok(_) => {}
            Err(wgpu::PollError::Timeout) => {}
            Err(err) => {
                return Err(format!("PSD blend device poll failed: {err:?}").into());
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
    fn separable_shader_uses_mode_specific_entries() {
        assert!(PSD_SEPARABLE_BLEND_SHADER.contains("mode: u32"));
        assert!(PSD_SEPARABLE_BLEND_SHADER.contains("fn cs_blend_normal"));
        assert!(PSD_SEPARABLE_BLEND_SHADER.contains("fn cs_blend_screen"));
        assert!(PSD_SEPARABLE_BLEND_SHADER.contains("fn cs_blend_linear_dodge"));
        assert!(PSD_SEPARABLE_BLEND_SHADER.contains("fn cs_blend_multiply"));
        assert!(PSD_SEPARABLE_BLEND_SHADER.contains("sa * (1.0 - da)"));
    }

    #[test]
    fn clip_shader_declares_capture_and_mask_entries() {
        assert!(PSD_SEPARABLE_BLEND_SHADER.contains("fn cs_capture_base_alpha"));
        assert!(PSD_SEPARABLE_BLEND_SHADER.contains("fn cs_apply_base_alpha_mask"));
        assert!(PSD_SEPARABLE_BLEND_SHADER.contains("fn cs_clear_storage"));
    }

    #[test]
    fn clip_shader_quantizes_masked_alpha_like_cpu() {
        // Must stay aligned with `psb_layer_blend_simd::UNIT_TO_U8_WGSL_FLOOR_BIAS`
        // / `f32_to_u8_round` (round-half-away-from-zero, not WGSL ties-to-even).
        assert!(
            PSD_SEPARABLE_BLEND_SHADER.contains("let a_u = u32(floor(group.a * 255.0 + 0.5));")
        );
        assert!(PSD_SEPARABLE_BLEND_SHADER.contains("let m_u = u32(floor(mask * 255.0 + 0.5));"));
        assert!(PSD_SEPARABLE_BLEND_SHADER.contains("floor("));
        assert!(PSD_SEPARABLE_BLEND_SHADER.contains("255.0 + 0.5"));
        assert!(crate::psb_layer_blend_simd::UNIT_TO_U8_WGSL_FLOOR_BIAS.contains("floor"));
        assert!(PSD_SEPARABLE_BLEND_SHADER.contains("let out_a_u = (a_u * m_u) / 255u;"));
        assert!(PSD_SEPARABLE_BLEND_SHADER.contains("if (out_a_u == 0u)"));
    }

    #[test]
    fn separable_shader_guards_div_by_tiny_out_a() {
        assert!(PSD_SEPARABLE_BLEND_SHADER.contains("if (out_a <= 0.0)"));
        assert!(PSD_SEPARABLE_BLEND_SHADER.contains("co / max(out_a, 1e-20)"));
    }

    #[test]
    fn clip_shader_preserves_straight_rgb_when_masking_alpha() {
        // Partial mask writes `group.rgb` unchanged with scaled alpha -- not
        // `group.rgb * mask` (would double-attenuate separable base blends).
        assert!(
            PSD_SEPARABLE_BLEND_SHADER.contains(
                "textureStore(target, coord, vec4<f32>(group.rgb, f32(out_a_u) / 255.0));"
            )
        );
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

    // GPU accuracy tests are default-on. When `try_test_psd_gpu_context()` is
    // None they soft-skip (still pass). See docs/psd-psb-known-limits.md.
    #[test]
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
