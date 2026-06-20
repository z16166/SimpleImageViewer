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

#[cfg(target_os = "windows")]
use super::types::{HdrMonitorSelection, HdrNativeSurfaceEncoding};

#[cfg(target_os = "windows")]
pub(crate) fn monitor_device_name(name: &[u16; 32]) -> String {
    let len = name
        .iter()
        .position(|value| *value == 0)
        .unwrap_or(name.len());
    String::from_utf16_lossy(&name[..len])
}

#[cfg(target_os = "windows")]
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020, DXGI_COLOR_SPACE_TYPE,
};

/// Whether a DXGI output is currently in active HDR signaling.
///
/// The decisive signal on Windows is the per-output `ColorSpace` (PQ /
/// `G2084_NONE_P2020`): when the user has HDR enabled in Windows Settings,
/// the desktop is composed and presented in PQ to the panel **regardless of
/// whether the physical panel is 8-bit-with-dithering or native 10-bit**.
///
/// We deliberately do NOT also gate on `BitsPerColor > 8` here. That's a
/// panel-link attribute (DisplayPort HBR2 / HDMI 2.0 bandwidth-constrained
/// links can force Windows down to 8 BPC + temporal dithering even with HDR
/// active), and using it as an HDR gate erroneously rejects perfectly valid
/// HDR configurations — see the LC49G95T regression where Windows reported
/// `BitsPerColor=8 ColorSpace=G2084_NONE_P2020` and we mis-classified it as
/// SDR, locking the swap chain into `Bgra8Unorm`.
///
/// Reference: <https://github.com/microsoft/DirectX-Graphics-Samples> (the
/// `D3D12HDR` sample uses the same single-condition check).
#[cfg(target_os = "windows")]
pub(crate) fn dxgi_output_hdr_active(colorspace: DXGI_COLOR_SPACE_TYPE) -> bool {
    colorspace == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020
}
#[cfg(target_os = "windows")]
fn dxgi_hdr_selection_for_monitor_handle(
    monitor: windows::Win32::Graphics::Gdi::HMONITOR,
) -> Result<HdrMonitorSelection, String> {
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, DXGI_ERROR_NOT_FOUND, IDXGIFactory1, IDXGIOutput6,
    };
    use windows::core::Interface;

    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.map_err(|err| err.to_string())?;
    let mut adapter_index = 0_u32;
    loop {
        let adapter = match unsafe { factory.EnumAdapters1(adapter_index) } {
            Ok(adapter) => adapter,
            Err(err) if err.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(err) => return Err(err.to_string()),
        };

        let mut output_index = 0_u32;
        loop {
            let output = match unsafe { adapter.EnumOutputs(output_index) } {
                Ok(output) => output,
                Err(err) if err.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(err) => return Err(err.to_string()),
            };
            if let Ok(output6) = output.cast::<IDXGIOutput6>() {
                let desc = unsafe { output6.GetDesc1() }.map_err(|err| err.to_string())?;
                let device_name = monitor_device_name(&desc.DeviceName);
                let matches = desc.Monitor == monitor;
                let hdr_supported = dxgi_output_hdr_active(desc.ColorSpace);
                log::debug!(
                    "[HDR] DXGI output#{adapter_index}/{output_index}: device={device_name} \
                     bits_per_color={} colorspace={:?} hdr_active={hdr_supported} \
                     matches_active_monitor={matches}",
                    desc.BitsPerColor,
                    desc.ColorSpace,
                );
                if matches {
                    return Ok(HdrMonitorSelection {
                        hdr_supported,
                        label: if device_name.is_empty() {
                            "unknown Windows monitor".to_string()
                        } else {
                            device_name
                        },
                        max_luminance_nits: finite_positive_luminance(desc.MaxLuminance),
                        max_full_frame_luminance_nits: finite_positive_luminance(
                            desc.MaxFullFrameLuminance,
                        ),
                        max_hdr_capacity: None,
                        hdr_capacity_source: Some("Windows DXGI MaxLuminance"),
                        native_surface_encoding: hdr_supported
                            .then_some(HdrNativeSurfaceEncoding::LinearScRgb),
                    });
                }
            }
            output_index += 1;
        }

        adapter_index += 1;
    }

    Err("active DXGI output was not found".to_string())
}

