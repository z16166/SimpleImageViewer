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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HdrSurfaceSelection {
    NativeHdr(wgpu::TextureFormat),
    Unavailable { reason: &'static str },
}

pub fn choose_native_hdr_surface_format(formats: &[wgpu::TextureFormat]) -> HdrSurfaceSelection {
    for preferred in [
        wgpu::TextureFormat::Rgba16Float,
        wgpu::TextureFormat::Rgba32Float,
    ] {
        if formats.contains(&preferred) {
            return HdrSurfaceSelection::NativeHdr(preferred);
        }
    }

    HdrSurfaceSelection::Unavailable {
        reason: "surface exposes no float HDR presentation format",
    }
}

pub fn preferred_native_hdr_target_format_for_platform() -> Option<wgpu::TextureFormat> {
    if cfg!(any(target_os = "windows", target_os = "macos")) {
        Some(wgpu::TextureFormat::Rgba16Float)
    } else {
        None
    }
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn preferred_native_hdr_target_format_for_settings(
    native_surface_enabled: bool,
) -> Option<wgpu::TextureFormat> {
    if native_surface_enabled {
        preferred_native_hdr_target_format_for_platform()
    } else {
        None
    }
}

/// Outcome of checking the *spawn monitor* (cursor / primary) for HDR support
/// before window creation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HdrEnvironmentProbe {
    /// The monitor where the window is expected to spawn reports active HDR
    /// signaling — keep the `Rgba16Float` swap chain so we can drive scRGB
    /// native presentation.
    SpawnMonitorHdr {
        label: String,
        origin: &'static str,
    },
    /// The monitor where the window is expected to spawn is SDR (HDR disabled in
    /// Windows Settings, or physically SDR panel) — force
    /// `preferred_target_format` to `None` so eframe selects an SDR
    /// (`Bgra8Unorm` / `Rgba8Unorm`) swap chain. This bypasses Windows DWM's
    /// scRGB → SDR conversion entirely.
    ///
    /// If the user later drags the window onto an HDR monitor at runtime,
    /// [`desired_target_format_for_active_monitor`] running on the per-frame
    /// monitor probe asks the (patched) egui-wgpu Painter to hot-swap the
    /// swap-chain format to `Rgba16Float`. Conversely, dragging back onto an
    /// SDR monitor swaps it back to `Bgra8Unorm`.
    SpawnMonitorSdr {
        label: String,
        origin: &'static str,
    },
    /// Probe failed (DXGI error, non-Windows platform, etc.) — keep whatever the
    /// caller-supplied policy decided. The runtime monitor probe will still
    /// gate the rendering path correctly, this just leaves the swap-chain
    /// format untouched.
    ProbeUnavailable,
}

/// Combine the user setting with a startup-time DXGI HDR probe of the **spawn
/// monitor** (saved window position → cursor → primary monitor) to decide
/// whether to request a float swap chain. This MUST run before window creation
/// because the surface format is locked at swap-chain creation time.
///
/// On mixed-monitor systems (one HDR + one SDR), this picks the swap-chain
/// format that matches the display the user is *actually* about to use, rather
/// than blanket-enabling Rgba16Float because *any* display happens to be HDR.
///
/// `saved_window_top_left` is the persisted outer-rect top-left from the
/// previous session (loaded from `siv_settings.yaml`). When present, the probe
/// lands on whichever monitor that point falls inside, ignoring the cursor —
/// so a window that closed on the HDR monitor reopens with HDR even if the
/// user moved the cursor to the SDR monitor before launching.
/// Seed [`crate::hdr::monitor::HdrMonitorState`] so the first frames after startup use the same
/// HDR/SDR output decision as the pre-window DXGI spawn probe.
///
/// Without this, `hdr_monitor_state.selection` stays `None` until the first per-frame
/// `active_monitor_hdr_status` succeeds; [`crate::hdr::monitor::effective_capability_output_mode`]
/// treats a missing selection as fail-closed `SdrToneMapped`, which **overwrites**
/// `hdr_capabilities.output_mode` every frame and forces the SDR tone-mapped renderer even when
/// `preferred_target_format` is already `Rgba16Float` from a successful `SpawnMonitorHdr` probe.
pub fn initial_monitor_selection_from_environment_probe(
    probe: &HdrEnvironmentProbe,
) -> Option<crate::hdr::monitor::HdrMonitorSelection> {
    match probe {
        HdrEnvironmentProbe::SpawnMonitorHdr { label, .. } => {
            Some(crate::hdr::monitor::HdrMonitorSelection {
                hdr_supported: true,
                label: label.clone(),
                max_luminance_nits: None,
                max_full_frame_luminance_nits: None,
                max_hdr_capacity: None,
                hdr_capacity_source: Some("spawn DXGI probe"),
            })
        }
        HdrEnvironmentProbe::SpawnMonitorSdr { label, .. } => {
            Some(crate::hdr::monitor::HdrMonitorSelection {
                hdr_supported: false,
                label: label.clone(),
                max_luminance_nits: None,
                max_full_frame_luminance_nits: None,
                max_hdr_capacity: None,
                hdr_capacity_source: None,
            })
        }
        HdrEnvironmentProbe::ProbeUnavailable => None,
    }
}

pub fn preferred_native_hdr_target_format_for_environment(
    native_surface_enabled: bool,
    saved_window_top_left: Option<[i32; 2]>,
) -> (Option<wgpu::TextureFormat>, HdrEnvironmentProbe) {
    if !native_surface_enabled {
        return (None, HdrEnvironmentProbe::ProbeUnavailable);
    }
    let candidate = preferred_native_hdr_target_format_for_platform();
    if candidate.is_none() {
        return (None, HdrEnvironmentProbe::ProbeUnavailable);
    }
    match crate::hdr::monitor::spawn_monitor_hdr_status(saved_window_top_left) {
        Ok(probe) if probe.hdr_supported => (
            candidate,
            HdrEnvironmentProbe::SpawnMonitorHdr {
                label: probe.label,
                origin: probe.origin,
            },
        ),
        Ok(probe) => (
            None,
            HdrEnvironmentProbe::SpawnMonitorSdr {
                label: probe.label,
                origin: probe.origin,
            },
        ),
        Err(_) => (candidate, HdrEnvironmentProbe::ProbeUnavailable),
    }
}

/// Decide which swap-chain target format the egui-wgpu surface should use
/// **right now** based on the live per-frame monitor probe.
///
/// This is the runtime counterpart to
/// [`preferred_native_hdr_target_format_for_environment`]: where the latter
/// snapshots the spawn monitor at startup, this function is invoked every
/// frame in `App::logic` and feeds its result into the patched egui-wgpu
/// `RequestedSurfaceFormat` mailbox so the swap chain follows the user as
/// they drag the window between an HDR monitor and an SDR monitor.
///
/// Returns `Some(format)` only when the runtime probe has produced positive
/// evidence that a *specific* format is correct. When the probe is missing
/// or temporarily transient (DXGI hand-off during cross-monitor drag, brief
/// `EnumWindows` hiccups, the very first frames before the first probe has
/// completed), this returns `None` to mean "no opinion — caller should keep
/// the current swap-chain format alone". Returning a concrete `SDR_FORMAT`
/// fallback in that case caused HDR→SDR thrashing every frame the probe was
/// pending, which silently demoted the swap chain to `Bgra8Unorm` even when
/// the spawn-time probe had already correctly chosen `Rgba16Float`.
///
/// Decision tree (matches the existing [`crate::hdr::monitor::effective_render_output_mode`]
/// gate so the swap-chain format and the renderer's HDR/SDR path always agree):
/// 1. User disabled HDR native presentation → `Some(Bgra8Unorm)`.
/// 2. Platform doesn't support native HDR (Linux today) → `Some(Bgra8Unorm)`.
/// 3. Active monitor probe is missing → `None` (keep whatever spawn-time
///    decided; do not thrash).
/// 4. Active monitor reports `hdr_supported = false` → `Some(Bgra8Unorm)`;
///    we never want to drive scRGB onto an SDR panel.
/// 5. Active monitor reports `hdr_supported = true` → `Some(<platform float
///    format>)` — `Rgba16Float` on Windows / macOS.
pub fn desired_target_format_for_active_monitor(
    native_surface_enabled: bool,
    selection: Option<&crate::hdr::monitor::HdrMonitorSelection>,
) -> Option<wgpu::TextureFormat> {
    const SDR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8Unorm;
    if !native_surface_enabled {
        return Some(SDR_FORMAT);
    }
    let Some(hdr_format) = preferred_native_hdr_target_format_for_platform() else {
        return Some(SDR_FORMAT);
    };
    let sel = selection?;
    Some(if sel.hdr_supported {
        hdr_format
    } else {
        SDR_FORMAT
    })
}

pub fn native_hdr_surface_request_diagnostics(
    native_surface_enabled: bool,
    preferred_target_format: Option<wgpu::TextureFormat>,
) -> [String; 2] {
    [
        format!("[HDR] native_surface_request_enabled={native_surface_enabled}"),
        format!("[HDR] preferred_target_format={preferred_target_format:?}"),
    ]
}

pub fn is_native_hdr_surface_format(format: Option<wgpu::TextureFormat>) -> bool {
    let Some(format) = format else {
        return false;
    };

    matches!(
        choose_native_hdr_surface_format(&[format]),
        HdrSurfaceSelection::NativeHdr(_)
    )
}

pub fn native_hdr_surface_blocker(format: Option<wgpu::TextureFormat>) -> Option<&'static str> {
    if is_native_hdr_surface_format(format) {
        return None;
    }

    Some(match format {
        Some(_) => "current eframe/wgpu target format is SDR; native HDR requires a float surface",
        None => "current eframe/wgpu target format is unknown; native HDR requires a float surface",
    })
}

