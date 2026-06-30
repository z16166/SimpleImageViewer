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
//! SDR outputs. Admission requires an explicit compositor / desktop HDR signal; WSI then chooses
//! the concrete `(format, color_space)` encoding, preferring ST2084 when available.
//!
//! Explicit desktop HDR state is currently sourced from KDE `kscreen-doctor` only; other Linux
//! desktops (GNOME, Sway, etc.) fail closed until a comparable explicit signal is integrated.

use super::monitor::{HdrMonitorSelection, HdrNativeSurfaceEncoding, LinuxWaylandTransferFunction};
use super::wsi_probe::WsiHdrSurfaceGates;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxHdrAdmission {
    Sdr,
    NativePqHdr10,
    NativeGamma22Electrical,
    NativeExtendedScRgb,
}

#[cfg(target_os = "linux")]
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

/// True when Wayland `wp_color_management` reports ST2084/PQ transfer on the output.
/// This reflects compositor metadata, not the final Vulkan WSI swapchain encoding.
fn wp_reports_st2084_transfer(selection: &HdrMonitorSelection) -> bool {
    matches!(
        selection.linux_wp_transfer,
        Some(LinuxWaylandTransferFunction::St2084)
    )
}

fn wp_describes_gamma22_offload(selection: &HdrMonitorSelection) -> bool {
    matches!(
        selection.linux_wp_transfer,
        Some(LinuxWaylandTransferFunction::Gamma22)
            | Some(LinuxWaylandTransferFunction::CompoundPower24)
    )
}

fn explicit_desktop_hdr_enabled(selection: &HdrMonitorSelection) -> bool {
    matches!(
        selection.linux_explicit_hdr_state,
        Some(super::monitor::LinuxExplicitHdrState::Enabled)
    )
}

fn explicit_desktop_hdr_disabled(selection: &HdrMonitorSelection) -> bool {
    matches!(
        selection.linux_explicit_hdr_state,
        Some(
            super::monitor::LinuxExplicitHdrState::Disabled
                | super::monitor::LinuxExplicitHdrState::Incapable
        )
    )
}

