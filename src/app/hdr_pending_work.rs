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

use super::hdr_pending_dispatch::{
    HdrCompletedRegisterOutcome, apply_hdr_completed_batch, dispatch_hdr_cpu_compose_batch,
    dispatch_hdr_gpu_upload_batch,
};
use super::types::ImageViewerApp;
#[cfg(feature = "heif-native")]
use crate::hdr::heif_apple_gain_map_gpu::compose_apple_heic_deferred_cpu_pixels;
use crate::hdr::jpeg_gain_map_gpu::iso_deferred_from_metadata;
use crate::hdr::renderer::{
    GpuUploadSink, HdrCallbackResourcesReadiness, HdrCompletedComposeFailure,
    HdrCompletedComposeWrite, HdrCompletedJpegTiledSourceUpload, HdrCompletedPlaneUpload,
    HdrCompletedTileUpload, HdrGpuUploadStage, HdrImageBinding, HdrPendingAppleImageComposeRequest,
    HdrPendingIsoImageComposeRequest, HdrPendingIsoTileComposeRequest,
    HdrPendingJpegTiledSourceUploadRequest, HdrPendingPlaneUploadRequest,
    HdrPendingTileUploadRequest, MAX_HDR_CPU_COMPOSE_STARTS_PER_LOGIC,
    MAX_HDR_GPU_WRITES_PER_LOGIC, MAX_HDR_JPEG_TILED_SOURCE_UPLOADS_PER_LOGIC,
    MAX_HDR_PLANE_UPLOADS_PER_LOGIC, MAX_HDR_TILE_UPLOADS_PER_LOGIC, ensure_hdr_callback_resources,
    hdr_callback_resources_readiness, pending_gpu_write_queue_full_err, upload_callback_tile,
    upload_image_plane_with_sink, upload_jpeg_tiled_source_textures,
};
use crate::loader::REFINEMENT_POOL;
use eframe::egui;
use std::sync::Arc;

impl ImageViewerApp {
    pub(crate) fn process_hdr_pending_work(
        &mut self,
        _ctx: &egui::Context,
        frame: &mut eframe::Frame,
    ) -> bool {
        let had_active = self.hdr_pending_work.has_active_work();
        self.flush_hdr_pending_gpu_writes(frame);
        self.start_pending_hdr_cpu_compose_jobs();
        self.start_pending_hdr_plane_upload_jobs(frame);
        self.start_pending_hdr_tile_upload_jobs(frame);
        self.start_pending_hdr_jpeg_tiled_source_upload_jobs(frame);
        let applied = self.apply_completed_hdr_pending_work(frame);
        if self.hdr_pending_work.has_pending_gpu_writes() {
            self.flush_hdr_pending_gpu_writes(frame);
        }
        let still_active = self.hdr_pending_work.has_active_work();
        had_active || applied || still_active
    }

    fn flush_hdr_pending_gpu_writes(&mut self, frame: &mut eframe::Frame) {
        let Some(wgpu_state) = frame.wgpu_render_state() else {
            return;
        };
        let flushed = self
            .hdr_pending_work
            .flush_gpu_writes(&wgpu_state.queue, MAX_HDR_GPU_WRITES_PER_LOGIC);
        #[cfg(feature = "preload-debug")]
        if flushed > 0 {
            crate::preload_debug!("[PreloadDebug][HDR-GPU] flushed {flushed} staged write(s)");
        }
        #[cfg(not(feature = "preload-debug"))]
        let _ = flushed;
    }

    fn wgpu_is_opengl_backend(&self) -> bool {
        self.wgpu_adapter_info
            .as_ref()
            .is_some_and(|info| info.backend == wgpu::Backend::Gl)
    }

    fn start_pending_hdr_plane_upload_jobs(&mut self, frame: &mut eframe::Frame) {
        let requests: Vec<HdrPendingPlaneUploadRequest> =
            std::mem::take(&mut *self.hdr_pending_work.plane_upload_requests.lock());
        if requests.is_empty() {
            return;
        }

        let requeue = dispatch_hdr_gpu_upload_batch(
            requests,
            frame.wgpu_render_state(),
            self.wgpu_is_opengl_backend(),
            self.current_device_id,
            Arc::clone(&self.hdr_pending_work),
            MAX_HDR_PLANE_UPLOADS_PER_LOGIC,
            Self::finish_plane_upload,
        );
        if !requeue.is_empty() {
            self.hdr_pending_work
                .plane_upload_requests
                .lock()
                .extend(requeue);
        }
    }

