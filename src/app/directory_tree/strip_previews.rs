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

//! Directory-tree strip thumbnail generation, polling, and cache invalidation.

use std::sync::Arc;

use eframe::egui;

use crate::app::ImageViewerApp;
use crate::app::directory_tree_strip_cache::{
    DirectoryTreeStripPendingGpuUpload, DirectoryTreeStripPreviewJobResult,
    MAX_STRIP_GPU_UPLOADS_PER_PAINT, MAX_STRIP_PENDING_GPU_UPLOADS, decoded_rgba_size_valid,
};
use crate::loader::DIRECTORY_TREE_STRIP_POOL;
use crate::loader::{
    DecodedImage, PreviewStage, TiledImageSource, generate_directory_tree_thumb_from_path,
    preview_aspect_matches_logical,
};

#[cfg(target_os = "windows")]
use super::workers::ensure_strip_worker_com_initialized;
use super::{
    BOOTSTRAP_STRIP_VISIBLE_ROW_CAP, DIRECTORY_TREE_COLD_NEIGHBOR_RADIUS,
    DirectoryTreeListPreviewLayout, MAX_COLD_STRIP_GENERATES_PER_FRAME,
    MAX_COLD_STRIP_GENERATES_PER_FRAME_BOOTSTRAP, MAX_DIRECTORY_TREE_STRIP_BOOTSTRAP_FRAMES,
    MAX_STRIP_GENERATE_INFLIGHT, MAX_STRIP_GENERATE_INFLIGHT_BOOTSTRAP,
    MAX_TILED_STRIP_GENERATES_PER_FRAME, domains, view,
};

fn send_strip_inflight_release(release_tx: &crossbeam_channel::Sender<usize>, index: usize) {
    if let Err(err) = release_tx.try_send(index) {
        log::warn!("[DirectoryTree] Strip inflight release dropped for index {index}: {err}");
    }
}

