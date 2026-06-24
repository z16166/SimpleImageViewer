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

use eframe::egui;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HdrMonitorSignature {
    pub(crate) outer_rect: Option<[i32; 4]>,
    pub(crate) monitor_size: Option<[i32; 2]>,
    pub(crate) native_pixels_per_point_milli: Option<i32>,
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

    /// HDR output follows the main image window only. Deferred child viewports (e.g. directory
    /// tree navigation) must not drive monitor probing or swap-chain decisions.
    pub fn from_main_viewport(ctx: &egui::Context) -> Self {
        ctx.viewport_for(egui::ViewportId::ROOT, |viewport| {
            Self::from_viewport(viewport.input.viewport())
        })
    }

    pub(crate) fn native_pixels_per_point(&self) -> Option<f32> {
        self.native_pixels_per_point_milli
            .map(|milli| milli as f32 / 1000.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HdrNativeSurfaceEncoding {
    LinearScRgb,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    PqHdr10,
    #[allow(dead_code)]
    Gamma22Electrical,
}

/// Transfer function reported by Wayland `wp_color_management` (Linux only).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxWaylandTransferFunction {
    Srgb,
    Gamma22,
    Bt1886,
    CompoundPower24,
    St2084,
    Hlg,
    Unknown,
}

/// Color gamut bucket from Wayland primaries (Linux only).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxWaylandColorPrimaries {
    /// sRGB / BT.709 and other conventional SDR gamuts.
    Narrow,
    /// BT.2020, Display P3, and other wide gamuts used for HDR offload.
    Wide,
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HdrMonitorSelection {
    pub hdr_supported: bool,
    pub label: String,
    pub max_luminance_nits: Option<f32>,
    pub max_full_frame_luminance_nits: Option<f32>,
    pub max_hdr_capacity: Option<f32>,
    pub hdr_capacity_source: Option<&'static str>,
    pub native_surface_encoding: Option<HdrNativeSurfaceEncoding>,
    /// Reference / mastering white from `wp_color_management` (Linux tone-map metadata).
    pub reference_luminance_nits: Option<f32>,
    /// Raw Wayland transfer function; populated by the Linux Wayland probe.
    pub linux_wp_transfer: Option<LinuxWaylandTransferFunction>,
    /// Raw Wayland primaries bucket; populated by the Linux Wayland probe.
    pub linux_wp_primaries: Option<LinuxWaylandColorPrimaries>,
}

impl HdrMonitorSelection {
    #[cfg(test)]
    pub fn new(label: impl Into<String>, hdr_supported: bool) -> Self {
        Self {
            label: label.into(),
            hdr_supported,
            max_luminance_nits: None,
            max_full_frame_luminance_nits: None,
            max_hdr_capacity: None,
            hdr_capacity_source: None,
            native_surface_encoding: None,
            reference_luminance_nits: None,
            linux_wp_transfer: None,
            linux_wp_primaries: None,
        }
    }
}

pub(crate) const HDR_MONITOR_PROBE_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(750);
