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

/// Returns the [`Path`] at `files[idx]`, or an empty path when `idx` is out of bounds.
fn path_or_empty<'a>(files: &'a [std::path::PathBuf], idx: usize) -> &'a std::path::Path {
    files
        .get(idx)
        .map(|p| p.as_path())
        .unwrap_or(std::path::Path::new(""))
}

/// Format a stable texture name into a stack buffer to avoid a per-upload `String` allocation.
/// Prefix plus `usize::MAX` decimal digits fits comfortably in 64 bytes.
fn write_preview_name<'a>(prefix: &str, idx: usize, buf: &'a mut [u8; 64]) -> &'a str {
    let mut cursor = std::io::Cursor::new(&mut buf[..]);
    use std::io::Write;
    let _ = std::write!(cursor, "{prefix}{idx}");
    let len = cursor.position() as usize;
    std::str::from_utf8(&buf[..len]).expect("preview name is ASCII prefix and digits")
}

impl ImageViewerApp {
    /// True when the active tile pyramid belongs to the image at [`Self::current_index`].
    pub(crate) fn tiled_canvas_matches_current_index(&self) -> bool {
        self.tile_manager
            .as_ref()
            .is_some_and(|tm| tm.image_index == self.current_index)
    }

    /// Image pixel size used for layout, zoom OSD, and zoom ceilings.
    ///
    /// When the tiled canvas is active this is always the document / full pyramid size
    /// (`TileManager::full_width` x `full_height`), never the HQ preview texture size.
    /// Preview textures are drawn with UV 0-1 into a dest sized for the full document; using
    /// preview dimensions for FitToWindow zoom math would make OSD 2000% mean only ~preview-relative
    /// magnification while Original Size (zoom_factor-based) still reaches true 2000% of full pixels.
    pub(crate) fn layout_image_resolution(&self) -> Option<(u32, u32)> {
        if self.tiled_canvas_matches_current_index() {
            self.tile_manager
                .as_ref()
                .map(|tm| (tm.full_width, tm.full_height))
        } else {
            self.current_image_res
        }
    }

    pub(crate) fn invalidate_tile_requests_for_view_change(&mut self) {
        if invalidate_tile_manager_requests_for_view_change(&mut self.tile_manager) {
            self.loader.flush_tile_queue();
        }
    }

    pub(crate) fn handle_tile_load_result(
        &mut self,
        tile_result: TileResult,
        _ctx: &egui::Context,
    ) {
        // SDR pixels are already in PIXEL_CACHE; HDR pixels are already in the
        // HdrTiledSource cache. Either way, clear the shared pending marker.
        let gate_ctx = self.result_gate_context();
        if let Some(ref mut tm) = self.tile_manager {
            let source_key = crate::loader::source_key_for_path(path_or_empty(
                &self.image_files,
                tile_result.index,
            ));
            let gate = result_gate::gate_tile_result(
                &gate_ctx,
                &tile_result,
                tm.image_index,
                &tm.decode_profile,
                &self.image_files,
                source_key,
                self.loader.is_loading(tile_result.index),
            );
            if gate != result_gate::GateDecision::Accept {
                return;
            }
            if tm.image_index == tile_result.index {
                tm.pending_tiles.remove(&tile_result.pending_key());
                if should_request_repaint_for_asset_update(
                    AssetUpdateKind::TileReady,
                    true,
                    tile_result.should_request_repaint(),
                ) {
                    _ctx.request_repaint();
                }
            }
        }
    }

