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
    pub(crate) fn clear_hdr_image_state(&mut self) {
        self.hdr_image_cache.clear();
        self.hdr_tiled_source_cache.clear();
        self.hdr_tiled_preview_cache.clear();
        self.hdr_sdr_fallback_indices.clear();
        self.hdr_placeholder_fallback_indices.clear();
        self.hdr_raw_gpu_demosaic_pending_indices.clear();
        self.hdr_raw_gpu_demosaic_pending_key_index.clear();
        self.gpu_demosaic_failed_indices.clear();
        self.raw_gpu_demosaic_await_hdr_present = false;
        self.raw_demosaic_baked_notify.lock().clear();
        // Clears all per-index RAW OSD rows (directory switch / full list reorder).
        self.raw_metadata.clear();
        self.hdr_in_flight_fallback_refinements.clear();
        self.deferred_sdr_uploads.clear();
        self.ultra_hdr_capacity_sensitive_indices.clear();
        self.current_hdr_image = None;
        self.current_hdr_tiled_image = None;
        self.current_hdr_tiled_preview = None;
    }

    pub(crate) fn remove_hdr_image_index(&mut self, index: usize) {
        self.remove_hdr_image_resources(index);
        self.gpu_demosaic_failed_indices.remove(&index);
    }

    /// Drop HDR GPU/tile caches for `index` while keeping RAW OSD metadata and failure flags.
    ///
    /// Also removes `hdr_raw_gpu_demosaic_pending_key_index` entries. If a distant prefetch
    /// eviction runs while GPU demosaic is still in flight, a late failure sentinel may no
    /// longer match any pending index (warn + drop) and will not insert into
    /// `gpu_demosaic_failed_indices`; revisiting that image retries GPU demosaic instead of
    /// forcing CPU. Directory rescan retains side maps until retain runs; prefetch eviction
    /// clears them here by design.
    pub(crate) fn remove_hdr_image_resources(&mut self, index: usize) {
        if let Some(hdr) = self.hdr_image_cache.get(&index) {
            let key = crate::hdr::renderer::HdrImageKey::from_image(hdr);
            self.hdr_raw_gpu_demosaic_pending_key_index.remove(&key);
        }
        self.hdr_image_cache.remove(&index);
        self.hdr_tiled_source_cache.remove(&index);
        self.hdr_tiled_preview_cache.remove(&index);
        self.hdr_sdr_fallback_indices.remove(&index);
        self.hdr_placeholder_fallback_indices.remove(&index);
        self.hdr_raw_gpu_demosaic_pending_indices.remove(&index);
        self.hdr_in_flight_fallback_refinements.remove(&index);
        self.deferred_sdr_uploads.remove(&index);
        self.ultra_hdr_capacity_sensitive_indices.remove(&index);
        if self
            .current_hdr_image
            .as_ref()
            .is_some_and(|current| current.image_for_index(index).is_some())
        {
            self.current_hdr_image = None;
        }
        if self
            .current_hdr_tiled_image
            .as_ref()
            .is_some_and(|current| current.source_for_index(index).is_some())
        {
            self.current_hdr_tiled_image = None;
        }
        if current_hdr_tiled_preview_matches_index(self.current_hdr_tiled_preview.as_ref(), index) {
            self.current_hdr_tiled_preview = None;
        }
    }

    /// First HDR still for `index` from static cache, completed animation cache, or in-flight
    /// deferred animation uploads.
    pub(crate) fn first_cached_hdr_still_for_index(
        &self,
        index: usize,
    ) -> Option<Arc<crate::hdr::types::HdrImageBuffer>> {
        first_cached_hdr_still_for_index(
            &self.hdr_image_cache,
            &self.animation_cache,
            &self.pending_anim_frames,
            index,
        )
    }

    /// Returns the best available HDR still for `index`, falling back to the tiled
    /// downsampled preview or in-memory current preview when no full-resolution static HDR entry exists.
    ///
    /// Used at navigation time to populate `prev_hdr_image` so that tiled HDR images
    /// can serve as the background during image transitions.
    pub(crate) fn first_cached_hdr_or_tiled_preview_for_index(
        &self,
        index: usize,
    ) -> Option<Arc<crate::hdr::types::HdrImageBuffer>> {
        first_cached_hdr_or_tiled_preview_for_index(
            &self.hdr_image_cache,
            &self.animation_cache,
            &self.pending_anim_frames,
            &self.hdr_tiled_preview_cache,
            self.current_hdr_tiled_preview.as_ref(),
            index,
        )
    }

    pub(crate) fn effective_ultra_hdr_decode_capacity(&self) -> f32 {
        crate::app::ultra_hdr_decode_capacity_for_output_mode(
            self.effective_hdr_tone_map_settings(),
            self.hdr_capabilities.output_mode,
            self.effective_hdr_monitor_selection().as_ref(),
        )
    }

    pub(crate) fn sync_hdr_tone_map_settings(&mut self) {
        let tone = self.effective_hdr_tone_map_settings();
        self.hdr_renderer.tone_map = tone;
        self.loader.set_hdr_tone_map_settings(tone);
    }

    fn flush_deferred_preload_after_hdr_capacity(&mut self) {
        if !self.preload_deferred_for_hdr_capacity {
            return;
        }
        let selection = self.effective_hdr_monitor_selection();
        let monitor_hdr_supported = selection.as_ref().is_some_and(|selection| selection.hdr_supported);
        if !super::startup_preload_defer_can_release(
            self.hdr_monitor_state.runtime_probe_completed(),
            monitor_hdr_supported,
            self.hdr_capabilities.output_mode,
        ) {
            return;
        }
        self.preload_deferred_for_hdr_capacity = false;
        if self.image_files.is_empty() {
            return;
        }
        preload_debug!(
            "[PreloadDebug] schedule after HDR capacity refresh: cur={} gen={}",
            self.current_index,
            self.generation
        );
        self.maybe_prefetch_startup_raw_open();
        self.schedule_preloads(true);
    }

    pub(crate) fn refresh_ultra_hdr_decode_capacity(&mut self, ctx: &egui::Context) {
        const CAPACITY_EPSILON: f32 = 0.001;
        let next_output_mode = self.hdr_capabilities.output_mode;
        let next_capacity = self.effective_ultra_hdr_decode_capacity();
        let crosses_hdr_sdr_boundary = output_mode_crosses_hdr_sdr_boundary(
            self.ultra_hdr_decode_output_mode,
            next_output_mode,
        );
        if (next_capacity - self.ultra_hdr_decode_capacity).abs() <= CAPACITY_EPSILON
            && !crosses_hdr_sdr_boundary
        {
            let selection = self.effective_hdr_monitor_selection();
            let monitor_hdr_supported =
                selection.as_ref().is_some_and(|selection| selection.hdr_supported);
            let can_release = startup_preload_defer_can_release(
                self.hdr_monitor_state.runtime_probe_completed(),
                monitor_hdr_supported,
                next_output_mode,
            );
            if can_release {
                self.flush_deferred_preload_after_hdr_capacity();
            }
            return;
        }

        let previous_capacity = self.ultra_hdr_decode_capacity;
        let previous_output_mode = self.ultra_hdr_decode_output_mode;
        self.ultra_hdr_decode_capacity = next_capacity;
        self.ultra_hdr_decode_output_mode = next_output_mode;
        self.loader.set_hdr_target_capacity(next_capacity);
        self.loader
            .set_hdr_tone_map_settings(self.effective_hdr_tone_map_settings());
        log::info!(
            "[HDR] ultra_hdr_decode_capacity changed {:.3} -> {:.3}; output_mode {:?} -> {:?}",
            previous_capacity,
            next_capacity,
            previous_output_mode,
            next_output_mode
        );

        if crosses_hdr_sdr_boundary {
            if self.preload_deferred_for_hdr_capacity {
                log::info!(
                    "[HDR] initial HDR output-mode probe {:?} -> {:?}; skipping reload until first load",
                    previous_output_mode,
                    next_output_mode
                );
            } else {
                log::info!(
                    "[HDR] HDR/SDR output boundary changed; invalidating in-flight/preload state and reloading current image"
                );
                self.reload_current_after_hdr_sdr_output_boundary_change();
            }
            self.flush_deferred_preload_after_hdr_capacity();
            ctx.request_repaint();
            return;
        }

        self.invalidate_ultra_hdr_capacity_sensitive_state(ctx);
        self.flush_deferred_preload_after_hdr_capacity();
    }

    fn refresh_current_hdr_presentation_after_capacity_refine(&mut self) {
        let idx = self.current_index;
        if let Some(hdr) = self.hdr_image_cache.get(&idx).cloned() {
            self.set_current_image_resolution(Some((hdr.width, hdr.height)));
            self.current_hdr_image =
                Some(crate::app::CurrentHdrImage::new(idx, hdr));
            self.refresh_hdr_view_status();
        }
    }

    fn invalidate_ultra_hdr_capacity_sensitive_state(&mut self, ctx: &egui::Context) {
        let static_hdr_indices: std::collections::HashSet<_> =
            self.hdr_image_cache.keys().copied().collect();
        let hdr_tiled_indices: std::collections::HashSet<_> =
            self.hdr_tiled_source_cache.keys().copied().collect();
        let refresh = crate::app::plan_ultra_hdr_capacity_refresh(
            self.current_index,
            &static_hdr_indices,
            &hdr_tiled_indices,
            &self.hdr_sdr_fallback_indices,
            &self.ultra_hdr_capacity_sensitive_indices,
        );

        if refresh.indices_to_invalidate.is_empty() {
            log::info!(
                "[HDR] ultra_hdr_decode_capacity refined to {:.3}; no cached HDR planes yet — updating loader without cancel_all",
                self.ultra_hdr_decode_capacity
            );
            if !self.image_files.is_empty() {
                self.schedule_preloads_with_options(true, true);
            }
            ctx.request_repaint();
            return;
        }

        let mut raw_retain = Vec::new();
        let mut hard_invalidate = Vec::new();
        for idx in &refresh.indices_to_invalidate {
            if super::raw_hq_static_hdr_retainable_on_capacity_refine(
                &self.image_files,
                *idx,
                self.settings.raw_high_quality,
                &self.hdr_image_cache,
            ) {
                raw_retain.push(*idx);
            } else {
                hard_invalidate.push(*idx);
            }
        }

        for idx in &raw_retain {
            self.ultra_hdr_capacity_sensitive_indices.remove(idx);
        }

        if hard_invalidate.is_empty() {
            log::info!(
                "[HDR] ultra_hdr_decode_capacity refined to {:.3}; retaining {} HQ RAW HDR plane(s) — no cancel_all",
                self.ultra_hdr_decode_capacity,
                raw_retain.len()
            );
            if refresh.reload_current {
                self.refresh_current_hdr_presentation_after_capacity_refine();
            }
            if crate::app::capacity_refresh_should_reschedule_preloads(&refresh) {
                self.schedule_preloads_with_options(true, true);
            }
            ctx.request_repaint();
            return;
        }

        log::info!(
            "[HDR] ultra_hdr_decode_capacity refined to {:.3}; evicting {} non-RAW HDR cache(s), retaining {} HQ RAW HDR plane(s)",
            self.ultra_hdr_decode_capacity,
            hard_invalidate.len(),
            raw_retain.len()
        );

        self.loader.cancel_all();
        self.clear_preloaded_assets_for_capacity_change();

        for idx in &hard_invalidate {
            self.texture_cache.remove(*idx);
            self.prefetched_tiles.remove(idx);
            crate::tile_cache::PIXEL_CACHE.lock().remove_image(*idx);
            self.remove_hdr_image_index(*idx);
        }

        if refresh.reload_current && !self.image_files.is_empty() {
            if super::raw_hq_static_hdr_retainable_on_capacity_refine(
                &self.image_files,
                self.current_index,
                self.settings.raw_high_quality,
                &self.hdr_image_cache,
            ) {
                self.refresh_current_hdr_presentation_after_capacity_refine();
            } else {
                self.generation = self.generation.wrapping_add(1);
                self.loader.set_generation(self.generation);
                self.tile_manager = None;
                self.set_current_image_resolution(None);
                self.animation = None;
                self.loader.request_load(
                    self.current_index,
                    self.generation,
                    self.image_files[self.current_index].clone(),
                    self.settings.raw_high_quality,
                    self.raw_demosaic_mode_for_index(self.current_index),
                );
            }
        }

        if crate::app::capacity_refresh_should_reschedule_preloads(&refresh) {
            self.schedule_preloads_with_options(true, true);
        }
        ctx.request_repaint();
    }

    fn reload_current_after_hdr_sdr_output_boundary_change(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.loader.set_generation(self.generation);
        self.loader.cancel_all();
        self.clear_preloaded_assets_for_capacity_change();

        if self.image_files.is_empty() {
            return;
        }

        let idx = self.current_index;
        self.texture_cache.remove(idx);
        self.prefetched_tiles.remove(&idx);
        crate::tile_cache::PIXEL_CACHE.lock().remove_image(idx);
        self.remove_hdr_image_index(idx);
        self.tile_manager = None;
        self.set_current_image_resolution(None);
        self.animation = None;
        self.animation_cache.remove(&idx);
        self.pending_anim_frames.remove(&idx);
        self.prev_texture = None;
        self.prev_hdr_image = None;
        self.prev_transition_rect = None;
        self.transition_start = None;
        self.pending_transition_target = None;
        self.prefetch_prev_generation = None;
        self.loader.request_load(
            idx,
            self.generation,
            self.image_files[idx].clone(),
            self.settings.raw_high_quality,
            self.raw_demosaic_mode_for_index(idx),
        );
    }
}