    fn finish_plane_upload(
        completed: &Arc<crate::hdr::renderer::HdrPendingWorkQueues>,
        device: &wgpu::Device,
        request: HdrPendingPlaneUploadRequest,
        device_id: u64,
    ) {
        let key = request.key;
        match upload_image_plane_with_sink(
            device,
            GpuUploadSink::Pending {
                queues: completed.as_ref(),
                stage: HdrGpuUploadStage::PlaneCreate,
            },
            &request.image,
            None,
        ) {
            Ok(uploaded) => {
                completed
                    .completed_plane_uploads
                    .lock()
                    .push(HdrCompletedPlaneUpload {
                        key,
                        uploaded,
                        image: request.image,
                        target_format: request.target_format,
                        tone_map: request.tone_map,
                        output_mode: request.output_mode,
                        keep_resident: request.keep_resident,
                        device_id,
                    });
            }
            Err(err) => {
                if pending_gpu_write_queue_full_err(&err) {
                    log::debug!("[HDR] Background plane upload deferred: {err}");
                } else {
                    log::warn!("[HDR] Background plane upload failed: {err}");
                }
                completed.clear_plane_upload_inflight(key);
            }
        }
    }

    fn start_pending_hdr_tile_upload_jobs(&mut self, frame: &mut eframe::Frame) {
        let requests: Vec<HdrPendingTileUploadRequest> =
            std::mem::take(&mut *self.hdr_pending_work.tile_upload_requests.lock());
        if requests.is_empty() {
            return;
        }

        let requeue = dispatch_hdr_gpu_upload_batch(
            requests,
            frame.wgpu_render_state(),
            self.wgpu_is_opengl_backend(),
            self.current_device_id,
            Arc::clone(&self.hdr_pending_work),
            MAX_HDR_TILE_UPLOADS_PER_LOGIC,
            Self::finish_tile_upload,
        );
        if !requeue.is_empty() {
            self.hdr_pending_work
                .tile_upload_requests
                .lock()
                .extend(requeue);
        }
    }

    fn finish_tile_upload(
        completed: &Arc<crate::hdr::renderer::HdrPendingWorkQueues>,
        device: &wgpu::Device,
        request: HdrPendingTileUploadRequest,
        device_id: u64,
    ) {
        let tile_key = request.tile_key;
        let sink = GpuUploadSink::Pending {
            queues: completed.as_ref(),
            stage: HdrGpuUploadStage::TileCreate,
        };
        match upload_callback_tile(device, sink, &request.tile, None) {
            Ok(uploaded) => {
                completed
                    .completed_tile_uploads
                    .lock()
                    .push(HdrCompletedTileUpload {
                        tile_key,
                        tile: request.tile,
                        target_format: request.target_format,
                        uploaded,
                        tone_map: request.tone_map,
                        output_mode: request.output_mode,
                        rotation_steps: request.rotation_steps,
                        alpha: request.alpha,
                        uv_rect: request.uv_rect,
                        device_id,
                        staged_gpu_upload: true,
                    });
            }
            Err(err) => {
                if pending_gpu_write_queue_full_err(&err) {
                    log::debug!("[HDR] Background tile upload deferred: {err}");
                } else {
                    log::warn!("[HDR] Background tile upload failed: {err}");
                }
                completed.clear_tile_upload_inflight(tile_key);
            }
        }
    }