    pub(crate) fn handle_preview_update(&mut self, update: PreviewResult, ctx: &egui::Context) {
        let gate_ctx = self.result_gate_context();
        let display = self.display_requirements_for_index(update.index);
        let Some(path_for_logs) = self.image_files.get(update.index) else {
            log::warn!(
                "[App] Preview update discarded (index {} out of range; list len {})",
                update.index,
                self.image_files.len()
            );
            return;
        };

        let existing_stage = self
            .tile_manager
            .as_ref()
            .and_then(|tm| {
                tiled_existing_preview_stage(
                    &self.texture_cache,
                    update.index,
                    tm.image_index == update.index && tm.preview_texture.is_some(),
                )
            })
            .or_else(|| {
                self.prefetched_tiles.get(&update.index).and_then(|tm| {
                    tiled_existing_preview_stage(
                        &self.texture_cache,
                        update.index,
                        tm.preview_texture.is_some(),
                    )
                })
            });

        let gate_decision = result_gate::gate_preview_result(
            &gate_ctx,
            &update,
            &self.image_files,
            &display,
            self.loader.is_loading(update.index),
            existing_stage,
        );
        if gate_decision != result_gate::GateDecision::Accept {
            let file_name = path_for_logs
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown");
            #[cfg(feature = "preload-debug")]
            {
                let gate_label = match gate_decision {
                    result_gate::GateDecision::Accept => "accept",
                    result_gate::GateDecision::Discard => "discard",
                    result_gate::GateDecision::Requeue => "requeue",
                };
                let incoming_stage = update.preview_bundle.stage();
                let hdr_dims = update.preview_bundle.hdr().map(|h| (h.width, h.height));
                let sdr_dims = update.preview_bundle.sdr().map(|s| (s.width, s.height));
                crate::preload_debug!(
                    "[PreloadDebug][Gate] preview {} idx={} stage={:?} existing_stage={:?} is_loading={} hdr_dims={:?} sdr_dims={:?} epoch={} current={} path={}",
                    gate_label,
                    update.index,
                    incoming_stage,
                    existing_stage,
                    self.loader.is_loading(update.index),
                    hdr_dims,
                    sdr_dims,
                    update.decode_profile.profile_epoch,
                    self.current_index,
                    file_name,
                );
            }
            log::warn!(
                "[App] [{}] Preview update discarded (result gate): idx={}",
                file_name,
                update.index
            );
            return;
        }

        if let Some(osd) = &update.raw_bootstrap_osd {
            self.set_raw_metadata_for_index(update.index, Some(osd.clone()), ctx);
            if matches!(
                osd.render_pixels,
                crate::loader::RawRenderPixels::FullDevelop { .. }
            ) {
                self.clear_cpu_raw_refinement_pending(update.index);
            }
        }

        if update.preview_bundle.hdr().is_some() {
            self.cache_hdr_tiled_preview(update.index, update.preview_bundle.hdr().cloned());
            if update.preview_bundle.stage() == crate::loader::PreviewStage::Refined {
                self.hq_tiled_preview_pending_indices.remove(&update.index);
            }
            // Native HDR still tone-maps a SDR bootstrap for TileManager / texture_cache while
            // HDR tiles arrive (`TiledHdrWithSdrPreviewFallback`). This is not a parallel SDR
            // display pipeline -- see `HdrRenderOutputMode` docs / review-checklist #26.
            if crate::loader::output_mode_is_hdr(display.output_mode) {
                match self.try_upload_tiled_sdr_from_hdr_cache(
                    update.index,
                    ctx,
                    update.preview_bundle.stage(),
                    &update.decode_profile,
                ) {
                    TiledSdrFromHdrCacheOutcome::Failed => {
                        log::warn!(
                            "[App] HDR tiled tone-map failed after preview update for index {}",
                            update.index
                        );
                    }
                    TiledSdrFromHdrCacheOutcome::Uploaded
                    | TiledSdrFromHdrCacheOutcome::AlreadyCached => {}
                }
            }
            if should_request_repaint_for_asset_update(
                AssetUpdateKind::PreviewUpgraded,
                update.index == self.current_index,
                false,
            ) {
                ctx.request_repaint();
            }
        }

        // Apply HQ preview if it matches the currently displayed tile manager.
        // Also check prefetched tiles and update the texture cache for future navigations.
        let preview = update.preview_bundle.sdr().cloned();
        let preview_error = update.error.clone();
        let current_tile_profile = self.decode_profile_for_index(update.index);
        match (preview, preview_error) {
            (Some(preview), _) => {
                self.cache_directory_tree_strip_thumbnail(
                    crate::app::directory_tree_strip_cache::StripThumbnailCacheRequest {
                        index: update.index,
                        job_key: None,
                        decoded: &preview,
                        stage: crate::loader::PreviewStage::Refined,
                        logical_size: self.directory_tree_strip_logical_size(update.index),
                        buffer_tag: crate::app::directory_tree_strip_cache::StripPreviewBufferTag::MainWindowTiledPreview,
                        strip_max_side_used: None,
                        ctx,
                        bypass_detach_queue: false,
                    },
                );
                self.upload_static_raw_gpu_bootstrap_preview_if_needed(update.index, &preview, ctx);
                if let Some(cpu_ms) = update.cpu_demosaic_ms
                    && self.raw_metadata.set_cpu_demosaic_ms(update.index, cpu_ms)
                    && update.index == self.current_index
                {
                    self.osd.sync_events();
                    ctx.request_repaint();
                }
                // 1. Update current TileManager
                let mut synced_current_dims: Option<(u32, u32)> = None;
                if let Some(ref mut tm) = self.tile_manager
                    && refined_preview_applies_to_tile_manager(tm, &update, &display)
                {
                    if update.decode_profile != tm.decode_profile {
                        tm.decode_profile = current_tile_profile.clone();
                    }
                    let dims_changed = tm.sync_dimensions_from_source();
                    log::debug!(
                        "[App] HQ preview applied for current index {} ({}x{})",
                        update.index,
                        preview.width,
                        preview.height
                    );
                    if update.preview_bundle.stage() == crate::loader::PreviewStage::Refined
                        && (tm.preview_texture.is_some() || dims_changed)
                    {
                        crate::tile_cache::PIXEL_CACHE
                            .write()
                            .remove_image(update.index);
                        tm.pending_tiles.clear();
                        tm.drop_gpu_tiles();
                    }
                    tm.set_preview(preview.clone(), ctx);
                    if dims_changed {
                        synced_current_dims = Some((tm.full_width, tm.full_height));
                    }
                    if should_request_repaint_for_asset_update(
                        AssetUpdateKind::PreviewUpgraded,
                        true,
                        false,
                    ) {
                        ctx.request_repaint();
                    }
                }
                if let Some((fw, fh)) = synced_current_dims {
                    self.set_current_image_resolution(Some((fw, fh)));
                    crate::tile_cache::set_tile_size_for_image(fw, fh);
                }
                if update.preview_bundle.stage() == crate::loader::PreviewStage::Refined
                    && self.tile_manager.as_ref().is_some_and(|tm| {
                        refined_preview_applies_to_tile_manager(tm, &update, &display)
                    })
                {
                    self.clear_cpu_raw_refinement_pending(update.index);
                }

                // 2. Update prefetched TileManagers
                if let Some(tm) = self.prefetched_tiles.get_mut(&update.index)
                    && refined_preview_applies_to_tile_manager(tm, &update, &display)
                {
                    if update.decode_profile != tm.decode_profile {
                        tm.decode_profile = current_tile_profile.clone();
                    }
                    let _ = tm.sync_dimensions_from_source();
                    log::debug!(
                        "[App] HQ preview applied for prefetched index {} ({}x{})",
                        update.index,
                        preview.width,
                        preview.height
                    );
                    tm.set_preview(preview.clone(), ctx);
                }
                self.hq_tiled_preview_pending_indices.remove(&update.index);

                // 3. Update global texture cache
                let preview_targets_tiled_canvas =
                    self.prefetched_tiles.contains_key(&update.index)
                        || self
                            .tile_manager
                            .as_ref()
                            .is_some_and(|tm| tm.image_index == update.index);
                if preview_targets_tiled_canvas
                    && !self.index_uses_animated_pipeline(update.index)
                    && should_cache_tiled_sdr_preview(
                        self.texture_cache.contains(update.index),
                        self.texture_cache.needs_tile_manager(update.index),
                        self.texture_cache.cached_buffer_tag(update.index),
                        self.texture_cache.cached_preview_stage(update.index),
                        update.effective_sdr_texture_tag(),
                        update.preview_bundle.stage(),
                    )
                {
                    let (orig_w, orig_h) = self
                        .texture_cache
                        .get_original_res(update.index)
                        .unwrap_or((preview.width, preview.height));

                    let name = self.texture_cache.get_or_create_preview_name(update.index);
                    let color_image = egui::ColorImage::from_rgba_unmultiplied(
                        [preview.width as usize, preview.height as usize],
                        preview.rgba(),
                    );
                    let handle = ctx.load_texture(name, color_image, egui::TextureOptions::LINEAR);
                    let texture_tag = update.effective_sdr_texture_tag();
                    let texture_stage = update.preview_bundle.stage();
                    self.insert_texture_cache_tracked(
                        update.index,
                        handle,
                        crate::loader::TextureCacheInsert {
                            orig_w,
                            orig_h,
                            needs_tile_manager: true,
                            buffer_tag: texture_tag,
                            stage: texture_stage,
                            current_index: self.current_index,
                            total_count: self.image_files.len(),
                        },
                    );
                }
            }
            (None, Some(error)) => {
                // Async tiled decode failures (e.g. PSD v1 composite) arrive as
                // error-only PreviewResult; surface them like a main load failure.
                self.install_image_error(update.index, &error);
                if should_request_repaint_for_asset_update(
                    AssetUpdateKind::ImageLoaded,
                    update.index == self.current_index,
                    false,
                ) {
                    ctx.request_repaint();
                }
            }
            (None, None) => {
                if update.preview_bundle.hdr().is_some() {
                    log::debug!(
                        "Preview update for index {} is HDR-only (no SDR plane)",
                        update.index
                    );
                } else {
                    log::warn!(
                        "Preview update for index {} carried no SDR preview plane",
                        update.index
                    );
                }
            }
        }
    }

