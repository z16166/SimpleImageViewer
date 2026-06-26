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

//! Strip preview need-checks, logical size lookup, and cache helper predicates.

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
use super::super::workers::ensure_strip_worker_com_initialized;
use super::{
    BOOTSTRAP_STRIP_VISIBLE_ROW_CAP, DIRECTORY_TREE_COLD_NEIGHBOR_RADIUS,
    DirectoryTreeListPreviewLayout, MAX_COLD_STRIP_GENERATES_PER_FRAME,
    MAX_COLD_STRIP_GENERATES_PER_FRAME_BOOTSTRAP, MAX_COLD_STRIP_SCHEDULE_PER_FRAME,
    MAX_DIRECTORY_TREE_STRIP_BOOTSTRAP_FRAMES, MAX_STRIP_GENERATE_INFLIGHT,
    MAX_STRIP_GENERATE_INFLIGHT_BOOTSTRAP, MAX_TILED_STRIP_GENERATES_PER_FRAME, domains, view,
};

impl ImageViewerApp {

    fn strip_hdr_animated_awaiting_real_strip_preview(&self, index: usize) -> bool {
        self.pending_anim_frames
            .get(&index)
            .is_some_and(|pending| pending.hdr_frames.is_some())
    }


    pub(crate) fn strip_main_loader_sdr_unreliable_for_strip(&self, index: usize) -> bool {
        if self.hdr_placeholder_fallback_indices.contains(&index) {
            return true;
        }
        if self.strip_hdr_animated_awaiting_real_strip_preview(index) {
            return true;
        }
        if self.iso_deferred_baseline_pixels_for_strip(index).is_some() {
            return false;
        }
        self.hdr_image_cache
            .get(&index)
            .is_some_and(|hdr| crate::loader::hdr_has_iso_deferred_gain_map(hdr.as_ref()))
    }


    fn iso_deferred_baseline_pixels_for_strip(
        &self,
        index: usize,
    ) -> Option<(u32, u32, std::sync::Arc<Vec<u8>>)> {
        if let Some(hdr) = self.hdr_image_cache.get(&index) {
            if let Some(iso) = hdr
                .metadata
                .gain_map
                .as_ref()
                .and_then(|gain_map| gain_map.iso_deferred.as_ref())
            {
                return Some((hdr.width, hdr.height, Arc::clone(&iso.sdr_rgba)));
            }
        }
        if let Some(decoded) = self.deferred_sdr_uploads.get(&index) {
            if !decoded.is_sdr_deferred_placeholder() {
                return Some((decoded.width, decoded.height, decoded.arc_pixels()));
            }
        }
        None
    }


    /// One pass over preload caches for indices `< up_to` (checklist #29 / bootstrap batching).
    pub(super) fn collect_iso_baseline_pixels_up_to(
        &self,
        up_to: usize,
    ) -> HashMap<usize, (u32, u32, Arc<Vec<u8>>)> {
        let up_to = up_to.min(self.image_files.len());
        let mut baselines = HashMap::new();
        for (&index, hdr) in &self.hdr_image_cache {
            if index >= up_to {
                continue;
            }
            if let Some(iso) = hdr
                .metadata
                .gain_map
                .as_ref()
                .and_then(|gain_map| gain_map.iso_deferred.as_ref())
            {
                baselines.insert(index, (hdr.width, hdr.height, Arc::clone(&iso.sdr_rgba)));
            }
        }
        for (&index, decoded) in &self.deferred_sdr_uploads {
            if index >= up_to || baselines.contains_key(&index) {
                continue;
            }
            if !decoded.is_sdr_deferred_placeholder() {
                baselines.insert(index, (decoded.width, decoded.height, decoded.arc_pixels()));
            }
        }
        baselines
    }


    pub(super) fn strip_needs_iso_baseline_sync_inner(&self, index: usize, has_baseline: bool) -> bool {
        if index >= self.image_files.len() {
            return false;
        }
        if self.directory_tree_strip_generate_inflight.contains(&index) {
            return false;
        }
        if !has_baseline {
            return false;
        }
        let target_rank = crate::app::directory_tree_strip_cache::strip_preview_quality_rank(
            StripPreviewBufferTag::IsoGainMapBaseline,
            PreviewStage::Initial,
        );
        if let Some(cached_tag) = self.directory_tree_strip_cache.cached_buffer_tag(index) {
            let cached_stage = self
                .directory_tree_strip_cache
                .cached_preview_stage(index)
                .unwrap_or(PreviewStage::Initial);
            let cached_rank = crate::app::directory_tree_strip_cache::strip_preview_quality_rank(
                cached_tag, cached_stage,
            );
            if cached_rank >= target_rank {
                if let Some(logical) = self.directory_tree_strip_logical_size(index) {
                    return !self
                        .directory_tree_strip_cache
                        .is_valid_for_logical(index, logical);
                }
                return false;
            }
        }
        true
    }


