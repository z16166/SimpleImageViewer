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

use super::*;

#[cfg(target_os = "windows")]
#[test]
fn dxgi_output_hdr_active_only_gates_on_pq_colorspace_not_bit_depth() {
    // Regression: the LC49G95T (5120×1440 HDR ultrawide) reports
    // `BitsPerColor=8 ColorSpace=G2084_NONE_P2020` when Windows HDR is
    // enabled and the DP / HDMI link runs at 8-bit + dithering. The
    // previous probe required `BitsPerColor > 8` and therefore declared
    // such monitors SDR, which silently locked the swap chain into
    // `Bgra8Unorm` and disabled the entire HDR rendering path. Only the
    // `ColorSpace` may be used to decide whether HDR is active.
    assert!(
        super::dxgi_output_hdr_active(DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020),
        "G2084 / PQ output must always be classified as HDR-active even \
         when the panel link is at 8 BPC + dithering"
    );
    assert!(
        !super::dxgi_output_hdr_active(DXGI_COLOR_SPACE_TYPE(0)),
        "sRGB G22 (DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709 = 0) must NOT \
         be classified as HDR-active"
    );
    assert!(
        !super::dxgi_output_hdr_active(DXGI_COLOR_SPACE_TYPE(1)),
        "linear scRGB (DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709 = 1) is \
         commonly used during HDR composition but NOT as the panel \
         output colour space; do not classify as HDR-active here"
    );
}

#[test]
fn monitor_probe_runs_first_time_immediately_on_signature_change() {
    let start = Instant::now();
    let first = HdrMonitorSignature {
        outer_rect: Some([0, 0, 100, 100]),
        monitor_size: Some([1920, 1080]),
        native_pixels_per_point_milli: Some(1000),
    };
    let moved = HdrMonitorSignature {
        outer_rect: Some([2000, 0, 2100, 100]),
        ..first
    };
    let mut state = HdrMonitorState::default();

    assert!(state.should_probe(first, start, false));
    state.last_signature = Some(first);
    state.last_probe_at = Some(start);
    if cfg!(target_os = "macos") {
        // Same signature: macOS re-probes for EDR capacity on the timer while still unknown.
        assert!(!state.should_probe(first, start + HDR_MONITOR_PROBE_INTERVAL * 2, false));
    } else {
        // Windows: timer reprobe even when the viewport signature is unchanged (outer rect
        // can lag during native drags).
        assert!(state.should_probe(first, start + HDR_MONITOR_PROBE_INTERVAL * 2, false));
    }
    // Cross-monitor style rect change must not wait 750 ms — swap-chain format may already
    // differ from `selection` within the same second.
    assert!(state.should_probe(moved, start + Duration::from_millis(100), false));
}

#[test]
fn macos_current_edr_reprobe_uses_interval_without_viewport_change() {
    let start = Instant::now();
    let signature = HdrMonitorSignature {
        outer_rect: Some([0, 0, 100, 100]),
        monitor_size: Some([1920, 1080]),
        native_pixels_per_point_milli: Some(1000),
    };
    let mut state = HdrMonitorState::default();
    state.last_signature = Some(signature);
    state.last_probe_at = Some(start);
    state.selection = Some(macos_edr_selection_from_values(
        "Built-in XDR".to_string(),
        1.0,
        16.0,
        0.0,
    ));

    assert!(!state.should_probe_for_platform(
        signature,
        start + Duration::from_millis(100),
        true,
        true
    ));
    assert!(state.should_probe_for_platform(
        signature,
        start + HDR_MONITOR_PROBE_INTERVAL,
        true,
        true
    ));
    assert!(state.should_probe_for_platform(
        signature,
        start + HDR_MONITOR_PROBE_INTERVAL,
        false,
        true
    ));
    // `supports_current_edr_reprobe == false` is the Windows/Linux call shape: same
    // signature still schedules DXGI refresh on the timer (see `should_probe_for_platform`).
    assert!(state.should_probe_for_platform(
        signature,
        start + HDR_MONITOR_PROBE_INTERVAL,
        true,
        false
    ));
}