    /// Promote cached HQ preview into the active tile manager and re-trigger HQ generation when
    /// only bootstrap remains (e.g. after prefetch eviction discarded an HQ update).
    pub(crate) fn sync_and_ensure_hq_tiled_preview(&mut self, idx: usize, ctx: &egui::Context) {
        let tm_max = self
            .tile_manager
            .as_ref()
            .filter(|tm| tm.image_index == idx)
            .and_then(|tm| {
                tm.preview_texture.as_ref().map(|h| {
                    let s = h.size();
                    s[0].max(s[1]) as u32
                })
            });
        let cached_max = self.texture_cache.cached_preview_max_side(idx);
        let _cached_tag = self.texture_cache.cached_buffer_tag(idx);
        let _cached_stage = self.texture_cache.cached_preview_stage(idx);

        if cached_max.is_some_and(|c| tm_max.is_none_or(|t| c > t))
            && let Some(handle) = self.texture_cache.get(idx)
            && let Some(tm) = self
                .tile_manager
                .as_mut()
                .filter(|tm| tm.image_index == idx)
        {
            tm.preview_texture = Some(handle.clone());
            if should_request_repaint_for_asset_update(
                AssetUpdateKind::PreviewUpgraded,
                true,
                false,
            ) {
                ctx.request_repaint();
            }
        }

        let hq_requirement_met = self.tiled_hq_preview_requirement_met(idx);
        let tile_manager_ready = self
            .tile_manager
            .as_ref()
            .is_some_and(|tm| tm.image_index == idx);
        if hq_requirement_met {
            if self.index_requires_tile_manager(idx)
                && !tile_manager_ready
                && idx == self.current_index
                && !self.loader.is_loading(idx)
            {
                crate::preload_debug!(
                    "[PreloadDebug][SyncHq] reload idx={} reason=hq_cache_without_tile_manager tag={:?} stage={:?} tm_max={:?} current={}",
                    idx,
                    _cached_tag,
                    _cached_stage,
                    tm_max,
                    self.current_index,
                );
                self.loader.request_load(
                    idx,
                    self.image_files[idx].clone(),
                    self.settings.raw_high_quality,
                    self.raw_demosaic_mode_for_index(idx),
                    self.settings.psd_hidden_layer_strategy,
                );
            } else {
                crate::preload_debug!(
                    "[PreloadDebug][SyncHq] skip idx={} reason=hq_requirement_met tag={:?} stage={:?} tm_max={:?} current={}",
                    idx,
                    _cached_tag,
                    _cached_stage,
                    tm_max,
                    self.current_index,
                );
            }
            self.hq_tiled_preview_pending_indices.remove(&idx);
            return;
        }

        if self.loader.is_loading(idx) {
            crate::preload_debug!(
                "[PreloadDebug][SyncHq] defer idx={} reason=loader_inflight current={}",
                idx,
                self.current_index,
            );
            return;
        }
        if self.hq_tiled_preview_pending_indices.contains(&idx) {
            crate::preload_debug!(
                "[PreloadDebug][SyncHq] defer idx={} reason=hq_pending current={}",
                idx,
                self.current_index,
            );
            return;
        }

        let Some(tm) = self
            .tile_manager
            .as_ref()
            .filter(|tm| tm.image_index == idx)
        else {
            crate::preload_debug!(
                "[PreloadDebug][SyncHq] skip idx={} reason=no_tile_manager current={}",
                idx,
                self.current_index,
            );
            return;
        };
        let source = tm.get_source();
        if source.defers_loader_hq_preview() {
            crate::preload_debug!(
                "[PreloadDebug][SyncHq] skip idx={} reason=async_raw_refinement current={}",
                idx,
                self.current_index,
            );
            return;
        }

        let profile = self.decode_profile_for_index(idx);
        let source_key = crate::loader::source_key_for_path(path_or_empty(&self.image_files, idx));
        let display = self.display_requirements_for_index(idx);
        if crate::loader::output_mode_is_hdr(display.output_mode) {
            if self.hdr_tiled_preview_cache.contains_key(&idx) {
                crate::preload_debug!(
                    "[PreloadDebug][SyncHq] tone_map_sdr idx={} tag={:?} stage={:?} tm_max={:?} epoch={} current={}",
                    idx,
                    _cached_tag,
                    _cached_stage,
                    tm_max,
                    profile.profile_epoch,
                    self.current_index,
                );
                match self.try_upload_tiled_sdr_from_hdr_cache(
                    idx,
                    ctx,
                    crate::loader::PreviewStage::Refined,
                    &profile,
                ) {
                    TiledSdrFromHdrCacheOutcome::Uploaded
                    | TiledSdrFromHdrCacheOutcome::AlreadyCached => {
                        self.hq_tiled_preview_pending_indices.remove(&idx);
                        return;
                    }
                    TiledSdrFromHdrCacheOutcome::Failed => {
                        crate::preload_debug!(
                            "[PreloadDebug][SyncHq] tone_map_failed idx={} will_try_hdr_refine current={}",
                            idx,
                            self.current_index,
                        );
                    }
                }
            }
            if let Some(hdr_source) = self.hdr_tiled_source_cache.get(&idx).cloned() {
                self.hq_tiled_preview_pending_indices.insert(idx);
                crate::preload_debug!(
                    "[PreloadDebug][SyncHq] trigger_hdr_refine idx={} tag={:?} stage={:?} tm_max={:?} epoch={} current={}",
                    idx,
                    _cached_tag,
                    _cached_stage,
                    tm_max,
                    profile.profile_epoch,
                    self.current_index,
                );
                self.loader
                    .trigger_hq_tiled_hdr_preview(idx, hdr_source, profile, source_key);
                log::debug!(
                    "[App] Triggered on-demand HQ HDR tiled preview for idx={}",
                    idx
                );
                return;
            }
        }
        self.hq_tiled_preview_pending_indices.insert(idx);
        crate::preload_debug!(
            "[PreloadDebug][SyncHq] trigger_on_demand idx={} tag={:?} stage={:?} tm_max={:?} epoch={} current={}",
            idx,
            _cached_tag,
            _cached_stage,
            tm_max,
            profile.profile_epoch,
            self.current_index,
        );
        self.loader
            .trigger_hq_tiled_sdr_preview(idx, source, profile, source_key);
        log::debug!("[App] Triggered on-demand HQ tiled preview for idx={}", idx);
    }

