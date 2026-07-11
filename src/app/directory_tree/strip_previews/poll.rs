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

//! Asynchronous strip preview result polling and failure recovery.

use std::collections::HashMap;
use std::path::PathBuf;

use eframe::egui;

use crate::app::ImageViewerApp;
use crate::app::directory_tree_strip_cache::{
    DirectoryTreeStripGpuUploadRequest, DirectoryTreeStripInflightRelease,
    DirectoryTreeStripInflightReleaseKind, DirectoryTreeStripJobKey, DirectoryTreeStripJobToken,
    DirectoryTreeStripPreviewFailure, DirectoryTreeStripPreviewJobResult,
    DirectoryTreeStripPreviewSuccess, decoded_rgba_size_valid,
};
use crate::loader::preview_aspect_matches_logical;

impl ImageViewerApp {
    pub(super) fn clear_strip_preview_attempt_state(&mut self, index: usize) {
        self.directory_tree_strip_generate_inflight.remove(&index);
        self.directory_tree_strip_inflight_tokens.remove(&index);
        self.directory_tree_strip_static_full_decode_inflight
            .remove(&index);
        self.directory_tree_strip_tiled_attempted.remove(&index);
        self.directory_tree_strip_cold_attempted.remove(&index);
        self.directory_tree_strip_cold_awaiting_main_loader
            .remove(&index);
    }

    pub(super) fn clear_strip_preview_attempt_state_for_key(
        &mut self,
        key: &DirectoryTreeStripJobKey,
    ) -> bool {
        let Some(active_index) = self.directory_tree_strip_active_index_for_job_token(key) else {
            return false;
        };
        self.clear_strip_preview_attempt_state(active_index);
        true
    }

    /// Drop inflight bookkeeping without clearing a completed cold attempt (avoids retry loops).
    ///
    /// Also leaves `directory_tree_strip_tiled_attempted` set: empty async tiled previews
    /// (PSD v1 before pixels land) must not clear that flag, or ensure_strip respawns every frame.
    fn finish_strip_preview_job_for_key(&mut self, key: &DirectoryTreeStripJobKey) -> bool {
        let Some(active_index) = self.directory_tree_strip_active_index_for_job_token(key) else {
            return false;
        };
        self.directory_tree_strip_generate_inflight
            .remove(&active_index);
        self.directory_tree_strip_inflight_tokens
            .remove(&active_index);
        self.directory_tree_strip_static_full_decode_inflight
            .remove(&active_index);
        self.flush_strip_pending_main_handoff_for_index(active_index);
        true
    }

