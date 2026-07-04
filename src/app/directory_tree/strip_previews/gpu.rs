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

//! Directory-tree strip GPU upload pipeline and cache write-through.

use eframe::egui;

use crate::app::ImageViewerApp;
use crate::app::directory_tree_strip_cache::{
    DirectoryTreeStripPendingGpuUpload, MAX_STRIP_GPU_UPLOADS_PER_PAINT,
    MAX_STRIP_PENDING_GPU_UPLOADS, StripPreviewBufferTag, StripPreviewReplaceParams,
    StripThumbnailCacheRequest, decide_strip_preview_replace, strip_decoded_ready_for_gpu_upload,
};
use crate::loader::{DecodedImage, PreviewStage, hdr_has_iso_deferred_gain_map};

impl ImageViewerApp {
    fn evict_strip_pending_gpu_uploads(&mut self, need: usize) -> usize {
        if need == 0 {
            return 0;
        }
        let mut dropped_indices = Vec::new();
        let mut still_need = need;

        // Evict Initial-stage items first (lower priority), O(1) pop from front.
        while still_need > 0 {
            let Some(item) = self.directory_tree_strip_pending_gpu_initial.pop_front() else {
                break;
            };
            dropped_indices.push(item.index);
            still_need -= 1;
        }

        // Fall back to evicting from the front of the Refined queue.
        while still_need > 0 {
            let Some(item) = self.directory_tree_strip_pending_gpu_refined.pop_front() else {
                break;
            };
            dropped_indices.push(item.index);
            still_need -= 1;
        }

        let dropped = dropped_indices.len();
        #[cfg(feature = "preload-debug")]
        if !dropped_indices.is_empty() {
            crate::preload_debug!(
                "[PreloadDebug][StripGpu] pending queue evicted {dropped} item(s): {:?}",
                dropped_indices
            );
        }
        for index in dropped_indices {
            self.clear_strip_preview_attempt_state(index);
        }
        dropped
    }

    pub(super) fn queue_directory_tree_strip_gpu_upload(
        &mut self,
        index: usize,
        decoded: DecodedImage,
        stage: PreviewStage,
        logical: Option<(u32, u32)>,
        buffer_tag: StripPreviewBufferTag,
        strip_max_side_used: Option<u32>,
    ) {
        if !self.directory_tree_list_previews_active() || index >= self.image_files.len() {
            return;
        }
        let pending_len = self.directory_tree_strip_pending_gpu_initial.len()
            + self.directory_tree_strip_pending_gpu_refined.len();
        if pending_len >= MAX_STRIP_PENDING_GPU_UPLOADS {
            let dropped = self.evict_strip_pending_gpu_uploads(
                pending_len.saturating_sub(MAX_STRIP_PENDING_GPU_UPLOADS - 1),
            );
            log::warn!(
                "[DirectoryTree] Strip pending GPU upload queue full; dropped {dropped} item(s)"
            );
        }
        #[cfg(feature = "preload-debug")]
        let decoded_w = decoded.width;
        #[cfg(feature = "preload-debug")]
        let decoded_h = decoded.height;
        let seq = self.directory_tree_strip_pending_gpu_next_seq;
        self.directory_tree_strip_pending_gpu_next_seq += 1;
        let upload = DirectoryTreeStripPendingGpuUpload {
            index,
            decoded,
            stage,
            logical,
            buffer_tag,
            seq,
            strip_max_side_used,
        };
        match stage {
            PreviewStage::Initial => &mut self.directory_tree_strip_pending_gpu_initial,
            PreviewStage::Refined => &mut self.directory_tree_strip_pending_gpu_refined,
        }
        .push_back(upload);
        #[cfg(feature = "preload-debug")]
        {
            let pending_len2 = self.directory_tree_strip_pending_gpu_initial.len()
                + self.directory_tree_strip_pending_gpu_refined.len();
            crate::preload_debug!(
                "[PreloadDebug][StripGpu] queue idx={} tag={buffer_tag:?} stage={:?} decoded={}x{} logical={:?} \
                 cache_contains={} cache_count={} pending_len={}",
                index,
                stage,
                decoded_w,
                decoded_h,
                logical,
                self.directory_tree_strip_cache.contains(index),
                self.directory_tree_strip_cache.textures().len(),
                pending_len2
            );
        }
    }

