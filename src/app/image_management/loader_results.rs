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
    pub(crate) fn process_file_op_results(&mut self) {
        while let Ok(res) = self.file_op_rx.try_recv() {
            match res {
                FileOpResult::Delete(path, original_idx, res) => {
                    if let Err(e) = res {
                        log::error!("Failed to delete {:?}: {}", path, e);
                        self.error_message =
                            Some(t!("status.delete_failed", err = e.to_string()).to_string());

                        // ROLLBACK: Restore the file to the in-memory list if it failed to delete.
                        // We use the original index to maintain order.
                        if original_idx <= self.image_files.len() {
                            self.image_files.insert(original_idx, path.clone());
                            let sz = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                            self.file_byte_len_by_index.insert(original_idx, sz);
                        } else {
                            self.image_files.push(path.clone());
                            let sz = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                            self.file_byte_len_by_index.push(sz);
                        }

                        // Restore viewer state to ensure consistency.
                        // We jump back to the file that failed to delete to ensure the index is valid.
                        self.current_index = original_idx;
                        self.generation = self.generation.wrapping_add(1);
                        self.loader.set_generation(self.generation);
                        self.status_message =
                            t!("status.found", count = self.image_files.len().to_string())
                                .to_string();
                        self.images_ever_loaded = true;
                        self.schedule_preloads(true);
                    } else {
                        log::info!("Successfully deleted {:?}", path);
                    }
                }
                FileOpResult::Exif(path, data) => {
                    if let Some(crate::ui::dialogs::modal_state::ActiveModal::Exif(ref mut state)) =
                        self.active_modal
                    {
                        if state.path == path {
                            state.data = data;
                            state.loading = false;
                        }
                    }
                }
                FileOpResult::Xmp(path, data) => {
                    if let Some(crate::ui::dialogs::modal_state::ActiveModal::Xmp(ref mut state)) =
                        self.active_modal
                    {
                        if state.path == path {
                            if let Some((d, x)) = data {
                                state.data = Some(d);
                                state.xml = Some(x);
                            } else {
                                state.data = None;
                                state.xml = None;
                            }
                            state.loading = false;
                        }
                    }
                }
                FileOpResult::Wallpaper {
                    current,
                    monitors,
                    supports_per_monitor,
                } => {
                    if let Some(crate::ui::dialogs::modal_state::ActiveModal::Wallpaper(
                        ref mut state,
                    )) = self.active_modal
                    {
                        state.apply_wallpaper_probe(current, monitors, supports_per_monitor);
                    }
                }
            }
        }
    }

    /// Process results from the background ImageLoader.
    pub(crate) fn process_loaded_images(&mut self, ctx: &egui::Context) {
        self.flush_deferred_sdr_upload_for_current(ctx);
        let is_transitioning = self.transition_start.is_some();

        // ── 1. Continue uploading deferred animation frames (max 8 per tick) ──
        const ANIM_UPLOAD_QUOTA: usize = 8;
        let defer_pending_animation_upload =
            self.pending_anim_frames.as_ref().is_some_and(|pending| {
                should_defer_background_upload_during_transition(
                    pending.image_index == self.current_index,
                    is_transitioning,
                    self.transition_settled_at,
                )
            });
        #[cfg(feature = "preload-debug")]
        if defer_pending_animation_upload && let Some(pending) = self.pending_anim_frames.as_ref() {
            preload_debug!(
                "[PreloadDebug] defer pending animation upload: idx={} current={} next_frame={} total_frames={} reason=transition",
                pending.image_index,
                self.current_index,
                pending.next_frame,
                pending.frames.len()
            );
        }
        if !defer_pending_animation_upload && let Some(ref mut pending) = self.pending_anim_frames {
            let mut uploaded = 0;
            while pending.next_frame < pending.frames.len() && uploaded < ANIM_UPLOAD_QUOTA {
                let i = pending.next_frame;
                let frame = &pending.frames[i];
                let color_image = ColorImage::from_rgba_unmultiplied(
                    [frame.width as usize, frame.height as usize],
                    frame.rgba(),
                );
                let name = format!("anim_{}_{}", pending.image_index, i);
                let handle = ctx.load_texture(name, color_image, TextureOptions::LINEAR);
                pending.textures.push(handle);
                pending.delays.push(frame.delay);
                pending.next_frame += 1;
                uploaded += 1;
            }

            // Check if all frames have been uploaded
            if pending.next_frame >= pending.frames.len() {
                let idx = pending.image_index;

                // Build the final AnimationPlayback from the now-complete upload
                let playback = AnimationPlayback {
                    image_index: idx,
                    textures: std::mem::take(&mut pending.textures),
                    hdr_frames: pending.hdr_frames.clone(),
                    delays: std::mem::take(&mut pending.delays),
                    current_frame: 0,
                    frame_start: Instant::now(),
                };

                if idx == self.current_index {
                    if let Some(hdr_frames) = &playback.hdr_frames {
                        if let Some(hdr) = hdr_frames.first() {
                            self.current_hdr_image =
                                Some(crate::app::CurrentHdrImage::new(idx, Arc::clone(hdr)));
                        }
                    }
                    self.animation = Some(AnimationPlayback {
                        image_index: playback.image_index,
                        textures: playback.textures.clone(),
                        hdr_frames: playback.hdr_frames.clone(),
                        delays: playback.delays.clone(),
                        current_frame: 0,
                        frame_start: Instant::now(),
                    });
                    self.invalidate_osd();
                }
                self.animation_cache.insert(idx, playback);
                self.pending_anim_frames = None;
            } else {
                // More frames remain — ask for another repaint
                ctx.request_repaint();
            }
        }

        // ── 2. Process results from the background ImageLoader ──
        //
        // Generation vs `prefetch_prev_generation` (why Preview is special):
        // `handle_preview_update` accepts HQ preview results whose `generation` equals
        // `prefetch_prev_generation` for the current index, because refinement can finish after
        // we bump `self.generation` when promoting a prefetched `TileManager`. `LoaderOutput::Image`
        // uses no analogous bypass: decoded images are keyed to the generation from the active
        // `request_load` / refinement request (`do_load` tracks that generation), so they either match
        // `self.generation` in `gen_match` below or must be dropped; extending the prefetch survivor rule
        // here would widen the stale-result window without a matching in-flight Image pipeline.
        //
        // QUOTA DESIGN:
        //   - We count each ctx.load_texture() call as one "upload slot".
        //   - Tile results and Refined notifications do NOT consume slots
        //     (they don't call load_texture on the main thread path).
        //   - The current image is always allowed through, regardless of quota,
        //     so switching images is never blocked by background preload traffic.
        //   - When quota is reached, the polled-but-unprocessed item is pushed
        //     back via repush() so it is the first thing processed next frame.
        const GLOBAL_UPLOAD_QUOTA: usize = 3;
        let background_upload_quota = background_upload_quota_after_transition(
            GLOBAL_UPLOAD_QUOTA,
            self.transition_settled_at,
        );
        let mut uploads_this_frame: usize = 0;
        let mut sdr_upload_bytes_this_frame: usize = 0;
        let sdr_upload_budget_bytes_this_frame =
            sdr_upload_budget_bytes_per_frame(self.hardware_tier);
        let mut yielded_background_outputs = Vec::new();
        let mut current_refinement_pending = self
            .hdr_in_flight_fallback_refinements
            .contains(&self.current_index);

        while let Some(output) = self.loader.poll() {
            match output {
                LoaderOutput::Image(load_result) => {
                    let idx = load_result.index;
                    let generation = load_result.generation;
                    let is_current = idx == self.current_index;
                    let gen_match = generation == self.generation;

                    // CRITICAL: Drop any stale results, even for the current index.
                    // This prevents a race where deleting an image reuses the index
                    // but a late decode from the deleted file arrives and overwrites
                    // the new current image state.
                    if !gen_match {
                        self.loader.finish_image_request(idx, generation);
                        continue;
                    }
                    if !source_key_matches_index(&self.image_files, idx, load_result.source_key) {
                        log::warn!(
                            "[App] Image result discarded (source key mismatch): index={} generation={}",
                            idx,
                            generation
                        );
                        self.loader.finish_image_request(idx, generation);
                        continue;
                    }

                    if should_yield_background_result_for_pending_transition(
                        is_current,
                        self.pending_transition_target,
                        self.current_index,
                    ) {
                        preload_debug!(
                            "[PreloadDebug] yield image install: idx={} current={} gen={} reason=pending_transition_target",
                            idx,
                            self.current_index,
                            generation,
                        );
                        yielded_background_outputs.push(LoaderOutput::Image(load_result));
                        continue;
                    }
                    if should_yield_background_result_for_post_transition_refinement(
                        is_current,
                        self.transition_settled_at,
                        current_refinement_pending,
                    ) {
                        preload_debug!(
                            "[PreloadDebug] yield image install: idx={} current={} gen={} reason=current_refinement_pending",
                            idx,
                            self.current_index,
                            generation,
                        );
                        yielded_background_outputs.push(LoaderOutput::Image(load_result));
                        continue;
                    }

                    if should_defer_background_upload_during_transition(
                        is_current,
                        is_transitioning,
                        self.transition_settled_at,
                    ) {
                        preload_debug!(
                            "[PreloadDebug] defer image install: idx={} current={} gen={} reason={}",
                            idx,
                            self.current_index,
                            generation,
                            if is_transitioning {
                                "transition"
                            } else {
                                "post_transition_settle"
                            }
                        );
                        self.loader.repush(LoaderOutput::Image(load_result));
                        ctx.request_repaint();
                        break;
                    }

                    let install_plan = ImageInstallPlan::from_load_result(&load_result);
                    let estimated_sdr_upload_bytes = install_plan.estimated_sdr_upload_bytes();
                    if estimated_sdr_upload_bytes > 0
                        && !should_upload_sdr_this_frame(
                            is_current,
                            sdr_upload_bytes_this_frame,
                            estimated_sdr_upload_bytes,
                            sdr_upload_budget_bytes_this_frame,
                        )
                    {
                        preload_debug!(
                            "[PreloadDebug] defer image install: idx={} current={} gen={} reason=sdr_upload_budget uploaded_bytes={} candidate_bytes={} budget_bytes={}",
                            idx,
                            self.current_index,
                            generation,
                            sdr_upload_bytes_this_frame,
                            estimated_sdr_upload_bytes,
                            sdr_upload_budget_bytes_this_frame
                        );
                        self.loader.repush(LoaderOutput::Image(load_result));
                        ctx.request_repaint();
                        break;
                    }

                    // DESIGN: The current image ALWAYS bypasses the upload quota.
                    if !is_current && uploads_this_frame >= background_upload_quota {
                        preload_debug!(
                            "[PreloadDebug] defer image install: idx={} current={} gen={} reason=global_upload_quota uploads_this_frame={} quota={}",
                            idx,
                            self.current_index,
                            generation,
                            uploads_this_frame,
                            background_upload_quota
                        );
                        self.loader.repush(LoaderOutput::Image(load_result));
                        ctx.request_repaint();
                        break;
                    }
                    if estimated_sdr_upload_bytes > 0
                        && should_space_background_upload_after_transition(
                            is_current,
                            self.transition_settled_at,
                            self.last_background_upload_at,
                        )
                    {
                        preload_debug!(
                            "[PreloadDebug] defer image install: idx={} current={} gen={} reason=post_transition_spacing",
                            idx,
                            self.current_index,
                            generation,
                        );
                        self.loader.repush(LoaderOutput::Image(load_result));
                        ctx.request_repaint_after(std::time::Duration::from_millis(16));
                        break;
                    }

                    preload_debug!(
                        "[PreloadDebug] install image: idx={} current={} gen={} is_current={} estimated_sdr_upload_bytes={} uploads_before={} uploaded_bytes_before={}",
                        idx,
                        self.current_index,
                        generation,
                        is_current,
                        estimated_sdr_upload_bytes,
                        uploads_this_frame,
                        sdr_upload_bytes_this_frame
                    );
                    self.loader.finish_image_request(idx, generation);
                    if let Some((requeue_idx, requeue_gen, requeue_path)) =
                        self.handle_image_load_result(&load_result, install_plan, ctx)
                    {
                        // The slot was just freed by finish_image_request above; it is now safe to
                        // re-queue.  The loader holds the current (correct) HDR capacity.
                        self.loader.request_load(
                            requeue_idx,
                            requeue_gen,
                            requeue_path,
                            self.settings.raw_high_quality,
                        );
                    }
                    uploads_this_frame += 1;
                    if !is_current && estimated_sdr_upload_bytes > 0 {
                        self.last_background_upload_at = Some(Instant::now());
                    }
                    sdr_upload_bytes_this_frame =
                        sdr_upload_bytes_this_frame.saturating_add(estimated_sdr_upload_bytes);

                    if should_request_repaint_for_asset_update(
                        AssetUpdateKind::ImageLoaded,
                        is_current,
                        false,
                    ) {
                        ctx.request_repaint();
                    }
                }

                LoaderOutput::Preview(preview_update) => {
                    let preview_is_current = preview_update.index == self.current_index;
                    if !source_key_matches_index(
                        &self.image_files,
                        preview_update.index,
                        preview_update.source_key,
                    ) {
                        log::warn!(
                            "[App] Preview update discarded (source key mismatch): index={} generation={}",
                            preview_update.index,
                            preview_update.generation
                        );
                        continue;
                    }

                    if should_yield_background_result_for_pending_transition(
                        preview_is_current,
                        self.pending_transition_target,
                        self.current_index,
                    ) {
                        preload_debug!(
                            "[PreloadDebug] yield preview update: idx={} current={} gen={} reason=pending_transition_target",
                            preview_update.index,
                            self.current_index,
                            preview_update.generation,
                        );
                        yielded_background_outputs.push(LoaderOutput::Preview(preview_update));
                        continue;
                    }
                    if should_yield_background_result_for_post_transition_refinement(
                        preview_is_current,
                        self.transition_settled_at,
                        current_refinement_pending,
                    ) {
                        preload_debug!(
                            "[PreloadDebug] yield preview update: idx={} current={} gen={} reason=current_refinement_pending",
                            preview_update.index,
                            self.current_index,
                            preview_update.generation,
                        );
                        yielded_background_outputs.push(LoaderOutput::Preview(preview_update));
                        continue;
                    }

                    // DESIGN: Mirror the Image bypass — the current image's HQ preview
                    // also skips the quota.
                    let preview_has_sdr_upload = preview_result_has_sdr_upload(&preview_update);
                    if should_defer_preview_update_during_transition(
                        preview_is_current,
                        is_transitioning,
                    ) {
                        preload_debug!(
                            "[PreloadDebug] defer preview update: idx={} current={} gen={} reason=transition",
                            preview_update.index,
                            self.current_index,
                            preview_update.generation
                        );
                        self.loader.repush(LoaderOutput::Preview(preview_update));
                        ctx.request_repaint();
                        break;
                    }
                    if preview_has_sdr_upload
                        && !preview_is_current
                        && uploads_this_frame >= background_upload_quota
                    {
                        preload_debug!(
                            "[PreloadDebug] defer preview update: idx={} current={} gen={} reason=global_upload_quota uploads_this_frame={} quota={}",
                            preview_update.index,
                            self.current_index,
                            preview_update.generation,
                            uploads_this_frame,
                            background_upload_quota
                        );
                        self.loader.repush(LoaderOutput::Preview(preview_update));
                        ctx.request_repaint();
                        break;
                    }
                    if preview_has_sdr_upload
                        && should_space_background_upload_after_transition(
                            preview_is_current,
                            self.transition_settled_at,
                            self.last_background_upload_at,
                        )
                    {
                        preload_debug!(
                            "[PreloadDebug] defer preview update: idx={} current={} gen={} reason=post_transition_spacing",
                            preview_update.index,
                            self.current_index,
                            preview_update.generation,
                        );
                        self.loader.repush(LoaderOutput::Preview(preview_update));
                        ctx.request_repaint_after(std::time::Duration::from_millis(16));
                        break;
                    }
                    preload_debug!(
                        "[PreloadDebug] install preview: idx={} current={} gen={} is_current={} uploads_before={}",
                        preview_update.index,
                        self.current_index,
                        preview_update.generation,
                        preview_is_current,
                        uploads_this_frame
                    );
                    self.handle_preview_update(preview_update, ctx);
                    if preview_has_sdr_upload {
                        uploads_this_frame += 1;
                        if !preview_is_current {
                            self.last_background_upload_at = Some(Instant::now());
                        }
                    }
                }

                LoaderOutput::Tile(tile_result) => {
                    // Tile signals are free: pixels live in PIXEL_CACHE; GPU upload
                    // happens lazily in the tile rendering pass, not here.
                    self.handle_tile_load_result(tile_result, ctx);
                }

                LoaderOutput::Refined(idx, gen_id) => {
                    // Metadata-only notification — no load_texture call here.
                    if self
                        .image_files
                        .get(idx)
                        .is_some_and(|p| crate::preload_debug::path_is_raw(p))
                    {
                        crate::preload_debug!(
                            "[PreloadDebug][RAW] refined_notify idx={} gen={} current={} app_gen={}",
                            idx,
                            gen_id,
                            idx == self.current_index,
                            self.generation
                        );
                    }
                    self.handle_refined_notification(idx, gen_id, ctx);
                }

                LoaderOutput::HdrSdrFallback(update) => {
                    let is_current = update.index == self.current_index;
                    if !source_key_matches_index(&self.image_files, update.index, update.source_key)
                    {
                        self.hdr_in_flight_fallback_refinements
                            .remove(&update.index);
                        log::warn!(
                            "[App] HDR SDR fallback discarded (source key mismatch): index={} generation={}",
                            update.index,
                            update.generation
                        );
                        continue;
                    }
                    if should_yield_background_result_for_pending_transition(
                        is_current,
                        self.pending_transition_target,
                        self.current_index,
                    ) {
                        preload_debug!(
                            "[PreloadDebug] yield hdr_sdr_fallback: idx={} current={} gen={} reason=pending_transition_target",
                            update.index,
                            self.current_index,
                            update.generation,
                        );
                        yielded_background_outputs.push(LoaderOutput::HdrSdrFallback(update));
                        continue;
                    }
                    if should_yield_background_result_for_post_transition_refinement(
                        is_current,
                        self.transition_settled_at,
                        current_refinement_pending,
                    ) {
                        preload_debug!(
                            "[PreloadDebug] yield hdr_sdr_fallback: idx={} current={} gen={} reason=current_refinement_pending",
                            update.index,
                            self.current_index,
                            update.generation,
                        );
                        yielded_background_outputs.push(LoaderOutput::HdrSdrFallback(update));
                        continue;
                    }
                    let estimated_sdr_upload_bytes =
                        update.fallback.as_ref().map_or(0, |fallback| {
                            decoded_rgba_bytes(fallback.width, fallback.height)
                        });
                    if should_defer_hdr_sdr_fallback_install(
                        is_current,
                        is_transitioning,
                        self.transition_settled_at,
                    ) {
                        preload_debug!(
                            "[PreloadDebug] defer hdr_sdr_fallback: idx={} current={} gen={} reason={}",
                            update.index,
                            self.current_index,
                            update.generation,
                            if is_transitioning {
                                "transition"
                            } else {
                                "post_transition_settle"
                            }
                        );
                        self.loader.repush(LoaderOutput::HdrSdrFallback(update));
                        ctx.request_repaint();
                        break;
                    }
                    if estimated_sdr_upload_bytes > 0
                        && !should_upload_sdr_this_frame(
                            is_current,
                            sdr_upload_bytes_this_frame,
                            estimated_sdr_upload_bytes,
                            sdr_upload_budget_bytes_this_frame,
                        )
                    {
                        preload_debug!(
                            "[PreloadDebug] defer hdr_sdr_fallback: idx={} current={} gen={} reason=sdr_upload_budget uploaded_bytes={} candidate_bytes={} budget_bytes={}",
                            update.index,
                            self.current_index,
                            update.generation,
                            sdr_upload_bytes_this_frame,
                            estimated_sdr_upload_bytes,
                            sdr_upload_budget_bytes_this_frame
                        );
                        self.loader.repush(LoaderOutput::HdrSdrFallback(update));
                        ctx.request_repaint();
                        break;
                    }
                    if !is_current && uploads_this_frame >= background_upload_quota {
                        preload_debug!(
                            "[PreloadDebug] defer hdr_sdr_fallback: idx={} current={} gen={} reason=global_upload_quota uploads_this_frame={} quota={}",
                            update.index,
                            self.current_index,
                            update.generation,
                            uploads_this_frame,
                            background_upload_quota
                        );
                        self.loader.repush(LoaderOutput::HdrSdrFallback(update));
                        ctx.request_repaint();
                        break;
                    }
                    if estimated_sdr_upload_bytes > 0
                        && should_space_background_upload_after_transition(
                            is_current,
                            self.transition_settled_at,
                            self.last_background_upload_at,
                        )
                    {
                        preload_debug!(
                            "[PreloadDebug] defer hdr_sdr_fallback: idx={} current={} gen={} reason=post_transition_spacing",
                            update.index,
                            self.current_index,
                            update.generation,
                        );
                        self.loader.repush(LoaderOutput::HdrSdrFallback(update));
                        ctx.request_repaint_after(std::time::Duration::from_millis(16));
                        break;
                    }
                    preload_debug!(
                        "[PreloadDebug] install hdr_sdr_fallback: idx={} current={} gen={} is_current={} estimated_sdr_upload_bytes={} uploads_before={} uploaded_bytes_before={}",
                        update.index,
                        self.current_index,
                        update.generation,
                        is_current,
                        estimated_sdr_upload_bytes,
                        uploads_this_frame,
                        sdr_upload_bytes_this_frame
                    );
                    self.hdr_in_flight_fallback_refinements
                        .remove(&update.index);
                    if is_current {
                        current_refinement_pending = false;
                    }
                    self.handle_hdr_sdr_fallback_update(update, ctx);
                    uploads_this_frame += 1;
                    if !is_current && estimated_sdr_upload_bytes > 0 {
                        self.last_background_upload_at = Some(Instant::now());
                    }
                    sdr_upload_bytes_this_frame =
                        sdr_upload_bytes_this_frame.saturating_add(estimated_sdr_upload_bytes);
                    if should_request_repaint_for_asset_update(
                        AssetUpdateKind::ImageLoaded,
                        is_current,
                        false,
                    ) {
                        ctx.request_repaint();
                    }
                }
            }

            // Secondary quota check after each processed item.
            if uploads_this_frame >= background_upload_quota {
                ctx.request_repaint();
                break;
            }
        }

        for output in yielded_background_outputs {
            self.loader.repush_back(output);
        }
        self.try_start_pending_transition_if_ready();
    }

    fn current_image_is_render_ready(&self) -> bool {
        target_is_render_ready(
            self.texture_cache.contains(self.current_index),
            self.current_hdr_image
                .as_ref()
                .is_some_and(|current| current.image_for_index(self.current_index).is_some())
                || self.hdr_image_cache.contains_key(&self.current_index)
                || self
                    .hdr_tiled_source_cache
                    .contains_key(&self.current_index),
            self.hdr_placeholder_fallback_indices
                .contains(&self.current_index),
        )
    }

    /// Upload deferred CPU pixels and start any pending transition before drawing.
    ///
    /// Navigation can happen after `process_loaded_images` in the same frame (keyboard in
    /// `update()`, pointer hotkeys at the start of `draw_image_canvas_ui`). Without this,
    /// preloaded PNG/JPG may sit in `deferred_sdr_uploads` for one extra frame and flash the
    /// canvas background between the hold frame and the new texture.
    pub(crate) fn prepare_display_frame(&mut self, ctx: &egui::Context) {
        self.flush_deferred_sdr_upload_for_current(ctx);
        self.try_start_pending_transition_if_ready();
    }

    pub(crate) fn try_start_pending_transition_if_ready(&mut self) {
        // Run after loader output processing (or deferred GPU upload) so we don't render one
        // static frame between "texture became ready" and "transition started".
        if !can_start_pending_transition(
            self.pending_transition_target,
            self.current_index,
            self.current_image_is_render_ready(),
        ) {
            return;
        }
        if self.active_transition != TransitionStyle::None {
            self.transition_start =
                Some(Instant::now() - transition_preroll_duration(self.settings.transition_ms));
        } else {
            // No-transition mode uses `prev_texture` only as a one-frame safety net while
            // waiting for the target texture. Once current texture is ready, release it
            // immediately instead of keeping an extra stale handle until next navigation.
            self.prev_texture = None;
            self.prev_hdr_image = None;
            self.prev_transition_rect = None;
        }
        self.pending_transition_target = None;
    }

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
    fn handle_image_load_result(
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
                self.raw_osd_by_index.insert(idx, osd.clone());
                if idx == self.current_index {
                    self.invalidate_osd();
                }
            }
        } else {
            if self.raw_osd_by_index.remove(&idx).is_some() && idx == self.current_index {
                self.invalidate_osd();
            }
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
