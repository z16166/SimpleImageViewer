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

/// Max frames to retry HDR pre-upload registration while callback prewarm is pending.
/// ~30 frames at 60 Hz ≈ 500 ms before abandoning pre-uploaded planes.
const MAX_HDR_REGISTER_PREWARM_REPUSH: u8 = 30;

impl ImageViewerApp {
    /// Upload deferred animation frames to GPU (max 8 per call). Runs even while scanning.
    pub(crate) fn process_pending_animation_uploads(&mut self, ctx: &egui::Context) {
        if self.pending_anim_frames.is_empty() {
            return;
        }

        const ANIM_UPLOAD_QUOTA: usize = 8;
        let is_transitioning = self.transition_start.is_some();
        let pending_idx = super::super::prefetch_animation_upload_index(
            &self.pending_anim_frames,
            self.current_index,
        );
        let defer_pending_animation_upload = pending_idx.is_some_and(|idx| {
            should_defer_background_upload_during_transition(
                idx == self.current_index,
                is_transitioning,
                self.transition_settled_at,
            )
        });
        #[cfg(feature = "preload-debug")]
        if defer_pending_animation_upload && let Some(idx) = pending_idx {
            if let Some(pending) = self.pending_anim_frames.get(&idx) {
                preload_debug!(
                    "[PreloadDebug] defer pending animation upload: idx={} current={} next_frame={} total_frames={} reason=transition",
                    pending.image_index,
                    self.current_index,
                    pending.next_frame,
                    pending.frames.len()
                );
            }
        }
        if !defer_pending_animation_upload && let Some(pending_idx) = pending_idx {
            let mut uploaded = 0;
            let mut finished = false;
            if let Some(pending) = self.pending_anim_frames.get_mut(&pending_idx) {
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
                finished = pending.next_frame >= pending.frames.len();
            }

            if pending_idx == self.current_index {
                self.ensure_current_animation_playback();
            }

            if finished {
                if let Some(pending) = self.pending_anim_frames.remove(&pending_idx) {
                    let idx = pending.image_index;

                    let playback = AnimationPlayback {
                        image_index: idx,
                        textures: pending.textures,
                        hdr_frames: pending.hdr_frames.clone(),
                        delays: pending.delays,
                        current_frame: 0,
                        frame_start: Instant::now(),
                        cpu_frames: Some(pending.frames.iter().map(|f| f.arc_pixels()).collect()),
                    };

                    if idx == self.current_index {
                        if let Some(hdr_frames) = &playback.hdr_frames {
                            if let Some(hdr) = hdr_frames.first() {
                                self.current_hdr_image =
                                    Some(crate::app::CurrentHdrImage::new(idx, Arc::clone(hdr)));
                            }
                        }
                        self.tile_manager = None;
                        self.animation = Some(AnimationPlayback {
                            image_index: playback.image_index,
                            textures: playback.textures.clone(),
                            hdr_frames: playback.hdr_frames.clone(),
                            delays: playback.delays.clone(),
                            current_frame: 0,
                            frame_start: Instant::now(),
                            cpu_frames: playback.cpu_frames.clone(),
                        });
                    }
                    self.animation_cache.insert(idx, playback);
                }
            } else if self.pending_anim_frames.contains_key(&pending_idx) {
                ctx.request_repaint();
            }
        }
    }