    /// Tone-map a cached HDR tiled preview to SDR and install it on tile managers / texture cache.
    pub(super) fn try_upload_tiled_sdr_from_hdr_cache(
        &mut self,
        idx: usize,
        ctx: &egui::Context,
        preview_stage: crate::loader::PreviewStage,
        decode_profile: &crate::loader::DecodeProfile,
    ) -> TiledSdrFromHdrCacheOutcome {
        let Some(hdr) = self.hdr_tiled_preview_cache.get(&idx).cloned() else {
            return TiledSdrFromHdrCacheOutcome::Failed;
        };
        let texture_tag = tiled_sdr_texture_tag_for_stage(preview_stage);
        let needs_tile_manager =
            self.texture_cache.needs_tile_manager(idx) || self.index_requires_tile_manager(idx);
        if !should_cache_tiled_sdr_preview(
            self.texture_cache.contains(idx),
            needs_tile_manager,
            self.texture_cache.cached_buffer_tag(idx),
            self.texture_cache.cached_preview_stage(idx),
            texture_tag,
            preview_stage,
        ) {
            return TiledSdrFromHdrCacheOutcome::AlreadyCached;
        }

        let exposure = self.effective_hdr_tone_map_settings().exposure_ev;
        let pixels = match self.with_loader_preview_tone_map_gpu(|| {
            crate::hdr::renderer::hdr_to_sdr_rgba8_for_preview(&hdr, exposure)
        }) {
            Ok(pixels) => pixels,
            Err(err) => {
                log::error!(
                    "[App] HDR tiled preview tone-map failed for index {}: {err}",
                    idx
                );
                return TiledSdrFromHdrCacheOutcome::Failed;
            }
        };
        let preview = DecodedImage::new(hdr.width, hdr.height, pixels);
        self.apply_tiled_sdr_preview_upload(
            idx,
            ctx,
            preview,
            texture_tag,
            preview_stage,
            decode_profile,
        );
        TiledSdrFromHdrCacheOutcome::Uploaded
    }

