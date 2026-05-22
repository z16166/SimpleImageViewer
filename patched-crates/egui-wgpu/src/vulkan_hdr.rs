//! Linux Vulkan HDR10 swap-chain metadata (`VK_EXT_hdr_metadata`).
//!
//! Color space for `Rgb10a2Unorm` is selected at swapchain creation in our
//! patched `wgpu-hal`. This module applies ST 2086 static metadata after each
//! `surface.configure`, analogous to the Windows DXGI `SetColorSpace1` hook.

#![expect(unsafe_code)]

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
