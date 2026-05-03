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

use std::time::{Duration, Instant};

use eframe::egui;

use super::renderer::HdrRenderOutputMode;
use super::types::HdrOutputMode;

const HDR_MONITOR_PROBE_INTERVAL: Duration = Duration::from_millis(750);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HdrMonitorSignature {
    outer_rect: Option<[i32; 4]>,
    monitor_size: Option<[i32; 2]>,
    native_pixels_per_point_milli: Option<i32>,
}

impl HdrMonitorSignature {
    pub fn from_viewport(viewport: &egui::ViewportInfo) -> Self {
        Self {
            outer_rect: viewport.outer_rect.map(|rect| {
                [
                    rect.min.x.round() as i32,
                    rect.min.y.round() as i32,
                    rect.max.x.round() as i32,
                    rect.max.y.round() as i32,
                ]
            }),
            monitor_size: viewport
                .monitor_size
                .map(|size| [size.x.round() as i32, size.y.round() as i32]),
            native_pixels_per_point_milli: viewport
                .native_pixels_per_point
                .map(|value| (value * 1000.0).round() as i32),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct HdrMonitorSelection {
    pub hdr_supported: bool,
    pub label: String,
    pub max_luminance_nits: Option<f32>,
    pub max_full_frame_luminance_nits: Option<f32>,
    pub max_hdr_capacity: Option<f32>,
    pub hdr_capacity_source: Option<&'static str>,
}

#[derive(Debug)]
pub struct HdrMonitorState {
    last_signature: Option<HdrMonitorSignature>,
    last_probe_at: Option<Instant>,
    selection: Option<HdrMonitorSelection>,
}

impl Default for HdrMonitorState {
    fn default() -> Self {
        Self {
            last_signature: None,
            last_probe_at: None,
            selection: None,
        }
    }
}

impl HdrMonitorState {
    pub fn selection(&self) -> Option<&HdrMonitorSelection> {
        self.selection.as_ref()
    }

    pub fn refresh_from_viewport(
        &mut self,
        ctx: &egui::Context,
        now: Instant,
        hdr_content_visible: bool,
    ) -> Option<&HdrMonitorSelection> {
        let signature = ctx.input(|input| HdrMonitorSignature::from_viewport(input.viewport()));
        if !self.should_probe(signature, now, hdr_content_visible) {
            return self.selection.as_ref();
        }

        self.last_signature = Some(signature);
        self.last_probe_at = Some(now);
        match active_monitor_hdr_status() {
            Ok(selection) => {
                if self.selection.as_ref() != Some(&selection) {
                    log::info!(
                        "[HDR] active_monitor={} hdr_supported={} max_luminance_nits={:?} max_full_frame_luminance_nits={:?} max_hdr_capacity={:?} hdr_capacity_source={:?}",
                        selection.label,
                        selection.hdr_supported,
                        selection.max_luminance_nits,
                        selection.max_full_frame_luminance_nits,
                        selection.max_hdr_capacity,
                        selection.hdr_capacity_source
                    );
                }
                self.selection = Some(selection);
            }
            Err(err) => {
                log::debug!("[HDR] active monitor HDR probe unavailable: {err}");
            }
        }
        self.selection.as_ref()
    }

    fn should_probe(
        &self,
        signature: HdrMonitorSignature,
        now: Instant,
        hdr_content_visible: bool,
    ) -> bool {
        self.should_probe_for_platform(
            signature,
            now,
            hdr_content_visible,
            cfg!(target_os = "macos"),
        )
    }

    fn should_probe_for_platform(
        &self,
        signature: HdrMonitorSignature,
        now: Instant,
        hdr_content_visible: bool,
        supports_current_edr_reprobe: bool,
    ) -> bool {
        let interval_elapsed = match self.last_probe_at {
            Some(last_probe_at) => now.duration_since(last_probe_at) >= HDR_MONITOR_PROBE_INTERVAL,
            None => true,
        };
        if self.last_signature == Some(signature) {
            return hdr_content_visible
                && self.should_reprobe_current_edr_capacity(supports_current_edr_reprobe)
                && interval_elapsed;
        }

        interval_elapsed
    }

    fn should_reprobe_current_edr_capacity(&self, supports_current_edr_reprobe: bool) -> bool {
        if !supports_current_edr_reprobe {
            return false;
        }
        self.selection.as_ref().is_some_and(|selection| {
            selection.hdr_supported && selection.max_hdr_capacity.is_none()
        })
    }
}

pub fn effective_render_output_mode(
    target_format: Option<wgpu::TextureFormat>,
    selection: Option<&HdrMonitorSelection>,
) -> HdrRenderOutputMode {
    let Some(target_format) = target_format else {
        return HdrRenderOutputMode::SdrToneMapped;
    };
    // Conservative fail-closed gate: only enable native scRGB / EDR presentation when we
    // have an explicit, positive confirmation that the active monitor supports HDR. When
    // the probe has not yet completed, failed silently, or reports `hdr_supported = false`
    // (e.g. Windows Settings says "不支持" / "not supported"), composit through the SDR
    // tone-mapped path so γ encoding for the actual SDR panel is correct.
    let Some(selection) = selection else {
        return HdrRenderOutputMode::SdrToneMapped;
    };
    if !selection.hdr_supported {
        return HdrRenderOutputMode::SdrToneMapped;
    }
    HdrRenderOutputMode::for_target_format(target_format)
}

pub fn effective_capability_output_mode(
    target_format: Option<wgpu::TextureFormat>,
    selection: Option<&HdrMonitorSelection>,
) -> HdrOutputMode {
    match effective_render_output_mode(target_format, selection) {
        HdrRenderOutputMode::NativeHdr => {
            if cfg!(target_os = "windows") {
                HdrOutputMode::WindowsScRgb
            } else if cfg!(target_os = "macos") {
                HdrOutputMode::MacOsEdr
            } else {
                HdrOutputMode::SdrToneMapped
            }
        }
        HdrRenderOutputMode::SdrToneMapped => HdrOutputMode::SdrToneMapped,
    }
}

#[cfg(target_os = "windows")]
pub fn active_monitor_hdr_status() -> Result<HdrMonitorSelection, String> {
    windows_active_monitor_hdr_status()
}

#[cfg(target_os = "macos")]
pub fn active_monitor_hdr_status() -> Result<HdrMonitorSelection, String> {
    macos_active_monitor_hdr_status()
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn active_monitor_hdr_status() -> Result<HdrMonitorSelection, String> {
    Err("active monitor HDR probing is not implemented on this platform".to_string())
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn macos_edr_selection_from_values(
    label: String,
    current_edr_capacity: f32,
    potential_edr_capacity: f32,
    reference_edr_capacity: f32,
) -> HdrMonitorSelection {
    let (capacity, source) = if let Some(value) =
        finite_positive_capacity(current_edr_capacity).filter(|value| *value > 1.0)
    {
        (
            Some(value),
            Some("macOS maximumExtendedDynamicRangeColorComponentValue"),
        )
    } else {
        (None, None)
    };
    let hdr_supported = capacity.is_some()
        || finite_positive_capacity(potential_edr_capacity).is_some_and(|value| value > 1.0)
        || finite_positive_capacity(reference_edr_capacity).is_some_and(|value| value > 1.0);
    HdrMonitorSelection {
        hdr_supported,
        label,
        max_luminance_nits: None,
        max_full_frame_luminance_nits: None,
        max_hdr_capacity: capacity,
        hdr_capacity_source: source,
    }
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn finite_positive_capacity(value: f32) -> Option<f32> {
    (value.is_finite() && value > 0.0).then_some(value)
}

#[cfg(target_os = "windows")]
fn windows_active_monitor_hdr_status() -> Result<HdrMonitorSelection, String> {
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
    use windows::Win32::Graphics::Dxgi::Common::DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020;
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, DXGI_ERROR_NOT_FOUND, IDXGIFactory1, IDXGIOutput6,
    };
    use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONEAREST, MonitorFromWindow};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GA_ROOT, GW_OWNER, GetAncestor, GetWindow, GetWindowThreadProcessId,
        IsWindowVisible,
    };
    use windows::core::Interface;

    unsafe extern "system" fn enum_process_windows(hwnd: HWND, lparam: LPARAM) -> BOOL {
        // Pick the first visible top-level window owned by the current process. We avoid
        // matching by window title because the egui main window is localized (e.g. Chinese
        // builds use "简易图片查看器"); any title-substring filter would silently fail on
        // non-English locales and the HDR monitor probe would never populate `selection`,
        // leaving the renderer to optimistically pick scRGB native HDR on physically SDR
        // displays.
        let state = unsafe { &mut *(lparam.0 as *mut EnumWindowState) };
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
        // Skip child / owned windows so we land on the actual application top-level frame
        // rather than a transient tooltip / IME window that may belong to the same process.
        if unsafe { GetAncestor(hwnd, GA_ROOT) } != hwnd {
            return true.into();
        }
        if !unsafe { GetWindow(hwnd, GW_OWNER) }.unwrap_or_default().is_invalid() {
            return true.into();
        }

        state.hwnd = Some(hwnd);
        false.into()
    }

    struct EnumWindowState {
        process_id: u32,
        hwnd: Option<HWND>,
    }

    let mut state = EnumWindowState {
        process_id: std::process::id(),
        hwnd: None,
    };
    unsafe {
        EnumWindows(
            Some(enum_process_windows),
            LPARAM((&mut state as *mut EnumWindowState) as isize),
        )
        .map_err(|err| err.to_string())?;
    }
    let hwnd = state
        .hwnd
        .ok_or_else(|| "Simple Image Viewer window handle was not found".to_string())?;
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    if monitor.is_invalid() {
        return Err("active window monitor was not found".to_string());
    }

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
                if desc.Monitor == monitor {
                    let device_name = monitor_device_name(&desc.DeviceName);
                    let hdr_supported = desc.BitsPerColor > 8
                        && desc.ColorSpace == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020;
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
fn finite_positive_luminance(value: f32) -> Option<f32> {
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

#[cfg(not(target_os = "windows"))]
#[allow(dead_code)]
pub fn any_active_output_supports_hdr() -> Result<bool, String> {
    Err("pre-creation HDR availability probing is only implemented on Windows".to_string())
}

/// Outcome of probing the monitor where the application window will spawn.
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
}

/// Probe HDR support of the **monitor where the window is most likely to spawn**
/// rather than "any output". On a mixed-monitor system (one HDR + one SDR) the
/// any-output probe always returns `Ok(true)` because the HDR display exists,
/// which then locks the swap chain into `Rgba16Float`. When the user actually
/// starts the app on the SDR monitor, Windows DWM has to convert scRGB → SDR and
/// can introduce subtle HDR-style processing. Probing the spawn monitor lets us
/// pick `Bgra8Unorm` directly and bypass DWM conversion entirely.
///
/// Spawn point selection:
/// 1. `GetCursorPos` — the user's cursor monitor. The window typically opens on
///    the same display as where the user double-clicked / launched the shortcut.
/// 2. Fallback: `MONITOR_DEFAULTTOPRIMARY` (Windows primary monitor).
///
/// Returns `Err(...)` when DXGI enumeration fails or the platform does not
/// support this probing path; callers should fall back to the platform default.
#[cfg(target_os = "windows")]
pub fn spawn_monitor_hdr_status() -> Result<SpawnMonitorHdrProbe, String> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Dxgi::Common::DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020;
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, DXGI_ERROR_NOT_FOUND, IDXGIFactory1, IDXGIOutput6,
    };
    use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTOPRIMARY, MonitorFromPoint};
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    use windows::core::Interface;

    let mut cursor = POINT::default();
    let (monitor, origin) = match unsafe { GetCursorPos(&mut cursor) } {
        Ok(()) => (
            unsafe { MonitorFromPoint(cursor, MONITOR_DEFAULTTOPRIMARY) },
            "cursor",
        ),
        Err(_) => (
            unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) },
            "primary",
        ),
    };
    if monitor.is_invalid() {
        return Err("spawn monitor handle was not found".to_string());
    }

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
                && desc.Monitor == monitor
            {
                let hdr_supported = desc.BitsPerColor > 8
                    && desc.ColorSpace == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020;
                let label = monitor_device_name(&desc.DeviceName);
                return Ok(SpawnMonitorHdrProbe {
                    hdr_supported,
                    label,
                    origin,
                });
            }
            output_index += 1;
        }

        adapter_index += 1;
    }

    Err("spawn monitor was not matched to any DXGI output".to_string())
}