    fn start_pending_hdr_jpeg_tiled_source_upload_jobs(&mut self, frame: &mut eframe::Frame) {
        let requests: Vec<HdrPendingJpegTiledSourceUploadRequest> =
            std::mem::take(&mut *self.hdr_pending_work.jpeg_tiled_source_requests.lock());
        if requests.is_empty() {
            return;
        }

        let requeue = dispatch_hdr_gpu_upload_batch(
            requests,
            frame.wgpu_render_state(),
            self.wgpu_is_opengl_backend(),
            self.current_device_id,
            Arc::clone(&self.hdr_pending_work),
            MAX_HDR_JPEG_TILED_SOURCE_UPLOADS_PER_LOGIC,
            Self::finish_jpeg_tiled_source_upload,
        );
        if !requeue.is_empty() {
            self.hdr_pending_work
                .jpeg_tiled_source_requests
                .lock()
                .extend(requeue);
        }
    }

    fn finish_jpeg_tiled_source_upload(
        completed: &Arc<crate::hdr::renderer::HdrPendingWorkQueues>,
        device: &wgpu::Device,
        request: HdrPendingJpegTiledSourceUploadRequest,
        device_id: u64,
    ) {
        let upload_key = request.upload_key;
        let target_format = request.target_format;
        let sink = GpuUploadSink::Pending {
            queues: completed.as_ref(),
            stage: HdrGpuUploadStage::AuxRgba8,
        };
        match upload_jpeg_tiled_source_textures(
            device,
            sink,
            &request.deferred,
            request.physical_width,
            request.physical_height,
            device.limits().max_texture_dimension_2d,
            None,
        ) {
            Ok((sdr, gain)) => {
                completed.completed_jpeg_tiled_source_uploads.lock().push(
                    HdrCompletedJpegTiledSourceUpload {
                        target_format,
                        upload_key,
                        sdr,
                        gain,
                        device_id,
                        staged_gpu_upload: true,
                    },
                );
            }
            Err(err) => {
                if pending_gpu_write_queue_full_err(&err) {
                    log::debug!("[HDR] Background JPEG tiled source upload deferred: {err}");
                } else {
                    log::warn!("[HDR] Background JPEG tiled source upload failed: {err}");
                }
                completed.clear_jpeg_tiled_source_upload_inflight(upload_key, target_format);
            }
        }
    }

    fn start_pending_hdr_cpu_compose_jobs(&mut self) {
        let mut started = 0usize;
        started += self.start_pending_iso_image_compose_jobs(started);
        #[cfg(feature = "heif-native")]
        {
            started += self.start_pending_apple_image_compose_jobs(started);
        }
        let _ = self.start_pending_iso_tile_compose_jobs(started);
    }

    fn start_pending_iso_image_compose_jobs(&mut self, started: usize) -> usize {
        let queues = Arc::clone(&self.hdr_pending_work);
        let requests: Vec<HdrPendingIsoImageComposeRequest> =
            std::mem::take(&mut *self.hdr_pending_work.iso_image_compose_requests.lock());
        if requests.is_empty() {
            return 0;
        }

        let mut started = started;
        let ran_before = started;
        let requeue = dispatch_hdr_cpu_compose_batch(
            requests,
            &mut started,
            MAX_HDR_CPU_COMPOSE_STARTS_PER_LOGIC,
            |request| {
                let queues = Arc::clone(&queues);
                REFINEMENT_POOL.spawn(move || {
                    let key = request.key;
                    let bits = request.target_capacity_bits;
                    let format = request.target_format;
                    let width = request.image.width;
                    let height = request.image.height;
                    let capacity = request.target_hdr_capacity;
                    let result = iso_deferred_from_metadata(&request.image.metadata)
                        .ok_or_else(|| "ISO deferred metadata missing".to_string())
                        .and_then(|deferred| {
                            crate::hdr::jpeg_gain_map_gpu::compose_iso_deferred_cpu_pixels(
                                width, height, deferred, capacity,
                            )
                        });
                    match result {
                        Ok(pixels) => {
                            queues.completed_compose_writes.lock().push(
                                HdrCompletedComposeWrite::IsoImage {
                                    key,
                                    target_capacity_bits: bits,
                                    target_format: format,
                                    width,
                                    height,
                                    pixels,
                                },
                            );
                        }
                        Err(err) => {
                            log::warn!("[HDR] ISO CPU compose failed: {err}");
                            queues.completed_compose_failures.lock().push(
                                HdrCompletedComposeFailure::IsoImage {
                                    key,
                                    target_capacity_bits: bits,
                                    target_format: format,
                                },
                            );
                        }
                    }
                });
            },
        );
        if !requeue.is_empty() {
            self.hdr_pending_work
                .iso_image_compose_requests
                .lock()
                .extend(requeue);
        }
        started - ran_before
    }

