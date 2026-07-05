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
use crate::hdr::types::IsoDeferredTileContext;
use crate::hdr::types::IsoGainMapGpuSource;
use parking_lot::Mutex;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

pub(crate) use super::pending_gpu_writes::{
    HdrPendingGpuWriteQueues, MAX_HDR_GPU_WRITES_PER_LOGIC,
};

/// Max loader workers concurrently packing/enqueueing plane GPU writes.
pub(crate) const MAX_HDR_LOADER_PLANE_UPLOADS_INFLIGHT: usize = 1;

/// Main-thread plane uploads per logic tick (OpenGL backend cannot upload off-thread).
pub(crate) const MAX_HDR_PLANE_UPLOADS_PER_LOGIC: usize = 1;

/// Tile plane uploads started per logic tick.
pub(crate) const MAX_HDR_TILE_UPLOADS_PER_LOGIC: usize = 2;

/// Shared JPEG tiled SDR/gain source uploads per logic tick.
pub(crate) const MAX_HDR_JPEG_TILED_SOURCE_UPLOADS_PER_LOGIC: usize = 1;

/// Background ISO/Apple CPU compose jobs started per logic tick.
/// OpenGL and resource-limited GPU paths defer HDR compose, RAW demosaic, and tone-map work
/// here instead of blocking `prepare()`; the cap keeps UI logic responsive on degraded backends.
pub(crate) const MAX_HDR_CPU_COMPOSE_STARTS_PER_LOGIC: usize = 2;

#[derive(Default)]
struct HdrPendingWorkInflight {
    plane_uploads: HashSet<HdrImageKey>,
    tile_uploads: HashSet<HdrTileKey>,
    jpeg_tiled_source_uploads: HashSet<(JpegTiledUploadKey, wgpu::TextureFormat)>,
    iso_image_compose: HashSet<(HdrImageKey, u32)>,
    apple_image_compose: HashSet<(HdrImageKey, u32)>,
    iso_tile_compose: HashSet<(HdrTileKey, u32)>,
}

pub(crate) struct HdrPendingPlaneUploadRequest {
    pub key: HdrImageKey,
    pub image: Arc<HdrImageBuffer>,
    pub target_format: wgpu::TextureFormat,
    pub tone_map: HdrToneMapSettings,
    pub output_mode: HdrRenderOutputMode,
    pub keep_resident: bool,
}

pub(crate) struct HdrPendingTileUploadRequest {
    pub tile_key: HdrTileKey,
    pub tile: Arc<crate::hdr::tiled::HdrTileBuffer>,
    pub target_format: wgpu::TextureFormat,
    pub tone_map: HdrToneMapSettings,
    pub output_mode: HdrRenderOutputMode,
    pub rotation_steps: u32,
    pub alpha: f32,
    pub uv_rect: egui::Rect,
}

pub(crate) struct HdrPendingJpegTiledSourceUploadRequest {
    pub target_format: wgpu::TextureFormat,
    pub upload_key: JpegTiledUploadKey,
    pub deferred: IsoGainMapGpuSource,
    pub physical_width: u32,
    pub physical_height: u32,
}

pub(crate) struct HdrCompletedPlaneUpload {
    pub key: HdrImageKey,
    pub uploaded: ImagePlaneUpload,
    pub image: Arc<HdrImageBuffer>,
    pub target_format: wgpu::TextureFormat,
    pub tone_map: HdrToneMapSettings,
    pub output_mode: HdrRenderOutputMode,
    pub keep_resident: bool,
    pub device_id: u64,
}

pub(crate) struct HdrCompletedTileUpload {
    pub tile_key: HdrTileKey,
    pub tile: Arc<crate::hdr::tiled::HdrTileBuffer>,
    pub target_format: wgpu::TextureFormat,
    pub uploaded: CallbackUpload,
    pub tone_map: HdrToneMapSettings,
    pub output_mode: HdrRenderOutputMode,
    pub rotation_steps: u32,
    pub alpha: f32,
    pub uv_rect: egui::Rect,
    pub device_id: u64,
    pub staged_gpu_upload: bool,
}

pub(crate) struct HdrCompletedJpegTiledSourceUpload {
    pub target_format: wgpu::TextureFormat,
    pub upload_key: JpegTiledUploadKey,
    pub sdr: CallbackUpload,
    pub gain: CallbackUpload,
    pub device_id: u64,
    pub staged_gpu_upload: bool,
}