    fn apply_tiled_sdr_preview_upload(
        &mut self,
        idx: usize,
        ctx: &egui::Context,
        preview: DecodedImage,
        texture_tag: crate::loader::TexturePreviewBufferTag,
        texture_stage: crate::loader::PreviewStage,
        decode_profile: &crate::loader::DecodeProfile,
    ) {
        let display = self.display_requirements_for_index(idx);
        let current_tile_profile = self.decode_profile_for_index(idx);
        if texture_stage == crate::loader::PreviewStage::Refined {
            self.cache_directory_tree_strip_thumbnail(
                crate::app::directory_tree_strip_cache::StripThumbnailCacheRequest {
                    index: idx,
                    job_key: None,
                    decoded: &preview,
                    stage: crate::loader::PreviewStage::Refined,
                    logical_size: self.directory_tree_strip_logical_size(idx),
                    buffer_tag:
                        crate::app::directory_tree_strip_cache::StripPreviewBufferTag::MainWindowTiledPreview,
                    strip_max_side_used: None,
                    ctx,
                    bypass_detach_queue: false,
                },
            );
        }
        self.upload_static_raw_gpu_bootstrap_preview_if_needed(idx, &preview, ctx);

        let should_clear_raw_pending = texture_stage == crate::loader::PreviewStage::Refined
            && self.tile_manager.as_ref().is_some_and(|tm| {
                tiled_sdr_preview_applies_to_manager(
                    tm,
                    idx,
                    decode_profile,
                    texture_stage,
                    &display,
                ) && tm.preview_texture.is_some()
            });

        if let Some(ref mut tm) = self.tile_manager
            && tiled_sdr_preview_applies_to_manager(
                tm,
                idx,
                decode_profile,
                texture_stage,
                &display,
            )
        {
            if decode_profile != &tm.decode_profile {
                tm.decode_profile = current_tile_profile.clone();
            }
            log::debug!(
                "[App] Tone-mapped tiled preview applied for current index {} ({}x{})",
                idx,
                preview.width,
                preview.height
            );
            if texture_stage == crate::loader::PreviewStage::Refined && tm.preview_texture.is_some()
            {
                crate::tile_cache::PIXEL_CACHE.write().remove_image(idx);
                tm.pending_tiles.clear();
                tm.drop_gpu_tiles();
            }
            tm.set_preview(preview.clone(), ctx);
            if should_request_repaint_for_asset_update(
                AssetUpdateKind::PreviewUpgraded,
                true,
                false,
            ) {
                ctx.request_repaint();
            }
        }
        if should_clear_raw_pending {
            self.clear_cpu_raw_refinement_pending(idx);
        }

        if let Some(tm) = self.prefetched_tiles.get_mut(&idx)
            && tiled_sdr_preview_applies_to_manager(
                tm,
                idx,
                decode_profile,
                texture_stage,
                &display,
            )
        {
            if decode_profile != &tm.decode_profile {
                tm.decode_profile = current_tile_profile.clone();
            }
            log::debug!(
                "[App] Tone-mapped tiled preview applied for prefetched index {} ({}x{})",
                idx,
                preview.width,
                preview.height
            );
            tm.set_preview(preview.clone(), ctx);
        }
        self.hq_tiled_preview_pending_indices.remove(&idx);

        let preview_targets_tiled_canvas = self.prefetched_tiles.contains_key(&idx)
            || self
                .tile_manager
                .as_ref()
                .is_some_and(|tm| tm.image_index == idx);
        if preview_targets_tiled_canvas
            && !self.index_uses_animated_pipeline(idx)
            && should_cache_tiled_sdr_preview(
                self.texture_cache.contains(idx),
                self.texture_cache.needs_tile_manager(idx),
                self.texture_cache.cached_buffer_tag(idx),
                self.texture_cache.cached_preview_stage(idx),
                texture_tag,
                texture_stage,
            )
        {
            let name = self
                .texture_cache
                .get_or_create_preview_name(idx)
                .to_owned();
            let (orig_w, orig_h) = self
                .texture_cache
                .get_original_res(idx)
                .unwrap_or((preview.width, preview.height));

            let color_image = egui::ColorImage::from_rgba_unmultiplied(
                [preview.width as usize, preview.height as usize],
                preview.rgba(),
            );
            let handle = ctx.load_texture(name, color_image, egui::TextureOptions::LINEAR);
            self.insert_texture_cache_tracked(
                idx,
                handle,
                crate::loader::TextureCacheInsert {
                    orig_w,
                    orig_h,
                    needs_tile_manager: true,
                    buffer_tag: texture_tag,
                    stage: texture_stage,
                    current_index: self.current_index,
                    total_count: self.image_files.len(),
                },
            );
        }
    }

