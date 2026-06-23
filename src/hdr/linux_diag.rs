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

//! Linux HDR capability diagnostics at default log level (`info`).
//!
//! Logs are emitted only when the merged runtime snapshot changes (Wayland wp probe,
//! Vulkan WSI gates, admission, settings gate, swap-chain target). Not gated on
//! `preload-debug`.

use super::linux_admission::{self, LinuxHdrAdmission};
use super::monitor::{
    HdrMonitorSelection, HdrNativeSurfaceEncoding, LinuxWaylandColorPrimaries,
    LinuxWaylandTransferFunction,
};
use super::types::HdrOutputMode;
use super::wsi_probe::WsiHdrSurfaceGates;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LinuxHdrRuntimeDiagSnapshot {
    wp_present: bool,
    wp_label: Option<String>,
    wp_hdr_supported: Option<bool>,
    wp_transfer: Option<LinuxWaylandTransferFunction>,
    wp_primaries: Option<LinuxWaylandColorPrimaries>,
    wp_max_luminance_nits: Option<u32>,
    wp_reference_luminance_nits: Option<u32>,
    wsi_probed: bool,
    wsi_hdr10_st2084_rgb10a2: bool,
    wsi_srgb_nonlinear_rgb10a2: bool,
    wsi_extended_srgb_linear_rgba16f: bool,
    admission: LinuxHdrAdmission,
    effective_hdr_supported: Option<bool>,
    effective_encoding: Option<HdrNativeSurfaceEncoding>,
    effective_capacity_source: Option<String>,
    effective_max_luminance_nits: Option<u32>,
    settings_native_surface_enabled: bool,
    settings_native_surface_effective: bool,
    native_swapchain_requests_enabled: bool,
    target_format: Option<wgpu::TextureFormat>,
    desired_target_format: Option<wgpu::TextureFormat>,
    output_mode: HdrOutputMode,
    native_presentation_enabled: bool,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct LinuxHdrRuntimeDiagInput<'a> {
    pub wp: Option<&'a HdrMonitorSelection>,
    pub effective: Option<&'a HdrMonitorSelection>,
    pub wsi: WsiHdrSurfaceGates,
    pub settings_native_surface_enabled: bool,
    pub settings_native_surface_effective: bool,
    pub native_swapchain_requests_enabled: bool,
    pub target_format: Option<wgpu::TextureFormat>,
    pub desired_target_format: Option<wgpu::TextureFormat>,
    pub output_mode: HdrOutputMode,
    pub native_presentation_enabled: bool,
}

fn finite_f32_key(value: Option<f32>) -> Option<u32> {
    value
        .filter(|v| v.is_finite())
        .map(|v| v.to_bits())
}

fn snapshot_from_input(input: LinuxHdrRuntimeDiagInput<'_>) -> LinuxHdrRuntimeDiagSnapshot {
    let wp = input.wp.cloned();
    let admission = wp
        .as_ref()
        .map(|selection| linux_admission::classify_linux_hdr_admission(selection, input.wsi))
        .unwrap_or(LinuxHdrAdmission::Sdr);
    let effective = input.effective.cloned();

    LinuxHdrRuntimeDiagSnapshot {
        wp_present: wp.is_some(),
        wp_label: wp.as_ref().map(|s| s.label.clone()),
        wp_hdr_supported: wp.as_ref().map(|s| s.hdr_supported),
        wp_transfer: wp.as_ref().and_then(|s| s.linux_wp_transfer),
        wp_primaries: wp.as_ref().and_then(|s| s.linux_wp_primaries),
        wp_max_luminance_nits: finite_f32_key(wp.as_ref().and_then(|s| s.max_luminance_nits)),
        wp_reference_luminance_nits: finite_f32_key(
            wp.as_ref().and_then(|s| s.reference_luminance_nits),
        ),
        wsi_probed: input.wsi.probed,
        wsi_hdr10_st2084_rgb10a2: input.wsi.hdr10_st2084_rgb10a2,
        wsi_srgb_nonlinear_rgb10a2: input.wsi.srgb_nonlinear_rgb10a2,
        wsi_extended_srgb_linear_rgba16f: input.wsi.extended_srgb_linear_rgba16f,
        admission,
        effective_hdr_supported: effective.as_ref().map(|s| s.hdr_supported),
        effective_encoding: effective
            .as_ref()
            .and_then(|s| s.native_surface_encoding),
        effective_capacity_source: effective
            .as_ref()
            .and_then(|s| s.hdr_capacity_source.map(str::to_string)),
        effective_max_luminance_nits: finite_f32_key(
            effective.as_ref().and_then(|s| s.max_luminance_nits),
        ),
        settings_native_surface_enabled: input.settings_native_surface_enabled,
        settings_native_surface_effective: input.settings_native_surface_effective,
        native_swapchain_requests_enabled: input.native_swapchain_requests_enabled,
        target_format: input.target_format,
        desired_target_format: input.desired_target_format,
        output_mode: input.output_mode,
        native_presentation_enabled: input.native_presentation_enabled,
    }
}

