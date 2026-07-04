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

#[cfg(feature = "heif-native")]
#[path = "../apple_compose_gpu.rs"]
mod apple_compose_gpu;
#[path = "../jpeg_compose_gpu.rs"]
mod jpeg_compose_gpu;

mod output_mode;
pub(crate) use output_mode::hdr_sdr_framebuffer_needs_manual_srgb_oetf;
pub use output_mode::{
    HdrRenderOutputMode, hdr_egui_overlay_diagnostics, hdr_render_output_diagnostics,
};

mod shader_source;
use shader_source::HDR_IMAGE_PLANE_SHADER;

mod tone_map_uniform;
use self::tone_map_uniform::{
    AppleToneMapCompose, HdrTileToneMapUniformParams, ImageToneMapUniformParams,
    RippleToneMapParams, ToneMapCommonParams, ToneMapInputMetadata, ToneMapUniform,
    ToneMapUniformParams, hdr_tile_tone_map_uniform, image_tone_map_uniform,
    libavif_tone_map_native_display_scale,
};

pub(super) mod image_key;
pub(super) use self::image_key::HdrTileKey;
pub(crate) use self::image_key::{HdrImageKey, RawGpuDemosaicBakedNotice};

pub(super) mod resources;
pub(crate) use self::resources::{
    CallbackUpload, HDR_APPLE_GAIN_TEXTURE_FORMAT, HdrCallbackResources, HdrImageBinding,
    ImagePlaneUpload, JpegTiledUploadKey,
};

#[cfg(test)]
pub(super) use self::resources::create_callback_resources;

mod prewarm;
pub(crate) use self::prewarm::{
    HdrCallbackResourcesPrewarm, HdrCallbackResourcesPrewarmSlot, HdrCallbackResourcesReadiness,
    HdrCallbackResourcesSet, ensure_hdr_callback_resources, hdr_callback_formats_to_prewarm,
    hdr_callback_resources_readiness, predicted_hdr_callback_target_format,
};

pub(super) mod tile_cache;
pub(super) use self::tile_cache::{
    HdrTileBindings, HdrTileInsert, iso_deferred_tile_compose_views_reusable,
};

mod pending_work;
pub(crate) use self::pending_work::{
    HdrCompletedComposeFailure, HdrCompletedComposeWrite, HdrCompletedJpegTiledSourceUpload,
    HdrCompletedPlaneUpload, HdrCompletedTileUpload, HdrPendingAppleImageComposeRequest,
    HdrPendingIsoImageComposeRequest, HdrPendingIsoTileComposeRequest,
    HdrPendingJpegTiledSourceUploadRequest, HdrPendingPlaneUploadRequest,
    HdrPendingTileUploadRequest, HdrPendingWorkQueues, MAX_HDR_CPU_COMPOSE_STARTS_PER_LOGIC,
    MAX_HDR_GPU_WRITES_PER_LOGIC, MAX_HDR_JPEG_TILED_SOURCE_UPLOADS_PER_LOGIC,
    MAX_HDR_LOADER_PLANE_UPLOADS_INFLIGHT, MAX_HDR_PLANE_UPLOADS_PER_LOGIC,
    MAX_HDR_TILE_UPLOADS_PER_LOGIC,
};

mod pending_gpu_writes;
pub(crate) use self::pending_gpu_writes::{GpuUploadSink, HdrGpuUploadStage};

mod tone_map_gpu;
pub(crate) use self::tone_map_gpu::{hdr_to_sdr_rgba8_for_preview, with_preview_tone_map_gpu};

pub(super) mod upload;
#[cfg(test)]
pub(crate) use self::resources::hdr_image_binding_is_eviction_candidate;
/// Background HDR plane upload used by loader workers and deferred cache-miss uploads in `logic()`.
pub(crate) use self::upload::{
    loader_background_upload_image_plane, upload_image_plane_with_sink,
};
#[cfg(test)]
pub(crate) use self::upload::test_upload_image_plane;
pub(crate) use self::upload::{upload_callback_tile, upload_jpeg_tiled_source_textures};
pub(super) use self::upload::{
    create_empty_rgba32f_texture, create_hdr_image_plane_bind_group, pack_rows_for_texture_copy,
    rgba32f_as_bytes, validate_upload_layout, write_rgba32f_to_texture,
};

pub(super) mod image_callback;
pub(super) use self::image_callback::HdrImagePlaneCallback;

pub(super) mod tile_callback;
pub(super) use self::tile_callback::HdrTilePlaneCallback;

#[cfg(feature = "heif-native")]
use super::heif_apple_gain_map::apple_gain_map_display_weight;
#[cfg(feature = "heif-native")]
use super::heif_apple_gain_map_gpu::apple_heic_deferred_from_metadata;
use super::jpeg_gain_map_gpu::iso_deferred_from_metadata;
use super::types::{
    HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat, HdrReference,
    HdrToneMapSettings, HdrTransferFunction,
};
use crate::hdr::gain_map::GainMapMetadata;
use eframe::{
    egui,
    egui_wgpu::{self, CallbackResources, CallbackTrait},
};
use parking_lot::Mutex;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use wgpu::util::DeviceExt;

