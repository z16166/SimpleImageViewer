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

//! Linux native HDR admission: combine Wayland `wp_color_management` with Vulkan WSI gates.
//!
//! WSI alone is **not** sufficient — compositors may advertise HDR swap-chain pairs even on
//! SDR outputs. Admission requires matching compositor metadata (transfer function, primaries,
//! peak vs reference luminance from the probe) with the correct WSI `(format, color_space)` pair.

use super::monitor::{
    HdrMonitorSelection, HdrNativeSurfaceEncoding, LinuxWaylandColorPrimaries,
    LinuxWaylandTransferFunction,
};
use super::wsi_probe::WsiHdrSurfaceGates;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxHdrAdmission {
    Sdr,
    NativePqHdr10,
    NativeGamma22Electrical,
    NativeExtendedScRgb,
}

impl LinuxHdrAdmission {
    pub fn hdr_supported(self) -> bool {
        !matches!(self, Self::Sdr)
    }

    pub fn native_surface_encoding(self) -> Option<HdrNativeSurfaceEncoding> {
        match self {
            Self::Sdr => None,
            Self::NativePqHdr10 => Some(HdrNativeSurfaceEncoding::PqHdr10),
            Self::NativeGamma22Electrical => Some(HdrNativeSurfaceEncoding::Gamma22Electrical),
            Self::NativeExtendedScRgb => Some(HdrNativeSurfaceEncoding::LinearScRgb),
        }
    }

    pub fn capacity_source(self) -> Option<&'static str> {
        match self {
            Self::Sdr => None,
            Self::NativePqHdr10 => Some("Wayland wp + Vulkan WSI PQ"),
            Self::NativeGamma22Electrical => Some("Wayland wp + Vulkan WSI gamma22"),
            Self::NativeExtendedScRgb => Some("Wayland wp + Vulkan WSI scRGB"),
        }
    }

    pub fn as_diagnostic_label(self) -> &'static str {
        match self {
            Self::Sdr => "sdr",
            Self::NativePqHdr10 => "native_pq_hdr10",
            Self::NativeGamma22Electrical => "native_gamma22",
            Self::NativeExtendedScRgb => "native_extended_scrgb",
        }
    }
}

fn wp_explicit_pq_output(selection: &HdrMonitorSelection) -> bool {
    matches!(
        selection.linux_wp_transfer,
        Some(LinuxWaylandTransferFunction::St2084)
    )
}

fn wp_peak_exceeds_reference(selection: &HdrMonitorSelection) -> bool {
    match (
        selection.max_luminance_nits,
        selection.reference_luminance_nits,
    ) {
        (Some(max), Some(reference))
            if max.is_finite() && reference.is_finite() && reference > 0.0 =>
        {
            max > reference
        }
        _ => false,
    }
}

/// True when compositor metadata describes a conventional SDR display profile.
fn wp_is_sdr_display_profile(selection: &HdrMonitorSelection) -> bool {
    if wp_explicit_pq_output(selection) {
        return false;
    }
    match selection.linux_wp_primaries {
        Some(LinuxWaylandColorPrimaries::Narrow) => true,
        Some(LinuxWaylandColorPrimaries::Wide) => false,
        Some(LinuxWaylandColorPrimaries::Unknown) | None => !wp_peak_exceeds_reference(selection),
    }
}

fn wp_supports_kwin_gamma22_offload(selection: &HdrMonitorSelection) -> bool {
    matches!(
        selection.linux_wp_transfer,
        Some(LinuxWaylandTransferFunction::Gamma22)
            | Some(LinuxWaylandTransferFunction::CompoundPower24)
    ) && !wp_is_sdr_display_profile(selection)
}