#[test]
fn non_hdr_selected_monitor_forces_sdr_tone_mapping_on_float_surface() {
    let non_hdr = HdrMonitorSelection {
        hdr_supported: false,
        label: "SDR".to_string(),
        max_luminance_nits: None,
        max_full_frame_luminance_nits: None,
        max_hdr_capacity: None,
        hdr_capacity_source: None,
        native_surface_encoding: None,
        ..HdrMonitorSelection::new("", false)
    };
    let hdr = HdrMonitorSelection {
        hdr_supported: true,
        label: "HDR".to_string(),
        max_luminance_nits: Some(1000.0),
        max_full_frame_luminance_nits: Some(500.0),
        max_hdr_capacity: None,
        hdr_capacity_source: Some("Windows DXGI MaxLuminance"),
        native_surface_encoding: Some(HdrNativeSurfaceEncoding::LinearScRgb),
        ..HdrMonitorSelection::new("", false)
    };

    assert_eq!(
        effective_render_output_mode(Some(wgpu::TextureFormat::Rgba16Float), Some(&non_hdr)),
        HdrRenderOutputMode::SdrToneMapped
    );
    assert_eq!(
        effective_render_output_mode(Some(wgpu::TextureFormat::Rgba16Float), Some(&hdr)),
        HdrRenderOutputMode::NativeHdr
    );
    assert_eq!(
        effective_render_output_mode(Some(wgpu::TextureFormat::Bgra8Unorm), Some(&hdr)),
        HdrRenderOutputMode::SdrToneMapped
    );
}

#[test]
fn macos_edr_values_build_capacity_based_monitor_selection() {
    let selection = macos_edr_selection_from_values("Built-in XDR".to_string(), 2.2, 4.0, 1.5);

    assert!(selection.hdr_supported);
    assert_eq!(selection.label, "Built-in XDR");
    assert_eq!(selection.max_hdr_capacity, Some(2.2));
    assert_eq!(
        selection.hdr_capacity_source,
        Some("macOS maximumExtendedDynamicRangeColorComponentValue")
    );
    assert_eq!(selection.max_luminance_nits, None);
    assert_eq!(selection.max_full_frame_luminance_nits, None);
}

#[test]
fn macos_sdr_edr_values_build_non_hdr_monitor_selection() {
    let selection = macos_edr_selection_from_values("SDR".to_string(), 1.0, 1.0, 0.0);

    assert!(!selection.hdr_supported);
    assert_eq!(selection.max_hdr_capacity, None);
}

#[test]
fn macos_potential_edr_only_does_not_force_decode_capacity() {
    let selection = macos_edr_selection_from_values("Built-in XDR".to_string(), 1.0, 16.0, 0.0);

    assert!(selection.hdr_supported);
    assert_eq!(selection.max_hdr_capacity, None);
    assert_eq!(selection.hdr_capacity_source, None);
}

#[cfg(target_os = "macos")]
#[test]
fn macos_edr_selected_monitor_enables_macos_edr_on_float_surface() {
    let selection = macos_edr_selection_from_values("Built-in XDR".to_string(), 2.2, 4.0, 1.5);

    assert_eq!(
        effective_capability_output_mode(Some(wgpu::TextureFormat::Rgba16Float), Some(&selection)),
        HdrOutputMode::MacOsEdr
    );
}

#[test]
fn linux_wayland_eligibility_gates_probe_error_message() {
    assert!(
        !crate::hdr::platform::wayland_session_from_display_var(Some(":0")),
        "X11-style display should not be treated as Wayland"
    );
    #[cfg(target_os = "linux")]
    if !crate::hdr::platform::linux_native_hdr_platform_eligible() {
        let err = active_monitor_hdr_status(None, None, None, None).unwrap_err();
        assert!(
            err.contains("Wayland session"),
            "expected Wayland gate error, got: {err}"
        );
        let spawn_err = spawn_monitor_hdr_status(None).unwrap_err();
        assert!(
            spawn_err.contains("Wayland session"),
            "expected Wayland gate error, got: {spawn_err}"
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn macos_non_edr_selected_monitor_forces_sdr_tone_mapping() {
    let selection = macos_edr_selection_from_values("SDR".to_string(), 1.0, 1.0, 0.0);

    assert_eq!(
        effective_capability_output_mode(Some(wgpu::TextureFormat::Rgba16Float), Some(&selection)),
        HdrOutputMode::SdrToneMapped
    );
}