    pub(crate) fn log_large_image(&self, idx: usize, w: u32, h: u32) {
        let Some(path) = self.image_files.get(idx) else {
            log::debug!(
                "[App] Skipped large-image log (index {}, {}×{}) — file list shorter than index",
                idx,
                w,
                h
            );
            return;
        };
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");
        log::info!(
            "[{}] Large image detected: {}x{} ({:.1} MP) — tiled mode active",
            file_name,
            w,
            h,
            (w as f64 * h as f64) / 1_000_000.0
        );
    }

    pub(crate) fn setup_tile_manager(
        &self,
        ctx: &egui::Context,
        idx: usize,
        tm: &mut TileManager,
        preview: DecodedImage,
    ) {
        let preview_img = egui::ColorImage::from_rgba_unmultiplied(
            [preview.width as usize, preview.height as usize],
            preview.rgba(),
        );
        let mut name_buf = [0u8; 64];
        let preview_handle = ctx.load_texture(
            write_preview_name("preview_", idx, &mut name_buf),
            preview_img,
            egui::TextureOptions::LINEAR,
        );
        tm.preview_texture = Some(preview_handle);
    }

    pub(super) fn upload_static_raw_gpu_bootstrap_preview_if_needed(
        &mut self,
        idx: usize,
        preview: &DecodedImage,
        ctx: &egui::Context,
    ) {
        if self.hdr_image_cache.contains_key(&idx) {
            return;
        }
        if self.texture_cache.contains(idx) && !self.texture_cache.needs_tile_manager(idx) {
            return;
        }
        self.queue_or_upload_raw_gpu_bootstrap_texture(idx, preview, ctx);
        if idx == self.current_index {
            // Keep full-document resolution when a tile pyramid is already authoritative.
            // Bootstrap preview pixels must not replace layout/OSD zoom math dimensions.
            if !self.tiled_canvas_matches_current_index() {
                self.set_current_image_resolution(Some((preview.width, preview.height)));
            }
            if should_request_repaint_for_asset_update(
                AssetUpdateKind::PreviewUpgraded,
                true,
                false,
            ) {
                ctx.request_repaint();
            }
        }
    }

