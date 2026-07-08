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
    pub(crate) fn maybe_prefetch_startup_raw_open(&self) {
        if self.preload_deferred_for_hdr_capacity || self.image_files.is_empty() {
            return;
        }
        let path = &self.image_files[self.current_index];
        if !crate::loader::should_prefetch_raw_gpu_open(
            &self.settings,
            path,
            self.gpu_demosaic_failed_indices
                .contains(&self.current_index),
        ) {
            return;
        }
        self.loader.prefetch_raw_open(path.clone());
    }

    pub(crate) fn schedule_preloads(&mut self, forward: bool) {
        self.schedule_preloads_with_options(forward, false);
    }

    /// Load only the current image while a directory scan is still running.
    /// Neighbor preloads are deferred until the scan finishes so disk IO does not
    /// stall enumeration on large folders.
    pub(crate) fn schedule_current_image_load_if_needed(&mut self) {
        let n = self.image_files.len();
        if n == 0 {
            return;
        }
        if self.preload_deferred_for_hdr_capacity {
            return;
        }
        self.sync_loader_preload_plan();

        let cur = self.current_index.min(n.saturating_sub(1));
        let current_has_asset = self.has_loaded_asset(cur);
        let current_is_loading = self.loader.is_loading(cur)
            || self.strip_full_decode_inflight_should_block_main_load(cur);
        let current_missing_hdr_plane = raw_hq_navigate_missing_hdr_plane(
            &self.image_files,
            cur,
            self.settings.raw_high_quality,
            &self.hdr_image_cache,
            &self.hdr_tiled_source_cache,
        );
        if (current_has_asset && !current_missing_hdr_plane) || current_is_loading {
            return;
        }
        if self.main_loader_failed_indices.contains(&cur) {
            return;
        }

        let path = self.image_files[cur].clone();
        self.loader.request_load(
            cur,
            path,
            self.settings.raw_high_quality,
            self.raw_demosaic_mode_for_index(cur),
        );
    }

    /// Rebuild the ring preload queue around `current_index` after a cache-preserving list
    /// reorder (directory-tree column sort).
    pub(crate) fn reschedule_preloads_after_image_list_reorder(&mut self) {
        self.sync_loader_preload_plan();
        self.evict_distant_prefetch_caches();
        self.cancel_outside_prefetch_window_loader_tasks();
        self.schedule_preloads(true);
        self.discard_stale_loader_outputs();
    }

    pub(crate) fn schedule_preloads_with_options(&mut self, forward: bool, force_neighbors: bool) {
        let n = self.image_files.len();
        if n == 0 {
            preload_debug!("[PreloadDebug] schedule skipped: no images");
            return;
        }
        if self.preload_deferred_for_hdr_capacity {
            #[cfg(feature = "preload-debug")]
            {
                let can_release = self.startup_preload_defer_can_release_now();
                self.debug_log_preload_defer_gate(can_release);
            }
            preload_debug!(
                "[PreloadDebug] schedule deferred: waiting for runtime HDR capacity refresh"
            );
            return;
        }
        self.sync_loader_preload_plan();
        let cur = self.current_index;
        crate::preload_debug_throttled!(
            &format!(
                "preload:schedule_start:{cur}:{forward}:{}",
                self.settings.preload
            ),
            crate::preload_debug::PRELOAD_DEBUG_THROTTLE_INTERVAL,
            "[PreloadDebug] schedule start: cur={} forward={} preload_enabled={}",
            cur,
            forward,
            self.settings.preload
        );

        // Always load the current image unless any renderable representation is already cached.
        // HDR tiled images often have no SDR texture_cache entry, so checking only texture_cache
        // would re-submit expensive EXR preview generation after the initial load is processed.
        let current_has_asset = self.has_loaded_asset(cur);
        let current_strip_full_decode_inflight =
            self.strip_full_decode_inflight_should_block_main_load(cur);
        let mut current_is_loading =
            self.loader.is_loading(cur) || current_strip_full_decode_inflight;
        crate::preload_debug_throttled!(
            &format!(
                "preload:current_state:{cur}:{current_has_asset}:{current_is_loading}:{current_strip_full_decode_inflight}"
            ),
            crate::preload_debug::PRELOAD_DEBUG_THROTTLE_INTERVAL,
            "[PreloadDebug] current state: idx={} has_asset={} is_loading={} strip_full_decode_inflight={}",
            cur,
            current_has_asset,
            current_is_loading,
            current_strip_full_decode_inflight
        );
        let current_missing_hdr_plane = raw_hq_navigate_missing_hdr_plane(
            &self.image_files,
            cur,
            self.settings.raw_high_quality,
            &self.hdr_image_cache,
            &self.hdr_tiled_source_cache,
        );
        if !current_has_asset
            && !current_is_loading
            && !self.main_loader_failed_indices.contains(&cur)
            || current_missing_hdr_plane && !current_is_loading
        {
            if current_missing_hdr_plane && current_has_asset {
                preload_debug!(
                    "[PreloadDebug][RAW] request current reload: idx={} reason=missing_hdr_plane",
                    cur,
                );
            }
            let path = self.image_files[cur].clone();
            preload_debug!(
                "[PreloadDebug] request current: idx={} path={}",
                cur,
                path.display()
            );
            self.loader.request_load(
                cur,
                path,
                self.settings.raw_high_quality,
                self.raw_demosaic_mode_for_index(cur),
            );
            current_is_loading = true;
        }

        if should_defer_neighbor_work_for_current_main(current_has_asset, current_is_loading) {
            preload_debug!(
                "[PreloadDebug] defer background preload: cur={} reason=current_main_in_flight loading={} has_asset={}",
                cur,
                current_is_loading,
                current_has_asset
            );
            return;
        }

        crate::preload_debug_throttled!(
            &format!("preload:neighbor_allowed:{cur}:{current_has_asset}:{current_is_loading}"),
            crate::preload_debug::PRELOAD_DEBUG_THROTTLE_INTERVAL,
            "[PreloadDebug] neighbor preload allowed: cur={} has_asset={} loading={}",
            cur,
            current_has_asset,
            current_is_loading
        );

        if !self.settings.preload {
            preload_debug!("[PreloadDebug] background preload disabled in settings");
            return;
        }

        let path_is_raw = self
            .image_files
            .get(cur)
            .is_some_and(|p| crate::preload_debug::path_is_raw(p));
        let gpu_demosaic_pending = self.hdr_raw_gpu_demosaic_pending_indices.contains(&cur);
        let current_raw_gpu_path_active = should_defer_background_preload_for_raw_gpu_current(
            self.raw_hq_index_requires_hdr_plane(cur),
            path_is_raw,
            current_is_loading,
            gpu_demosaic_pending,
            self.raw_gpu_demosaic_await_hdr_present,
        );
        // Always hold neighbor preloads while the current HQ RAW GPU path is active.
        // `force_neighbors` bypasses defer after capacity retain when extract/GPU demosaic finished
        // but `await_hdr_present` may still be true until the first HDR frame is drawn.
        if current_raw_gpu_path_active
            && !(force_neighbors && !current_is_loading && !gpu_demosaic_pending)
        {
            preload_debug!(
                "[PreloadDebug] defer background preload: cur={} reason=raw_gpu_current loading={} gpu_pending={} await_hdr={}",
                cur,
                current_is_loading,
                gpu_demosaic_pending,
                self.raw_gpu_demosaic_await_hdr_present
            );
            return;
        }

        let available_memory_mb = self.cached_available_memory_mb;
        let total_memory_mb = self.cached_total_memory_mb;
        let memory_guard_threshold_mb =
            background_preload_memory_guard_threshold_mb(total_memory_mb);
        if should_skip_background_preloads_for_memory(available_memory_mb, total_memory_mb) {
            preload_debug!(
                "[PreloadDebug] memory guard: skip background preloads available_mb={} threshold_mb={} total_mb={}",
                available_memory_mb,
                memory_guard_threshold_mb,
                total_memory_mb
            );
            log::warn!(
                "[Preload] Skipping background preloads because available memory is low: {} MB available below {} MB reserve (total {} MB)",
                available_memory_mb,
                memory_guard_threshold_mb,
                total_memory_mb
            );
            self.clear_preloaded_assets_for_capacity_change();
            return;
        }
        crate::preload_debug_throttled!(
            &format!("preload:memory_allow:{memory_guard_threshold_mb}:{total_memory_mb}"),
            crate::preload_debug::PRELOAD_DEBUG_THROTTLE_INTERVAL,
            "[PreloadDebug] memory guard: allow background preloads available_mb={} threshold_mb={} total_mb={}",
            available_memory_mb,
            memory_guard_threshold_mb,
            total_memory_mb
        );

        // Schedule only indices inside the effective retention window so decode/GPU work
        // is not discarded by `evict_distant_prefetch_caches` on the next navigation.
        let window = self.prefetch_window_max_distance;
        let (primary_max, primary_budget, secondary_max, secondary_budget) = if forward {
            (
                window,
                self.preload_budget_forward,
                window,
                self.preload_budget_backward,
            )
        } else {
            (
                window,
                self.preload_budget_backward,
                window,
                self.preload_budget_forward,
            )
        };

        let primary_indices =
            prefetch_retention::prefetch_window_neighbors_in_direction(cur, n, window, forward);
        let secondary_indices =
            prefetch_retention::prefetch_window_neighbors_in_direction(cur, n, window, !forward);

        crate::preload_debug_throttled!(
            "preload:direction_budgets",
            crate::preload_debug::PRELOAD_DEBUG_THROTTLE_INTERVAL,
            "[PreloadDebug] direction budgets: primary_max={} primary_budget={} secondary_max={} secondary_budget={}",
            primary_max,
            primary_budget,
            secondary_max,
            secondary_budget
        );

        self.preload_direction("primary", primary_indices, primary_max, primary_budget);
        self.preload_direction(
            "secondary",
            secondary_indices,
            secondary_max,
            secondary_budget,
        );
    }

    /// Preload images from a list of candidate indices, respecting count and byte limits.
    /// Rule 1: Background preload candidates must fit the decoded-byte budget.
    /// Rule 2: Stop if count >= max_count OR cumulative NEW file size >= budget.
    /// Tiled-candidate images are skipped entirely (they use on-demand tile loading).
    /// Already-cached images occupy a count slot (preventing over-reach) but
    /// do NOT consume byte budget (no new memory allocation occurs).
    pub(crate) fn preload_direction(
        &mut self,
        #[cfg_attr(not(feature = "preload-debug"), allow(unused_variables))] direction_name: &str,
        candidates: Vec<usize>,
        max_count: usize,
        budget: u64,
    ) {
        let mut count = 0usize;
        let mut new_bytes = 0u64;
        crate::preload_debug_throttled!(
            &format!("preload:direction_start:{direction_name}:{max_count}:{budget}"),
            crate::preload_debug::PRELOAD_DEBUG_THROTTLE_INTERVAL,
            "[PreloadDebug] direction start: name={} max_count={} budget={} candidates={:?}",
            direction_name,
            max_count,
            budget,
            candidates
        );

        let mut in_flight = self.loader.in_flight_snapshot();

        for idx in candidates {
            if count >= max_count {
                preload_debug!(
                    "[PreloadDebug] direction stop: name={} reason=count_limit count={} max_count={} new_bytes={}",
                    direction_name,
                    count,
                    max_count,
                    new_bytes
                );
                break;
            }

            // Already cached or in-flight: occupies a slot but costs nothing new.
            let has_asset = self.has_loaded_asset(idx);
            let is_loading = in_flight.contains(&idx);
            let strip_full_decode_inflight = !has_asset
                && !is_loading
                && self.strip_full_decode_inflight_should_block_main_load(idx);
            if has_asset || is_loading || strip_full_decode_inflight {
                crate::preload_debug_throttled!(
                    &format!(
                        "preload:candidate_counted_existing:{direction_name}:{idx}:{has_asset}:{is_loading}:{strip_full_decode_inflight}"
                    ),
                    crate::preload_debug::PRELOAD_DEBUG_THROTTLE_INTERVAL,
                    "[PreloadDebug] candidate counted existing: name={} idx={} has_asset={} is_loading={} strip_full_decode_inflight={} count_before={}",
                    direction_name,
                    idx,
                    has_asset,
                    is_loading,
                    strip_full_decode_inflight,
                    count
                );
                count += 1;
                continue;
            }

            if idx != self.current_index && in_flight.len() >= MAX_CONCURRENT_DECODER_LOADS {
                preload_debug!(
                    "[PreloadDebug] direction stop: name={} reason=decoder_concurrency in_flight={} max={}",
                    direction_name,
                    in_flight.len(),
                    MAX_CONCURRENT_DECODER_LOADS
                );
                break;
            }

            let path = &self.image_files[idx];

            let file_size = self.file_byte_len_by_index.get(idx).copied().unwrap_or(0);

            // Sizes come from the scanner thread; unknown (0) skips the byte gate.
            // Compressed on-disk size understates decoded RGBA footprint (HEIC/JPEG often 10–20×).
            let decode_budget_bytes = estimate_preload_decode_bytes(file_size);
            preload_debug!(
                "[PreloadDebug] candidate evaluate: name={} idx={} file_size={} decode_budget={} used={} budget={} path={}",
                direction_name,
                idx,
                file_size,
                decode_budget_bytes,
                new_bytes,
                budget,
                path.display()
            );
            match decide_preload_for_budget(count, new_bytes, decode_budget_bytes, budget) {
                PreloadBudgetDecision::Request => {}
                PreloadBudgetDecision::SkipCandidate => {
                    if should_request_oversized_preload_candidate(
                        file_size,
                        decode_budget_bytes,
                        budget,
                    ) {
                        preload_debug!(
                            "[PreloadDebug] request oversized preload: name={} idx={} file_size={} decode_budget={} budget={} path={}",
                            direction_name,
                            idx,
                            file_size,
                            decode_budget_bytes,
                            budget,
                            path.display()
                        );
                    } else {
                        preload_debug!(
                            "[PreloadDebug] candidate skip: name={} idx={} reason=oversized_first decode_budget={} budget={} used={}",
                            direction_name,
                            idx,
                            decode_budget_bytes,
                            budget,
                            new_bytes
                        );
                        continue;
                    }
                }
                PreloadBudgetDecision::StopDirection => {
                    preload_debug!(
                        "[PreloadDebug] direction stop: name={} idx={} reason=budget_exhausted decode_budget={} budget={} used={} count={}",
                        direction_name,
                        idx,
                        decode_budget_bytes,
                        budget,
                        new_bytes,
                        count
                    );
                    break;
                }
            }

            preload_debug!(
                "[PreloadDebug] request preload: name={} idx={} file_size={} decode_budget={} used_before={} path={}",
                direction_name,
                idx,
                file_size,
                decode_budget_bytes,
                new_bytes,
                path.display()
            );
            self.loader.request_load(
                idx,
                path.clone(),
                self.settings.raw_high_quality,
                self.raw_demosaic_mode_for_index(idx),
            );
            in_flight.insert(idx);
            count += 1;
            let budget_charge = if decode_budget_bytes > budget {
                budget
            } else {
                decode_budget_bytes.max(file_size)
            };
            new_bytes = new_bytes.saturating_add(budget_charge);
        }
        crate::preload_debug_throttled!(
            &format!("preload:direction_done:{direction_name}:{count}:{new_bytes}"),
            crate::preload_debug::PRELOAD_DEBUG_THROTTLE_INTERVAL,
            "[PreloadDebug] direction done: name={} count={} new_bytes={}",
            direction_name,
            count,
            new_bytes
        );
    }

    pub(crate) fn has_loaded_asset(&self, index: usize) -> bool {
        let has_static_hdr = self.hdr_image_cache.contains_key(&index);
        let has_hdr_tiled_source = self.hdr_tiled_source_cache.contains_key(&index);
        let has_hdr_plane = has_static_hdr || has_hdr_tiled_source;
        if !hdr_fallback_asset_is_loaded(
            self.hdr_sdr_fallback_indices.contains(&index),
            has_hdr_plane,
        ) {
            return false;
        }
        let base_loaded = current_image_has_loaded_asset(
            self.texture_cache.contains(index),
            has_static_hdr,
            has_hdr_tiled_source,
            self.animation_cache.contains_key(&index),
        ) || self.deferred_sdr_uploads.contains_key(&index);
        if !base_loaded {
            return false;
        }
        if self.raw_hq_index_requires_hdr_plane(index) && !has_hdr_plane {
            return false;
        }
        true
    }

    /// After the current image finishes installing, kick neighbor preloads when idle.
    pub(crate) fn maybe_schedule_neighbor_preloads_after_current_install(&mut self) {
        if !self.settings.preload
            || self.preload_deferred_for_hdr_capacity
            || self.scanning
            || self.image_files.is_empty()
        {
            return;
        }
        if self.transition_start.is_some() {
            return;
        }
        let cur = self.current_index;
        let current_has_asset = self.has_loaded_asset(cur);
        let current_is_loading = self.loader.is_loading(cur);
        if !current_has_asset
            || current_is_loading
            || should_defer_neighbor_work_for_current_main(current_has_asset, current_is_loading)
        {
            return;
        }
        self.schedule_preloads(true);
    }
}
