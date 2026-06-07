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

use super::*;

impl ImageViewerApp {
    const PREFETCH_WINDOW_DISTANCE: usize = 2;

    pub(crate) fn invalidate_random_slideshow_order(&mut self) {
        self.random_slideshow_order_ready = false;
    }

    pub(super) fn shuffle_current_image_list_preserving_pairs(&mut self) {
        let mut combined = image_file_size_pairs_with_missing_sizes_as_zero(
            std::mem::take(&mut self.image_files),
            std::mem::take(&mut self.file_byte_len_by_index),
        );
        combined.shuffle(&mut rand::thread_rng());
        let (paths, sizes): (Vec<_>, Vec<_>) = combined.into_iter().unzip();
        self.image_files = paths;
        self.file_byte_len_by_index = sizes;
    }

    pub(super) fn clear_index_keyed_state_after_list_reorder(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.loader.set_generation(self.generation);
        self.loader.cancel_all();
        self.texture_cache.clear_all();
        self.clear_hdr_image_state();
        self.prefetched_tiles.clear();
        self.animation = None;
        self.animation_cache.clear();
        self.pending_anim_frames = None;
        self.tile_manager = None;
        self.current_image_res = None;
        self.prev_texture = None;
        self.prev_hdr_image = None;
        self.transition_start = None;
        self.prefetch_prev_generation = None;
        crate::tile_cache::PIXEL_CACHE.lock().clear();
    }

    pub(super) fn relocate_index_keyed_cache(&mut self, from: usize, to: usize) {
        if from == to {
            return;
        }
        // 1. Texture cache
        self.texture_cache.relocate(from, to);

        // 2. HDR caches
        if let Some(hdr) = self.hdr_image_cache.remove(&from) {
            self.hdr_image_cache.insert(to, hdr);
        }
        if let Some(src) = self.hdr_tiled_source_cache.remove(&from) {
            self.hdr_tiled_source_cache.insert(to, src);
        }
        if let Some(prev) = self.hdr_tiled_preview_cache.remove(&from) {
            self.hdr_tiled_preview_cache.insert(to, prev);
        }

        // 3. Fallback sets
        if self.hdr_sdr_fallback_indices.remove(&from) {
            self.hdr_sdr_fallback_indices.insert(to);
        }
        if self.hdr_placeholder_fallback_indices.remove(&from) {
            self.hdr_placeholder_fallback_indices.insert(to);
        }
        if self.hdr_in_flight_fallback_refinements.remove(&from) {
            self.hdr_in_flight_fallback_refinements.insert(to);
        }
        if self.ultra_hdr_capacity_sensitive_indices.remove(&from) {
            self.ultra_hdr_capacity_sensitive_indices.insert(to);
        }

        // 4. Deferred uploads
        if let Some(upload) = self.deferred_sdr_uploads.remove(&from) {
            self.deferred_sdr_uploads.insert(to, upload);
        }

        // 5. Prefetched tiles / animations
        if let Some(mut tiles) = self.prefetched_tiles.remove(&from) {
            tiles.image_index = to;
            self.prefetched_tiles.insert(to, tiles);
        }
        if let Some(mut anim) = self.animation_cache.remove(&from) {
            anim.image_index = to;
            self.animation_cache.insert(to, anim);
        }
        if let Some(ref mut anim) = self.animation {
            if anim.image_index == from {
                anim.image_index = to;
            }
        }

        // 6. Current HDR image states
        if let Some(ref mut curr) = self.current_hdr_image {
            if curr.index == from {
                curr.index = to;
            }
        }
        if let Some(ref mut curr) = self.current_hdr_tiled_image {
            if curr.index == from {
                curr.index = to;
            }
        }
        if let Some(ref mut curr) = self.current_hdr_tiled_preview {
            if curr.index == from {
                curr.index = to;
            }
        }

        // 7. Tile manager index
        if let Some(ref mut manager) = self.tile_manager {
            if manager.image_index == from {
                manager.image_index = to;
            }
        }

        // 8. Global tile pixel cache
        crate::tile_cache::PIXEL_CACHE
            .lock()
            .relocate_image(from, to);
    }

