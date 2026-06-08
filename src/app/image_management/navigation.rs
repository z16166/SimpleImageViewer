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
    /// Current animation frame texture when the outgoing index is an animated image.
    fn transition_animation_texture_for_index(&self, index: usize) -> Option<egui::TextureHandle> {
        if let Some(animation) = self.animation.as_ref() {
            if animation.image_index == index {
                return animation.textures.get(animation.current_frame).cloned();
            }
        }
        self.animation_cache
            .get(&index)
            .and_then(|cached| cached.textures.first().cloned())
    }

    /// Resolve the outgoing transition source for `index`, uploading deferred CPU pixels first.
    pub(crate) fn capture_transition_source_at_index(
        &mut self,
        index: usize,
        ctx: &egui::Context,
    ) -> (
        Option<egui::TextureHandle>,
        Option<Arc<crate::hdr::types::HdrImageBuffer>>,
    ) {
        self.flush_deferred_sdr_upload_for_index(index, ctx);

        // Prefer the HDR buffer already on screen (`current_hdr_image`) so the outgoing
        // transition plane reuses the same GPU binding instead of re-uploading/re-composing.
        let hdr = if index == self.current_index {
            self.current_hdr_image
                .as_ref()
                .and_then(|current| current.image_for_index(index))
                .cloned()
        } else {
            None
        }
        .or_else(|| self.first_cached_hdr_or_tiled_preview_for_index(index));
        let placeholder = self.hdr_placeholder_fallback_indices.contains(&index);

        let mut texture = self
            .texture_cache
            .get(index)
            .cloned()
            .or_else(|| self.transition_animation_texture_for_index(index));

        // When the HDR float plane is available, avoid using the dim placeholder SDR fallback
        // as the outgoing transition source.
        if should_drop_placeholder_sdr_transition_source(
            placeholder,
            hdr.is_some(),
            self.effective_hdr_display_output().is_some(),
        ) {
            texture = None;
        }

        (texture, hdr)
    }

    fn transition_source_rect_for_index(
        &self,
        index: usize,
        texture: Option<&egui::TextureHandle>,
        hdr: Option<&Arc<crate::hdr::types::HdrImageBuffer>>,
        screen_rect: egui::Rect,
    ) -> Option<egui::Rect> {
        let source_size = if index == self.current_index {
            self.tile_manager
                .as_ref()
                .filter(|tm| tm.image_index == index)
                .map(|tm| Vec2::new(tm.full_width as f32, tm.full_height as f32))
        } else {
            None
        }
        .or_else(|| {
            self.texture_cache
                .get_original_res(index)
                .map(|(w, h)| Vec2::new(w as f32, h as f32))
        })
        .or_else(|| hdr.map(|image| Vec2::new(image.width as f32, image.height as f32)))
        .or_else(|| texture.map(|texture| texture.size_vec2()));

        source_size.map(|size| self.compute_plane_layout(size, screen_rect).dest)
    }

    pub(crate) fn reload_current(&mut self) {
        if self.image_files.is_empty() {
            return;
        }

        let has_any_raw = self.image_files.iter().any(|path| {
            path.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|ext| crate::raw_processor::is_raw_extension(ext))
        });
        if !has_any_raw {
            return;
        }

        self.invalidate_all_raw_image_caches();

        crate::preload_debug!(
            "[PreloadDebug][RAW] setting_reload raw_hq={} current_idx={} gen={}",
            self.settings.raw_high_quality,
            self.current_index,
            self.generation
        );

        let path = self.image_files[self.current_index].clone();
        self.loader.request_load(
            self.current_index,
            self.generation,
            path,
            self.settings.raw_high_quality,
        );

        // Re-schedule preloads so nearby RAW files pick up the new mode too.
        self.schedule_preloads(true);
    }

    /// Drop every cached/prefetched RAW decode so a [`Settings::raw_high_quality`] toggle cannot
    /// leave neighbor images stuck in the previous performance/HQ pipeline.
    fn invalidate_all_raw_image_caches(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.loader.set_generation(self.generation);
        self.loader.cancel_all();

        let raw_indices: Vec<usize> = self
            .image_files
            .iter()
            .enumerate()
            .filter_map(|(idx, path)| {
                path.extension()
                    .and_then(|e| e.to_str())
                    .filter(|ext| crate::raw_processor::is_raw_extension(ext))
                    .map(|_| idx)
            })
            .collect();

        let _raw_index_count = raw_indices.len();
        for idx in raw_indices {
            self.texture_cache.remove(idx);
            self.remove_hdr_image_index(idx);
            self.prefetched_tiles.remove(&idx);
            self.deferred_sdr_uploads.remove(&idx);
            crate::tile_cache::PIXEL_CACHE.lock().remove_image(idx);
        }

        self.tile_manager = None;
        self.current_image_res = None;
        self.animation = None;
        self.prev_transition_rect = None;
        self.prev_texture = None;
        self.prev_hdr_image = None;
        self.transition_start = None;
        self.pending_transition_target = None;
        self.prefetch_prev_generation = None;

        crate::preload_debug!(
            "[PreloadDebug][RAW] invalidate_all_raw_caches cleared {} raw indices gen={}",
            _raw_index_count,
            self.generation
        );
    }

    pub(crate) fn navigate_to(&mut self, new_index: usize, ctx: &egui::Context) {
        if self.refresh_scan_in_progress || self.image_files.is_empty() {
            return;
        }

        let previous_index = self.current_index;
        let target_index = new_index % self.image_files.len();
        if target_index == self.current_index {
            return;
        }
        self.transition_settled_at = None;
        self.transition_end_hold = false;
        self.last_background_upload_at = None;
        let preload_forward =
            navigation_is_forward(previous_index, target_index, self.image_files.len());

        let outgoing_index = self.current_index;
        let (source_tex, source_hdr) = self.capture_transition_source_at_index(outgoing_index, ctx);
        let source_rect = self.transition_source_rect_for_index(
            outgoing_index,
            source_tex.as_ref(),
            source_hdr.as_ref(),
            ctx.input(|i| i.content_rect()),
        );

        // Setup transition if enabled. We defer transition start until the target
        // texture is actually ready to draw, avoiding black/stale-frame flashes.
        if self.settings.transition_style != TransitionStyle::None {
            let now = Instant::now();
            if self.settings.transition_style == TransitionStyle::Random {
                // Pick a random style from the pool using rand for uniform distribution
                let pool = TransitionStyle::RANDOM_POOL;
                self.active_transition = *pool
                    .choose(&mut rand::thread_rng())
                    .unwrap_or(&TransitionStyle::Fade);
            } else {
                self.active_transition = self.settings.transition_style;
            }

            // Always overwrite transition source with the outgoing frame only. If the outgoing
            // index has no drawable source (decode failed, etc.), do not reuse stale handles from
            // an earlier navigation.
            self.prev_texture = source_tex;
            self.prev_hdr_image = source_hdr;
            self.prev_transition_rect = source_rect;
            // Handle wrap-around logic for direction
            self.is_next = transition_direction_is_next(
                self.current_index,
                target_index,
                self.image_files.len(),
            );
            self.transition_start = None;
            let source_has_texture = self.prev_texture.is_some() || self.prev_hdr_image.is_some();
            let target_has_texture = self.texture_cache.contains(target_index);
            let target_has_hdr_plane = self.hdr_image_cache.contains_key(&target_index)
                || self.hdr_tiled_source_cache.contains_key(&target_index);
            let target_placeholder_only = self
                .hdr_placeholder_fallback_indices
                .contains(&target_index);
            let target_render_ready = target_is_render_ready(
                target_has_texture,
                target_has_hdr_plane,
                target_placeholder_only,
            );
            if should_start_transition_immediately(target_render_ready, source_has_texture) {
                self.transition_start =
                    Some(now - transition_preroll_duration(self.settings.transition_ms));
                self.pending_transition_target = None;
            } else {
                self.pending_transition_target = Some(target_index);
            }

            if should_reset_transition_when_source_texture_missing(
                self.prev_texture.is_some() || self.prev_hdr_image.is_some(),
            ) {
                // No texture available for the source frame: avoid reusing stale
                // transition state from previous navigation.
                self.prev_texture = None;
                self.prev_hdr_image = None;
                self.prev_transition_rect = None;
                self.pending_transition_target = None;
                self.transition_start = None;
            }
        } else {
            let source_has_texture = source_tex.is_some() || source_hdr.is_some();
            let target_has_texture = self.texture_cache.contains(target_index);
            let target_has_hdr_plane = self.hdr_image_cache.contains_key(&target_index)
                || self.hdr_tiled_source_cache.contains_key(&target_index);
            let target_placeholder_only = self
                .hdr_placeholder_fallback_indices
                .contains(&target_index);
            self.active_transition = TransitionStyle::None;
            self.transition_start = None;
            self.prev_texture = source_tex;
            self.prev_hdr_image = source_hdr;
            self.prev_transition_rect = source_rect;
            self.pending_transition_target = if !target_is_render_ready(
                target_has_texture,
                target_has_hdr_plane,
                target_placeholder_only,
            ) && source_has_texture
            {
                Some(target_index)
            } else {
                None
            };
            if should_reset_transition_when_source_texture_missing(
                self.prev_texture.is_some() || self.prev_hdr_image.is_some(),
            ) {
                self.prev_texture = None;
                self.prev_hdr_image = None;
                self.prev_transition_rect = None;
                self.pending_transition_target = None;
                self.transition_start = None;
            }
        }

        preserve_current_tile_manager_for_navigation(
            self.current_index,
            target_index,
            &mut self.tile_manager,
            &mut self.prefetched_tiles,
        );
        self.current_index = target_index;
        self.refresh_current_osd_file_name();
        self.current_hdr_image = self
            .first_cached_hdr_still_for_index(self.current_index)
            .map(|image| crate::app::CurrentHdrImage::new(self.current_index, image));
        self.current_hdr_tiled_image = self
            .hdr_tiled_source_cache
            .get(&self.current_index)
            .cloned()
            .map(|source| crate::app::CurrentHdrTiledImage::new(self.current_index, source));
        self.current_hdr_tiled_preview = self
            .hdr_tiled_preview_cache
            .get(&self.current_index)
            .cloned()
            .map(|image| crate::app::CurrentHdrImage::new(self.current_index, image));
        self.current_rotation = 0;
        self.zoom_factor = 1.0;
        self.pan_offset = Vec2::ZERO;
        self.animation = None;

        // Update resolution if already in cache (for immediate low-res display)
        if self.texture_cache.contains(self.current_index) {
            if let Some((w, h)) = self.texture_cache.get_original_res(self.current_index) {
                self.current_image_res = Some((w, h));
            } else if let Some(texture) = self.texture_cache.get(self.current_index) {
                let size = texture.size();
                self.current_image_res = Some((size[0] as u32, size[1] as u32));
            }
        } else {
            self.current_image_res = None;
        }

        self.last_switch_time = Instant::now();
        self.error_message = None;
        self.is_font_error = false;
        ctx.request_repaint();
        self.invalidate_osd();
        self.reset_osd_image_cache();
        // Close any open EXIF/XMP modal — it shows data for the previous image
        if matches!(
            self.active_modal,
            Some(crate::ui::dialogs::modal_state::ActiveModal::Exif(_))
                | Some(crate::ui::dialogs::modal_state::ActiveModal::Xmp(_))
        ) {
            self.active_modal = None;
        }

        // Try to pull from predictive cache if available
        if let Some(cached_anim) = self.animation_cache.get(&self.current_index) {
            if let Some(hdr_frames) = &cached_anim.hdr_frames {
                if let Some(hdr) = hdr_frames.first() {
                    self.current_hdr_image = Some(crate::app::CurrentHdrImage::new(
                        self.current_index,
                        Arc::clone(hdr),
                    ));
                }
            }
            self.animation = Some(AnimationPlayback {
                image_index: cached_anim.image_index,
                textures: cached_anim.textures.clone(),
                hdr_frames: cached_anim.hdr_frames.clone(),
                delays: cached_anim.delays.clone(),
                current_frame: 0,
                frame_start: Instant::now(),
            });
        }

        // Check if we have a prefetched TileManager ready to use!
        if let Some(mut tm) = self.prefetched_tiles.remove(&self.current_index) {
            // We successfully hit the cache!
            // Save the prefetch-phase generation before incrementing. Any in-flight HQ preview
            // tasks (HDR or SDR) were spawned with this old generation. We record it so that
            // handle_preview_update() can accept their results instead of discarding them as
            // stale — avoiding a from-scratch re-render of huge EXR/JXL files.
            let prefetch_gen = self.generation;
            self.generation = self.generation.wrapping_add(1);
            self.loader.set_generation(self.generation);
            self.prefetch_prev_generation = Some(prefetch_gen);

            tm.generation = self.generation;
            self.current_image_res = Some((tm.full_width, tm.full_height));

            // Trigger deferred refinement for RAW sources (LibRaw demosaic).
            // HDR tiled sources: in-flight prefetch tasks carry `prefetch_gen` and will be
            // accepted by handle_preview_update via prefetch_prev_generation — no re-spawn needed.
            tm.get_source()
                .request_refinement(self.current_index, self.generation);

            self.tile_manager = Some(tm);

            crate::preload_debug!(
                "[PreloadDebug][RAW] navigate prefetch_tile_hit idx={} gen={} raw_hq={} logical={}",
                self.current_index,
                self.generation,
                self.settings.raw_high_quality,
                self.current_image_res
                    .map(|(w, h)| format!("{w}x{h}"))
                    .unwrap_or_default()
            );
            log::debug!(
                "[App] Cache Hit: Restored prefetched TileManager for index {} (prefetch_gen={} → current_gen={})",
                self.current_index,
                prefetch_gen,
                self.generation
            );
        } else if self.has_loaded_asset(self.current_index) {
            crate::preload_debug!(
                "[PreloadDebug][RAW] navigate asset_cache_hit idx={} raw_hq={} tiled_placeholder={} tile_mgr={}",
                self.current_index,
                self.settings.raw_high_quality,
                self.texture_cache.is_preview_placeholder(self.current_index),
                self.tile_manager.is_some()
            );
            // Decoded during preload (HDR cache and/or deferred SDR pixels) — avoid re-decoding.
            self.prefetch_prev_generation = None;
            self.generation = self.generation.wrapping_add(1);
            self.loader.set_generation(self.generation);
            let is_tiled = self
                .texture_cache
                .is_preview_placeholder(self.current_index);
            if is_tiled && self.tile_manager.is_none() {
                // Defensive fallback for any tiled preview (SDR or HDR with missing source cache)
                // that doesn't have a TileManager installed.
                if let Some((w, h)) = self.texture_cache.get_original_res(self.current_index) {
                    self.current_image_res = Some((w, h));
                }
                self.loader.request_load(
                    self.current_index,
                    self.generation,
                    self.image_files[self.current_index].clone(),
                    self.settings.raw_high_quality,
                );
            } else if let Some(hdr) = self.hdr_image_cache.get(&self.current_index) {
                self.current_image_res = Some((hdr.width, hdr.height));
            } else if let Some(src) = self.hdr_tiled_source_cache.get(&self.current_index) {
                self.current_image_res = Some((src.width(), src.height()));
                // Defensive fallback: if it is a tiled HDR image but the TileManager is missing,
                // trigger a request_load to rebuild the TileManager.
                if self.tile_manager.is_none() {
                    self.loader.request_load(
                        self.current_index,
                        self.generation,
                        self.image_files[self.current_index].clone(),
                        self.settings.raw_high_quality,
                    );
                }
            } else if let Some(decoded) = self.deferred_sdr_uploads.get(&self.current_index) {
                self.current_image_res = Some((decoded.width, decoded.height));
            }
        } else {
            crate::preload_debug!(
                "[PreloadDebug][RAW] navigate cache_miss idx={} raw_hq={} → request_load",
                self.current_index,
                self.settings.raw_high_quality
            );
            // Cache miss: fresh load required. Clear any leftover prefetch_prev_generation
            // so handle_preview_update doesn't erroneously accept stale old-gen results.
            self.prefetch_prev_generation = None;
            // ALWAYS increment generation on every navigation and request a fresh load.
            // This ensures TileManager is re-initialized for large images and
            // low-res thumbnails are upgraded to full resolution.
            self.generation = self.generation.wrapping_add(1);
            self.loader.set_generation(self.generation);
            self.loader.request_load(
                self.current_index,
                self.generation,
                self.image_files[self.current_index].clone(),
                self.settings.raw_high_quality,
            );
        }

        // Housekeeping: evict distant prefetch CPU caches (tiles, deferred SDR, static HDR).
        self.evict_distant_prefetch_caches();

        self.schedule_preloads(preload_forward);
        // When a prefetch hit occurred, also_keep_preview preserves any Preview result for the
        // current index that still carries the old prefetch generation — it may have arrived in
        // the channel between the generation bump and now and must not be thrown away.
        let also_keep = self
            .prefetch_prev_generation
            .map(|old_gen| (self.current_index, old_gen));
        self.loader
            .discard_pending_stale_outputs(self.generation, also_keep);
        self.trigger_current_hdr_fallback_refinement_if_needed();
        self.try_start_pending_transition_if_ready();
    }

    pub(crate) fn navigate_next(&mut self, ctx: &egui::Context) {
        if self.image_files.is_empty() {
            return;
        }
        let idx = (self.current_index + 1) % self.image_files.len();
        self.navigate_to(idx, ctx);
    }

    pub(crate) fn navigate_prev(&mut self, ctx: &egui::Context) {
        if self.image_files.is_empty() {
            return;
        }
        let idx = if self.current_index == 0 {
            self.image_files.len() - 1
        } else {
            self.current_index - 1
        };
        self.navigate_to(idx, ctx);
    }

    pub(crate) fn navigate_first(&mut self, ctx: &egui::Context) {
        self.navigate_to(0, ctx);
    }

    pub(crate) fn navigate_last(&mut self, ctx: &egui::Context) {
        if !self.image_files.is_empty() {
            let last = self.image_files.len() - 1;
            self.navigate_to(last, ctx);
        }
    }

    pub(crate) fn shuffle_slideshow_order_to_first(&mut self) {
        if self.image_files.is_empty() {
            self.random_slideshow_order_ready = false;
            return;
        }

        self.shuffle_current_image_list_preserving_pairs();
        self.clear_index_keyed_state_after_list_reorder();

        self.current_index = 0;
        self.current_rotation = 0;
        self.zoom_factor = 1.0;
        self.pan_offset = Vec2::ZERO;
        self.error_message = None;
        self.is_font_error = false;
        self.random_slideshow_order_ready = true;
        self.last_switch_time = Instant::now();

        self.loader.request_load(
            self.current_index,
            self.generation,
            self.image_files[self.current_index].clone(),
            self.settings.raw_high_quality,
        );
        self.schedule_preloads(true);
    }
}