pub(crate) struct HdrPendingIsoImageComposeRequest {
    pub key: HdrImageKey,
    pub target_capacity_bits: u32,
    pub target_format: wgpu::TextureFormat,
    pub image: Arc<HdrImageBuffer>,
    pub target_hdr_capacity: f32,
}

pub(crate) struct HdrPendingAppleImageComposeRequest {
    pub key: HdrImageKey,
    pub target_capacity_bits: u32,
    pub target_format: wgpu::TextureFormat,
    pub image: Arc<HdrImageBuffer>,
    pub target_hdr_capacity: f32,
}

pub(crate) struct HdrPendingIsoTileComposeRequest {
    pub tile_key: HdrTileKey,
    pub target_capacity_bits: u32,
    pub target_format: wgpu::TextureFormat,
    pub tile: Arc<crate::hdr::tiled::HdrTileBuffer>,
    pub tile_ctx: IsoDeferredTileContext,
    pub tile_width: u32,
    pub tile_height: u32,
    pub target_hdr_capacity: f32,
}

pub(crate) enum HdrCompletedComposeWrite {
    IsoImage {
        key: HdrImageKey,
        target_capacity_bits: u32,
        target_format: wgpu::TextureFormat,
        width: u32,
        height: u32,
        pixels: Vec<f32>,
    },
    AppleImage {
        key: HdrImageKey,
        target_capacity_bits: u32,
        target_format: wgpu::TextureFormat,
        width: u32,
        height: u32,
        pixels: Vec<f32>,
    },
    IsoTile {
        tile_key: HdrTileKey,
        target_capacity_bits: u32,
        target_format: wgpu::TextureFormat,
        width: u32,
        height: u32,
        pixels: Vec<f32>,
    },
}

pub(crate) enum HdrCompletedComposeFailure {
    IsoImage {
        key: HdrImageKey,
        target_capacity_bits: u32,
        target_format: wgpu::TextureFormat,
    },
    AppleImage {
        key: HdrImageKey,
        target_capacity_bits: u32,
        target_format: wgpu::TextureFormat,
    },
    IsoTile {
        tile_key: HdrTileKey,
        target_capacity_bits: u32,
        target_format: wgpu::TextureFormat,
    },
}

pub(crate) struct HdrPendingWorkQueues {
    inflight: Mutex<HdrPendingWorkInflight>,
    loader_plane_upload_inflight: AtomicUsize,
    active_work_count: AtomicUsize,
    pub(crate) gpu_writes: Mutex<HdrPendingGpuWriteQueues>,
    pub(crate) plane_upload_requests: Mutex<Vec<HdrPendingPlaneUploadRequest>>,
    pub(crate) completed_plane_uploads: Mutex<Vec<HdrCompletedPlaneUpload>>,
    pub(crate) tile_upload_requests: Mutex<Vec<HdrPendingTileUploadRequest>>,
    pub(crate) completed_tile_uploads: Mutex<Vec<HdrCompletedTileUpload>>,
    pub(crate) jpeg_tiled_source_requests: Mutex<Vec<HdrPendingJpegTiledSourceUploadRequest>>,
    pub(crate) completed_jpeg_tiled_source_uploads: Mutex<Vec<HdrCompletedJpegTiledSourceUpload>>,
    pub(crate) iso_image_compose_requests: Mutex<Vec<HdrPendingIsoImageComposeRequest>>,
    pub(crate) apple_image_compose_requests: Mutex<Vec<HdrPendingAppleImageComposeRequest>>,
    pub(crate) iso_tile_compose_requests: Mutex<Vec<HdrPendingIsoTileComposeRequest>>,
    pub(crate) completed_compose_writes: Mutex<Vec<HdrCompletedComposeWrite>>,
    pub(crate) completed_compose_failures: Mutex<Vec<HdrCompletedComposeFailure>>,
}

