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
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Resolve file indices that should receive a GPU RAW demosaic bake notice.
pub(crate) fn resolve_raw_demosaic_notice_indices(
    notice: &crate::hdr::renderer::RawGpuDemosaicBakedNotice,
    is_failure: bool,
    hdr_image_cache: &HashMap<usize, Arc<crate::hdr::types::HdrImageBuffer>>,
    hdr_raw_gpu_demosaic_pending_indices: &HashSet<usize>,
    hdr_raw_gpu_demosaic_pending_key_index: &HashMap<crate::hdr::renderer::HdrImageKey, usize>,
    current_hdr_image: Option<&crate::app::CurrentHdrImage>,
) -> Vec<usize> {
    let mut matching: Vec<usize> = hdr_image_cache
        .iter()
        .filter_map(|(&idx, hdr)| {
            if crate::hdr::renderer::HdrImageKey::from_image(hdr) == notice.key
                && hdr_raw_gpu_demosaic_pending_indices.contains(&idx)
            {
                Some(idx)
            } else {
                None
            }
        })
        .collect();
    if !matching.is_empty() || !is_failure {
        return matching;
    }
    let mut seen = HashSet::new();
    if let Some(&idx) = hdr_raw_gpu_demosaic_pending_key_index.get(&notice.key) {
        if hdr_raw_gpu_demosaic_pending_indices.contains(&idx) && seen.insert(idx) {
            matching.push(idx);
        }
    }
    for &idx in hdr_raw_gpu_demosaic_pending_indices {
        if seen.contains(&idx) {
            continue;
        }
        let key_matches = hdr_image_cache
            .get(&idx)
            .is_some_and(|hdr| crate::hdr::renderer::HdrImageKey::from_image(hdr) == notice.key)
            || current_hdr_image
                .and_then(|current| current.image_for_index(idx))
                .is_some_and(|hdr| {
                    crate::hdr::renderer::HdrImageKey::from_image(hdr) == notice.key
                });
        if key_matches {
            seen.insert(idx);
            matching.push(idx);
        }
    }
    if matching.is_empty() && hdr_raw_gpu_demosaic_pending_indices.len() == 1 {
        matching.extend(hdr_raw_gpu_demosaic_pending_indices.iter().copied());
    }
    if matching.is_empty() {
        log::warn!(
            "[HDR] Dropping GPU RAW demosaic notice: no pending index matched key {:?} (pending={})",
            notice.key,
            hdr_raw_gpu_demosaic_pending_indices.len()
        );
    }
    matching
}

impl ImageViewerApp {
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
        self.drain_raw_demosaic_baked_notifications(ctx);
        self.flush_deferred_sdr_upload_for_current(ctx);
        self.try_start_pending_transition_if_ready();
    }

    /// Drain GPU demosaic bake notices after the paint pass (prepare may push notices during draw).
    pub(crate) fn finish_display_frame(&mut self, ctx: &egui::Context) {
        self.drain_raw_demosaic_baked_notifications(ctx);
    }

    fn drain_raw_demosaic_baked_notifications(&mut self, ctx: &egui::Context) {
        let baked = std::mem::take(&mut *self.raw_demosaic_baked_notify.lock());
        if baked.is_empty() {
            return;
        }
        let mut cleared_any = false;
        let mut cleared_current = false;
        for notice in baked {
            let is_failure = notice.demosaic_ms == u32::MAX;
            let matching = self.indices_for_raw_demosaic_notice(&notice, is_failure);
            for idx in matching {
                self.hdr_raw_gpu_demosaic_pending_indices.remove(&idx);
                cleared_any = true;
                if idx == self.current_index {
                    cleared_current = true;
                    if !is_failure {
                        self.raw_gpu_demosaic_await_hdr_present = true;
                    }
                }
                if is_failure {
                    log::warn!(
                        "[HDR] GPU RAW demosaicing failed for index {}, falling back to CPU",
                        idx
                    );
                    self.texture_cache.remove(idx);
                    self.raw_metadata.note_gpu_demosaic_failed(idx);
                    self.remove_hdr_image_resources(idx);
                    self.gpu_demosaic_failed_indices.insert(idx);
                    self.prefetched_tiles.remove(&idx);
                    self.deferred_sdr_uploads.remove(&idx);
                    crate::tile_cache::PIXEL_CACHE.lock().remove_image(idx);
                    if let Some(path) = self.image_files.get(idx).cloned() {
                        // Use the app generation so the worker spawn check matches global_gen.
                        // Clear any stale loading-map slot first so should_spawn_load_task accepts
                        // a re-queue at the same generation after the GPU path finished.
                        self.loader.finish_image_request(idx, u64::MAX);
                        self.loader.request_load(
                            idx,
                            self.generation,
                            path,
                            self.settings.raw_high_quality,
                            crate::settings::RawDemosaicMode::Cpu,
                        );
                    }
                } else {
                    self.raw_metadata
                        .set_gpu_demosaic_ms(idx, notice.demosaic_ms);
                    self.raw_metadata.promote_gpu_demosaic_complete(idx);
                }
            }
        }
        if cleared_current {
            self.osd.sync_events();
        }
        if cleared_current {
            if let Some(hdr) = self.hdr_image_cache.get(&self.current_index).cloned() {
                self.current_hdr_image =
                    Some(crate::app::CurrentHdrImage::new(self.current_index, hdr));
            }
            self.refresh_hdr_view_status();
            if self.settings.preload && !self.raw_gpu_demosaic_await_hdr_present {
                self.schedule_preloads(true);
            }
        }
        if cleared_any {
            ctx.request_repaint();
        }
    }

    fn indices_for_raw_demosaic_notice(
        &self,
        notice: &crate::hdr::renderer::RawGpuDemosaicBakedNotice,
        is_failure: bool,
    ) -> Vec<usize> {
        resolve_raw_demosaic_notice_indices(
            notice,
            is_failure,
            &self.hdr_image_cache,
            &self.hdr_raw_gpu_demosaic_pending_indices,
            &self.hdr_raw_gpu_demosaic_pending_key_index,
            self.current_hdr_image.as_ref(),
        )
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
}
