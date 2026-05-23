//! Linux Vulkan HDR10 swap-chain metadata (`VK_EXT_hdr_metadata`).
//!
//! Color space for `Rgb10a2Unorm` is selected at swapchain creation in our
//! patched `wgpu-hal` via [`wgpu_hal::linux_swapchain`]. ST 2086 static metadata
//! is applied only for PQ/HDR10 swap chains, analogous to the Windows DXGI
//! `SetColorSpace1` hook.
//!
//! [`VulkanHdrMetadata`] is the cross-platform mailbox payload; Linux-only Vulkan
//! hooks are compiled only on `target_os = "linux"`.

/// HDR10 / ST 2086 metadata submitted through `vkSetHdrMetadataEXT`.
///
/// `mastering_max_luminance_nits` is the reference mastering display peak.
/// `max_content_light_level_nits` (MaxCLL) is the brightest pixel in the
/// current content — the compositor uses this for tone mapping roll-off.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VulkanHdrMetadata {
    pub mastering_max_luminance_nits: f32,
    pub max_content_light_level_nits: f32,
    pub max_frame_average_luminance_nits: f32,
    pub min_luminance_nits: f32,
}

impl Default for VulkanHdrMetadata {
    fn default() -> Self {
        Self {
            mastering_max_luminance_nits: 1000.0,
            max_content_light_level_nits: 1000.0,
            max_frame_average_luminance_nits: 0.0,
            min_luminance_nits: 0.005,
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    #![expect(unsafe_code)]

    use super::VulkanHdrMetadata;
    use wgpu_hal::linux_swapchain::{LinuxRgb10a2VkColorSpace, preferred_linux_rgb10a2_vk_color_space};

    /// BT.2020 primaries + D65 white point for HDR10 PQ swap chains.
    const BT2020_PRIMARY_RED: (f32, f32) = (0.708, 0.292);
    const BT2020_PRIMARY_GREEN: (f32, f32) = (0.170, 0.797);
    const BT2020_PRIMARY_BLUE: (f32, f32) = (0.131, 0.046);
    const D65_WHITE_POINT: (f32, f32) = (0.3127, 0.3290);

    pub fn default_vulkan_hdr_metadata_for_format(
        format: wgpu::TextureFormat,
    ) -> Option<VulkanHdrMetadata> {
        match format {
            wgpu::TextureFormat::Rgb10a2Unorm => Some(VulkanHdrMetadata::default()),
            wgpu::TextureFormat::Rgba16Float | wgpu::TextureFormat::Rgba32Float => {
                Some(VulkanHdrMetadata::default())
            }
            _ => None,
        }
    }

    /// Sync the patched Vulkan swap-chain color-space preference with the active
    /// `Rgb10a2Unorm` UI encoding (PQ vs gamma 2.2 electrical).
    pub fn linux_sync_rgb10a2_vk_color_space(format: wgpu::TextureFormat, pq_framebuffer: bool) {
        if format != wgpu::TextureFormat::Rgb10a2Unorm {
            return;
        }
        wgpu_hal::linux_swapchain::set_linux_rgb10a2_vk_color_space(if pq_framebuffer {
            LinuxRgb10a2VkColorSpace::Hdr10St2084
        } else {
            LinuxRgb10a2VkColorSpace::SrgbNonLinear
        });
    }

    pub fn linux_rgb10a2_uses_hdr10_st2084(format: wgpu::TextureFormat) -> bool {
        format == wgpu::TextureFormat::Rgb10a2Unorm
            && preferred_linux_rgb10a2_vk_color_space() == LinuxRgb10a2VkColorSpace::Hdr10St2084
    }

    /// Log raw Vulkan WSI surface `(format, color_space)` pairs once per process.
    pub fn linux_log_vulkan_hdr_surface_probe_once(
        surface: &wgpu::Surface<'_>,
        adapter: &wgpu::Adapter,
        gates: &crate::VulkanWsiHdrGatesMailbox,
    ) {
        use core::sync::atomic::{AtomicBool, Ordering};

        static LOGGED: AtomicBool = AtomicBool::new(false);
        if LOGGED.swap(true, Ordering::AcqRel) {
            return;
        }
        if adapter.get_info().backend != wgpu::Backend::Vulkan {
            log::info!(
                "[HDR] Vulkan WSI surface probe skipped: backend={:?}",
                adapter.get_info().backend
            );
            return;
        }

        let result = (|| {
            // SAFETY: hal handles are valid for the duration of the wgpu objects.
            let hal_surface = unsafe { surface.as_hal::<wgpu::hal::api::Vulkan>() }
                .ok_or("wgpu::Surface::as_hal<Vulkan> returned None")?;
            let hal_adapter = unsafe { adapter.as_hal::<wgpu::hal::api::Vulkan>() }
                .ok_or("wgpu::Adapter::as_hal<Vulkan> returned None")?;
            wgpu_hal::linux_surface_probe::probe_hdr_surface(&hal_adapter, &hal_surface)
        })();

        match result {
            Ok(probe) => {
                log::info!(
                    "[HDR] Vulkan WSI surface probe: wgpu_adapter={}",
                    adapter.get_info().name
                );
                wgpu_hal::linux_surface_probe::log_hdr_surface_probe(&probe);
                gates.set(crate::VulkanWsiHdrGates {
                    hdr10_st2084_rgb10a2: probe.hdr10_st2084_rgb10a2,
                    extended_srgb_linear_rgba16f: probe.extended_srgb_linear_rgba16f,
                    probed: true,
                });
            }
            Err(err) => {
                log::warn!("[HDR] Vulkan WSI surface probe failed: {err}");
            }
        }
    }

    pub fn linux_vulkan_set_swap_chain_hdr_metadata(
        surface: &wgpu::Surface<'_>,
        device: &wgpu::Device,
        adapter: &wgpu::Adapter,
        format: wgpu::TextureFormat,
        metadata: VulkanHdrMetadata,
    ) {
        use ash::vk;

        if adapter.get_info().backend != wgpu::Backend::Vulkan {
            return;
        }
        if default_vulkan_hdr_metadata_for_format(format).is_none() {
            return;
        }

        let result: Result<(), String> = (|| unsafe {
            let hal_surface = surface
                .as_hal::<wgpu::hal::api::Vulkan>()
                .ok_or_else(|| "wgpu::Surface::as_hal<Vulkan> returned None".to_string())?;
            let swapchain = hal_surface
                .raw_native_swapchain()
                .ok_or_else(|| "Vulkan hal surface has no native swapchain".to_string())?;
            let hal_device = device
                .as_hal::<wgpu::hal::api::Vulkan>()
                .ok_or_else(|| "wgpu::Device::as_hal<Vulkan> returned None".to_string())?;

            if !hal_device
                .enabled_device_extensions()
                .iter()
                .any(|ext| *ext == ash::ext::hdr_metadata::NAME)
            {
                return Err("VK_EXT_hdr_metadata is not enabled on the logical device".to_string());
            }

            let hdr = ash::ext::hdr_metadata::Device::new(
                hal_device.raw_vulkan_instance(),
                hal_device.raw_device(),
            );

            let mut vk_metadata = vk::HdrMetadataEXT::default();
            vk_metadata.display_primary_red = vk::XYColorEXT {
                x: BT2020_PRIMARY_RED.0,
                y: BT2020_PRIMARY_RED.1,
            };
            vk_metadata.display_primary_green = vk::XYColorEXT {
                x: BT2020_PRIMARY_GREEN.0,
                y: BT2020_PRIMARY_GREEN.1,
            };
            vk_metadata.display_primary_blue = vk::XYColorEXT {
                x: BT2020_PRIMARY_BLUE.0,
                y: BT2020_PRIMARY_BLUE.1,
            };
            vk_metadata.white_point = vk::XYColorEXT {
                x: D65_WHITE_POINT.0,
                y: D65_WHITE_POINT.1,
            };
            vk_metadata.max_luminance = metadata.mastering_max_luminance_nits;
            vk_metadata.min_luminance = metadata.min_luminance_nits;
            vk_metadata.max_content_light_level = metadata.max_content_light_level_nits;
            vk_metadata.max_frame_average_light_level = metadata.max_frame_average_luminance_nits;

            hdr.set_hdr_metadata(
                std::slice::from_ref(&swapchain),
                std::slice::from_ref(&vk_metadata),
            );
            Ok(())
        })();

        match result {
            Ok(()) => {
                log::debug!(
                    "egui-wgpu: Vulkan HDR metadata applied for {format:?} \
                     (mastering_max={} nits, max_cll={} nits, max_fall={} nits)",
                    metadata.mastering_max_luminance_nits,
                    metadata.max_content_light_level_nits,
                    metadata.max_frame_average_luminance_nits
                );
            }
            Err(reason) => {
                log::debug!("egui-wgpu: Vulkan HDR metadata not applied ({reason})");
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn hdr_metadata_defaults_exist_for_rgb10a2_and_float_formats() {
            assert!(
                default_vulkan_hdr_metadata_for_format(wgpu::TextureFormat::Rgb10a2Unorm).is_some()
            );
            assert!(
                default_vulkan_hdr_metadata_for_format(wgpu::TextureFormat::Rgba16Float).is_some()
            );
            assert!(
                default_vulkan_hdr_metadata_for_format(wgpu::TextureFormat::Bgra8Unorm).is_none()
            );
        }
    }
}

#[cfg(target_os = "linux")]
pub use linux::{
    default_vulkan_hdr_metadata_for_format, linux_log_vulkan_hdr_surface_probe_once,
    linux_rgb10a2_uses_hdr10_st2084, linux_sync_rgb10a2_vk_color_space,
    linux_vulkan_set_swap_chain_hdr_metadata,
};

#[cfg(test)]
mod tests {
    use super::VulkanHdrMetadata;

    #[test]
    fn default_metadata_separates_mastering_max_from_max_cll_fields() {
        let metadata = VulkanHdrMetadata::default();
        assert_eq!(metadata.mastering_max_luminance_nits, 1000.0);
        assert_eq!(metadata.max_content_light_level_nits, 1000.0);
        assert_eq!(metadata.max_frame_average_luminance_nits, 0.0);
        assert_eq!(metadata.min_luminance_nits, 0.005);
    }
}
