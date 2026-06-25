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

use crate::app::index_cache_permute::{permute_usize_hashmap, permute_usize_set};

impl ImageViewerApp {
    pub(crate) fn invalidate_random_slideshow_order(&mut self) {
        self.random_slideshow_order_ready = false;
    }

    pub(super) fn shuffle_current_image_list_preserving_pairs(&mut self) {
        let mut combined = image_file_entries_with_missing_tail(
            std::mem::take(&mut self.image_files),
            std::mem::take(&mut self.file_byte_len_by_index),
            std::mem::take(&mut self.file_modified_unix_by_index),
        );
        combined.shuffle(&mut rand::thread_rng());
        let mut paths = Vec::with_capacity(combined.len());
        let mut sizes = Vec::with_capacity(combined.len());
        let mut modified = Vec::with_capacity(combined.len());
        for (path, len, mtime) in combined {
            paths.push(path);
            sizes.push(len);
            modified.push(mtime);
        }
        self.image_files = paths;
        self.file_byte_len_by_index = sizes;
        self.file_modified_unix_by_index = modified;
    }

    pub(super) fn clear_index_keyed_state_after_list_reorder(&mut self) {
        self.invalidate_decode_profile_epoch();
        self.loader.cancel_all();
        self.texture_cache.clear_all();
        self.clear_hdr_image_state();
        self.prefetched_tiles.clear();
        self.animation = None;
        self.animation_cache.clear();
        self.pending_anim_frames.clear();
        self.installed_display_modes.clear();
        self.tile_manager = None;
        self.set_current_image_resolution(None);
        self.raw_metadata.clear();
        self.prev_texture = None;
        self.prev_hdr_image = None;
        self.prev_transition_rect = None;
        self.transition_start = None;
        crate::tile_cache::PIXEL_CACHE.lock().clear();
        self.discard_stale_loader_outputs();
    }

