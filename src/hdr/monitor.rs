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
                        "[HDR] active_monitor={} hdr_supported={} max_luminance_nits={:?} max_full_frame_luminance_nits={:?}",
                        selection.label,
                        selection.hdr_supported,
                        selection.max_luminance_nits,
                        selection.max_full_frame_luminance_nits
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

#[cfg(not(target_os = "windows"))]
pub fn active_monitor_hdr_status() -> Result<HdrMonitorSelection, String> {
    Err("active monitor HDR probing is not implemented on this platform".to_string())
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
        };
        let hdr = HdrMonitorSelection {
            hdr_supported: true,
            label: "HDR".to_string(),
            max_luminance_nits: Some(1000.0),
            max_full_frame_luminance_nits: Some(500.0),
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
}
