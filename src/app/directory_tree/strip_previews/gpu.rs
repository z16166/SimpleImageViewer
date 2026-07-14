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

#[cfg(feature = "preload-debug")]
use std::time::Instant;

use eframe::egui;

use crate::app::ImageViewerApp;
use crate::app::directory_tree_strip_cache::{
    DIRECTORY_TREE_STRIP_RGBA_BYTES_PER_PIXEL, DirectoryTreeStripGpuUploadRequest,
    DirectoryTreeStripJobKey, DirectoryTreeStripPendingGpuUpload,
    MAX_STRIP_GPU_UPLOAD_BYTES_PER_PAINT, MAX_STRIP_GPU_UPLOADS_PER_PAINT,
    MAX_STRIP_PENDING_GPU_UPLOAD_BYTES, MAX_STRIP_PENDING_GPU_UPLOADS, StripPreviewBufferTag,
    StripPreviewReplaceParams, StripThumbnailCacheOwnedRequest, StripThumbnailCacheRequest,
    decide_strip_preview_replace, strip_decoded_ready_for_gpu_upload, strip_preview_quality_rank,
};
use crate::loader::{DecodedImage, PreviewStage, hdr_has_iso_deferred_gain_map};

struct StripThumbnailCachePrepare<'a> {
    index: usize,
    decoded: &'a DecodedImage,
    job_key: Option<&'a DirectoryTreeStripJobKey>,
    stage: PreviewStage,
    logical_size: Option<(u32, u32)>,
    buffer_tag: StripPreviewBufferTag,
    strip_max_side_used: Option<u32>,
    source: &'static str,
}

enum StripThumbnailCacheDecision {
    Drop,
    Resample,
    Proceed { strip_max_side: u32 },
}

struct StripPendingGpuCoalesce {
    dropped: usize,
    keep_incoming: bool,
}

struct StripThumbnailCacheUpsert<'a> {
    /// Authoritative cache key; must match the decoded pixels' source path.
    path: &'a std::path::Path,
    decoded: &'a DecodedImage,
    stage: PreviewStage,
    logical_size: Option<(u32, u32)>,
    buffer_tag: StripPreviewBufferTag,
    strip_max_side_used: Option<u32>,
    ctx: &'a egui::Context,
    strip_max_side: u32,
}

impl ImageViewerApp {
    fn strip_visible_image_list_range(&self) -> Option<(usize, usize)> {
        self.directory_tree
            .list
            .try_lock()
            .and_then(|list| list.image_list_visible_row_range)
    }

    fn strip_pending_key_is_visible(
        visible_range: Option<(usize, usize)>,
        key: &DirectoryTreeStripJobKey,
        path_to_index: &std::collections::HashMap<std::path::PathBuf, usize>,
    ) -> bool {
        let Some((start, end)) = visible_range else {
            return false;
        };
        // Resolve the path's current row; submit-time `key.index` may be stale after reorder.
        path_to_index
            .get(&key.path)
            .is_some_and(|&index| index >= start && index < end)
    }

    fn decoded_strip_upload_bytes(decoded: &DecodedImage) -> usize {
        (decoded.width as usize)
            .saturating_mul(decoded.height as usize)
            .saturating_mul(DIRECTORY_TREE_STRIP_RGBA_BYTES_PER_PIXEL)
    }

    fn pending_strip_upload_bytes(item: &DirectoryTreeStripPendingGpuUpload) -> usize {
        item.upload_bytes
    }

    fn total_pending_strip_upload_bytes(&self) -> usize {
        self.directory_tree_strip_pending_gpu_initial
            .iter()
            .chain(self.directory_tree_strip_pending_gpu_refined.iter())
            .map(Self::pending_strip_upload_bytes)
            .sum()
    }

    fn pending_strip_upload_budget_need(
        pending_len: usize,
        pending_bytes: usize,
        incoming_bytes: usize,
    ) -> (usize, usize) {
        let need_by_count = pending_len
            .saturating_add(1)
            .saturating_sub(MAX_STRIP_PENDING_GPU_UPLOADS);
        let need_by_bytes = pending_bytes
            .saturating_add(incoming_bytes)
            .saturating_sub(MAX_STRIP_PENDING_GPU_UPLOAD_BYTES);
        (need_by_count, need_by_bytes)
    }

    fn pop_evictable_pending_upload(
        &mut self,
        stage: PreviewStage,
        visible_range: Option<(usize, usize)>,
    ) -> Option<DirectoryTreeStripPendingGpuUpload> {
        // Resolve visibility via the generation-cached path index, then mutate the queue.
        // Warm first; find uses disjoint field borrows (cache map + pending queue).
        let _ = self.image_strip_path_index();
        let position = {
            let path_to_index = &self
                .cached_image_strip_path_index
                .as_ref()
                .expect("warmed above")
                .1;
            let queue = match stage {
                PreviewStage::Initial => &self.directory_tree_strip_pending_gpu_initial,
                PreviewStage::Refined => &self.directory_tree_strip_pending_gpu_refined,
            };
            queue.iter().position(|item| {
                !Self::strip_pending_key_is_visible(visible_range, &item.key, path_to_index)
            })
        }?;
        match stage {
            PreviewStage::Initial => self
                .directory_tree_strip_pending_gpu_initial
                .remove(position),
            PreviewStage::Refined => self
                .directory_tree_strip_pending_gpu_refined
                .remove(position),
        }
    }

