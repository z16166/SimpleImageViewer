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

use std::num::NonZeroU64;

use eframe::egui;

use crate::app::ImageViewerApp;
use crate::app::MAX_CONCURRENT_DECODER_LOADS;
use crate::app::directory_tree_strip_cache::{DirectoryTreeStripJobToken, StripPreviewBufferTag};
use crate::loader::{DecodedImage, PreviewStage};

/// Strip pixels produced by main-loader install, waiting for a free strip worker slot.
#[derive(Clone)]
pub(crate) struct DirectoryTreeStripPendingMainHandoff {
    pub decoded: DecodedImage,
    pub stage: PreviewStage,
    pub logical_size: Option<(u32, u32)>,
    pub buffer_tag: StripPreviewBufferTag,
}

use super::{
    BOOTSTRAP_STRIP_VISIBLE_ROW_CAP, DIRECTORY_TREE_COLD_NEIGHBOR_RADIUS,
    DirectoryTreeListPreviewLayout, MAX_COLD_STRIP_GENERATES_PER_FRAME,
    MAX_COLD_STRIP_GENERATES_PER_FRAME_BOOTSTRAP, MAX_DEFERRED_SDR_STRIP_UPLOADS_PER_FRAME,
    MAX_DIRECTORY_TREE_STRIP_BOOTSTRAP_FRAMES, MAX_STRIP_GENERATE_INFLIGHT,
    MAX_STRIP_GENERATE_INFLIGHT_BOOTSTRAP, MAX_TILED_STRIP_GENERATES_PER_FRAME, domains, view,
};

mod checks;
mod gpu;
mod poll;
mod schedule;

fn strip_full_decode_reuse_allowed(
    index: usize,
    current_index: usize,
    image_count: usize,
    max_preload_distance: usize,
    preload_enabled: bool,
) -> bool {
    if image_count == 0 || index >= image_count || current_index >= image_count {
        return false;
    }
    if index == current_index {
        return true;
    }
    if !preload_enabled {
        return false;
    }
    let forward = (index + image_count - current_index) % image_count;
    let backward = (current_index + image_count - index) % image_count;
    forward.min(backward) <= max_preload_distance
}

pub(super) fn send_strip_inflight_release(
    release_tx: &crossbeam_channel::Sender<
        crate::app::directory_tree_strip_cache::DirectoryTreeStripInflightRelease,
    >,
    key: crate::app::directory_tree_strip_cache::DirectoryTreeStripJobKey,
    kind: crate::app::directory_tree_strip_cache::DirectoryTreeStripInflightReleaseKind,
    root_wake: Option<&crate::app::RootRedrawWake>,
) -> bool {
    // `key.index` is a submit-time hint and may be stale after reorder; include path for diagnosis.
    let index = key.index;
    let path = key.path.display().to_string();
    let release =
        crate::app::directory_tree_strip_cache::DirectoryTreeStripInflightRelease { key, kind };
    if let Err(err) = release_tx.try_send(release) {
        log::warn!(
            "[DirectoryTree] Strip inflight release dropped for index {index} path {path}: {err}"
        );
        return false;
    }
    if let Some(wake) = root_wake {
        wake();
    }
    true
}

impl ImageViewerApp {
    /// Begin a strip worker job for `index`.
    ///
    /// Must be called without holding `directory_tree.list`; this function briefly locks it
    /// to snapshot `image_list_generation` for stale-result rejection.
    ///
    /// Returns the job key and a clone of the strip-local cancel flag for the worker.
    pub(super) fn begin_directory_tree_strip_job(
        &mut self,
        index: usize,
    ) -> Option<(
        crate::app::directory_tree_strip_cache::DirectoryTreeStripJobKey,
        crate::loader::DecodeCancelFlag,
    )> {
        let path = self.image_files.get(index)?.clone();
        debug_assert!(
            !self.directory_tree_strip_generate_inflight.contains(&path),
            "begin_directory_tree_strip_job called while strip job is already in-flight"
        );
        debug_assert!(
            !self
                .directory_tree_strip_inflight_tokens
                .contains_key(&path),
            "begin_directory_tree_strip_job would overwrite active strip job token"
        );
        if self.directory_tree_strip_generate_inflight.contains(&path)
            || self
                .directory_tree_strip_inflight_tokens
                .contains_key(&path)
        {
            return None;
        }
        let image_list_generation = self.directory_tree.list.lock().image_list_generation;
        self.directory_tree_strip_next_job_token = self
            .directory_tree_strip_next_job_token
            .wrapping_add(1)
            .max(1);
        let job_token = NonZeroU64::new(self.directory_tree_strip_next_job_token)?;
        let cancel = crate::loader::DecodeCancelFlag::new();
        self.directory_tree_strip_generate_inflight
            .insert(path.clone());
        self.directory_tree_strip_inflight_tokens
            .insert(path.clone(), job_token);
        self.directory_tree_strip_inflight_cancel
            .insert(path.clone(), cancel.clone());
        Some((
            crate::app::directory_tree_strip_cache::DirectoryTreeStripJobKey {
                index,
                path,
                image_list_generation,
                job_token: DirectoryTreeStripJobToken::Worker(job_token),
            },
            cancel,
        ))
    }