/// Convert egui viewport outer top-left (UI points) to physical screen pixels for Win32.
#[cfg(target_os = "windows")]
pub(crate) fn outer_top_left_to_physical_screen_px(
    top_left: [i32; 2],
    native_pixels_per_point: Option<f32>,
) -> [i32; 2] {
    let scale = native_pixels_per_point
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(1.0);
    [
        (top_left[0] as f32 * scale).round() as i32,
        (top_left[1] as f32 * scale).round() as i32,
    ]
}

/// Same monitor lookup as [`super::probe::spawn_monitor_hdr_status`]: bias ~20px inside
/// the ROOT outer top-left in **physical screen pixels** so boundary pixels do not flip to
/// a neighbour display. egui reports outer rects in UI points; spawn-time probing uses the
/// persisted physical position from settings.
#[cfg(target_os = "windows")]
fn dxgi_selection_for_outer_top_left(
    top_left: [i32; 2],
    native_pixels_per_point: Option<f32>,
) -> Result<HdrMonitorSelection, String> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTOPRIMARY, MonitorFromPoint};

    let physical = outer_top_left_to_physical_screen_px(top_left, native_pixels_per_point);
    let point = POINT {
        x: physical[0] + 20,
        y: physical[1] + 20,
    };
    let monitor = unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTOPRIMARY) };
    if monitor.is_invalid() {
        return Err("active window monitor was not found".to_string());
    }
    log::info!(
        "[HDR] active-monitor probe: origin=cached_outer_top_left logical=({}, {}) \
         physical=({}, {}) npp={native_pixels_per_point:?} probe_point=({}, {}) \
         monitor_handle={monitor:?}",
        top_left[0],
        top_left[1],
        physical[0],
        physical[1],
        point.x,
        point.y,
    );
    dxgi_hdr_selection_for_monitor_handle(monitor)
}

#[cfg(target_os = "windows")]
pub(crate) fn windows_active_monitor_hdr_status(
    viewport_outer_rect_screen_px: Option<[i32; 4]>,
    main_window_outer_top_left: Option<[i32; 2]>,
    native_pixels_per_point: Option<f32>,
    settings_spawn_top_left: Option<[i32; 2]>,
) -> Result<HdrMonitorSelection, String> {
    if let Some(top_left) = main_window_outer_top_left {
        return dxgi_selection_for_outer_top_left(top_left, native_pixels_per_point);
    }

    // Persisted spawn placement is already in physical screen pixels.
    if let Some(spawn_top_left) = settings_spawn_top_left {
        return dxgi_selection_for_outer_top_left(spawn_top_left, None);
    }

    // Before ROOT placement is cached, reuse the outer rect top-left with the same +20 bias.
    if let Some([vl, vt, _, _]) = plausible_main_viewport_outer_rect(viewport_outer_rect_screen_px)
    {
        return dxgi_selection_for_outer_top_left([vl, vt], native_pixels_per_point);
    }

    Err(
        "main viewport outer rect unavailable for HDR probe (will retry when ROOT publishes a plausible rect)"
            .to_string(),
    )
}

/// Returns the main-window outer rect when it is large enough to trust for HDR probing.
#[cfg(target_os = "windows")]
pub(crate) fn plausible_main_viewport_outer_rect(
    viewport_outer_rect_screen_px: Option<[i32; 4]>,
) -> Option<[i32; 4]> {
    const MIN_PLAUSIBLE_OUTER_AREA: i64 = 64 * 64;
    let [vl, vt, vr, vb] = viewport_outer_rect_screen_px?;
    let vp_area = i64::from(vr.saturating_sub(vl)).max(0) * i64::from(vb.saturating_sub(vt)).max(0);
    (vp_area >= MIN_PLAUSIBLE_OUTER_AREA).then_some([vl, vt, vr, vb])
}

#[cfg(all(test, target_os = "windows"))]
mod viewport_probe_tests {
    use super::{
        outer_top_left_to_physical_screen_px, plausible_main_viewport_outer_rect,
        windows_active_monitor_hdr_status,
    };

