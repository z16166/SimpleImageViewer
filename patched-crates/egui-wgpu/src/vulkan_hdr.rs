//! Linux Vulkan HDR10 swap-chain metadata (`VK_EXT_hdr_metadata`).
//!
//! Color space for `Rgb10a2Unorm` is selected at swapchain creation in our
//! patched `wgpu-hal` via [`wgpu_hal::linux_swapchain`]. ST 2086 static metadata
//! is applied only for PQ/HDR10 swap chains, analogous to the Windows DXGI
//! `SetColorSpace1` hook.

#![expect(unsafe_code)]

#[cfg(target_os = "linux")]
use wgpu_hal::linux_swapchain::{LinuxRgb10a2VkColorSpace, preferred_linux_rgb10a2_vk_color_space};

/// Sync the patched Vulkan swap-chain color-space preference with the active
/// `Rgb10a2Unorm` UI encoding (PQ vs gamma 2.2 electrical).
#[cfg(target_os = "linux")]
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

#[cfg(not(target_os = "linux"))]
pub fn linux_sync_rgb10a2_vk_color_space(_format: wgpu::TextureFormat, _pq_framebuffer: bool) {}

#[cfg(target_os = "linux")]
pub fn linux_rgb10a2_uses_hdr10_st2084(format: wgpu::TextureFormat) -> bool {
    format == wgpu::TextureFormat::Rgb10a2Unorm
        && preferred_linux_rgb10a2_vk_color_space() == LinuxRgb10a2VkColorSpace::Hdr10St2084
}

#[cfg(not(target_os = "linux"))]
pub fn linux_rgb10a2_uses_hdr10_st2084(_format: wgpu::TextureFormat) -> bool {
    false
}

/// Log raw Vulkan WSI surface `(format, color_space)` pairs once per process.
#[cfg(target_os = "linux")]
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

#[cfg(not(target_os = "linux"))]
pub fn linux_log_vulkan_hdr_surface_probe_once(
    _surface: &wgpu::Surface<'_>,
    _adapter: &wgpu::Adapter,
    _gates: &crate::VulkanWsiHdrGatesMailbox,
) {
}

/// Default HDR10 metadata when the monitor probe has not yet supplied values.
#[derive(Debug, Clone, Copy)]
pub struct VulkanHdrMetadata {
    pub max_luminance_nits: f32,
    pub max_frame_average_luminance_nits: f32,
    pub min_luminance_nits: f32,
}

impl Default for VulkanHdrMetadata {
    fn default() -> Self {
        Self {
            max_luminance_nits: 1000.0,
            max_frame_average_luminance_nits: 400.0,
            min_luminance_nits: 0.05,
        }
    }
}

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

#[cfg(target_os = "linux")]
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
        vk_metadata.display_primary_red = vk::XYColorEXT { x: 0.640, y: 0.330 };
        vk_metadata.display_primary_green = vk::XYColorEXT { x: 0.300, y: 0.600 };
        vk_metadata.display_primary_blue = vk::XYColorEXT { x: 0.150, y: 0.060 };
        vk_metadata.white_point = vk::XYColorEXT {
            x: 0.3127,
            y: 0.3290,
        };
        vk_metadata.max_luminance = metadata.max_luminance_nits;
        vk_metadata.min_luminance = metadata.min_luminance_nits;
        vk_metadata.max_content_light_level = metadata.max_luminance_nits;
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
                 (max={} nits, max_fall={} nits)",
                metadata.max_luminance_nits,
                metadata.max_frame_average_luminance_nits
            );
        }
        Err(reason) => {
            log::debug!("egui-wgpu: Vulkan HDR metadata not applied ({reason})");
        }
    }
}

#[cfg(not(target_os = "linux"))]
pub fn linux_vulkan_set_swap_chain_hdr_metadata(
    _surface: &wgpu::Surface<'_>,
    _device: &wgpu::Device,
    _adapter: &wgpu::Adapter,
    _format: wgpu::TextureFormat,
    _metadata: VulkanHdrMetadata,
) {
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hdr_metadata_defaults_exist_for_rgb10a2_and_float_formats() {
        assert!(default_vulkan_hdr_metadata_for_format(wgpu::TextureFormat::Rgb10a2Unorm).is_some());
        assert!(default_vulkan_hdr_metadata_for_format(wgpu::TextureFormat::Rgba16Float).is_some());
        assert!(default_vulkan_hdr_metadata_for_format(wgpu::TextureFormat::Bgra8Unorm).is_none());
    }
}
