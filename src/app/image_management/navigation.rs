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
        if let Some(animation) = self.animation.as_ref()
            && animation.image_index == index
        {
            return animation.textures.get(animation.current_frame).cloned();
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
        let mut hdr = if index == self.current_index {
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

        // Outgoing transitions must reuse the same pixels the canvas was showing. Embedded SDR
        // master draws the 8-bit fallback texture; routing the outgoing frame through the HDR
        // float plane re-composes and looks noticeably dimmer for one or more frames.
        if hdr.as_ref().is_some_and(|buffer| {
            self.hdr_prefers_embedded_sdr_master_on_output(buffer)
                && (texture.is_some() || self.hdr_sdr_fallback_indices.contains(&index))
        }) {
            hdr = None;
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

    /// Drop gain-map HDR caches in the preload window and reload the current image after
    /// [`crate::settings::HdrGainMapSdrDisplayMode`] changes. Unlike [`Self::reload_current`],
    /// this applies to HEIF/AVIF/JPEG-R gain-map files, not only RAW.
    pub(crate) fn reload_after_hdr_gain_map_sdr_display_change(&mut self) {
        if self.image_files.is_empty() {
            return;
        }

        self.sync_loader_hdr_callback_upload_snapshot();
        self.cached_frame_hdr_render_path = None;

        let count = self.image_files.len();
        let sensitive: Vec<usize> = (0..count)
            .filter(|&idx| {
                prefetch_window_contains(
                    self.current_index,
                    count,
                    idx,
                    self.prefetch_window_max_distance,
                )
            })
            .filter(|&idx| self.index_sensitive_to_hdr_gain_map_sdr_display(idx))
            .collect();

        if sensitive.is_empty() {
            return;
        }

        let current = self.current_index;
        let cache_fast_path = sensitive
            .iter()
            .all(|&idx| self.hdr_gain_map_sdr_display_refreshable_from_cache(idx));

        if cache_fast_path {
            log::info!(
                "[HDR] hdr_gain_map_sdr_display -> {}; refreshed {} gain-map cache(s) in preload window (idx={} current)",
                self.settings.hdr_gain_map_sdr_display.label(),
                sensitive.len(),
                current
            );
            for &idx in &sensitive {
                self.apply_hdr_gain_map_sdr_display_from_cache(idx);
                self.texture_cache.remove(idx);
                self.prefetched_tiles.remove(&idx);
                crate::tile_cache::PIXEL_CACHE.write().remove_image(idx);
            }
            self.schedule_preloads(true);
            self.wake_root_for_logic();
            return;
        }

        log::info!(
            "[HDR] hdr_gain_map_sdr_display -> {}; evicting {} gain-map cache(s) in preload window and reloading current idx={}",
            self.settings.hdr_gain_map_sdr_display.label(),
            sensitive.len(),
            current
        );

        self.invalidate_decode_profile_epoch();
        self.loader.cancel_all();

        for idx in &sensitive {
            self.texture_cache.remove(*idx);
            self.prefetched_tiles.remove(idx);
            crate::tile_cache::PIXEL_CACHE.write().remove_image(*idx);
            self.remove_hdr_image_index(*idx);
            if *idx == current {
                self.tile_manager = None;
                self.set_current_image_resolution(None);
                self.animation = None;
                self.animation_cache.remove(idx);
                self.pending_anim_frames.remove(idx);
                self.prev_texture = None;
                self.prev_hdr_image = None;
                self.prev_transition_rect = None;
                self.transition_start = None;
                self.pending_transition_target = None;
            }
        }

        self.loader.request_load(
            current,
            self.image_files[current].clone(),
            self.settings.raw_high_quality,
            self.raw_demosaic_mode_for_index(current),
            self.settings.psd_hidden_layer_strategy,
        );
        self.schedule_preloads(true);
        self.wake_root_for_logic();
    }

    fn index_has_sdr_fallback_resident(&self, idx: usize) -> bool {
        self.hdr_sdr_fallback_indices.contains(&idx)
            || self.texture_cache.contains(idx)
            || self.deferred_sdr_uploads.contains_key(&idx)
    }

    fn index_sensitive_to_hdr_gain_map_sdr_display(&self, idx: usize) -> bool {
        let Some(path) = self.image_files.get(idx) else {
            return false;
        };
        crate::loader::index_hdr_gain_map_sdr_display_mode_affects(
            path,
            self.hdr_image_cache.get(&idx).map(|entry| entry.as_ref()),
            self.ultra_hdr_capacity_sensitive_indices.contains(&idx),
            self.index_has_sdr_fallback_resident(idx),
        )
    }

    /// Switch SDR gain-map presentation using cached planes when both routes are already resident.
    fn hdr_gain_map_sdr_display_refreshable_from_cache(&self, idx: usize) -> bool {
        let Some(hdr) = self.hdr_image_cache.get(&idx) else {
            return false;
        };
        let Some(path) = self.image_files.get(idx) else {
            return false;
        };
        if !crate::loader::hdr_gain_map_sdr_display_mode_affects_image(hdr, path) {
            return false;
        }
        let output_mode = crate::hdr::monitor::effective_render_output_mode(
            self.effective_hdr_target_format(),
            self.effective_hdr_monitor_selection().as_ref(),
        );
        if output_mode != crate::hdr::renderer::HdrRenderOutputMode::SdrToneMapped {
            return false;
        }

        let want_embedded = self.settings.hdr_gain_map_sdr_display
            == crate::settings::HdrGainMapSdrDisplayMode::EmbeddedSdrMaster;
        let has_sdr_fallback = self.index_has_sdr_fallback_resident(idx);
        let has_tone_map_plane = crate::loader::hdr_tone_map_plane_available_in_cache(hdr);
        if want_embedded {
            crate::loader::hdr_supports_embedded_sdr_master_display(hdr) && has_sdr_fallback
        } else {
            has_tone_map_plane
        }
    }

    fn apply_hdr_gain_map_sdr_display_from_cache(&mut self, idx: usize) {
        let Some(hdr) = self.hdr_image_cache.get(&idx).cloned() else {
            return;
        };
        if idx == self.current_index {
            self.current_hdr_image = Some(crate::app::CurrentHdrImage::new(idx, hdr));
            self.refresh_hdr_view_status();
        }
    }

    pub(crate) fn reload_current(&mut self) {
        if self.image_files.is_empty() {
            return;
        }

        let has_any_raw = self.image_files.iter().any(|path| {
            path.extension()
                .and_then(|e| e.to_str())
                .is_some_and(crate::raw_processor::is_raw_extension)
        });
        if !has_any_raw {
            return;
        }

        self.invalidate_all_raw_image_caches();

        crate::preload_debug!(
            "[PreloadDebug][RAW] setting_reload raw_hq={} current_idx={}",
            self.settings.raw_high_quality,
            self.current_index,
        );

        let path = self.image_files[self.current_index].clone();
        self.loader.request_load(
            self.current_index,
            path,
            self.settings.raw_high_quality,
            self.raw_demosaic_mode_for_index(self.current_index),
            self.settings.psd_hidden_layer_strategy,
        );

        // Re-schedule preloads so nearby RAW files pick up the new mode too.
        self.schedule_preloads(true);
    }

    /// Re-decode PSD/PSB after [`Settings::psd_hidden_layer_strategy`] changes.
    ///
    /// Unlike [`Self::reload_current`] (RAW-only), this clears failed-load marks so a previously
    /// unopenable PSD can be retried without restarting the process.
    pub(crate) fn reload_after_psd_hidden_layer_strategy_change(&mut self) {
        if self.image_files.is_empty() {
            return;
        }

        let psd_indices: Vec<usize> = self
            .image_files
            .iter()
            .enumerate()
            .filter_map(|(idx, path)| path_is_psd_or_psb(path).then_some(idx))
            .collect();
        if psd_indices.is_empty() {
            return;
        }

        self.invalidate_decode_profile_epoch();
        self.loader.cancel_all();
        self.image_status.set_psd_osd_line(None);

        let current = self.current_index;
        for idx in psd_indices {
            self.texture_cache.remove(idx);
            self.clear_installed_display_mode(idx);
            self.remove_hdr_image_resources(idx);
            self.prefetched_tiles.remove(&idx);
            self.deferred_sdr_uploads.remove(&idx);
            self.animation_cache.remove(&idx);
            self.pending_anim_frames.remove(&idx);
            crate::tile_cache::PIXEL_CACHE.write().remove_image(idx);
            self.main_loader_failed_indices.remove(&idx);
            self.main_loader_failed_errors.remove(&idx);
            self.directory_tree_strip_cache.remove_index(idx);
            self.directory_tree_strip_tiled_attempted.remove(&idx);
            self.directory_tree_strip_cold_attempted.remove(&idx);
            self.directory_tree_strip_cold_awaiting_main_loader
                .remove(&idx);
            if idx == current {
                self.tile_manager = None;
                self.set_current_image_resolution(None);
                self.animation = None;
                self.prev_texture = None;
                self.prev_hdr_image = None;
                self.prev_transition_rect = None;
                self.transition_start = None;
                self.pending_transition_target = None;
                self.pixel_data_source = None;
                self.error_message = None;
                self.is_font_error = false;
            }
        }

        crate::preload_debug!(
            "[PreloadDebug][PSD] setting_reload strategy={} current_idx={}",
            self.settings.psd_hidden_layer_strategy,
            current,
        );

        if path_is_psd_or_psb(&self.image_files[current]) {
            self.loader.request_load(
                current,
                self.image_files[current].clone(),
                self.settings.raw_high_quality,
                self.raw_demosaic_mode_for_index(current),
                self.settings.psd_hidden_layer_strategy,
            );
        }
        self.schedule_preloads(true);
        self.wake_root_for_logic();
    }

    /// Drop every cached/prefetched RAW decode so a [`Settings::raw_high_quality`] toggle cannot
    /// leave neighbor images stuck in the previous performance/HQ pipeline.
    fn invalidate_all_raw_image_caches(&mut self) {
        self.invalidate_decode_profile_epoch();
        self.loader.cancel_all();
        self.gpu_demosaic_failed_indices.clear();
        self.raw_gpu_demosaic_await_hdr_present = false;
        self.hdr_raw_gpu_demosaic_pending_key_index.clear();
        self.cpu_raw_refinement_pending_indices.clear();
        self.hq_tiled_preview_pending_indices.clear();

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
            self.clear_installed_display_mode(idx);
            self.remove_hdr_image_resources(idx);
            self.raw_metadata.remove(idx);
            self.prefetched_tiles.remove(&idx);
            self.deferred_sdr_uploads.remove(&idx);
            crate::tile_cache::PIXEL_CACHE.write().remove_image(idx);
        }

        self.tile_manager = None;
        self.set_current_image_resolution(None);
        self.animation = None;
        self.prev_transition_rect = None;
        self.prev_texture = None;
        self.prev_hdr_image = None;
        self.transition_start = None;
        self.pending_transition_target = None;

        crate::preload_debug!(
            "[PreloadDebug][RAW] invalidate_all_raw_caches cleared {} raw indices",
            _raw_index_count,
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
        self.canvas_display_timing.on_navigate();
        #[cfg(feature = "preload-debug")]
        {
            let target_is_raw = self
                .image_files
                .get(target_index)
                .is_some_and(|p| crate::preload_debug::path_is_raw(p));
            let target_has_texture = self.texture_cache.contains(target_index);
            let target_has_hdr_plane = self.hdr_image_cache.contains_key(&target_index)
                || self.hdr_tiled_source_cache.contains_key(&target_index);
            let target_gpu_raw = self
                .hdr_image_cache
                .get(&target_index)
                .is_some_and(|img| img.metadata.raw_gpu_source.is_some());
            crate::preload_debug!(
                "[PreloadDebug][Nav] {previous_index} -> {target_index} raw={target_is_raw} gpu_raw={target_gpu_raw} tex_cache={target_has_texture} hdr_cache={target_has_hdr_plane}"
            );
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
            self.canvas_rect_for_layout(ctx),
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

        preserve_current_tile_manager_for_navigation(self, self.current_index, target_index);
        if self.current_index != target_index
            && self.prefetched_tiles.contains_key(&self.current_index)
        {
            self.track_prefetch_resource(self.current_index);
        }
        self.set_current_index(target_index);
        self.refresh_current_file_name();
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
        self.set_zoom_factor(1.0);
        self.pan_offset = Vec2::ZERO;
        self.animation = None;
        self.pixel_data_source = None;
        self.pixel_hover_cache = None;
        self.pixel_region_first_point = None;

        // Update resolution if already in cache (for immediate low-res display)
        if self.texture_cache.contains(self.current_index) {
            if let Some((w, h)) = self.texture_cache.get_original_res(self.current_index) {
                self.set_current_image_resolution(Some((w, h)));
            } else if let Some(texture) = self.texture_cache.get(self.current_index) {
                let size = texture.size();
                self.set_current_image_resolution(Some((size[0] as u32, size[1] as u32)));
            }
        } else {
            self.set_current_image_resolution(None);
        }

        self.last_switch_time = Instant::now();
        self.error_message = None;
        self.is_font_error = false;
        self.surface_main_loader_failure_for_current();
        ctx.request_repaint();
        // Close any open EXIF/XMP/PixelRegion modal — it shows data for the previous image
        if matches!(
            self.active_modal,
            Some(crate::ui::dialogs::modal_state::ActiveModal::Exif(_))
                | Some(crate::ui::dialogs::modal_state::ActiveModal::Xmp(_))
                | Some(crate::ui::dialogs::modal_state::ActiveModal::PixelRegion(_))
        ) {
            self.active_modal = None;
        }

        // Try to pull from predictive cache if available
        self.ensure_current_animation_playback();

        let needs_animation_reload = self.needs_stale_animated_first_frame_reload();

        // Check if we have a prefetched TileManager ready to use!
        if self.index_uses_animated_pipeline(self.current_index) {
            if self.prefetched_tiles.remove(&self.current_index).is_some() {
                log::debug!(
                    "[App] Dropped stale prefetched TileManager for animated index {}",
                    self.current_index
                );
            }
        } else if let Some(mut tm) = self.prefetched_tiles.remove(&self.current_index) {
            if self.index_requires_tile_manager(self.current_index) {
                if self.installed_display_mode(self.current_index)
                    != Some(crate::loader::RenderShape::Tiled)
                {
                    self.record_installed_display_mode(
                        self.current_index,
                        crate::loader::RenderShape::Tiled,
                    );
                }
                tm.decode_profile = self.decode_profile_for_index(self.current_index);
                let _ = tm.sync_dimensions_from_source();
                self.set_current_image_resolution(Some((tm.full_width, tm.full_height)));
                crate::tile_cache::set_tile_size_for_image(tm.full_width, tm.full_height);

                tm.get_source()
                    .request_refinement(self.current_index, tm.decode_profile.clone());

                self.note_cpu_raw_refinement_requested(self.current_index);

                if self.texture_cache.contains(self.current_index) {
                    let tm_max = tm.preview_texture.as_ref().map(|h| {
                        let s = h.size();
                        s[0].max(s[1]) as u32
                    });
                    let cached_max = self
                        .texture_cache
                        .cached_preview_max_side(self.current_index);
                    if (tm.preview_texture.is_none()
                        || cached_max.is_some_and(|c| tm_max.is_none_or(|t| c > t)))
                        && let Some(handle) = self.texture_cache.get(self.current_index)
                    {
                        tm.preview_texture = Some(handle.clone());
                    }
                }

                self.pixel_data_source = Some(crate::pixel_inspector::PixelDataSource::Tiled(
                    tm.get_source(),
                ));
                self.tile_manager = Some(tm);

                crate::preload_debug!(
                    "[PreloadDebug][RAW] navigate prefetch_tile_hit idx={} raw_hq={} logical={}",
                    self.current_index,
                    self.settings.raw_high_quality,
                    self.current_image_res
                        .map(|(w, h)| format!("{w}x{h}"))
                        .unwrap_or_default()
                );
                log::debug!(
                    "[App] Cache Hit: Restored prefetched TileManager for index {}",
                    self.current_index
                );
            } else {
                log::debug!(
                    "[App] Dropped stale prefetched TileManager for non-tiled index {}",
                    self.current_index
                );
            }
        } else if needs_animation_reload {
            crate::preload_debug!(
                "[PreloadDebug] navigate animation_reload idx={} reason=first_frame_only_preload",
                self.current_index
            );
            self.loader.request_load(
                self.current_index,
                self.image_files[self.current_index].clone(),
                self.settings.raw_high_quality,
                self.raw_demosaic_mode_for_index(self.current_index),
                self.settings.psd_hidden_layer_strategy,
            );
        } else if self.has_loaded_asset(self.current_index)
            && !raw_hq_navigate_missing_hdr_plane(
                &self.image_files,
                self.current_index,
                self.settings.raw_high_quality,
                &self.hdr_image_cache,
                &self.hdr_tiled_source_cache,
            )
        {
            crate::preload_debug!(
                "[PreloadDebug][RAW] navigate asset_cache_hit idx={} raw_hq={} display_mode={:?} tile_mgr={}",
                self.current_index,
                self.settings.raw_high_quality,
                self.installed_display_mode(self.current_index),
                self.tile_manager.is_some()
            );
            self.flush_deferred_sdr_upload_for_index(self.current_index, ctx);
            let needs_tile_manager_rebuild =
                self.index_requires_tile_manager(self.current_index) && self.tile_manager.is_none();
            if needs_tile_manager_rebuild {
                if let Some((w, h)) = self.texture_cache.get_original_res(self.current_index) {
                    self.set_current_image_resolution(Some((w, h)));
                } else if let Some(hdr) = self.hdr_image_cache.get(&self.current_index) {
                    self.set_current_image_resolution(Some((hdr.width, hdr.height)));
                } else if let Some(src) = self.hdr_tiled_source_cache.get(&self.current_index) {
                    self.set_current_image_resolution(Some((src.width(), src.height())));
                }
                crate::preload_debug!(
                    "[PreloadDebug][RAW] navigate tile_manager_rebuild idx={} display_mode={:?} needs_tile_mgr_flag={}",
                    self.current_index,
                    self.installed_display_mode(self.current_index),
                    self.texture_cache.needs_tile_manager(self.current_index),
                );
                self.loader.request_load(
                    self.current_index,
                    self.image_files[self.current_index].clone(),
                    self.settings.raw_high_quality,
                    self.raw_demosaic_mode_for_index(self.current_index),
                    self.settings.psd_hidden_layer_strategy,
                );
            } else if let Some(hdr) = self.hdr_image_cache.get(&self.current_index) {
                self.set_current_image_resolution(Some((hdr.width, hdr.height)));
            } else if let Some(src) = self.hdr_tiled_source_cache.get(&self.current_index) {
                self.set_current_image_resolution(Some((src.width(), src.height())));
            } else if let Some(decoded) = self.deferred_sdr_uploads.get(&self.current_index) {
                self.set_current_image_resolution(Some((decoded.width, decoded.height)));
            }
        } else {
            let idx = self.current_index;
            if self.loader.is_loading(idx) {
                self.loader.promote_inflight_to_current(idx);
                #[cfg(feature = "preload-debug")]
                {
                    let missing_hdr = raw_hq_navigate_missing_hdr_plane(
                        &self.image_files,
                        idx,
                        self.settings.raw_high_quality,
                        &self.hdr_image_cache,
                        &self.hdr_tiled_source_cache,
                    );
                    crate::preload_debug!(
                        "[PreloadDebug] navigate inflight_reuse idx={} raw_hq={} missing_hdr={}",
                        idx,
                        self.settings.raw_high_quality,
                        missing_hdr
                    );
                }
                if self.current_image_res.is_none() {
                    if let Some((w, h)) = self.texture_cache.get_original_res(idx) {
                        self.set_current_image_resolution(Some((w, h)));
                    } else if let Some(hdr) = self.hdr_image_cache.get(&idx) {
                        self.set_current_image_resolution(Some((hdr.width, hdr.height)));
                    } else if let Some(decoded) = self.deferred_sdr_uploads.get(&idx) {
                        self.set_current_image_resolution(Some((decoded.width, decoded.height)));
                    }
                }
                self.flush_deferred_sdr_upload_for_index(idx, ctx);
            } else if self.main_loader_failed_indices.contains(&idx) {
                // Re-navigation clears a prior failure so transient OOM / I/O
                // errors can be retried. Preload stays gated on the set so
                // background work does not storm a permanently broken file.
                self.main_loader_failed_indices.remove(&idx);
                self.main_loader_failed_errors.remove(&idx);
                self.error_message = None;
                self.is_font_error = false;
                self.loader.request_load(
                    idx,
                    self.image_files[idx].clone(),
                    self.settings.raw_high_quality,
                    self.raw_demosaic_mode_for_index(idx),
                    self.settings.psd_hidden_layer_strategy,
                );
            } else {
                #[cfg_attr(not(feature = "preload-debug"), allow(unused_variables))]
                let missing_hdr = raw_hq_navigate_missing_hdr_plane(
                    &self.image_files,
                    idx,
                    self.settings.raw_high_quality,
                    &self.hdr_image_cache,
                    &self.hdr_tiled_source_cache,
                );
                #[cfg(feature = "preload-debug")]
                {
                    let bootstrap_only = missing_hdr
                        && crate::app::image_management::raw_hq_has_bootstrap_sdr_only(
                            &self.image_files,
                            idx,
                            self.settings.raw_high_quality,
                            &self.hdr_image_cache,
                            &self.hdr_tiled_source_cache,
                            self.texture_cache.contains(idx),
                            self.deferred_sdr_uploads.contains_key(&idx),
                        );
                    if missing_hdr {
                        crate::preload_debug!(
                            "[PreloadDebug][RAW] navigate missing_hdr_plane idx={} bootstrap_only={} → request_load",
                            idx,
                            bootstrap_only
                        );
                    } else {
                        crate::preload_debug!(
                            "[PreloadDebug][RAW] navigate cache_miss idx={} raw_hq={} → request_load",
                            idx,
                            self.settings.raw_high_quality
                        );
                    }
                }
                self.loader.request_load(
                    idx,
                    self.image_files[idx].clone(),
                    self.settings.raw_high_quality,
                    self.raw_demosaic_mode_for_index(idx),
                    self.settings.psd_hidden_layer_strategy,
                );
            }
        }

        self.ensure_raw_inflight_bootstrap_present(self.current_index, ctx);

        self.sync_loader_preload_plan();

        // Housekeeping: evict distant prefetch CPU caches (tiles, deferred SDR, static HDR).
        self.evict_distant_prefetch_caches();
        self.cancel_outside_prefetch_window_loader_tasks();

        self.schedule_preloads(preload_forward);
        self.discard_stale_loader_outputs();
        self.refresh_pixel_data_source_for_current_index();
        if self.settings.show_pixel_inspector
            && self.pixel_data_source.is_none()
            && !self
                .main_loader_failed_indices
                .contains(&self.current_index)
        {
            self.loader.request_load(
                self.current_index,
                self.image_files[self.current_index].clone(),
                self.settings.raw_high_quality,
                self.raw_demosaic_mode_for_index(self.current_index),
                self.settings.psd_hidden_layer_strategy,
            );
        }
        self.sync_and_ensure_hq_tiled_preview(self.current_index, ctx);
        self.try_start_pending_transition_if_ready();
        self.sync_directory_tree_file_list_state(ctx);
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

        self.set_current_index(0);
        self.current_rotation = 0;
        self.set_zoom_factor(1.0);
        self.pan_offset = Vec2::ZERO;
        self.error_message = None;
        self.is_font_error = false;
        self.random_slideshow_order_ready = true;
        self.last_switch_time = Instant::now();

        self.loader.request_load(
            self.current_index,
            self.image_files[self.current_index].clone(),
            self.settings.raw_high_quality,
            self.raw_demosaic_mode_for_index(self.current_index),
            self.settings.psd_hidden_layer_strategy,
        );
        self.schedule_preloads(true);
        self.refresh_current_file_name();
    }

    /// Rebind main-canvas presentation to whatever file now occupies `current_index` after a
    /// cache-preserving directory-tree list reorder.
    pub(crate) fn refresh_current_image_presentation_after_list_reorder(&mut self) {
        if self.image_files.is_empty() {
            return;
        }
        let idx = self
            .current_index
            .min(self.image_files.len().saturating_sub(1));

        self.transition_start = None;
        self.pending_transition_target = None;
        self.prev_texture = None;
        self.prev_hdr_image = None;
        self.prev_transition_rect = None;
        self.tile_manager = None;
        self.pixel_data_source = None;
        self.pixel_hover_cache = None;
        self.animation = None;

        self.refresh_current_file_name();
        self.current_hdr_image = self
            .first_cached_hdr_still_for_index(idx)
            .map(|image| crate::app::CurrentHdrImage::new(idx, image));
        self.current_hdr_tiled_image = self
            .hdr_tiled_source_cache
            .get(&idx)
            .cloned()
            .map(|source| crate::app::CurrentHdrTiledImage::new(idx, source));
        self.current_hdr_tiled_preview = self
            .hdr_tiled_preview_cache
            .get(&idx)
            .cloned()
            .map(|image| crate::app::CurrentHdrImage::new(idx, image));

        if self.texture_cache.contains(idx) {
            if let Some((w, h)) = self.texture_cache.get_original_res(idx) {
                self.set_current_image_resolution(Some((w, h)));
            } else if let Some(texture) = self.texture_cache.get(idx) {
                let size = texture.size();
                self.set_current_image_resolution(Some((size[0] as u32, size[1] as u32)));
            }
        } else {
            self.set_current_image_resolution(None);
        }

        self.ensure_current_animation_playback();
        self.refresh_pixel_data_source_for_current_index();
        self.refresh_hdr_view_status();
        self.error_message = None;
        self.is_font_error = false;
        self.surface_main_loader_failure_for_current();
        self.canvas_display_timing.on_navigate();
    }

    pub(crate) fn refresh_pixel_data_source_for_current_index(&mut self) {
        if self.image_files.is_empty() {
            self.pixel_data_source = None;
            return;
        }

        // 1. TileManager
        if let Some(tm) = &self.tile_manager {
            self.pixel_data_source = Some(crate::pixel_inspector::PixelDataSource::Tiled(
                tm.get_source(),
            ));
            return;
        }

        // 2. Animated frames
        if let Some(ref anim) = self.animation
            && anim.image_index == self.current_index
            && let Some(ref cpu_frames) = anim.cpu_frames
            && let Some(pixels) = cpu_frames.get(anim.current_frame)
        {
            let size = anim.textures[anim.current_frame].size();
            self.pixel_data_source = Some(crate::pixel_inspector::PixelDataSource::Static {
                width: size[0] as u32,
                height: size[1] as u32,
                pixels: std::sync::Arc::clone(pixels),
            });
            return;
        }
        if let Some(cached_anim) = self.animation_cache.get(&self.current_index)
            && let Some(ref cpu_frames) = cached_anim.cpu_frames
            && let Some(pixels) = cpu_frames.first()
            && let Some(texture) = cached_anim.textures.first()
        {
            let size = texture.size();
            self.pixel_data_source = Some(crate::pixel_inspector::PixelDataSource::Static {
                width: size[0] as u32,
                height: size[1] as u32,
                pixels: std::sync::Arc::clone(pixels),
            });
            return;
        }

        // 3. Deferred SDR upload
        if let Some(decoded) = self.deferred_sdr_uploads.get(&self.current_index) {
            self.pixel_data_source = Some(crate::pixel_inspector::PixelDataSource::Static {
                width: decoded.width,
                height: decoded.height,
                pixels: decoded.arc_pixels(),
            });
            return;
        }

        self.pixel_data_source = None;
    }
}

fn path_is_psd_or_psb(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| {
            let ext = ext.to_ascii_lowercase();
            ext == "psd" || ext == "psb"
        })
}