#[cfg(test)]
mod tests {
    use crate::hdr::monitor::HdrMonitorSelection;
    use crate::hdr::surface::{
        HdrEnvironmentProbe, HdrSurfaceSelection, choose_native_hdr_surface_format,
        desired_target_format_for_active_monitor,
        initial_monitor_selection_from_environment_probe, native_hdr_surface_blocker,
    };

    fn hdr_selection() -> HdrMonitorSelection {
        HdrMonitorSelection {
            hdr_supported: true,
            label: "HDR test monitor".to_string(),
            max_luminance_nits: Some(1000.0),
            max_full_frame_luminance_nits: Some(500.0),
            max_hdr_capacity: None,
            hdr_capacity_source: Some("test"),
        }
    }

    fn sdr_selection() -> HdrMonitorSelection {
        HdrMonitorSelection {
            hdr_supported: false,
            label: "SDR test monitor".to_string(),
            max_luminance_nits: None,
            max_full_frame_luminance_nits: None,
            max_hdr_capacity: None,
            hdr_capacity_source: None,
        }
    }

    #[test]
    fn desired_target_format_returns_sdr_when_native_hdr_disabled() {
        let hdr = hdr_selection();
        assert_eq!(
            desired_target_format_for_active_monitor(false, Some(&hdr)),
            Some(wgpu::TextureFormat::Bgra8Unorm),
            "user opt-out of native HDR must override even an HDR monitor"
        );
    }

