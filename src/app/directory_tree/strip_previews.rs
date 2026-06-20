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
    DirectoryTreeStripPreviewJobResult, decoded_rgba_size_valid,
};
use crate::loader::DIRECTORY_TREE_STRIP_POOL;
use crate::loader::{
    DecodedImage, PreviewStage, TiledImageSource, generate_directory_tree_thumb_from_path,
    preview_aspect_matches_logical,
};

#[cfg(target_os = "windows")]
use super::workers::strip_worker_com_initialized;
use super::{
    DIRECTORY_TREE_COLD_NEIGHBOR_RADIUS, DirectoryTreeListPreviewLayout,
    MAX_COLD_STRIP_GENERATES_PER_FRAME, MAX_STRIP_GENERATE_INFLIGHT,
    MAX_TILED_STRIP_GENERATES_PER_FRAME,
};

fn send_strip_inflight_release(release_tx: &crossbeam_channel::Sender<usize>, index: usize) {
    if let Err(err) = release_tx.try_send(index) {
        log::warn!("[DirectoryTree] Strip inflight release dropped for index {index}: {err}");
    }
}

impl ImageViewerApp {
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
        self.directory_tree_strip_cache.upsert_from_decoded(
            index,
            decoded,
            stage,
            logical_size,
            ctx,
            self.current_index,
            self.image_files.len(),
            self.settings
                .directory_tree_list_preview_size
                .strip_max_side(),
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
        if self
            .deferred_sdr_uploads
            .get(&index)
            .is_some_and(|decoded| !crate::loader::decoded_looks_like_black_placeholder(decoded))
        {
            return false;
        }
        if self.directory_tree_strip_generate_inflight.contains(&index) {
            return false;
        }
        if self.directory_tree_strip_cold_attempted.contains(&index) {
            return false;
        }
        if let Some(logical) = self.directory_tree_strip_logical_size(index) {
            if self
                .directory_tree_strip_cache
                .is_valid_for_logical(index, logical)
            {
                return false;
            }
        } else if self.directory_tree_strip_cache.contains(index) {
            return false;
        }
        true
    }

    pub(super) fn visible_cold_strip_indices(
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
        visible_row_range
            .map(|(start, end)| (start..end.min(total)).collect())
            .unwrap_or_default()
    }

    fn collect_cold_strip_thumbnail_candidates(
        &self,
        visible_row_range: Option<(usize, usize)>,
        scroll_to_current_pending: bool,
        bootstrap_visible: bool,
    ) -> Vec<usize> {
        let total = self.image_files.len();
        if total == 0 {
            return Vec::new();
        }
        let current = self.current_index.min(total.saturating_sub(1));
        let mut ordered = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut push = |index: usize| {
            if index < total && seen.insert(index) && self.strip_index_needs_cold_thumbnail(index) {
                ordered.push(index);
            }
        };

        push(current);

        for index in Self::visible_cold_strip_indices(
            visible_row_range,
            scroll_to_current_pending,
            total,
            bootstrap_visible,
        ) {
            push(index);
        }

        for delta in 1..=DIRECTORY_TREE_COLD_NEIGHBOR_RADIUS {
            push(current.saturating_sub(delta));
            if current + delta < total {
                push(current + delta);
            }
        }

        ordered
    }

