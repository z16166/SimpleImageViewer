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
use parking_lot::Mutex;

#[cfg(target_os = "windows")]
use std::sync::OnceLock;

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
                    let reference_luminance_nits = if hdr_supported {
                        sdr_white_level_nits_for_gdi_name(&desc.DeviceName)
                    } else {
                        None
                    };
                    log_sdr_white_level_if_changed(
                        &device_name,
                        hdr_supported,
                        reference_luminance_nits,
                    );
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
                        reference_luminance_nits,
                        linux_wp_transfer: None,
                        linux_wp_primaries: None,
                        linux_explicit_hdr_state: None,
                        linux_explicit_hdr_state_source: None,
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

#[cfg(target_os = "windows")]
#[derive(Clone, Copy, Debug, PartialEq)]
struct ActiveMonitorProbeLogKey {
    logical: [i32; 2],
    physical: [i32; 2],
    native_pixels_per_point_bits: Option<u32>,
    probe_point: [i32; 2],
    monitor_handle: usize,
}

#[cfg(target_os = "windows")]
static LAST_ACTIVE_MONITOR_PROBE_LOG: OnceLock<Mutex<Option<ActiveMonitorProbeLogKey>>> =
    OnceLock::new();

#[cfg(target_os = "windows")]
fn active_monitor_probe_log_should_emit(
    last: &mut Option<ActiveMonitorProbeLogKey>,
    logical: [i32; 2],
    physical: [i32; 2],
    native_pixels_per_point: Option<f32>,
    probe_point: [i32; 2],
    monitor_handle: usize,
) -> bool {
    let key = ActiveMonitorProbeLogKey {
        logical,
        physical,
        native_pixels_per_point_bits: native_pixels_per_point.map(f32::to_bits),
        probe_point,
        monitor_handle,
    };
    if last.as_ref() == Some(&key) {
        return false;
    }
    *last = Some(key);
    true
}

#[cfg(target_os = "windows")]
fn active_monitor_probe_log_gate_should_emit(
    logical: [i32; 2],
    physical: [i32; 2],
    native_pixels_per_point: Option<f32>,
    probe_point: [i32; 2],
    monitor_handle: usize,
) -> bool {
    let gate = LAST_ACTIVE_MONITOR_PROBE_LOG.get_or_init(|| Mutex::new(None));
    let mut last = gate.lock();
    active_monitor_probe_log_should_emit(
        &mut last,
        logical,
        physical,
        native_pixels_per_point,
        probe_point,
        monitor_handle,
    )
}

#[cfg(target_os = "windows")]
fn monitor_handle_log_id(monitor: windows::Win32::Graphics::Gdi::HMONITOR) -> usize {
    monitor.0 as usize
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
    if active_monitor_probe_log_gate_should_emit(
        top_left,
        physical,
        native_pixels_per_point,
        [point.x, point.y],
        monitor_handle_log_id(monitor),
    ) {
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
    }
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
        active_monitor_probe_log_should_emit, gdi_device_names_equal,
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
    fn gdi_device_names_match_ignoring_ascii_case_and_trailing_nuls() {
        let mut left = [0_u16; 32];
        let mut right = [0_u16; 32];
        for (index, unit) in r"\\.\DISPLAY1".encode_utf16().enumerate() {
            left[index] = unit;
            right[index] = unit;
        }
        assert!(gdi_device_names_equal(&left, &right));
        // `\\.\DISPLAY1` : index 4 is `D`.
        right[4] = u16::from(b'd');
        assert!(gdi_device_names_equal(&left, &right));
        right[4] = u16::from(b'E');
        assert!(!gdi_device_names_equal(&left, &right));
    }

    #[test]
    fn active_monitor_probe_fails_closed_without_plausible_main_outer_rect() {
        assert!(windows_active_monitor_hdr_status(None, None, None, None).is_err());
        assert!(windows_active_monitor_hdr_status(Some([0, 0, 10, 10]), None, None, None).is_err());
    }

    #[test]
    fn active_monitor_probe_log_skips_unchanged_parameters() {
        let mut last = None;

        assert!(active_monitor_probe_log_should_emit(
            &mut last,
            [1914, 170],
            [3828, 340],
            Some(2.0),
            [3848, 360],
            0x10001,
        ));
        assert!(!active_monitor_probe_log_should_emit(
            &mut last,
            [1914, 170],
            [3828, 340],
            Some(2.0),
            [3848, 360],
            0x10001,
        ));
        assert!(active_monitor_probe_log_should_emit(
            &mut last,
            [1915, 170],
            [3830, 340],
            Some(2.0),
            [3850, 360],
            0x10001,
        ));
    }
}

