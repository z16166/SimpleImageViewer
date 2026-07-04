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

//! Throttled sysinfo RAM snapshot for preload memory budgeting.
//!
//! Call [`PreloadMemorySnapshot::refresh_if_stale`] from the logic thread only
//! ([`crate::app::ImageViewerApp::refresh_preload_memory_plan`]); hot paths read
//! cached MB values without triggering sysinfo refresh.

/// RAM stats for preload budgeting; delegates to [`crate::system_memory`].
pub(crate) struct PreloadMemorySnapshot;

impl PreloadMemorySnapshot {
    pub(crate) fn new() -> Self {
        Self
    }

    pub(crate) fn refresh_if_stale(&mut self) {
        crate::system_memory::refresh_if_stale();
    }

    pub(crate) fn available_memory_mb(&self) -> u64 {
        crate::system_memory::available_memory_mb()
    }

    pub(crate) fn total_memory_mb(&self) -> u64 {
        crate::system_memory::total_memory_mb()
    }
}
