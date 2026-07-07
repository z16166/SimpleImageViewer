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

use super::{
    BOOTSTRAP_STRIP_VISIBLE_ROW_CAP, DIRECTORY_TREE_COLD_NEIGHBOR_RADIUS,
    DirectoryTreeListPreviewLayout, MAX_COLD_STRIP_GENERATES_PER_FRAME,
    MAX_COLD_STRIP_GENERATES_PER_FRAME_BOOTSTRAP, MAX_DEFERRED_SDR_STRIP_UPLOADS_PER_FRAME,
    MAX_DIRECTORY_TREE_STRIP_BOOTSTRAP_FRAMES, MAX_STRIP_GENERATE_INFLIGHT,
    MAX_STRIP_GENERATE_INFLIGHT_BOOTSTRAP, MAX_TILED_STRIP_GENERATES_PER_FRAME, domains, view,
};
use crate::app::index_cache_permute::permute_usize_set;

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
    let index = key.index;
    let release =
        crate::app::directory_tree_strip_cache::DirectoryTreeStripInflightRelease { key, kind };
    if let Err(err) = release_tx.try_send(release) {
        log::warn!("[DirectoryTree] Strip inflight release dropped for index {index}: {err}");
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
    pub(super) fn begin_directory_tree_strip_job(
        &mut self,
        index: usize,
    ) -> Option<crate::app::directory_tree_strip_cache::DirectoryTreeStripJobKey> {
        debug_assert!(
            !self.directory_tree_strip_generate_inflight.contains(&index),
            "begin_directory_tree_strip_job called while strip job is already in-flight"
        );
        debug_assert!(
            !self
                .directory_tree_strip_inflight_tokens
                .contains_key(&index),
            "begin_directory_tree_strip_job would overwrite active strip job token"
        );
        if self.directory_tree_strip_generate_inflight.contains(&index)
            || self
                .directory_tree_strip_inflight_tokens
                .contains_key(&index)
        {
            return None;
        }
        let path = self.image_files.get(index)?.clone();
        let image_list_generation = self.directory_tree.list.lock().image_list_generation;
        self.directory_tree_strip_next_job_token = self
            .directory_tree_strip_next_job_token
            .wrapping_add(1)
            .max(1);
        let job_token = NonZeroU64::new(self.directory_tree_strip_next_job_token)?;
        self.directory_tree_strip_generate_inflight.insert(index);
        self.directory_tree_strip_inflight_tokens
            .insert(index, job_token);
        Some(
            crate::app::directory_tree_strip_cache::DirectoryTreeStripJobKey {
                index,
                path,
                image_list_generation,
                job_token: DirectoryTreeStripJobToken::Worker(job_token),
            },
        )
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

    pub(super) fn directory_tree_strip_index_path_matches_current_list(
        &self,
        index: usize,
        path: &std::path::Path,
    ) -> bool {
        self.image_files
            .get(index)
            .is_some_and(|current| current == path)
    }

    pub(super) fn directory_tree_strip_key_matches_current_list(
        &self,
        key: &crate::app::directory_tree_strip_cache::DirectoryTreeStripJobKey,
    ) -> bool {
        self.directory_tree_strip_index_path_matches_current_list(key.index, &key.path)
            && self.directory_tree.list.lock().image_list_generation == key.image_list_generation
    }

    pub(super) fn directory_tree_strip_key_matches_active_job(
        &self,
        key: &crate::app::directory_tree_strip_cache::DirectoryTreeStripJobKey,
    ) -> bool {
        key.job_token.worker_token().is_some_and(|token| {
            self.directory_tree_strip_inflight_tokens.get(&key.index) == Some(&token)
        })
    }

    pub(super) fn directory_tree_strip_active_index_for_job_token(
        &self,
        key: &crate::app::directory_tree_strip_cache::DirectoryTreeStripJobKey,
    ) -> Option<usize> {
        let token = key.job_token.worker_token()?;
        if self.directory_tree_strip_key_matches_active_job(key) {
            return Some(key.index);
        }
        self.directory_tree_strip_inflight_tokens
            .iter()
            .find_map(|(&index, &active_token)| (active_token == token).then_some(index))
    }

    pub(crate) fn invalidate_directory_tree_strip_preview_for_index(&mut self, index: usize) {
        self.directory_tree_strip_cache.remove_index(index);
        self.directory_tree_strip_cold_attempted.remove(&index);
        self.directory_tree_strip_cold_awaiting_main_loader
            .remove(&index);
        self.directory_tree_strip_generate_inflight.remove(&index);
        self.directory_tree_strip_inflight_tokens.remove(&index);
        self.directory_tree_strip_static_full_decode_inflight
            .remove(&index);
        self.directory_tree_strip_tiled_attempted.remove(&index);
    }

    pub(super) fn mark_strip_cold_awaiting_main_loader(&mut self, index: usize) {
        self.directory_tree_strip_cold_awaiting_main_loader
            .insert(index);
        self.directory_tree_strip_cold_attempted.insert(index);
    }

    pub(super) fn release_strip_cold_awaiting_main_loader_if_resolved(&mut self, index: usize) {
        if !self
            .directory_tree_strip_cold_awaiting_main_loader
            .contains(&index)
        {
            return;
        }
        let resolved = self.directory_tree_strip_cache.contains(index)
            || self.hdr_image_cache.contains_key(&index)
            || (!self.loader.is_loading(index)
                && !self.strip_cold_skip_slow_embedded_sdr_primary(index));
        if resolved {
            self.directory_tree_strip_cold_awaiting_main_loader
                .remove(&index);
            self.directory_tree_strip_cold_attempted.remove(&index);
        }
    }

    fn release_resolved_strip_cold_awaiting_main_loader(&mut self) {
        self.strip_cold_awaiting_scratch.clear();
        self.strip_cold_awaiting_scratch.extend(
            self.directory_tree_strip_cold_awaiting_main_loader
                .iter()
                .copied(),
        );
        for i in 0..self.strip_cold_awaiting_scratch.len() {
            let index = self.strip_cold_awaiting_scratch[i];
            self.release_strip_cold_awaiting_main_loader_if_resolved(index);
        }
    }

    pub(crate) fn ensure_directory_tree_strip_thumbnails(&mut self, ctx: &egui::Context) {
        if !self.directory_tree_list_previews_active() {
            return;
        }

        self.poll_avif_strip_probe_results();
        self.poll_directory_tree_strip_preview_results(ctx);
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
        let max_inflight = if bootstrap_visible {
            MAX_STRIP_GENERATE_INFLIGHT_BOOTSTRAP
        } else {
            MAX_STRIP_GENERATE_INFLIGHT
        };

        // Do not drop `cold_attempted` here when cache is empty: failed decodes (e.g. motion-video
        // JPG) stay out of cache but must remain attempted so they do not monopolize cold slots.
        self.directory_tree_strip_tiled_attempted.retain(|index| {
            self.directory_tree_strip_cache.contains(*index)
                || self.directory_tree_strip_generate_inflight.contains(index)
        });

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
            if self
                .directory_tree_strip_cache
                .invalidate_if_invalid(index, logical)
            {
                #[cfg(feature = "preload-debug")]
                crate::preload_debug!(
                    "[PreloadDebug][Strip] invalidate idx={} logical={}x{} (aspect mismatch vs cached texture)",
                    index,
                    logical.0,
                    logical.1
                );
                self.directory_tree_strip_tiled_attempted.remove(&index);
            }
            self.try_sync_strip_from_tile_manager_preview(index);
            self.try_sync_strip_from_texture_cache(index);
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
            self.try_sync_strip_from_texture_cache(current);
            for delta in 1..=DIRECTORY_TREE_COLD_NEIGHBOR_RADIUS {
                if current >= delta {
                    self.try_sync_strip_from_texture_cache(current - delta);
                }
                if current + delta < file_count {
                    self.try_sync_strip_from_texture_cache(current + delta);
                }
            }
            // Preloaded neighbors can sit in texture_cache while strip LRU evicts them.
            // Cold strip scheduling skips those indices; resync when they scroll into view.
            if !(scroll_to_current_pending && !bootstrap_visible)
                && let Some((start, end)) = visible_row_range
            {
                for index in start..end.min(file_count) {
                    self.directory_tree_strip_cache.touch_cached_index(index);
                    self.try_sync_strip_from_texture_cache(index);
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

        // Stale-index cleanup: remove entries whose index now exceeds the file list
        // (e.g. after deletion or re-scan shrink). The `image_list_generation` counter
        // is bumped on every structural list mutation — only run the O(n) retain when
        // the generation has actually changed. When the directory is idle this skips
        // 4 × HashSet::retain that can each grow to directory scale (10k+ entries).
        let current_gen = self.directory_tree.list.lock().image_list_generation;
        if current_gen != self.strip_stale_retain_last_generation {
            self.strip_stale_retain_last_generation = current_gen;
            self.directory_tree_strip_cache
                .retain(|index| index < self.image_files.len());
            self.directory_tree_strip_tiled_attempted
                .retain(|index| *index < self.image_files.len());
            self.directory_tree_strip_generate_inflight
                .retain(|index| *index < self.image_files.len());
            self.directory_tree_strip_static_full_decode_inflight
                .retain(|index| *index < self.image_files.len());
            self.directory_tree_strip_cold_attempted
                .retain(|index| *index < self.image_files.len());
        }
    }

    fn permute_directory_tree_strip_pending_gpu(&mut self, old_to_new: &[usize]) {
        let permute_deque = |deque: &mut std::collections::VecDeque<
            crate::app::directory_tree_strip_cache::DirectoryTreeStripPendingGpuUpload,
        >| {
            deque.retain_mut(|pending| {
                if pending.key.index >= old_to_new.len() {
                    return false;
                }
                let new_idx = old_to_new[pending.key.index];
                if new_idx == usize::MAX {
                    return false;
                }
                pending.key.index = new_idx;
                true
            });
        };
        permute_deque(&mut self.directory_tree_strip_pending_gpu_initial);
        permute_deque(&mut self.directory_tree_strip_pending_gpu_refined);
    }

    /// Remap strip preview state after a single file is removed from `image_files`
    /// (delete/cut). Call after `remove` so `image_files.len() + 1` is the old length.
    pub(crate) fn permute_directory_tree_strip_after_single_removal(
        &mut self,
        removed_index: usize,
    ) {
        if !self.directory_tree_list_previews_active() {
            return;
        }
        let old_len = self.image_files.len() + 1;
        if removed_index >= old_len {
            return;
        }
        let mut old_to_new = vec![usize::MAX; old_len];
        for (old_idx, mapped_idx) in old_to_new.iter_mut().enumerate() {
            if old_idx < removed_index {
                *mapped_idx = old_idx;
            } else if old_idx > removed_index {
                *mapped_idx = old_idx - 1;
            }
        }
        self.directory_tree_strip_cache.permute(&old_to_new);
        permute_usize_set(
            &mut self.directory_tree_strip_generate_inflight,
            &old_to_new,
        );
        crate::app::index_cache_permute::permute_usize_hashmap(
            &mut self.directory_tree_strip_inflight_tokens,
            &old_to_new,
        );
        permute_usize_set(
            &mut self.directory_tree_strip_static_full_decode_inflight,
            &old_to_new,
        );
        permute_usize_set(&mut self.directory_tree_strip_tiled_attempted, &old_to_new);
        permute_usize_set(&mut self.directory_tree_strip_cold_attempted, &old_to_new);
        permute_usize_set(
            &mut self.directory_tree_strip_cold_awaiting_main_loader,
            &old_to_new,
        );
        self.permute_directory_tree_strip_pending_gpu(&old_to_new);
        self.cached_image_strip_path_index = None;
        {
            let mut list = self.directory_tree.list.lock();
            list.image_list_generation = list.image_list_generation.wrapping_add(1);
            list.mark_snapshot_dirty();
        }
    }

    pub(crate) fn permute_directory_tree_strip_after_image_list_reorder(
        &mut self,
        old_to_new: &[usize],
    ) {
        self.directory_tree_strip_cache.permute(old_to_new);
        permute_usize_set(&mut self.directory_tree_strip_generate_inflight, old_to_new);
        crate::app::index_cache_permute::permute_usize_hashmap(
            &mut self.directory_tree_strip_inflight_tokens,
            old_to_new,
        );
        permute_usize_set(
            &mut self.directory_tree_strip_static_full_decode_inflight,
            old_to_new,
        );
        permute_usize_set(&mut self.directory_tree_strip_tiled_attempted, old_to_new);
        permute_usize_set(&mut self.directory_tree_strip_cold_attempted, old_to_new);
        permute_usize_set(
            &mut self.directory_tree_strip_cold_awaiting_main_loader,
            old_to_new,
        );
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

    /// After a cache-preserving column sort: remap pending GPU uploads, refresh the preview
    /// snapshot, and enter bootstrap mode so visible list rows reschedule strip thumbnails.
    pub(crate) fn prepare_directory_tree_strip_scheduling_after_list_reorder(
        &mut self,
        old_to_new: &[usize],
    ) {
        if !self.directory_tree_list_previews_active() {
            return;
        }
        self.permute_directory_tree_strip_pending_gpu(old_to_new);
        domains::clear_preview_snapshot(&self.directory_tree.preview_snapshot);
        self.directory_tree_strip_bootstrap_after_scan = true;
        self.directory_tree_strip_bootstrap_frames = 0;
        self.strip_preload_cooldown_frames = 0;
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
        if old_to_new.contains(&usize::MAX) {
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
        permute_usize_set(
            &mut self.directory_tree_strip_generate_inflight,
            &old_to_new,
        );
        crate::app::index_cache_permute::permute_usize_hashmap(
            &mut self.directory_tree_strip_inflight_tokens,
            &old_to_new,
        );
        permute_usize_set(
            &mut self.directory_tree_strip_static_full_decode_inflight,
            &old_to_new,
        );
        permute_usize_set(&mut self.directory_tree_strip_tiled_attempted, &old_to_new);
        permute_usize_set(&mut self.directory_tree_strip_cold_attempted, &old_to_new);
        permute_usize_set(
            &mut self.directory_tree_strip_cold_awaiting_main_loader,
            &old_to_new,
        );
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
        self.directory_tree_strip_inflight_tokens.clear();
        self.directory_tree_strip_static_full_decode_inflight
            .clear();
        self.directory_tree_strip_tiled_attempted.clear();
        self.directory_tree_strip_cold_attempted.clear();
        self.directory_tree_strip_cold_awaiting_main_loader.clear();
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
        list.mark_snapshot_dirty();
    }

    pub(crate) fn invalidate_directory_tree_strip_gpu_textures(&mut self) {
        self.directory_tree_strip_cache.clear_gpu_textures();
        self.directory_tree_strip_tiled_attempted.clear();
        self.directory_tree_strip_cold_attempted.clear();
        self.directory_tree_strip_cold_awaiting_main_loader.clear();
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
