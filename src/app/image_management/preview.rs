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
    /// True when the active tile pyramid belongs to the image at [`Self::current_index`].
    pub(crate) fn tiled_canvas_matches_current_index(&self) -> bool {
        self.tile_manager
            .as_ref()
            .is_some_and(|tm| tm.image_index == self.current_index)
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
            let source_key = crate::loader::source_key_for_path(
                self.image_files
                    .get(tile_result.index)
                    .map(|p| p.as_path())
                    .unwrap_or(std::path::Path::new("")),
            );
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
                    update.index,
                    &preview,
                    crate::loader::PreviewStage::Refined,
                    self.directory_tree_strip_logical_size(update.index),
                    ctx,
                );
                self.upload_static_raw_gpu_bootstrap_preview_if_needed(update.index, &preview, ctx);
                if let Some(cpu_ms) = update.cpu_demosaic_ms {
                    if self.raw_metadata.set_cpu_demosaic_ms(update.index, cpu_ms)
                        && update.index == self.current_index
                    {
                        self.osd.sync_events();
                        ctx.request_repaint();
                    }
                }
                // 1. Update current TileManager
                if let Some(ref mut tm) = self.tile_manager {
                    if refined_preview_applies_to_tile_manager(tm, &update, &display) {
                        if update.decode_profile != tm.decode_profile {
                            tm.decode_profile = current_tile_profile.clone();
                        }
                        log::debug!(
                            "[App] HQ preview applied for current index {} ({}x{})",
                            update.index,
                            preview.width,
                            preview.height
                        );
                        tm.set_preview(preview.clone(), ctx);
                        if should_request_repaint_for_asset_update(
                            AssetUpdateKind::PreviewUpgraded,
                            true,
                            false,
                        ) {
                            ctx.request_repaint();
                        }
                    }
                }

                // 2. Update prefetched TileManagers
                if let Some(tm) = self.prefetched_tiles.get_mut(&update.index) {
                    if refined_preview_applies_to_tile_manager(tm, &update, &display) {
                        if update.decode_profile != tm.decode_profile {
                            tm.decode_profile = current_tile_profile.clone();
                        }
                        log::debug!(
                            "[App] HQ preview applied for prefetched index {} ({}x{})",
                            update.index,
                            preview.width,
                            preview.height
                        );
                        tm.set_preview(preview.clone(), ctx);
                    }
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
                        self.texture_cache.cached_preview_max_side(update.index),
                        preview.width.max(preview.height),
                    )
                {
                    let (orig_w, orig_h) = self
                        .texture_cache
                        .get_original_res(update.index)
                        .unwrap_or((preview.width, preview.height));

                    let name = format!("img_hq_preview_{}", update.index);
                    let color_image = egui::ColorImage::from_rgba_unmultiplied(
                        [preview.width as usize, preview.height as usize],
                        preview.rgba(),
                    );
                    let handle = ctx.load_texture(name, color_image, egui::TextureOptions::LINEAR);
                    if let Some(evicted_idx) = self.texture_cache.insert(
                        update.index,
                        handle,
                        orig_w,
                        orig_h,
                        true, // is_tiled
                        self.current_index,
                        self.image_files.len(),
                    ) {
                        self.handle_texture_cache_eviction(evicted_idx);
                    }
                }
            }
            (None, Some(error)) => {
                log::error!(
                    "Preview update failed for index {}: {}",
                    update.index,
                    error
                );
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

        if cached_max.is_some_and(|c| tm_max.is_none_or(|t| c > t)) {
            if let Some(handle) = self.texture_cache.get(idx) {
                if let Some(tm) = self
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
            }
        }

        let effective_max = self.texture_cache.cached_preview_max_side(idx).or(tm_max);
        let bootstrap_max = crate::constants::DEFAULT_PREVIEW_SIZE;
        let is_bootstrap_only = effective_max.is_none_or(|m| m <= bootstrap_max);
        if !is_bootstrap_only {
            self.hq_tiled_preview_pending_indices.remove(&idx);
            return;
        }

        if self.loader.is_loading(idx) || self.hq_tiled_preview_pending_indices.contains(&idx) {
            return;
        }

        let Some(tm) = self
            .tile_manager
            .as_ref()
            .filter(|tm| tm.image_index == idx)
        else {
            return;
        };
        let source = tm.get_source();
        if source.defers_loader_hq_preview() {
            return;
        }

        let profile = self.decode_profile_for_index(idx);
        let source_key = crate::loader::source_key_for_path(
            self.image_files
                .get(idx)
                .map(|p| p.as_path())
                .unwrap_or(std::path::Path::new("")),
        );
        self.hq_tiled_preview_pending_indices.insert(idx);
        self.loader
            .trigger_hq_tiled_sdr_preview(idx, source, profile, source_key);
        log::debug!("[App] Triggered on-demand HQ tiled preview for idx={}", idx);
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
        let preview_handle = ctx.load_texture(
            format!("preview_{}", idx),
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
            self.set_current_image_resolution(Some((preview.width, preview.height)));
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

        let bootstrap_max = preview.width.max(preview.height);
        if !should_upload_tiled_bootstrap_preview(
            self.texture_cache.contains(idx),
            self.texture_cache.cached_preview_max_side(idx),
            bootstrap_max,
        ) {
            return;
        }

        let color_image = ColorImage::from_rgba_unmultiplied(
            [preview.width as usize, preview.height as usize],
            preview.rgba(),
        );
        let name = format!("img_preview_{}", idx);
        let handle = ctx.load_texture(name, color_image, TextureOptions::LINEAR);
        if let Some(evicted_idx) = self.texture_cache.insert(
            idx,
            handle,
            full_width,
            full_height,
            true,
            self.current_index,
            self.image_files.len(),
        ) {
            self.handle_texture_cache_eviction(evicted_idx);
        }
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
        if tm.preview_texture.is_none() {
            if let Some(preview) = preview {
                self.setup_tile_manager(ctx, idx, tm, preview.clone());
            }
        }
    }
}
