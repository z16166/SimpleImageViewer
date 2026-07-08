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

//! Shared knobs for decode tests (`TILED_THRESHOLD` overrides).

use parking_lot::{Mutex, MutexGuard};
use std::sync::LazyLock;

static TILED_THRESHOLD_TEST_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

pub(crate) struct TiledThresholdOverride {
    old_threshold: u64,
}

impl TiledThresholdOverride {
    pub(crate) fn set(value: u64) -> Self {
        let old_threshold = crate::tile_cache::get_tiled_threshold();
        crate::tile_cache::TILED_THRESHOLD.store(value, std::sync::atomic::Ordering::Release);
        Self { old_threshold }
    }
}

impl Drop for TiledThresholdOverride {
    fn drop(&mut self) {
        crate::tile_cache::TILED_THRESHOLD
            .store(self.old_threshold, std::sync::atomic::Ordering::Release);
    }
}

pub(crate) fn lock_tiled_threshold_for_test() -> MutexGuard<'static, ()> {
    TILED_THRESHOLD_TEST_LOCK.lock()
}