    /// Request repaints after a strip GPU flush batch. More uploads still queued uses the
    /// existing per-batch repaint; a final install that bumps cache revision gets one coalesced
    /// directory-tree viewport refresh (and a root logic wake when detached).
    fn request_directory_tree_strip_flush_repaint(
        &mut self,
        ctx: &egui::Context,
        cache_revision_changed: bool,
        pending_uploads_remain: bool,
    ) {
        if pending_uploads_remain {
            ctx.request_repaint_of(self.directory_tree_repaint_viewport_id());
            ctx.request_repaint();
            // Linux/Wayland may not schedule another frame from egui repaint alone when idle.
            self.wake_root_for_logic();
            return;
        }
        if !cache_revision_changed {
            return;
        }
        self.mark_directory_tree_repaint_pending();
        self.request_directory_tree_viewport_repaint(ctx);
        ctx.request_repaint();
        // Final install often runs during paint after logic() already published an older rev;
        // wake the winit loop so the next logic pass publishes the new textures (and so idle
        // platforms repaint without waiting for pointer input).
        self.wake_root_for_logic();
    }

    pub(crate) fn flush_directory_tree_strip_pending_gpu_uploads(&mut self, ctx: &egui::Context) {
        let pending_len = self.directory_tree_strip_pending_gpu_initial.len()
            + self.directory_tree_strip_pending_gpu_refined.len();
        if pending_len == 0 {
            return;
        }
        let revision_before = self.directory_tree_strip_cache.gpu_revision();
        let take = MAX_STRIP_GPU_UPLOADS_PER_PAINT.min(pending_len);
        // Merge the per-stage queues in FIFO order by comparing sequence numbers.
        let mut batch = Vec::with_capacity(take);
        for _ in 0..take {
            let from_initial = self
                .directory_tree_strip_pending_gpu_initial
                .front()
                .map(|item| item.seq);
            let from_refined = self
                .directory_tree_strip_pending_gpu_refined
                .front()
                .map(|item| item.seq);
            let source = match (from_initial, from_refined) {
                (Some(si), Some(sr)) if si < sr => {
                    &mut self.directory_tree_strip_pending_gpu_initial
                }
                (Some(_), Some(_)) => &mut self.directory_tree_strip_pending_gpu_refined,
                (Some(_), None) => &mut self.directory_tree_strip_pending_gpu_initial,
                (None, Some(_)) => &mut self.directory_tree_strip_pending_gpu_refined,
                (None, None) => break,
            };
            batch.push(source.pop_front().unwrap());
        }
        #[cfg(feature = "preload-debug")]
        {
            let indices: Vec<usize> = batch.iter().map(|item| item.index).collect();
            let remaining = self.directory_tree_strip_pending_gpu_initial.len()
                + self.directory_tree_strip_pending_gpu_refined.len();
            crate::preload_debug!(
                "[PreloadDebug][StripGpu] flush take={} pending_left={remaining} indices={indices:?}",
                batch.len(),
            );
        }
        for item in batch {
            #[cfg(feature = "preload-debug")]
            let cache_before = self.directory_tree_strip_cache.contains(item.index);
            self.cache_directory_tree_strip_thumbnail(StripThumbnailCacheRequest {
                index: item.index,
                decoded: &item.decoded,
                stage: item.stage,
                logical_size: item.logical,
                buffer_tag: item.buffer_tag,
                strip_max_side_used: item.strip_max_side_used,
                ctx,
                bypass_detach_queue: true,
            });
            #[cfg(feature = "preload-debug")]
            {
                let cache_after = self.directory_tree_strip_cache.contains(item.index);
                let cache_count = self.directory_tree_strip_cache.textures().len();
                crate::preload_debug!(
                    "[PreloadDebug][StripGpu] flush done idx={} cache_before={} \
                     cache_after={} cache_count={} rev={}",
                    item.index,
                    cache_before,
                    cache_after,
                    cache_count,
                    self.directory_tree_strip_cache.gpu_revision()
                );
            }
        }
        let cache_revision_changed =
            self.directory_tree_strip_cache.gpu_revision() != revision_before;
        let pending_uploads_remain = !self.directory_tree_strip_pending_gpu_initial.is_empty()
            || !self.directory_tree_strip_pending_gpu_refined.is_empty();
        #[cfg(feature = "preload-debug")]
        if cache_revision_changed && !pending_uploads_remain {
            crate::preload_debug!(
                "[PreloadDebug][StripGpu] flush installed rev {revision_before} -> {} \
                 repaint coalesced",
                self.directory_tree_strip_cache.gpu_revision()
            );
        }
        self.request_directory_tree_strip_flush_repaint(
            ctx,
            cache_revision_changed,
            pending_uploads_remain,
        );
        // Flush runs during paint, after logic() may have published a stale preview rev.
        if cache_revision_changed {
            self.publish_directory_tree_view_from_state(false);
        }
    }

