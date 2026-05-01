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
    ) -> Option<&HdrMonitorSelection> {
        let signature = ctx.input(|input| HdrMonitorSignature::from_viewport(input.viewport()));
        if !self.should_probe(signature, now) {
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

    fn should_probe(&self, signature: HdrMonitorSignature, now: Instant) -> bool {
        if self.last_signature == Some(signature) {
            return false;
        }

        match self.last_probe_at {
            Some(last_probe_at) => now.duration_since(last_probe_at) >= HDR_MONITOR_PROBE_INTERVAL,
            None => true,
        }
    }
}

pub fn effective_render_output_mode(
    target_format: Option<wgpu::TextureFormat>,
    selection: Option<&HdrMonitorSelection>,
) -> HdrRenderOutputMode {
    let Some(target_format) = target_format else {
        return HdrRenderOutputMode::SdrToneMapped;
    };
    if selection.is_some_and(|selection| !selection.hdr_supported) {
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
    } else if let Some(value) =
        finite_positive_capacity(potential_edr_capacity).filter(|value| *value > 1.0)
    {
        (
            Some(value),
            Some("macOS maximumPotentialExtendedDynamicRangeColorComponentValue"),
        )
    } else if let Some(value) =
        finite_positive_capacity(reference_edr_capacity).filter(|value| *value > 1.0)
    {
        (
            Some(value),
            Some("macOS maximumReferenceExtendedDynamicRangeColorComponentValue"),
        )
    } else {
        (None, None)
    };
    HdrMonitorSelection {
        hdr_supported: capacity.is_some(),
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
        EnumWindows, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
    };
    use windows::core::Interface;

    unsafe extern "system" fn enum_process_windows(hwnd: HWND, lparam: LPARAM) -> BOOL {
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

        let mut title = [0_u16; 256];
        let title_len = unsafe { GetWindowTextW(hwnd, &mut title) };
        if title_len <= 0 {
            return true.into();
        }
        let title = String::from_utf16_lossy(&title[..title_len as usize]);
        if !title.contains("Simple Image Viewer") {
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
    #[link_name = "objc_msgSend"]
    fn objc_msg_send_f64(receiver: ObjcId, selector: ObjcSel) -> f64;
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

        assert!(state.should_probe(first, start));
        state.last_signature = Some(first);
        state.last_probe_at = Some(start);
        assert!(!state.should_probe(first, start + HDR_MONITOR_PROBE_INTERVAL * 2));
        assert!(!state.should_probe(moved, start + Duration::from_millis(100)));
        assert!(state.should_probe(moved, start + HDR_MONITOR_PROBE_INTERVAL));
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