    #[cfg(feature = "heif-native")]
    fn start_pending_apple_image_compose_jobs(&mut self, started: usize) -> usize {
        let queues = Arc::clone(&self.hdr_pending_work);
        let requests: Vec<HdrPendingAppleImageComposeRequest> =
            std::mem::take(&mut *self.hdr_pending_work.apple_image_compose_requests.lock());
        if requests.is_empty() {
            return 0;
        }

        let mut started = started;
        let ran_before = started;
        let requeue = dispatch_hdr_cpu_compose_batch(
            requests,
            &mut started,
            MAX_HDR_CPU_COMPOSE_STARTS_PER_LOGIC,
            |request| {
                let queues = Arc::clone(&queues);
                REFINEMENT_POOL.spawn(move || {
                    let key = request.key;
                    let bits = request.target_capacity_bits;
                    let format = request.target_format;
                    let width = request.image.width;
                    let height = request.image.height;
                    match compose_apple_heic_deferred_cpu_pixels(
                        &request.image,
                        request.target_hdr_capacity,
                    ) {
                        Ok(pixels) => {
                            queues.completed_compose_writes.lock().push(
                                HdrCompletedComposeWrite::AppleImage {
                                    key,
                                    target_capacity_bits: bits,
                                    target_format: format,
                                    width,
                                    height,
                                    pixels,
                                },
                            );
                        }
                        Err(err) => {
                            log::warn!("[HDR] Apple CPU compose failed: {err}");
                            queues.completed_compose_failures.lock().push(
                                HdrCompletedComposeFailure::AppleImage {
                                    key,
                                    target_capacity_bits: bits,
                                    target_format: format,
                                },
                            );
                        }
                    }
                });
            },
        );
        if !requeue.is_empty() {
            self.hdr_pending_work
                .apple_image_compose_requests
                .lock()
                .extend(requeue);
        }
        started - ran_before
    }

    fn start_pending_iso_tile_compose_jobs(&mut self, started: usize) -> usize {
        let queues = Arc::clone(&self.hdr_pending_work);
        let requests: Vec<HdrPendingIsoTileComposeRequest> =
            std::mem::take(&mut *self.hdr_pending_work.iso_tile_compose_requests.lock());
        if requests.is_empty() {
            return 0;
        }

        let mut started = started;
        let ran_before = started;
        let requeue = dispatch_hdr_cpu_compose_batch(
            requests,
            &mut started,
            MAX_HDR_CPU_COMPOSE_STARTS_PER_LOGIC,
            |request| {
                let queues = Arc::clone(&queues);
                REFINEMENT_POOL.spawn(move || {
                    let tile_key = request.tile_key;
                    let bits = request.target_capacity_bits;
                    let format = request.target_format;
                    let width = request.tile_width;
                    let height = request.tile_height;
                    let capacity = request.target_hdr_capacity;
                    let ctx = request.tile_ctx;
                    let result = iso_deferred_from_metadata(&request.tile.metadata)
                        .ok_or_else(|| "ISO deferred metadata missing".to_string())
                        .and_then(|deferred| {
                            crate::hdr::jpeg_gain_map_gpu::compose_iso_deferred_tile_cpu_pixels(
                                deferred, &ctx, width, height, capacity,
                            )
                        });
                    match result {
                        Ok(pixels) => {
                            queues.completed_compose_writes.lock().push(
                                HdrCompletedComposeWrite::IsoTile {
                                    tile_key,
                                    target_capacity_bits: bits,
                                    target_format: format,
                                    width,
                                    height,
                                    pixels,
                                },
                            );
                        }
                        Err(err) => {
                            log::warn!("[HDR] ISO tile CPU compose failed: {err}");
                            queues.completed_compose_failures.lock().push(
                                HdrCompletedComposeFailure::IsoTile {
                                    tile_key,
                                    target_capacity_bits: bits,
                                    target_format: format,
                                },
                            );
                        }
                    }
                });
            },
        );
        if !requeue.is_empty() {
            self.hdr_pending_work
                .iso_tile_compose_requests
                .lock()
                .extend(requeue);
        }
        started - ran_before
    }