    #[test]
    fn outer_top_left_to_physical_scales_by_native_pixels_per_point() {
        assert_eq!(
            outer_top_left_to_physical_screen_px([1914, 170], Some(1.25)),
            [2393, 213]
        );
        assert_eq!(
            outer_top_left_to_physical_screen_px([2461, 192], None),
            [2461, 192]
        );
    }

    #[test]
    fn plausible_main_viewport_outer_rect_accepts_typical_main_window() {
        assert_eq!(
            plausible_main_viewport_outer_rect(Some([0, 0, 1280, 720])),
            Some([0, 0, 1280, 720])
        );
    }

    #[test]
    fn plausible_main_viewport_outer_rect_rejects_degenerate_rects() {
        assert_eq!(plausible_main_viewport_outer_rect(None), None);
        assert_eq!(
            plausible_main_viewport_outer_rect(Some([0, 0, 10, 10])),
            None
        );
    }

    #[test]
    fn active_monitor_probe_fails_closed_without_plausible_main_outer_rect() {
        assert!(windows_active_monitor_hdr_status(None, None, None, None).is_err());
        assert!(windows_active_monitor_hdr_status(Some([0, 0, 10, 10]), None, None, None).is_err());
    }
}

#[cfg(target_os = "windows")]
pub(crate) fn finite_positive_luminance(value: f32) -> Option<f32> {
    (value.is_finite() && value > 0.0).then_some(value)
}

/// Pre-window-creation HDR availability probe.
///
/// Returns:
/// - `Ok(true)` if any DXGI output reports active HDR signaling
///   (`ColorSpace == G2084_NONE_P2020`, matching [`dxgi_output_hdr_active`])
/// - `Ok(false)` if no output advertises HDR — the window's swap chain should NOT
///   request `Rgba16Float` because the Windows compositor will route scRGB linear
///   values through HDR-style processing on physically SDR panels (visibly washing
///   out shadow contrast — see `bench_oriented_brg/input.jxl`)
/// - `Err(...)` if probing failed; callers should treat this as "unknown" and may
///   choose either policy. On Linux this returns `Err` unconditionally.
///
/// Kept available as a fallback strategy for future code that wants to know about
/// system-wide HDR availability (vs. the spawn-monitor probe, which is what the
/// current swap-chain selector uses).
#[cfg(target_os = "windows")]
#[allow(dead_code)]
pub fn any_active_output_supports_hdr() -> Result<bool, String> {
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, DXGI_ERROR_NOT_FOUND, IDXGIFactory1, IDXGIOutput6,
    };
    use windows::core::Interface;

    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.map_err(|err| err.to_string())?;
    let mut adapter_index = 0_u32;
    loop {
        let adapter = match unsafe { factory.EnumAdapters1(adapter_index) } {
            Ok(adapter) => adapter,
            Err(err) if err.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(err) => return Err(err.to_string()),
        };

        let mut output_index = 0_u32;
        loop {
            let output = match unsafe { adapter.EnumOutputs(output_index) } {
                Ok(output) => output,
                Err(err) if err.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(err) => return Err(err.to_string()),
            };
            if let Ok(output6) = output.cast::<IDXGIOutput6>()
                && let Ok(desc) = unsafe { output6.GetDesc1() }
                && dxgi_output_hdr_active(desc.ColorSpace)
            {
                return Ok(true);
            }
            output_index += 1;
        }

        adapter_index += 1;
    }

    Ok(false)
}

#[cfg(target_os = "linux")]
#[allow(dead_code)]
pub fn any_active_output_supports_hdr() -> Result<bool, String> {
    if crate::hdr::platform::linux_native_hdr_platform_eligible() {
        Err(
            "pre-creation HDR availability probing is not yet implemented on Linux Wayland"
                .to_string(),
        )
    } else {
        Err("HDR probing requires a Wayland session".to_string())
    }
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
#[allow(dead_code)]
pub fn any_active_output_supports_hdr() -> Result<bool, String> {
    Err("pre-creation HDR availability probing is only implemented on Windows".to_string())
}