    /// Build a strip upload key for pixels produced synchronously from the current list.
    ///
    /// `SynchronousUpload` marks an upload that has no worker in-flight state, so
    /// token-matched release and cleanup paths must ignore it.
    pub(super) fn directory_tree_strip_upload_key_for_current_index(
        &self,
        index: usize,
    ) -> Option<crate::app::directory_tree_strip_cache::DirectoryTreeStripJobKey> {
        Some(
            crate::app::directory_tree_strip_cache::DirectoryTreeStripJobKey {
                index,
                path: self.image_files.get(index)?.clone(),
                image_list_generation: self.directory_tree.list.lock().image_list_generation,
                job_token: DirectoryTreeStripJobToken::SynchronousUpload,
            },
        )
    }

    /// True when `path` is still present in the current image list (index is ignored).
    pub(super) fn directory_tree_strip_path_in_current_list(&self, path: &std::path::Path) -> bool {
        self.image_files.iter().any(|current| current == path)
    }

    pub(super) fn directory_tree_strip_key_matches_current_list(
        &self,
        key: &crate::app::directory_tree_strip_cache::DirectoryTreeStripJobKey,
    ) -> bool {
        // Path is authoritative; `key.index` is only a submit-time hint and may be stale after
        // reorder without a generation bump (e.g. column sort).
        self.directory_tree_strip_path_in_current_list(&key.path)
            && self.directory_tree.list.lock().image_list_generation == key.image_list_generation
    }

    pub(super) fn directory_tree_strip_key_matches_active_job(
        &self,
        key: &crate::app::directory_tree_strip_cache::DirectoryTreeStripJobKey,
    ) -> bool {
        key.job_token.worker_token().is_some_and(|token| {
            self.directory_tree_strip_inflight_tokens.get(&key.path) == Some(&token)
        })
    }

    pub(crate) fn invalidate_directory_tree_strip_preview_for_index(&mut self, index: usize) {
        let Some(path) = self.image_files.get(index).cloned() else {
            return;
        };
        self.directory_tree_strip_cache.remove_path(&path);
        self.directory_tree_strip_cold_attempted.remove(&path);
        self.directory_tree_strip_cold_awaiting_main_loader
            .remove(&path);
        self.directory_tree_strip_pending_main_handoff.remove(&path);
        self.directory_tree_strip_generate_inflight.remove(&path);
        self.directory_tree_strip_inflight_tokens.remove(&path);
        self.directory_tree_strip_inflight_cancel.remove(&path);
        self.directory_tree_strip_static_full_decode_inflight
            .remove(&path);
        self.directory_tree_strip_tiled_attempted.remove(&path);
    }

    /// Resolve the row's current path and invalidate an aspect-mismatched cached texture.
    fn strip_cache_invalidate_if_invalid_index(
        &mut self,
        index: usize,
        logical: (u32, u32),
    ) -> bool {
        let Some(path) = self.image_files.get(index).cloned() else {
            return false;
        };
        self.directory_tree_strip_cache
            .invalidate_if_invalid(&path, logical)
    }

    /// Mark the row's current path recently used so LRU eviction skips visible rows.
    fn strip_cache_touch_cached_index(&mut self, index: usize) {
        if let Some(path) = self.image_files.get(index).cloned() {
            self.directory_tree_strip_cache.touch_cached_path(&path);
        }
    }