#[cfg(not(target_os = "windows"))]
pub fn spawn_monitor_hdr_status() -> Result<SpawnMonitorHdrProbe, String> {
    Err("spawn-monitor HDR probing is only implemented on Windows".to_string())
}

#[cfg(target_os = "macos")]
fn macos_active_monitor_hdr_status() -> Result<HdrMonitorSelection, String> {
    let screen = unsafe {
        let app_class = objc_class("NSApplication")?;
        let app = objc_msg_send_id(app_class, objc_sel("sharedApplication")?);
        let mut window = if app.is_null() {
            std::ptr::null_mut()
        } else {
            objc_msg_send_id(app, objc_sel("keyWindow")?)
        };
        if window.is_null() && !app.is_null() {
            window = objc_msg_send_id(app, objc_sel("mainWindow")?);
        }

        let mut screen = if window.is_null() {
            std::ptr::null_mut()
        } else {
            objc_msg_send_id(window, objc_sel("screen")?)
        };
        if screen.is_null() {
            let screen_class = objc_class("NSScreen")?;
            screen = objc_msg_send_id(screen_class, objc_sel("mainScreen")?);
        }
        screen
    };
    if screen.is_null() {
        return Err("active NSScreen was not found".to_string());
    }

    let label = unsafe {
        let localized_name = objc_msg_send_id(screen, objc_sel("localizedName")?);
        ns_string_to_string(localized_name).unwrap_or_else(|| "macOS screen".to_string())
    };
    let current = unsafe {
        objc_msg_send_f64(
            screen,
            objc_sel("maximumExtendedDynamicRangeColorComponentValue")?,
        ) as f32
    };
    let potential = unsafe {
        objc_msg_send_f64(
            screen,
            objc_sel("maximumPotentialExtendedDynamicRangeColorComponentValue")?,
        ) as f32
    };
    let reference = unsafe {
        objc_msg_send_f64(
            screen,
            objc_sel("maximumReferenceExtendedDynamicRangeColorComponentValue")?,
        ) as f32
    };

    Ok(macos_edr_selection_from_values(
        label, current, potential, reference,
    ))
}

