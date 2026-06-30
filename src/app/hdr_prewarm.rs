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
use crate::hdr::renderer::predicted_hdr_callback_target_format;

impl ImageViewerApp {
    pub(crate) fn hdr_callback_prewarm_target_format(&self) -> Option<wgpu::TextureFormat> {
        predicted_hdr_callback_target_format(
            crate::hdr::surface::native_hdr_swapchain_requests_enabled(
                self.settings.hdr_native_surface_enabled_effective(),
                self.hdr_capabilities.backend,
            ),
            self.effective_hdr_monitor_selection()
                .is_some_and(|selection| selection.hdr_supported),
            self.hdr_capabilities.candidate_texture_format,
            self.hdr_target_format,
        )
    }

    pub(crate) fn sync_loader_hdr_callback_upload_snapshot(&self) {
        self.loader
            .set_hdr_callback_upload_active(self.hdr_callback_prewarm_target_format().is_some());
    }

    /// Keep loader worker `device_id` and GPU handles aligned with the live painter Device.
    pub(crate) fn sync_loader_wgpu_context_from_frame(&mut self, frame: &eframe::Frame) {
        let Some(state) = frame.wgpu_render_state() else {
            return;
        };
        if self.loader_wgpu_device.as_ref() == Some(&state.device) {
            return;
        }
        self.sync_loader_wgpu_context(state.device.clone(), state.queue.clone());
    }

    pub(crate) fn sync_loader_wgpu_context(&mut self, device: wgpu::Device, queue: wgpu::Queue) {
        if self.loader_wgpu_device.as_ref() == Some(&device) {
            return;
        }

        if self.loader_wgpu_device.is_some() {
            // Bump epoch so in-flight loader workers observe a stale device_id via
            // `wgpu_device_id_live` and drop pre-uploaded planes instead of registering them
            // against the replaced Device.
            self.current_device_id = self.current_device_id.saturating_add(1);
            log::debug!(
                "[Loader] wgpu Device instance replaced; current_device_id={}",
                self.current_device_id
            );
        }

        self.loader_wgpu_device = Some(device.clone());
        self.loader
            .set_wgpu_context(Some(device), Some(queue), self.current_device_id);
    }

    pub(crate) fn sync_hdr_callback_resources_prewarm(&mut self, frame: &eframe::Frame) {
        let Some(wgpu_state) = frame.wgpu_render_state() else {
            return;
        };
        let Some(format) = self.hdr_callback_prewarm_target_format() else {
            return;
        };

        self.hdr_callback_resources_prewarm.ensure_started(
            &wgpu_state.device,
            format,
            self.wgpu_pipeline_cache.as_deref(),
        );

        let mut renderer = wgpu_state.renderer.write();
        crate::hdr::renderer::HdrCallbackResourcesPrewarm::ensure_prewarm_slot(
            &mut renderer.callback_resources,
            &self.hdr_callback_resources_prewarm,
        );
        if self
            .hdr_callback_resources_prewarm
            .inject_ready_into_callback_resources(format, &mut renderer.callback_resources)
            && let (Some(info), Some(cache)) = (
                self.wgpu_adapter_info.as_ref(),
                self.wgpu_pipeline_cache.as_deref(),
            )
            && crate::wgpu_pipeline_cache::runtime_prewarm_persist_enabled(info.backend)
        {
            crate::wgpu_pipeline_cache::persist_async(info, cache);
        }
    }
}