    /// Drop the row's `tiled_attempted` flag by resolving its current path.
    fn strip_tiled_attempted_remove_index(&mut self, index: usize) {
        if let Some(path) = self.image_files.get(index).cloned() {
            self.directory_tree_strip_tiled_attempted.remove(&path);
        }
    }

    pub(super) fn mark_strip_cold_awaiting_main_loader(&mut self, index: usize) {
        let Some(path) = self.image_files.get(index).cloned() else {
            return;
        };
        self.directory_tree_strip_cold_awaiting_main_loader
            .insert(path.clone());
        self.directory_tree_strip_cold_attempted.insert(path);
    }

    pub(crate) fn release_strip_cold_awaiting_main_loader_if_resolved(&mut self, index: usize) {
        let Some(path) = self.image_files.get(index).cloned() else {
            return;
        };
        if !self
            .directory_tree_strip_cold_awaiting_main_loader
            .contains(&path)
        {
            return;
        }
        // Strip GPU/resample handoff from main install counts as resolved even before
        // the cache slot is filled (oversized SDR anim textures take this path).
        let strip_handoff_inflight = self.directory_tree_strip_cache.contains(&path)
            || self.directory_tree_strip_generate_inflight.contains(&path)
            || self
                .directory_tree_strip_pending_main_handoff
                .contains_key(&path)
            || self
                .directory_tree_strip_pending_gpu_initial
                .iter()
                .any(|u| u.key.path == path)
            || self
                .directory_tree_strip_pending_gpu_refined
                .iter()
                .any(|u| u.key.path == path);
        let main_sdr_ready_without_strip_handoff = !strip_handoff_inflight
            && !self.strip_main_loader_sdr_unreliable_for_strip(index)
            && (self.deferred_sdr_uploads.contains_key(&index)
                || self.texture_cache.contains(index));
        let resolved = strip_handoff_inflight
            || self.hdr_image_cache.contains_key(&index)
            // Main already installed SDR but strip was LRU-evicted / never handed off:
            // stop awaiting a re-install that will not happen so cold can self-decode.
            || main_sdr_ready_without_strip_handoff;
        if resolved {
            self.directory_tree_strip_cold_awaiting_main_loader
                .remove(&path);
            // Keep cold_attempted while handoff is still in flight so ensure_strip does not
            // respawn a duplicate cold job; clear once the strip cache actually has pixels.
            // Also clear when releasing a dead await so LRU-evicted rows can cold-retry
            // even without retained logical_sizes.
            if self.directory_tree_strip_cache.contains(&path)
                || self.hdr_image_cache.contains_key(&index)
                || main_sdr_ready_without_strip_handoff
            {
                self.directory_tree_strip_cold_attempted.remove(&path);
            }
        }
    }

    fn release_resolved_strip_cold_awaiting_main_loader(&mut self) {
        self.strip_cold_awaiting_scratch.clear();
        self.strip_cold_awaiting_scratch.extend(
            self.directory_tree_strip_cold_awaiting_main_loader
                .iter()
                .cloned(),
        );
        for i in 0..self.strip_cold_awaiting_scratch.len() {
            let path = self.strip_cold_awaiting_scratch[i].clone();
            let Some(index) = self.strip_path_current_index(&path) else {
                // Path dropped from the list -- clear the stale await entry.
                self.directory_tree_strip_cold_awaiting_main_loader
                    .remove(&path);
                continue;
            };
            self.release_strip_cold_awaiting_main_loader_if_resolved(index);
        }
    }

    /// Retry main loads for strip rows still awaiting install after a capacity miss.
    ///
    /// Neighbor prefetch fills at most [`crate::loader::MAX_IMG_LOADER_THREADS`] slots and
    /// never schedules the circular-window hole (comment on
    /// [`Self::request_main_load_for_strip_deferred_index`]); without a per-frame retry the
    /// hole index stays on a strip placeholder until the user navigates to it.
    fn retry_strip_cold_awaiting_main_loads(&mut self) {
        if self
            .directory_tree_strip_cold_awaiting_main_loader
            .is_empty()
        {
            return;
        }
        self.strip_cold_awaiting_scratch.clear();
        self.strip_cold_awaiting_scratch.extend(
            self.directory_tree_strip_cold_awaiting_main_loader
                .iter()
                .cloned(),
        );
        for i in 0..self.strip_cold_awaiting_scratch.len() {
            let path = self.strip_cold_awaiting_scratch[i].clone();
            if let Some(index) = self.strip_path_current_index(&path) {
                self.request_main_load_for_strip_deferred_index(index);
            }
        }
    }

