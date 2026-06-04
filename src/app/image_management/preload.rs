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
            return;
        }
        let cur = self.current_index;

        // Always load the current image unless any renderable representation is already cached.
        // HDR tiled images often have no SDR texture_cache entry, so checking only texture_cache
        // would re-submit expensive EXR preview generation after the initial load is processed.
        if !self.has_loaded_asset(cur) && !self.loader.is_loading(cur, self.generation) {
            let path = self.image_files[cur].clone();
            self.loader
                .request_load(cur, self.generation, path, self.settings.raw_high_quality);
        }

        if !self.settings.preload {
            return;
        }

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

        self.preload_direction(primary_indices, primary_max, primary_budget);
        self.preload_direction(secondary_indices, secondary_max, secondary_budget);
    }

    /// Preload images from a list of candidate indices, respecting count and byte limits.
    /// Rule 1: Always preload at least 1 non-tiled image (guaranteed minimum).
    /// Rule 2: Stop if count >= max_count OR cumulative NEW file size >= budget.
    /// Tiled-candidate images are skipped entirely (they use on-demand tile loading).
    /// Already-cached images occupy a count slot (preventing over-reach) but
    /// do NOT consume byte budget (no new memory allocation occurs).
    pub(crate) fn preload_direction(
        &mut self,
        candidates: Vec<usize>,
        max_count: usize,
        budget: u64,
    ) {
        let mut count = 0usize;
        let mut new_bytes = 0u64;

        for idx in candidates {
            if count >= max_count {
                break;
            }

            // Already cached or in-flight: occupies a slot but costs nothing new.
            if self.has_loaded_asset(idx) || self.loader.is_loading(idx, self.generation) {
                count += 1;
                continue;
            }

            let path = &self.image_files[idx];

            let file_size = self.file_byte_len_by_index.get(idx).copied().unwrap_or(0);

            // After the guaranteed first image, enforce the byte budget.
            // Sizes come from the scanner thread; unknown (0) skips the byte gate.
            // Compressed on-disk size understates decoded RGBA footprint (HEIC/JPEG often 10–20×).
            let decode_budget_bytes = if file_size > 0 {
                file_size.saturating_mul(12)
            } else {
                0
            };
            if count > 0
                && decode_budget_bytes > 0
                && new_bytes.saturating_add(decode_budget_bytes) > budget
            {
                break;
            }

            self.loader.request_load(
                idx,
                self.generation,
                path.clone(),
                self.settings.raw_high_quality,
            );
            count += 1;
            new_bytes += decode_budget_bytes.max(file_size);
        }
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