    /// Relocate index-keyed caches when the image list order changes.
    ///
    /// When `relocate_strip_cache` is `false`, strip thumbnails keep pre-refresh indices until
    /// scan Done remaps by path (`reorder_directory_tree_strip_after_image_list_change`).
    /// Pass `true` for ordinary index relocations outside F5 refresh.
    pub(super) fn relocate_index_keyed_cache(
        &mut self,
        from: usize,
        to: usize,
        relocate_strip_cache: bool,
    ) {
        if from == to {
            return;
        }
        // 1. Texture cache
        self.texture_cache.relocate(from, to);
        if relocate_strip_cache {
            self.directory_tree_strip_cache.relocate(from, to);
        }

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
        if self.hdr_raw_gpu_demosaic_pending_indices.remove(&from) {
            self.hdr_raw_gpu_demosaic_pending_indices.insert(to);
        }
        if self.raw_gpu_embedded_bootstrap_indices.remove(&from) {
            self.raw_gpu_embedded_bootstrap_indices.insert(to);
        }
        if self.gpu_demosaic_failed_indices.remove(&from) {
            self.gpu_demosaic_failed_indices.insert(to);
        }
        if self.hdr_in_flight_fallback_refinements.remove(&from) {
            self.hdr_in_flight_fallback_refinements.insert(to);
        }
        if self.cpu_raw_refinement_pending_indices.remove(&from) {
            self.cpu_raw_refinement_pending_indices.insert(to);
        }
        if self.hq_tiled_preview_pending_indices.remove(&from) {
            self.hq_tiled_preview_pending_indices.insert(to);
        }
        if let Some(mode) = self.installed_display_modes.remove(&from) {
            self.installed_display_modes.insert(to, mode);
        }
        if self.ultra_hdr_capacity_sensitive_indices.remove(&from) {
            self.ultra_hdr_capacity_sensitive_indices.insert(to);
        }

        // 4. Deferred uploads
        if let Some(upload) = self.deferred_sdr_uploads.remove(&from) {
            self.deferred_sdr_uploads.insert(to, upload);
        }
        self.raw_metadata.relocate_index(from, to);

        if self.hdr_raw_gpu_demosaic_pending_indices.contains(&to) {
            if let Some(hdr) = self.hdr_image_cache.get(&to) {
                let key = crate::hdr::renderer::HdrImageKey::from_image(hdr);
                self.hdr_raw_gpu_demosaic_pending_key_index.insert(key, to);
            }
        }
        self.hdr_raw_gpu_demosaic_pending_key_index
            .retain(|_, idx| *idx != from);

        // 5. Prefetched tiles / animations
        if let Some(mut tiles) = self.prefetched_tiles.remove(&from) {
            tiles.image_index = to;
            self.prefetched_tiles.insert(to, tiles);
        }
        if let Some(mut anim) = self.animation_cache.remove(&from) {
            anim.image_index = to;
            self.animation_cache.insert(to, anim);
        }
        if let Some(mut pending) = self.pending_anim_frames.remove(&from) {
            pending.image_index = to;
            self.pending_anim_frames.insert(to, pending);
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
        self.hdr_raw_gpu_demosaic_pending_indices
            .retain(|&idx| idx == except_idx);
        self.raw_gpu_embedded_bootstrap_indices
            .retain(|&idx| idx == except_idx);
        self.gpu_demosaic_failed_indices
            .retain(|&idx| idx == except_idx);
        self.hdr_raw_gpu_demosaic_pending_key_index
            .retain(|_, idx| *idx == except_idx);
        self.hdr_in_flight_fallback_refinements
            .retain(|&idx| idx == except_idx);
        self.cpu_raw_refinement_pending_indices
            .retain(|&idx| idx == except_idx);
        self.hq_tiled_preview_pending_indices
            .retain(|&idx| idx == except_idx);
        self.installed_display_modes
            .retain(|&idx, _| idx == except_idx);
        self.deferred_sdr_uploads
            .retain(|&idx, _| idx == except_idx);
        self.raw_metadata
            .retain_only_indices(|idx| idx == except_idx);
        self.ultra_hdr_capacity_sensitive_indices
            .retain(|&idx| idx == except_idx);

        // 3. Prefetched tiles, animation cache
        self.prefetched_tiles.retain(|&idx, _| idx == except_idx);
        self.animation_cache.retain(|&idx, _| idx == except_idx);
        self.pending_anim_frames.retain(|&idx, _| idx == except_idx);

        // 4. Other states
        if let Some(ref anim) = self.animation {
            if anim.image_index != except_idx {
                self.animation = None;
            }
        }

        // Keep self.tile_manager if its index matches except_idx
        if let Some(ref manager) = self.tile_manager {
            if manager.image_index != except_idx {
                self.tile_manager = None;
            }
        }

        self.prev_texture = None;
        self.prev_hdr_image = None;
        self.prev_transition_rect = None;
        self.transition_start = None;
        self.pending_transition_target = None;

        // Clear only non-except_idx entries from the global tile pixel cache
        crate::tile_cache::PIXEL_CACHE
            .lock()
            .remove_images_except(except_idx);
    }

    pub(super) fn handle_texture_cache_eviction(&mut self, evicted_idx: usize) {
        self.animation_cache.remove(&evicted_idx);
        self.pending_anim_frames.remove(&evicted_idx);
        self.clear_installed_display_mode(evicted_idx);
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
        indices.extend(self.pending_anim_frames.keys().copied());
        indices.extend(self.hdr_sdr_fallback_indices.iter().copied());
        indices.extend(self.hdr_placeholder_fallback_indices.iter().copied());
        indices.extend(self.hdr_raw_gpu_demosaic_pending_indices.iter().copied());
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
            self.pending_anim_frames.remove(&idx);
            self.deferred_sdr_uploads.remove(&idx);
            self.clear_installed_display_mode(idx);
            self.remove_hdr_image_index(idx);
        }
    }

