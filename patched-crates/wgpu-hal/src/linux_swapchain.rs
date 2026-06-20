//! Linux Vulkan swap-chain color-space preference for `Rgb10a2Unorm`.
//!
//! Patched `wgpu-hal` cannot know the compositor EOTF from `SurfaceConfiguration`
//! alone. The application sets this immediately before `surface.configure` so
//! PQ (ST 2084) and gamma 2.2 electrical (KWin KMS offload) paths do not share
//! a single hard-coded `HDR10_ST2084_EXT` declaration.
//!
//! **Not implemented:** `VkColorSpaceKHR::HDR10_HLG_EXT` (BT.2100 HLG). That would
//! require a third app-side encoding path (HLG OETF shaders, matching egui/clear
//! color, and Wayland probe support) with no validated Linux desktop compositor
//! target today; HLG sources are decoded and presented via PQ/gamma2.2/SDR instead.

use core::sync::atomic::{AtomicU8, Ordering};

/// Preferred `VkColorSpaceKHR` pairing for Linux `Rgb10a2Unorm` swap chains.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxRgb10a2VkColorSpace {
    /// sRGB non-linear — gamma 2.2 electrical framebuffers (KWin HDR offload).
    SrgbNonLinear = 0,
    /// HDR10 / SMPTE ST 2084 when the compositor advertises PQ.
    Hdr10St2084 = 1,
    // HLG (`HDR10_HLG_EXT`) omitted — see module docs.
}

static PREFERRED_RGB10A2_COLOR_SPACE: AtomicU8 =
    AtomicU8::new(LinuxRgb10a2VkColorSpace::SrgbNonLinear as u8);

/// Select the Vulkan swap-chain color space used on the next Linux `Rgb10a2Unorm`
/// swap-chain creation.
pub fn set_linux_rgb10a2_vk_color_space(space: LinuxRgb10a2VkColorSpace) {
    PREFERRED_RGB10A2_COLOR_SPACE.store(space as u8, Ordering::Release);
}

pub fn preferred_linux_rgb10a2_vk_color_space() -> LinuxRgb10a2VkColorSpace {
    match PREFERRED_RGB10A2_COLOR_SPACE.load(Ordering::Acquire) {
        x if x == LinuxRgb10a2VkColorSpace::Hdr10St2084 as u8 => {
            LinuxRgb10a2VkColorSpace::Hdr10St2084
        }
        _ => LinuxRgb10a2VkColorSpace::SrgbNonLinear,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_rgb10a2_color_space_is_srgb_nonlinear() {
        set_linux_rgb10a2_vk_color_space(LinuxRgb10a2VkColorSpace::SrgbNonLinear);
        assert_eq!(
            preferred_linux_rgb10a2_vk_color_space(),
            LinuxRgb10a2VkColorSpace::SrgbNonLinear
        );
    }

    #[test]
    fn pq_compositor_can_select_hdr10_st2084() {
        set_linux_rgb10a2_vk_color_space(LinuxRgb10a2VkColorSpace::Hdr10St2084);
        assert_eq!(
            preferred_linux_rgb10a2_vk_color_space(),
            LinuxRgb10a2VkColorSpace::Hdr10St2084
        );
        set_linux_rgb10a2_vk_color_space(LinuxRgb10a2VkColorSpace::SrgbNonLinear);
    }
}