    /// Process results from the background ImageLoader.
    pub(crate) fn process_loaded_images(
        &mut self,
        ctx: &egui::Context,
        frame: &mut Option<&mut eframe::Frame>,
    ) {
        #[cfg(feature = "preload-debug")]
        let loaded_started = std::time::Instant::now();
        self.flush_deferred_sdr_upload_for_current(ctx);
        self.process_pending_animation_uploads(ctx);
        if self.scanning {
            #[cfg(feature = "preload-debug")]
            crate::preload_debug!(
                "[PreloadDebug][Scan] loader poll skipped while scanning frame_ms={}",
                crate::preload_debug::elapsed_ms(loaded_started)
            );
            return;
        }
        let is_transitioning = self.transition_start.is_some();

        // ── 2. Process results from the background ImageLoader ──
        //
        // Profile + retention gate (`result_gate` / `prefetch_retention`):
        // Background `LoaderOutput::Image` within the preload window may install when profile
        // matches display requirements; distant indices survive only while loader in-flight.
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
                LoaderOutput::Image(mut load_result) => {
                    let idx = load_result.index;
                    let is_current = idx == self.current_index;
                    let gate_ctx = self.result_gate_context();
                    let display = self.display_requirements_for_index(idx);
                    let gate_decision = result_gate::gate_load_result(
                        &gate_ctx,
                        &load_result,
                        &self.image_files,
                        &display,
                        self.loader.is_loading(idx),
                    );
                    match gate_decision {
                        result_gate::GateDecision::Requeue => {
                            self.loader.finish_image_request(idx);
                            if self.loader.try_note_capacity_requeue(idx)
                                && !self.hdr_image_cache.contains_key(&idx)
                                && !self.loader.is_loading(idx)
                                && !self.image_files.is_empty()
                                && idx < self.image_files.len()
                            {
                                self.loader.request_load(
                                    idx,
                                    self.image_files[idx].clone(),
                                    self.settings.raw_high_quality,
                                    self.raw_demosaic_mode_for_index(idx),
                                );
                            }
                            continue;
                        }
                        result_gate::GateDecision::Discard => {
                            #[cfg(feature = "preload-debug")]
                            preload_debug!(
                                "[PreloadDebug] discard image: idx={} gate={}",
                                idx,
                                result_gate::gate_decision_log_label(gate_decision)
                            );
                            let source_still_valid = result_gate::source_key_matches_index(
                                &self.image_files,
                                idx,
                                load_result.source_key,
                            );
                            self.loader.finish_image_request(idx);
                            if is_current
                                && !self.has_loaded_asset(idx)
                                && source_still_valid
                            {
                                self.sync_loader_preload_plan();
                                self.schedule_current_image_load_if_needed();
                            }
                            continue;
                        }
                        result_gate::GateDecision::Accept => {
                            self.loader.clear_capacity_requeue(idx);
                        }
                    }

                    if !self.try_register_preuploaded_hdr_plane(frame, &mut load_result) {
                        self.loader.repush(LoaderOutput::Image(load_result));
                        ctx.request_repaint();
                        break;
                    }

                    if should_yield_background_result_for_pending_transition(
                        is_current,
                        self.pending_transition_target,
                        self.current_index,
                    ) {
                        preload_debug!(
                            "[PreloadDebug] yield image install: idx={} current={} reason=pending_transition_target",
                            idx,
                            self.current_index,
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
                            "[PreloadDebug] yield image install: idx={} current={} reason=current_refinement_pending",
                            idx,
                            self.current_index,
                        );
                        yielded_background_outputs.push(LoaderOutput::Image(load_result));
                        continue;
                    }

                    if should_defer_background_upload_during_transition(
                        is_current,
                        is_transitioning,
                        self.transition_settled_at,
                    ) {
                        let install_plan = ImageInstallPlan::from_load_result(&load_result);
                        if self.try_install_background_static_hdr_hdr_only(
                            &load_result,
                            &install_plan,
                            if is_transitioning {
                                "transition"
                            } else {
                                "post_transition_settle"
                            },
                            ctx,
                        ) {
                            if should_request_repaint_for_asset_update(
                                AssetUpdateKind::ImageLoaded,
                                false,
                                false,
                            ) {
                                ctx.request_repaint();
                            }
                            continue;
                        }
                        preload_debug!(
                            "[PreloadDebug] defer image install: idx={} current={} reason={}",
                            idx,
                            self.current_index,
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
                        if self.try_install_background_static_hdr_hdr_only(
                            &load_result,
                            &install_plan,
                            "sdr_upload_budget",
                            ctx,
                        ) {
                            if should_request_repaint_for_asset_update(
                                AssetUpdateKind::ImageLoaded,
                                false,
                                false,
                            ) {
                                ctx.request_repaint();
                            }
                            continue;
                        }
                        preload_debug!(
                            "[PreloadDebug] defer image install: idx={} current={} reason=sdr_upload_budget uploaded_bytes={} candidate_bytes={} budget_bytes={}",
                            idx,
                            self.current_index,
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
                        if self.try_install_background_static_hdr_hdr_only(
                            &load_result,
                            &install_plan,
                            "global_upload_quota",
                            ctx,
                        ) {
                            if should_request_repaint_for_asset_update(
                                AssetUpdateKind::ImageLoaded,
                                false,
                                false,
                            ) {
                                ctx.request_repaint();
                            }
                            continue;
                        }
                        preload_debug!(
                            "[PreloadDebug] defer image install: idx={} current={} reason=global_upload_quota uploads_this_frame={} quota={}",
                            idx,
                            self.current_index,
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
                        if self.try_install_background_static_hdr_hdr_only(
                            &load_result,
                            &install_plan,
                            "post_transition_spacing",
                            ctx,
                        ) {
                            if should_request_repaint_for_asset_update(
                                AssetUpdateKind::ImageLoaded,
                                false,
                                false,
                            ) {
                                ctx.request_repaint();
                            }
                            continue;
                        }
                        preload_debug!(
                            "[PreloadDebug] defer image install: idx={} current={} reason=post_transition_spacing",
                            idx,
                            self.current_index,
                        );
                        self.loader.repush(LoaderOutput::Image(load_result));
                        ctx.request_repaint_after(std::time::Duration::from_millis(16));
                        break;
                    }

                    preload_debug!(
                        "[PreloadDebug] install image: idx={} current={} is_current={} estimated_sdr_upload_bytes={} uploads_before={} uploaded_bytes_before={}",
                        idx,
                        self.current_index,
                        is_current,
                        estimated_sdr_upload_bytes,
                        uploads_this_frame,
                        sdr_upload_bytes_this_frame
                    );
                    self.loader.finish_image_request(idx);
                    self.handle_image_load_result(&load_result, install_plan, ctx, false);
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

                    if should_yield_background_result_for_pending_transition(
                        preview_is_current,
                        self.pending_transition_target,
                        self.current_index,
                    ) {
                        preload_debug!(
                            "[PreloadDebug] yield preview update: idx={} current={} reason=pending_transition_target",
                            preview_update.index,
                            self.current_index
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
                            "[PreloadDebug] yield preview update: idx={} current={} reason=current_refinement_pending",
                            preview_update.index,
                            self.current_index
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
                            "[PreloadDebug] defer preview update: idx={} current={} reason=transition",
                            preview_update.index,
                            self.current_index
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
                            "[PreloadDebug] defer preview update: idx={} current={} reason=global_upload_quota uploads_this_frame={} quota={}",
                            preview_update.index,
                            self.current_index,
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
                            "[PreloadDebug] defer preview update: idx={} current={} reason=post_transition_spacing",
                            preview_update.index,
                            self.current_index
                        );
                        self.loader.repush(LoaderOutput::Preview(preview_update));
                        ctx.request_repaint_after(std::time::Duration::from_millis(16));
                        break;
                    }
                    preload_debug!(
                        "[PreloadDebug] install preview: idx={} current={} is_current={} uploads_before={}",
                        preview_update.index,
                        self.current_index,
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

                LoaderOutput::Refined(idx) => {
                    if self
                        .image_files
                        .get(idx)
                        .is_some_and(|p| crate::preload_debug::path_is_raw(p))
                    {
                        crate::preload_debug!(
                            "[PreloadDebug][RAW] refined_notify idx={} current={}",
                            idx,
                            idx == self.current_index
                        );
                    }
                    self.handle_refined_notification(idx, ctx);
                }

                LoaderOutput::HdrSdrFallback(update) => {
                    let is_current = update.index == self.current_index;
                    if !result_gate::source_key_matches_index(
                        &self.image_files,
                        update.index,
                        update.source_key,
                    ) {
                        self.hdr_in_flight_fallback_refinements
                            .remove(&update.index);
                        log::warn!(
                            "[App] HDR SDR fallback discarded (source key mismatch): index={}",
                            update.index
                        );
                        continue;
                    }
                    let display = self.display_requirements_for_index(update.index);
                    if !crate::loader::profile_satisfies_display(&update.decode_profile, &display) {
                        self.hdr_in_flight_fallback_refinements
                            .remove(&update.index);
                        continue;
                    }
                    if should_yield_background_result_for_pending_transition(
                        is_current,
                        self.pending_transition_target,
                        self.current_index,
                    ) {
                        preload_debug!(
                            "[PreloadDebug] yield hdr_sdr_fallback: idx={} current={} reason=pending_transition_target",
                            update.index,
                            self.current_index
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
                            "[PreloadDebug] yield hdr_sdr_fallback: idx={} current={} reason=current_refinement_pending",
                            update.index,
                            self.current_index
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
                            "[PreloadDebug] defer hdr_sdr_fallback: idx={} current={} reason={}",
                            update.index,
                            self.current_index,
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
                            "[PreloadDebug] defer hdr_sdr_fallback: idx={} current={} reason=sdr_upload_budget uploaded_bytes={} candidate_bytes={} budget_bytes={}",
                            update.index,
                            self.current_index,
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
                            "[PreloadDebug] defer hdr_sdr_fallback: idx={} current={} reason=global_upload_quota uploads_this_frame={} quota={}",
                            update.index,
                            self.current_index,
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
                            "[PreloadDebug] defer hdr_sdr_fallback: idx={} current={} reason=post_transition_spacing",
                            update.index,
                            self.current_index
                        );
                        self.loader.repush(LoaderOutput::HdrSdrFallback(update));
                        ctx.request_repaint_after(std::time::Duration::from_millis(16));
                        break;
                    }
                    preload_debug!(
                        "[PreloadDebug] install hdr_sdr_fallback: idx={} current={} is_current={} estimated_sdr_upload_bytes={} uploads_before={} uploaded_bytes_before={}",
                        update.index,
                        self.current_index,
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
        #[cfg(feature = "preload-debug")]
        {
            let frame_ms = crate::preload_debug::elapsed_ms(loaded_started);
            if frame_ms > 16 {
                crate::preload_debug!(
                    "[PreloadDebug] process_loaded_images frame_ms={} current_idx={}",
                    frame_ms,
                    self.current_index
                );
            }
        }
    }

    // Runs on the main thread during the update/event phase (`process_loaded_images`),
    // which completes before egui's paint phase. Prewarm readiness uses a read lock on the
    // steady path; `from_uploaded` runs outside the lock so uniform buffer allocation does
    // not block paint.
    fn try_register_preuploaded_hdr_plane(
        &mut self,
        frame: &mut Option<&mut eframe::Frame>,
        load_result: &mut LoadResult,
    ) -> bool {
        if load_result.uploaded_planes.is_none() {
            return true;
        }

        if load_result.device_id != Some(self.current_device_id) {
            return abandon_preuploaded_planes(load_result);
        }

        let Ok(ImageData::Hdr { ref hdr, .. }) = load_result.result else {
            return abandon_preuploaded_planes(load_result);
        };

        let Some(frame) = frame.as_mut() else {
            return abandon_preuploaded_planes(load_result);
        };
        let Some(wgpu_state) = frame.wgpu_render_state() else {
            return abandon_preuploaded_planes(load_result);
        };
        let Some(format) = self.hdr_callback_prewarm_target_format() else {
            return abandon_preuploaded_planes(load_result);
        };

        let image_key = crate::hdr::renderer::HdrImageKey::from_image(hdr);
        let readiness = {
            let renderer = wgpu_state.renderer.read();
            crate::hdr::renderer::hdr_callback_resources_readiness(
                &renderer.callback_resources,
                format,
            )
        };
        if matches!(
            readiness,
            crate::hdr::renderer::HdrCallbackResourcesReadiness::PrewarmRunning
        ) {
            return self.finish_or_defer_hdr_preupload_registration(load_result);
        }

        if matches!(
            readiness,
            crate::hdr::renderer::HdrCallbackResourcesReadiness::NeedsEnsure
        ) {
            let mut renderer = wgpu_state.renderer.write();
            if !crate::hdr::renderer::ensure_hdr_callback_resources(
                &wgpu_state.device,
                format,
                &mut renderer.callback_resources,
            ) {
                return self.finish_or_defer_hdr_preupload_registration(load_result);
            }
        }

        let upload_device_id = match load_result.device_id {
            Some(id) => id,
            None => return abandon_preuploaded_planes(load_result),
        };
        if upload_device_id != self.current_device_id {
            return abandon_preuploaded_planes(load_result);
        }

        let Some(uploaded) = load_result.uploaded_planes.take() else {
            return true;
        };
        let tone_map = self.effective_hdr_tone_map_settings();
        let output_mode = crate::hdr::monitor::effective_render_output_mode(
            Some(format),
            self.effective_hdr_monitor_selection().as_ref(),
        );
        let binding = crate::hdr::renderer::HdrImageBinding::from_uploaded(
            &wgpu_state.device,
            uploaded,
            hdr,
            tone_map,
            format,
            output_mode,
            upload_device_id,
        );
        let mut renderer = wgpu_state.renderer.write();
        if let Some(resources) = renderer
            .callback_resources
            .get_mut::<crate::hdr::renderer::HdrCallbackResources>()
        {
            if !resources.register_preuploaded_binding(image_key, binding, self.current_device_id) {
                // Device replaced during `from_uploaded`; `prepare()` will bind synchronously.
            }
        }

        self.hdr_register_prewarm_repush_counts
            .remove(&load_result.index);
        true
    }

    fn finish_or_defer_hdr_preupload_registration(&mut self, load_result: &mut LoadResult) -> bool {
        let key = load_result.index;
        let count = self
            .hdr_register_prewarm_repush_counts
            .entry(key)
            .and_modify(|count| *count = count.saturating_add(1))
            .or_insert(1);
        if *count >= MAX_HDR_REGISTER_PREWARM_REPUSH {
            // Safety valve: abandon pre-uploaded planes after ~500 ms of stalled prewarm
            // and let `prepare()` bind synchronously. Dropping here avoids orphan VRAM if
            // shader compilation never completes; `prepare()` will re-upload on cache miss.
            self.hdr_register_prewarm_repush_counts.remove(&key);
            load_result.uploaded_planes = None;
            return true;
        }
        false
    }
}

fn abandon_preuploaded_planes(load_result: &mut LoadResult) -> bool {
    load_result.uploaded_planes = None;
    true
}
