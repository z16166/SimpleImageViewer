//! Linux Vulkan WSI surface format + color-space diagnostics for HDR gating.

use alloc::{format, string::String, vec::Vec};

use ash::vk;

use crate::vulkan::conv;
use crate::vulkan::swapchain::NativeSurface;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VulkanSurfaceFormatPair {
    pub vk_format: String,
    pub vk_color_space: String,
    pub wgpu_format: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VulkanHdrSurfaceProbe {
    pub adapter_name: String,
    pub pairs: Vec<VulkanSurfaceFormatPair>,
    /// `A2B10G10R10` + `HDR10_ST2084_EXT` — industrial HDR10 PQ gate signal.
    pub hdr10_st2084_rgb10a2: bool,
    /// `R16G16B16A16_SFLOAT` + `EXTENDED_SRGB_LINEAR_EXT` — scRGB-style path.
    pub extended_srgb_linear_rgba16f: bool,
    /// `A2B10G10R10` + `HDR10_HLG_EXT` — BT.2100 HLG (not implemented in-app).
    pub hdr10_hlg_rgb10a2: bool,
    /// `A2B10G10R10` + `SRGB_NONLINEAR` — 10-bit SDR / KWin gamma-2.2 electrical path.
    pub srgb_nonlinear_rgb10a2: bool,
}

fn vk_format_name(format: vk::Format) -> String {
    format!("{format:?}")
}

fn vk_color_space_name(color_space: vk::ColorSpaceKHR) -> String {
    match color_space {
        vk::ColorSpaceKHR::SRGB_NONLINEAR => "SRGB_NONLINEAR".into(),
        vk::ColorSpaceKHR::HDR10_ST2084_EXT => "HDR10_ST2084_EXT".into(),
        vk::ColorSpaceKHR::EXTENDED_SRGB_LINEAR_EXT => "EXTENDED_SRGB_LINEAR_EXT".into(),
        vk::ColorSpaceKHR::HDR10_HLG_EXT => "HDR10_HLG_EXT".into(),
        other => format!("{other:?}"),
    }
}

fn is_a2b10g10r10(format: vk::Format) -> bool {
    format == vk::Format::A2B10G10R10_UNORM_PACK32
}

fn is_r16g16b16a16_sfloat(format: vk::Format) -> bool {
    format == vk::Format::R16G16B16A16_SFLOAT
}

/// Query raw `vkGetPhysicalDeviceSurfaceFormatsKHR` pairs for HDR migration work.
pub fn probe_hdr_surface(
    adapter: &super::Adapter,
    surface: &super::Surface,
) -> Result<VulkanHdrSurfaceProbe, String> {
    let native = surface
        .inner
        .as_any()
        .downcast_ref::<NativeSurface>()
        .ok_or_else(|| String::from("Vulkan surface is not a Wayland/X11 native surface"))?;

    let raw_formats = native
        .raw_surface_formats(adapter.raw)
        .map_err(|err| format!("vkGetPhysicalDeviceSurfaceFormatsKHR failed: {err}"))?;

    let mut pairs = Vec::with_capacity(raw_formats.len());
    let mut hdr10_st2084_rgb10a2 = false;
    let mut extended_srgb_linear_rgba16f = false;
    let mut hdr10_hlg_rgb10a2 = false;
    let mut srgb_nonlinear_rgb10a2 = false;

    for sf in raw_formats {
        let vk_format = vk_format_name(sf.format);
        let vk_color_space = vk_color_space_name(sf.color_space);
        let wgpu_format = conv::map_vk_surface_formats(sf).map(|f| format!("{f:?}"));

        if is_a2b10g10r10(sf.format) {
            match sf.color_space {
                vk::ColorSpaceKHR::HDR10_ST2084_EXT => hdr10_st2084_rgb10a2 = true,
                vk::ColorSpaceKHR::HDR10_HLG_EXT => hdr10_hlg_rgb10a2 = true,
                vk::ColorSpaceKHR::SRGB_NONLINEAR => srgb_nonlinear_rgb10a2 = true,
                _ => {}
            }
        }
        if is_r16g16b16a16_sfloat(sf.format)
            && sf.color_space == vk::ColorSpaceKHR::EXTENDED_SRGB_LINEAR_EXT
        {
            extended_srgb_linear_rgba16f = true;
        }

        pairs.push(VulkanSurfaceFormatPair {
            vk_format,
            vk_color_space,
            wgpu_format,
        });
    }

    Ok(VulkanHdrSurfaceProbe {
        adapter_name: adapter
            .physical_device_capabilities()
            .properties()
            .device_name_as_c_str()
            .ok()
            .and_then(|name| name.to_str().ok())
            .map(String::from)
            .unwrap_or_else(|| String::from("?")),
        pairs,
        hdr10_st2084_rgb10a2,
        extended_srgb_linear_rgba16f,
        hdr10_hlg_rgb10a2,
        srgb_nonlinear_rgb10a2,
    })
}

pub fn log_hdr_surface_probe(probe: &VulkanHdrSurfaceProbe) {
    log::info!(
        "[HDR] Vulkan WSI surface probe: adapter_driver={} pair_count={}",
        probe.adapter_name,
        probe.pairs.len()
    );
    for pair in &probe.pairs {
        log::info!(
            "[HDR]   vk_format={} vk_color_space={} wgpu_format={}",
            pair.vk_format,
            pair.vk_color_space,
            pair.wgpu_format.as_deref().unwrap_or("(unmapped)")
        );
    }
    log::info!(
        "[HDR] Vulkan WSI HDR gates: hdr10_st2084_rgb10a2={} extended_srgb_linear_rgba16f={} \
         hdr10_hlg_rgb10a2={} srgb_nonlinear_rgb10a2={}",
        probe.hdr10_st2084_rgb10a2,
        probe.extended_srgb_linear_rgba16f,
        probe.hdr10_hlg_rgb10a2,
        probe.srgb_nonlinear_rgb10a2
    );
}