    fn evict_strip_pending_gpu_uploads(
        &mut self,
        need_count: usize,
        need_bytes: usize,
        visible_range: Option<(usize, usize)>,
    ) -> (usize, usize) {
        if need_count == 0 && need_bytes == 0 {
            return (0, 0);
        }
        // Keep visible rows resident even when pending count/byte budgets are exceeded.
        // This can temporarily exceed caps, but avoids dropping thumbnails the user is
        // actively looking at; scrolling or generation changes make them evictable again.
        let mut dropped_keys = std::mem::take(&mut self.directory_tree_strip_pending_drop_scratch);
        dropped_keys.clear();
        let mut dropped = 0usize;
        let mut released_bytes = 0usize;

        for stage in [PreviewStage::Initial, PreviewStage::Refined] {
            while dropped < need_count || released_bytes < need_bytes {
                let Some(item) = self.pop_evictable_pending_upload(stage, visible_range) else {
                    break;
                };
                released_bytes =
                    released_bytes.saturating_add(Self::pending_strip_upload_bytes(&item));
                dropped_keys.push(item.key);
                dropped += 1;
            }
            if dropped >= need_count && released_bytes >= need_bytes {
                break;
            }
        }

        #[cfg(feature = "preload-debug")]
        if !dropped_keys.is_empty() {
            let dropped_indices: Vec<usize> = dropped_keys.iter().map(|key| key.index).collect();
            crate::preload_debug!(
                "[PreloadDebug][StripGpu] pending queue evicted {dropped} item(s), bytes={released_bytes}: {:?}",
                dropped_indices
            );
        }
        for key in dropped_keys.drain(..) {
            self.clear_strip_preview_attempt_state_for_key(&key);
        }
        self.directory_tree_strip_pending_drop_scratch = dropped_keys;
        (dropped, released_bytes)
    }

    fn pending_strip_upload_quality_rank(item: &DirectoryTreeStripPendingGpuUpload) -> u16 {
        strip_preview_quality_rank(item.buffer_tag, item.stage)
    }

    fn pending_strip_upload_ready_for_gpu(
        item: &DirectoryTreeStripPendingGpuUpload,
        strip_max_side: u32,
    ) -> bool {
        strip_decoded_ready_for_gpu_upload(&item.decoded, strip_max_side, item.strip_max_side_used)
    }

    fn has_ready_pending_strip_upload_at_least_rank(
        &self,
        path: &std::path::Path,
        incoming_rank: u16,
        strip_max_side: u32,
    ) -> bool {
        self.directory_tree_strip_pending_gpu_initial
            .iter()
            .chain(self.directory_tree_strip_pending_gpu_refined.iter())
            .any(|item| {
                item.key.path == path
                    && Self::pending_strip_upload_quality_rank(item) >= incoming_rank
                    && Self::pending_strip_upload_ready_for_gpu(item, strip_max_side)
            })
    }

    fn coalesce_pending_gpu_upload_for_path(
        &mut self,
        path: &std::path::Path,
        incoming_stage: PreviewStage,
        incoming_tag: StripPreviewBufferTag,
    ) -> StripPendingGpuCoalesce {
        // Coalesce by path: submit-time `key.index` may be stale after a column-sort reorder
        // that does not bump `image_list_generation`. Coalesced uploads either have no worker
        // state or were released by result polling, so this path only needs to count dropped
        // queued pixels. Do not let a lower-rank source (for example a full-size SDR fallback)
        // replace an already queued strip-sized result for the same path; otherwise the
        // fallback can fail the pre-upload size gate and leave the row stuck on the placeholder.
        let incoming_rank = strip_preview_quality_rank(incoming_tag, incoming_stage);
        let mut retained_higher_rank = false;

        let initial_before = self.directory_tree_strip_pending_gpu_initial.len();
        self.directory_tree_strip_pending_gpu_initial
            .retain(|item| {
                if item.key.path != path {
                    return true;
                }
                if Self::pending_strip_upload_quality_rank(item) > incoming_rank {
                    retained_higher_rank = true;
                    return true;
                }
                false
            });
        let mut dropped = initial_before - self.directory_tree_strip_pending_gpu_initial.len();

        // Initial uploads do not evict pending Refined uploads for the same path: Refined
        // pixels are higher quality, and stale keys are rejected again before GPU upload.
        if incoming_stage == PreviewStage::Refined {
            let refined_before = self.directory_tree_strip_pending_gpu_refined.len();
            self.directory_tree_strip_pending_gpu_refined
                .retain(|item| {
                    if item.key.path != path {
                        return true;
                    }
                    if Self::pending_strip_upload_quality_rank(item) > incoming_rank {
                        retained_higher_rank = true;
                        return true;
                    }
                    false
                });
            dropped += refined_before - self.directory_tree_strip_pending_gpu_refined.len();
        }

        StripPendingGpuCoalesce {
            dropped,
            keep_incoming: !retained_higher_rank,
        }
    }

    /// Refresh `key.index` from the current path->row map. Returns false when the path left
    /// the list or the key's generation no longer matches.
    fn refresh_strip_job_key_index(&mut self, key: &mut DirectoryTreeStripJobKey) -> bool {
        if !self.directory_tree_strip_key_matches_current_list(key) {
            return false;
        }
        let Some(&current_index) = self.image_strip_path_index().get(&key.path) else {
            return false;
        };
        key.index = current_index;
        true
    }

