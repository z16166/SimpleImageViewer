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

//! Preload queue pixel-data retention: window distance + in-flight registration.
//!
//! Replaces generation tolerance for prefetch CPU/GPU cache eviction. Current index
//! is always retained; neighbors within the effective preload window are retained;
//! window-outside entries survive only while a loader task is still registered.
//!
//! **Checklist #8:** `prefetched_tiles`, `hdr_image_cache`, and related index maps
//! are bounded by the circular preload window (see [`prefetch_window_index_cap`]) plus
//! at most one in-flight grace entry per active loader slot. Small folders
//! (`image_count <= 2 * max_distance + 1`) intentionally retain every index.

use super::{
    PREFETCH_WINDOW_DISTANCE, prefetch_circular_distance, prefetch_window_contains,
    should_skip_background_preloads_for_memory,
};

/// Why a prefetch cache entry is kept during distant eviction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PrefetchRetainReason {
    CurrentIndex,
    WithinWindow { distance: usize },
    InFlightLoad,
}

/// Why a prefetch cache entry is evicted during distant eviction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PrefetchEvictReason {
    EmptyList,
    OutsideWindow { distance: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PrefetchCacheRetention {
    Retain(PrefetchRetainReason),
    Evict(PrefetchEvictReason),
}

impl PrefetchCacheRetention {
    pub(super) fn should_retain(self) -> bool {
        matches!(self, Self::Retain(_))
    }

    #[allow(dead_code)]
    pub(super) fn log_label(self) -> &'static str {
        match self {
            Self::Retain(PrefetchRetainReason::CurrentIndex) => "current_index",
            Self::Retain(PrefetchRetainReason::WithinWindow { .. }) => "within_window",
            Self::Retain(PrefetchRetainReason::InFlightLoad) => "in_flight_load",
            Self::Evict(PrefetchEvictReason::EmptyList) => "empty_list",
            Self::Evict(PrefetchEvictReason::OutsideWindow { .. }) => "outside_window",
        }
    }
}

/// Effective neighbor preload radius aligned with `schedule_preloads` / memory guard.
pub(super) fn effective_prefetch_window_distance(
    available_memory_mb: u64,
    total_memory_mb: u64,
) -> usize {
    if should_skip_background_preloads_for_memory(available_memory_mb, total_memory_mb) {
        0
    } else {
        PREFETCH_WINDOW_DISTANCE
    }
}

/// Neighbor indices within the preload window in one ring direction (excludes current).
pub(super) fn prefetch_window_neighbors_in_direction(
    current_index: usize,
    image_count: usize,
    max_distance: usize,
    forward: bool,
) -> Vec<usize> {
    if image_count <= 1 || max_distance == 0 {
        return Vec::new();
    }
    (1..=max_distance)
        .map(|delta| {
            if forward {
                (current_index + delta) % image_count
            } else {
                (current_index + image_count - delta) % image_count
            }
        })
        .collect()
}

/// Max distinct indices inside the circular preload window (includes current).
#[allow(dead_code)]
pub(super) fn prefetch_window_index_cap(image_count: usize, max_distance: usize) -> usize {
    if image_count == 0 {
        return 0;
    }
    image_count.min(max_distance.saturating_mul(2).saturating_add(1))
}

/// Steady-state upper bound on `prefetched_tiles` length (current lives in `tile_manager`).
///
/// In-flight loads outside the window may temporarily add entries until navigation runs
/// `evict_distant_prefetch_caches`.
#[allow(dead_code)]
pub(super) fn prefetched_tiles_steady_state_cap(image_count: usize, max_distance: usize) -> usize {
    prefetch_window_index_cap(image_count, max_distance).saturating_sub(1)
}

/// Indices inside the circular preload window (includes current).
pub(super) fn prefetch_window_index_set(
    current_index: usize,
    image_count: usize,
    max_distance: usize,
) -> std::collections::HashSet<usize> {
    let mut indices = std::collections::HashSet::new();
    if image_count == 0 {
        return indices;
    }
    indices.insert(current_index);
    if max_distance == 0 {
        return indices;
    }
    for delta in 1..=max_distance {
        indices.insert((current_index + delta) % image_count);
        indices.insert((current_index + image_count - delta) % image_count);
    }
    indices
}

