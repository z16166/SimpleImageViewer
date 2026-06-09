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

pub use types::{HdrMonitorSelection, HdrMonitorSignature, HdrNativeSurfaceEncoding};
pub use state::HdrMonitorState;
pub use probe::{spawn_monitor_hdr_status, SpawnMonitorHdrProbe};
pub use effective::{
    active_monitor_hdr_status, effective_capability_output_mode,
    effective_monitor_selection, effective_render_output_mode,
};
pub use windows::any_active_output_supports_hdr;

#[cfg(target_os = "windows")]
pub(crate) use windows::dxgi_output_hdr_active;
