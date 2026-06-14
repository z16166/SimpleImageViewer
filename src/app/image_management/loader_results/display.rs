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

    fn drain_raw_demosaic_baked_notifications(&mut self, ctx: &egui::Context) {
        let baked = std::mem::take(&mut *self.raw_demosaic_baked_notify.lock());
        if baked.is_empty() {
            return;
        }
        let mut cleared_any = false;
        let mut cleared_current = false;
        for notice in baked {
            let matching: Vec<usize> = self
                .hdr_image_cache
                .iter()
                .filter_map(|(&idx, hdr)| {
                    if crate::hdr::renderer::HdrImageKey::from_image(hdr) == notice.key
                        && self.hdr_raw_gpu_demosaic_pending_indices.contains(&idx)
                    {
                        Some(idx)
                    } else {
                        None
                    }
                })
                .collect();
            let is_failure = notice.demosaic_ms == u32::MAX;
            for idx in matching {
                self.hdr_raw_gpu_demosaic_pending_indices.remove(&idx);
                cleared_any = true;
                if idx == self.current_index {
                    cleared_current = true;
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
                        let fallback_gen = self.loader.current_generation(idx) + 1;
                        self.loader.request_load(
                            idx,
                            fallback_gen,
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
            if self.settings.preload {
                self.schedule_preloads(true);
            }
        }
        if cleared_any {
            ctx.request_repaint();
        }
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
