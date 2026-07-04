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

use super::types::ImageViewerApp;
use crate::hdr::jpeg_gain_map_gpu::iso_deferred_from_metadata;
#[cfg(feature = "heif-native")]
use crate::hdr::heif_apple_gain_map_gpu::compose_apple_heic_deferred_cpu_pixels;
use crate::hdr::renderer::{
    ensure_hdr_callback_resources, hdr_callback_resources_readiness, upload_image_plane,
    HdrCallbackResourcesReadiness, HdrCompletedComposeFailure, HdrCompletedComposeWrite,
    HdrCompletedPlaneUpload, HdrImageBinding, HdrPendingAppleImageComposeRequest,
    HdrPendingIsoImageComposeRequest, HdrPendingIsoTileComposeRequest, HdrPendingPlaneUploadRequest,
    MAX_HDR_CPU_COMPOSE_STARTS_PER_LOGIC, MAX_HDR_PLANE_UPLOADS_PER_LOGIC,
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
        self.start_pending_hdr_cpu_compose_jobs();
        self.start_pending_hdr_plane_upload_jobs(frame);
        let applied = self.apply_completed_hdr_pending_work(frame);
        let still_active = self.hdr_pending_work.has_active_work();
        had_active || applied || still_active
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

        let Some(wgpu_state) = frame.wgpu_render_state() else {
            *self.hdr_pending_work.plane_upload_requests.lock() = requests;
            return;
        };

        let device_id = self.current_device_id;
        let wgpu_is_opengl = self.wgpu_is_opengl_backend();
        let device = wgpu_state.device.clone();
        let queue = wgpu_state.queue.clone();
        let completed = Arc::clone(&self.hdr_pending_work);

        if wgpu_is_opengl {
            let (run_now, requeue): (Vec<_>, Vec<_>) = requests
                .into_iter()
                .enumerate()
                .partition(|(idx, _)| *idx < MAX_HDR_PLANE_UPLOADS_PER_LOGIC);
            for (_, request) in run_now {
                Self::finish_plane_upload(&completed, &device, &queue, request, device_id);
            }
            if !requeue.is_empty() {
                self.hdr_pending_work
                    .plane_upload_requests
                    .lock()
                    .extend(requeue.into_iter().map(|(_, request)| request));
            }
            return;
        }

        for request in requests {
            let completed = Arc::clone(&completed);
            let device = device.clone();
            let queue = queue.clone();
            REFINEMENT_POOL.spawn(move || {
                Self::finish_plane_upload(&completed, &device, &queue, request, device_id);
            });
        }
    }

    fn finish_plane_upload(
        completed: &Arc<crate::hdr::renderer::HdrPendingWorkQueues>,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        request: HdrPendingPlaneUploadRequest,
        device_id: u64,
    ) {
        let key = request.key;
        match upload_image_plane(device, queue, &request.image) {
            Ok(uploaded) => {
                completed.completed_plane_uploads.lock().push(HdrCompletedPlaneUpload {
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
                log::warn!("[HDR] Background plane upload failed: {err}");
                completed.clear_plane_upload_inflight(key);
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

        let mut ran = 0usize;
        let mut requeue = Vec::new();
        let mut started = started;
        for request in requests {
            if started >= MAX_HDR_CPU_COMPOSE_STARTS_PER_LOGIC {
                requeue.push(request);
                continue;
            }
            started += 1;
            ran += 1;
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
                        queues
                            .completed_compose_failures
                            .lock()
                            .push(HdrCompletedComposeFailure::IsoImage {
                                key,
                                target_capacity_bits: bits,
                                target_format: format,
                            });
                    }
                }
                queues.clear_iso_image_compose_inflight(key, bits);
            });
        }
        if !requeue.is_empty() {
            self.hdr_pending_work
                .iso_image_compose_requests
                .lock()
                .extend(requeue);
        }
        ran
    }

    #[cfg(feature = "heif-native")]
    fn start_pending_apple_image_compose_jobs(&mut self, started: usize) -> usize {
        let queues = Arc::clone(&self.hdr_pending_work);
        let requests: Vec<HdrPendingAppleImageComposeRequest> =
            std::mem::take(&mut *self.hdr_pending_work.apple_image_compose_requests.lock());
        if requests.is_empty() {
            return 0;
        }

        let mut ran = 0usize;
        let mut requeue = Vec::new();
        let mut started = started;
        for request in requests {
            if started >= MAX_HDR_CPU_COMPOSE_STARTS_PER_LOGIC {
                requeue.push(request);
                continue;
            }
            started += 1;
            ran += 1;
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
                        queues
                            .completed_compose_failures
                            .lock()
                            .push(HdrCompletedComposeFailure::AppleImage {
                                key,
                                target_capacity_bits: bits,
                                target_format: format,
                            });
                    }
                }
                queues.clear_apple_image_compose_inflight(key, bits);
            });
        }
        if !requeue.is_empty() {
            self.hdr_pending_work
                .apple_image_compose_requests
                .lock()
                .extend(requeue);
        }
        ran
    }

    fn start_pending_iso_tile_compose_jobs(&mut self, started: usize) -> usize {
        let queues = Arc::clone(&self.hdr_pending_work);
        let requests: Vec<HdrPendingIsoTileComposeRequest> =
            std::mem::take(&mut *self.hdr_pending_work.iso_tile_compose_requests.lock());
        if requests.is_empty() {
            return 0;
        }

        let mut ran = 0usize;
        let mut requeue = Vec::new();
        let mut started = started;
        for request in requests {
            if started >= MAX_HDR_CPU_COMPOSE_STARTS_PER_LOGIC {
                requeue.push(request);
                continue;
            }
            started += 1;
            ran += 1;
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
                            deferred,
                            &ctx,
                            width,
                            height,
                            capacity,
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
                        queues
                            .completed_compose_failures
                            .lock()
                            .push(HdrCompletedComposeFailure::IsoTile {
                                tile_key,
                                target_format: format,
                            });
                    }
                }
                queues.clear_iso_tile_compose_inflight(tile_key, bits);
            });
        }
        if !requeue.is_empty() {
            self.hdr_pending_work
                .iso_tile_compose_requests
                .lock()
                .extend(requeue);
        }
        ran
    }

    fn apply_completed_hdr_pending_work(&mut self, frame: &mut eframe::Frame) -> bool {
        self.register_completed_hdr_plane_uploads(frame)
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

        let Some(wgpu_state) = frame.wgpu_render_state() else {
            *self.hdr_pending_work.completed_plane_uploads.lock() = completed;
            return false;
        };

        let mut changed = false;
        let mut defer = Vec::new();
        for item in completed {
            if item.device_id != self.current_device_id {
                self.hdr_pending_work.clear_plane_upload_inflight(item.key);
                continue;
            }
            if !Self::ensure_hdr_resources(&wgpu_state, item.target_format) {
                defer.push(item);
                continue;
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
            if let Some(resources) = renderer
                .callback_resources
                .get_mut::<crate::hdr::renderer::HdrCallbackResourcesSet>()
                .and_then(|set| set.get_for_mut(item.target_format))
            {
                if resources.register_preuploaded_binding(item.key, binding, self.current_device_id)
                {
                    resources.set_image_binding_keep_resident(item.key, item.keep_resident);
                    changed = true;
                }
            }
            self.hdr_pending_work.clear_plane_upload_inflight(item.key);
        }
        if !defer.is_empty() {
            self.hdr_pending_work
                .completed_plane_uploads
                .lock()
                .extend(defer);
        }
        changed
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
        for write in writes {
            let target_format = match &write {
                HdrCompletedComposeWrite::IsoImage { target_format, .. }
                | HdrCompletedComposeWrite::IsoTile { target_format, .. } => *target_format,
                #[cfg(feature = "heif-native")]
                HdrCompletedComposeWrite::AppleImage { target_format, .. } => *target_format,
                #[cfg(not(feature = "heif-native"))]
                HdrCompletedComposeWrite::AppleImage { .. } => continue,
            };
            if !Self::ensure_hdr_resources(&wgpu_state, target_format) {
                defer.push(write);
                continue;
            }

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
                } => match resources.apply_iso_image_cpu_compose(
                    &wgpu_state.queue,
                    key,
                    target_capacity_bits,
                    width,
                    height,
                    &pixels,
                ) {
                    Ok(()) => changed = true,
                    Err(err) => {
                        log::warn!("[HDR] ISO CPU compose upload failed: {err}");
                        resources.mark_iso_image_compose_failed(key, target_capacity_bits);
                    }
                },
                #[cfg(feature = "heif-native")]
                HdrCompletedComposeWrite::AppleImage {
                    key,
                    target_capacity_bits,
                    width,
                    height,
                    pixels,
                    ..
                } => match resources.apply_apple_image_cpu_compose(
                    &wgpu_state.queue,
                    key,
                    target_capacity_bits,
                    width,
                    height,
                    &pixels,
                ) {
                    Ok(()) => changed = true,
                    Err(err) => {
                        log::warn!("[HDR] Apple CPU compose upload failed: {err}");
                        resources.mark_apple_image_compose_failed(key, target_capacity_bits);
                    }
                },
                HdrCompletedComposeWrite::IsoTile {
                    tile_key,
                    target_capacity_bits,
                    width,
                    height,
                    pixels,
                    ..
                } => match resources.apply_iso_tile_cpu_compose(
                    &wgpu_state.queue,
                    tile_key,
                    target_capacity_bits,
                    width,
                    height,
                    &pixels,
                ) {
                    Ok(()) => changed = true,
                    Err(err) => {
                        log::warn!("[HDR] ISO tile CPU compose upload failed: {err}");
                        resources.mark_iso_tile_compose_failed(tile_key);
                    }
                },
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
            let format = match failure {
                HdrCompletedComposeFailure::IsoImage {
                    target_format, ..
                }
                | HdrCompletedComposeFailure::IsoTile {
                    target_format, ..
                } => target_format,
                #[cfg(feature = "heif-native")]
                HdrCompletedComposeFailure::AppleImage {
                    target_format, ..
                } => target_format,
                #[cfg(not(feature = "heif-native"))]
                HdrCompletedComposeFailure::AppleImage { .. } => continue,
            };
            if !Self::ensure_hdr_resources(&wgpu_state, format) {
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
                }
                #[cfg(feature = "heif-native")]
                HdrCompletedComposeFailure::AppleImage {
                    key,
                    target_capacity_bits,
                    ..
                } => {
                    resources.mark_apple_image_compose_failed(key, target_capacity_bits);
                    changed = true;
                }
                HdrCompletedComposeFailure::IsoTile { tile_key, .. } => {
                    resources.mark_iso_tile_compose_failed(tile_key);
                    changed = true;
                }
                #[cfg(not(feature = "heif-native"))]
                HdrCompletedComposeFailure::AppleImage { .. } => {}
            }
        }
        changed
    }
}
