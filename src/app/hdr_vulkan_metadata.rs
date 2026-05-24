// Simple Image Viewer - A high-performance, cross-platform image viewer
// Copyright (C) 2024-2026 Simple Image Viewer Contributors
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

use crate::app::ImageViewerApp;
#[cfg(target_os = "linux")]
use crate::hdr::types::{HdrImageBuffer, HdrImageMetadata};

impl ImageViewerApp {
    /// Push per-content ST 2086 metadata to the egui-wgpu painter when Linux
    /// HDR10 PQ swap chains are active.
    ///
    /// Works for every HDR decode path (AVIF, HEIF, JXL, Ultra HDR JPEG, EXR,
    /// Radiance HDR, float TIFF, tiled sources, …) via unified
    /// [`HdrImageMetadata`] + optional pixel peak scan.
    ///
    /// When the active view is SDR-only but the swap chain remains HDR10 PQ
    /// (HDR monitor), conservative default metadata is published so the
    /// compositor does not keep the previous image's MaxCLL / MaxFALL.
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
                self.reset_linux_vulkan_hdr_metadata_state();
                return;
            }

            let vk_metadata = match self.current_hdr_vulkan_metadata_inputs() {
                Some((image_metadata, scan_buffer)) => {
                    crate::hdr::vulkan_metadata::vulkan_hdr_metadata_for_content(
                        &image_metadata,
                        scan_buffer,
                    )
                }
                None => crate::hdr::vulkan_metadata::default_vulkan_hdr_metadata_for_sdr_view(),
            };

            // Republish every frame (mailbox must stay populated for swap-chain
            // reconfigure). Log only when metadata changes.
            if self.last_vulkan_hdr_metadata != Some(vk_metadata) {
                log::debug!(
                    "[HDR] Vulkan swap-chain metadata: max_cll={} nits, max_fall={} nits, mastering_max={} nits",
                    vk_metadata.max_content_light_level_nits,
                    vk_metadata.max_frame_average_luminance_nits,
                    vk_metadata.mastering_max_luminance_nits,
                );
            }
            self.last_vulkan_hdr_metadata = Some(vk_metadata);
            self.requested_vulkan_hdr_metadata.request(vk_metadata);
        }
    }

    #[cfg(target_os = "linux")]
    fn reset_linux_vulkan_hdr_metadata_state(&mut self) {
        self.last_vulkan_hdr_metadata = None;
    }

    #[cfg(target_os = "linux")]
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

/// Resolve ST 2086 metadata for Linux HDR10 PQ sync (unit-testable).
#[cfg(test)]
pub(crate) fn linux_vulkan_hdr_metadata_for_view(
    has_hdr_source: bool,
    image_metadata: Option<&crate::hdr::types::HdrImageMetadata>,
    scan_buffer: Option<&crate::hdr::types::HdrImageBuffer>,
) -> eframe::egui_wgpu::VulkanHdrMetadata {
    if has_hdr_source {
        let metadata = image_metadata.expect("metadata required with HDR source");
        crate::hdr::vulkan_metadata::vulkan_hdr_metadata_for_content(metadata, scan_buffer)
    } else {
        crate::hdr::vulkan_metadata::default_vulkan_hdr_metadata_for_sdr_view()
    }
}

#[cfg(test)]
mod tests {
    use super::linux_vulkan_hdr_metadata_for_view;
    use crate::hdr::types::{
        HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrLuminanceMetadata, HdrPixelFormat,
        HdrReference, HdrTransferFunction,
    };
    use std::sync::Arc;

    #[test]
    fn sdr_view_without_hdr_source_uses_default_metadata() {
        let metadata = linux_vulkan_hdr_metadata_for_view(false, None, None);
        assert_eq!(metadata.max_content_light_level_nits, 1000.0);
        assert_eq!(metadata.max_frame_average_luminance_nits, 0.0);
    }

    #[test]
    fn hdr_source_uses_container_clli_over_sdr_default() {
        let image_metadata = HdrImageMetadata {
            transfer_function: HdrTransferFunction::Pq,
            reference: HdrReference::DisplayReferred,
            luminance: HdrLuminanceMetadata {
                max_cll_nits: Some(2500.0),
                max_fall_nits: Some(180.0),
                ..HdrLuminanceMetadata::default()
            },
            ..HdrImageMetadata::from_color_space(HdrColorSpace::Rec2020Linear)
        };
        let metadata = linux_vulkan_hdr_metadata_for_view(true, Some(&image_metadata), None);
        assert_eq!(metadata.max_content_light_level_nits, 2500.0);
        assert_eq!(metadata.max_frame_average_luminance_nits, 180.0);
    }

    #[test]
    fn switching_from_hdr_to_sdr_view_resets_to_default_metadata() {
        let hdr_meta = HdrImageMetadata {
            transfer_function: HdrTransferFunction::Pq,
            reference: HdrReference::DisplayReferred,
            luminance: HdrLuminanceMetadata {
                max_cll_nits: Some(4000.0),
                ..HdrLuminanceMetadata::default()
            },
            ..HdrImageMetadata::from_color_space(HdrColorSpace::Rec2020Linear)
        };
        let hdr = linux_vulkan_hdr_metadata_for_view(true, Some(&hdr_meta), None);
        let sdr = linux_vulkan_hdr_metadata_for_view(false, None, None);
        assert_ne!(
            hdr.max_content_light_level_nits,
            sdr.max_content_light_level_nits
        );
        assert_eq!(sdr.max_content_light_level_nits, 1000.0);
    }

    #[test]
    fn hdr_source_with_linear_buffer_uses_pixel_peak_not_sdr_default() {
        let buffer = HdrImageBuffer {
            width: 1,
            height: 1,
            format: HdrPixelFormat::Rgba32Float,
            color_space: HdrColorSpace::LinearSrgb,
            metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
            rgba_f32: Arc::new(vec![2.0, 2.0, 2.0, 1.0]),
        };
        let metadata =
            linux_vulkan_hdr_metadata_for_view(true, Some(&buffer.metadata), Some(&buffer));
        assert!(
            metadata.max_content_light_level_nits > 400.0,
            "linear 2.0 should exceed SDR default, got {}",
            metadata.max_content_light_level_nits
        );
    }
}