    pub(super) fn queue_directory_tree_strip_gpu_upload(
        &mut self,
        request: DirectoryTreeStripGpuUploadRequest,
    ) {
        let DirectoryTreeStripGpuUploadRequest {
            index,
            decoded,
            stage,
            logical,
            buffer_tag,
            strip_max_side_used,
            job_key,
        } = request;
        #[cfg(feature = "preload-debug")]
        crate::preload_debug!(
            "[PreloadDebug][StripGpu] queue_enter idx={} decoded={}x{} strip_max_side_used={:?} stage={:?} tag={:?} cache_count={}",
            index,
            decoded.width,
            decoded.height,
            strip_max_side_used,
            stage,
            buffer_tag,
            self.directory_tree_strip_cache.textures().len(),
        );
        if !self.directory_tree_list_previews_active() {
            return;
        }
        // Prefer job_key.path even when submit-time index is stale/OOB after a reorder.
        let Some(mut key) = job_key.or_else(|| {
            (index < self.image_files.len())
                .then(|| self.directory_tree_strip_upload_key_for_current_index(index))
                .flatten()
        }) else {
            return;
        };
        if !self.refresh_strip_job_key_index(&mut key) {
            self.clear_strip_preview_attempt_state_for_key(&key);
            return;
        }
        let index = key.index;
        let strip_max_side = self
            .settings
            .directory_tree_list_preview_size
            .strip_max_side();
        if !strip_decoded_ready_for_gpu_upload(&decoded, strip_max_side, strip_max_side_used) {
            let incoming_rank = strip_preview_quality_rank(buffer_tag, stage);
            if self.has_ready_pending_strip_upload_at_least_rank(
                &key.path,
                incoming_rank,
                strip_max_side,
            ) {
                #[cfg(feature = "preload-debug")]
                crate::preload_debug_throttled!(
                    &format!(
                        "strip_gpu:skip_full_size_pending_ready:{index}:{buffer_tag:?}:{stage:?}"
                    ),
                    crate::preload_debug::PRELOAD_DEBUG_THROTTLE_INTERVAL,
                    "[PreloadDebug][StripGpu] skip queue idx={} tag={buffer_tag:?} stage={:?} reason=ready_pending_upload",
                    index,
                    stage
                );
                return;
            }
            self.schedule_or_queue_strip_pending_gpu_resample(
                index,
                decoded,
                stage,
                logical,
                buffer_tag,
                Some(key),
            );
            return;
        }
        let coalesce = self.coalesce_pending_gpu_upload_for_path(&key.path, stage, buffer_tag);
        if !coalesce.keep_incoming {
            #[cfg(feature = "preload-debug")]
            crate::preload_debug_throttled!(
                &format!("strip_gpu:skip_pending_higher_rank:{index}:{buffer_tag:?}:{stage:?}"),
                crate::preload_debug::PRELOAD_DEBUG_THROTTLE_INTERVAL,
                "[PreloadDebug][StripGpu] skip queue idx={} tag={buffer_tag:?} stage={:?} reason=pending_higher_rank coalesced={}",
                index,
                stage,
                coalesce.dropped
            );
            return;
        }
        #[cfg(feature = "preload-debug")]
        let coalesced = coalesce.dropped;
        #[cfg(not(feature = "preload-debug"))]
        let _ = coalesce.dropped;
        let visible_range = self.strip_visible_image_list_range();
        let incoming_visible = {
            let path_to_index = self.image_strip_path_index();
            Self::strip_pending_key_is_visible(visible_range, &key, path_to_index)
        };
        let pending_len = self.directory_tree_strip_pending_gpu_initial.len()
            + self.directory_tree_strip_pending_gpu_refined.len();
        let incoming_bytes = Self::decoded_strip_upload_bytes(&decoded);
        let pending_bytes = self.total_pending_strip_upload_bytes();
        let (need_by_count, need_by_bytes) =
            Self::pending_strip_upload_budget_need(pending_len, pending_bytes, incoming_bytes);
        if need_by_count > 0 || need_by_bytes > 0 {
            let (dropped, _released_bytes) =
                self.evict_strip_pending_gpu_uploads(need_by_count, need_by_bytes, visible_range);
            if dropped > 0 {
                log::warn!(
                    "[DirectoryTree] Strip pending GPU upload queue full; dropped {dropped} item(s)"
                );
            }
        }
        let pending_len_after_evict = self.directory_tree_strip_pending_gpu_initial.len()
            + self.directory_tree_strip_pending_gpu_refined.len();
        let pending_bytes_after_evict = self.total_pending_strip_upload_bytes();
        let would_exceed_count =
            pending_len_after_evict.saturating_add(1) > MAX_STRIP_PENDING_GPU_UPLOADS;
        let would_exceed_bytes = pending_bytes_after_evict.saturating_add(incoming_bytes)
            > MAX_STRIP_PENDING_GPU_UPLOAD_BYTES;
        if (would_exceed_count || would_exceed_bytes) && !incoming_visible {
            self.clear_strip_preview_attempt_state_for_key(&key);
            log::warn!(
                "[DirectoryTree] Strip pending GPU upload queue full; dropped incoming item index {index}"
            );
            return;
        }
        #[cfg(feature = "preload-debug")]
        let decoded_w = decoded.width;
        #[cfg(feature = "preload-debug")]
        let decoded_h = decoded.height;
        let seq = self.directory_tree_strip_pending_gpu_next_seq;
        self.directory_tree_strip_pending_gpu_next_seq += 1;
        let upload = DirectoryTreeStripPendingGpuUpload {
            key,
            decoded,
            upload_bytes: incoming_bytes,
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
            crate::preload_debug_throttled!(
                &format!("strip_gpu:queue:{index}:{buffer_tag:?}:{stage:?}:{logical:?}"),
                crate::preload_debug::PRELOAD_DEBUG_THROTTLE_INTERVAL,
                "[PreloadDebug][StripGpu] queue idx={} tag={buffer_tag:?} stage={:?} decoded={}x{} logical={:?} \
                 cache_contains={} cache_count={} pending_len={} coalesced={}",
                index,
                stage,
                decoded_w,
                decoded_h,
                logical,
                self.strip_cache_contains_index(index),
                self.directory_tree_strip_cache.textures().len(),
                pending_len2,
                coalesced
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
        #[cfg(feature = "preload-debug")]
        crate::preload_debug!(
            "[PreloadDebug][StripGpu] flush enter pending_len={} cache_count={}",
            pending_len,
            self.directory_tree_strip_cache.textures().len(),
        );
        if pending_len == 0 {
            return;
        }
        #[cfg(feature = "preload-debug")]
        let flush_started = Instant::now();
        let revision_before = self.directory_tree_strip_cache.gpu_revision();
        let take = MAX_STRIP_GPU_UPLOADS_PER_PAINT.min(pending_len);
        // Merge the per-stage queues in FIFO order by comparing sequence numbers.
        let mut batch = Vec::with_capacity(take);
        let mut upload_bytes = 0usize;
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
            let Some(item) = source.pop_front() else {
                break;
            };
            let item_bytes = Self::pending_strip_upload_bytes(&item);
            if !batch.is_empty()
                && upload_bytes.saturating_add(item_bytes) > MAX_STRIP_GPU_UPLOAD_BYTES_PER_PAINT
            {
                source.push_front(item);
                break;
            }
            upload_bytes = upload_bytes.saturating_add(item_bytes);
            batch.push(item);
        }
        #[cfg(feature = "preload-debug")]
        {
            let indices: Vec<usize> = batch.iter().map(|item| item.key.index).collect();
            let remaining = self.directory_tree_strip_pending_gpu_initial.len()
                + self.directory_tree_strip_pending_gpu_refined.len();
            crate::preload_debug_throttled!(
                "strip_gpu:flush_take",
                crate::preload_debug::PRELOAD_DEBUG_THROTTLE_INTERVAL,
                "[PreloadDebug][StripGpu] flush take={} bytes={} budget={} pending_left={remaining} indices={indices:?}",
                batch.len(),
                upload_bytes,
                MAX_STRIP_GPU_UPLOAD_BYTES_PER_PAINT,
            );
        }
        #[cfg(feature = "preload-debug")]
        let upload_count = batch.len();
        for mut item in batch {
            // Rematch submit-time index: column sort keeps generation but moves rows, so
            // writing via a stale index would upsert another path's cache slot.
            if !self.refresh_strip_job_key_index(&mut item.key) {
                self.clear_strip_preview_attempt_state_for_key(&item.key);
                continue;
            }
            #[cfg(feature = "preload-debug")]
            let cache_before = self.directory_tree_strip_cache.contains(&item.key.path);
            self.cache_directory_tree_strip_thumbnail_owned(StripThumbnailCacheOwnedRequest {
                index: item.key.index,
                decoded: item.decoded,
                job_key: Some(item.key.clone()),
                stage: item.stage,
                logical_size: item.logical,
                buffer_tag: item.buffer_tag,
                strip_max_side_used: item.strip_max_side_used,
                ctx,
                bypass_detach_queue: true,
            });
            #[cfg(feature = "preload-debug")]
            {
                let cache_after = self.directory_tree_strip_cache.contains(&item.key.path);
                let cache_count = self.directory_tree_strip_cache.textures().len();
                crate::preload_debug_throttled!(
                    &format!(
                        "strip_gpu:flush_done:{}:{cache_before}:{cache_after}",
                        item.key.index
                    ),
                    crate::preload_debug::PRELOAD_DEBUG_THROTTLE_INTERVAL,
                    "[PreloadDebug][StripGpu] flush done idx={} cache_before={} \
                     cache_after={} cache_count={} rev={}",
                    item.key.index,
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
        {
            let flush_ms = crate::preload_debug::elapsed_ms(flush_started);
            crate::preload_debug_throttled!(
                &format!(
                    "strip_gpu:flush_summary:{cache_revision_changed}:{pending_uploads_remain}"
                ),
                crate::preload_debug::PRELOAD_DEBUG_THROTTLE_INTERVAL,
                "[PreloadDebug][StripGpu] flush summary count={} bytes={} elapsed_ms={} rev {} -> {} pending_remain={}",
                upload_count,
                upload_bytes,
                flush_ms,
                revision_before,
                self.directory_tree_strip_cache.gpu_revision(),
                pending_uploads_remain,
            );
            if cache_revision_changed && !pending_uploads_remain {
                crate::preload_debug!(
                    "[PreloadDebug][StripGpu] flush installed rev {revision_before} -> {} \
                     repaint coalesced",
                    self.directory_tree_strip_cache.gpu_revision()
                );
            }
        }
        self.request_directory_tree_strip_flush_repaint(
            ctx,
            cache_revision_changed,
            pending_uploads_remain,
        );
        // Flush runs during paint, after logic() may have published a stale preview rev.
        if cache_revision_changed {
            self.publish_directory_tree_strip_preview_if_stale(ctx);
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

    fn prepare_directory_tree_strip_thumbnail_cache(
        &mut self,
        request: StripThumbnailCachePrepare<'_>,
    ) -> StripThumbnailCacheDecision {
        let StripThumbnailCachePrepare {
            index,
            decoded,
            job_key,
            stage,
            logical_size,
            buffer_tag,
            strip_max_side_used,
            source,
        } = request;
        if !self.directory_tree_list_previews_active() || index >= self.image_files.len() {
            return StripThumbnailCacheDecision::Drop;
        }
        if let Some(key) = job_key
            && !self.directory_tree_strip_key_matches_current_list(key)
        {
            self.clear_strip_preview_attempt_state_for_key(key);
            return StripThumbnailCacheDecision::Drop;
        }
        if decoded.is_sdr_deferred_placeholder() {
            #[cfg(feature = "preload-debug")]
            crate::preload_debug!(
                "[PreloadDebug][Strip] skip strip cache idx={} reason=black_placeholder (pre-cache gate)",
                index
            );
            return StripThumbnailCacheDecision::Drop;
        }
        let strip_max_side = self
            .settings
            .directory_tree_list_preview_size
            .strip_max_side();
        if !strip_decoded_ready_for_gpu_upload(decoded, strip_max_side, strip_max_side_used) {
            return StripThumbnailCacheDecision::Resample;
        }
        if let Some(logical) = logical_size
            && self.strip_skip_texture_cache_sync_for_deferred_black_sdr(index)
            && self.strip_cache_is_valid_for_logical_index(index, logical)
        {
            let path = &self.image_files[index];
            let cached_tag = self.directory_tree_strip_cache.cached_buffer_tag(path);
            let cached_stage = self.directory_tree_strip_cache.cached_preview_stage(path);
            let cached_dims = self.directory_tree_strip_cache.preview_dimensions(path);
            let would_upgrade = decide_strip_preview_replace(&StripPreviewReplaceParams {
                path,
                source,
                cached_tag,
                cached_stage,
                cached_logical: self
                    .directory_tree_strip_cache
                    .logical_sizes()
                    .get(path)
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
                return StripThumbnailCacheDecision::Drop;
            }
        }
        StripThumbnailCacheDecision::Proceed { strip_max_side }
    }

    fn upsert_directory_tree_strip_thumbnail_decoded(
        &mut self,
        request: StripThumbnailCacheUpsert<'_>,
    ) {
        let StripThumbnailCacheUpsert {
            path,
            decoded,
            stage,
            logical_size,
            buffer_tag,
            strip_max_side_used,
            ctx,
            strip_max_side,
        } = request;
        self.directory_tree_strip_cache.upsert_from_decoded(
            decoded,
            crate::app::directory_tree_strip_cache::StripDecodedUpsert {
                stage,
                buffer_tag,
                logical_size,
                path,
                ctx,
                strip_max_side,
                strip_max_side_used,
            },
        );
        self.publish_directory_tree_strip_preview_if_stale(ctx);
    }

    pub(crate) fn cache_directory_tree_strip_thumbnail(
        &mut self,
        request: StripThumbnailCacheRequest<'_>,
    ) {
        let StripThumbnailCacheRequest {
            index,
            decoded,
            job_key,
            stage,
            logical_size,
            buffer_tag,
            strip_max_side_used,
            ctx,
            bypass_detach_queue,
        } = request;
        let mut effective_job_key =
            job_key.or_else(|| self.directory_tree_strip_upload_key_for_current_index(index));
        let index = if let Some(key) = effective_job_key.as_mut() {
            if !self.refresh_strip_job_key_index(key) {
                self.clear_strip_preview_attempt_state_for_key(key);
                return;
            }
            key.index
        } else if index < self.image_files.len() {
            index
        } else {
            return;
        };
        let cache_path = effective_job_key
            .as_ref()
            .map(|key| key.path.clone())
            .unwrap_or_else(|| self.image_files[index].clone());
        match self.prepare_directory_tree_strip_thumbnail_cache(StripThumbnailCachePrepare {
            index,
            decoded,
            job_key: effective_job_key.as_ref(),
            stage,
            logical_size,
            buffer_tag,
            strip_max_side_used,
            source: "cache_directory_tree_strip_thumbnail",
        }) {
            StripThumbnailCacheDecision::Drop => {}
            StripThumbnailCacheDecision::Resample => {
                self.schedule_or_queue_strip_pending_gpu_resample(
                    index,
                    decoded.clone(),
                    stage,
                    logical_size,
                    buffer_tag,
                    effective_job_key.clone(),
                );
            }
            StripThumbnailCacheDecision::Proceed { strip_max_side } => {
                // Always queue uploads for a paint-time flush. Embedded nav used to
                // upsert directly from logic(); after HDR swap-chain hot-swap those
                // textures did not display even though preview snapshots looked valid.
                if !bypass_detach_queue {
                    self.queue_directory_tree_strip_gpu_upload(
                        DirectoryTreeStripGpuUploadRequest {
                            index,
                            decoded: decoded.clone(),
                            stage,
                            logical: logical_size,
                            buffer_tag,
                            strip_max_side_used,
                            job_key: effective_job_key,
                        },
                    );
                } else {
                    self.upsert_directory_tree_strip_thumbnail_decoded(StripThumbnailCacheUpsert {
                        path: &cache_path,
                        decoded,
                        stage,
                        logical_size,
                        buffer_tag,
                        strip_max_side_used,
                        ctx,
                        strip_max_side,
                    });
                }
            }
        }
        if bypass_detach_queue {
            self.publish_directory_tree_strip_preview_if_stale(ctx);
        }
    }

    pub(crate) fn cache_directory_tree_strip_thumbnail_owned(
        &mut self,
        request: StripThumbnailCacheOwnedRequest<'_>,
    ) {
        let StripThumbnailCacheOwnedRequest {
            index,
            decoded,
            job_key,
            stage,
            logical_size,
            buffer_tag,
            strip_max_side_used,
            ctx,
            bypass_detach_queue,
        } = request;
        let mut effective_job_key =
            job_key.or_else(|| self.directory_tree_strip_upload_key_for_current_index(index));
        let index = if let Some(key) = effective_job_key.as_mut() {
            if !self.refresh_strip_job_key_index(key) {
                self.clear_strip_preview_attempt_state_for_key(key);
                return;
            }
            key.index
        } else if index < self.image_files.len() {
            index
        } else {
            return;
        };
        let cache_path = effective_job_key
            .as_ref()
            .map(|key| key.path.clone())
            .unwrap_or_else(|| self.image_files[index].clone());
        match self.prepare_directory_tree_strip_thumbnail_cache(StripThumbnailCachePrepare {
            index,
            decoded: &decoded,
            job_key: effective_job_key.as_ref(),
            stage,
            logical_size,
            buffer_tag,
            strip_max_side_used,
            source: "cache_directory_tree_strip_thumbnail_owned",
        }) {
            StripThumbnailCacheDecision::Drop => {}
            StripThumbnailCacheDecision::Resample => {
                self.schedule_or_queue_strip_pending_gpu_resample(
                    index,
                    decoded,
                    stage,
                    logical_size,
                    buffer_tag,
                    effective_job_key,
                );
            }
            StripThumbnailCacheDecision::Proceed { strip_max_side } => {
                if !bypass_detach_queue {
                    self.queue_directory_tree_strip_gpu_upload(
                        DirectoryTreeStripGpuUploadRequest {
                            index,
                            decoded,
                            stage,
                            logical: logical_size,
                            buffer_tag,
                            strip_max_side_used,
                            job_key: effective_job_key,
                        },
                    );
                } else {
                    self.upsert_directory_tree_strip_thumbnail_decoded(StripThumbnailCacheUpsert {
                        path: &cache_path,
                        decoded: &decoded,
                        stage,
                        logical_size,
                        buffer_tag,
                        strip_max_side_used,
                        ctx,
                        strip_max_side,
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;
    use std::path::PathBuf;

    use super::*;
    use crate::app::directory_tree_strip_cache::DirectoryTreeStripJobToken;

    fn strip_job_key_with_token(index: usize, job_token: u64) -> DirectoryTreeStripJobKey {
        let Some(job_token) = NonZeroU64::new(job_token) else {
            panic!("test token must be non-zero");
        };
        DirectoryTreeStripJobKey {
            index,
            path: PathBuf::from(format!("image-{index}.png")),
            image_list_generation: 1,
            job_token: DirectoryTreeStripJobToken::Worker(job_token),
        }
    }

    fn strip_job_key(index: usize) -> DirectoryTreeStripJobKey {
        strip_job_key_with_token(index, 1)
    }

    fn decoded_with_marker(marker: u8) -> DecodedImage {
        decoded_with_size_marker(1, 1, marker)
    }

    fn decoded_with_size_marker(width: u32, height: u32, marker: u8) -> DecodedImage {
        let len = width as usize * height as usize * 4;
        DecodedImage::new(width, height, vec![marker; len])
    }

    fn pending_upload(
        index: usize,
        stage: PreviewStage,
        job_token: u64,
        seq: u64,
    ) -> DirectoryTreeStripPendingGpuUpload {
        let decoded = decoded_with_marker(index as u8);
        let upload_bytes = ImageViewerApp::decoded_strip_upload_bytes(&decoded);
        DirectoryTreeStripPendingGpuUpload {
            key: strip_job_key_with_token(index, job_token),
            decoded,
            upload_bytes,
            stage,
            logical: Some((1, 1)),
            buffer_tag: StripPreviewBufferTag::StripDecodedPixels,
            seq,
            strip_max_side_used: Some(1),
        }
    }

    fn make_strip_test_app() -> ImageViewerApp {
        let mut app = crate::app::image_management::tests::make_test_app();
        app.settings.browse_mode = crate::settings::BrowseMode::Tree;
        app.settings.show_directory_tree_nav = true;
        app.settings.directory_tree_show_list_previews = true;
        app
    }

    #[test]
    fn strip_pending_evict_old_token_does_not_clear_new_inflight_token() {
        let mut app = make_strip_test_app();
        let path0 = PathBuf::from("image-0.png");
        app.directory_tree_strip_pending_gpu_initial
            .push_back(pending_upload(0, PreviewStage::Initial, 1, 0));
        app.directory_tree_strip_generate_inflight
            .insert(path0.clone());
        let Some(new_token) = NonZeroU64::new(2) else {
            panic!("test token must be non-zero");
        };
        app.directory_tree_strip_inflight_tokens
            .insert(path0.clone(), new_token);

        let (dropped, released_bytes) = app.evict_strip_pending_gpu_uploads(1, 0, None);

        assert_eq!(dropped, 1);
        assert!(released_bytes > 0);
        assert!(app.directory_tree_strip_generate_inflight.contains(&path0));
        assert_eq!(
            app.directory_tree_strip_inflight_tokens.get(&path0),
            Some(&new_token)
        );
    }

    #[test]
    fn strip_pending_queue_coalesces_same_index_initial_upload() {
        let mut app = make_strip_test_app();
        app.image_files = vec![PathBuf::from("image-0.png")];
        app.directory_tree.list.lock().image_list_generation = 1;

        app.queue_directory_tree_strip_gpu_upload(DirectoryTreeStripGpuUploadRequest {
            index: 0,
            decoded: decoded_with_marker(1),
            stage: PreviewStage::Initial,
            logical: Some((1, 1)),
            buffer_tag: StripPreviewBufferTag::StripDecodedPixels,
            strip_max_side_used: Some(1),
            job_key: None,
        });
        app.queue_directory_tree_strip_gpu_upload(DirectoryTreeStripGpuUploadRequest {
            index: 0,
            decoded: decoded_with_marker(2),
            stage: PreviewStage::Initial,
            logical: Some((1, 1)),
            buffer_tag: StripPreviewBufferTag::StripDecodedPixels,
            strip_max_side_used: Some(1),
            job_key: None,
        });

        assert_eq!(app.directory_tree_strip_pending_gpu_initial.len(), 1);
        let upload = app
            .directory_tree_strip_pending_gpu_initial
            .front()
            .expect("coalesced upload should remain");
        assert_eq!(upload.key.index, 0);
        assert_eq!(upload.decoded.rgba()[0], 2);
    }

    #[test]
    fn strip_pending_queue_keeps_higher_rank_initial_upload() {
        let mut app = make_strip_test_app();
        app.image_files = vec![PathBuf::from("image-0.png")];
        app.directory_tree.list.lock().image_list_generation = 1;

        app.queue_directory_tree_strip_gpu_upload(DirectoryTreeStripGpuUploadRequest {
            index: 0,
            decoded: decoded_with_marker(1),
            stage: PreviewStage::Initial,
            logical: Some((1, 1)),
            buffer_tag: StripPreviewBufferTag::StripDecodedPixels,
            strip_max_side_used: Some(1),
            job_key: None,
        });
        app.queue_directory_tree_strip_gpu_upload(DirectoryTreeStripGpuUploadRequest {
            index: 0,
            decoded: decoded_with_marker(2),
            stage: PreviewStage::Initial,
            logical: Some((1, 1)),
            buffer_tag: StripPreviewBufferTag::PreloadSdrFallback,
            strip_max_side_used: Some(1),
            job_key: None,
        });

        assert_eq!(app.directory_tree_strip_pending_gpu_initial.len(), 1);
        let upload = app
            .directory_tree_strip_pending_gpu_initial
            .front()
            .expect("higher-rank upload should remain");
        assert_eq!(upload.key.index, 0);
        assert_eq!(upload.buffer_tag, StripPreviewBufferTag::StripDecodedPixels);
        assert_eq!(upload.decoded.rgba()[0], 1);
    }

    #[test]
    fn strip_pending_queue_keeps_iso_baseline_over_sdr_fallback() {
        let mut app = make_strip_test_app();
        app.image_files = vec![PathBuf::from("image-0.png")];
        app.directory_tree.list.lock().image_list_generation = 1;

        app.queue_directory_tree_strip_gpu_upload(DirectoryTreeStripGpuUploadRequest {
            index: 0,
            decoded: decoded_with_marker(3),
            stage: PreviewStage::Initial,
            logical: Some((1, 1)),
            buffer_tag: StripPreviewBufferTag::IsoGainMapBaseline,
            strip_max_side_used: Some(1),
            job_key: None,
        });
        app.queue_directory_tree_strip_gpu_upload(DirectoryTreeStripGpuUploadRequest {
            index: 0,
            decoded: decoded_with_marker(4),
            stage: PreviewStage::Initial,
            logical: Some((1, 1)),
            buffer_tag: StripPreviewBufferTag::PreloadSdrFallback,
            strip_max_side_used: Some(1),
            job_key: None,
        });

        assert_eq!(app.directory_tree_strip_pending_gpu_initial.len(), 1);
        let upload = app
            .directory_tree_strip_pending_gpu_initial
            .front()
            .expect("ISO baseline upload should remain");
        assert_eq!(upload.key.index, 0);
        assert_eq!(upload.buffer_tag, StripPreviewBufferTag::IsoGainMapBaseline);
        assert_eq!(upload.decoded.rgba()[0], 3);
    }

    #[test]
    fn strip_gpu_queue_routes_full_size_upload_to_resample() {
        let mut app = make_strip_test_app();
        app.image_files = vec![PathBuf::from("image-0.png")];
        app.directory_tree.list.lock().image_list_generation = 1;

        app.queue_directory_tree_strip_gpu_upload(DirectoryTreeStripGpuUploadRequest {
            index: 0,
            decoded: decoded_with_size_marker(256, 128, 5),
            stage: PreviewStage::Initial,
            logical: Some((256, 128)),
            buffer_tag: StripPreviewBufferTag::PreloadSdrFallback,
            strip_max_side_used: None,
            job_key: None,
        });

        assert!(app.directory_tree_strip_pending_gpu_initial.is_empty());
        assert!(app.directory_tree_strip_pending_gpu_refined.is_empty());
        assert!(app.strip_generate_inflight_contains_index(0));
    }

    #[test]
    fn strip_gpu_queue_keeps_ready_pending_upload_over_full_size_repeat() {
        let mut app = make_strip_test_app();
        app.image_files = vec![PathBuf::from("image-0.png")];
        app.directory_tree.list.lock().image_list_generation = 1;
        let strip_max_side = app
            .settings
            .directory_tree_list_preview_size
            .strip_max_side();

        app.queue_directory_tree_strip_gpu_upload(DirectoryTreeStripGpuUploadRequest {
            index: 0,
            decoded: decoded_with_marker(6),
            stage: PreviewStage::Initial,
            logical: Some((1, 1)),
            buffer_tag: StripPreviewBufferTag::PreloadSdrFallback,
            strip_max_side_used: Some(strip_max_side),
            job_key: None,
        });
        app.queue_directory_tree_strip_gpu_upload(DirectoryTreeStripGpuUploadRequest {
            index: 0,
            decoded: decoded_with_size_marker(256, 128, 7),
            stage: PreviewStage::Initial,
            logical: Some((256, 128)),
            buffer_tag: StripPreviewBufferTag::PreloadSdrFallback,
            strip_max_side_used: None,
            job_key: None,
        });

        assert_eq!(app.directory_tree_strip_pending_gpu_initial.len(), 1);
        assert!(app.directory_tree_strip_pending_gpu_refined.is_empty());
        assert!(!app.strip_generate_inflight_contains_index(0));
        let upload = app
            .directory_tree_strip_pending_gpu_initial
            .front()
            .expect("ready upload should remain");
        assert_eq!(upload.decoded.rgba()[0], 6);
        assert_eq!(upload.logical, Some((1, 1)));
    }

    #[test]
    fn strip_pending_byte_eviction_preserves_visible_and_drops_offscreen_first() {
        let mut app = make_strip_test_app();
        app.image_files = vec![
            PathBuf::from("image-0.png"),
            PathBuf::from("image-1.png"),
            PathBuf::from("image-2.png"),
        ];
        app.directory_tree_strip_pending_gpu_initial
            .push_back(pending_upload(1, PreviewStage::Initial, 11, 0));
        app.directory_tree_strip_pending_gpu_initial
            .push_back(pending_upload(0, PreviewStage::Initial, 10, 1));
        app.directory_tree_strip_pending_gpu_initial
            .push_back(pending_upload(2, PreviewStage::Initial, 12, 2));

        let (dropped, released_bytes) = app.evict_strip_pending_gpu_uploads(0, 1, Some((1, 3)));

        assert_eq!(dropped, 1);
        assert!(released_bytes > 0);
        let remaining: Vec<usize> = app
            .directory_tree_strip_pending_gpu_initial
            .iter()
            .map(|item| item.key.index)
            .collect();
        assert_eq!(remaining, vec![1, 2]);
    }

    #[test]
    fn strip_pending_byte_eviction_allows_visible_items_to_exceed_budget() {
        let mut app = make_strip_test_app();
        app.image_files = vec![
            PathBuf::from("image-0.png"),
            PathBuf::from("image-1.png"),
            PathBuf::from("image-2.png"),
        ];
        app.directory_tree_strip_pending_gpu_initial
            .push_back(pending_upload(1, PreviewStage::Initial, 11, 0));
        app.directory_tree_strip_pending_gpu_initial
            .push_back(pending_upload(2, PreviewStage::Initial, 12, 1));

        let (dropped, released_bytes) = app.evict_strip_pending_gpu_uploads(0, 1, Some((1, 3)));

        assert_eq!((dropped, released_bytes), (0, 0));
        assert_eq!(app.directory_tree_strip_pending_gpu_initial.len(), 2);
    }

    #[test]
    fn strip_pending_flush_rejects_generation_bumped_stale_upload() {
        let mut app = make_strip_test_app();
        app.image_files = vec![PathBuf::from("image-0.png")];
        app.directory_tree.list.lock().image_list_generation = 2;
        app.directory_tree_strip_pending_gpu_initial
            .push_back(pending_upload(0, PreviewStage::Initial, 1, 0));
        let ctx = egui::Context::default();

        app.flush_directory_tree_strip_pending_gpu_uploads(&ctx);

        assert!(!app.strip_cache_contains_index(0));
        assert!(app.directory_tree_strip_pending_gpu_initial.is_empty());
    }

    #[test]
    fn strip_pending_flush_after_reorder_writes_cache_by_path_not_stale_index() {
        // Column sort keeps image_list_generation; pending uploads still carry the
        // submit-time index. Flush must upsert under key.path, not image_files[old_index].
        let mut app = make_strip_test_app();
        let path_a = PathBuf::from("image-a.png");
        let path_b = PathBuf::from("image-b.png");
        app.image_files = vec![path_a.clone(), path_b.clone()];
        app.directory_tree.list.lock().image_list_generation = 1;

        let mut upload = pending_upload(0, PreviewStage::Initial, 1, 0);
        upload.key.path = path_a.clone();
        upload.decoded = decoded_with_marker(0xAA);
        upload.upload_bytes = ImageViewerApp::decoded_strip_upload_bytes(&upload.decoded);
        app.directory_tree_strip_pending_gpu_initial
            .push_back(upload);

        // Reorder like a UI column sort: A moves from index 0 -> 1; generation unchanged.
        app.image_files = vec![path_b.clone(), path_a.clone()];
        app.cached_image_strip_path_index = None;
        let ctx = egui::Context::default();

        app.flush_directory_tree_strip_pending_gpu_uploads(&ctx);

        assert!(
            app.directory_tree_strip_cache.contains(&path_a),
            "decoded pixels for A must land under path A after reorder"
        );
        assert!(
            !app.directory_tree_strip_cache.contains(&path_b),
            "stale submit index 0 must not write A's pixels into B's cache slot"
        );
        assert!(app.directory_tree_strip_pending_gpu_initial.is_empty());
    }

    #[test]
    fn strip_pending_coalesce_matches_by_path_across_stale_indices() {
        let mut app = make_strip_test_app();
        let path_a = PathBuf::from("image-a.png");
        app.image_files = vec![PathBuf::from("image-b.png"), path_a.clone()];
        app.directory_tree.list.lock().image_list_generation = 1;

        // Queued under stale index 0 while path A now lives at index 1.
        let mut stale = pending_upload(0, PreviewStage::Initial, 1, 0);
        stale.key.path = path_a.clone();
        stale.decoded = decoded_with_marker(1);
        stale.upload_bytes = ImageViewerApp::decoded_strip_upload_bytes(&stale.decoded);
        app.directory_tree_strip_pending_gpu_initial
            .push_back(stale);

        app.queue_directory_tree_strip_gpu_upload(DirectoryTreeStripGpuUploadRequest {
            index: 1,
            decoded: decoded_with_marker(2),
            stage: PreviewStage::Initial,
            logical: Some((1, 1)),
            buffer_tag: StripPreviewBufferTag::StripDecodedPixels,
            strip_max_side_used: Some(1),
            job_key: Some(DirectoryTreeStripJobKey {
                index: 1,
                path: path_a,
                image_list_generation: 1,
                job_token: DirectoryTreeStripJobToken::Worker(
                    NonZeroU64::new(2).expect("non-zero"),
                ),
            }),
        });

        assert_eq!(app.directory_tree_strip_pending_gpu_initial.len(), 1);
        let upload = app
            .directory_tree_strip_pending_gpu_initial
            .front()
            .expect("path-coalesced upload");
        assert_eq!(upload.key.index, 1);
        assert_eq!(upload.decoded.rgba()[0], 2);
    }

    #[test]
    fn strip_pending_key_visibility_treats_empty_range_as_not_visible() {
        let key = strip_job_key(3);
        let path_to_index = std::collections::HashMap::from([(key.path.clone(), 3usize)]);

        assert!(!ImageViewerApp::strip_pending_key_is_visible(
            None,
            &key,
            &path_to_index
        ));
        assert!(!ImageViewerApp::strip_pending_key_is_visible(
            Some((3, 3)),
            &key,
            &path_to_index
        ));
        assert!(ImageViewerApp::strip_pending_key_is_visible(
            Some((2, 4)),
            &key,
            &path_to_index
        ));
    }

    #[test]
    fn strip_pending_upload_budget_need_reports_count_and_byte_overflow() {
        assert_eq!(
            ImageViewerApp::pending_strip_upload_budget_need(MAX_STRIP_PENDING_GPU_UPLOADS, 0, 4,),
            (1, 0)
        );
        assert_eq!(
            ImageViewerApp::pending_strip_upload_budget_need(
                0,
                MAX_STRIP_PENDING_GPU_UPLOAD_BYTES - 4,
                8,
            ),
            (0, 4)
        );
        assert_eq!(
            ImageViewerApp::pending_strip_upload_budget_need(0, 0, 4),
            (0, 0)
        );
    }
}
