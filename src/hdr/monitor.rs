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
fn dxgi_output_hdr_active(colorspace: DXGI_COLOR_SPACE_TYPE) -> bool {
    colorspace == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020
}

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
    /// Sticky flag so we only `warn!` on the first failure in a streak of
    /// consecutive failures (avoid log spam at 1.33 Hz). Cleared the moment
    /// any probe succeeds.
    last_probe_failed: bool,
}

impl Default for HdrMonitorState {
    fn default() -> Self {
        Self {
            last_signature: None,
            last_probe_at: None,
            selection: None,
            last_probe_failed: false,
        }
    }
}

impl HdrMonitorState {
    /// Same as [`Default::default`], but starts with a known DXGI spawn outcome so
    /// [`crate::hdr::monitor::effective_capability_output_mode`] matches the swap chain
    /// from frame zero (see [`crate::hdr::surface::initial_monitor_selection_from_environment_probe`]).
    pub fn with_initial_selection(selection: Option<HdrMonitorSelection>) -> Self {
        Self {
            last_signature: None,
            last_probe_at: None,
            selection,
            last_probe_failed: false,
        }
    }

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
        match active_monitor_hdr_status(signature.outer_rect) {
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
                // Promoted from debug → warn so it's visible at the default
                // log level: when the runtime active-monitor probe never
                // succeeds, the cross-monitor swap-chain hot-swap chain is
                // dead in the water (we have no `selection`, so
                // `desired_target_format_for_active_monitor` always returns
                // `Bgra8Unorm` and mismatches never fire). The first failure
                // is what we need to see in user logs to diagnose.
                if !self.last_probe_failed {
                    log::warn!(
                        "[HDR] active monitor HDR probe FAILED: {err} \
                         (will retry on next viewport change / 750ms; \
                         dynamic HDR↔SDR swap-chain switching is disabled \
                         until probe succeeds)"
                    );
                    self.last_probe_failed = true;
                } else {
                    log::debug!("[HDR] active monitor HDR probe still failing: {err}");
                }
            }
        }
        if self.selection.is_some() {
            self.last_probe_failed = false;
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
            if supports_current_edr_reprobe {
                return hdr_content_visible
                    && self.should_reprobe_current_edr_capacity(supports_current_edr_reprobe)
                    && interval_elapsed;
            }
            // Windows (and other non-macOS): `HdrMonitorSignature` can stay identical for
            // many frames while the native frame is dragged between monitors because
            // `egui::ViewportInfo::outer_rect` may not update until the move ends. The DXGI
            // monitor bound to the main HWND still changes; without a timer reprobe,
            // `active_monitor_hdr_status` never runs again and HDR↔SDR swap-chain switching
            // appears permanently stuck.
            //
            // Windows uses a shorter poll than `HDR_MONITOR_PROBE_INTERVAL` so cross-monitor
            // drags do not wait ~750ms after the outer rect stops changing.
            if cfg!(target_os = "windows") {
                return match self.last_probe_at {
                    Some(last_probe_at) => {
                        now.duration_since(last_probe_at) >= Duration::from_millis(200)
                    }
                    None => true,
                };
            }
            return interval_elapsed;
        }

        // Viewport signature changed (outer rect, reported monitor size, or PPP). This almost
        // always means a cross-monitor move or resize that can change DXGI `ColorSpace` / EDR
        // while the swap-chain format is being hot-swapped. We must **not** gate on the generic
        // 750 ms interval here: until `selection` catches up, `effective_render_output_mode` can
        // pair `Rgba16Float` with stale `hdr_supported = false`, which runs `encode_sdr` (γ
        // encoded for 8-bit) into a linear scRGB buffer — lifted blacks on SDR and visible color
        // skew when switching to HDR. Always probe immediately on signature change.
        true
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

/// `viewport_outer_rect_screen_px` is [`HdrMonitorSignature::outer_rect`] (used for
/// probe scheduling / signature only). On Windows the DXGI monitor is resolved from the
/// process **largest** visible top-level `HWND` via `MonitorFromWindow`, with periodic
/// reprobes when the signature is unchanged so cross-monitor drags still update after
/// `outer_rect` lag.
#[cfg(target_os = "windows")]
pub fn active_monitor_hdr_status(
    viewport_outer_rect_screen_px: Option<[i32; 4]>,
) -> Result<HdrMonitorSelection, String> {
    windows_active_monitor_hdr_status(viewport_outer_rect_screen_px)
}

#[cfg(target_os = "macos")]
pub fn active_monitor_hdr_status(
    _viewport_outer_rect_screen_px: Option<[i32; 4]>,
) -> Result<HdrMonitorSelection, String> {
    macos_active_monitor_hdr_status()
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn active_monitor_hdr_status(
    _viewport_outer_rect_screen_px: Option<[i32; 4]>,
) -> Result<HdrMonitorSelection, String> {
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
        if !unsafe { GetWindow(hwnd, GW_OWNER) }.unwrap_or_default().is_invalid() {
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
fn windows_active_monitor_hdr_status(
    viewport_outer_rect_screen_px: Option<[i32; 4]>,
) -> Result<HdrMonitorSelection, String> {
    use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONEAREST, MonitorFromWindow};

    let _ = viewport_outer_rect_screen_px;

    let candidates = windows_collect_process_tl_hwnds();
    if candidates.is_empty() {
        return Err("Simple Image Viewer window handle was not found".to_string());
    }

    // Pick the process top-level HWND with the largest screen area — the main egui frame
    // — then `MonitorFromWindow`. Z-order's first visible TL window can be an IME-sized
    // utility; `outer_rect` can also lag during drags. The native `GetWindowRect` for the
    // largest TL window tracks the compositor across monitors; this pairs with
    // [`HdrMonitorState::should_probe_for_platform`] timer reprobes on Windows.
    let hwnd = windows_pick_tl_hwnd_largest_screen_area(&candidates).unwrap_or(candidates[0]);
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    if monitor.is_invalid() {
        return Err("active window monitor was not found".to_string());
    }
    log::debug!(
        "[HDR] active-monitor probe: origin=largest_tl_hwnd hwnd={hwnd:?} monitor_handle={monitor:?}"
    );

    dxgi_hdr_selection_for_monitor_handle(monitor)
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
            unsafe { MonitorFromPoint(POINT { x: x + 20, y: y + 20 }, MONITOR_DEFAULTTOPRIMARY) },
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
                    });
                }
            }
            output_index += 1;
        }

        adapter_index += 1;
    }

    Err("spawn monitor was not matched to any DXGI output".to_string())
}

#[cfg(not(target_os = "windows"))]
pub fn spawn_monitor_hdr_status(
    _saved_window_top_left: Option<[i32; 2]>,
) -> Result<SpawnMonitorHdrProbe, String> {
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
            // Same signature: macOS only re-probes for EDR capacity, gated on `hdr_content_visible`.
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
        assert!(!state.should_probe_for_platform(
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