    pub(super) fn upload_tiled_bootstrap_preview(
        &mut self,
        ctx: &egui::Context,
        idx: usize,
        preview: Option<&DecodedImage>,
        full_width: u32,
        full_height: u32,
    ) {
        let Some(preview) = preview else {
            return;
        };

        if !should_upload_tiled_bootstrap_preview(
            self.texture_cache.contains(idx),
            self.texture_cache.cached_buffer_tag(idx),
            self.texture_cache.cached_preview_stage(idx),
        ) {
            return;
        }

        let color_image = ColorImage::from_rgba_unmultiplied(
            [preview.width as usize, preview.height as usize],
            preview.rgba(),
        );
        let mut name_buf = [0u8; 64];
        let name = write_preview_name("img_preview_", idx, &mut name_buf);
        let handle = ctx.load_texture(name, color_image, TextureOptions::LINEAR);
        self.insert_texture_cache_tracked(
            idx,
            handle,
            crate::loader::TextureCacheInsert {
                orig_w: full_width,
                orig_h: full_height,
                needs_tile_manager: true,
                buffer_tag: crate::loader::TexturePreviewBufferTag::TiledBootstrap,
                stage: crate::loader::PreviewStage::Initial,
                current_index: self.current_index,
                total_count: self.image_files.len(),
            },
        );
    }

    pub(super) fn cache_hdr_tiled_preview(
        &mut self,
        idx: usize,
        preview: Option<Arc<crate::hdr::types::HdrImageBuffer>>,
    ) {
        let Some(path) = self.image_files.get(idx) else {
            log::warn!(
                "[App] Skipped HDR tiled preview cache for index {} (out of range; list len {})",
                idx,
                self.image_files.len()
            );
            return;
        };
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();
        cache_hdr_tiled_preview_state(
            idx,
            self.current_index,
            &mut self.hdr_tiled_preview_cache,
            &mut self.current_hdr_tiled_preview,
            preview,
            &file_name,
        );
    }

    pub(super) fn attach_initial_preview_if_needed(
        &self,
        ctx: &egui::Context,
        idx: usize,
        tm: &mut TileManager,
        preview: Option<&DecodedImage>,
    ) {
        if tm.preview_texture.is_none()
            && let Some(preview) = preview
        {
            self.setup_tile_manager(ctx, idx, tm, preview.clone());
        }
    }
}