    fn apply_completed_hdr_pending_work(&mut self, frame: &mut eframe::Frame) -> bool {
        self.register_completed_hdr_plane_uploads(frame)
            | self.register_completed_hdr_tile_uploads(frame)
            | self.register_completed_hdr_jpeg_tiled_source_uploads(frame)
            | self.apply_completed_hdr_compose_writes(frame)
            | self.apply_completed_hdr_compose_failures(frame)
    }

    fn ensure_hdr_resources(
        wgpu_state: &eframe::egui_wgpu::RenderState,
        format: wgpu::TextureFormat,
    ) -> bool {
        let readiness = {
            let renderer = wgpu_state.renderer.read();
            hdr_callback_resources_readiness(&renderer.callback_resources, format)
        };
        if matches!(readiness, HdrCallbackResourcesReadiness::PrewarmRunning) {
            return false;
        }
        if matches!(readiness, HdrCallbackResourcesReadiness::NeedsEnsure) {
            let mut renderer = wgpu_state.renderer.write();
            if !ensure_hdr_callback_resources(
                &wgpu_state.device,
                format,
                &mut renderer.callback_resources,
            ) {
                return false;
            }
        }
        true
    }

    fn register_completed_hdr_plane_uploads(&mut self, frame: &mut eframe::Frame) -> bool {
        let completed: Vec<HdrCompletedPlaneUpload> =
            std::mem::take(&mut *self.hdr_pending_work.completed_plane_uploads.lock());
        if completed.is_empty() {
            return false;
        }

        let device_id = self.current_device_id;
        let hdr_pending_work = Arc::clone(&self.hdr_pending_work);
        apply_hdr_completed_batch(
            frame.wgpu_render_state(),
            completed,
            |items| {
                *hdr_pending_work.completed_plane_uploads.lock() = items;
            },
            |defer| {
                hdr_pending_work
                    .completed_plane_uploads
                    .lock()
                    .extend(defer);
            },
            |wgpu_state, item| {
                if item.device_id != device_id {
                    hdr_pending_work.clear_plane_upload_inflight(item.key);
                    return HdrCompletedRegisterOutcome::Skipped;
                }
                if !Self::ensure_hdr_resources(wgpu_state, item.target_format) {
                    return HdrCompletedRegisterOutcome::Deferred(item);
                }
                if !hdr_pending_work.flush_staged_writes_for_registration(&wgpu_state.queue) {
                    return HdrCompletedRegisterOutcome::Deferred(item);
                }

                let binding = HdrImageBinding::from_uploaded(
                    &wgpu_state.device,
                    item.uploaded,
                    &item.image,
                    item.tone_map,
                    item.target_format,
                    item.output_mode,
                    item.device_id,
                );
                let mut renderer = wgpu_state.renderer.write();
                let mut applied = false;
                if let Some(resources) = renderer
                    .callback_resources
                    .get_mut::<crate::hdr::renderer::HdrCallbackResourcesSet>()
                    .and_then(|set| set.get_for_mut(item.target_format))
                    && resources.register_preuploaded_binding(item.key, binding, device_id)
                {
                    resources.set_image_binding_keep_resident(item.key, item.keep_resident);
                    applied = true;
                }
                hdr_pending_work.clear_plane_upload_inflight(item.key);
                if applied {
                    HdrCompletedRegisterOutcome::Applied
                } else {
                    HdrCompletedRegisterOutcome::Skipped
                }
            },
        )
    }