#[cfg(target_os = "macos")]
type ObjcId = *mut std::ffi::c_void;

#[cfg(target_os = "macos")]
type ObjcSel = *mut std::ffi::c_void;

#[cfg(target_os = "macos")]
#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {}

#[cfg(target_os = "macos")]
#[link(name = "objc")]
unsafe extern "C" {
    fn objc_getClass(name: *const std::ffi::c_char) -> ObjcId;
    fn sel_registerName(name: *const std::ffi::c_char) -> ObjcSel;
    #[link_name = "objc_msgSend"]
    fn objc_msg_send_id(receiver: ObjcId, selector: ObjcSel) -> ObjcId;
}

#[cfg(target_os = "macos")]
fn objc_class(name: &str) -> Result<ObjcId, String> {
    let name = std::ffi::CString::new(name).map_err(|err| err.to_string())?;
    let class = unsafe { objc_getClass(name.as_ptr()) };
    if class.is_null() {
        Err(format!(
            "Objective-C class was not found: {}",
            name.to_string_lossy()
        ))
    } else {
        Ok(class)
    }
}

#[cfg(target_os = "macos")]
fn objc_sel(name: &str) -> Result<ObjcSel, String> {
    let name = std::ffi::CString::new(name).map_err(|err| err.to_string())?;
    let selector = unsafe { sel_registerName(name.as_ptr()) };
    if selector.is_null() {
        Err(format!(
            "Objective-C selector was not found: {}",
            name.to_string_lossy()
        ))
    } else {
        Ok(selector)
    }
}