impl ImageViewerApp {
    fn evict_strip_pending_gpu_uploads(&mut self, need: usize) -> usize {
        if need == 0 {
            return 0;
        }
        let mut dropped_indices = Vec::new();
        let mut still_need = need;

        if still_need > 0 {
            let mut kept = Vec::with_capacity(self.directory_tree_strip_pending_gpu.len());
            for item in self.directory_tree_strip_pending_gpu.drain(..) {
                if still_need > 0 && item.stage == PreviewStage::Initial {
                    dropped_indices.push(item.index);
                    still_need -= 1;
                } else {
                    kept.push(item);
                }
            }
            self.directory_tree_strip_pending_gpu = kept;
        }

        if still_need > 0 {
            let drop_count = still_need.min(self.directory_tree_strip_pending_gpu.len());
            for item in self.directory_tree_strip_pending_gpu.drain(..drop_count) {
                dropped_indices.push(item.index);
            }
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

    fn queue_directory_tree_strip_gpu_upload(
        &mut self,
        index: usize,
        decoded: DecodedImage,
        stage: PreviewStage,
        logical: Option<(u32, u32)>,
    ) {
        if !self.directory_tree_list_previews_active() || index >= self.image_files.len() {
            return;
        }
        if self.directory_tree_strip_pending_gpu.len() >= MAX_STRIP_PENDING_GPU_UPLOADS {
            let dropped = self.evict_strip_pending_gpu_uploads(
                self.directory_tree_strip_pending_gpu
                    .len()
                    .saturating_sub(MAX_STRIP_PENDING_GPU_UPLOADS - 1),
            );
            log::warn!(
                "[DirectoryTree] Strip pending GPU upload queue full; dropped {dropped} item(s)"
            );
        }
        #[cfg(feature = "preload-debug")]
        let decoded_w = decoded.width;
        #[cfg(feature = "preload-debug")]
        let decoded_h = decoded.height;
        self.directory_tree_strip_pending_gpu
            .push(DirectoryTreeStripPendingGpuUpload {
                index,
                decoded,
                stage,
                logical,
            });
        #[cfg(feature = "preload-debug")]
        {
            crate::preload_debug!(
                "[PreloadDebug][StripGpu] queue idx={} stage={:?} decoded={}x{} logical={:?} \
                 cache_contains={} cache_count={} pending_len={}",
                index,
                stage,
                decoded_w,
                decoded_h,
                logical,
                self.directory_tree_strip_cache.contains(index),
                self.directory_tree_strip_cache.textures().len(),
                self.directory_tree_strip_pending_gpu.len()
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
        if self.directory_tree_strip_pending_gpu.is_empty() {
            return;
        }
        let revision_before = self.directory_tree_strip_cache.gpu_revision();
        let take = MAX_STRIP_GPU_UPLOADS_PER_PAINT.min(self.directory_tree_strip_pending_gpu.len());
        let batch: Vec<_> = self
            .directory_tree_strip_pending_gpu
            .drain(..take)
            .collect();
        #[cfg(feature = "preload-debug")]
        {
            let indices: Vec<usize> = batch.iter().map(|item| item.index).collect();
            crate::preload_debug!(
                "[PreloadDebug][StripGpu] flush take={} pending_left={} indices={indices:?}",
                batch.len(),
                self.directory_tree_strip_pending_gpu.len()
            );
        }
        for item in batch {
            #[cfg(feature = "preload-debug")]
            let cache_before = self.directory_tree_strip_cache.contains(item.index);
            self.cache_directory_tree_strip_thumbnail(
                item.index,
                &item.decoded,
                item.stage,
                item.logical,
                ctx,
            );
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
        let pending_uploads_remain = !self.directory_tree_strip_pending_gpu.is_empty();
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

    fn strip_hdr_animated_awaiting_real_strip_preview(&self, index: usize) -> bool {
        self.pending_anim_frames
            .get(&index)
            .is_some_and(|pending| pending.hdr_frames.is_some())
    }

    fn strip_main_loader_sdr_unreliable_for_strip(&self, index: usize) -> bool {
        if self.hdr_placeholder_fallback_indices.contains(&index) {
            return true;
        }
        if self.strip_hdr_animated_awaiting_real_strip_preview(index) {
            return true;
        }
        self.ultra_hdr_capacity_sensitive_indices.contains(&index)
            && self.hdr_sdr_fallback_indices.contains(&index)
    }

    pub(crate) fn invalidate_directory_tree_strip_preview_for_index(&mut self, index: usize) {
        self.directory_tree_strip_cache.remove_index(index);
        self.directory_tree_strip_cold_attempted.remove(&index);
        self.directory_tree_strip_generate_inflight.remove(&index);
        self.directory_tree_strip_tiled_attempted.remove(&index);
    }

    fn strip_skip_texture_cache_sync_for_deferred_black_sdr(&self, index: usize) -> bool {
        if self.strip_main_loader_sdr_unreliable_for_strip(index) {
            return true;
        }
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
        if hdr
            .metadata
            .gain_map
            .as_ref()
            .and_then(|g| g.iso_deferred.as_ref())
            .is_some()
        {
            return false;
        }
        crate::loader::libraw_scene_linear_needs_eager_sdr_fallback(hdr.as_ref())
            && !crate::loader::hdr_display_requests_sdr_preview(self.ultra_hdr_decode_capacity)
    }

    pub(crate) fn cache_directory_tree_strip_thumbnail(
        &mut self,
        index: usize,
        decoded: &crate::loader::DecodedImage,
        stage: crate::loader::PreviewStage,
        logical_size: Option<(u32, u32)>,
        ctx: &egui::Context,
    ) {
        if !self.directory_tree_list_previews_active() || index >= self.image_files.len() {
            return;
        }
        if decoded.is_sdr_deferred_placeholder() {
            #[cfg(feature = "preload-debug")]
            crate::preload_debug!(
                "[PreloadDebug][Strip] skip strip cache idx={} reason=black_placeholder",
                index
            );
            return;
        }
        let strip_max_side = self
            .settings
            .directory_tree_list_preview_size
            .strip_max_side();
        if let Some(logical) = logical_size {
            if self.strip_skip_texture_cache_sync_for_deferred_black_sdr(index)
                && self
                    .directory_tree_strip_cache
                    .is_valid_for_logical(index, logical)
            {
                let from_full_install = self.hdr_image_cache.get(&index).is_some_and(|hdr| {
                    logical.0 == hdr.width
                        && logical.1 == hdr.height
                        && decoded.width == hdr.width
                        && decoded.height == hdr.height
                });
                if !from_full_install {
                    #[cfg(feature = "preload-debug")]
                    crate::preload_debug!(
                        "[PreloadDebug][Strip] skip strip cache idx={} reason=keep_cold_strip \
                         logical={logical:?} decoded={}x{} hdr_cached={}",
                        index,
                        decoded.width,
                        decoded.height,
                        self.hdr_image_cache
                            .get(&index)
                            .map(|hdr| format!("{}x{}", hdr.width, hdr.height))
                            .unwrap_or_else(|| "none".to_string())
                    );
                    return;
                }
            }
        }
        if self.directory_tree_nav_is_detached() {
            self.queue_directory_tree_strip_gpu_upload(index, decoded.clone(), stage, logical_size);
            return;
        }
        let allow_initial_over_refined = self.strip_main_loader_sdr_unreliable_for_strip(index);
        self.directory_tree_strip_cache.upsert_from_decoded(
            index,
            decoded,
            stage,
            logical_size,
            ctx,
            self.current_index,
            self.image_files.len(),
            strip_max_side,
            allow_initial_over_refined,
        );
    }

    pub(crate) fn directory_tree_strip_logical_size(&self, index: usize) -> Option<(u32, u32)> {
        if let Some((width, height)) = self.texture_cache.get_original_res(index) {
            return Some((width, height));
        }
        if let Some(&(width, height)) = self.directory_tree_strip_cache.logical_sizes().get(&index)
        {
            return Some((width, height));
        }
        if let Some(tm) = self.prefetched_tiles.get(&index) {
            let source = tm.get_source();
            return Some((source.width(), source.height()));
        }
        if let Some(tm) = self.tile_manager.as_ref()
            && tm.image_index == index
        {
            let source = tm.get_source();
            return Some((source.width(), source.height()));
        }
        None
    }

    fn tiled_sdr_source_for_index(&self, index: usize) -> Option<Arc<dyn TiledImageSource>> {
        if let Some(tm) = self.prefetched_tiles.get(&index) {
            return Some(tm.get_source());
        }
        if let Some(tm) = self.tile_manager.as_ref()
            && tm.image_index == index
        {
            return Some(tm.get_source());
        }
        None
    }

    pub(crate) fn try_sync_strip_from_tile_manager_preview(&mut self, index: usize) {
        // Main-window tile previews live on the ROOT egui context; cloning their
        // TextureHandle into the strip cache breaks painting on the detached nav viewport.
        if self.directory_tree_nav_is_detached() {
            return;
        }
        let Some(logical) = self.directory_tree_strip_logical_size(index) else {
            return;
        };
        let preview_texture = self
            .prefetched_tiles
            .get(&index)
            .and_then(|tm| tm.preview_texture.as_ref())
            .or_else(|| {
                self.tile_manager
                    .as_ref()
                    .filter(|tm| tm.image_index == index)
                    .and_then(|tm| tm.preview_texture.as_ref())
            });
        let Some(texture) = preview_texture else {
            return;
        };
        let size = texture.size();
        let preview_w = size[0] as u32;
        let preview_h = size[1] as u32;
        if !preview_aspect_matches_logical(preview_w, preview_h, logical.0, logical.1) {
            return;
        }
        let incoming_max = preview_w.max(preview_h);
        if self
            .directory_tree_strip_cache
            .is_valid_for_logical(index, logical)
        {
            if self
                .directory_tree_strip_cache
                .cached_preview_max_side(index)
                .is_some_and(|cached_max| incoming_max <= cached_max)
            {
                return;
            }
        }
        self.directory_tree_strip_cache.insert_from_texture_handle(
            index,
            texture.clone(),
            crate::loader::PreviewStage::Refined,
            incoming_max,
            Some(logical),
            self.current_index,
            self.image_files.len(),
        );
    }

    pub(crate) fn try_sync_strip_from_texture_cache(&mut self, index: usize) {
        // Main-window texture_cache handles are ROOT-context textures; the detached
        // directory-tree viewport must upload strip thumbs via its own egui context.
        if self.directory_tree_nav_is_detached() {
            return;
        }
        if self.strip_skip_texture_cache_sync_for_deferred_black_sdr(index) {
            #[cfg(feature = "preload-debug")]
            crate::preload_debug!(
                "[PreloadDebug][Strip] skip texture_cache sync idx={} reason=deferred_black_sdr",
                index
            );
            return;
        }
        let Some(logical) = self.directory_tree_strip_logical_size(index) else {
            return;
        };
        if self
            .directory_tree_strip_cache
            .is_valid_for_logical(index, logical)
        {
            return;
        }
        let Some(texture) = self.texture_cache.get(index).cloned() else {
            return;
        };
        let size = texture.size();
        let preview_w = size[0] as u32;
        let preview_h = size[1] as u32;
        if !preview_aspect_matches_logical(preview_w, preview_h, logical.0, logical.1) {
            return;
        }
        let incoming_max = preview_w.max(preview_h);
        self.directory_tree_strip_cache.insert_from_texture_handle(
            index,
            texture,
            crate::loader::PreviewStage::Refined,
            incoming_max,
            Some(logical),
            self.current_index,
            self.image_files.len(),
        );
        #[cfg(feature = "preload-debug")]
        crate::preload_debug!(
            "[PreloadDebug][DirTree] strip sync from texture_cache idx={} logical={}x{} tex={}x{} cache_rev={}",
            index,
            logical.0,
            logical.1,
            preview_w,
            preview_h,
            self.directory_tree_strip_cache.gpu_revision()
        );
    }

    fn strip_index_needs_cold_thumbnail(&self, index: usize) -> bool {
        if index >= self.image_files.len() {
            return false;
        }
        if self.tiled_sdr_source_for_index(index).is_some() {
            return false;
        }
        if !self.strip_main_loader_sdr_unreliable_for_strip(index)
            && self
                .deferred_sdr_uploads
                .get(&index)
                .is_some_and(|decoded| !decoded.is_sdr_deferred_placeholder())
        {
            return false;
        }
        if self.directory_tree_strip_generate_inflight.contains(&index) {
            return false;
        }
        if self.directory_tree_strip_cold_attempted.contains(&index) {
            return false;
        }
        let cached_preview_authoritative = !self.strip_main_loader_sdr_unreliable_for_strip(index);
        if let Some(logical) = self.directory_tree_strip_logical_size(index) {
            if cached_preview_authoritative
                && self
                    .directory_tree_strip_cache
                    .is_valid_for_logical(index, logical)
            {
                return false;
            }
        } else if cached_preview_authoritative && self.directory_tree_strip_cache.contains(index) {
            return false;
        }
        true
    }

    /// Visible image-list row indices used for strip prefetch scheduling (unit tests).
    #[cfg(test)]
    pub(super) fn visible_strip_row_indices(
        visible_row_range: Option<(usize, usize)>,
        scroll_to_current_pending: bool,
        total: usize,
        bootstrap_visible: bool,
    ) -> Vec<usize> {
        if total == 0 {
            return Vec::new();
        }
        if scroll_to_current_pending && !bootstrap_visible {
            return Vec::new();
        }
        if let Some((start, end)) = visible_row_range {
            return (start..end.min(total)).collect();
        }
        if bootstrap_visible {
            return (0..total.min(BOOTSTRAP_STRIP_VISIBLE_ROW_CAP)).collect();
        }
        Vec::new()
    }

    fn collect_cold_strip_thumbnail_candidates(
        &self,
        visible_row_range: Option<(usize, usize)>,
        scroll_to_current_pending: bool,
        bootstrap_visible: bool,
        schedule_budget: usize,
    ) -> Vec<usize> {
        let total = self.image_files.len();
        if total == 0 || schedule_budget == 0 {
            return Vec::new();
        }
        if scroll_to_current_pending && !bootstrap_visible {
            return Vec::new();
        }
        let current = self.current_index.min(total.saturating_sub(1));
        let mut ordered = Vec::with_capacity(schedule_budget.min(8));
        let mut seen = std::collections::HashSet::new();
        let mut try_push = |index: usize| -> bool {
            if index < total && seen.insert(index) && self.strip_index_needs_cold_thumbnail(index) {
                ordered.push(index);
            }
            ordered.len() >= schedule_budget
        };

        if bootstrap_visible {
            if try_push(current) {
                return ordered;
            }
        }

        if let Some((start, end)) = visible_row_range {
            for index in start..end.min(total) {
                if try_push(index) {
                    return ordered;
                }
            }
        } else if bootstrap_visible {
            for index in 0..total.min(BOOTSTRAP_STRIP_VISIBLE_ROW_CAP) {
                if try_push(index) {
                    return ordered;
                }
            }
        }

        if !bootstrap_visible {
            if try_push(current) {
                return ordered;
            }
        }

        for delta in 1..=DIRECTORY_TREE_COLD_NEIGHBOR_RADIUS {
            if try_push(current.saturating_sub(delta)) {
                return ordered;
            }
            if current + delta < total && try_push(current + delta) {
                return ordered;
            }
        }

        ordered
    }

    pub(crate) fn try_generate_cold_directory_tree_strip_thumbnail(&mut self, index: usize) {
        if !self.strip_index_needs_cold_thumbnail(index) {
            return;
        }
        let path = self.image_files[index].clone();
        let Some(list) = self.directory_tree.list.try_lock() else {
            return;
        };
        let list_generation = list.image_list_generation;
        self.directory_tree_strip_cold_attempted.insert(index);
        self.directory_tree_strip_generate_inflight.insert(index);
        let tx = self.directory_tree_strip_preview_tx.clone();
        let release_tx = self.directory_tree_strip_inflight_release_tx.clone();
        let root_wake = self.root_redraw_wake_handle();
        let max_side = self
            .settings
            .directory_tree_list_preview_size
            .strip_max_side();
        crate::preload_debug!(
            "[PreloadDebug][Strip] pool submit idx={} path={} kind=cold max_side={}",
            index,
            path.display(),
            max_side
        );
        DIRECTORY_TREE_STRIP_POOL.spawn(move || {
            crate::preload_debug!(
                "[PreloadDebug][Strip] cold worker start idx={} path={}",
                index,
                path.display()
            );
            #[cfg(target_os = "windows")]
            let com_ok = ensure_strip_worker_com_initialized();
            #[cfg(not(target_os = "windows"))]
            let com_ok = true;

            let mut decoded = DecodedImage::new(0, 0, Vec::new());
            let mut logical = (0u32, 0u32);
            if com_ok {
                match generate_directory_tree_thumb_from_path(&path, max_side) {
                    Ok((preview, logical_size)) => {
                        decoded = preview;
                        logical = logical_size;
                    }
                    Err(err) => {
                        log::warn!(
                            "[DirectoryTree] Cold strip preview failed for index {index} ({}): {err}",
                            path.display()
                        );
                    }
                }
            } else {
                log::warn!(
                    "[DirectoryTree] COM init failed for cold strip preview worker index {index}"
                );
            }
            crate::preload_debug!(
                "[PreloadDebug][Strip] cold worker done idx={} out={}x{} logical={}x{} aspect_ok={} placeholder={}",
                index,
                decoded.width,
                decoded.height,
                logical.0,
                logical.1,
                preview_aspect_matches_logical(
                    decoded.width,
                    decoded.height,
                    logical.0,
                    logical.1,
                ),
                decoded.is_sdr_deferred_placeholder()
            );
            let job = DirectoryTreeStripPreviewJobResult {
                index,
                path,
                image_list_generation: list_generation,
                decoded,
                logical,
                stage: PreviewStage::Initial,
            };
            let send_result = tx.try_send(job);
            if send_result.is_ok() {
                if let Some(wake) = &root_wake {
                    wake();
                }
            } else if let Err(err) = send_result {
                log::warn!(
                    "[DirectoryTree] Cold strip preview result dropped for index {index}: {err}"
                );
                send_strip_inflight_release(&release_tx, index);
            }
        });
    }

    fn clear_strip_preview_attempt_state(&mut self, index: usize) {
        self.directory_tree_strip_generate_inflight.remove(&index);
        self.directory_tree_strip_tiled_attempted.remove(&index);
        self.directory_tree_strip_cold_attempted.remove(&index);
    }

    /// Drop inflight bookkeeping without clearing a completed cold attempt (avoids retry loops).
    fn finish_strip_preview_job(&mut self, index: usize) {
        self.directory_tree_strip_generate_inflight.remove(&index);
        self.directory_tree_strip_tiled_attempted.remove(&index);
    }

    fn strip_preview_failure_is_permanent(result: &DirectoryTreeStripPreviewJobResult) -> bool {
        result.decoded.width == 0
            || result.decoded.height == 0
            || !decoded_rgba_size_valid(&result.decoded)
            || result.decoded.is_sdr_deferred_placeholder()
            || !preview_aspect_matches_logical(
                result.decoded.width,
                result.decoded.height,
                result.logical.0,
                result.logical.1,
            )
    }

    fn abandon_strip_preview_attempt_after_failure(&mut self, index: usize) {
        self.finish_strip_preview_job(index);
        // Keep `cold_attempted` so undecodable files (e.g. motion-video JPG) do not monopolize
        // the limited cold-generate budget and block thumbnails for neighboring rows.
    }

    fn strip_preview_result_matches_index(
        &self,
        result: &DirectoryTreeStripPreviewJobResult,
    ) -> bool {
        self.image_files.get(result.index) == Some(&result.path)
    }

    fn try_apply_relocated_strip_preview_result(
        &mut self,
        result: DirectoryTreeStripPreviewJobResult,
        ctx: &egui::Context,
    ) -> bool {
        self.clear_strip_preview_attempt_state(result.index);
        let Some(new_index) = self
            .image_files
            .iter()
            .position(|path| path == &result.path)
        else {
            return false;
        };
        if new_index != result.index {
            self.clear_strip_preview_attempt_state(new_index);
        }

        if Self::strip_preview_failure_is_permanent(&result) {
            self.abandon_strip_preview_attempt_after_failure(new_index);
            return false;
        }

        self.queue_directory_tree_strip_gpu_upload(
            new_index,
            result.decoded,
            result.stage,
            Some(result.logical),
        );
        if !self
            .directory_tree_strip_cache
            .is_valid_for_logical(new_index, result.logical)
        {
            self.directory_tree_strip_tiled_attempted.remove(&new_index);
            return false;
        }
        ctx.request_repaint();
        ctx.request_repaint_of(self.directory_tree_repaint_viewport_id());
        true
    }

    pub(crate) fn poll_directory_tree_strip_preview_results(&mut self, ctx: &egui::Context) {
        while let Ok(index) = self.directory_tree_strip_inflight_release_rx.try_recv() {
            self.clear_strip_preview_attempt_state(index);
        }
        while let Ok(result) = self.directory_tree_strip_preview_rx.try_recv() {
            self.directory_tree_strip_generate_inflight
                .remove(&result.index);
            // Lock available: exact generation match rejects any reorder since the job started.
            // Lock contended: snapshot may lag the live list; accept result.gen >= snapshot.gen
            // so we do not discard valid work, and rely on path/index relocate for edge cases.
            #[allow(unused_variables)]
            let (stale, active_list_generation) = match self.directory_tree.list.try_lock() {
                Some(list) => (
                    result.image_list_generation != list.image_list_generation,
                    list.image_list_generation,
                ),
                None => {
                    let snapshot_gen = self
                        .directory_tree
                        .list_snapshot
                        .load()
                        .image_list_generation;
                    (result.image_list_generation < snapshot_gen, snapshot_gen)
                }
            };
            if stale {
                #[cfg(feature = "preload-debug")]
                crate::preload_debug!(
                    "[PreloadDebug][DirTree] strip result stale gen idx={} job_gen={} active_gen={}",
                    result.index,
                    result.image_list_generation,
                    active_list_generation
                );
                if Self::strip_preview_failure_is_permanent(&result) {
                    self.abandon_strip_preview_attempt_after_failure(result.index);
                } else {
                    self.clear_strip_preview_attempt_state(result.index);
                }
                continue;
            }
            if !self.strip_preview_result_matches_index(&result) {
                let index = result.index;
                if !self.try_apply_relocated_strip_preview_result(result, ctx) {
                    log::debug!(
                        "[DirectoryTree] Strip preview relocation failed for index {}",
                        index
                    );
                }
                continue;
            }
            if Self::strip_preview_failure_is_permanent(&result) {
                if result.decoded.width == 0 || result.decoded.height == 0 {
                    log::debug!(
                        "[DirectoryTree] Strip preview unavailable for index {} ({})",
                        result.index,
                        result.path.display()
                    );
                } else if !decoded_rgba_size_valid(&result.decoded) {
                    log::warn!(
                        "[DirectoryTree] Strip preview job size mismatch for index {}: {}x{}",
                        result.index,
                        result.decoded.width,
                        result.decoded.height
                    );
                } else {
                    log::warn!(
                        "[DirectoryTree] Strip preview job aspect mismatch for index {}: {}x{} vs {}x{}",
                        result.index,
                        result.decoded.width,
                        result.decoded.height,
                        result.logical.0,
                        result.logical.1
                    );
                }
                self.abandon_strip_preview_attempt_after_failure(result.index);
                continue;
            }
            #[cfg(feature = "preload-debug")]
            let decoded_w = result.decoded.width;
            #[cfg(feature = "preload-debug")]
            let decoded_h = result.decoded.height;
            self.queue_directory_tree_strip_gpu_upload(
                result.index,
                result.decoded,
                result.stage,
                Some(result.logical),
            );
            let cache_valid = self
                .directory_tree_strip_cache
                .is_valid_for_logical(result.index, result.logical);
            #[cfg(feature = "preload-debug")]
            {
                crate::preload_debug!(
                    "[PreloadDebug][StripPoll] idx={} path={} stage={:?} decoded={}x{} logical={}x{} \
                     cache_valid_before_flush={cache_valid} cold_attempted={} pending_len={}",
                    result.index,
                    result.path.display(),
                    result.stage,
                    decoded_w,
                    decoded_h,
                    result.logical.0,
                    result.logical.1,
                    self.directory_tree_strip_cold_attempted
                        .contains(&result.index),
                    self.directory_tree_strip_pending_gpu.len()
                );
            }
            if !cache_valid {
                self.directory_tree_strip_tiled_attempted
                    .remove(&result.index);
                #[cfg(feature = "preload-debug")]
                crate::preload_debug!(
                    "[PreloadDebug][StripPoll] idx={} no repaint (cache not valid until GPU flush)",
                    result.index
                );
            } else {
                self.finish_strip_preview_job(result.index);
                ctx.request_repaint();
                ctx.request_repaint_of(self.directory_tree_repaint_viewport_id());
                #[cfg(feature = "preload-debug")]
                crate::preload_debug!(
                    "[PreloadDebug][StripPoll] idx={} repaint requested (cache already valid)",
                    result.index
                );
            }
        }
    }

    pub(crate) fn try_generate_directory_tree_strip_from_tiled_source(&mut self, index: usize) {
        if self.directory_tree_strip_tiled_attempted.contains(&index)
            || self.directory_tree_strip_generate_inflight.contains(&index)
        {
            return;
        }
        let Some(source) = self.tiled_sdr_source_for_index(index) else {
            return;
        };
        let logical = (source.width(), source.height());
        if self
            .directory_tree_strip_cache
            .is_valid_for_logical(index, logical)
        {
            return;
        }

        let path = self.image_files.get(index).cloned().unwrap_or_default();
        let Some(list) = self.directory_tree.list.try_lock() else {
            return;
        };
        let list_generation = list.image_list_generation;
        self.directory_tree_strip_tiled_attempted.insert(index);
        self.directory_tree_strip_generate_inflight.insert(index);
        let source = Arc::clone(&source);
        let tx = self.directory_tree_strip_preview_tx.clone();
        let release_tx = self.directory_tree_strip_inflight_release_tx.clone();
        let root_wake = self.root_redraw_wake_handle();
        let max_side = self
            .settings
            .directory_tree_list_preview_size
            .strip_max_side();
        crate::preload_debug!(
            "[PreloadDebug][Strip] pool submit idx={} path={} kind=tiled logical={}x{} max_side={}",
            index,
            path.display(),
            logical.0,
            logical.1,
            max_side
        );
        DIRECTORY_TREE_STRIP_POOL.spawn(move || {
            crate::preload_debug!(
                "[PreloadDebug][Strip] worker start idx={} logical={}x{} max_side={}",
                index,
                logical.0,
                logical.1,
                max_side
            );
            #[cfg(target_os = "windows")]
            let com_ok = ensure_strip_worker_com_initialized();
            #[cfg(not(target_os = "windows"))]
            let com_ok = true;
            // SAFETY: panic in generate_full_image_preview is caught below; the rayon worker
            // thread stays healthy without spawning a nested OS thread.
            let preview_result = if com_ok {
                Some(std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                    || source.generate_full_image_preview(max_side, max_side),
                )))
            } else {
                log::warn!(
                    "[DirectoryTree] COM init failed for strip preview worker index {index}"
                );
                None
            };
            let mut decoded = DecodedImage::new(0, 0, Vec::new());
            if let Some(preview_result) = preview_result {
                match preview_result {
                    Ok((pw, ph, pixels)) if pw > 0 && ph > 0 => {
                        decoded = DecodedImage::new(pw, ph, pixels);
                    }
                    Ok(_) | Err(_) => {}
                }
            }
            crate::preload_debug!(
                "[PreloadDebug][Strip] worker done idx={} out={}x{} logical={}x{} aspect_ok={}",
                index,
                decoded.width,
                decoded.height,
                logical.0,
                logical.1,
                preview_aspect_matches_logical(decoded.width, decoded.height, logical.0, logical.1,)
            );
            let job = DirectoryTreeStripPreviewJobResult {
                index,
                path,
                image_list_generation: list_generation,
                decoded,
                logical,
                stage: PreviewStage::Refined,
            };
            let send_result = tx.try_send(job);
            if send_result.is_ok() {
                if let Some(wake) = &root_wake {
                    wake();
                }
            } else if let Err(err) = send_result {
                log::warn!("[DirectoryTree] Strip preview result dropped for index {index}: {err}");
                send_strip_inflight_release(&release_tx, index);
            }
        });
    }

    pub(crate) fn ensure_directory_tree_strip_thumbnails(&mut self, ctx: &egui::Context) {
        if !self.directory_tree_list_previews_active() {
            return;
        }

        self.poll_directory_tree_strip_preview_results(ctx);

        // Do not drop `cold_attempted` here when cache is empty: failed decodes (e.g. motion-video
        // JPG) stay out of cache but must remain attempted so they do not monopolize cold slots.
        self.directory_tree_strip_tiled_attempted.retain(|index| {
            self.directory_tree_strip_cache.contains(*index)
                || self.directory_tree_strip_generate_inflight.contains(index)
        });

        let mut tiled_indices: Vec<usize> = self.prefetched_tiles.keys().copied().collect();
        if let Some(tm) = &self.tile_manager {
            if !tiled_indices.contains(&tm.image_index) {
                tiled_indices.push(tm.image_index);
            }
        }
        let current = self.current_index;
        let file_count = self.image_files.len();
        let total = file_count.max(1);
        tiled_indices.sort_by_key(|&idx| {
            if idx == current {
                0
            } else {
                let forward = (idx + total - current) % total;
                let backward = (current + total - idx) % total;
                1 + forward.min(backward)
            }
        });

        for index in &tiled_indices {
            let Some(logical) = self.directory_tree_strip_logical_size(*index) else {
                continue;
            };
            if self
                .directory_tree_strip_cache
                .invalidate_if_invalid(*index, logical)
            {
                #[cfg(feature = "preload-debug")]
                crate::preload_debug!(
                    "[PreloadDebug][Strip] invalidate idx={} logical={}x{} (aspect mismatch vs cached texture)",
                    index,
                    logical.0,
                    logical.1
                );
                self.directory_tree_strip_tiled_attempted.remove(index);
            }
            self.try_sync_strip_from_tile_manager_preview(*index);
            self.try_sync_strip_from_texture_cache(*index);
        }

        if file_count > 0 {
            let current = self.current_index.min(file_count - 1);
            self.try_sync_strip_from_texture_cache(current);
            for delta in 1..=DIRECTORY_TREE_COLD_NEIGHBOR_RADIUS {
                if current >= delta {
                    self.try_sync_strip_from_texture_cache(current - delta);
                }
                if current + delta < file_count {
                    self.try_sync_strip_from_texture_cache(current + delta);
                }
            }
        }

        let mut generated_this_frame = 0usize;
        for index in tiled_indices {
            let Some(logical) = self.directory_tree_strip_logical_size(index) else {
                continue;
            };
            if self
                .directory_tree_strip_cache
                .is_valid_for_logical(index, logical)
            {
                continue;
            }
            if generated_this_frame >= MAX_TILED_STRIP_GENERATES_PER_FRAME {
                break;
            }
            self.try_generate_directory_tree_strip_from_tiled_source(index);
            generated_this_frame += 1;
        }

        let deferred_indices: Vec<usize> = self.deferred_sdr_uploads.keys().copied().collect();
        for index in deferred_indices {
            if self.tiled_sdr_source_for_index(index).is_some() {
                continue;
            }
            if self.strip_main_loader_sdr_unreliable_for_strip(index) {
                continue;
            }
            if self.directory_tree_strip_cache.contains(index) {
                continue;
            }
            if self
                .deferred_sdr_uploads
                .get(&index)
                .is_some_and(DecodedImage::is_sdr_deferred_placeholder)
            {
                continue;
            }
            let Some(decoded) = self.deferred_sdr_uploads.get(&index).cloned() else {
                continue;
            };
            self.queue_directory_tree_strip_gpu_upload(
                index,
                decoded,
                PreviewStage::Initial,
                self.directory_tree_strip_logical_size(index),
            );
        }

        let (visible_row_range, scroll_to_current_pending, defer_sync) = {
            match self.directory_tree.list.try_lock() {
                Some(list) => (
                    list.image_list_visible_row_range,
                    list.scroll_image_list_to_current,
                    false,
                ),
                None => (None, false, true),
            }
        };
        if defer_sync {
            self.defer_directory_tree_file_list_sync();
        }
        let bootstrap_visible = self.directory_tree_strip_bootstrap_after_scan;
        let max_cold_per_frame = if bootstrap_visible {
            MAX_COLD_STRIP_GENERATES_PER_FRAME_BOOTSTRAP
        } else {
            MAX_COLD_STRIP_GENERATES_PER_FRAME
        };
        let max_inflight = if bootstrap_visible {
            MAX_STRIP_GENERATE_INFLIGHT_BOOTSTRAP
        } else {
            MAX_STRIP_GENERATE_INFLIGHT
        };
        let inflight_room =
            max_inflight.saturating_sub(self.directory_tree_strip_generate_inflight.len());
        let schedule_budget = max_cold_per_frame.min(inflight_room);
        let cold_candidates = self.collect_cold_strip_thumbnail_candidates(
            visible_row_range,
            scroll_to_current_pending,
            bootstrap_visible,
            schedule_budget,
        );
        if bootstrap_visible {
            if visible_row_range.is_some() {
                self.directory_tree_strip_bootstrap_after_scan = false;
                self.directory_tree_strip_bootstrap_frames = 0;
            } else {
                self.directory_tree_strip_bootstrap_frames =
                    self.directory_tree_strip_bootstrap_frames.saturating_add(1);
                if self.directory_tree_strip_bootstrap_frames
                    >= MAX_DIRECTORY_TREE_STRIP_BOOTSTRAP_FRAMES
                {
                    self.directory_tree_strip_bootstrap_after_scan = false;
                    self.directory_tree_strip_bootstrap_frames = 0;
                }
            }
        }
        let mut cold_scheduled = 0usize;
        if !self.scanning {
            for index in cold_candidates {
                if cold_scheduled >= schedule_budget {
                    break;
                }
                self.try_generate_cold_directory_tree_strip_thumbnail(index);
                cold_scheduled += 1;
            }
        }

        #[cfg(feature = "preload-debug")]
        if bootstrap_visible
            || cold_scheduled > 0
            || !self.directory_tree_strip_generate_inflight.is_empty()
        {
            let ui_preview_count = self.directory_tree.preview_snapshot.load().textures.len();
            crate::preload_debug!(
                "[PreloadDebug][DirTree] ensure_strip current={} rows={} cache={} ui_preview={} rev={} inflight={} cold_sched={} visible={:?} scroll_pending={} bootstrap={}",
                self.current_index,
                self.image_files.len(),
                self.directory_tree_strip_cache.textures().len(),
                ui_preview_count,
                self.directory_tree_strip_cache.gpu_revision(),
                self.directory_tree_strip_generate_inflight.len(),
                cold_scheduled,
                visible_row_range,
                scroll_to_current_pending,
                bootstrap_visible
            );
        }

        self.directory_tree_strip_cache
            .retain(|index| index < self.image_files.len());
        self.directory_tree_strip_tiled_attempted
            .retain(|index| *index < self.image_files.len());
        self.directory_tree_strip_generate_inflight
            .retain(|index| *index < self.image_files.len());
        self.directory_tree_strip_cold_attempted
            .retain(|index| *index < self.image_files.len());
    }

    fn permute_strip_index_set(set: &mut std::collections::HashSet<usize>, old_to_new: &[usize]) {
        let previous: Vec<usize> = set.iter().copied().collect();
        set.clear();
        for index in previous {
            if index < old_to_new.len() {
                let new_idx = old_to_new[index];
                if new_idx != usize::MAX {
                    set.insert(new_idx);
                }
            }
        }
    }

    fn permute_directory_tree_strip_pending_gpu(&mut self, old_to_new: &[usize]) {
        self.directory_tree_strip_pending_gpu.retain_mut(|pending| {
            if pending.index >= old_to_new.len() {
                return false;
            }
            let new_idx = old_to_new[pending.index];
            if new_idx == usize::MAX {
                return false;
            }
            pending.index = new_idx;
            true
        });
    }

    pub(crate) fn permute_directory_tree_strip_after_image_list_reorder(
        &mut self,
        old_to_new: &[usize],
    ) {
        self.directory_tree_strip_cache.permute(old_to_new);
        Self::permute_strip_index_set(&mut self.directory_tree_strip_generate_inflight, old_to_new);
        Self::permute_strip_index_set(&mut self.directory_tree_strip_tiled_attempted, old_to_new);
        Self::permute_strip_index_set(&mut self.directory_tree_strip_cold_attempted, old_to_new);
        self.permute_directory_tree_strip_pending_gpu(old_to_new);
        {
            let mut list = self.directory_tree.list.lock();
            list.image_list_generation = list.image_list_generation.wrapping_add(1);
            list.mark_snapshot_dirty();
        }
        domains::clear_preview_snapshot(&self.directory_tree.preview_snapshot);
        view::assemble_directory_tree_view(
            &self.directory_tree.view,
            &self.directory_tree.tree_snapshot,
            &self.directory_tree.list_snapshot,
            &self.directory_tree.preview_snapshot,
        );
    }

    // Path-based list diff for F5 refresh strip cache realignment.
    pub(crate) fn reorder_directory_tree_strip_after_image_list_change(
        &mut self,
        old_files: &[std::path::PathBuf],
        new_files: &[std::path::PathBuf],
    ) {
        if old_files.is_empty() || old_files.len() != new_files.len() {
            self.invalidate_directory_tree_strip_after_image_list_reorder();
            return;
        }
        let mut old_to_new = vec![usize::MAX; old_files.len()];
        for (new_idx, path) in new_files.iter().enumerate() {
            let Some(old_idx) = old_files.iter().position(|existing| existing == path) else {
                self.apply_partial_directory_tree_strip_reorder(old_files, new_files);
                return;
            };
            if old_to_new[old_idx] != usize::MAX {
                self.invalidate_directory_tree_strip_after_image_list_reorder();
                return;
            }
            old_to_new[old_idx] = new_idx;
        }
        if old_to_new.iter().any(|&idx| idx == usize::MAX) {
            self.apply_partial_directory_tree_strip_reorder(old_files, new_files);
            return;
        }
        self.permute_directory_tree_strip_after_image_list_reorder(&old_to_new);
    }

    fn apply_partial_directory_tree_strip_reorder(
        &mut self,
        old_files: &[std::path::PathBuf],
        new_files: &[std::path::PathBuf],
    ) {
        use std::collections::HashSet;

        let new_path_set: HashSet<_> = new_files.iter().collect();
        for (old_idx, path) in old_files.iter().enumerate() {
            if !new_path_set.contains(path) {
                self.directory_tree_strip_cache.remove_index(old_idx);
            }
        }

        let mut old_to_new = vec![usize::MAX; old_files.len()];
        for (old_idx, old_path) in old_files.iter().enumerate() {
            if let Some(new_idx) = new_files.iter().position(|path| path == old_path) {
                old_to_new[old_idx] = new_idx;
            }
        }

        let mut target_used = vec![false; new_files.len()];
        let mut full_permutation = true;
        // Entries with usize::MAX are unmapped paths; full_permutation stays false for those.
        for &new_idx in &old_to_new {
            if new_idx == usize::MAX {
                full_permutation = false;
                continue;
            }
            if new_idx >= new_files.len() || target_used[new_idx] {
                self.invalidate_directory_tree_strip_after_image_list_reorder();
                return;
            }
            target_used[new_idx] = true;
        }

        if full_permutation {
            self.permute_directory_tree_strip_after_image_list_reorder(&old_to_new);
            return;
        }

        log::debug!("[DirectoryTree] Partial strip cache reorder retaining mapped entries");
        self.directory_tree_strip_cache.partial_remap(&old_to_new);
        Self::permute_strip_index_set(
            &mut self.directory_tree_strip_generate_inflight,
            &old_to_new,
        );
        Self::permute_strip_index_set(&mut self.directory_tree_strip_tiled_attempted, &old_to_new);
        Self::permute_strip_index_set(&mut self.directory_tree_strip_cold_attempted, &old_to_new);
        self.permute_directory_tree_strip_pending_gpu(&old_to_new);
        {
            let mut list = self.directory_tree.list.lock();
            list.image_list_generation = list.image_list_generation.wrapping_add(1);
            list.mark_snapshot_dirty();
        }
        domains::clear_preview_snapshot(&self.directory_tree.preview_snapshot);
        view::assemble_directory_tree_view(
            &self.directory_tree.view,
            &self.directory_tree.tree_snapshot,
            &self.directory_tree.list_snapshot,
            &self.directory_tree.preview_snapshot,
        );
    }

    pub(crate) fn invalidate_directory_tree_strip_after_image_list_reorder(&mut self) {
        self.directory_tree_strip_cache.clear_all();
        self.directory_tree_strip_generate_inflight.clear();
        self.directory_tree_strip_tiled_attempted.clear();
        self.directory_tree_strip_cold_attempted.clear();
        self.directory_tree_strip_pending_gpu.clear();
        {
            let mut list = self.directory_tree.list.lock();
            list.image_list_generation = list.image_list_generation.wrapping_add(1);
            list.mark_snapshot_dirty();
        }
        domains::clear_preview_snapshot(&self.directory_tree.preview_snapshot);
        view::assemble_directory_tree_view(
            &self.directory_tree.view,
            &self.directory_tree.tree_snapshot,
            &self.directory_tree.list_snapshot,
            &self.directory_tree.preview_snapshot,
        );
    }

    /// Drop stale navigation list rows and strip previews before a new directory scan.
    pub(crate) fn reset_directory_tree_file_list_for_scan(&mut self) {
        if self.settings.browse_mode != crate::settings::BrowseMode::Tree {
            return;
        }
        self.invalidate_directory_tree_strip_after_image_list_reorder();
        let mut list = self.directory_tree.list.lock();
        list.image_rows.clear();
        list.current_index = 0;
        list.scanning = true;
        list.image_list_scroll_offset_y = 0.0;
        list.scroll_image_list_to_current = true;
        list.mark_snapshot_dirty();
    }

    pub(crate) fn invalidate_directory_tree_strip_gpu_textures(&mut self) {
        self.directory_tree_strip_cache.clear_gpu_textures();
        self.directory_tree_strip_tiled_attempted.clear();
        self.directory_tree_strip_cold_attempted.clear();
    }

    pub(crate) fn directory_tree_list_previews_active(&self) -> bool {
        self.directory_tree_settings_active() && self.settings.directory_tree_show_list_previews
    }

    pub(crate) fn on_directory_tree_list_preview_settings_changed(&mut self, ctx: &egui::Context) {
        self.invalidate_directory_tree_strip_gpu_textures();
        if let Some(mut list) = self.directory_tree.list.try_lock() {
            DirectoryTreeListPreviewLayout::from_settings(&self.settings).apply_to_list(&mut list);
        }
        domains::clear_preview_snapshot(&self.directory_tree.preview_snapshot);
        view::assemble_directory_tree_view(
            &self.directory_tree.view,
            &self.directory_tree.tree_snapshot,
            &self.directory_tree.list_snapshot,
            &self.directory_tree.preview_snapshot,
        );
        ctx.request_repaint();
        self.queue_save();
    }
}

#[cfg(test)]
mod tests {
    use super::send_strip_inflight_release;

    #[test]
    fn strip_inflight_release_sends_index_on_bounded_channel() {
        let (tx, rx) = crossbeam_channel::bounded(4);
        send_strip_inflight_release(&tx, 42);
        assert_eq!(rx.try_recv().ok(), Some(42));
    }

    #[test]
    fn strip_inflight_release_try_send_when_full_does_not_panic() {
        let (tx, _rx) = crossbeam_channel::bounded(0);
        send_strip_inflight_release(&tx, 1);
    }
}