/// Log the three-layer Linux HDR snapshot when any contributing signal changes.
pub(crate) fn log_runtime_if_changed(
    last: &mut Option<LinuxHdrRuntimeDiagSnapshot>,
    input: LinuxHdrRuntimeDiagInput<'_>,
) {
    let snapshot = snapshot_from_input(input);
    if last.as_ref() == Some(&snapshot) {
        return;
    }
    *last = Some(snapshot.clone());

    if snapshot.wp_present {
        log::info!(
            "[HDR] display: output={} wp_hdr={} transfer={:?} primaries={:?} \
             max_luminance_nits={:?} reference_luminance_nits={:?}",
            snapshot.wp_label.as_deref().unwrap_or("(unknown)"),
            snapshot.wp_hdr_supported.unwrap_or(false),
            snapshot.wp_transfer,
            snapshot.wp_primaries,
            f32_from_key(snapshot.wp_max_luminance_nits),
            f32_from_key(snapshot.wp_reference_luminance_nits),
        );
    } else {
        log::info!("[HDR] display: wp_color_management probe pending or unavailable");
    }

    log::info!(
        "[HDR] compositor_wsi: probed={} hdr10_st2084_rgb10a2={} \
         srgb_nonlinear_rgb10a2={} extended_srgb_linear_rgba16f={}",
        snapshot.wsi_probed,
        snapshot.wsi_hdr10_st2084_rgb10a2,
        snapshot.wsi_srgb_nonlinear_rgb10a2,
        snapshot.wsi_extended_srgb_linear_rgba16f,
    );

    log::info!(
        "[HDR] admission: decision={} hdr_supported_effective={} encoding={:?} \
         capacity_source={:?} max_luminance_nits={:?}",
        snapshot.admission.as_diagnostic_label(),
        snapshot.effective_hdr_supported,
        snapshot.effective_encoding,
        snapshot.effective_capacity_source.as_deref(),
        f32_from_key(snapshot.effective_max_luminance_nits),
    );

    log::info!(
        "[HDR] app_active: output_mode={:?} native_presentation={} target_format={:?} \
         desired_target_format={:?} settings_native_surface_enabled={} \
         settings_native_effective={} native_swapchain_requests={}",
        snapshot.output_mode,
        snapshot.native_presentation_enabled,
        snapshot.target_format,
        snapshot.desired_target_format,
        snapshot.settings_native_surface_enabled,
        snapshot.settings_native_surface_effective,
        snapshot.native_swapchain_requests_enabled,
    );

    if snapshot.effective_hdr_supported == Some(true)
        && !snapshot.native_swapchain_requests_enabled
    {
        log::info!(
            "[HDR] native HDR admission passed but swap-chain requests are disabled \
             (check Settings > HDR native surface, or Linux session eligibility)"
        );
    }
}

pub(crate) fn log_session_startup(
    settings_native_surface_enabled: bool,
    settings_native_surface_effective: bool,
    output_mode: HdrOutputMode,
) {
    log::info!(
        "[HDR] linux session: wayland={} hdr_platform_eligible={} \
         settings_native_surface_enabled={} settings_native_effective={} output_mode={:?}",
        super::platform::is_wayland_session(),
        super::platform::linux_native_hdr_platform_eligible(),
        settings_native_surface_enabled,
        settings_native_surface_effective,
        output_mode,
    );
    if !super::platform::linux_native_hdr_platform_eligible() {
        log::info!(
            "[HDR] linux session: native HDR swap-chain unavailable on this session \
             (X11 or non-Wayland); HDR images use tone-mapped SDR output"
        );
    }
}

fn f32_from_key(bits: Option<u32>) -> Option<f32> {
    bits.map(f32::from_bits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_diag_dedupes_identical_snapshots() {
        let wp = HdrMonitorSelection {
            hdr_supported: false,
            label: "eDP-1".to_string(),
            max_luminance_nits: Some(450.0),
            max_full_frame_luminance_nits: None,
            max_hdr_capacity: None,
            hdr_capacity_source: Some("Wayland wp_color_management"),
            native_surface_encoding: None,
            reference_luminance_nits: Some(210.0),
            linux_wp_transfer: Some(LinuxWaylandTransferFunction::Gamma22),
            linux_wp_primaries: Some(LinuxWaylandColorPrimaries::Wide),
        };
        let wsi = WsiHdrSurfaceGates {
            hdr10_st2084_rgb10a2: true,
            extended_srgb_linear_rgba16f: true,
            srgb_nonlinear_rgb10a2: true,
            probed: true,
        };
        let effective = super::super::wsi_probe::linux_effective_monitor_selection(
            Some(&wp),
            wsi,
        )
        .expect("effective selection");
        let input = LinuxHdrRuntimeDiagInput {
            wp: Some(&wp),
            effective: Some(&effective),
            wsi,
            settings_native_surface_enabled: true,
            settings_native_surface_effective: true,
            native_swapchain_requests_enabled: true,
            target_format: Some(wgpu::TextureFormat::Rgb10a2Unorm),
            desired_target_format: Some(wgpu::TextureFormat::Rgb10a2Unorm),
            output_mode: HdrOutputMode::WaylandHdr,
            native_presentation_enabled: true,
        };
        let mut last = None;
        log_runtime_if_changed(&mut last, input.clone());
        assert!(last.is_some());
        let cached = last.clone();
        log_runtime_if_changed(&mut last, input);
        assert_eq!(last, cached);
    }
}