    #[test]
    fn spawn_hdr_probe_seeds_initial_monitor_selection() {
        let probe = HdrEnvironmentProbe::SpawnMonitorHdr {
            label: r"\\.\DISPLAY1".to_string(),
            origin: "saved_window_position",
        };
        let sel = initial_monitor_selection_from_environment_probe(&probe).expect("seed");
        assert!(sel.hdr_supported);
        assert_eq!(sel.label, r"\\.\DISPLAY1");
        assert_eq!(sel.hdr_capacity_source, Some("spawn DXGI probe"));
    }

    #[test]
    fn spawn_sdr_probe_seeds_non_hdr_selection() {
        let probe = HdrEnvironmentProbe::SpawnMonitorSdr {
            label: r"\\.\DISPLAY2".to_string(),
            origin: "cursor",
        };
        let sel = initial_monitor_selection_from_environment_probe(&probe).expect("seed");
        assert!(!sel.hdr_supported);
    }

    #[test]
    fn probe_unavailable_yields_no_seed() {
        assert!(initial_monitor_selection_from_environment_probe(&HdrEnvironmentProbe::ProbeUnavailable).is_none());
    }

    #[test]
    fn desired_target_format_returns_none_when_probe_pending() {
        // Regression: the per-frame probe is briefly `None` on Windows
        // during cross-monitor drag (DXGI hand-off, transient `EnumWindows`
        // hiccups, very first frames before the first probe completes).
        // Returning `Some(Bgra8Unorm)` in that case caused HDR→SDR
        // thrashing every frame the probe was missing, silently demoting
        // the swap chain even when the spawn-time probe had already
        // correctly chosen `Rgba16Float`. The contract here is: *only*
        // return a concrete format when the probe has positive evidence;
        // otherwise return `None` so the caller leaves the swap chain
        // alone.
        assert_eq!(
            desired_target_format_for_active_monitor(true, None),
            None,
            "missing probe must produce no opinion, never an SDR demotion"
        );
    }

