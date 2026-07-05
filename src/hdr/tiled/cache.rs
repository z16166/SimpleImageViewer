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
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use super::buffer::HdrTileBuffer;
use super::globals::{
    DEFAULT_HDR_TILE_CACHE_MAX_BYTES, HDR_TILE_CACHE_MAX_BYTES, HdrTileCacheKey,
    MAX_HDR_TILE_CACHE_MAX_BYTES,
};

pub(crate) fn configured_hdr_tile_cache_max_bytes() -> usize {
    HDR_TILE_CACHE_MAX_BYTES.load(Ordering::Relaxed)
}

pub(crate) fn configure_hdr_tile_cache_budget_from_system_memory() {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    HDR_TILE_CACHE_MAX_BYTES.store(
        hdr_tile_cache_budget_for_memory(sys.total_memory() as usize),
        Ordering::Relaxed,
    );
}

pub(crate) fn hdr_tile_cache_budget_for_memory(total_memory_bytes: usize) -> usize {
    (total_memory_bytes / 16).clamp(
        DEFAULT_HDR_TILE_CACHE_MAX_BYTES,
        MAX_HDR_TILE_CACHE_MAX_BYTES,
    )
}

#[cfg(test)]
pub(crate) fn set_global_hdr_tile_cache_max_bytes_for_tests(max_bytes: usize) {
    HDR_TILE_CACHE_MAX_BYTES.store(max_bytes, Ordering::Relaxed);
}

#[derive(Debug)]
pub(crate) struct HdrTileCache {
    entries: HashMap<HdrTileCacheKey, Arc<HdrTileBuffer>>,
    evictable_lru: crate::lru_order::LruOrder<HdrTileCacheKey>,
    protected: HashSet<HdrTileCacheKey>,
    current_bytes: usize,
    max_bytes: usize,
}

impl HdrTileCache {
    pub(crate) fn new(max_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            evictable_lru: crate::lru_order::LruOrder::default(),
            protected: HashSet::new(),
            current_bytes: 0,
            max_bytes,
        }
    }

    pub(crate) fn get(&mut self, key: HdrTileCacheKey) -> Option<Arc<HdrTileBuffer>> {
        let tile = self.entries.get(&key).cloned()?;
        self.touch(key);
        Some(tile)
    }

    pub(crate) fn insert(&mut self, key: HdrTileCacheKey, tile: Arc<HdrTileBuffer>) {
        if let Some(old_tile) = self.entries.remove(&key) {
            self.current_bytes = self.current_bytes.saturating_sub(tile_len_bytes(&old_tile));
            self.evictable_lru.remove(key);
        }

        let bytes = tile_len_bytes(&tile);
        while !self.evictable_lru.is_empty()
            && self.current_bytes.saturating_add(bytes) > self.max_bytes
        {
            let Some(evicted_key) = self.evictable_lru.pop_oldest() else {
                break;
            };
            if let Some(evicted_tile) = self.entries.remove(&evicted_key) {
                self.current_bytes = self
                    .current_bytes
                    .saturating_sub(tile_len_bytes(&evicted_tile));
            }
        }

        if self.current_bytes.saturating_add(bytes) <= self.max_bytes {
            self.entries.insert(key, tile);
            if !self.protected.contains(&key) {
                self.evictable_lru.touch(key);
            }
            self.current_bytes += bytes;
        }
    }

    fn touch(&mut self, key: HdrTileCacheKey) {
        if !self.protected.contains(&key) {
            self.evictable_lru.touch(key);
        }
    }

    pub(crate) fn set_protected_keys(&mut self, keys: impl IntoIterator<Item = HdrTileCacheKey>) {
        let new_protected: HashSet<_> = keys.into_iter().collect();

        for key in self.protected.difference(&new_protected) {
            if self.entries.contains_key(key) {
                self.evictable_lru.touch(*key);
            }
        }

        for key in new_protected.difference(&self.protected) {
            self.evictable_lru.remove(*key);
        }

        self.protected = new_protected;
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    pub(crate) fn current_bytes(&self) -> usize {
        self.current_bytes
    }

    #[cfg(test)]
    pub(crate) fn max_bytes(&self) -> usize {
        self.max_bytes
    }
}

fn tile_len_bytes(tile: &HdrTileBuffer) -> usize {
    tile.rgba_f32.len() * std::mem::size_of::<f32>()
}
