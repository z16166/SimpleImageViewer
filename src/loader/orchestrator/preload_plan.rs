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

//! Atomic preload plan snapshot for background workers (generation-plan §3.G).

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Default preload radius before the main thread publishes navigation.
/// Keep in sync with `crate::app::image_management::PREFETCH_WINDOW_DISTANCE`.
const DEFAULT_MAX_DISTANCE: usize = 2;

/// Main-thread snapshot of navigation / preload window for worker early-exit.
pub struct PreloadPlanSnapshot {
    pub current_index: AtomicUsize,
    pub image_count: AtomicUsize,
    pub max_distance: AtomicUsize,
    pub profile_epoch: AtomicU64,
}

impl PreloadPlanSnapshot {
    pub fn new() -> Self {
        Self {
            current_index: AtomicUsize::new(0),
            image_count: AtomicUsize::new(0),
            max_distance: AtomicUsize::new(DEFAULT_MAX_DISTANCE),
            profile_epoch: AtomicU64::new(0),
        }
    }

    pub fn write_navigation(
        &self,
        current_index: usize,
        image_count: usize,
        max_distance: usize,
    ) {
        self.current_index.store(current_index, Ordering::Release);
        self.image_count.store(image_count, Ordering::Release);
        self.max_distance.store(max_distance, Ordering::Release);
    }

    pub fn bump_profile_epoch(&self) -> u64 {
        self.profile_epoch.fetch_add(1, Ordering::Release) + 1
    }

    pub fn profile_epoch(&self) -> u64 {
        self.profile_epoch.load(Ordering::Acquire)
    }

    pub fn current_index(&self) -> usize {
        self.current_index.load(Ordering::Acquire)
    }

    pub fn image_count(&self) -> usize {
        self.image_count.load(Ordering::Acquire)
    }

    pub fn max_distance(&self) -> usize {
        self.max_distance.load(Ordering::Acquire)
    }

    pub fn index_in_window(&self, idx: usize) -> bool {
        let count = self.image_count();
        if count == 0 {
            // Navigation not published yet — do not drop tile/refine work (tests, early startup).
            return true;
        }
        let current = self.current_index();
        if idx == current {
            return true;
        }
        let max_distance = self.max_distance();
        let dist_forward = (idx + count - current) % count;
        let dist_backward = (current + count - idx) % count;
        dist_forward.min(dist_backward) <= max_distance
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_in_window_before_navigation_published() {
        let plan = PreloadPlanSnapshot::new();
        assert!(plan.index_in_window(99));
    }
}