    #[test]
    fn desired_target_format_returns_sdr_when_active_monitor_is_sdr() {
        let sdr = sdr_selection();
        assert_eq!(
            desired_target_format_for_active_monitor(true, Some(&sdr)),
            Some(wgpu::TextureFormat::Bgra8Unorm)
        );
    }

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    #[test]
    fn desired_target_format_returns_rgba16_float_on_hdr_monitor() {
        let hdr = hdr_selection();
        assert_eq!(
            desired_target_format_for_active_monitor(true, Some(&hdr)),
            Some(wgpu::TextureFormat::Rgba16Float)
        );
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    #[test]
    fn desired_target_format_stays_sdr_on_platforms_without_native_hdr() {
        // Linux currently has no shipping native HDR presentation in wgpu —
        // we must not request `Rgba16Float` even when the monitor reports HDR.
        let hdr = hdr_selection();
        assert_eq!(
            desired_target_format_for_active_monitor(true, Some(&hdr)),
            Some(wgpu::TextureFormat::Bgra8Unorm)
        );
    }

    #[test]
    fn prefers_rgba16_float_for_native_hdr_surface() {
        let selection = choose_native_hdr_surface_format(&[
            wgpu::TextureFormat::Bgra8Unorm,
            wgpu::TextureFormat::Rgba16Float,
            wgpu::TextureFormat::Rgba8Unorm,
        ]);

        assert_eq!(
            selection,
            HdrSurfaceSelection::NativeHdr(wgpu::TextureFormat::Rgba16Float)
        );
    }

    #[test]
    fn reports_blocker_when_only_sdr_formats_are_available() {
        let selection = choose_native_hdr_surface_format(&[
            wgpu::TextureFormat::Bgra8Unorm,
            wgpu::TextureFormat::Rgba8Unorm,
        ]);

        assert_eq!(
            selection,
            HdrSurfaceSelection::Unavailable {
                reason: "surface exposes no float HDR presentation format"
            }
        );
    }

    #[test]
    fn reports_current_sdr_target_format_as_native_hdr_blocker() {
        assert_eq!(
            native_hdr_surface_blocker(Some(wgpu::TextureFormat::Bgra8Unorm)),
            Some("current eframe/wgpu target format is SDR; native HDR requires a float surface")
        );
        assert_eq!(
            native_hdr_surface_blocker(Some(wgpu::TextureFormat::Rgba16Float)),
            None
        );
    }

    #[test]
    fn reports_unknown_target_format_separately_from_sdr() {
        assert_eq!(
            native_hdr_surface_blocker(None),
            Some(
                "current eframe/wgpu target format is unknown; native HDR requires a float surface"
            )
        );
    }

    #[test]
    fn platform_native_hdr_request_is_limited_to_windows_and_macos() {
        let expected = if cfg!(any(target_os = "windows", target_os = "macos")) {
            Some(wgpu::TextureFormat::Rgba16Float)
        } else {
            None
        };

        assert_eq!(
            super::preferred_native_hdr_target_format_for_platform(),
            expected
        );
    }

    #[test]
    fn disabled_native_hdr_request_returns_no_preferred_target_format() {
        assert_eq!(
            super::preferred_native_hdr_target_format_for_settings(false),
            None
        );
    }

    #[test]
    fn native_hdr_request_diagnostics_include_preference_and_format() {
        assert_eq!(
            super::native_hdr_surface_request_diagnostics(
                false,
                Some(wgpu::TextureFormat::Rgba16Float)
            ),
            [
                "[HDR] native_surface_request_enabled=false",
                "[HDR] preferred_target_format=Some(Rgba16Float)",
            ]
        );
    }

    #[test]
    fn environment_probe_skips_when_setting_is_disabled() {
        let (format, probe) =
            super::preferred_native_hdr_target_format_for_environment(false, None);
        assert_eq!(format, None);
        assert_eq!(probe, super::HdrEnvironmentProbe::ProbeUnavailable);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn environment_probe_runs_on_windows_when_enabled() {
        // We can't assert the exact outcome (depends on the test machine), but we can assert
        // the probe yields one of the well-defined variants and the format follows the variant.
        let (format, probe) =
            super::preferred_native_hdr_target_format_for_environment(true, None);
        match &probe {
            super::HdrEnvironmentProbe::SpawnMonitorHdr { origin, .. } => {
                assert_eq!(format, Some(wgpu::TextureFormat::Rgba16Float));
                assert!(
                    *origin == "cursor"
                        || *origin == "primary"
                        || *origin == "saved_window_position",
                    "origin must be a known spawn-point label, got {origin:?}"
                );
            }
            super::HdrEnvironmentProbe::SpawnMonitorSdr { origin, .. } => {
                assert_eq!(
                    format, None,
                    "spawn monitor is SDR — must NOT request a float swap chain on Windows"
                );
                assert!(
                    *origin == "cursor"
                        || *origin == "primary"
                        || *origin == "saved_window_position"
                );
            }
            super::HdrEnvironmentProbe::ProbeUnavailable => {
                // DXGI probing failed (CI / headless / virtualized GPU); we keep the platform
                // default so HDR still works on real user systems where probing later succeeds
                // via the per-window monitor probe + conservative render gate.
                assert_eq!(format, Some(wgpu::TextureFormat::Rgba16Float));
            }
        }
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn environment_probe_keeps_platform_default_on_non_windows() {
        let (format, probe) =
            super::preferred_native_hdr_target_format_for_environment(true, None);
        // macOS keeps the float swap chain because the EDR layer always handles SDR-only
        // displays cleanly (1.0 = SDR white, no scRGB-style brightening). Linux returns
        // None already from `preferred_native_hdr_target_format_for_platform`.
        assert_eq!(format, super::preferred_native_hdr_target_format_for_platform());
        assert_eq!(probe, super::HdrEnvironmentProbe::ProbeUnavailable);
    }
}