/// Classify native HDR admission once Vulkan WSI gates are available.
pub fn classify_linux_hdr_admission(
    wp: &HdrMonitorSelection,
    wsi: WsiHdrSurfaceGates,
) -> LinuxHdrAdmission {
    if explicit_desktop_hdr_disabled(wp) {
        return LinuxHdrAdmission::Sdr;
    }

    // WSI gates are not ready yet: trust wp seed metadata only for explicit PQ (St2084) paths.
    // Gamma22 / scRGB HDR TVs stay SDR until WSI confirms the matching swap-chain pair, avoiding
    // false positives when compositors advertise HDR pairs on SDR outputs (walkthrough §6).
    if !wsi.probed {
        return if wp.hdr_supported {
            match wp.native_surface_encoding {
                Some(HdrNativeSurfaceEncoding::PqHdr10) => LinuxHdrAdmission::NativePqHdr10,
                // Non-PQ native paths require WSI confirmation; otherwise fail closed.
                Some(
                    HdrNativeSurfaceEncoding::Gamma22Electrical
                    | HdrNativeSurfaceEncoding::LinearScRgb,
                )
                | None => LinuxHdrAdmission::Sdr,
            }
        } else {
            LinuxHdrAdmission::Sdr
        };
    }

    let native_hdr_admitted = wp_reports_st2084_transfer(wp) || explicit_desktop_hdr_enabled(wp);
    if !native_hdr_admitted {
        return LinuxHdrAdmission::Sdr;
    }

    if wsi.hdr10_st2084_rgb10a2 {
        return LinuxHdrAdmission::NativePqHdr10;
    }

    if wp_describes_gamma22_offload(wp) && wsi.srgb_nonlinear_rgb10a2 {
        return LinuxHdrAdmission::NativeGamma22Electrical;
    }

    if wsi.extended_srgb_linear_rgba16f {
        return LinuxHdrAdmission::NativeExtendedScRgb;
    }

    LinuxHdrAdmission::Sdr
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hdr::monitor::LinuxWaylandColorPrimaries;

    fn wp_selection(
        transfer: LinuxWaylandTransferFunction,
        primaries: LinuxWaylandColorPrimaries,
        max_nits: Option<f32>,
        reference_nits: Option<f32>,
    ) -> HdrMonitorSelection {
        wp_selection_for_label("test-output", transfer, primaries, max_nits, reference_nits)
    }

    fn wp_selection_for_label(
        label: impl Into<String>,
        transfer: LinuxWaylandTransferFunction,
        primaries: LinuxWaylandColorPrimaries,
        max_nits: Option<f32>,
        reference_nits: Option<f32>,
    ) -> HdrMonitorSelection {
        let hdr_supported = matches!(transfer, LinuxWaylandTransferFunction::St2084);
        HdrMonitorSelection {
            hdr_supported,
            label: label.into(),
            max_luminance_nits: max_nits,
            max_full_frame_luminance_nits: None,
            max_hdr_capacity: None,
            hdr_capacity_source: hdr_supported.then_some("Wayland wp_color_management"),
            native_surface_encoding: hdr_supported.then_some(HdrNativeSurfaceEncoding::PqHdr10),
            reference_luminance_nits: reference_nits,
            linux_wp_transfer: Some(transfer),
            linux_wp_primaries: Some(primaries),
            linux_explicit_hdr_state: None,
            linux_explicit_hdr_state_source: None,
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
    fn gamma22_without_explicit_hdr_state_vetoes_wsi_hdr10_false_positive() {
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
    fn high_luminance_metadata_does_not_enable_hdr_without_explicit_state() {
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
    fn gamma22_output_without_explicit_hdr_state_fails_closed() {
        let wp = wp_selection(
            LinuxWaylandTransferFunction::Gamma22,
            LinuxWaylandColorPrimaries::Wide,
            Some(1780.0),
            Some(203.0),
        );
        let admission = classify_linux_hdr_admission(&wp, wsi_all_hdr_pairs());
        assert_eq!(admission, LinuxHdrAdmission::Sdr);
    }

    #[test]
    fn kde_enabled_gamma22_output_prefers_st2084_wsi_pair() {
        let mut wp = wp_selection(
            LinuxWaylandTransferFunction::Gamma22,
            LinuxWaylandColorPrimaries::Wide,
            Some(1800.0),
            Some(1800.0),
        );
        wp.linux_explicit_hdr_state = Some(crate::hdr::monitor::LinuxExplicitHdrState::Enabled);
        wp.linux_explicit_hdr_state_source = Some("KDE KScreen");

        let admission = classify_linux_hdr_admission(&wp, wsi_all_hdr_pairs());
        assert_eq!(admission, LinuxHdrAdmission::NativePqHdr10);
    }

    #[test]
    fn kde_disabled_gamma22_output_vetoes_st2084_wsi_pair() {
        let mut wp = wp_selection(
            LinuxWaylandTransferFunction::Gamma22,
            LinuxWaylandColorPrimaries::Wide,
            Some(1800.0),
            Some(1800.0),
        );
        wp.linux_explicit_hdr_state = Some(crate::hdr::monitor::LinuxExplicitHdrState::Disabled);
        wp.linux_explicit_hdr_state_source = Some("KDE KScreen");

        let admission = classify_linux_hdr_admission(&wp, wsi_all_hdr_pairs());
        assert_eq!(admission, LinuxHdrAdmission::Sdr);
    }

    #[test]
    fn gamma22_with_st2084_wsi_fails_closed_without_explicit_hdr_state() {
        let wp = wp_selection(
            LinuxWaylandTransferFunction::Gamma22,
            LinuxWaylandColorPrimaries::Wide,
            Some(1800.0),
            Some(1800.0),
        );

        let admission = classify_linux_hdr_admission(&wp, wsi_all_hdr_pairs());
        assert_eq!(admission, LinuxHdrAdmission::Sdr);
    }

    #[test]
    fn kde_enabled_gamma22_falls_back_to_gamma22_without_st2084_pair() {
        let mut wp = wp_selection(
            LinuxWaylandTransferFunction::Gamma22,
            LinuxWaylandColorPrimaries::Wide,
            Some(1800.0),
            Some(1800.0),
        );
        wp.linux_explicit_hdr_state = Some(crate::hdr::monitor::LinuxExplicitHdrState::Enabled);
        wp.linux_explicit_hdr_state_source = Some("KDE KScreen");

        let admission = classify_linux_hdr_admission(
            &wp,
            WsiHdrSurfaceGates {
                hdr10_st2084_rgb10a2: false,
                extended_srgb_linear_rgba16f: false,
                srgb_nonlinear_rgb10a2: true,
                probed: true,
            },
        );
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

    #[test]
    fn explicit_disabled_vetoes_wp_st2084_before_wsi_probe() {
        let mut wp = wp_selection(
            LinuxWaylandTransferFunction::St2084,
            LinuxWaylandColorPrimaries::Wide,
            Some(1000.0),
            Some(203.0),
        );
        wp.linux_explicit_hdr_state = Some(crate::hdr::monitor::LinuxExplicitHdrState::Disabled);
        wp.linux_explicit_hdr_state_source = Some("KDE KScreen");
        let admission = classify_linux_hdr_admission(
            &wp,
            WsiHdrSurfaceGates {
                probed: false,
                ..Default::default()
            },
        );
        assert_eq!(admission, LinuxHdrAdmission::Sdr);
    }

    #[test]
    fn compound_power24_wide_peak_admits_extended_scrgb() {
        let mut wp = wp_selection(
            LinuxWaylandTransferFunction::CompoundPower24,
            LinuxWaylandColorPrimaries::Wide,
            Some(1000.0),
            Some(203.0),
        );
        wp.linux_explicit_hdr_state = Some(crate::hdr::monitor::LinuxExplicitHdrState::Enabled);
        wp.linux_explicit_hdr_state_source = Some("KDE KScreen");
        let admission = classify_linux_hdr_admission(
            &wp,
            WsiHdrSurfaceGates {
                hdr10_st2084_rgb10a2: false,
                extended_srgb_linear_rgba16f: true,
                srgb_nonlinear_rgb10a2: false,
                probed: true,
            },
        );
        assert_eq!(admission, LinuxHdrAdmission::NativeExtendedScRgb);
    }

    #[test]
    fn explicit_hdr_state_admits_extended_scrgb() {
        let mut wp = wp_selection(
            LinuxWaylandTransferFunction::Gamma22,
            LinuxWaylandColorPrimaries::Unknown,
            Some(800.0),
            Some(203.0),
        );
        wp.linux_explicit_hdr_state = Some(crate::hdr::monitor::LinuxExplicitHdrState::Enabled);
        wp.linux_explicit_hdr_state_source = Some("KDE KScreen");
        let admission = classify_linux_hdr_admission(
            &wp,
            WsiHdrSurfaceGates {
                hdr10_st2084_rgb10a2: false,
                extended_srgb_linear_rgba16f: true,
                srgb_nonlinear_rgb10a2: false,
                probed: true,
            },
        );
        assert_eq!(admission, LinuxHdrAdmission::NativeExtendedScRgb);
    }
}
