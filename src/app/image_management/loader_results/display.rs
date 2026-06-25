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

#[cfg(feature = "preload-debug")]
fn preload_debug_log_demosaic_notice_resolution(
    notice: &crate::hdr::renderer::RawGpuDemosaicBakedNotice,
    is_failure: bool,
    matching: &[usize],
    hdr_image_cache: &HashMap<usize, Arc<crate::hdr::types::HdrImageBuffer>>,
    hdr_raw_gpu_demosaic_pending_indices: &HashSet<usize>,
    hdr_raw_gpu_demosaic_pending_key_index: &HashMap<crate::hdr::renderer::HdrImageKey, usize>,
) {
    if is_failure {
        return;
    }
    if !matching.is_empty() {
        crate::preload_debug!(
            "[PreloadDebug][RAW-GPU] demosaic notice matched idx={matching:?} ms={}",
            notice.demosaic_ms
        );
        return;
    }
    let mut details = Vec::new();
    for &idx in hdr_raw_gpu_demosaic_pending_indices {
        let cache_key_eq = hdr_image_cache
            .get(&idx)
            .is_some_and(|hdr| crate::hdr::renderer::HdrImageKey::from_image(hdr) == notice.key);
        let side_map_idx = hdr_raw_gpu_demosaic_pending_key_index
            .get(&notice.key)
            .copied();
        details.push(format!(
            "idx={idx} cache_key_eq={cache_key_eq} side_map_idx={side_map_idx:?}"
        ));
    }
    crate::preload_debug!(
        "[PreloadDebug][RAW-GPU] demosaic notice DROPPED (success): ms={} pending={:?} side_map_has_key={} [{}]",
        notice.demosaic_ms,
        hdr_raw_gpu_demosaic_pending_indices,
        hdr_raw_gpu_demosaic_pending_key_index.contains_key(&notice.key),
        details.join("; ")
    );
    crate::preload_debug!(
        "[PreloadDebug][RAW-GPU] demosaic notice key={:?}",
        notice.key
    );
    for (&idx, hdr) in hdr_image_cache {
        if hdr_raw_gpu_demosaic_pending_indices.contains(&idx) {
            crate::preload_debug!(
                "[PreloadDebug][RAW-GPU] demosaic pending cache idx={idx} key={:?}",
                crate::hdr::renderer::HdrImageKey::from_image(hdr)
            );
        }
    }
}