pub const HDR_IMAGE_PLANE_TEXTURE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba32Float;

pub(crate) const RIPPLE_CLIP_INSIDE: u32 = 1;
pub(crate) const RIPPLE_CLIP_OUTSIDE: u32 = 2;

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
        let pending = HdrPendingWorkQueues::new_shared();
        let uploaded = upload_image_plane_with_sink(
            device,
            GpuUploadSink::Pending {
                queues: pending.as_ref(),
                stage: HdrGpuUploadStage::PlaneCreate,
            },
            image,
        )?;
        pending.flush_staged_writes_for_registration(queue);

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
            texture: Arc::try_unwrap(uploaded.base.texture)
                .unwrap_or_else(|arc| (*arc).clone()),
            view: uploaded.base.view,
            sampler,
        });

        Ok(())
    }
}

#[allow(dead_code)]
pub fn hdr_image_plane_callback(
    rect: egui::Rect,
    image: Arc<HdrImageBuffer>,
    tone_map: HdrToneMapSettings,
    target_format: wgpu::TextureFormat,
    output_mode: HdrRenderOutputMode,
    rotation_steps: u32,
    alpha: f32,
) -> egui::Shape {
    hdr_image_plane_callback_with_uv(
        rect,
        HdrImagePlaneCallbackParams {
            image,
            tone_map,
            target_format,
            output_mode,
            rotation_steps,
            alpha,
            uv_rect: egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
            ripple: None,
            keep_resident: false,
            raw_demosaic_baked_notify: None,
            pending_work: None,
        },
    )
}

pub struct HdrImagePlaneCallbackParams {
    pub image: Arc<HdrImageBuffer>,
    pub tone_map: HdrToneMapSettings,
    pub target_format: wgpu::TextureFormat,
    pub output_mode: HdrRenderOutputMode,
    pub rotation_steps: u32,
    pub alpha: f32,
    pub uv_rect: egui::Rect,
    pub ripple: Option<(egui::Pos2, f32, f32, u32)>,
    pub keep_resident: bool,
    pub raw_demosaic_baked_notify: Option<Arc<Mutex<Vec<RawGpuDemosaicBakedNotice>>>>,
    pub pending_work: Option<Arc<HdrPendingWorkQueues>>,
}

pub fn hdr_image_plane_callback_with_uv(
    rect: egui::Rect,
    params: HdrImagePlaneCallbackParams,
) -> egui::Shape {
    let HdrImagePlaneCallbackParams {
        image,
        tone_map,
        target_format,
        output_mode,
        rotation_steps,
        alpha,
        uv_rect,
        ripple,
        keep_resident,
        raw_demosaic_baked_notify,
        pending_work,
    } = params;
    egui::Shape::Callback(egui_wgpu::Callback::new_paint_callback(
        rect,
        HdrImagePlaneCallback {
            image,
            tone_map,
            target_format,
            output_mode,
            rotation_steps: rotation_steps % 4,
            alpha,
            uv_rect,
            ripple,
            keep_resident,
            raw_demosaic_baked_notify,
            pending_work,
        },
    ))
}

#[allow(dead_code)]
pub fn hdr_tile_plane_callback(
    rect: egui::Rect,
    tile: Arc<crate::hdr::tiled::HdrTileBuffer>,
    tone_map: HdrToneMapSettings,
    target_format: wgpu::TextureFormat,
    output_mode: HdrRenderOutputMode,
    rotation_steps: u32,
    alpha: f32,
) -> egui::Shape {
    hdr_tile_plane_callback_with_uv(
        rect,
        HdrTilePlaneCallbackParams {
            tile,
            tone_map,
            target_format,
            output_mode,
            rotation_steps,
            alpha,
            uv_rect: egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
            pending_work: None,
        },
    )
}

#[allow(dead_code)]
pub struct HdrTilePlaneCallbackParams {
    pub tile: Arc<crate::hdr::tiled::HdrTileBuffer>,
    pub tone_map: HdrToneMapSettings,
    pub target_format: wgpu::TextureFormat,
    pub output_mode: HdrRenderOutputMode,
    pub rotation_steps: u32,
    pub alpha: f32,
    pub uv_rect: egui::Rect,
    pub pending_work: Option<Arc<HdrPendingWorkQueues>>,
}

pub fn hdr_tile_plane_callback_with_uv(
    rect: egui::Rect,
    params: HdrTilePlaneCallbackParams,
) -> egui::Shape {
    let HdrTilePlaneCallbackParams {
        tile,
        tone_map,
        target_format,
        output_mode,
        rotation_steps,
        alpha,
        uv_rect,
        pending_work,
    } = params;
    egui::Shape::Callback(egui_wgpu::Callback::new_paint_callback(
        rect,
        HdrTilePlaneCallback {
            tile,
            tone_map,
            target_format,
            output_mode,
            rotation_steps: rotation_steps % 4,
            alpha,
            uv_rect,
            pending_work,
        },
    ))
}

#[cfg(test)]
mod tests;