    pub(super) fn evict_distant_prefetch_caches(&mut self) {
        let len = self.image_files.len();
        let current_index = self.current_index;
        self.preload_memory.refresh_if_stale();
        let max_distance = prefetch_retention::effective_prefetch_window_distance(
            self.preload_memory.available_memory_mb(),
            self.preload_memory.total_memory_mb(),
        );
        preload_debug!(
            "[PreloadDebug] prefetch eviction scan: cur={} len={} max_distance={} available_mb={}",
            current_index,
            len,
            max_distance,
            self.preload_memory.available_memory_mb()
        );

        let retention_for = |idx: usize| {
            prefetch_retention::prefetch_cache_retention(
                current_index,
                len,
                max_distance,
                idx,
                self.loader.is_loading(idx),
            )
        };

        // Track distant indices from prefetched_tiles eviction so we can clean their textures & metadata too
        let mut distant_indices = Vec::new();

        self.prefetched_tiles.retain(|&idx, _| {
            let decision = retention_for(idx);
            let keep = decision.should_retain();
            if !keep {
                preload_debug!(
                    "[PreloadDebug] prefetch eviction retain=false: kind=prefetched_tiles idx={} reason={}",
                    idx,
                    decision.log_label()
                );
                distant_indices.push(idx);
            }
            keep
        });

        self.deferred_sdr_uploads.retain(|&idx, _| {
            let decision = retention_for(idx);
            let keep = decision.should_retain();
            if !keep {
                preload_debug!(
                    "[PreloadDebug] prefetch eviction retain=false: kind=deferred_sdr idx={} reason={}",
                    idx,
                    decision.log_label()
                );
                distant_indices.push(idx);
            }
            keep
        });

        // Gather distant static HDR images
        let distant_hdr: Vec<usize> = self
            .hdr_image_cache
            .keys()
            .copied()
            .filter(|&idx| {
                let decision = retention_for(idx);
                if !decision.should_retain() {
                    preload_debug!(
                        "[PreloadDebug] prefetch eviction retain=false: kind=hdr_image idx={} reason={}",
                        idx,
                        decision.log_label()
                    );
                }
                !decision.should_retain()
            })
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
            .filter(|&idx| {
                let decision = retention_for(idx);
                if !decision.should_retain() {
                    preload_debug!(
                        "[PreloadDebug] prefetch eviction retain=false: kind=hdr_tiled_source idx={} reason={}",
                        idx,
                        decision.log_label()
                    );
                }
                !decision.should_retain()
            })
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
            .filter(|&idx| {
                let decision = retention_for(idx);
                if !decision.should_retain() {
                    preload_debug!(
                        "[PreloadDebug] prefetch eviction retain=false: kind=texture idx={} reason={}",
                        idx,
                        decision.log_label()
                    );
                }
                !decision.should_retain()
            })
            .collect();
        distant_indices.extend(distant_textures);

        let distant_animations: Vec<usize> = self
            .animation_cache
            .keys()
            .copied()
            .filter(|&idx| {
                let decision = retention_for(idx);
                if !decision.should_retain() {
                    preload_debug!(
                        "[PreloadDebug] prefetch eviction retain=false: kind=animation idx={} reason={}",
                        idx,
                        decision.log_label()
                    );
                }
                !decision.should_retain()
            })
            .collect();
        distant_indices.extend(distant_animations);

        let distant_pixel_cache: Vec<usize> = crate::tile_cache::PIXEL_CACHE
            .lock()
            .distinct_image_indices()
            .into_iter()
            .filter(|&idx| {
                let decision = retention_for(idx);
                if !decision.should_retain() {
                    preload_debug!(
                        "[PreloadDebug] prefetch eviction retain=false: kind=pixel_cache idx={} reason={}",
                        idx,
                        decision.log_label()
                    );
                }
                !decision.should_retain()
            })
            .collect();
        distant_indices.extend(distant_pixel_cache);

        // Deduplicate the combined list of indices to evict
        distant_indices.sort_unstable();
        distant_indices.dedup();

        #[cfg_attr(not(feature = "preload-debug"), allow(unused_variables))]
        let evicted_count = distant_indices.len();

        if !distant_indices.is_empty() {
            let pixel_cache_indices: std::collections::HashSet<usize> =
                distant_indices.iter().copied().collect();
            preload_debug!(
                "[PreloadDebug] prefetch eviction pixel_cache remove: indices={:?}",
                distant_indices
            );
            crate::tile_cache::PIXEL_CACHE
                .lock()
                .remove_images(&pixel_cache_indices);
        }

        for idx in distant_indices {
            self.texture_cache.remove(idx);
            self.animation_cache.remove(&idx);
            self.pending_anim_frames.remove(&idx);
            self.clear_installed_display_mode(idx);
            self.remove_hdr_image_index(idx);
        }

        preload_debug!(
            "[PreloadDebug] prefetch eviction done: evicted_count={}",
            evicted_count
        );
    }

    pub(crate) fn permute_image_file_arrays(&mut self, order: &[usize]) {
        let mut paths = Vec::with_capacity(order.len());
        let mut sizes = Vec::with_capacity(order.len());
        let mut modified = Vec::with_capacity(order.len());
        for &old_index in order {
            paths.push(self.image_files[old_index].clone());
            sizes.push(
                self.file_byte_len_by_index
                    .get(old_index)
                    .copied()
                    .unwrap_or(0),
            );
            modified.push(
                self.file_modified_unix_by_index
                    .get(old_index)
                    .copied()
                    .flatten(),
            );
        }
        self.image_files = paths;
        self.file_byte_len_by_index = sizes;
        self.file_modified_unix_by_index = modified;
    }

