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
    DecodedImage, PreviewStage, TiledImageSource, generate_directory_tree_thumb_from_path,
    hdr_has_iso_deferred_gain_map, preview_aspect_matches_logical,
};

#[cfg(target_os = "windows")]
use super::super::workers::ensure_strip_worker_com_initialized;
use super::{
    BOOTSTRAP_STRIP_VISIBLE_ROW_CAP, DIRECTORY_TREE_COLD_NEIGHBOR_RADIUS,
    DirectoryTreeListPreviewLayout, MAX_COLD_STRIP_GENERATES_PER_FRAME,
    MAX_COLD_STRIP_GENERATES_PER_FRAME_BOOTSTRAP, MAX_COLD_STRIP_SCHEDULE_PER_FRAME,
    MAX_DIRECTORY_TREE_STRIP_BOOTSTRAP_FRAMES, MAX_STRIP_GENERATE_INFLIGHT,
    MAX_STRIP_GENERATE_INFLIGHT_BOOTSTRAP, MAX_TILED_STRIP_GENERATES_PER_FRAME, domains, view,
};

impl ImageViewerApp {

    pub(super) fn clear_strip_preview_attempt_state(&mut self, index: usize) {
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


    fn image_strip_path_index(&mut self) -> &HashMap<PathBuf, usize> {
        let key = (
            self.image_files.as_ptr() as usize,
            self.image_files.len(),
        );
        let stale = self
            .cached_image_strip_path_index
            .as_ref()
            .map_or(true, |(k, _)| *k != key);
        if stale {
            let map: HashMap<PathBuf, usize> = self
                .image_files
                .iter()
                .enumerate()
                .map(|(i, p)| (p.clone(), i))
                .collect();
            self.cached_image_strip_path_index = Some((key, map));
        }
        &self
            .cached_image_strip_path_index
            .as_ref()
            .expect("just inserted")
            .1
    }


    fn try_apply_relocated_strip_preview_result(
        &mut self,
        result: DirectoryTreeStripPreviewJobResult,
        ctx: &egui::Context,
    ) -> bool {
        self.clear_strip_preview_attempt_state(result.index);
        let Some(&new_index) = self.image_strip_path_index().get(&result.path) else {
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
            result.buffer_tag,
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
                result.buffer_tag,
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
                    self.directory_tree_strip_pending_gpu_initial.len()
                        + self.directory_tree_strip_pending_gpu_refined.len()
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

}