    fn register_completed_hdr_tile_uploads(&mut self, frame: &mut eframe::Frame) -> bool {
        let completed: Vec<HdrCompletedTileUpload> =
            std::mem::take(&mut *self.hdr_pending_work.completed_tile_uploads.lock());
        if completed.is_empty() {
            return false;
        }

        let device_id = self.current_device_id;
        let hdr_pending_work = Arc::clone(&self.hdr_pending_work);
        apply_hdr_completed_batch(
            frame.wgpu_render_state(),
            completed,
            |items| {
                *hdr_pending_work.completed_tile_uploads.lock() = items;
            },
            |defer| {
                hdr_pending_work.completed_tile_uploads.lock().extend(defer);
            },
            |wgpu_state, item| {
                if item.device_id != device_id {
                    hdr_pending_work.clear_tile_upload_inflight(item.tile_key);
                    return HdrCompletedRegisterOutcome::Skipped;
                }
                if !Self::ensure_hdr_resources(wgpu_state, item.target_format) {
                    return HdrCompletedRegisterOutcome::Deferred(item);
                }
                if item.staged_gpu_upload
                    && !hdr_pending_work.flush_staged_writes_for_registration(&wgpu_state.queue)
                {
                    return HdrCompletedRegisterOutcome::Deferred(item);
                }

                let mut renderer = wgpu_state.renderer.write();
                let tile_key = item.tile_key;
                let applied = renderer
                    .callback_resources
                    .get_mut::<crate::hdr::renderer::HdrCallbackResourcesSet>()
                    .and_then(|set| set.get_for_mut(item.target_format))
                    .is_some_and(|resources| {
                        resources.register_completed_tile_upload(&wgpu_state.device, item)
                    });
                hdr_pending_work.clear_tile_upload_inflight(tile_key);
                if applied {
                    HdrCompletedRegisterOutcome::Applied
                } else {
                    HdrCompletedRegisterOutcome::Skipped
                }
            },
        )
    }

    fn register_completed_hdr_jpeg_tiled_source_uploads(
        &mut self,
        frame: &mut eframe::Frame,
    ) -> bool {
        let completed: Vec<HdrCompletedJpegTiledSourceUpload> = std::mem::take(
            &mut *self
                .hdr_pending_work
                .completed_jpeg_tiled_source_uploads
                .lock(),
        );
        if completed.is_empty() {
            return false;
        }

        let device_id = self.current_device_id;
        let hdr_pending_work = Arc::clone(&self.hdr_pending_work);
        apply_hdr_completed_batch(
            frame.wgpu_render_state(),
            completed,
            |items| {
                *hdr_pending_work.completed_jpeg_tiled_source_uploads.lock() = items;
            },
            |defer| {
                hdr_pending_work
                    .completed_jpeg_tiled_source_uploads
                    .lock()
                    .extend(defer);
            },
            |wgpu_state, item| {
                if item.device_id != device_id {
                    hdr_pending_work.clear_jpeg_tiled_source_upload_inflight(
                        item.upload_key,
                        item.target_format,
                    );
                    return HdrCompletedRegisterOutcome::Skipped;
                }
                if !Self::ensure_hdr_resources(wgpu_state, item.target_format) {
                    return HdrCompletedRegisterOutcome::Deferred(item);
                }
                if item.staged_gpu_upload
                    && !hdr_pending_work.flush_staged_writes_for_registration(&wgpu_state.queue)
                {
                    return HdrCompletedRegisterOutcome::Deferred(item);
                }

                let mut renderer = wgpu_state.renderer.write();
                let mut applied = false;
                if let Some(resources) = renderer
                    .callback_resources
                    .get_mut::<crate::hdr::renderer::HdrCallbackResourcesSet>()
                    .and_then(|set| set.get_for_mut(item.target_format))
                {
                    resources.register_jpeg_tiled_source_upload(
                        item.upload_key,
                        item.sdr,
                        item.gain,
                    );
                    applied = true;
                }
                hdr_pending_work
                    .clear_jpeg_tiled_source_upload_inflight(item.upload_key, item.target_format);
                if applied {
                    HdrCompletedRegisterOutcome::Applied
                } else {
                    HdrCompletedRegisterOutcome::Skipped
                }
            },
        )
    }