    pub(crate) fn permute_index_keyed_caches(&mut self, old_to_new: &[usize]) {
        self.loader.cancel_all();

        self.texture_cache.permute(old_to_new);
        self.directory_tree_strip_cache.permute(old_to_new);
        permute_usize_hashmap(&mut self.hdr_image_cache, old_to_new);
        permute_usize_hashmap(&mut self.hdr_tiled_source_cache, old_to_new);
        permute_usize_hashmap(&mut self.hdr_tiled_preview_cache, old_to_new);
        permute_usize_set(&mut self.hdr_sdr_fallback_indices, old_to_new);
        permute_usize_set(&mut self.hdr_placeholder_fallback_indices, old_to_new);
        permute_usize_set(&mut self.hdr_raw_gpu_demosaic_pending_indices, old_to_new);
        permute_usize_set(&mut self.hdr_raw_gpu_demosaic_baked_indices, old_to_new);
        permute_usize_set(&mut self.raw_gpu_embedded_bootstrap_indices, old_to_new);
        permute_usize_set(&mut self.gpu_demosaic_failed_indices, old_to_new);
        permute_usize_set(&mut self.hdr_in_flight_fallback_refinements, old_to_new);
        permute_usize_set(&mut self.cpu_raw_refinement_pending_indices, old_to_new);
        permute_usize_set(&mut self.hq_tiled_preview_pending_indices, old_to_new);
        permute_usize_hashmap(&mut self.installed_display_modes, old_to_new);
        permute_usize_set(&mut self.ultra_hdr_capacity_sensitive_indices, old_to_new);
        permute_usize_hashmap(&mut self.deferred_sdr_uploads, old_to_new);

        self.raw_metadata.permute_indices(old_to_new);

        for value in self.hdr_raw_gpu_demosaic_pending_key_index.values_mut() {
            if *value < old_to_new.len() {
                *value = old_to_new[*value];
            }
        }

        let prefetched = std::mem::take(&mut self.prefetched_tiles);
        for (old_idx, mut tiles) in prefetched {
            if old_idx < old_to_new.len() {
                let new_idx = old_to_new[old_idx];
                tiles.image_index = new_idx;
                self.prefetched_tiles.insert(new_idx, tiles);
            }
        }

        let animations = std::mem::take(&mut self.animation_cache);
        for (old_idx, mut anim) in animations {
            if old_idx < old_to_new.len() {
                let new_idx = old_to_new[old_idx];
                anim.image_index = new_idx;
                self.animation_cache.insert(new_idx, anim);
            }
        }

        let pending_frames = std::mem::take(&mut self.pending_anim_frames);
        for (old_idx, mut pending) in pending_frames {
            if old_idx < old_to_new.len() {
                let new_idx = old_to_new[old_idx];
                pending.image_index = new_idx;
                self.pending_anim_frames.insert(new_idx, pending);
            }
        }

        if let Some(ref mut anim) = self.animation {
            if anim.image_index < old_to_new.len() {
                anim.image_index = old_to_new[anim.image_index];
            }
        }

        if let Some(ref mut curr) = self.current_hdr_image {
            if curr.index < old_to_new.len() {
                curr.index = old_to_new[curr.index];
            }
        }
        if let Some(ref mut curr) = self.current_hdr_tiled_image {
            if curr.index < old_to_new.len() {
                curr.index = old_to_new[curr.index];
            }
        }
        if let Some(ref mut curr) = self.current_hdr_tiled_preview {
            if curr.index < old_to_new.len() {
                curr.index = old_to_new[curr.index];
            }
        }
        if let Some(ref mut manager) = self.tile_manager {
            if manager.image_index < old_to_new.len() {
                manager.image_index = old_to_new[manager.image_index];
            }
        }

        crate::tile_cache::PIXEL_CACHE
            .lock()
            .permute_images(old_to_new);

        if self.current_index < old_to_new.len() {
            self.current_index = old_to_new[self.current_index];
            self.image_status.set_current_index(self.current_index);
            self.raw_metadata.set_current_index(self.current_index);
        }

        permute_usize_set(&mut self.directory_tree_strip_tiled_attempted, old_to_new);
        permute_usize_set(&mut self.directory_tree_strip_cold_attempted, old_to_new);
        permute_usize_set(&mut self.directory_tree_strip_generate_inflight, old_to_new);
        self.invalidate_random_slideshow_order();
    }
}
