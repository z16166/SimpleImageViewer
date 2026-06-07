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
    pub(crate) fn schedule_preloads(&mut self, forward: bool) {
        let n = self.image_files.len();
        if n == 0 {
            preload_debug!("[PreloadDebug] schedule skipped: no images");
            return;
        }
        let cur = self.current_index;
        preload_debug!(
            "[PreloadDebug] schedule start: cur={} forward={} generation={} preload_enabled={}",
            cur,
            forward,
            self.generation,
            self.settings.preload
        );

        // Always load the current image unless any renderable representation is already cached.
        // HDR tiled images often have no SDR texture_cache entry, so checking only texture_cache
        // would re-submit expensive EXR preview generation after the initial load is processed.
        let current_has_asset = self.has_loaded_asset(cur);
        let current_is_loading = self.loader.is_loading(cur, self.generation);
        preload_debug!(
            "[PreloadDebug] current state: idx={} has_asset={} is_loading={}",
            cur,
            current_has_asset,
            current_is_loading
        );
        if !current_has_asset && !current_is_loading {
            let path = self.image_files[cur].clone();
            preload_debug!(
                "[PreloadDebug] request current: idx={} gen={} path={}",
                cur,
                self.generation,
                path.display()
            );
            self.loader
                .request_load(cur, self.generation, path, self.settings.raw_high_quality);
        }

        if !self.settings.preload {
            preload_debug!("[PreloadDebug] background preload disabled in settings");
            return;
        }

        let mut sys = sysinfo::System::new();
        sys.refresh_memory_specifics(sysinfo::MemoryRefreshKind::nothing().with_ram());
        let available_memory_mb = sys.available_memory() / (1024 * 1024);
        let total_memory_mb = sys.total_memory() / (1024 * 1024);
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
        preload_debug!(
            "[PreloadDebug] memory guard: allow background preloads available_mb={} threshold_mb={} total_mb={}",
            available_memory_mb,
            memory_guard_threshold_mb,
            total_memory_mb
        );

        // Determine the "primary" and "secondary" directions.
        // Primary gets the larger budget; secondary gets the smaller one.
        let (primary_max, primary_budget, secondary_max, secondary_budget) = if forward {
            (
                MAX_PRELOAD_FORWARD,
                self.preload_budget_forward,
                MAX_PRELOAD_BACKWARD,
                self.preload_budget_backward,
            )
        } else {
            (
                MAX_PRELOAD_BACKWARD,
                self.preload_budget_backward,
                MAX_PRELOAD_FORWARD,
                self.preload_budget_forward,
            )
        };

        // Collect indices for each direction
        let primary_indices: Vec<usize> = (1..=n.min(primary_max + 10)) // +10 headroom to skip tiled images
            .map(|i| {
                if forward {
                    (cur + i) % n
                } else {
                    (cur + n - i) % n
                }
            })
            .collect();

        let secondary_indices: Vec<usize> = (1..=n.min(secondary_max + 10))
            .map(|i| {
                if forward {
                    (cur + n - i) % n
                } else {
                    (cur + i) % n
                }
            })
            .collect();

        preload_debug!(
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
        preload_debug!(
            "[PreloadDebug] direction start: name={} max_count={} budget={} candidates={:?}",
            direction_name,
            max_count,
            budget,
            candidates
        );

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
            let is_loading = self.loader.is_loading(idx, self.generation);
            if has_asset || is_loading {
                preload_debug!(
                    "[PreloadDebug] candidate counted existing: name={} idx={} has_asset={} is_loading={} count_before={}",
                    direction_name,
                    idx,
                    has_asset,
                    is_loading,
                    count
                );
                count += 1;
                continue;
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
                            "[PreloadDebug] request oversized preload: name={} idx={} gen={} file_size={} decode_budget={} budget={} path={}",
                            direction_name,
                            idx,
                            self.generation,
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
                "[PreloadDebug] request preload: name={} idx={} gen={} file_size={} decode_budget={} used_before={} path={}",
                direction_name,
                idx,
                self.generation,
                file_size,
                decode_budget_bytes,
                new_bytes,
                path.display()
            );
            self.loader.request_load(
                idx,
                self.generation,
                path.clone(),
                self.settings.raw_high_quality,
            );
            count += 1;
            let budget_charge = if decode_budget_bytes > budget {
                budget
            } else {
                decode_budget_bytes.max(file_size)
            };
            new_bytes = new_bytes.saturating_add(budget_charge);
        }
        preload_debug!(
            "[PreloadDebug] direction done: name={} count={} new_bytes={}",
            direction_name,
            count,
            new_bytes
        );
    }

    pub(super) fn has_loaded_asset(&self, index: usize) -> bool {
        let has_static_hdr = self.hdr_image_cache.contains_key(&index);
        let has_hdr_tiled_source = self.hdr_tiled_source_cache.contains_key(&index);
        let has_hdr_plane = has_static_hdr || has_hdr_tiled_source;
        if !hdr_fallback_asset_is_loaded(
            self.hdr_sdr_fallback_indices.contains(&index),
            has_hdr_plane,
        ) {
            return false;
        }
        current_image_has_loaded_asset(
            self.texture_cache.contains(index),
            has_static_hdr,
            has_hdr_tiled_source,
            self.animation_cache.contains_key(&index),
        ) || self.deferred_sdr_uploads.contains_key(&index)
    }
}
