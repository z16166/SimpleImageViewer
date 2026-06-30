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

//! Linux Vulkan WSI `(format, color_space)` gates for native HDR presentation.

use super::monitor::HdrMonitorSelection;

/// Subset of [`wgpu_hal::linux_surface_probe::VulkanHdrSurfaceProbe`] published by egui-wgpu.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WsiHdrSurfaceGates {
    pub hdr10_st2084_rgb10a2: bool,
    pub extended_srgb_linear_rgba16f: bool,
    /// `A2B10G10R10` + `SRGB_NONLINEAR` — KWin gamma-2.2 electrical offload path.
    pub srgb_nonlinear_rgb10a2: bool,
    /// Set after the first successful `vkGetPhysicalDeviceSurfaceFormatsKHR` probe.
    pub probed: bool,
}

/// Combine Wayland `wp_color_management` metadata with Vulkan WSI surface gates.
///
/// WSI availability is **necessary** but not sufficient: compositors may list HDR
/// `(format, color_space)` pairs on SDR outputs. See [`crate::hdr::linux_admission`].
#[cfg(target_os = "linux")]
pub fn linux_effective_monitor_selection(
    wp: Option<&HdrMonitorSelection>,
    wsi: WsiHdrSurfaceGates,
) -> Option<HdrMonitorSelection> {
    let wp = wp.cloned()?;
    let admission = crate::hdr::linux_admission::classify_linux_hdr_admission(&wp, wsi);
    let hdr_supported = admission.hdr_supported();
    Some(HdrMonitorSelection {
        hdr_supported,
        native_surface_encoding: admission.native_surface_encoding(),
        // Use admission-derived source only; wp probe source can remain set while WSI vetoed SDR.
        hdr_capacity_source: admission.capacity_source(),
        label: wp.label,
        max_luminance_nits: wp.max_luminance_nits,
        max_full_frame_luminance_nits: wp.max_full_frame_luminance_nits,
        max_hdr_capacity: wp.max_hdr_capacity,
        reference_luminance_nits: wp.reference_luminance_nits,
        linux_wp_transfer: wp.linux_wp_transfer,
        linux_wp_primaries: wp.linux_wp_primaries,
        linux_explicit_hdr_state: wp.linux_explicit_hdr_state,
        linux_explicit_hdr_state_source: wp.linux_explicit_hdr_state_source,
    })
}

#[cfg(not(target_os = "linux"))]
pub fn linux_effective_monitor_selection(
    wp: Option<&HdrMonitorSelection>,
    _wsi: WsiHdrSurfaceGates,
) -> Option<HdrMonitorSelection> {
    wp.cloned()
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::{WsiHdrSurfaceGates, linux_effective_monitor_selection};
    use crate::hdr::monitor::{
        HdrMonitorSelection, HdrNativeSurfaceEncoding, LinuxWaylandColorPrimaries,
        LinuxWaylandTransferFunction,
    };

    fn wp_gamma22_tv() -> HdrMonitorSelection {
        HdrMonitorSelection {
            hdr_supported: false,
            label: "HDMI-A-1".to_string(),
            max_luminance_nits: Some(1780.0),
            max_full_frame_luminance_nits: None,
            max_hdr_capacity: None,
            hdr_capacity_source: Some("Wayland wp_color_management"),
            native_surface_encoding: None,
            reference_luminance_nits: Some(203.0),
            linux_wp_transfer: Some(LinuxWaylandTransferFunction::Gamma22),
            linux_wp_primaries: Some(LinuxWaylandColorPrimaries::Wide),
            linux_explicit_hdr_state: Some(super::super::monitor::LinuxExplicitHdrState::Enabled),
            linux_explicit_hdr_state_source: Some("KDE KScreen"),
        }
    }

    fn wp_sdr_monitor() -> HdrMonitorSelection {
        HdrMonitorSelection {
            hdr_supported: false,
            label: "HDMI-A-3".to_string(),
            max_luminance_nits: Some(200.0),
            max_full_frame_luminance_nits: None,
            max_hdr_capacity: None,
            hdr_capacity_source: Some("Wayland wp_color_management"),
            native_surface_encoding: None,
            reference_luminance_nits: Some(200.0),
            linux_wp_transfer: Some(LinuxWaylandTransferFunction::Gamma22),
            linux_wp_primaries: Some(LinuxWaylandColorPrimaries::Narrow),
            linux_explicit_hdr_state: Some(super::super::monitor::LinuxExplicitHdrState::Incapable),
            linux_explicit_hdr_state_source: Some("KDE KScreen"),
        }
    }

    #[test]
    fn merge_prefers_pq_for_kde_enabled_output_when_st2084_pair_exists() {
        let merged = linux_effective_monitor_selection(
            Some(&wp_gamma22_tv()),
            WsiHdrSurfaceGates {
                hdr10_st2084_rgb10a2: true,
                extended_srgb_linear_rgba16f: false,
                srgb_nonlinear_rgb10a2: true,
                probed: true,
            },
        )
        .expect("merged selection");

        assert!(merged.hdr_supported);
        assert_eq!(
            merged.native_surface_encoding,
            Some(HdrNativeSurfaceEncoding::PqHdr10)
        );
        assert_eq!(
            merged.hdr_capacity_source,
            Some("Wayland wp + Vulkan WSI PQ")
        );
    }

    #[test]
    fn merge_vetoes_sdr_wp_despite_wsi_hdr10_pair() {
        let merged = linux_effective_monitor_selection(
            Some(&wp_sdr_monitor()),
            WsiHdrSurfaceGates {
                hdr10_st2084_rgb10a2: true,
                extended_srgb_linear_rgba16f: true,
                srgb_nonlinear_rgb10a2: true,
                probed: true,
            },
        )
        .expect("merged selection");

        assert!(!merged.hdr_supported);
        assert_eq!(merged.native_surface_encoding, None);
        assert_eq!(merged.hdr_capacity_source, None);
    }

    #[test]
    fn wsi_fail_closed_when_no_hdr_pairs() {
        let merged = linux_effective_monitor_selection(
            Some(&wp_gamma22_tv()),
            WsiHdrSurfaceGates {
                hdr10_st2084_rgb10a2: false,
                extended_srgb_linear_rgba16f: false,
                srgb_nonlinear_rgb10a2: false,
                probed: true,
            },
        )
        .expect("merged selection");

        assert!(!merged.hdr_supported);
        assert_eq!(merged.native_surface_encoding, None);
        assert_eq!(merged.hdr_capacity_source, None);
    }
}