    pub(super) fn strip_fallback_for_hdr_cache_sync(
        &self,
        index: usize,
        hdr: &crate::hdr::types::HdrImageBuffer,
    ) -> DecodedImage {
        if let Some((width, height, baseline)) = self.iso_deferred_baseline_pixels_for_strip(index)
        {
            return DecodedImage::from_arc(width, height, baseline);
        }
        if let Some(decoded) = self.deferred_sdr_uploads.get(&index) {
            if !decoded.is_sdr_deferred_placeholder() {
                return decoded.clone();
            }
        }
        if let Some(preview) = crate::loader::hdr_raw_gpu_bootstrap_fallback_decoded(hdr) {
            return preview;
        }
        let mut placeholder = crate::loader::cheap_hdr_sdr_placeholder_rgba8(hdr.width, hdr.height)
            .map(|pixels| DecodedImage::new(hdr.width, hdr.height, pixels))
            .unwrap_or_else(|_| DecodedImage::new(hdr.width, hdr.height, Vec::new()));
        placeholder.mark_sdr_deferred_placeholder();
        placeholder
    }


    pub(super) fn strip_needs_hdr_cache_sync_for_hdr(
        &self,
        index: usize,
        hdr: &crate::hdr::types::HdrImageBuffer,
    ) -> bool {
        if index >= self.image_files.len() {
            return false;
        }
        if self.directory_tree_strip_generate_inflight.contains(&index) {
            return false;
        }
        if crate::loader::hdr_has_iso_deferred_gain_map(hdr) && hdr.rgba_f32.is_empty() {
            return false;
        }
        let Some(cached_tag) = self.directory_tree_strip_cache.cached_buffer_tag(index) else {
            return true;
        };
        let cached_stage = self.directory_tree_strip_cache.cached_preview_stage(index);
        let fallback = self.strip_fallback_for_hdr_cache_sync(index, hdr);
        // ISO-deferred empty-float entries use the baseline sync path (early return above).
        let target_tag =
            crate::app::directory_tree_strip_cache::strip_buffer_tag_for_hdr_preview(
                !hdr.rgba_f32.is_empty(),
                fallback.is_sdr_deferred_placeholder(),
                false,
                false,
            );
        if target_tag == StripPreviewBufferTag::SdrDeferredPlaceholder {
            return false;
        }
        let target_rank = crate::app::directory_tree_strip_cache::strip_preview_quality_rank(
            target_tag,
            PreviewStage::Refined,
        );
        let cached_rank = crate::app::directory_tree_strip_cache::strip_preview_quality_rank(
            cached_tag,
            cached_stage.unwrap_or(PreviewStage::Initial),
        );
        if cached_rank < target_rank {
            return true;
        }
        let Some(logical) = self.directory_tree_strip_logical_size(index) else {
            return false;
        };
        if self
            .directory_tree_strip_cache
            .is_valid_for_logical(index, logical)
        {
            return false;
        }
        true
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


    pub(super) fn tiled_sdr_source_for_index(&self, index: usize) -> Option<Arc<dyn TiledImageSource>> {
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


    pub(super) fn strip_index_needs_cold_thumbnail(&self, index: usize) -> bool {
        if index >= self.image_files.len() {
            return false;
        }
        if self.tiled_sdr_source_for_index(index).is_some() {
            return false;
        }
        if self.settings.preload
            && self.directory_tree_strip_bootstrap_after_scan
            && index < self.image_files.len().min(BOOTSTRAP_STRIP_VISIBLE_ROW_CAP)
        {
            // Strip thumbnails come from preload install / hdr cache sync; avoid a second
            // full decode on the strip pool (same CPU cost, doubles fan load).
            return false;
        }
        if self.hdr_image_cache.get(&index).is_some_and(|hdr| {
            if !hdr.rgba_f32.is_empty() {
                return true;
            }
            self.iso_deferred_baseline_pixels_for_strip(index).is_some()
        }) {
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


    pub(super) fn strip_needs_compose_upgrade(&self, index: usize) -> bool {
        if index >= self.image_files.len() {
            return false;
        }
        if self.directory_tree_strip_generate_inflight.contains(&index) {
            return false;
        }
        let Some(cached_tag) = self.directory_tree_strip_cache.cached_buffer_tag(index) else {
            return false;
        };
        let compose_rank = crate::app::directory_tree_strip_cache::strip_preview_quality_rank(
            StripPreviewBufferTag::HdrComposedStrip,
            PreviewStage::Refined,
        );
        let cached_stage = self
            .directory_tree_strip_cache
            .cached_preview_stage(index)
            .unwrap_or(PreviewStage::Initial);
        if crate::app::directory_tree_strip_cache::strip_preview_quality_rank(
            cached_tag, cached_stage,
        ) >= compose_rank
        {
            return false;
        }
        self.hdr_image_cache
            .get(&index)
            .is_some_and(|hdr| crate::loader::hdr_has_iso_deferred_gain_map(hdr.as_ref()))
    }


    /// Visible image-list row indices used for strip prefetch scheduling (unit tests).
    #[cfg(test)]
    pub(crate) fn visible_strip_row_indices(
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

}
