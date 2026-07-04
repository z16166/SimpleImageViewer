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
use parking_lot::Mutex;
use std::collections::HashSet;
use std::sync::Arc;

/// Main-thread plane uploads per logic tick (OpenGL backend cannot upload off-thread).
pub(crate) const MAX_HDR_PLANE_UPLOADS_PER_LOGIC: usize = 1;

/// Background ISO/Apple CPU compose jobs started per logic tick.
pub(crate) const MAX_HDR_CPU_COMPOSE_STARTS_PER_LOGIC: usize = 2;

#[derive(Default)]
struct HdrPendingWorkInflight {
    plane_uploads: HashSet<HdrImageKey>,
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
        target_format: wgpu::TextureFormat,
    },
}

pub(crate) struct HdrPendingWorkQueues {
    inflight: Mutex<HdrPendingWorkInflight>,
    pub(crate) plane_upload_requests: Mutex<Vec<HdrPendingPlaneUploadRequest>>,
    pub(crate) completed_plane_uploads: Mutex<Vec<HdrCompletedPlaneUpload>>,
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
            plane_upload_requests: Mutex::new(Vec::new()),
            completed_plane_uploads: Mutex::new(Vec::new()),
            iso_image_compose_requests: Mutex::new(Vec::new()),
            apple_image_compose_requests: Mutex::new(Vec::new()),
            iso_tile_compose_requests: Mutex::new(Vec::new()),
            completed_compose_writes: Mutex::new(Vec::new()),
            completed_compose_failures: Mutex::new(Vec::new()),
        })
    }

    pub(crate) fn has_active_work(&self) -> bool {
        let inflight = self.inflight.lock();
        !inflight.plane_uploads.is_empty()
            || !inflight.iso_image_compose.is_empty()
            || !inflight.apple_image_compose.is_empty()
            || !inflight.iso_tile_compose.is_empty()
            || !self.plane_upload_requests.lock().is_empty()
            || !self.iso_image_compose_requests.lock().is_empty()
            || !self.apple_image_compose_requests.lock().is_empty()
            || !self.iso_tile_compose_requests.lock().is_empty()
            || !self.completed_plane_uploads.lock().is_empty()
            || !self.completed_compose_writes.lock().is_empty()
            || !self.completed_compose_failures.lock().is_empty()
    }

    pub(crate) fn try_queue_plane_upload(&self, request: HdrPendingPlaneUploadRequest) -> bool {
        let mut inflight = self.inflight.lock();
        if inflight.plane_uploads.contains(&request.key) {
            return false;
        }
        inflight.plane_uploads.insert(request.key);
        self.plane_upload_requests.lock().push(request);
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
        true
    }

    pub(crate) fn clear_plane_upload_inflight(&self, key: HdrImageKey) {
        self.inflight.lock().plane_uploads.remove(&key);
    }

    pub(crate) fn clear_iso_image_compose_inflight(&self, key: HdrImageKey, target_capacity_bits: u32) {
        self.inflight
            .lock()
            .iso_image_compose
            .remove(&(key, target_capacity_bits));
    }

    pub(crate) fn clear_apple_image_compose_inflight(
        &self,
        key: HdrImageKey,
        target_capacity_bits: u32,
    ) {
        self.inflight
            .lock()
            .apple_image_compose
            .remove(&(key, target_capacity_bits));
    }

    pub(crate) fn clear_iso_tile_compose_inflight(
        &self,
        tile_key: HdrTileKey,
        target_capacity_bits: u32,
    ) {
        self.inflight
            .lock()
            .iso_tile_compose
            .remove(&(tile_key, target_capacity_bits));
    }
}