/// How many times to retry `QueryDisplayConfig` after `ERROR_INSUFFICIENT_BUFFER`.
const DISPLAYCONFIG_QUERY_ATTEMPTS: usize = 4;
/// Reject absurd buffer sizes from a broken display-config query.
const DISPLAYCONFIG_MAX_PATHS: u32 = 64;
const DISPLAYCONFIG_MAX_MODES: u32 = 128;

/// Windows HDR desktop: `SDRWhiteLevel` is the "SDR content brightness" slider.
/// `device_name` is the DXGI GDI name (`\\.\DISPLAYn`), the same string as
/// `DISPLAYCONFIG_SOURCE_DEVICE_NAME::viewGdiDeviceName`.
#[cfg(target_os = "windows")]
pub(crate) fn sdr_white_level_nits_for_gdi_name(device_name: &[u16; 32]) -> Option<f32> {
    use windows::Win32::Devices::Display::{
        QDC_ONLY_ACTIVE_PATHS, QDC_VIRTUAL_MODE_AWARE, QDC_VIRTUAL_REFRESH_RATE_AWARE,
    };

    // Mixed DPI needs virtual modes. Windows 11 dynamic refresh also needs
    // `QDC_VIRTUAL_REFRESH_RATE_AWARE`; without it the first query returns
    // `ERROR_INVALID_PARAMETER`. Dropping straight to `QDC_ONLY_ACTIVE_PATHS`
    // then fails on the same mixed-DPI desktop. A successful query that does
    // not match this GDI name tries the next flag set: virtual-mode paths can
    // disagree with the DXGI device name.
    let flag_sets = [
        QDC_ONLY_ACTIVE_PATHS | QDC_VIRTUAL_MODE_AWARE | QDC_VIRTUAL_REFRESH_RATE_AWARE,
        QDC_ONLY_ACTIVE_PATHS | QDC_VIRTUAL_MODE_AWARE,
        QDC_ONLY_ACTIVE_PATHS,
    ];
    for (index, flags) in flag_sets.into_iter().enumerate() {
        let last = index + 1 == flag_sets.len();
        match query_active_display_paths(flags) {
            ActivePathQuery::Paths(paths) => {
                if let Some(nits) = sdr_white_level_nits_for_device(&paths, device_name) {
                    return Some(nits);
                }
                if last {
                    return None;
                }
            }
            ActivePathQuery::UnsupportedFlags => continue,
            ActivePathQuery::Failed if !last => continue,
            ActivePathQuery::Failed => return None,
        }
    }
    None
}

#[cfg(target_os = "windows")]
enum ActivePathQuery {
    Paths(Vec<windows::Win32::Devices::Display::DISPLAYCONFIG_PATH_INFO>),
    UnsupportedFlags,
    Failed,
}

#[cfg(target_os = "windows")]
fn query_active_display_paths(
    flags: windows::Win32::Devices::Display::QUERY_DISPLAY_CONFIG_FLAGS,
) -> ActivePathQuery {
    use windows::Win32::Devices::Display::{
        DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, GetDisplayConfigBufferSizes,
        QueryDisplayConfig,
    };
    use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_INVALID_PARAMETER};

    for _attempt in 0..DISPLAYCONFIG_QUERY_ATTEMPTS {
        let mut path_count = 0_u32;
        let mut mode_count = 0_u32;
        let size_status =
            unsafe { GetDisplayConfigBufferSizes(flags, &mut path_count, &mut mode_count) };
        if size_status == ERROR_INVALID_PARAMETER {
            return ActivePathQuery::UnsupportedFlags;
        }
        if size_status.is_err()
            || path_count == 0
            || path_count > DISPLAYCONFIG_MAX_PATHS
            || mode_count > DISPLAYCONFIG_MAX_MODES
        {
            return ActivePathQuery::Failed;
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
        let mode_ptr = if mode_count == 0 {
            std::ptr::null_mut()
        } else {
            modes.as_mut_ptr()
        };
        let query_status = unsafe {
            QueryDisplayConfig(
                flags,
                &mut path_count,
                paths.as_mut_ptr(),
                &mut mode_count,
                mode_ptr,
                None,
            )
        };
        if query_status.is_ok() {
            paths.truncate(path_count as usize);
            return ActivePathQuery::Paths(paths);
        }
        if query_status == ERROR_INVALID_PARAMETER {
            return ActivePathQuery::UnsupportedFlags;
        }
        if query_status == ERROR_INSUFFICIENT_BUFFER {
            continue;
        }
        return ActivePathQuery::Failed;
    }
    ActivePathQuery::Failed
}

