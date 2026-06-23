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

use std::time::Instant;

use super::PRELOAD_MEMORY_REFRESH_MIN_INTERVAL;

/// RAM stats for preload budgeting; refresh timestamp is private so callers cannot desync it.
pub(crate) struct PreloadMemorySnapshot {
    sys: sysinfo::System,
    refreshed_at: Option<Instant>,
}

impl PreloadMemorySnapshot {
    pub(crate) fn new() -> Self {
        Self {
            sys: sysinfo::System::new_with_specifics(
                sysinfo::RefreshKind::nothing()
                    .with_memory(sysinfo::MemoryRefreshKind::nothing().with_ram()),
            ),
            refreshed_at: None,
        }
    }

    pub(crate) fn refresh_if_stale(&mut self) {
        let now = Instant::now();
        if self
            .refreshed_at
            .is_some_and(|at| now.duration_since(at) < PRELOAD_MEMORY_REFRESH_MIN_INTERVAL)
        {
            return;
        }
        self.sys
            .refresh_memory_specifics(sysinfo::MemoryRefreshKind::nothing().with_ram());
        self.refreshed_at = Some(now);
    }

    pub(crate) fn available_memory_mb(&self) -> u64 {
        self.sys.available_memory() / (1024 * 1024)
    }

    pub(crate) fn total_memory_mb(&self) -> u64 {
        self.sys.total_memory() / (1024 * 1024)
    }
}
