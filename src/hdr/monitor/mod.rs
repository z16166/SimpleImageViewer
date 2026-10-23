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

mod effective;
#[cfg(any(target_os = "linux", test))]
mod kde;
mod macos;
#[cfg(target_os = "macos")]
mod macos_screen_parameters;
#[cfg(target_os = "macos")]
mod objc_util;
mod probe;
mod state;
mod types;
mod windows;

#[cfg(target_os = "linux")]
mod wayland;

#[cfg(test)]
mod tests;

pub use effective::{
    effective_capability_output_mode, effective_monitor_selection, effective_render_output_mode,
};
#[cfg(target_os = "linux")]
pub(crate) use kde::refresh_linux_explicit_hdr_state_from_kscreen;
pub use probe::spawn_monitor_hdr_status;
pub use state::HdrMonitorState;
pub use types::{HdrMonitorSelection, HdrNativeSurfaceEncoding};
#[cfg(any(target_os = "linux", test))]
pub use types::{LinuxExplicitHdrState, LinuxWaylandColorPrimaries, LinuxWaylandTransferFunction};

#[cfg(test)]
pub(crate) use crate::hdr::renderer::HdrRenderOutputMode;
#[cfg(all(test, target_os = "macos"))]
pub(crate) use crate::hdr::types::HdrOutputMode;
#[cfg(all(test, target_os = "windows"))]
pub(crate) use ::windows::Win32::Graphics::Dxgi::Common::{
    DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020, DXGI_COLOR_SPACE_TYPE,
};
#[cfg(all(test, target_os = "linux"))]
pub(crate) use effective::active_monitor_hdr_status;
#[cfg(test)]
pub(crate) use macos::macos_edr_selection_from_values;
#[cfg(test)]
pub(crate) use std::time::{Duration, Instant};
#[cfg(test)]
pub(crate) use types::{HDR_MONITOR_PROBE_INTERVAL, HdrMonitorSignature};
#[cfg(all(test, target_os = "windows"))]
pub(crate) use windows::dxgi_output_hdr_active;

/// scRGB linear 1.0 is 80 nits (DXGI `G10_NONE_P709` / D2D scene-referred white).
pub(crate) const SCRGB_REFERENCE_WHITE_NITS: f32 = 80.0;

/// Windows "SDR content brightness" tops out around 480 nits. Cap a bad query
/// so a garbage `SDRWhiteLevel` cannot blow the framebuffer out.
const SCRGB_OUTPUT_SCALE_MAX: f32 = 8.0;

/// `SDRWhiteLevel` counts thousandths of the 80-nit reference (`1000` = 80 nits).
#[cfg(target_os = "windows")]
const SDR_WHITE_LEVEL_MILLI: f32 = 1000.0;

/// `DISPLAYCONFIG_SDR_WHITE_LEVEL::SDRWhiteLevel` is a multiplier of 80 nits, times 1000.
/// `1000` -> 80 nits, `2000` -> 160 nits. Values above the scale cap stay as nits;
/// [`scrgb_output_scale_from_reference_nits`] clamps them. Dropping them would
/// fall back to an unboosted 80-nit picture.
#[cfg(target_os = "windows")]
pub(crate) fn sdr_white_level_raw_to_nits(raw: u32) -> Option<f32> {
    if raw == 0 {
        return None;
    }
    let nits = (raw as f32) / SDR_WHITE_LEVEL_MILLI * SCRGB_REFERENCE_WHITE_NITS;
    (nits.is_finite() && nits >= SCRGB_REFERENCE_WHITE_NITS).then_some(nits)
}

/// Scale applied to linear scRGB output so SDR white matches the OS slider.
/// `None` and sub-80 nits stay at 1.0 (80-nit reference, no extra boost).
pub(crate) fn scrgb_output_scale_from_reference_nits(reference_luminance_nits: Option<f32>) -> f32 {
    let Some(nits) = reference_luminance_nits else {
        return 1.0;
    };
    if !nits.is_finite() || nits < SCRGB_REFERENCE_WHITE_NITS {
        return 1.0;
    }
    (nits / SCRGB_REFERENCE_WHITE_NITS).clamp(1.0, SCRGB_OUTPUT_SCALE_MAX)
}

/// Keep the last real Windows SDR white when a later probe of the same HDR
/// monitor fails. The nits live only on the selection, and a failed read would
/// otherwise store `hdr_supported` with `None`. The one-frame copy used by
/// [`scrgb_output_scale_for_presentation`] then goes stale on the second
/// failure and the float picture drops back to 80 nits.
#[cfg(target_os = "windows")]
pub(crate) fn retain_windows_sdr_white_across_failed_read(
    previous: Option<&HdrMonitorSelection>,
    fresh: &mut HdrMonitorSelection,
) {
    if !fresh.hdr_supported || fresh.reference_luminance_nits.is_some() {
        return;
    }
    let Some(previous) = previous else {
        return;
    };
    if previous.hdr_supported && previous.label == fresh.label {
        fresh.reference_luminance_nits = previous.reference_luminance_nits;
    }
}

/// Windows scRGB boost for this frame.
///
/// Prefer the fresh probe when it has a real SDR white. Failed reads on the
/// same HDR monitor keep that white via
/// [`retain_windows_sdr_white_across_failed_read`], including a streak of
/// failures. This function still falls back to `previous` for the frame after
/// the window leaves an HDR monitor, while the swap chain is still float.
/// Callers on Linux must pass scale 1.0 instead: `reference_luminance_nits`
/// there is the Wayland reference white, not an SDR slider.
///
/// The returned scale may be above 1.0 before the swap chain is float. Only
/// float scRGB targets consume it.
pub(crate) fn scrgb_output_scale_for_presentation(
    fresh: Option<&HdrMonitorSelection>,
    previous: Option<&HdrMonitorSelection>,
    target_format: Option<wgpu::TextureFormat>,
) -> f32 {
    let format_is_scrgb_float = matches!(
        target_format,
        Some(wgpu::TextureFormat::Rgba16Float | wgpu::TextureFormat::Rgba32Float)
    );
    let fresh_nits = fresh
        .filter(|selection| selection.hdr_supported)
        .and_then(|selection| selection.reference_luminance_nits);
    let previous_nits = previous
        .filter(|selection| selection.hdr_supported)
        .and_then(|selection| selection.reference_luminance_nits);
    let nits = if fresh_nits.is_some() {
        fresh_nits
    } else if format_is_scrgb_float {
        previous_nits
    } else {
        None
    };
    scrgb_output_scale_from_reference_nits(nits)
}