#[cfg(target_os = "windows")]
fn sdr_white_level_nits_for_device(
    paths: &[windows::Win32::Devices::Display::DISPLAYCONFIG_PATH_INFO],
    device_name: &[u16; 32],
) -> Option<f32> {
    use windows::Win32::Devices::Display::{
        DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
        DISPLAYCONFIG_SDR_WHITE_LEVEL, DISPLAYCONFIG_SOURCE_DEVICE_NAME,
        DisplayConfigGetDeviceInfo,
    };

    for path in paths {
        let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
        source.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
        source.header.size = std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
        source.header.adapterId = path.sourceInfo.adapterId;
        source.header.id = path.sourceInfo.id;
        if unsafe { DisplayConfigGetDeviceInfo(&mut source.header) } != 0 {
            continue;
        }
        if !gdi_device_names_equal(&source.viewGdiDeviceName, device_name) {
            continue;
        }

        let mut white = DISPLAYCONFIG_SDR_WHITE_LEVEL::default();
        white.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL;
        white.header.size = std::mem::size_of::<DISPLAYCONFIG_SDR_WHITE_LEVEL>() as u32;
        white.header.adapterId = path.targetInfo.adapterId;
        white.header.id = path.targetInfo.id;
        if unsafe { DisplayConfigGetDeviceInfo(&mut white.header) } != 0 {
            log::debug!(
                "[HDR] DisplayConfigGetDeviceInfo(SDR white level) failed for {}",
                monitor_device_name(device_name)
            );
            continue;
        }
        return super::sdr_white_level_raw_to_nits(white.SDRWhiteLevel);
    }
    log::debug!(
        "[HDR] no display-config path matched GDI device {}",
        monitor_device_name(device_name)
    );
    None
}

#[cfg(target_os = "windows")]
fn gdi_device_names_equal(left: &[u16; 32], right: &[u16; 32]) -> bool {
    monitor_device_name(left).eq_ignore_ascii_case(&monitor_device_name(right))
}

#[cfg(target_os = "windows")]
pub(crate) fn log_sdr_white_level_if_changed(label: &str, hdr_supported: bool, nits: Option<f32>) {
    struct LogKey {
        label: String,
        hdr_supported: bool,
        nits_bits: u32,
    }

    static LAST: parking_lot::Mutex<Option<LogKey>> = parking_lot::Mutex::new(None);
    let nits_bits = nits.map(f32::to_bits).unwrap_or(0);
    let unchanged = {
        let last = LAST.lock();
        last.as_ref().is_some_and(|prev| {
            prev.label == label
                && prev.hdr_supported == hdr_supported
                && prev.nits_bits == nits_bits
        })
    };
    if unchanged {
        return;
    }
    *LAST.lock() = Some(LogKey {
        label: label.to_string(),
        hdr_supported,
        nits_bits,
    });
    let scale = super::scrgb_output_scale_from_reference_nits(nits);
    if hdr_supported && nits.is_none() {
        log::warn!(
            "[HDR] Windows SDR white level unknown: monitor={label} hdr_supported=true \
             sdr_white_nits=None scrgb_output_scale={scale:.3}"
        );
    } else {
        log::info!(
            "[HDR] Windows SDR white level: monitor={label} hdr_supported={hdr_supported} \
             sdr_white_nits={nits:?} scrgb_output_scale={scale:.3}"
        );
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
