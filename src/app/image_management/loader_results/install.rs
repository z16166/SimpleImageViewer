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
    /// Handles a Refined notification: bumps generation so TileManager
    /// re-fetches tiles from the newly developed high-resolution buffer.
    pub(super) fn handle_refined_notification(
        &mut self,
        idx: usize,
        gen_id: u64,
        ctx: &egui::Context,
    ) {
        if idx == self.current_index && gen_id == self.generation {
            log::debug!("[App] Refined image notification for index={}", idx);

            crate::tile_cache::PIXEL_CACHE.lock().remove_image(idx);

            self.generation = self.generation.wrapping_add(1);
            self.loader.set_generation(self.generation);

            if let Some(tm) = &mut self.tile_manager {
                log::debug!("[App] Refined: Tiled mode — forcing tile upgrade to high definition");
                tm.generation = self.generation;
                tm.pending_tiles.clear();
                self.texture_cache.remove(idx);
                let preserve_hdr_tiled = self.hdr_tiled_source_cache.contains_key(&idx);
                if !preserve_hdr_tiled {
                    self.remove_hdr_image_index(idx);
                } else if idx == self.current_index {
                    if let Some(source) = self.hdr_tiled_source_cache.get(&idx).cloned() {
                        self.current_hdr_tiled_image =
                            Some(crate::app::CurrentHdrTiledImage::new(idx, source));
                    }
                }
            } else {
                log::warn!(
                    "[App] Refined: Static mode encountered unexpectedly. Attempting to reload."
                );
                self.texture_cache.remove(idx);
                self.remove_hdr_image_index(idx);
                self.loader.request_load(
                    self.current_index,
                    self.generation,
                    self.image_files[self.current_index].clone(),
                    self.settings.raw_high_quality,
                );
            }

            self.loader.flush_tile_queue();
            if should_request_repaint_for_asset_update(
                AssetUpdateKind::RefinedFullPlane,
                true,
                false,
            ) {
                ctx.request_repaint();
            }
        } else {
            // Non-current image refined in background OR stale refinement result.

            // CRITICAL: If it's the current index but the generation doesn't match,
            // it's a stale result from a previous visit. We MUST NOT evict the
            // CURRENT texture cache, otherwise the screen will flicker or go blank.
            if idx == self.current_index {
                log::debug!(
                    "[App] Refined: ignoring stale background update for current index {} (gen {} vs current {})",
                    idx,
                    gen_id,
                    self.generation
                );
                return;
            }

            log::debug!(
                "[App] Refined: background update for index {} (not current). Invalidating caches.",
                idx
            );
            crate::tile_cache::PIXEL_CACHE.lock().remove_image(idx);
            self.prefetched_tiles.remove(&idx);
            self.texture_cache.remove(idx);
            self.remove_hdr_image_index(idx);
        }
    }

    /// Returns `Some((idx, generation, path))` when the result was stale (wrong HDR capacity) and
    /// the caller must re-queue **after** calling `finish_image_request` to clear the loading-map
    /// slot.
    pub(crate) fn handle_image_load_result(
        &mut self,
        load_result: &LoadResult,
        install_plan: ImageInstallPlan<'_>,
        ctx: &egui::Context,
    ) -> Option<(usize, u64, std::path::PathBuf)> {
        let idx = load_result.index;
        let generation = load_result.generation;

        if hdr_load_result_capacity_is_stale(&load_result, self.ultra_hdr_decode_capacity) {
            log::info!(
                "[HDR] Stale-capacity result for index={}: decoded_capacity={:.3} != current={:.3}; will re-queue after slot is freed.",
                idx,
                load_result.target_hdr_capacity,
                self.ultra_hdr_decode_capacity
            );
            if !self.image_files.is_empty() && idx < self.image_files.len() {
                return Some((idx, generation, self.image_files[idx].clone()));
            }
            return None;
        }

        if let Some(osd) = &load_result.raw_osd {
            if osd.sensor_size.0 > 0 {
                self.set_raw_metadata_for_index(idx, Some(osd.clone()), ctx);
            }
        } else {
            self.set_raw_metadata_for_index(idx, None, ctx);
        }

        match install_plan {
            ImageInstallPlan::StaticSdr { decoded } => {
                self.install_static_sdr_image(idx, decoded, ctx);
            }
            ImageInstallPlan::StaticHdr {
                hdr,
                fallback,
                ultra_hdr_capacity_sensitive,
            } => {
                self.install_static_hdr_image(
                    idx,
                    hdr,
                    fallback,
                    load_result.sdr_fallback_is_placeholder,
                    ultra_hdr_capacity_sensitive,
                    ctx,
                );
            }
            ImageInstallPlan::Tiled {
                source,
                hdr_source,
                sdr_preview,
                hdr_preview,
                hdr_sdr_fallback,
                ultra_hdr_capacity_sensitive,
            } => {
                self.install_tiled_image(
                    idx,
                    generation,
                    source,
                    hdr_source,
                    sdr_preview,
                    hdr_preview,
                    hdr_sdr_fallback,
                    ultra_hdr_capacity_sensitive,
                    ctx,
                );
            }
            ImageInstallPlan::Animated { frames } => {
                self.install_animated_image(idx, frames, ctx);
            }
            ImageInstallPlan::HdrAnimated {
                frames,
                ultra_hdr_capacity_sensitive,
            } => {
                self.install_hdr_animated_image(idx, frames, ultra_hdr_capacity_sensitive, ctx);
            }
            ImageInstallPlan::Error { error } => {
                self.install_image_error(idx, error);
            }
        }
        None
    }
}