fn resolve_raw_demosaic_notice_fallback_indices(
    notice: &crate::hdr::renderer::RawGpuDemosaicBakedNotice,
    hdr_image_cache: &HashMap<usize, Arc<crate::hdr::types::HdrImageBuffer>>,
    hdr_raw_gpu_demosaic_pending_indices: &HashSet<usize>,
    hdr_raw_gpu_demosaic_pending_key_index: &HashMap<crate::hdr::renderer::HdrImageKey, usize>,
    current_hdr_image: Option<&crate::app::CurrentHdrImage>,
) -> Vec<usize> {
    let mut matching = Vec::new();
    let mut seen = HashSet::new();

    // Side map records the index when demosaic started. Cache may evict the HDR entry while
    // pending still holds the index; trust side_map + pending until refresh clears both.
    if let Some(&idx) = hdr_raw_gpu_demosaic_pending_key_index.get(&notice.key) {
        let cache_confirms = hdr_image_cache
            .get(&idx)
            .is_some_and(|hdr| crate::hdr::renderer::HdrImageKey::from_image(hdr) == notice.key);
        if (cache_confirms || hdr_raw_gpu_demosaic_pending_indices.contains(&idx))
            && seen.insert(idx)
        {
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
    if matching.is_empty() {
        for (&idx, hdr) in hdr_image_cache {
            if crate::hdr::renderer::HdrImageKey::from_image(hdr) == notice.key {
                matching.push(idx);
                break;
            }
        }
    }
    // Last resort: one pending entry and notice key matches that cache slot (avoids mis-routing
    // stale GPU notices when prefetch evicted the intended image but another RAW is pending).
    if matching.is_empty() && hdr_raw_gpu_demosaic_pending_indices.len() == 1 {
        let sole_idx = *hdr_raw_gpu_demosaic_pending_indices.iter().next().unwrap();
        if hdr_image_cache
            .get(&sole_idx)
            .is_some_and(|hdr| crate::hdr::renderer::HdrImageKey::from_image(hdr) == notice.key)
        {
            matching.push(sole_idx);
        }
    }
    matching
}

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
    if matching.is_empty() {
        matching = resolve_raw_demosaic_notice_fallback_indices(
            notice,
            hdr_image_cache,
            hdr_raw_gpu_demosaic_pending_indices,
            hdr_raw_gpu_demosaic_pending_key_index,
            current_hdr_image,
        );
    }
    if matching.is_empty() && is_failure {
        log::warn!(
            "[HDR] Dropping GPU RAW demosaic notice: no pending index matched key {:?} (pending={})",
            notice.key,
            hdr_raw_gpu_demosaic_pending_indices.len()
        );
    }
    #[cfg(feature = "preload-debug")]
    preload_debug_log_demosaic_notice_resolution(
        notice,
        is_failure,
        &matching,
        hdr_image_cache,
        hdr_raw_gpu_demosaic_pending_indices,
        hdr_raw_gpu_demosaic_pending_key_index,
    );
    matching
}

impl ImageViewerApp {
    /// Apply GPU RAW demosaic completion for `idx`: clear pending, promote OSD, release bootstrap.
    pub(crate) fn apply_raw_gpu_demosaic_success(
        &mut self,
        idx: usize,
        demosaic_ms: Option<u32>,
        ctx: &egui::Context,
    ) {
        self.hdr_raw_gpu_demosaic_pending_indices.remove(&idx);
        self.hdr_raw_gpu_demosaic_baked_indices.insert(idx);
        if let Some(hdr) = self.hdr_image_cache.get(&idx) {
            let key = crate::hdr::renderer::HdrImageKey::from_image(hdr);
            self.hdr_raw_gpu_demosaic_pending_key_index.remove(&key);
        }
        let is_current = idx == self.current_index;
        if is_current {
            self.raw_gpu_demosaic_await_hdr_present = true;
        }
        if let Some(ms) = demosaic_ms {
            self.raw_metadata.set_gpu_demosaic_ms(idx, ms);
        }
        let develop_dims = self
            .hdr_image_cache
            .get(&idx)
            .map(|hdr| (hdr.width, hdr.height));
        #[cfg_attr(not(feature = "preload-debug"), allow(unused_variables))]
        let promoted = develop_dims
            .is_some_and(|(w, h)| self.raw_metadata.promote_gpu_demosaic_complete(idx, w, h));
        if is_current && develop_dims.is_none() {
            log::warn!(
                "[RAW-GPU] demosaic complete for current index={idx} but hdr_image_cache \
                 missing; OSD may stay on bootstrap until refine metadata arrives"
            );
        }
        crate::preload_debug!(
            "[PreloadDebug][RAW-GPU] demosaic complete idx={idx} ms={} osd_promoted={promoted} cur={}",
            demosaic_ms.unwrap_or(0),
            self.current_index
        );
        self.on_raw_hdr_plane_ready(idx);
        if is_current {
            self.osd.sync_events();
            if let Some(hdr) = self.hdr_image_cache.get(&self.current_index).cloned() {
                self.current_hdr_image =
                    Some(crate::app::CurrentHdrImage::new(self.current_index, hdr));
            }
            // Draw may have recorded an SDR-bootstrap plan for this frame before the GPU bake
            // notice was drained; recompute OSD from live state instead of the stale cache.
            self.clear_frame_render_plan_cache();
            self.refresh_hdr_view_status();
            if self.settings.preload && !self.raw_gpu_demosaic_await_hdr_present {
                self.schedule_preloads(true);
            }
            ctx.request_repaint();
            // `request_repaint` from `post_rendering` / `logic` is not always enough to
            // schedule the next frame while idle; wake the native window too.
            self.wake_root_for_logic();
        }
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
    pub(crate) fn prepare_display_frame(
        &mut self,
        ctx: &egui::Context,
        frame: Option<&eframe::Frame>,
    ) {
        if let Some(frame) = frame {
            if self.tick_raw_gpu_demosaic_completion(ctx, Some(frame)) {
                ctx.request_repaint();
                self.wake_root_for_logic();
            }
        }
        self.flush_deferred_sdr_upload_for_current(ctx);
        self.try_start_pending_transition_if_ready();
    }

    /// Apply GPU RAW demosaic completion notices and stale pending flags.
    ///
    /// Notices are queued from wgpu `prepare()` which runs **after** `ui()` returns, so this
    /// must run from `post_rendering` (with `RepaintNow`) as well as from `logic()`.
    pub(crate) fn tick_raw_gpu_demosaic_completion(
        &mut self,
        ctx: &egui::Context,
        frame: Option<&eframe::Frame>,
    ) -> bool {
        if self.scanning {
            return false;
        }
        let from_drain = self.drain_raw_demosaic_baked_notifications(ctx);
        let from_refresh = frame
            .map(|frame| self.refresh_raw_gpu_demosaic_pending_from_gpu_bindings(ctx, Some(frame)))
            .unwrap_or(false);
        from_drain || from_refresh
    }

    /// Returns `true` when the current image should immediately repaint through the HDR plane.
    fn drain_raw_demosaic_baked_notifications(&mut self, ctx: &egui::Context) -> bool {
        let baked = std::mem::take(&mut *self.raw_demosaic_baked_notify.lock());
        if baked.is_empty() {
            return false;
        }
        crate::preload_debug!(
            "[PreloadDebug][RAW-GPU] drain {} demosaic notice(s) cur={} pending={:?}",
            baked.len(),
            self.current_index,
            self.hdr_raw_gpu_demosaic_pending_indices
        );
        let mut cleared_any = false;
        let mut applied_current = false;
        for notice in baked {
            let is_failure = notice.demosaic_ms == u32::MAX;
            let matching = self.indices_for_raw_demosaic_notice(&notice, is_failure);
            for idx in matching {
                cleared_any = true;
                if idx == self.current_index {
                    applied_current = true;
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
                        self.loader.finish_image_request(idx);
                        self.loader.request_load(
            idx,
                            path,
                            self.settings.raw_high_quality,
                            crate::settings::RawDemosaicMode::Cpu,
                        );
                    }
                } else {
                    self.apply_raw_gpu_demosaic_success(idx, Some(notice.demosaic_ms), ctx);
                }
            }
        }
        // Success path repaints via `apply_raw_gpu_demosaic_success`; failure cleanup must too.
        if cleared_any && !applied_current {
            ctx.request_repaint();
            self.wake_root_for_logic();
        }
        applied_current
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