    pub(crate) fn try_generate_cold_directory_tree_strip_thumbnail(&mut self, index: usize) {
        if !self.strip_index_needs_cold_thumbnail(index) {
            return;
        }
        let path = self.image_files[index].clone();
        let list_generation = self.directory_tree.state.lock().image_list_generation;
        self.directory_tree_strip_cold_attempted.insert(index);
        self.directory_tree_strip_generate_inflight.insert(index);
        let tx = self.directory_tree_strip_preview_tx.clone();
        let release_tx = self.directory_tree_strip_inflight_release_tx.clone();
        let max_side = self
            .settings
            .directory_tree_list_preview_size
            .strip_max_side();
        DIRECTORY_TREE_STRIP_POOL.spawn(move || {
            crate::preload_debug!(
                "[PreloadDebug][Strip] cold worker start idx={} path={}",
                index,
                path.display()
            );
            #[cfg(target_os = "windows")]
            let com_ok = strip_worker_com_initialized();
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
                "[PreloadDebug][Strip] cold worker done idx={} out={}x{} logical={}x{} aspect_ok={}",
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
                )
            );
            let job = DirectoryTreeStripPreviewJobResult {
                index,
                path,
                image_list_generation: list_generation,
                decoded,
                logical,
                stage: PreviewStage::Initial,
            };
            if let Err(err) = tx.send(job) {
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
        self.clear_strip_preview_attempt_state(new_index);

        if result.decoded.width == 0 || result.decoded.height == 0 {
            return false;
        }
        if !decoded_rgba_size_valid(&result.decoded) {
            log::warn!(
                "[DirectoryTree] Relocated strip preview size mismatch for {}: {}x{}",
                result.path.display(),
                result.decoded.width,
                result.decoded.height
            );
            return false;
        }
        if !preview_aspect_matches_logical(
            result.decoded.width,
            result.decoded.height,
            result.logical.0,
            result.logical.1,
        ) {
            log::warn!(
                "[DirectoryTree] Relocated strip preview aspect mismatch for {}: {}x{} vs {}x{}",
                result.path.display(),
                result.decoded.width,
                result.decoded.height,
                result.logical.0,
                result.logical.1
            );
            return false;
        }

        self.cache_directory_tree_strip_thumbnail(
            new_index,
            &result.decoded,
            result.stage,
            Some(result.logical),
            ctx,
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
            let active_list_generation = self
                .directory_tree
                .state
                .try_lock()
                .map(|state| state.image_list_generation);
            let Some(active_list_generation) = active_list_generation else {
                self.clear_strip_preview_attempt_state(result.index);
                continue;
            };
            if result.image_list_generation != active_list_generation {
                #[cfg(feature = "preload-debug")]
                crate::preload_debug!(
                    "[PreloadDebug][DirTree] strip result stale gen idx={} job_gen={} active_gen={}",
                    result.index,
                    result.image_list_generation,
                    active_list_generation
                );
                self.clear_strip_preview_attempt_state(result.index);
                continue;
            }
            if !self.strip_preview_result_matches_index(&result) {
                let _ = self.try_apply_relocated_strip_preview_result(result, ctx);
                continue;
            }
            if result.decoded.width == 0 || result.decoded.height == 0 {
                self.directory_tree_strip_tiled_attempted
                    .remove(&result.index);
                self.directory_tree_strip_cold_attempted
                    .remove(&result.index);
                continue;
            }
            if !decoded_rgba_size_valid(&result.decoded) {
                log::warn!(
                    "[DirectoryTree] Strip preview job size mismatch for index {}: {}x{}",
                    result.index,
                    result.decoded.width,
                    result.decoded.height
                );
                self.directory_tree_strip_tiled_attempted
                    .remove(&result.index);
                self.directory_tree_strip_cold_attempted
                    .remove(&result.index);
                continue;
            }
            if !preview_aspect_matches_logical(
                result.decoded.width,
                result.decoded.height,
                result.logical.0,
                result.logical.1,
            ) {
                log::warn!(
                    "[DirectoryTree] Strip preview job aspect mismatch for index {}: {}x{} vs {}x{}",
                    result.index,
                    result.decoded.width,
                    result.decoded.height,
                    result.logical.0,
                    result.logical.1
                );
                self.directory_tree_strip_tiled_attempted
                    .remove(&result.index);
                self.directory_tree_strip_cold_attempted
                    .remove(&result.index);
                continue;
            }
            self.cache_directory_tree_strip_thumbnail(
                result.index,
                &result.decoded,
                result.stage,
                Some(result.logical),
                ctx,
            );
            if !self
                .directory_tree_strip_cache
                .is_valid_for_logical(result.index, result.logical)
            {
                self.directory_tree_strip_tiled_attempted
                    .remove(&result.index);
            } else {
                ctx.request_repaint();
                ctx.request_repaint_of(self.directory_tree_repaint_viewport_id());
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
        let list_generation = self.directory_tree.state.lock().image_list_generation;
        self.directory_tree_strip_tiled_attempted.insert(index);
        self.directory_tree_strip_generate_inflight.insert(index);
        let source = Arc::clone(&source);
        let tx = self.directory_tree_strip_preview_tx.clone();
        let release_tx = self.directory_tree_strip_inflight_release_tx.clone();
        let max_side = self
            .settings
            .directory_tree_list_preview_size
            .strip_max_side();
        DIRECTORY_TREE_STRIP_POOL.spawn(move || {
            let mut decoded = DecodedImage::new(0, 0, Vec::new());
            crate::preload_debug!(
                "[PreloadDebug][Strip] worker start idx={} logical={}x{} max_side={}",
                index,
                logical.0,
                logical.1,
                max_side
            );
            #[cfg(target_os = "windows")]
            let com_ok = strip_worker_com_initialized();
            #[cfg(not(target_os = "windows"))]
            let com_ok = true;
            if com_ok {
                let gen_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    source.generate_full_image_preview(max_side, max_side)
                }));
                if let Ok((pw, ph, pixels)) = gen_result {
                    if pw > 0 && ph > 0 {
                        decoded = DecodedImage::new(pw, ph, pixels);
                    }
                }
            } else {
                log::warn!(
                    "[DirectoryTree] COM init failed for strip preview worker index {index}"
                );
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
            if let Err(err) = tx.send(job) {
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

        self.directory_tree_strip_cold_attempted.retain(|index| {
            self.directory_tree_strip_cache.contains(*index)
                || self.directory_tree_strip_generate_inflight.contains(index)
        });
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
            if self.directory_tree_strip_cache.contains(index) {
                continue;
            }
            if self
                .deferred_sdr_uploads
                .get(&index)
                .is_some_and(crate::loader::decoded_looks_like_black_placeholder)
            {
                continue;
            }
            let Some(decoded) = self.deferred_sdr_uploads.get(&index).cloned() else {
                continue;
            };
            self.cache_directory_tree_strip_thumbnail(
                index,
                &decoded,
                PreviewStage::Initial,
                self.directory_tree_strip_logical_size(index),
                ctx,
            );
        }

        let (visible_row_range, scroll_to_current_pending, defer_sync) = {
            match self.directory_tree.state.try_lock() {
                Some(state) => (
                    state.image_list_visible_row_range,
                    state.scroll_image_list_to_current,
                    false,
                ),
                None => (None, false, true),
            }
        };
        if defer_sync {
            self.defer_directory_tree_file_list_sync();
        }
        let bootstrap_visible = self.directory_tree_strip_bootstrap_after_scan;
        let cold_candidates = self.collect_cold_strip_thumbnail_candidates(
            visible_row_range,
            scroll_to_current_pending,
            bootstrap_visible,
        );
        if bootstrap_visible && visible_row_range.is_some() {
            self.directory_tree_strip_bootstrap_after_scan = false;
        }
        let inflight_room = MAX_STRIP_GENERATE_INFLIGHT
            .saturating_sub(self.directory_tree_strip_generate_inflight.len());
        let mut cold_scheduled = 0usize;
        if !self.scanning {
            for index in cold_candidates {
                if cold_scheduled >= MAX_COLD_STRIP_GENERATES_PER_FRAME.min(inflight_room) {
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
            let ui_preview_count = self
                .directory_tree
                .state
                .try_lock()
                .map(|s| s.preview_textures.len())
                .unwrap_or(0);
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

    pub(crate) fn invalidate_directory_tree_strip_after_image_list_reorder(&mut self) {
        self.directory_tree_strip_cache.clear_all();
        self.directory_tree_strip_generate_inflight.clear();
        self.directory_tree_strip_tiled_attempted.clear();
        self.directory_tree_strip_cold_attempted.clear();
        if let Some(mut state) = self.directory_tree.state.try_lock() {
            state.image_list_generation = state.image_list_generation.wrapping_add(1);
            state.clear_list_preview_textures();
        }
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
        if let Some(mut state) = self.directory_tree.state.try_lock() {
            state.clear_list_preview_textures();
            DirectoryTreeListPreviewLayout::from_settings(&self.settings)
                .apply_to_state(&mut state);
        }
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