    fn strip_texture_cache_sdr_is_dark_deferred_baseline(&self, index: usize) -> bool {
        if self
            .deferred_sdr_uploads
            .get(&index)
            .is_some_and(DecodedImage::is_sdr_deferred_placeholder)
        {
            return true;
        }
        let Some(hdr) = self.hdr_image_cache.get(&index) else {
            return false;
        };
        if hdr_has_iso_deferred_gain_map(hdr.as_ref()) {
            return true;
        }
        self.hdr_placeholder_fallback_indices.contains(&index)
    }

    pub(super) fn strip_skip_texture_cache_sync_for_deferred_black_sdr(
        &self,
        index: usize,
    ) -> bool {
        self.strip_texture_cache_sdr_is_dark_deferred_baseline(index)
    }

    pub(crate) fn cache_directory_tree_strip_thumbnail(
        &mut self,
        request: StripThumbnailCacheRequest<'_>,
    ) {
        let StripThumbnailCacheRequest {
            index,
            decoded,
            stage,
            logical_size,
            buffer_tag,
            strip_max_side_used,
            ctx,
            bypass_detach_queue,
        } = request;
        if !self.directory_tree_list_previews_active() || index >= self.image_files.len() {
            return;
        }
        if decoded.is_sdr_deferred_placeholder() {
            #[cfg(feature = "preload-debug")]
            crate::preload_debug!(
                "[PreloadDebug][Strip] skip strip cache idx={} reason=black_placeholder (pre-cache gate)",
                index
            );
            return;
        }
        let strip_max_side = self
            .settings
            .directory_tree_list_preview_size
            .strip_max_side();
        if !strip_decoded_ready_for_gpu_upload(decoded, strip_max_side, strip_max_side_used) {
            self.schedule_strip_pending_gpu_resample(
                index,
                decoded.clone(),
                stage,
                logical_size,
                buffer_tag,
            );
            return;
        }
        if let Some(logical) = logical_size
            && self.strip_skip_texture_cache_sync_for_deferred_black_sdr(index)
            && self
                .directory_tree_strip_cache
                .is_valid_for_logical(index, logical)
        {
            let cached_tag = self.directory_tree_strip_cache.cached_buffer_tag(index);
            let cached_stage = self.directory_tree_strip_cache.cached_preview_stage(index);
            let cached_dims = self.directory_tree_strip_cache.preview_dimensions(index);
            let would_upgrade = decide_strip_preview_replace(&StripPreviewReplaceParams {
                index,
                source: "cache_directory_tree_strip_thumbnail",
                cached_tag,
                cached_stage,
                cached_logical: self
                    .directory_tree_strip_cache
                    .logical_sizes()
                    .get(&index)
                    .copied(),
                cached_preview_w: cached_dims.map(|(w, _)| w),
                cached_preview_h: cached_dims.map(|(_, h)| h),
                incoming_tag: buffer_tag,
                incoming_stage: stage,
                incoming_logical: Some(logical),
                preview_w: decoded.width,
                preview_h: decoded.height,
                decoded: Some(decoded),
            });
            if !would_upgrade {
                return;
            }
        }
        if self.directory_tree_nav_is_detached() && !bypass_detach_queue {
            self.queue_directory_tree_strip_gpu_upload(
                index,
                decoded.clone(),
                stage,
                logical_size,
                buffer_tag,
                strip_max_side_used,
            );
            return;
        }
        self.directory_tree_strip_cache.upsert_from_decoded(
            index,
            decoded,
            crate::app::directory_tree_strip_cache::StripDecodedUpsert {
                stage,
                buffer_tag,
                logical_size,
                path: &self.image_files[index],
                ctx,
                strip_max_side,
                strip_max_side_used,
            },
        );
    }
}
