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

#[cfg(target_os = "windows")]
fn windows_screen_rect_area(rect: &windows::Win32::Foundation::RECT) -> i64 {
    i64::from(rect.right.saturating_sub(rect.left))
        * i64::from(rect.bottom.saturating_sub(rect.top))
}

/// Collect visible root unowned top-level HWNDs for this process (Z-order: front to back).
#[cfg(target_os = "windows")]
fn windows_collect_process_tl_hwnds() -> Vec<windows::Win32::Foundation::HWND> {
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GA_ROOT, GW_OWNER, GetAncestor, GetWindow, GetWindowThreadProcessId,
        IsWindowVisible,
    };

    struct CollectState {
        process_id: u32,
        hwnds: Vec<HWND>,
    }

    unsafe extern "system" fn enum_collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let state = unsafe { &mut *(lparam.0 as *mut CollectState) };
        let mut process_id = 0_u32;
        unsafe {
            GetWindowThreadProcessId(hwnd, Some(&mut process_id));
        }
        if process_id != state.process_id {
            return true.into();
        }
        if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
            return true.into();
        }
        if unsafe { GetAncestor(hwnd, GA_ROOT) } != hwnd {
            return true.into();
        }
        if !unsafe { GetWindow(hwnd, GW_OWNER) }
            .unwrap_or_default()
            .is_invalid()
        {
            return true.into();
        }
        state.hwnds.push(hwnd);
        true.into()
    }

    let mut state = CollectState {
        process_id: std::process::id(),
        hwnds: Vec::new(),
    };
    let _ = unsafe {
        EnumWindows(
            Some(enum_collect),
            LPARAM((&mut state as *mut CollectState) as isize),
        )
    };
    state.hwnds
}

#[cfg(target_os = "windows")]
fn windows_pick_tl_hwnd_largest_screen_area(
    candidates: &[windows::Win32::Foundation::HWND],
) -> Option<windows::Win32::Foundation::HWND> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;

    let mut best: Option<(HWND, i64)> = None;
    for &hwnd in candidates {
        let mut rect = windows::Win32::Foundation::RECT::default();
        if unsafe { GetWindowRect(hwnd, &mut rect) }.is_err() {
            continue;
        }
        let area = windows_screen_rect_area(&rect);
        if area <= 0 {
            continue;
        }
        if best.map_or(true, |(_, a)| area > a) {
            best = Some((hwnd, area));
        }
    }
    best.map(|(hwnd, _)| hwnd)
}

#[cfg(target_os = "windows")]
pub(crate) fn windows_active_monitor_hdr_status(
    viewport_outer_rect_screen_px: Option<[i32; 4]>,
) -> Result<HdrMonitorSelection, String> {
    use windows::Win32::Foundation::{POINT, RECT};
    use windows::Win32::Graphics::Gdi::{
        MONITOR_DEFAULTTONEAREST, MonitorFromPoint, MonitorFromWindow,
    };
    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;

    /// Ignore degenerate rects from Win32 before the real client size is committed.
    const MIN_PLAUSIBLE_OUTER_AREA: i64 = 64 * 64;

    let candidates = windows_collect_process_tl_hwnds();
    if candidates.is_empty() {
        return Err("Simple Image Viewer window handle was not found".to_string());
    }

    // Pick the process top-level HWND with the largest screen area — the main egui frame.
    //
    // **Do not** use `MonitorFromWindow(hwnd, …)` for HDR vs SDR policy: it returns the
    // monitor with the *largest intersection area* with the window rect. During cross-monitor
    // drags a wide window can keep most of its area on the HDR display while the user's focus
    // (and the majority of pixels they care about) has already moved to the SDR display — logs
    // then show `hdr_supported=true` + `Rgba16Float` for thousands of frames until the rect
    // finally tips, producing scRGB→SDR tonemap "washed" chrome and late swap-chain demotion.
    //
    // `MonitorFromPoint` at the window **center** matches common OS "which monitor is this
    // window on?" behaviour and updates as soon as the center crosses the boundary.
    let hwnd = windows_pick_tl_hwnd_largest_screen_area(&candidates).unwrap_or(candidates[0]);
    let mut rect = RECT::default();
    let hwnd_rect_ok = unsafe { GetWindowRect(hwnd, &mut rect) }.is_ok();
    let hwnd_area = if hwnd_rect_ok {
        windows_screen_rect_area(&rect)
    } else {
        0
    };

    let viewport_choice = viewport_outer_rect_screen_px.and_then(|[vl, vt, vr, vb]| {
        let vp_area =
            i64::from(vr.saturating_sub(vl)).max(0) * i64::from(vb.saturating_sub(vt)).max(0);
        if vp_area < MIN_PLAUSIBLE_OUTER_AREA {
            return None;
        }
        if hwnd_area > 0 && vp_area <= hwnd_area {
            // Normal steady state (and cross-monitor drags where `outer_rect` lags): keep the
            // HWND center path so we still track the native frame while egui catches up.
            return None;
        }
        Some([vl, vt, vr, vb])
    });

    let monitor = if let Some([vl, vt, vr, vb]) = viewport_choice {
        let cx = (vl + vr) / 2;
        let cy = (vt + vb) / 2;
        let m = unsafe { MonitorFromPoint(POINT { x: cx, y: cy }, MONITOR_DEFAULTTONEAREST) };
        log::debug!(
            "[HDR] active-monitor probe: origin=viewport_outer_rect center_screen=({cx},{cy}) \
             vp_area={} hwnd_area={} monitor_handle={:?}",
            i64::from(vr.saturating_sub(vl)).max(0) * i64::from(vb.saturating_sub(vt)).max(0),
            hwnd_area,
            m,
        );
        m
    } else if hwnd_rect_ok {
        let cx = (rect.left + rect.right) / 2;
        let cy = (rect.top + rect.bottom) / 2;
        let m = unsafe { MonitorFromPoint(POINT { x: cx, y: cy }, MONITOR_DEFAULTTONEAREST) };
        log::debug!(
            "[HDR] active-monitor probe: origin=largest_tl_hwnd hwnd={hwnd:?} center_screen=({cx},{cy}) monitor_handle={m:?}"
        );
        m
    } else {
        let m = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
        log::debug!(
            "[HDR] active-monitor probe: origin=largest_tl_hwnd hwnd={hwnd:?} monitor_handle={m:?} (GetWindowRect failed; used MonitorFromWindow)"
        );
        m
    };
    if monitor.is_invalid() {
        return Err("active window monitor was not found".to_string());
    }

    dxgi_hdr_selection_for_monitor_handle(monitor)
}

#[cfg(target_os = "windows")]
pub(crate) fn finite_positive_luminance(value: f32) -> Option<f32> {
    (value.is_finite() && value > 0.0).then_some(value)
}

/// Pre-window-creation HDR availability probe.
///
/// Returns:
/// - `Ok(true)` if any DXGI output reports active HDR signaling
///   (BitsPerColor > 8 AND ColorSpace == G2084_NONE_P2020)
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
    use windows::Win32::Graphics::Dxgi::Common::DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020;
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
                && desc.BitsPerColor > 8
                && desc.ColorSpace == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020
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