/// Classify native HDR admission once Vulkan WSI gates are available.
pub fn classify_linux_hdr_admission(
    wp: &HdrMonitorSelection,
    wsi: WsiHdrSurfaceGates,
) -> LinuxHdrAdmission {
    if !wsi.probed {
        return if wp.hdr_supported {
            match wp.native_surface_encoding {
                Some(HdrNativeSurfaceEncoding::PqHdr10) => LinuxHdrAdmission::NativePqHdr10,
                Some(HdrNativeSurfaceEncoding::Gamma22Electrical) => {
                    LinuxHdrAdmission::NativeGamma22Electrical
                }
                Some(HdrNativeSurfaceEncoding::LinearScRgb) => {
                    LinuxHdrAdmission::NativeExtendedScRgb
                }
                None => LinuxHdrAdmission::Sdr,
            }
        } else {
            LinuxHdrAdmission::Sdr
        };
    }

    if wp_explicit_pq_output(wp) && wsi.hdr10_st2084_rgb10a2 {
        return LinuxHdrAdmission::NativePqHdr10;
    }

    if wp_supports_kwin_gamma22_offload(wp) && wsi.srgb_nonlinear_rgb10a2 {
        return LinuxHdrAdmission::NativeGamma22Electrical;
    }

    if !wp_is_sdr_display_profile(wp) && wsi.extended_srgb_linear_rgba16f {
        return LinuxHdrAdmission::NativeExtendedScRgb;
    }

    LinuxHdrAdmission::Sdr
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wp_selection(
        transfer: LinuxWaylandTransferFunction,
        primaries: LinuxWaylandColorPrimaries,
        max_nits: Option<f32>,
        reference_nits: Option<f32>,
    ) -> HdrMonitorSelection {
        let hdr_supported = matches!(transfer, LinuxWaylandTransferFunction::St2084);
        HdrMonitorSelection {
            hdr_supported,
            label: "test-output".to_string(),
            max_luminance_nits: max_nits,
            max_full_frame_luminance_nits: None,
            max_hdr_capacity: None,
            hdr_capacity_source: hdr_supported.then_some("Wayland wp_color_management"),
            native_surface_encoding: hdr_supported.then_some(HdrNativeSurfaceEncoding::PqHdr10),
            reference_luminance_nits: reference_nits,
            linux_wp_transfer: Some(transfer),
            linux_wp_primaries: Some(primaries),
        }
    }

    fn wsi_all_hdr_pairs() -> WsiHdrSurfaceGates {
        WsiHdrSurfaceGates {
            hdr10_st2084_rgb10a2: true,
            extended_srgb_linear_rgba16f: true,
            srgb_nonlinear_rgb10a2: true,
            probed: true,
        }
    }

    #[test]
    fn sdr_profile_vetoes_wsi_hdr10_false_positive() {
        let wp = wp_selection(
            LinuxWaylandTransferFunction::Gamma22,
            LinuxWaylandColorPrimaries::Narrow,
            Some(200.0),
            Some(200.0),
        );
        let admission = classify_linux_hdr_admission(&wp, wsi_all_hdr_pairs());
        assert_eq!(admission, LinuxHdrAdmission::Sdr);
    }

    #[test]
    fn bright_sdr_narrow_gamut_vetoed_despite_high_peak() {
        let wp = wp_selection(
            LinuxWaylandTransferFunction::Gamma22,
            LinuxWaylandColorPrimaries::Narrow,
            Some(600.0),
            Some(600.0),
        );
        let admission = classify_linux_hdr_admission(&wp, wsi_all_hdr_pairs());
        assert_eq!(admission, LinuxHdrAdmission::Sdr);
    }

    #[test]
    fn kwin_gamma22_tv_admits_with_srgb_nonlinear_wsi_pair() {
        let wp = wp_selection(
            LinuxWaylandTransferFunction::Gamma22,
            LinuxWaylandColorPrimaries::Wide,
            Some(1780.0),
            Some(203.0),
        );
        let admission = classify_linux_hdr_admission(&wp, wsi_all_hdr_pairs());
        assert_eq!(admission, LinuxHdrAdmission::NativeGamma22Electrical);
    }

    #[test]
    fn st2084_wp_requires_hdr10_wsi_pair() {
        let wp = wp_selection(
            LinuxWaylandTransferFunction::St2084,
            LinuxWaylandColorPrimaries::Wide,
            Some(1000.0),
            Some(203.0),
        );
        let admission = classify_linux_hdr_admission(
            &wp,
            WsiHdrSurfaceGates {
                hdr10_st2084_rgb10a2: true,
                extended_srgb_linear_rgba16f: false,
                srgb_nonlinear_rgb10a2: false,
                probed: true,
            },
        );
        assert_eq!(admission, LinuxHdrAdmission::NativePqHdr10);
    }

    #[test]
    fn wsi_fail_closed_when_no_hdr_pairs() {
        let wp = wp_selection(
            LinuxWaylandTransferFunction::Gamma22,
            LinuxWaylandColorPrimaries::Wide,
            Some(1780.0),
            Some(203.0),
        );
        let admission = classify_linux_hdr_admission(
            &wp,
            WsiHdrSurfaceGates {
                hdr10_st2084_rgb10a2: false,
                extended_srgb_linear_rgba16f: false,
                srgb_nonlinear_rgb10a2: false,
                probed: true,
            },
        );
        assert_eq!(admission, LinuxHdrAdmission::Sdr);
    }

    #[test]
    fn unprobed_wsi_defers_to_wp_st2084_only() {
        let wp = wp_selection(
            LinuxWaylandTransferFunction::St2084,
            LinuxWaylandColorPrimaries::Wide,
            Some(1000.0),
            Some(203.0),
        );
        let admission = classify_linux_hdr_admission(
            &wp,
            WsiHdrSurfaceGates {
                probed: false,
                ..Default::default()
            },
        );
        assert_eq!(admission, LinuxHdrAdmission::NativePqHdr10);
    }
}