pub(super) fn prefetch_cache_retention(
    current_index: usize,
    image_count: usize,
    max_distance: usize,
    idx: usize,
    is_loading: bool,
) -> PrefetchCacheRetention {
    if image_count == 0 {
        return PrefetchCacheRetention::Evict(PrefetchEvictReason::EmptyList);
    }
    if idx == current_index {
        return PrefetchCacheRetention::Retain(PrefetchRetainReason::CurrentIndex);
    }
    let distance = prefetch_circular_distance(current_index, image_count, idx);
    if prefetch_window_contains(current_index, image_count, idx, max_distance) {
        return PrefetchCacheRetention::Retain(PrefetchRetainReason::WithinWindow { distance });
    }
    if is_loading {
        // Strategy B (in-flight grace): retain cache entries while a load is still registered,
        // even when the index left the preload window. Loader cancel clears registration next.
        return PrefetchCacheRetention::Retain(PrefetchRetainReason::InFlightLoad);
    }
    PrefetchCacheRetention::Evict(PrefetchEvictReason::OutsideWindow { distance })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_index_always_retained() {
        let decision = prefetch_cache_retention(5, 100, 2, 5, false);
        assert_eq!(
            decision,
            PrefetchCacheRetention::Retain(PrefetchRetainReason::CurrentIndex)
        );
    }

    #[test]
    fn within_window_neighbor_retained_without_loading() {
        let decision = prefetch_cache_retention(0, 100, 2, 2, false);
        assert!(matches!(
            decision,
            PrefetchCacheRetention::Retain(PrefetchRetainReason::WithinWindow { distance: 2 })
        ));
    }

    #[test]
    fn outside_window_evicted_when_not_loading() {
        let decision = prefetch_cache_retention(0, 100, 2, 5, false);
        assert!(matches!(
            decision,
            PrefetchCacheRetention::Evict(PrefetchEvictReason::OutsideWindow { distance: 5 })
        ));
    }

    #[test]
    fn outside_window_retained_while_in_flight() {
        let decision = prefetch_cache_retention(0, 100, 2, 5, true);
        assert_eq!(
            decision,
            PrefetchCacheRetention::Retain(PrefetchRetainReason::InFlightLoad)
        );
    }

    #[test]
    fn memory_guard_shrinks_window_to_current_only() {
        assert_eq!(effective_prefetch_window_distance(512, 4096), 0);
        let decision = prefetch_cache_retention(0, 100, 0, 1, false);
        assert!(matches!(
            decision,
            PrefetchCacheRetention::Evict(PrefetchEvictReason::OutsideWindow { distance: 1 })
        ));
    }

    #[test]
    fn normal_memory_uses_default_window_distance() {
        assert_eq!(
            effective_prefetch_window_distance(4096, 8192),
            PREFETCH_WINDOW_DISTANCE
        );
    }

    #[test]
    fn prefetch_window_caps_prefetched_tiles_for_large_folders() {
        let d = PREFETCH_WINDOW_DISTANCE;
        assert_eq!(prefetch_window_index_cap(100, d), 2 * d + 1);
        assert_eq!(prefetched_tiles_steady_state_cap(100, d), 2 * d);
    }

    #[test]
    fn neighbors_in_direction_respect_max_distance() {
        assert_eq!(
            prefetch_window_neighbors_in_direction(0, 10, 2, true),
            vec![1, 2]
        );
        assert_eq!(
            prefetch_window_neighbors_in_direction(0, 10, 2, false),
            vec![9, 8]
        );
        assert!(prefetch_window_neighbors_in_direction(0, 10, 0, true).is_empty());
    }

    #[test]
    fn small_folder_retains_all_indices_in_window() {
        let d = PREFETCH_WINDOW_DISTANCE;
        assert_eq!(prefetch_window_index_cap(3, d), 3);
        assert_eq!(prefetched_tiles_steady_state_cap(3, d), 2);
        for idx in 0..3 {
            assert!(prefetch_window_contains(0, 3, idx, d));
        }
    }

    #[test]
    fn memory_guard_window_cap_is_current_index_only() {
        assert_eq!(prefetch_window_index_cap(100, 0), 1);
        assert_eq!(prefetched_tiles_steady_state_cap(100, 0), 0);
    }

    #[test]
    fn prefetch_window_index_set_matches_contains() {
        let d = PREFETCH_WINDOW_DISTANCE;
        let set = prefetch_window_index_set(0, 10, d);
        assert_eq!(set.len(), prefetch_window_index_cap(10, d));
        for idx in 0..10 {
            assert_eq!(
                set.contains(&idx),
                prefetch_window_contains(0, 10, idx, d)
            );
        }
    }
}
