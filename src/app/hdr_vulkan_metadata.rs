// Simple Image Viewer - A high-performance, cross-platform image viewer
// Copyright (C) 2024-2026 Simple Image Viewer Contributors
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

use crate::app::ImageViewerApp;
use crate::hdr::types::{HdrImageBuffer, HdrImageMetadata};

impl ImageViewerApp {
    /// Push per-content ST 2086 metadata to the egui-wgpu painter when Linux
    /// HDR10 PQ swap chains are active.
    ///
    /// Works for every HDR decode path (AVIF, HEIF, JXL, Ultra HDR JPEG, EXR,
    /// Radiance HDR, float TIFF, tiled sources, …) via unified
    /// [`HdrImageMetadata`] + optional pixel peak scan.
    pub(crate) fn sync_linux_vulkan_hdr_metadata(&mut self) {
        #[cfg(not(target_os = "linux"))]
        {
            return;
        }

        #[cfg(target_os = "linux")]
        {
            let render_mode = crate::hdr::monitor::effective_render_output_mode(
                self.hdr_target_format,
                self.effective_hdr_monitor_selection().as_ref(),
            );
            if !render_mode.rgb10a2_uses_pq_shader() {
                return;
            }

            let Some((image_metadata, scan_buffer)) = self.current_hdr_vulkan_metadata_inputs()
            else {
                return;
            };

            let peak = crate::hdr::vulkan_metadata::content_peak_nits(
                &image_metadata.luminance,
                scan_buffer,
            );

            let vk_metadata = crate::hdr::vulkan_metadata::vulkan_hdr_metadata_from_luminance(
                &image_metadata.luminance,
                peak,
            );

            if self.last_vulkan_hdr_metadata == Some(vk_metadata) {
                return;
            }

            log::info!(
                "[HDR] Vulkan swap-chain metadata: max_cll={} nits, max_fall={} nits, mastering_max={} nits",
                vk_metadata.max_content_light_level_nits,
                vk_metadata.max_frame_average_luminance_nits,
                vk_metadata.mastering_max_luminance_nits,
            );
            self.last_vulkan_hdr_metadata = Some(vk_metadata);
            self.requested_vulkan_hdr_metadata.request(vk_metadata);
        }
    }

    fn current_hdr_vulkan_metadata_inputs(
        &self,
    ) -> Option<(HdrImageMetadata, Option<&HdrImageBuffer>)> {
        if let Some(image) = self
            .current_hdr_image
            .as_ref()
            .and_then(|current| current.image_for_index(self.current_index))
        {
            return Some((image.metadata.clone(), Some(image.as_ref())));
        }

        if let Some(source) = self
            .current_hdr_tiled_image
            .as_ref()
            .and_then(|current| current.source_for_index(self.current_index))
        {
            let metadata = source.metadata();
            let preview = self
                .current_hdr_tiled_preview
                .as_ref()
                .and_then(|current| current.image_for_index(self.current_index))
                .map(|image| image.as_ref());
            return Some((metadata, preview));
        }

        None
    }
}