impl HdrPendingWorkQueues {
    pub(crate) fn new_shared() -> Arc<Self> {
        Arc::new(Self {
            inflight: Mutex::new(HdrPendingWorkInflight::default()),
            loader_plane_upload_inflight: AtomicUsize::new(0),
            active_work_count: AtomicUsize::new(0),
            gpu_writes: Mutex::new(HdrPendingGpuWriteQueues::default()),
            plane_upload_requests: Mutex::new(Vec::new()),
            completed_plane_uploads: Mutex::new(Vec::new()),
            tile_upload_requests: Mutex::new(Vec::new()),
            completed_tile_uploads: Mutex::new(Vec::new()),
            jpeg_tiled_source_requests: Mutex::new(Vec::new()),
            completed_jpeg_tiled_source_uploads: Mutex::new(Vec::new()),
            iso_image_compose_requests: Mutex::new(Vec::new()),
            apple_image_compose_requests: Mutex::new(Vec::new()),
            iso_tile_compose_requests: Mutex::new(Vec::new()),
            completed_compose_writes: Mutex::new(Vec::new()),
            completed_compose_failures: Mutex::new(Vec::new()),
        })
    }

    pub(crate) fn flush_gpu_writes(&self, queue: &wgpu::Queue, quota: usize) -> usize {
        let flushed = self.gpu_writes.lock().flush(queue, quota);
        if flushed > 0 {
            self.note_work_finished(flushed);
        }
        flushed
    }

    pub(crate) fn has_pending_gpu_writes(&self) -> bool {
        self.gpu_writes.lock().pending_len() > 0
    }

    /// Drain staged GPU writes before binding pre-uploaded planes on the main thread.
    pub(crate) fn flush_staged_writes_for_registration(&self, queue: &wgpu::Queue) -> bool {
        const MAX_ROUNDS: usize = 32;
        for _ in 0..MAX_ROUNDS {
            if self.gpu_writes.lock().pending_len() == 0 {
                return true;
            }
            let flushed = self.flush_gpu_writes(queue, MAX_HDR_GPU_WRITES_PER_LOGIC);
            if flushed == 0 {
                return false;
            }
        }
        self.gpu_writes.lock().pending_len() == 0
    }

    pub(crate) fn try_begin_loader_plane_upload(&self) -> bool {
        loop {
            let current = self.loader_plane_upload_inflight.load(Ordering::Acquire);
            if current >= MAX_HDR_LOADER_PLANE_UPLOADS_INFLIGHT {
                return false;
            }
            if self
                .loader_plane_upload_inflight
                .compare_exchange_weak(current, current + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return true;
            }
        }
    }

    pub(crate) fn finish_loader_plane_upload(&self) {
        self.loader_plane_upload_inflight
            .fetch_sub(1, Ordering::AcqRel);
    }

    pub(crate) fn bump_active_work(&self, delta: isize) {
        if delta > 0 {
            self.active_work_count
                .fetch_add(delta as usize, Ordering::Release);
        } else if delta < 0 {
            self.active_work_count
                .fetch_sub((-delta) as usize, Ordering::Release);
        }
    }

    pub(crate) fn note_work_finished(&self, count: usize) {
        if count > 0 {
            self.bump_active_work(-(count as isize));
        }
    }

    pub(crate) fn has_active_work(&self) -> bool {
        if self.active_work_count.load(Ordering::Acquire) > 0 {
            return true;
        }
        if self.gpu_writes.lock().pending_len() > 0 {
            return true;
        }
        let inflight = self.inflight.lock();
        if !inflight.plane_uploads.is_empty()
            || !inflight.tile_uploads.is_empty()
            || !inflight.jpeg_tiled_source_uploads.is_empty()
            || !inflight.iso_image_compose.is_empty()
            || !inflight.apple_image_compose.is_empty()
            || !inflight.iso_tile_compose.is_empty()
        {
            return true;
        }
        drop(inflight);
        self.has_pending_completed_work() || self.has_pending_request_work()
    }

    fn has_pending_completed_work(&self) -> bool {
        !self.completed_plane_uploads.lock().is_empty()
            || !self.completed_tile_uploads.lock().is_empty()
            || !self.completed_jpeg_tiled_source_uploads.lock().is_empty()
            || !self.completed_compose_writes.lock().is_empty()
            || !self.completed_compose_failures.lock().is_empty()
    }

    fn has_pending_request_work(&self) -> bool {
        !self.plane_upload_requests.lock().is_empty()
            || !self.tile_upload_requests.lock().is_empty()
            || !self.jpeg_tiled_source_requests.lock().is_empty()
            || !self.iso_image_compose_requests.lock().is_empty()
            || !self.apple_image_compose_requests.lock().is_empty()
            || !self.iso_tile_compose_requests.lock().is_empty()
    }