    fn apply_completed_hdr_compose_writes(&mut self, frame: &mut eframe::Frame) -> bool {
        let writes: Vec<HdrCompletedComposeWrite> =
            std::mem::take(&mut *self.hdr_pending_work.completed_compose_writes.lock());
        if writes.is_empty() {
            return false;
        }

        let Some(wgpu_state) = frame.wgpu_render_state() else {
            *self.hdr_pending_work.completed_compose_writes.lock() = writes;
            return false;
        };

        let mut changed = false;
        let mut defer = Vec::new();
        let compose_sink = GpuUploadSink::Pending {
            queues: self.hdr_pending_work.as_ref(),
            stage: HdrGpuUploadStage::ComposeWrite,
        };
        for write in writes {
            let target_format = match &write {
                HdrCompletedComposeWrite::IsoImage { target_format, .. }
                | HdrCompletedComposeWrite::IsoTile { target_format, .. } => *target_format,
                #[cfg(feature = "heif-native")]
                HdrCompletedComposeWrite::AppleImage { target_format, .. } => *target_format,
                #[cfg(not(feature = "heif-native"))]
                HdrCompletedComposeWrite::AppleImage {
                    key,
                    target_capacity_bits,
                    ..
                } => {
                    self.hdr_pending_work
                        .clear_apple_image_compose_inflight(key, target_capacity_bits);
                    continue;
                }
            };
            if !Self::ensure_hdr_resources(wgpu_state, target_format) {
                defer.push(write);
                continue;
            }

            // Lock order: renderer -> gpu_writes (compose paths call submit_texture_write).
            // Do not acquire renderer while holding gpu_writes elsewhere.
            let mut renderer = wgpu_state.renderer.write();
            let Some(resources) = renderer
                .callback_resources
                .get_mut::<crate::hdr::renderer::HdrCallbackResourcesSet>()
                .and_then(|set| set.get_for_mut(target_format))
            else {
                defer.push(write);
                continue;
            };

            match write {
                HdrCompletedComposeWrite::IsoImage {
                    key,
                    target_capacity_bits,
                    width,
                    height,
                    pixels,
                    ..
                } => {
                    match resources.apply_iso_image_cpu_compose(
                        compose_sink,
                        key,
                        target_capacity_bits,
                        width,
                        height,
                        &pixels,
                    ) {
                        Ok(()) => {
                            changed = true;
                            self.hdr_pending_work
                                .clear_iso_image_compose_inflight(key, target_capacity_bits);
                        }
                        Err(err) if pending_gpu_write_queue_full_err(&err) => {
                            log::debug!("[HDR] ISO CPU compose deferred: {err}");
                            defer.push(HdrCompletedComposeWrite::IsoImage {
                                key,
                                target_capacity_bits,
                                target_format,
                                width,
                                height,
                                pixels,
                            });
                        }
                        Err(err) => {
                            log::warn!("[HDR] ISO CPU compose upload failed: {err}");
                            resources.mark_iso_image_compose_failed(key, target_capacity_bits);
                            self.hdr_pending_work
                                .clear_iso_image_compose_inflight(key, target_capacity_bits);
                        }
                    }
                }
                #[cfg(feature = "heif-native")]
                HdrCompletedComposeWrite::AppleImage {
                    key,
                    target_capacity_bits,
                    width,
                    height,
                    pixels,
                    ..
                } => {
                    match resources.apply_apple_image_cpu_compose(
                        compose_sink,
                        key,
                        target_capacity_bits,
                        width,
                        height,
                        &pixels,
                    ) {
                        Ok(()) => {
                            changed = true;
                            self.hdr_pending_work
                                .clear_apple_image_compose_inflight(key, target_capacity_bits);
                        }
                        Err(err) if pending_gpu_write_queue_full_err(&err) => {
                            log::debug!("[HDR] Apple CPU compose deferred: {err}");
                            defer.push(HdrCompletedComposeWrite::AppleImage {
                                key,
                                target_capacity_bits,
                                target_format,
                                width,
                                height,
                                pixels,
                            });
                        }
                        Err(err) => {
                            log::warn!("[HDR] Apple CPU compose upload failed: {err}");
                            resources.mark_apple_image_compose_failed(key, target_capacity_bits);
                            self.hdr_pending_work
                                .clear_apple_image_compose_inflight(key, target_capacity_bits);
                        }
                    }
                }
                HdrCompletedComposeWrite::IsoTile {
                    tile_key,
                    target_capacity_bits,
                    width,
                    height,
                    pixels,
                    ..
                } => {
                    match resources.apply_iso_tile_cpu_compose(
                        compose_sink,
                        tile_key,
                        target_capacity_bits,
                        width,
                        height,
                        &pixels,
                    ) {
                        Ok(()) => {
                            changed = true;
                            self.hdr_pending_work
                                .clear_iso_tile_compose_inflight(tile_key, target_capacity_bits);
                        }
                        Err(err) if pending_gpu_write_queue_full_err(&err) => {
                            log::debug!("[HDR] ISO tile CPU compose deferred: {err}");
                            defer.push(HdrCompletedComposeWrite::IsoTile {
                                tile_key,
                                target_capacity_bits,
                                target_format,
                                width,
                                height,
                                pixels,
                            });
                        }
                        Err(err) => {
                            log::warn!("[HDR] ISO tile CPU compose upload failed: {err}");
                            resources.mark_iso_tile_compose_failed(tile_key);
                            self.hdr_pending_work
                                .clear_iso_tile_compose_inflight(tile_key, target_capacity_bits);
                        }
                    }
                }
                #[cfg(not(feature = "heif-native"))]
                HdrCompletedComposeWrite::AppleImage { .. } => {}
            }
        }
        if !defer.is_empty() {
            self.hdr_pending_work
                .completed_compose_writes
                .lock()
                .extend(defer);
        }
        changed
    }