#[cfg(target_os = "macos")]
unsafe fn objc_msg_send_f64(receiver: ObjcId, selector: ObjcSel) -> f64 {
    let send: unsafe extern "C" fn(ObjcId, ObjcSel) -> f64 =
        unsafe { std::mem::transmute(objc_msg_send_id as *const ()) };
    unsafe { send(receiver, selector) }
}

#[cfg(target_os = "macos")]
unsafe fn ns_string_to_string(value: ObjcId) -> Option<String> {
    if value.is_null() {
        return None;
    }
    let ptr = unsafe { objc_msg_send_id(value, objc_sel("UTF8String").ok()?) };
    if ptr.is_null() {
        return None;
    }
    let text = unsafe { std::ffi::CStr::from_ptr(ptr.cast()).to_string_lossy() };
    Some(text.into_owned())
}

#[cfg(target_os = "windows")]
fn monitor_device_name(name: &[u16; 32]) -> String {
    let len = name
        .iter()
        .position(|value| *value == 0)
        .unwrap_or(name.len());
    String::from_utf16_lossy(&name[..len])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monitor_probe_runs_first_time_and_only_after_signature_change_with_throttle() {
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
        assert!(!state.should_probe(first, start + HDR_MONITOR_PROBE_INTERVAL * 2, false));
        assert!(!state.should_probe(moved, start + Duration::from_millis(100), false));
        assert!(state.should_probe(moved, start + HDR_MONITOR_PROBE_INTERVAL, false));
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
        assert!(!state.should_probe_for_platform(
            signature,
            start + HDR_MONITOR_PROBE_INTERVAL,
            false,
            true
        ));
        assert!(!state.should_probe_for_platform(
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
        };
        let hdr = HdrMonitorSelection {
            hdr_supported: true,
            label: "HDR".to_string(),
            max_luminance_nits: Some(1000.0),
            max_full_frame_luminance_nits: Some(500.0),
            max_hdr_capacity: None,
            hdr_capacity_source: Some("Windows DXGI MaxLuminance"),
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
            effective_capability_output_mode(
                Some(wgpu::TextureFormat::Rgba16Float),
                Some(&selection)
            ),
            HdrOutputMode::MacOsEdr
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_non_edr_selected_monitor_forces_sdr_tone_mapping() {
        let selection = macos_edr_selection_from_values("SDR".to_string(), 1.0, 1.0, 0.0);

        assert_eq!(
            effective_capability_output_mode(
                Some(wgpu::TextureFormat::Rgba16Float),
                Some(&selection)
            ),
            HdrOutputMode::SdrToneMapped
        );
    }
}
