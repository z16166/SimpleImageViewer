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

//! Shared knobs for decode tests (tiled pixel-threshold overrides).

use parking_lot::MutexGuard;

pub(crate) struct TiledThresholdOverride {
    was_active: bool,
    old_value: u64,
}

impl TiledThresholdOverride {
    pub(crate) fn set(value: u64) -> Self {
        let (was_active, old_value) = crate::tile_cache::tiled_threshold_override_snapshot();
        crate::tile_cache::set_tiled_threshold_override(value);
        Self {
            was_active,
            old_value,
        }
    }
}

impl Drop for TiledThresholdOverride {
    fn drop(&mut self) {
        if self.was_active {
            // Override was already active before `set`: restore its prior value.
            crate::tile_cache::set_tiled_threshold_override(self.old_value);
        } else {
            // Override was inactive before `set`: deactivate it rather than
            // leaving it permanently active with a stale value.
            crate::tile_cache::clear_tiled_threshold_override();
        }
    }
}

/// Guards tiled-routing test mutations (side limit, pixel budget, threshold
/// override). Delegates to the single shared lock in `tile_cache` so this
/// module's tests cannot race against `tile_cache`'s own tiled-plane tests.
pub(crate) fn lock_tiled_threshold_for_test() -> MutexGuard<'static, ()> {
    crate::tile_cache::lock_tiled_routing_for_test()
}
