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
    fn hdr_callback_prewarm_target_format(&self) -> Option<wgpu::TextureFormat> {
        predicted_hdr_callback_target_format(
            self.settings.hdr_native_surface_enabled_effective(),
            self.effective_hdr_monitor_selection()
                .is_some_and(|selection| selection.hdr_supported),
            self.hdr_capabilities.candidate_texture_format,
            self.hdr_target_format,
        )
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
        {
            crate::wgpu_pipeline_cache::persist(info, cache);
        }
    }
}