    pub(super) fn clear_index_keyed_state_after_list_reorder_except_index(
        &mut self,
        except_idx: usize,
    ) {
        self.generation = self.generation.wrapping_add(1);
        self.loader.set_generation(self.generation);
        self.loader.cancel_all();

        // 1. Texture cache: remove everything except except_idx
        let to_remove_tex: Vec<usize> = self
            .texture_cache
            .textures
            .keys()
            .copied()
            .filter(|&idx| idx != except_idx)
            .collect();
        for idx in to_remove_tex {
            self.texture_cache.remove(idx);
        }

        // 2. HDR caches
        let to_remove_hdr: Vec<usize> = self
            .hdr_image_cache
            .keys()
            .copied()
            .filter(|&idx| idx != except_idx)
            .collect();
        for idx in to_remove_hdr {
            self.hdr_image_cache.remove(&idx);
        }

        let to_remove_tiled_source: Vec<usize> = self
            .hdr_tiled_source_cache
            .keys()
            .copied()
            .filter(|&idx| idx != except_idx)
            .collect();
        for idx in to_remove_tiled_source {
            self.hdr_tiled_source_cache.remove(&idx);
        }

        let to_remove_tiled_preview: Vec<usize> = self
            .hdr_tiled_preview_cache
            .keys()
            .copied()
            .filter(|&idx| idx != except_idx)
            .collect();
        for idx in to_remove_tiled_preview {
            self.hdr_tiled_preview_cache.remove(&idx);
        }

        self.hdr_sdr_fallback_indices
            .retain(|&idx| idx == except_idx);
        self.hdr_placeholder_fallback_indices
            .retain(|&idx| idx == except_idx);
        self.hdr_in_flight_fallback_refinements
            .retain(|&idx| idx == except_idx);
        self.deferred_sdr_uploads
            .retain(|&idx, _| idx == except_idx);
        self.ultra_hdr_capacity_sensitive_indices
            .retain(|&idx| idx == except_idx);

        // 3. Prefetched tiles, animation cache
        self.prefetched_tiles.retain(|&idx, _| idx == except_idx);
        self.animation_cache.retain(|&idx, _| idx == except_idx);

        // 4. Other states
        if let Some(ref anim) = self.animation {
            if anim.image_index != except_idx {
                self.animation = None;
            }
        }
        self.pending_anim_frames = None;

        // Keep self.tile_manager if its index matches except_idx
        if let Some(ref manager) = self.tile_manager {
            if manager.image_index != except_idx {
                self.tile_manager = None;
            }
        }

        self.prev_texture = None;
        self.prev_hdr_image = None;
        self.transition_start = None;
        self.pending_transition_target = None;
        self.prefetch_prev_generation = None;

        // Clear only non-except_idx entries from the global tile pixel cache
        crate::tile_cache::PIXEL_CACHE
            .lock()
            .remove_images_except(except_idx);
    }

    pub(super) fn handle_texture_cache_eviction(&mut self, evicted_idx: usize) {
        self.animation_cache.remove(&evicted_idx);
        self.remove_hdr_image_index(evicted_idx);
    }

    pub(super) fn clear_preloaded_assets_for_capacity_change(&mut self) {
        let current = self.current_index;
        let mut indices = std::collections::BTreeSet::new();
        indices.extend(self.texture_cache.textures.keys().copied());
        indices.extend(self.prefetched_tiles.keys().copied());
        indices.extend(self.hdr_image_cache.keys().copied());
        indices.extend(self.hdr_tiled_source_cache.keys().copied());
        indices.extend(self.hdr_tiled_preview_cache.keys().copied());
        indices.extend(self.deferred_sdr_uploads.keys().copied());
        indices.extend(self.animation_cache.keys().copied());
        indices.extend(self.hdr_sdr_fallback_indices.iter().copied());
        indices.extend(self.hdr_placeholder_fallback_indices.iter().copied());
        indices.extend(self.ultra_hdr_capacity_sensitive_indices.iter().copied());

        let pixel_cache_indices: std::collections::HashSet<usize> = indices
            .iter()
            .copied()
            .filter(|&idx| idx != current)
            .collect();
        crate::tile_cache::PIXEL_CACHE
            .lock()
            .remove_images(&pixel_cache_indices);

        for idx in indices {
            if idx == current {
                continue;
            }
            self.texture_cache.remove(idx);
            self.prefetched_tiles.remove(&idx);
            self.animation_cache.remove(&idx);
            self.deferred_sdr_uploads.remove(&idx);
            self.remove_hdr_image_index(idx);
        }
    }

    pub(super) fn evict_distant_prefetch_caches(&mut self) {
        let len = self.image_files.len();
        let within_window = |idx: usize| {
            prefetch_window_contains(self.current_index, len, idx, Self::PREFETCH_WINDOW_DISTANCE)
        };

        // Track distant indices from prefetched_tiles eviction so we can clean their textures & metadata too
        let mut distant_indices = Vec::new();

        self.prefetched_tiles.retain(|&idx, _| {
            let keep = within_window(idx);
            if !keep {
                distant_indices.push(idx);
            }
            keep
        });

        self.deferred_sdr_uploads
            .retain(|&idx, _| within_window(idx));

        // Gather distant static HDR images
        let distant_hdr: Vec<usize> = self
            .hdr_image_cache
            .keys()
            .copied()
            .filter(|&idx| !within_window(idx))
            .collect();
        distant_indices.extend(distant_hdr);

        // Gather distant tiled HDR image sources. This ensures tiled HDR sources (like gain-map JPEGs)
        // are correctly evicted and do not leak in hdr_tiled_source_cache, which would cause
        // subsequent visits to trigger has_loaded_asset() but fail to construct the TileManager,
        // hanging the UI on loading.
        let distant_tiled_hdr: Vec<usize> = self
            .hdr_tiled_source_cache
            .keys()
            .copied()
            .filter(|&idx| !within_window(idx))
            .collect();
        distant_indices.extend(distant_tiled_hdr);

        // Gather distant uploaded SDR/static preview textures as well. These can be
        // produced by background preload without a matching TileManager/HDR cache entry,
        // so relying only on prefetched_tiles/HDR cleanup leaves stale GPU textures alive
        // until the texture cache reaches its capacity.
        let distant_textures: Vec<usize> = self
            .texture_cache
            .textures
            .keys()
            .copied()
            .filter(|&idx| !within_window(idx))
            .collect();
        distant_indices.extend(distant_textures);

        let distant_animations: Vec<usize> = self
            .animation_cache
            .keys()
            .copied()
            .filter(|&idx| !within_window(idx))
            .collect();
        distant_indices.extend(distant_animations);

        // Deduplicate the combined list of indices to evict
        distant_indices.sort_unstable();
        distant_indices.dedup();

        for idx in distant_indices {
            self.texture_cache.remove(idx);
            self.animation_cache.remove(&idx);
            self.remove_hdr_image_index(idx);
        }
    }
}
