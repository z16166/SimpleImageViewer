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
    /// scRGB → SDR conversion entirely. **Trade-off**: if the user later drags
    /// the window onto an HDR monitor, native-HDR presentation is disabled
    /// until the next launch (we do not currently recreate the swap chain).
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
/// monitor** (cursor location → primary monitor fallback) to decide whether to
/// request a float swap chain. This MUST run before window creation because
/// the surface format is locked at swap-chain creation time.
///
/// On mixed-monitor systems (one HDR + one SDR), this picks the swap-chain
/// format that matches the display the user is *actually* about to use, rather
/// than blanket-enabling Rgba16Float because *any* display happens to be HDR.
pub fn preferred_native_hdr_target_format_for_environment(
    native_surface_enabled: bool,
) -> (Option<wgpu::TextureFormat>, HdrEnvironmentProbe) {
    if !native_surface_enabled {
        return (None, HdrEnvironmentProbe::ProbeUnavailable);
    }
    let candidate = preferred_native_hdr_target_format_for_platform();
    if candidate.is_none() {
        return (None, HdrEnvironmentProbe::ProbeUnavailable);
    }
    match crate::hdr::monitor::spawn_monitor_hdr_status() {
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
    use crate::hdr::surface::{
        HdrSurfaceSelection, choose_native_hdr_surface_format, native_hdr_surface_blocker,
    };

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
        let (format, probe) = super::preferred_native_hdr_target_format_for_environment(false);
        assert_eq!(format, None);
        assert_eq!(probe, super::HdrEnvironmentProbe::ProbeUnavailable);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn environment_probe_runs_on_windows_when_enabled() {
        // We can't assert the exact outcome (depends on the test machine), but we can assert
        // the probe yields one of the well-defined variants and the format follows the variant.
        let (format, probe) = super::preferred_native_hdr_target_format_for_environment(true);
        match &probe {
            super::HdrEnvironmentProbe::SpawnMonitorHdr { origin, .. } => {
                assert_eq!(format, Some(wgpu::TextureFormat::Rgba16Float));
                assert!(
                    *origin == "cursor" || *origin == "primary",
                    "origin must be a known spawn-point label, got {origin:?}"
                );
            }
            super::HdrEnvironmentProbe::SpawnMonitorSdr { origin, .. } => {
                assert_eq!(
                    format, None,
                    "spawn monitor is SDR — must NOT request a float swap chain on Windows"
                );
                assert!(*origin == "cursor" || *origin == "primary");
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
        let (format, probe) = super::preferred_native_hdr_target_format_for_environment(true);
        // macOS keeps the float swap chain because the EDR layer always handles SDR-only
        // displays cleanly (1.0 = SDR white, no scRGB-style brightening). Linux returns
        // None already from `preferred_native_hdr_target_format_for_platform`.
        assert_eq!(format, super::preferred_native_hdr_target_format_for_platform());
        assert_eq!(probe, super::HdrEnvironmentProbe::ProbeUnavailable);
    }
}
