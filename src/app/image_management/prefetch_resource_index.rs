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

//! Unified index of prefetch-related CPU/GPU cache entries for distant eviction scans.

use super::ImageViewerApp;

impl ImageViewerApp {
    pub(crate) fn register_prefetch_resource(&mut self, idx: usize) {
        self.prefetch_resource_indices.insert(idx);
    }

    pub(crate) fn maybe_unregister_prefetch_resource(&mut self, idx: usize) {
        if !self.index_has_prefetch_resource(idx) {
            self.prefetch_resource_indices.remove(&idx);
        }
    }

    pub(crate) fn clear_prefetch_resource_indices(&mut self) {
        self.prefetch_resource_indices.clear();
    }

    pub(super) fn relocate_prefetch_resource_index(&mut self, from: usize, to: usize) {
        if self.prefetch_resource_indices.remove(&from) {
            self.prefetch_resource_indices.insert(to);
        }
        if self.index_has_prefetch_resource(to) {
            self.prefetch_resource_indices.insert(to);
        }
        self.maybe_unregister_prefetch_resource(from);
    }

    fn index_has_prefetch_resource(&self, idx: usize) -> bool {
        self.prefetched_tiles.contains_key(&idx)
            || self.deferred_sdr_uploads.contains_key(&idx)
            || self.hdr_image_cache.contains_key(&idx)
            || self.hdr_tiled_source_cache.contains_key(&idx)
            || self.texture_cache.contains(idx)
            || self.animation_cache.contains_key(&idx)
            || crate::tile_cache::PIXEL_CACHE.lock().has_image(idx)
    }
}