    fn apply_completed_hdr_compose_failures(&mut self, frame: &mut eframe::Frame) -> bool {
        let failures: Vec<HdrCompletedComposeFailure> =
            std::mem::take(&mut *self.hdr_pending_work.completed_compose_failures.lock());
        if failures.is_empty() {
            return false;
        }

        let Some(wgpu_state) = frame.wgpu_render_state() else {
            *self.hdr_pending_work.completed_compose_failures.lock() = failures;
            return false;
        };

        let mut changed = false;
        for failure in failures {
            let format = match &failure {
                HdrCompletedComposeFailure::IsoImage { target_format, .. }
                | HdrCompletedComposeFailure::IsoTile { target_format, .. } => *target_format,
                #[cfg(feature = "heif-native")]
                HdrCompletedComposeFailure::AppleImage { target_format, .. } => *target_format,
                #[cfg(not(feature = "heif-native"))]
                HdrCompletedComposeFailure::AppleImage {
                    key,
                    target_capacity_bits,
                    ..
                } => {
                    self.hdr_pending_work
                        .clear_apple_image_compose_inflight(key, target_capacity_bits);
                    continue;
                }
            };
            if !Self::ensure_hdr_resources(wgpu_state, format) {
                self.hdr_pending_work
                    .completed_compose_failures
                    .lock()
                    .push(failure);
                continue;
            }
            let mut renderer = wgpu_state.renderer.write();
            let Some(resources) = renderer
                .callback_resources
                .get_mut::<crate::hdr::renderer::HdrCallbackResourcesSet>()
                .and_then(|set| set.get_for_mut(format))
            else {
                continue;
            };
            match failure {
                HdrCompletedComposeFailure::IsoImage {
                    key,
                    target_capacity_bits,
                    ..
                } => {
                    resources.mark_iso_image_compose_failed(key, target_capacity_bits);
                    changed = true;
                    self.hdr_pending_work
                        .clear_iso_image_compose_inflight(key, target_capacity_bits);
                }
                #[cfg(feature = "heif-native")]
                HdrCompletedComposeFailure::AppleImage {
                    key,
                    target_capacity_bits,
                    ..
                } => {
                    resources.mark_apple_image_compose_failed(key, target_capacity_bits);
                    changed = true;
                    self.hdr_pending_work
                        .clear_apple_image_compose_inflight(key, target_capacity_bits);
                }
                HdrCompletedComposeFailure::IsoTile {
                    tile_key,
                    target_capacity_bits,
                    ..
                } => {
                    resources.mark_iso_tile_compose_failed(tile_key);
                    changed = true;
                    self.hdr_pending_work
                        .clear_iso_tile_compose_inflight(tile_key, target_capacity_bits);
                }
                #[cfg(not(feature = "heif-native"))]
                HdrCompletedComposeFailure::AppleImage { .. } => {}
            }
        }
        changed
    }
}
