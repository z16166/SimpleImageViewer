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
mod macos;
mod probe;
mod state;
mod types;
mod windows;

#[cfg(target_os = "linux")]
mod wayland;

#[cfg(test)]
mod tests;

pub use types::{HdrMonitorSelection, HdrNativeSurfaceEncoding};
pub use state::HdrMonitorState;
pub use probe::spawn_monitor_hdr_status;
pub use effective::{
    effective_capability_output_mode,
    effective_monitor_selection, effective_render_output_mode,
};

#[cfg(test)]
pub(crate) use std::time::{Duration, Instant};
#[cfg(test)]
pub(crate) use crate::hdr::renderer::HdrRenderOutputMode;
#[cfg(test)]
pub(crate) use crate::hdr::types::HdrOutputMode;
#[cfg(test)]
pub(crate) use effective::active_monitor_hdr_status;
#[cfg(test)]
pub(crate) use macos::macos_edr_selection_from_values;
#[cfg(test)]
pub(crate) use types::{HdrMonitorSignature, HDR_MONITOR_PROBE_INTERVAL};
#[cfg(all(test, target_os = "windows"))]
pub(crate) use windows::dxgi_output_hdr_active;
#[cfg(all(test, target_os = "windows"))]
pub(crate) use ::windows::Win32::Graphics::Dxgi::Common::{
    DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020, DXGI_COLOR_SPACE_TYPE,
};

