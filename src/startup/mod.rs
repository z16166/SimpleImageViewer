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

pub(crate) mod icon;
mod launch;
mod logging;
mod panic;
mod phases;

#[cfg(all(target_os = "windows", not(feature = "legacy_win7")))]
mod wgpu;

pub use launch::run;
pub(crate) use logging::shutdown_logger;

#[cfg(all(target_os = "windows", not(feature = "legacy_win7")))]
pub(crate) use wgpu::take_and_join_dx12_cache_validate_thread;