    fn strip_preview_success_is_permanent_failure(
        result: &DirectoryTreeStripPreviewSuccess,
    ) -> bool {
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

    fn abandon_strip_preview_attempt_after_failure_for_key(
        &mut self,
        key: &DirectoryTreeStripJobKey,
    ) {
        self.finish_strip_preview_job_for_key(key);
        // Keep `cold_attempted` so undecodable files (e.g. motion-video JPG) do not monopolize
        // the limited cold-generate budget and block thumbnails for neighboring rows.
        // Keep `tiled_attempted` as well: async PSD/PSB sources return empty previews until
        // composite pixels arrive; clearing would cause a per-frame tiled strip storm.
    }

    fn strip_preview_result_matches_index(&self, key: &DirectoryTreeStripJobKey) -> bool {
        self.image_files.get(key.index) == Some(&key.path)
    }

    fn image_strip_path_index(&mut self) -> &HashMap<PathBuf, usize> {
        let generation = self.directory_tree.list.lock().image_list_generation;
        let stale = self
            .cached_image_strip_path_index
            .as_ref()
            .is_none_or(|(g, _)| *g != generation);
        if stale {
            let map: HashMap<PathBuf, usize> = self
                .image_files
                .iter()
                .enumerate()
                .map(|(i, p)| (p.clone(), i))
                .collect();
            self.cached_image_strip_path_index = Some((generation, map));
        }
        &self
            .cached_image_strip_path_index
            .as_ref()
            .expect("just inserted")
            .1
    }

    fn try_apply_relocated_strip_preview_result(
        &mut self,
        mut result: DirectoryTreeStripPreviewSuccess,
        ctx: &egui::Context,
    ) -> bool {
        self.finish_strip_preview_job_for_key(&result.key);
        let Some(&new_index) = self.image_strip_path_index().get(&result.key.path) else {
            return false;
        };

        if Self::strip_preview_success_is_permanent_failure(&result) {
            return false;
        }

        self.cache_reusable_strip_full_decode(
            new_index,
            result.logical,
            result.reusable_full_decoded.take(),
        );
        result.key.index = new_index;
        self.queue_directory_tree_strip_gpu_upload(DirectoryTreeStripGpuUploadRequest {
            index: new_index,
            decoded: result.decoded,
            stage: result.stage,
            logical: Some(result.logical),
            buffer_tag: result.buffer_tag,
            strip_max_side_used: Some(result.strip_max_side_used),
            job_key: Some(result.key),
        });
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

    fn cache_reusable_strip_full_decode(
        &mut self,
        index: usize,
        logical: (u32, u32),
        reusable_full: Option<crate::loader::DecodedImage>,
    ) {
        let Some(full) = reusable_full else {
            return;
        };
        if full.is_sdr_deferred_placeholder() || (full.width, full.height) != logical {
            return;
        }
        if !super::strip_full_decode_reuse_allowed(
            index,
            self.current_index,
            self.image_files.len(),
            self.prefetch_window_max_distance,
            self.settings.preload,
        ) {
            return;
        }
        if self.texture_cache.contains(index)
            || self.hdr_image_cache.contains_key(&index)
            || self.hdr_tiled_source_cache.contains_key(&index)
            || self.animation_cache.contains_key(&index)
            || self.deferred_sdr_uploads.contains_key(&index)
        {
            return;
        }
        let pixel_bytes = u64::from(full.width) * u64::from(full.height) * 4;
        if pixel_bytes > crate::hdr::decode::MAX_HDR_FALLBACK_DECODE_BYTES {
            return;
        }
        self.insert_deferred_sdr_upload(index, full);
    }

    fn handle_strip_inflight_release(&mut self, release: DirectoryTreeStripInflightRelease) {
        match release.kind {
            DirectoryTreeStripInflightReleaseKind::ClearAttempt => {
                self.clear_strip_preview_attempt_state_for_key(&release.key);
            }
            DirectoryTreeStripInflightReleaseKind::PermanentFailure => {
                self.abandon_strip_preview_attempt_after_failure_for_key(&release.key);
            }
        }
    }

    fn strip_result_generation_is_stale(&self, key: &DirectoryTreeStripJobKey) -> (bool, u64) {
        match self.directory_tree.list.try_lock() {
            Some(list) => (
                key.image_list_generation != list.image_list_generation,
                list.image_list_generation,
            ),
            None => (true, 0),
        }
    }

    fn poll_deferred_strip_result(&mut self, failure: DirectoryTreeStripPreviewFailure) {
        let _failure_reason = failure.reason;
        let active_index = self
            .directory_tree_strip_active_index_for_job_token(&failure.key)
            .unwrap_or(failure.key.index);
        if self.finish_strip_preview_job_for_key(&failure.key) {
            self.mark_strip_cold_awaiting_main_loader(active_index);
            // PSD/PSB (and other slow primaries) defer strip to the main loader. Neighbor
            // preload may never cover every visible strip row (e.g. idx 3 with window
            // [1,2]/[5,4]), so kick a main load here or the strip waits forever.
            // Capacity misses are retried from ensure_directory_tree_strip_thumbnails;
            // cancel_outside retains cold-awaiting indices so hole loads are not aborted.
            self.request_main_load_for_strip_deferred_index(active_index);
            #[cfg(feature = "preload-debug")]
            crate::preload_debug!(
                "[PreloadDebug][StripPoll] idx={} cold deferred reason={} (await main loader fast path or install)",
                active_index,
                _failure_reason
            );
        }
    }

    pub(super) fn request_main_load_for_strip_deferred_index(&mut self, index: usize) {
        if self.has_loaded_asset(index) || self.loader.is_loading(index) {
            return;
        }
        let Some(path) = self.image_files.get(index).cloned() else {
            return;
        };
        #[cfg(feature = "preload-debug")]
        let path_for_log = path.clone();
        let spawned = self.loader.request_load(
            index,
            path,
            self.settings.raw_high_quality,
            self.raw_demosaic_mode_for_index(index),
            self.settings.psd_hidden_layer_heuristic,
        );
        #[cfg(feature = "preload-debug")]
        if spawned {
            crate::preload_debug!(
                "[PreloadDebug][Strip] request main load for deferred strip idx={} path={}",
                index,
                path_for_log.display()
            );
        } else {
            crate::preload_debug!(
                "[PreloadDebug][Strip] defer main load (loader capacity) idx={} path={}",
                index,
                path_for_log.display()
            );
        }
        #[cfg(not(feature = "preload-debug"))]
        let _ = spawned;
    }

    fn poll_successful_strip_result(
        &mut self,
        result: DirectoryTreeStripPreviewSuccess,
        ctx: &egui::Context,
    ) {
        if !self.strip_preview_result_matches_index(&result.key) {
            let index = result.key.index;
            if !self.try_apply_relocated_strip_preview_result(result, ctx) {
                log::debug!(
                    "[DirectoryTree] Strip preview relocation failed for index {}",
                    index
                );
            }
            return;
        }
        if Self::strip_preview_success_is_permanent_failure(&result) {
            if result.decoded.width == 0 || result.decoded.height == 0 {
                log::debug!(
                    "[DirectoryTree] Strip preview unavailable for index {} ({})",
                    result.key.index,
                    result.key.path.display()
                );
            } else if !decoded_rgba_size_valid(&result.decoded) {
                log::warn!(
                    "[DirectoryTree] Strip preview job size mismatch for index {}: {}x{}",
                    result.key.index,
                    result.decoded.width,
                    result.decoded.height
                );
            } else {
                log::warn!(
                    "[DirectoryTree] Strip preview job aspect mismatch for index {}: {}x{} vs {}x{}",
                    result.key.index,
                    result.decoded.width,
                    result.decoded.height,
                    result.logical.0,
                    result.logical.1
                );
            }
            self.abandon_strip_preview_attempt_after_failure_for_key(&result.key);
            return;
        }
        let index = result.key.index;
        #[cfg(feature = "preload-debug")]
        let path = result.key.path.clone();
        let logical = result.logical;
        let stage = result.stage;
        let buffer_tag = result.buffer_tag;
        let strip_max_side_used = result.strip_max_side_used;
        let job_key = result.key.clone();
        let reusable_full = result.reusable_full_decoded;
        #[cfg(feature = "preload-debug")]
        let decoded_w = result.decoded.width;
        #[cfg(feature = "preload-debug")]
        let decoded_h = result.decoded.height;
        self.queue_directory_tree_strip_gpu_upload(DirectoryTreeStripGpuUploadRequest {
            index,
            decoded: result.decoded,
            stage,
            logical: Some(logical),
            buffer_tag,
            strip_max_side_used: Some(strip_max_side_used),
            job_key: Some(job_key.clone()),
        });
        self.cache_reusable_strip_full_decode(index, logical, reusable_full);
        let cache_valid = self
            .directory_tree_strip_cache
            .is_valid_for_logical(index, logical);
        #[cfg(feature = "preload-debug")]
        {
            crate::preload_debug_throttled!(
                &format!("strip:poll:{index}:{stage:?}:{cache_valid}"),
                crate::preload_debug::PRELOAD_DEBUG_THROTTLE_INTERVAL,
                "[PreloadDebug][StripPoll] idx={} path={} stage={:?} decoded={}x{} logical={}x{} \
                 cache_valid_before_flush={cache_valid} cold_attempted={} pending_len={}",
                index,
                path.display(),
                stage,
                decoded_w,
                decoded_h,
                logical.0,
                logical.1,
                self.directory_tree_strip_cold_attempted.contains(&index),
                self.directory_tree_strip_pending_gpu_initial.len()
                    + self.directory_tree_strip_pending_gpu_refined.len()
            );
        }
        self.finish_strip_preview_job_for_key(&job_key);
        if cache_valid {
            ctx.request_repaint();
            ctx.request_repaint_of(self.directory_tree_repaint_viewport_id());
            #[cfg(feature = "preload-debug")]
            crate::preload_debug!(
                "[PreloadDebug][StripPoll] idx={} repaint requested (cache already valid)",
                index
            );
        } else {
            #[cfg(feature = "preload-debug")]
            crate::preload_debug_throttled!(
                &format!("strip:poll_no_repaint:{index}"),
                crate::preload_debug::PRELOAD_DEBUG_THROTTLE_INTERVAL,
                "[PreloadDebug][StripPoll] idx={} no repaint (cache not valid until GPU flush)",
                index
            );
        }
    }

    pub(crate) fn poll_directory_tree_strip_preview_results(&mut self, ctx: &egui::Context) {
        while let Ok(release) = self.directory_tree_strip_inflight_release_rx.try_recv() {
            self.handle_strip_inflight_release(release);
        }
        while let Ok(result) = self.directory_tree_strip_preview_rx.try_recv() {
            let key = match &result {
                DirectoryTreeStripPreviewJobResult::Success(result) => &result.key,
                DirectoryTreeStripPreviewJobResult::DeferredToMainLoader(failure) => &failure.key,
            };
            if let DirectoryTreeStripJobToken::Worker(_token) = key.job_token
                && self
                    .directory_tree_strip_active_index_for_job_token(key)
                    .is_none()
            {
                #[cfg(feature = "preload-debug")]
                crate::preload_debug!(
                    "[PreloadDebug][DirTree] strip result stale token idx={} token={}",
                    key.index,
                    _token
                );
                continue;
            }
            let (stale, _active_list_generation) = self.strip_result_generation_is_stale(key);
            if stale {
                #[cfg(feature = "preload-debug")]
                crate::preload_debug!(
                    "[PreloadDebug][DirTree] strip result stale gen idx={} job_gen={} active_gen={}",
                    key.index,
                    key.image_list_generation,
                    _active_list_generation
                );
                self.clear_strip_preview_attempt_state_for_key(key);
                continue;
            }
            match result {
                DirectoryTreeStripPreviewJobResult::Success(result) => {
                    self.poll_successful_strip_result(result, ctx);
                }
                DirectoryTreeStripPreviewJobResult::DeferredToMainLoader(failure) => {
                    self.poll_deferred_strip_result(failure);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::strip_full_decode_reuse_allowed;
    use crate::app::directory_tree_strip_cache::{
        DirectoryTreeStripJobKey, DirectoryTreeStripJobToken,
    };
    use std::num::NonZeroU64;
    use std::path::PathBuf;

    #[test]
    fn strip_full_decode_reuse_keeps_current_even_when_preload_disabled() {
        assert!(strip_full_decode_reuse_allowed(5, 5, 20, 2, false));
    }

    #[test]
    fn strip_full_decode_reuse_requires_preload_window_for_neighbors() {
        assert!(strip_full_decode_reuse_allowed(7, 5, 20, 2, true));
        assert!(strip_full_decode_reuse_allowed(3, 5, 20, 2, true));
        assert!(!strip_full_decode_reuse_allowed(8, 5, 20, 2, true));
        assert!(!strip_full_decode_reuse_allowed(7, 5, 20, 2, false));
    }

    #[test]
    fn strip_full_decode_reuse_uses_circular_preload_distance() {
        assert!(strip_full_decode_reuse_allowed(19, 0, 20, 1, true));
        assert!(strip_full_decode_reuse_allowed(0, 19, 20, 1, true));
        assert!(!strip_full_decode_reuse_allowed(17, 0, 20, 1, true));
    }

    #[test]
    fn permanent_failure_keeps_tiled_attempted_to_avoid_retry_storm() {
        let mut app = crate::app::image_management::tests::make_test_app();
        app.image_files = vec![PathBuf::from("async.psd")];
        app.directory_tree.list.lock().image_list_generation = 1;
        let token = NonZeroU64::new(9).expect("non-zero");
        app.directory_tree_strip_generate_inflight.insert(0);
        app.directory_tree_strip_inflight_tokens.insert(0, token);
        app.directory_tree_strip_tiled_attempted.insert(0);

        let key = DirectoryTreeStripJobKey {
            index: 0,
            path: PathBuf::from("async.psd"),
            image_list_generation: 1,
            job_token: DirectoryTreeStripJobToken::Worker(token),
        };
        app.abandon_strip_preview_attempt_after_failure_for_key(&key);

        assert!(
            app.directory_tree_strip_tiled_attempted.contains(&0),
            "empty async tiled strip must keep tiled_attempted"
        );
        assert!(!app.directory_tree_strip_generate_inflight.contains(&0));
        assert!(!app.directory_tree_strip_inflight_tokens.contains_key(&0));
    }
}
