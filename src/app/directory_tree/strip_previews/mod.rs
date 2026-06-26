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

use std::collections::HashMap;
use std::sync::Arc;

use eframe::egui;

use crate::app::ImageViewerApp;
use crate::app::MAX_CONCURRENT_DECODER_LOADS;
use crate::app::directory_tree_strip_cache::{
    DirectoryTreeStripPendingGpuUpload, DirectoryTreeStripPreviewJobResult,
    MAX_STRIP_GPU_UPLOADS_PER_PAINT, MAX_STRIP_PENDING_GPU_UPLOADS, StripPreviewBufferTag,
    StripPreviewReplaceParams, decoded_rgba_size_valid, decide_strip_preview_replace,
};
use crate::loader::DIRECTORY_TREE_STRIP_POOL;
use crate::loader::{
    DecodedImage, PreviewStage, TiledImageSource, downsample_decoded_for_strip,
    generate_directory_tree_thumb_from_path, hdr_has_iso_deferred_gain_map,
    preview_aspect_matches_logical,
};

#[cfg(target_os = "windows")]
use super::workers::ensure_strip_worker_com_initialized;
use super::{
    BOOTSTRAP_STRIP_VISIBLE_ROW_CAP, DIRECTORY_TREE_COLD_NEIGHBOR_RADIUS,
    DirectoryTreeListPreviewLayout, MAX_COLD_STRIP_GENERATES_PER_FRAME,
    MAX_COLD_STRIP_GENERATES_PER_FRAME_BOOTSTRAP, MAX_COLD_STRIP_SCHEDULE_PER_FRAME,
    MAX_DIRECTORY_TREE_STRIP_BOOTSTRAP_FRAMES, MAX_STRIP_GENERATE_INFLIGHT,
    MAX_STRIP_GENERATE_INFLIGHT_BOOTSTRAP, MAX_TILED_STRIP_GENERATES_PER_FRAME, domains, view,
};

mod checks;
mod gpu;
mod poll;
mod schedule;

pub(super) fn send_strip_inflight_release(release_tx: &crossbeam_channel::Sender<usize>, index: usize) {
    if let Err(err) = release_tx.try_send(index) {
        log::warn!("[DirectoryTree] Strip inflight release dropped for index {index}: {err}");
    }
}


impl ImageViewerApp {

    pub(crate) fn invalidate_directory_tree_strip_preview_for_index(&mut self, index: usize) {
        self.directory_tree_strip_cache.remove_index(index);
        self.directory_tree_strip_cold_attempted.remove(&index);
        self.directory_tree_strip_generate_inflight.remove(&index);
        self.directory_tree_strip_tiled_attempted.remove(&index);
    }


    pub(crate) fn ensure_directory_tree_strip_thumbnails(&mut self, ctx: &egui::Context) {
        if !self.directory_tree_list_previews_active() {
            return;
        }

        self.poll_directory_tree_strip_preview_results(ctx);

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
        let can_preload = bootstrap_visible
            && self.settings.preload
            && !self.preload_deferred_for_hdr_capacity
            && self.loader.active_load_count() < MAX_CONCURRENT_DECODER_LOADS;
        if can_preload {
            self.schedule_preloads(true);
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
            let preload_sync_cap = file_count.min(BOOTSTRAP_STRIP_VISIBLE_ROW_CAP);
            let hdr_sync_budget = max_inflight
                .saturating_sub(self.directory_tree_strip_generate_inflight.len());
            let mut iso_sync_scheduled = 0usize;
            let iso_sync_budget = max_inflight
                .saturating_sub(self.directory_tree_strip_generate_inflight.len());
            let mut hdr_sync_scheduled = 0usize;
            let iso_baselines = self.collect_iso_baseline_pixels_up_to(preload_sync_cap);
            for index in 0..preload_sync_cap {
                if iso_sync_scheduled < iso_sync_budget {
                    if let Some((width, height, baseline)) = iso_baselines.get(&index) {
                        if self.try_schedule_strip_from_preloaded_iso_baseline_with_pixels(
                            index,
                            *width,
                            *height,
                            Arc::clone(baseline),
                        ) {
                            iso_sync_scheduled += 1;
                        }
                    }
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
                StripPreviewBufferTag::PreloadSdrFallback,
            );
        }

        let max_cold_per_frame = if bootstrap_visible {
            MAX_COLD_STRIP_GENERATES_PER_FRAME_BOOTSTRAP
        } else {
            MAX_COLD_STRIP_GENERATES_PER_FRAME
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
        let allow_cold_strip = !self.scanning || bootstrap_visible;
        if allow_cold_strip {
            for index in cold_candidates {
                if cold_scheduled >= schedule_budget {
                    break;
                }
                self.try_generate_cold_directory_tree_strip_thumbnail(index);
                cold_scheduled += 1;
            }
        }

        let compose_room = inflight_room.saturating_sub(cold_scheduled);
        if compose_room > 0 {
            let mut compose_scheduled = 0usize;
            if bootstrap_visible {
                let compose_cap = file_count.min(BOOTSTRAP_STRIP_VISIBLE_ROW_CAP);
                for index in 0..compose_cap {
                    if compose_scheduled >= compose_room {
                        break;
                    }
                    if self.strip_needs_compose_upgrade(index) {
                        self.try_schedule_strip_compose_upgrade(index);
                        compose_scheduled += 1;
                    }
                }
            } else if self.strip_needs_compose_upgrade(self.current_index) {
                self.try_schedule_strip_compose_upgrade(self.current_index);
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