    /// Cancel strip worker jobs whose index falls outside the visible/neighbor priority window.
    ///
    /// Uses strip-local [`DecodeCancelFlag`]s -- independent of ImageLoader main/prefetch cancel.
    /// Only signals cancel; workers clear inflight bookkeeping via release when they exit.
    pub(super) fn cancel_strip_jobs_outside_priority_window(
        &mut self,
        current_index: usize,
        image_count: usize,
        visible_row_range: Option<(usize, usize)>,
    ) {
        if image_count == 0 || self.directory_tree_strip_inflight_cancel.is_empty() {
            return;
        }
        let mut retain = std::collections::HashSet::new();
        if let Some((start, end)) = visible_row_range {
            for idx in start..end.min(image_count) {
                retain.insert(idx);
            }
        }
        if current_index < image_count {
            retain.insert(current_index);
            for delta in 1..=DIRECTORY_TREE_COLD_NEIGHBOR_RADIUS {
                if let Some(idx) = current_index.checked_sub(delta) {
                    retain.insert(idx);
                }
                let idx = current_index + delta;
                if idx < image_count {
                    retain.insert(idx);
                }
            }
        }
        let _ = self.image_strip_path_index();
        let path_to_index = &self
            .cached_image_strip_path_index
            .as_ref()
            .expect("warmed above")
            .1;
        for (path, flag) in &self.directory_tree_strip_inflight_cancel {
            let keep = path_to_index
                .get(path)
                .is_some_and(|idx| retain.contains(idx));
            if !keep {
                flag.cancel();
            }
        }
    }