    pub(crate) fn try_queue_plane_upload(&self, request: HdrPendingPlaneUploadRequest) -> bool {
        let mut inflight = self.inflight.lock();
        if inflight.plane_uploads.contains(&request.key) {
            return false;
        }
        inflight.plane_uploads.insert(request.key);
        self.plane_upload_requests.lock().push(request);
        self.bump_active_work(1);
        true
    }

    pub(crate) fn try_queue_tile_upload(&self, request: HdrPendingTileUploadRequest) -> bool {
        let mut inflight = self.inflight.lock();
        if inflight.tile_uploads.contains(&request.tile_key) {
            return false;
        }
        inflight.tile_uploads.insert(request.tile_key);
        self.tile_upload_requests.lock().push(request);
        self.bump_active_work(1);
        true
    }

    pub(crate) fn try_queue_jpeg_tiled_source_upload(
        &self,
        request: HdrPendingJpegTiledSourceUploadRequest,
    ) -> bool {
        let key = (request.upload_key, request.target_format);
        let mut inflight = self.inflight.lock();
        if inflight.jpeg_tiled_source_uploads.contains(&key) {
            return false;
        }
        inflight.jpeg_tiled_source_uploads.insert(key);
        self.jpeg_tiled_source_requests.lock().push(request);
        self.bump_active_work(1);
        true
    }

    pub(crate) fn try_queue_iso_image_compose(
        &self,
        request: HdrPendingIsoImageComposeRequest,
    ) -> bool {
        let key = (request.key, request.target_capacity_bits);
        let mut inflight = self.inflight.lock();
        if inflight.iso_image_compose.contains(&key) {
            return false;
        }
        inflight.iso_image_compose.insert(key);
        self.iso_image_compose_requests.lock().push(request);
        self.bump_active_work(1);
        true
    }

    pub(crate) fn try_queue_apple_image_compose(
        &self,
        request: HdrPendingAppleImageComposeRequest,
    ) -> bool {
        let key = (request.key, request.target_capacity_bits);
        let mut inflight = self.inflight.lock();
        if inflight.apple_image_compose.contains(&key) {
            return false;
        }
        inflight.apple_image_compose.insert(key);
        self.apple_image_compose_requests.lock().push(request);
        self.bump_active_work(1);
        true
    }

    pub(crate) fn try_queue_iso_tile_compose(
        &self,
        request: HdrPendingIsoTileComposeRequest,
    ) -> bool {
        let key = (request.tile_key, request.target_capacity_bits);
        let mut inflight = self.inflight.lock();
        if inflight.iso_tile_compose.contains(&key) {
            return false;
        }
        inflight.iso_tile_compose.insert(key);
        self.iso_tile_compose_requests.lock().push(request);
        self.bump_active_work(1);
        true
    }

    pub(crate) fn clear_plane_upload_inflight(&self, key: HdrImageKey) {
        if self.inflight.lock().plane_uploads.remove(&key) {
            self.note_work_finished(1);
        }
    }

    pub(crate) fn clear_tile_upload_inflight(&self, key: HdrTileKey) {
        if self.inflight.lock().tile_uploads.remove(&key) {
            self.note_work_finished(1);
        }
    }

    pub(crate) fn clear_jpeg_tiled_source_upload_inflight(
        &self,
        upload_key: JpegTiledUploadKey,
        target_format: wgpu::TextureFormat,
    ) {
        if self
            .inflight
            .lock()
            .jpeg_tiled_source_uploads
            .remove(&(upload_key, target_format))
        {
            self.note_work_finished(1);
        }
    }

    pub(crate) fn clear_iso_image_compose_inflight(
        &self,
        key: HdrImageKey,
        target_capacity_bits: u32,
    ) {
        if self
            .inflight
            .lock()
            .iso_image_compose
            .remove(&(key, target_capacity_bits))
        {
            self.note_work_finished(1);
        }
    }

    pub(crate) fn clear_apple_image_compose_inflight(
        &self,
        key: HdrImageKey,
        target_capacity_bits: u32,
    ) {
        if self
            .inflight
            .lock()
            .apple_image_compose
            .remove(&(key, target_capacity_bits))
        {
            self.note_work_finished(1);
        }
    }

    pub(crate) fn clear_iso_tile_compose_inflight(
        &self,
        tile_key: HdrTileKey,
        target_capacity_bits: u32,
    ) {
        if self
            .inflight
            .lock()
            .iso_tile_compose
            .remove(&(tile_key, target_capacity_bits))
        {
            self.note_work_finished(1);
        }
    }
}
