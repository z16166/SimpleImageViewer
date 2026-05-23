// Simple Image Viewer - A high-performance, cross-platform image viewer
// Copyright (C) 2024-2026 Simple Image Viewer Contributors
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

//! Linux Vulkan WSI `(format, color_space)` gates for native HDR presentation.

use super::monitor::HdrMonitorSelection;
#[cfg(any(target_os = "linux", test))]
use super::monitor::HdrNativeSurfaceEncoding;

/// Subset of [`wgpu_hal::linux_surface_probe::VulkanHdrSurfaceProbe`] published by egui-wgpu.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WsiHdrSurfaceGates {
    pub hdr10_st2084_rgb10a2: bool,
    pub extended_srgb_linear_rgba16f: bool,
    /// Set after the first successful `vkGetPhysicalDeviceSurfaceFormatsKHR` probe.
    pub probed: bool,
}

#[cfg(target_os = "linux")]
impl WsiHdrSurfaceGates {
    pub fn hdr_native_presentation_available(self) -> bool {
        self.probed && (self.hdr10_st2084_rgb10a2 || self.extended_srgb_linear_rgba16f)
    }
}

/// Combine Wayland `wp_color_management` metadata with Vulkan WSI surface gates.
///
/// On Linux, **WSI is authoritative** once probed: HDR10 requires
/// `A2B10G10R10 + HDR10_ST2084_EXT`. Wayland peak luminance / primaries remain
/// metadata-only (tone-map + ST2086), not HDR on/off signals.
#[cfg(target_os = "linux")]
pub fn linux_effective_monitor_selection(
    wp: Option<&HdrMonitorSelection>,
    wsi: WsiHdrSurfaceGates,
) -> Option<HdrMonitorSelection> {
    let wp = wp.cloned()?;

    if !wsi.probed {
        // Surface not configured yet — only trust explicit ST2084 from wp (no peak-nit heuristics).
        return Some(wp);
    }

    let hdr_supported = wsi.hdr_native_presentation_available();
    let native_surface_encoding = if wsi.hdr10_st2084_rgb10a2 {
        Some(HdrNativeSurfaceEncoding::PqHdr10)
    } else if wsi.extended_srgb_linear_rgba16f {
        Some(HdrNativeSurfaceEncoding::LinearScRgb)
    } else {
        None
    };

    Some(HdrMonitorSelection {
        hdr_supported,
        native_surface_encoding: hdr_supported
            .then(|| native_surface_encoding)
            .flatten(),
        hdr_capacity_source: if hdr_supported {
            Some("Vulkan WSI surface formats")
        } else {
            wp.hdr_capacity_source
        },
        label: wp.label,
        max_luminance_nits: wp.max_luminance_nits,
        max_full_frame_luminance_nits: wp.max_full_frame_luminance_nits,
        max_hdr_capacity: wp.max_hdr_capacity,
    })
}

#[cfg(not(target_os = "linux"))]
pub fn linux_effective_monitor_selection(
    wp: Option<&HdrMonitorSelection>,
    _wsi: WsiHdrSurfaceGates,
) -> Option<HdrMonitorSelection> {
    wp.cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wp_gamma22_laptop() -> HdrMonitorSelection {
        HdrMonitorSelection {
            hdr_supported: false,
            label: "eDP-1".to_string(),
            max_luminance_nits: Some(450.0),
            max_full_frame_luminance_nits: None,
            max_hdr_capacity: None,
            hdr_capacity_source: Some("Wayland wp_color_management"),
            native_surface_encoding: None,
        }
    }

    #[test]
    fn wsi_hdr10_overrides_wp_gamma22_without_peak_nit_heuristic() {
        let merged = linux_effective_monitor_selection(
            Some(&wp_gamma22_laptop()),
            WsiHdrSurfaceGates {
                hdr10_st2084_rgb10a2: true,
                extended_srgb_linear_rgba16f: false,
                probed: true,
            },
        )
        .expect("merged selection");

        assert!(merged.hdr_supported);
        assert_eq!(
            merged.native_surface_encoding,
            Some(HdrNativeSurfaceEncoding::PqHdr10)
        );
        assert_eq!(merged.hdr_capacity_source, Some("Vulkan WSI surface formats"));
        assert_eq!(merged.max_luminance_nits, Some(450.0));
    }

    #[test]
    fn wsi_fail_closed_when_no_hdr_pairs() {
        let merged = linux_effective_monitor_selection(
            Some(&wp_gamma22_laptop()),
            WsiHdrSurfaceGates {
                hdr10_st2084_rgb10a2: false,
                extended_srgb_linear_rgba16f: false,
                probed: true,
            },
        )
        .expect("merged selection");

        assert!(!merged.hdr_supported);
        assert_eq!(merged.native_surface_encoding, None);
    }
}
