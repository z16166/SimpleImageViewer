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
use super::windows::{dxgi_output_hdr_active, finite_positive_luminance, monitor_device_name};
#[derive(Debug, Clone)]
pub struct SpawnMonitorHdrProbe {
    /// True when the monitor where we expect the window to appear advertises HDR
    /// (BitsPerColor > 8 AND ColorSpace == G2084_NONE_P2020 from DXGI).
    pub hdr_supported: bool,
    /// Friendly DXGI device name for the matched monitor (e.g. `"\\.\DISPLAY1"`).
    /// Empty when the cursor / primary fallback could not be matched to any
    /// enumerated DXGI output (e.g. headless test runners).
    pub label: String,
    /// Where the probe got the "spawn point" from, for logging:
    /// `"cursor"` — `GetCursorPos` (mouse cursor is on this monitor),
    /// `"primary"` — fell back to `MonitorFromPoint(0, 0, MONITOR_DEFAULTTOPRIMARY)`.
    pub origin: &'static str,
    /// DXGI reported peak luminance in nits for the matched monitor, if available.
    /// Pre-seeding this prevents a stale-capacity re-decode cycle at startup.
    pub max_luminance_nits: Option<f32>,
    /// DXGI reported max full-frame luminance in nits for the matched monitor.
    pub max_full_frame_luminance_nits: Option<f32>,
}

/// Probe HDR support of the **monitor where the window is most likely to spawn**
/// rather than "any output". On a mixed-monitor system (one HDR + one SDR) the
/// any-output probe always returns `Ok(true)` because the HDR display exists,
/// which then locks the swap chain into `Rgba16Float`. When the user actually
/// starts the app on the SDR monitor, Windows DWM has to convert scRGB → SDR and
/// can introduce subtle HDR-style processing. Probing the spawn monitor lets us
/// pick `Bgra8Unorm` directly and bypass DWM conversion entirely.
///
/// Spawn point selection priority:
/// 1. `saved_window_top_left` — the persisted outer-rect top-left from the
///    previous session. When the app remembered that the window closed on the
///    HDR monitor, we want to spawn into the same HDR swap chain regardless of
///    where the cursor currently sits.
/// 2. `GetCursorPos` — the user's cursor monitor. The window typically opens on
///    the same display as where the user double-clicked / launched the shortcut.
/// 3. Fallback: `MONITOR_DEFAULTTOPRIMARY` (Windows primary monitor).
///
/// Returns `Err(...)` when DXGI enumeration fails or the platform does not
/// support this probing path; callers should fall back to the platform default.
#[cfg(target_os = "windows")]
pub fn spawn_monitor_hdr_status(
    saved_window_top_left: Option<[i32; 2]>,
) -> Result<SpawnMonitorHdrProbe, String> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, DXGI_ERROR_NOT_FOUND, IDXGIFactory1, IDXGIOutput6,
    };
    use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTOPRIMARY, MonitorFromPoint};
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    use windows::core::Interface;

    let (monitor, origin) = if let Some([x, y]) = saved_window_top_left {
        // Bias the monitor lookup ~20px inside the saved frame so that we don't
        // accidentally land on the *neighbouring* monitor when the window's
        // exact top-left pixel sits on a monitor boundary.
        (
            unsafe {
                MonitorFromPoint(
                    POINT {
                        x: x + 20,
                        y: y + 20,
                    },
                    MONITOR_DEFAULTTOPRIMARY,
                )
            },
            "saved_window_position",
        )
    } else {
        let mut cursor = POINT::default();
        match unsafe { GetCursorPos(&mut cursor) } {
            Ok(()) => (
                unsafe { MonitorFromPoint(cursor, MONITOR_DEFAULTTOPRIMARY) },
                "cursor",
            ),
            Err(_) => (
                unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) },
                "primary",
            ),
        }
    };
    if monitor.is_invalid() {
        return Err("spawn monitor handle was not found".to_string());
    }

    log::info!(
        "[HDR] spawn-monitor probe: origin={origin} monitor_handle={monitor:?} \
         saved_window_top_left={saved_window_top_left:?}"
    );

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
            {
                let label = monitor_device_name(&desc.DeviceName);
                let matches = desc.Monitor == monitor;
                let hdr_supported = dxgi_output_hdr_active(desc.ColorSpace);
                // Verbose every-output diagnostic so users can see exactly
                // which physical monitor each `\\.\DISPLAYn` GDI name maps
                // to, what bit depth / colour space DXGI reports for it,
                // and which one actually matched the spawn-point lookup.
                log::info!(
                    "[HDR] DXGI output#{adapter_index}/{output_index}: device={label} \
                     bits_per_color={} colorspace={:?} hdr_active={hdr_supported} \
                     desktop_coords=[{},{},{},{}] matches_spawn_monitor={matches}",
                    desc.BitsPerColor,
                    desc.ColorSpace,
                    desc.DesktopCoordinates.left,
                    desc.DesktopCoordinates.top,
                    desc.DesktopCoordinates.right,
                    desc.DesktopCoordinates.bottom,
                );
                if matches {
                    return Ok(SpawnMonitorHdrProbe {
                        hdr_supported,
                        label,
                        origin,
                        max_luminance_nits: finite_positive_luminance(desc.MaxLuminance),
                        max_full_frame_luminance_nits: finite_positive_luminance(
                            desc.MaxFullFrameLuminance,
                        ),
                    });
                }
            }
            output_index += 1;
        }

        adapter_index += 1;
    }

    Err("spawn monitor was not matched to any DXGI output".to_string())
}

#[cfg(target_os = "linux")]
pub fn spawn_monitor_hdr_status(
    saved_window_top_left: Option<[i32; 2]>,
) -> Result<SpawnMonitorHdrProbe, String> {
    if crate::hdr::platform::linux_native_hdr_platform_eligible() {
        super::wayland::spawn_monitor_hdr_status(saved_window_top_left)
    } else {
        Err("HDR probing requires a Wayland session".to_string())
    }
}

#[cfg(target_os = "macos")]
pub fn spawn_monitor_hdr_status(
    _saved_window_top_left: Option<[i32; 2]>,
) -> Result<SpawnMonitorHdrProbe, String> {
    Err("spawn-monitor HDR probing is only implemented on Windows".to_string())
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
pub fn spawn_monitor_hdr_status(
    _saved_window_top_left: Option<[i32; 2]>,
) -> Result<SpawnMonitorHdrProbe, String> {
    Err("spawn-monitor HDR probing is only implemented on Windows".to_string())
}

