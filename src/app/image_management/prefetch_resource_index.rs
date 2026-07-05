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
//!
//! Prefer [`PrefetchResourceGuard`] or the `insert_*_tracked` helpers below instead of
//! calling [`ImageViewerApp::track_prefetch_resource`] at every cache mutation site.

use super::{AnimationPlayback, ImageViewerApp};
use crate::loader::{DecodedImage, TextureCacheInsert};
use crate::tile_cache::TileManager;
use eframe::egui::TextureHandle;
use std::sync::Arc;

/// Registers `idx` on creation; on drop, syncs the denormalized index unless [`Self::commit`]
/// was called after a successful cache install.
pub(crate) struct PrefetchResourceGuard<'a> {
    app: &'a mut ImageViewerApp,
    idx: usize,
    armed: bool,
}

impl<'a> PrefetchResourceGuard<'a> {
    pub fn new(app: &'a mut ImageViewerApp, idx: usize) -> Self {
        app.track_prefetch_resource(idx);
        Self {
            app,
            idx,
            armed: true,
        }
    }

    /// Keep `idx` registered after this guard is dropped.
    pub fn commit(mut self) {
        self.armed = false;
    }
}

impl Drop for PrefetchResourceGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.app.sync_prefetch_resource_index(self.idx);
        }
    }
}

impl ImageViewerApp {
    pub(super) fn track_prefetch_resource(&mut self, idx: usize) {
        self.prefetch_resource_indices.insert(idx);
    }

    pub(super) fn sync_prefetch_resource_index(&mut self, idx: usize) {
        if !self.index_has_prefetch_resource(idx) {
            self.prefetch_resource_indices.remove(&idx);
        }
    }

    pub(crate) fn maybe_unregister_prefetch_resource(&mut self, idx: usize) {
        self.sync_prefetch_resource_index(idx);
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
        self.sync_prefetch_resource_index(from);
    }

    pub(super) fn note_prefetch_tile_pixels_loaded(&mut self, idx: usize) {
        self.track_prefetch_resource(idx);
    }

    pub(super) fn insert_prefetched_tiles_tracked(&mut self, idx: usize, tm: TileManager) {
        self.prefetched_tiles.insert(idx, tm);
        self.track_prefetch_resource(idx);
    }

    pub(super) fn insert_hdr_image_cache_tracked(
        &mut self,
        idx: usize,
        hdr: Arc<crate::hdr::types::HdrImageBuffer>,
    ) {
        self.hdr_image_cache.insert(idx, hdr);
        self.track_prefetch_resource(idx);
    }

    pub(super) fn insert_animation_cache_tracked(
        &mut self,
        idx: usize,
        playback: AnimationPlayback,
    ) {
        self.animation_cache.insert(idx, playback);
        self.track_prefetch_resource(idx);
    }

    pub(super) fn insert_texture_cache_tracked(
        &mut self,
        idx: usize,
        handle: TextureHandle,
        meta: TextureCacheInsert,
    ) -> Option<usize> {
        let evicted = self.texture_cache.insert(idx, handle, meta);
        self.track_prefetch_resource(idx);
        if let Some(evicted_idx) = evicted {
            self.handle_texture_cache_eviction(evicted_idx);
        }
        evicted
    }

    pub(super) fn store_deferred_sdr_upload_tracked(&mut self, idx: usize, decoded: DecodedImage) {
        use std::collections::hash_map::Entry;

        if let Entry::Occupied(mut slot) = self.deferred_sdr_uploads.entry(idx) {
            *slot.get_mut() = decoded;
            self.track_prefetch_resource(idx);
            return;
        }
        if self.deferred_sdr_uploads.len() >= crate::app::MAX_DEFERRED_SDR_UPLOADS {
            let current = self.current_index;
            let total = self.image_files.len();
            if let Some(evict_idx) = self
                .deferred_sdr_uploads
                .keys()
                .copied()
                .max_by_key(|&i| super::prefetch_circular_distance(current, total, i))
            {
                self.deferred_sdr_uploads.remove(&evict_idx);
                self.sync_prefetch_resource_index(evict_idx);
            }
        }
        self.deferred_sdr_uploads.insert(idx, decoded);
        self.track_prefetch_resource(idx);
    }

    fn index_has_prefetch_resource(&self, idx: usize) -> bool {
        self.prefetched_tiles.contains_key(&idx)
            || self.deferred_sdr_uploads.contains_key(&idx)
            || self.hdr_image_cache.contains_key(&idx)
            || self.hdr_tiled_source_cache.contains_key(&idx)
            || self.texture_cache.contains(idx)
            || self.animation_cache.contains_key(&idx)
            || crate::tile_cache::PIXEL_CACHE.read().has_image(idx)
    }
}