    pub(crate) fn ensure_directory_tree_strip_thumbnails(&mut self, ctx: &egui::Context) {
        if !self.directory_tree_list_previews_active() {
            return;
        }

        self.poll_avif_strip_probe_results();
        self.poll_directory_tree_strip_preview_results(ctx);
        if self.preload_deferred_for_hdr_capacity {
            return;
        }

        self.flush_strip_pending_main_handoffs();
        self.release_resolved_strip_cold_awaiting_main_loader();

        let (visible_row_range, scroll_to_current_pending) =
            if let Some(list) = self.directory_tree.list.try_lock() {
                (
                    list.image_list_visible_row_range,
                    list.scroll_image_list_to_current,
                )
            } else {
                self.defer_directory_tree_file_list_sync();
                (None, false)
            };
        self.cancel_strip_jobs_outside_priority_window(
            self.current_index,
            self.image_files.len(),
            visible_row_range,
        );
        let bootstrap_visible = self.directory_tree_strip_bootstrap_after_scan;
        // Cooldown: once all preload slots fill, schedule_preloads(true) is idempotent;
        // skip for a few frames to avoid redundant per-frame scheduling overhead.
        let preload_cooled_down = self.strip_preload_cooldown_frames == 0;
        if preload_cooled_down {
            let can_preload_neighbors = self.settings.preload
                && !self.preload_deferred_for_hdr_capacity
                && self.has_loaded_asset(self.current_index)
                && self.loader.active_load_count() < MAX_CONCURRENT_DECODER_LOADS
                && !crate::app::image_management::should_defer_neighbor_work_for_current_main(
                    self.has_loaded_asset(self.current_index),
                    self.loader.is_loading(self.current_index),
                );
            if can_preload_neighbors {
                self.schedule_preloads(true);
                self.strip_preload_cooldown_frames = 3;
            }
        } else {
            self.strip_preload_cooldown_frames =
                self.strip_preload_cooldown_frames.saturating_sub(1);
        }
        // After neighbor preload may have freed slots (and without canceling strip-deferred
        // hole loads), retry any cold-awaiting main loads that missed capacity earlier.
        self.retry_strip_cold_awaiting_main_loads();
        let max_inflight = if bootstrap_visible {
            MAX_STRIP_GENERATE_INFLIGHT_BOOTSTRAP
        } else {
            MAX_STRIP_GENERATE_INFLIGHT
        };

        // Keep `tiled_attempted` even when the strip cache is still empty. Async PSD v1 sources
        // return empty previews until composite pixels land; pruning here used to clear the flag
        // every frame after PermanentFailure and respawn thousands of strip jobs.
        // Entries are path-keyed and dropped on list invalidate / explicit invalidate_for_index
        // and by the generation-gated stale retain below.

        self.strip_indices_scratch.clear();
        self.strip_indices_scratch
            .extend(self.prefetched_tiles.keys().copied());
        if let Some(tm) = &self.tile_manager
            && !self.strip_indices_scratch.contains(&tm.image_index)
        {
            self.strip_indices_scratch.push(tm.image_index);
        }
        let current = self.current_index;
        let file_count = self.image_files.len();
        let total = file_count.max(1);
        self.strip_indices_scratch.sort_by_key(|&idx| {
            if idx == current {
                0
            } else {
                let forward = (idx + total - current) % total;
                let backward = (current + total - idx) % total;
                1 + forward.min(backward)
            }
        });

        let tiled_count = self.strip_indices_scratch.len();
        for i in 0..tiled_count {
            let index = self.strip_indices_scratch[i];
            let Some(logical) = self.directory_tree_strip_logical_size(index) else {
                continue;
            };
            if self.strip_cache_invalidate_if_invalid_index(index, logical) {
                #[cfg(feature = "preload-debug")]
                crate::preload_debug!(
                    "[PreloadDebug][Strip] invalidate idx={} logical={}x{} (aspect mismatch vs cached texture)",
                    index,
                    logical.0,
                    logical.1
                );
                self.strip_tiled_attempted_remove_index(index);
            }
            self.try_sync_strip_from_tile_manager_preview(index);
            self.try_sync_strip_from_texture_cache(index, ctx);
        }

        if file_count > 0 {
            let preload_sync_cap = file_count.min(BOOTSTRAP_STRIP_VISIBLE_ROW_CAP);
            let hdr_sync_budget =
                max_inflight.saturating_sub(self.directory_tree_strip_generate_inflight.len());
            let mut iso_sync_scheduled = 0usize;
            let iso_sync_budget =
                max_inflight.saturating_sub(self.directory_tree_strip_generate_inflight.len());
            let mut hdr_sync_scheduled = 0usize;
            for index in 0..preload_sync_cap {
                if iso_sync_scheduled < iso_sync_budget
                    && let Some((width, height, baseline)) =
                        self.iso_deferred_baseline_pixels_for_strip(index)
                    && self.try_schedule_strip_from_preloaded_iso_baseline_with_pixels(
                        index, width, height, baseline,
                    )
                {
                    iso_sync_scheduled += 1;
                }
                if hdr_sync_scheduled < hdr_sync_budget
                    && self.try_schedule_strip_from_hdr_image_cache(index)
                {
                    hdr_sync_scheduled += 1;
                }
            }
            let current = self.current_index.min(file_count - 1);
            self.try_sync_strip_from_texture_cache(current, ctx);
            for delta in 1..=DIRECTORY_TREE_COLD_NEIGHBOR_RADIUS {
                if current >= delta {
                    self.try_sync_strip_from_texture_cache(current - delta, ctx);
                }
                if current + delta < file_count {
                    self.try_sync_strip_from_texture_cache(current + delta, ctx);
                }
            }
            // Preloaded neighbors can sit in texture_cache while strip LRU evicts them.
            // Cold strip scheduling skips those indices; resync when they scroll into view.
            if !(scroll_to_current_pending && !bootstrap_visible)
                && let Some((start, end)) = visible_row_range
            {
                for index in start..end.min(file_count) {
                    self.strip_cache_touch_cached_index(index);
                    self.try_sync_strip_from_texture_cache(index, ctx);
                }
            }
        }

        let mut generated_this_frame = 0usize;
        let tiled_count = self.strip_indices_scratch.len();
        for i in 0..tiled_count {
            let index = self.strip_indices_scratch[i];
            let Some(logical) = self.directory_tree_strip_logical_size(index) else {
                continue;
            };
            if self.strip_cache_is_valid_for_logical_index(index, logical) {
                continue;
            }
            if generated_this_frame >= MAX_TILED_STRIP_GENERATES_PER_FRAME {
                break;
            }
            self.try_generate_directory_tree_strip_from_tiled_source(index);
            generated_this_frame += 1;
        }

        // Collect keys in ring-distance order so the nearest deferred entries go first,
        // then bound to MAX_DEFERRED_SDR_STRIP_UPLOADS_PER_FRAME to avoid O(cache_size)
        // per-frame iteration when many HDR images have deferred SDR fallbacks.
        let deferred_upload_budget = MAX_DEFERRED_SDR_STRIP_UPLOADS_PER_FRAME;
        if deferred_upload_budget > 0 {
            let file_count = self.image_files.len();
            let current = self.current_index.min(file_count.saturating_sub(1));
            self.strip_indices_scratch.clear();
            self.strip_indices_scratch
                .extend(self.deferred_sdr_uploads.keys().copied());
            self.strip_indices_scratch.sort_by_key(|&idx| {
                if file_count == 0 || idx == current {
                    return 0;
                }
                let forward = (idx + file_count - current) % file_count;
                let backward = (current + file_count - idx) % file_count;
                forward.min(backward)
            });
            let mut deferred_processed = 0usize;
            let deferred_count = self.strip_indices_scratch.len();
            for i in 0..deferred_count {
                let index = self.strip_indices_scratch[i];
                if deferred_processed >= deferred_upload_budget {
                    break;
                }
                if self.tiled_sdr_source_for_index(index).is_some() {
                    continue;
                }
                if self.strip_main_loader_sdr_unreliable_for_strip(index) {
                    continue;
                }
                if self.strip_cache_contains_index(index) {
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
                    crate::app::directory_tree_strip_cache::DirectoryTreeStripGpuUploadRequest {
                        index,
                        decoded,
                        stage: PreviewStage::Initial,
                        logical: self.directory_tree_strip_logical_size(index),
                        buffer_tag: StripPreviewBufferTag::PreloadSdrFallback,
                        strip_max_side_used: None,
                        job_key: None,
                    },
                );
                deferred_processed += 1;
            }
        }

        let max_cold_per_frame = if bootstrap_visible {
            MAX_COLD_STRIP_GENERATES_PER_FRAME_BOOTSTRAP
        } else {
            MAX_COLD_STRIP_GENERATES_PER_FRAME
        };
        let inflight_room =
            max_inflight.saturating_sub(self.directory_tree_strip_generate_inflight.len());
        let schedule_budget = max_cold_per_frame.min(inflight_room);
        let cold_count = self.collect_cold_strip_thumbnail_candidates(
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
        let allow_cold_strip = !self.scanning || bootstrap_visible;
        if allow_cold_strip {
            for i in 0..cold_count {
                if cold_scheduled >= schedule_budget {
                    break;
                }
                let index = self.strip_cold_candidates_scratch[i];
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
            crate::preload_debug_throttled!(
                &format!(
                    "strip:ensure:{}:{}:{}:{}",
                    self.current_index,
                    cold_scheduled,
                    scroll_to_current_pending,
                    bootstrap_visible
                ),
                crate::preload_debug::PRELOAD_DEBUG_THROTTLE_INTERVAL,
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

        // Stale-path cleanup: drop authoritative state for paths no longer in the file list
        // (e.g. after deletion or re-scan). The `image_list_generation` counter is bumped on
        // every structural list mutation -- only run the O(n) retain when the generation has
        // actually changed. When the directory is idle this skips several path-keyed retains
        // that can each grow to directory scale (10k+ entries).
        //
        let current_gen = self.directory_tree.list.lock().image_list_generation;
        if current_gen != self.strip_stale_retain_last_generation {
            self.strip_stale_retain_last_generation = current_gen;
            let live: std::collections::HashSet<std::path::PathBuf> =
                self.image_files.iter().cloned().collect();
            for (path, flag) in &self.directory_tree_strip_inflight_cancel {
                if !live.contains(path) {
                    flag.cancel();
                }
            }
            self.directory_tree_strip_cache
                .retain(|path| live.contains(path));
            self.directory_tree_strip_tiled_attempted
                .retain(|path| live.contains(path));
            self.directory_tree_strip_generate_inflight
                .retain(|path| live.contains(path));
            self.directory_tree_strip_inflight_tokens
                .retain(|path, _| live.contains(path));
            self.directory_tree_strip_inflight_cancel
                .retain(|path, _| live.contains(path));
            self.directory_tree_strip_static_full_decode_inflight
                .retain(|path| live.contains(path));
            self.directory_tree_strip_cold_attempted
                .retain(|path| live.contains(path));
            self.directory_tree_strip_cold_awaiting_main_loader
                .retain(|path| live.contains(path));
            self.directory_tree_strip_pending_main_handoff
                .retain(|path, _| live.contains(path));
            self.directory_tree_strip_pending_gpu_initial
                .retain(|upload| live.contains(&upload.key.path));
            self.directory_tree_strip_pending_gpu_refined
                .retain(|upload| live.contains(&upload.key.path));
        }
    }

    /// Drop authoritative strip state for paths no longer present in `image_files`, cancel their
    /// in-flight jobs, bump the image-list generation (so stale-generation workers are ignored),
    /// and refresh the preview snapshot. Path-keyed state needs no index remap after reorders or
    /// removals, so retained rows keep their thumbnails at their new positions via projection.
    pub(crate) fn reconcile_directory_tree_strip_state_for_current_list(&mut self) {
        let live: std::collections::HashSet<std::path::PathBuf> =
            self.image_files.iter().cloned().collect();
        for (path, flag) in &self.directory_tree_strip_inflight_cancel {
            if !live.contains(path) {
                flag.cancel();
            }
        }
        self.directory_tree_strip_cache
            .retain(|path| live.contains(path));
        self.directory_tree_strip_generate_inflight
            .retain(|path| live.contains(path));
        self.directory_tree_strip_inflight_tokens
            .retain(|path, _| live.contains(path));
        self.directory_tree_strip_inflight_cancel
            .retain(|path, _| live.contains(path));
        self.directory_tree_strip_static_full_decode_inflight
            .retain(|path| live.contains(path));
        self.directory_tree_strip_tiled_attempted
            .retain(|path| live.contains(path));
        self.directory_tree_strip_cold_attempted
            .retain(|path| live.contains(path));
        self.directory_tree_strip_cold_awaiting_main_loader
            .retain(|path| live.contains(path));
        self.directory_tree_strip_pending_main_handoff
            .retain(|path, _| live.contains(path));
        self.directory_tree_strip_pending_gpu_initial
            .retain(|upload| live.contains(&upload.key.path));
        self.directory_tree_strip_pending_gpu_refined
            .retain(|upload| live.contains(&upload.key.path));
        self.cached_image_strip_path_index = None;
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

    /// Reconcile strip preview state after a single file is removed from `image_files`
    /// (delete/cut). Path-keyed state only needs stale-path pruning, not index remap.
    pub(crate) fn reconcile_directory_tree_strip_after_single_removal(&mut self) {
        if !self.directory_tree_list_previews_active() {
            return;
        }
        self.reconcile_directory_tree_strip_state_for_current_list();
    }

    /// After a cache-preserving column sort: refresh the preview snapshot and enter bootstrap
    /// mode so visible list rows reschedule strip thumbnails. Path-keyed entries stay valid and
    /// are republished at their new indices via snapshot projection. Does not bump
    /// `image_list_generation` (unlike `reconcile_*`); pending GPU uploads keep their generation
    /// but must rematch `key.index` from path before flush.
    pub(crate) fn prepare_directory_tree_strip_scheduling_after_list_reorder(&mut self) {
        if !self.directory_tree_list_previews_active() {
            return;
        }
        self.cached_image_strip_path_index = None;
        self.rematch_pending_strip_gpu_upload_indices();
        domains::clear_preview_snapshot(&self.directory_tree.preview_snapshot);
        self.directory_tree_strip_bootstrap_after_scan = true;
        self.directory_tree_strip_bootstrap_frames = 0;
        self.strip_preload_cooldown_frames = 0;
    }

    /// Rewrite submit-time indices on pending GPU uploads after a path-preserving reorder.
    fn rematch_pending_strip_gpu_upload_indices(&mut self) {
        let _ = self.image_strip_path_index();
        let Some((_, path_to_index)) = self.cached_image_strip_path_index.as_ref() else {
            return;
        };
        for item in self
            .directory_tree_strip_pending_gpu_initial
            .iter_mut()
            .chain(self.directory_tree_strip_pending_gpu_refined.iter_mut())
        {
            if let Some(&current_index) = path_to_index.get(&item.key.path) {
                item.key.index = current_index;
            }
        }
    }

    // Path-based list realignment for F5 refresh and column sort. Path-keyed state survives
    // reorders directly; only paths that left the list are pruned.

    pub(crate) fn reorder_directory_tree_strip_after_image_list_change(
        &mut self,
        _old_files: &[std::path::PathBuf],
        _new_files: &[std::path::PathBuf],
    ) {
        self.reconcile_directory_tree_strip_state_for_current_list();
    }

    pub(crate) fn invalidate_directory_tree_strip_after_image_list_reorder(&mut self) {
        for flag in self.directory_tree_strip_inflight_cancel.values() {
            flag.cancel();
        }
        self.directory_tree_strip_cache.clear_all();
        self.directory_tree_strip_generate_inflight.clear();
        self.directory_tree_strip_inflight_tokens.clear();
        self.directory_tree_strip_inflight_cancel.clear();
        self.directory_tree_strip_static_full_decode_inflight
            .clear();
        self.directory_tree_strip_reusable_full_decode_cache.clear();
        self.directory_tree_strip_tiled_attempted.clear();
        self.directory_tree_strip_cold_attempted.clear();
        self.directory_tree_strip_cold_awaiting_main_loader.clear();
        self.directory_tree_strip_pending_main_handoff.clear();
        self.directory_tree_strip_pending_gpu_initial.clear();
        self.directory_tree_strip_pending_gpu_refined.clear();
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
        if !self.directory_tree_settings_active() {
            return;
        }
        self.invalidate_directory_tree_strip_after_image_list_reorder();
        let mut list = self.directory_tree.list.lock();
        list.image_rows.clear();
        list.current_index = 0;
        list.scanning = true;
        list.image_list_scroll_offset_y = 0.0;
        list.scroll_image_list_to_current = true;
        // Drop the stale visible-row hint so cold bootstrap uses the forced top-of-list window
        // until the UI reports a fresh visible range for the new scan.
        list.image_list_visible_row_range = None;
        list.mark_snapshot_dirty();
    }

    pub(crate) fn invalidate_directory_tree_strip_gpu_textures(&mut self) {
        self.directory_tree_strip_cache.clear_gpu_textures();
        self.directory_tree_strip_tiled_attempted.clear();
        self.directory_tree_strip_cold_attempted.clear();
        self.directory_tree_strip_reusable_full_decode_cache.clear();
        self.directory_tree_strip_cold_awaiting_main_loader.clear();
        domains::clear_preview_snapshot(&self.directory_tree.preview_snapshot);
        view::assemble_directory_tree_view(
            &self.directory_tree.view,
            &self.directory_tree.tree_snapshot,
            &self.directory_tree.list_snapshot,
            &self.directory_tree.preview_snapshot,
        );
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
    use std::num::NonZeroU64;
    use std::path::PathBuf;

    use super::send_strip_inflight_release;
    use crate::app::directory_tree_strip_cache::{
        DirectoryTreeStripInflightReleaseKind, DirectoryTreeStripJobKey, DirectoryTreeStripJobToken,
    };

    fn strip_job_key(index: usize, job_token: u64) -> DirectoryTreeStripJobKey {
        let Some(job_token) = NonZeroU64::new(job_token) else {
            panic!("test token must be non-zero");
        };
        DirectoryTreeStripJobKey {
            index,
            path: PathBuf::from(format!("image-{index}.png")),
            image_list_generation: 7,
            job_token: DirectoryTreeStripJobToken::Worker(job_token),
        }
    }

    #[test]
    fn strip_inflight_release_sends_key_on_bounded_channel() {
        let (tx, rx) = crossbeam_channel::bounded(4);
        assert!(send_strip_inflight_release(
            &tx,
            strip_job_key(42, 9),
            DirectoryTreeStripInflightReleaseKind::ClearAttempt,
            None,
        ));
        let release = rx.try_recv().expect("release should be queued");
        assert_eq!(release.key, strip_job_key(42, 9));
        assert!(matches!(
            release.kind,
            DirectoryTreeStripInflightReleaseKind::ClearAttempt
        ));
    }

    #[test]
    fn strip_inflight_release_try_send_when_full_does_not_panic() {
        let (tx, _rx) = crossbeam_channel::bounded(0);
        assert!(!send_strip_inflight_release(
            &tx,
            strip_job_key(1, 3),
            DirectoryTreeStripInflightReleaseKind::PermanentFailure,
            None,
        ));
    }
}
