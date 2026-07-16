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

use crate::app::ImageViewerApp;
use crate::app::directory_tree_strip_cache::StripPreviewBufferTag;
use crate::loader::{DecodedImage, PreviewStage, TiledImageSource};

#[cfg(test)]
use super::BOOTSTRAP_STRIP_VISIBLE_ROW_CAP;

fn path_extension_matches_any(path: &std::path::Path, candidates: &[&str]) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            candidates
                .iter()
                .any(|candidate| ext.eq_ignore_ascii_case(candidate))
        })
}

impl ImageViewerApp {
    // Transitional index -> path wrappers for the path-keyed strip cache and attempt state.
    // Call sites still schedule by row index; these resolve the current path and forward to
    // the authoritative path-keyed collections. A missing path (index out of range) reads as
    // "not present", matching the previous out-of-range index behavior.
    /// Build a fresh `path -> current row index` map (uncached). Prefer
    /// [`Self::image_strip_path_index`] on hot paths so the generation-gated cache is reused.
    pub(crate) fn strip_path_to_index_map(&self) -> HashMap<std::path::PathBuf, usize> {
        self.image_files
            .iter()
            .enumerate()
            .map(|(index, path)| (path.clone(), index))
            .collect()
    }

    /// Generation-cached `path -> current row index` map. Rebuilds only when
    /// `image_list_generation` changes (or the cache was cleared).
    pub(crate) fn image_strip_path_index(&mut self) -> &HashMap<std::path::PathBuf, usize> {
        #[cfg(feature = "preload-debug")]
        let cached_gen = self.cached_image_strip_path_index.as_ref().map(|(g, _)| *g);
        let generation = self.directory_tree.list.lock().image_list_generation;
        let stale = self
            .cached_image_strip_path_index
            .as_ref()
            .is_none_or(|(g, _)| *g != generation);
        if stale {
            let map = self.strip_path_to_index_map();
            #[cfg(feature = "preload-debug")]
            crate::preload_debug_throttled!(
                &format!("strip:rebuild_idx:{}", generation),
                crate::preload_debug::PRELOAD_DEBUG_THROTTLE_INTERVAL,
                "[PreloadDebug][StripIdx] rebuild cache: gen={} image_files_len={} map_len={} cached_gen={:?} stale={}",
                generation,
                self.image_files.len(),
                map.len(),
                cached_gen,
                stale,
            );
            self.cached_image_strip_path_index = Some((generation, map));
        }
        &self
            .cached_image_strip_path_index
            .as_ref()
            .expect("just inserted")
            .1
    }

    /// Resolve `path` to its current row via [`Self::image_strip_path_index`].
    #[inline]
    pub(crate) fn strip_path_current_index(&mut self, path: &std::path::Path) -> Option<usize> {
        self.image_strip_path_index().get(path).copied()
    }

    #[inline]
    pub(crate) fn strip_cache_contains_index(&self, index: usize) -> bool {
        self.image_files
            .get(index)
            .is_some_and(|path| self.directory_tree_strip_cache.contains(path))
    }

    #[inline]
    pub(super) fn strip_cache_is_valid_for_logical_index(
        &self,
        index: usize,
        logical: (u32, u32),
    ) -> bool {
        self.image_files.get(index).is_some_and(|path| {
            self.directory_tree_strip_cache
                .is_valid_for_logical(path, logical)
        })
    }

    #[inline]
    pub(super) fn strip_cache_cached_buffer_tag_index(
        &self,
        index: usize,
    ) -> Option<StripPreviewBufferTag> {
        self.image_files
            .get(index)
            .and_then(|path| self.directory_tree_strip_cache.cached_buffer_tag(path))
    }

    #[inline]
    pub(super) fn strip_cache_cached_preview_stage_index(
        &self,
        index: usize,
    ) -> Option<PreviewStage> {
        self.image_files
            .get(index)
            .and_then(|path| self.directory_tree_strip_cache.cached_preview_stage(path))
    }

    #[inline]
    pub(super) fn strip_cache_logical_sizes_contains_index(&self, index: usize) -> bool {
        self.image_files.get(index).is_some_and(|path| {
            self.directory_tree_strip_cache
                .logical_sizes()
                .contains_key(path)
        })
    }

    #[inline]
    pub(super) fn strip_generate_inflight_contains_index(&self, index: usize) -> bool {
        self.image_files
            .get(index)
            .is_some_and(|path| self.directory_tree_strip_generate_inflight.contains(path))
    }

    #[inline]
    pub(super) fn strip_static_full_decode_inflight_contains_index(&self, index: usize) -> bool {
        self.image_files.get(index).is_some_and(|path| {
            self.directory_tree_strip_static_full_decode_inflight
                .contains(path)
        })
    }

    #[inline]
    pub(crate) fn strip_cold_attempted_contains_index(&self, index: usize) -> bool {
        self.image_files
            .get(index)
            .is_some_and(|path| self.directory_tree_strip_cold_attempted.contains(path))
    }

    #[inline]
    pub(super) fn strip_cold_awaiting_contains_index(&self, index: usize) -> bool {
        self.image_files.get(index).is_some_and(|path| {
            self.directory_tree_strip_cold_awaiting_main_loader
                .contains(path)
        })
    }

    #[cfg(feature = "avif-native")]
    fn avif_strip_probe_cache_generation(&self) -> u64 {
        self.directory_tree.list.lock().image_list_generation
    }

    #[cfg(feature = "avif-native")]
    fn schedule_avif_gain_map_strip_probe(&self, path: &std::path::Path) {
        use crate::loader::DIRECTORY_TREE_STRIP_POOL;

        let path_buf = path.to_path_buf();
        {
            let cache = self.cached_avif_strip_probe.lock();
            if let Some((generation, map)) = cache.as_ref()
                && *generation == self.avif_strip_probe_cache_generation()
                && map.contains_key(&path_buf)
            {
                return;
            }
        }
        {
            let mut inflight = self.avif_strip_probe_inflight.lock();
            if !inflight.insert(path_buf.clone()) {
                return;
            }
        }

        let generation = self.avif_strip_probe_cache_generation();
        let tx = self.avif_strip_probe_result_tx.clone();
        let root_wake = self.root_redraw_wake_handle();
        DIRECTORY_TREE_STRIP_POOL.spawn(move || {
            let probe = (|| -> Option<Option<crate::hdr::avif::AvifGainMapStripProbe>> {
                let (mmap, _) = crate::mmap_util::map_file(&path_buf).ok()?;
                if crate::hdr::avif::bytes_is_avif_image_sequence(mmap.as_ref()) {
                    return Some(None);
                }
                Some(crate::hdr::avif::avif_probe_gain_map_strip_kind(
                    mmap.as_ref(),
                ))
            })()
            .flatten();
            let _ = tx.try_send(crate::app::types::AvifStripProbeJobResult {
                path: path_buf,
                image_list_generation: generation,
                probe,
            });
            if let Some(wake) = root_wake {
                wake();
            }
        });
    }

    #[cfg(feature = "avif-native")]
    pub(crate) fn poll_avif_strip_probe_results(&mut self) {
        while let Ok(result) = self.avif_strip_probe_result_rx.try_recv() {
            let current_gen = self.avif_strip_probe_cache_generation();
            if result.image_list_generation != current_gen {
                continue;
            }
            let mut cache = self.cached_avif_strip_probe.lock();
            if cache.as_ref().is_none_or(|(g, _)| *g != current_gen) {
                *cache = Some((current_gen, HashMap::new()));
            }
            let (_, map) = cache.as_mut().expect("just inserted");
            map.insert(result.path.clone(), result.probe);
            self.avif_strip_probe_inflight.lock().remove(&result.path);
        }
    }

    #[cfg(feature = "avif-native")]
    fn cached_avif_gain_map_strip_probe(
        &self,
        path: &std::path::Path,
    ) -> Option<crate::hdr::avif::AvifGainMapStripProbe> {
        let current_gen = self.avif_strip_probe_cache_generation();
        {
            let cache = self.cached_avif_strip_probe.lock();
            if let Some((generation, map)) = cache.as_ref()
                && *generation == current_gen
                && let Some(probe) = map.get(path)
            {
                return *probe;
            }
        }
        self.schedule_avif_gain_map_strip_probe(path);
        None
    }

    fn strip_hdr_animated_awaiting_real_strip_preview(&self, index: usize) -> bool {
        let Some(pending) = self.pending_anim_frames.get(&index) else {
            return false;
        };
        if pending.hdr_frames.is_none() {
            return false;
        }
        // Bootstrap first frame matches the main-window SDR fallback; once both strip cache and
        // texture cache are populated, allow texture_cache sync instead of blocking for remainder.
        if self.strip_cache_contains_index(index) && self.texture_cache.contains(index) {
            return false;
        }
        true
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
        if self
            .hdr_image_cache
            .get(&index)
            .is_some_and(|hdr| crate::loader::hdr_has_embedded_sdr_master_display(hdr.as_ref()))
        {
            return false;
        }
        self.hdr_image_cache
            .get(&index)
            .is_some_and(|hdr| crate::loader::hdr_has_iso_deferred_gain_map(hdr.as_ref()))
    }

    pub(super) fn iso_deferred_baseline_pixels_for_strip(
        &self,
        index: usize,
    ) -> Option<(u32, u32, std::sync::Arc<Vec<u8>>)> {
        if let Some(hdr) = self.hdr_image_cache.get(&index)
            && let Some(iso) = hdr
                .metadata
                .gain_map
                .as_ref()
                .and_then(|gain_map| gain_map.iso_deferred.as_ref())
        {
            return Some((hdr.width, hdr.height, Arc::clone(&iso.sdr_rgba)));
        }
        if let Some(decoded) = self.deferred_sdr_uploads.get(&index)
            && !decoded.is_sdr_deferred_placeholder()
        {
            return Some((decoded.width, decoded.height, decoded.arc_pixels()));
        }
        None
    }

    pub(super) fn strip_needs_iso_baseline_sync_inner(
        &self,
        index: usize,
        has_baseline: bool,
    ) -> bool {
        if index >= self.image_files.len() {
            return false;
        }
        if self.strip_generate_inflight_contains_index(index) {
            return false;
        }
        if !has_baseline {
            return false;
        }
        let target_rank = crate::app::directory_tree_strip_cache::strip_preview_quality_rank(
            StripPreviewBufferTag::IsoGainMapBaseline,
            PreviewStage::Initial,
        );
        if let Some(cached_tag) = self.strip_cache_cached_buffer_tag_index(index) {
            let cached_stage = self
                .strip_cache_cached_preview_stage_index(index)
                .unwrap_or(PreviewStage::Initial);
            let cached_rank = crate::app::directory_tree_strip_cache::strip_preview_quality_rank(
                cached_tag,
                cached_stage,
            );
            if cached_rank >= target_rank {
                if let Some(logical) = self.directory_tree_strip_logical_size(index) {
                    return !self.strip_cache_is_valid_for_logical_index(index, logical);
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
        if let Some(decoded) = self.deferred_sdr_uploads.get(&index)
            && !decoded.is_sdr_deferred_placeholder()
        {
            return decoded.clone();
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
        if self.strip_generate_inflight_contains_index(index) {
            return false;
        }
        if !crate::loader::hdr_directory_tree_strip_cache_sync_viable(hdr) {
            return false;
        }
        if crate::loader::hdr_has_iso_deferred_gain_map(hdr) && hdr.rgba_f32.is_empty() {
            return false;
        }
        let Some(cached_tag) = self.strip_cache_cached_buffer_tag_index(index) else {
            return true;
        };
        let cached_stage = self.strip_cache_cached_preview_stage_index(index);
        // ISO-deferred empty-float entries use the baseline sync path (early return above).
        let target_tag = crate::app::directory_tree_strip_cache::strip_buffer_tag_for_hdr_preview(
            !hdr.rgba_f32.is_empty(),
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
        if self.strip_cache_is_valid_for_logical_index(index, logical) {
            return false;
        }
        true
    }

    pub(crate) fn directory_tree_strip_logical_size(&self, index: usize) -> Option<(u32, u32)> {
        if let Some((width, height)) = self.texture_cache.get_original_res(index) {
            return Some((width, height));
        }
        if let Some(path) = self.image_files.get(index)
            && let Some(&(width, height)) =
                self.directory_tree_strip_cache.logical_sizes().get(path)
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

    pub(super) fn tiled_sdr_source_for_index(
        &self,
        index: usize,
    ) -> Option<Arc<dyn TiledImageSource>> {
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

    /// True when the main loader worker is decoding this index (current or neighbor prefetch).
    pub(crate) fn strip_main_loader_decode_in_flight(&self, index: usize) -> bool {
        self.loader.is_loading(index)
    }

    fn strip_index_within_prefetch_window(&self, index: usize) -> bool {
        let count = self.image_files.len();
        if count == 0 || !self.settings.preload {
            return false;
        }
        super::strip_full_decode_reuse_allowed(
            index,
            self.current_index.min(count - 1),
            count,
            self.prefetch_window_max_distance,
            true,
        )
    }

    fn strip_full_decode_share_window_contains(&self, index: usize) -> bool {
        let count = self.image_files.len();
        if count == 0 {
            return false;
        }
        super::strip_full_decode_reuse_allowed(
            index,
            self.current_index.min(count - 1),
            count,
            self.prefetch_window_max_distance,
            self.settings.preload,
        )
    }

    /// Static/full-raster formats whose strip cold decode can publish reusable SDR pixels.
    /// AVIF/HEIF/JXL are handled by embedded-SDR / ISO gain-map sharing instead. HDR/EXR are
    /// excluded because their strip reusable buffer is only a tone-mapped SDR fallback.
    pub(crate) fn strip_path_provides_reusable_static_full_decode(
        &mut self,
        path: &std::path::Path,
    ) -> bool {
        if let Some(&cached) = self
            .directory_tree_strip_reusable_full_decode_cache
            .get(path)
        {
            return cached;
        }
        let result = crate::loader::strip_path_provides_reusable_static_full_decode(path);
        // Bounded cache: paths are already cleared on directory change; this guard
        // prevents unbounded growth in extreme-sized folders between re-scans.
        if self.directory_tree_strip_reusable_full_decode_cache.len() >= 4096 {
            self.directory_tree_strip_reusable_full_decode_cache.clear();
        }
        self.directory_tree_strip_reusable_full_decode_cache
            .insert(path.to_path_buf(), result);
        result
    }

    pub(crate) fn strip_cold_static_full_decode_can_share_with_main(
        &mut self,
        index: usize,
        path: &std::path::Path,
    ) -> bool {
        self.strip_full_decode_share_window_contains(index)
            && self.strip_path_provides_reusable_static_full_decode(path)
    }

    pub(crate) fn strip_full_decode_inflight_should_block_main_load(&self, index: usize) -> bool {
        self.strip_generate_inflight_contains_index(index)
            && self.strip_static_full_decode_inflight_contains_index(index)
            && self.strip_full_decode_share_window_contains(index)
    }

    /// True when Viewing settings use embedded SDR master on an SDR tone-mapped output path.
    pub(crate) fn strip_embedded_sdr_master_mode_active(&self) -> bool {
        let Some((_, output_mode)) = self.effective_hdr_display_output() else {
            return false;
        };
        output_mode == crate::hdr::renderer::HdrRenderOutputMode::SdrToneMapped
            && self.settings.hdr_gain_map_sdr_display
                == crate::settings::HdrGainMapSdrDisplayMode::EmbeddedSdrMaster
    }

    /// True when deferring strip decode to the main loader can reuse embedded SDR / ISO gain-map work.
    pub(crate) fn strip_path_benefits_from_main_loader_embedded_sdr_share(
        &self,
        path: &std::path::Path,
    ) -> bool {
        if !path_extension_matches_any(path, &["avif", "avifs", "heif", "heic", "hif", "jxl"]) {
            return false;
        }
        if path_extension_matches_any(path, &["avif", "avifs"]) {
            #[cfg(feature = "avif-native")]
            {
                return matches!(
                    self.cached_avif_gain_map_strip_probe(path),
                    Some(crate::hdr::avif::AvifGainMapStripProbe::ForwardIsoGainMap)
                        | Some(crate::hdr::avif::AvifGainMapStripProbe::PrecomposedHdr)
                );
            }
            #[cfg(not(feature = "avif-native"))]
            {
                return false;
            }
        }
        path_extension_matches_any(path, &["heif", "heic", "hif", "jxl"])
    }

    fn strip_main_sdr_decode_available_or_in_flight(&self, index: usize) -> bool {
        if self.strip_main_loader_decode_in_flight(index) {
            return true;
        }
        // Prefetched/current tiled SDR (async PSD) is owned by the main loader even before
        // texture_cache has a bootstrap preview -- cold strip must not call load_psd again.
        if self.tiled_sdr_source_for_index(index).is_some() {
            return true;
        }
        !self.strip_main_loader_sdr_unreliable_for_strip(index)
            && (self.deferred_sdr_uploads.contains_key(&index)
                || self.texture_cache.contains(index))
    }

    /// Skip strip paths that duplicate the main loader; cheap embedded previews still run.
    pub(crate) fn strip_cold_skip_slow_embedded_sdr_primary(&self, index: usize) -> bool {
        if self.strip_main_sdr_decode_available_or_in_flight(index) {
            return true;
        }
        if self.hdr_image_cache.get(&index).is_some_and(|hdr| {
            crate::loader::hdr_directory_tree_strip_cache_sync_viable(hdr.as_ref())
        }) {
            return true;
        }
        if !self.strip_index_within_prefetch_window(index) {
            return false;
        }
        self.strip_prefetch_window_defers_to_main_loader(index)
    }

    /// Skip static raster full decode when the main loader is already or imminently responsible.
    pub(crate) fn strip_cold_skip_slow_static_full_decode_primary(
        &self,
        index: usize,
        can_share_with_main: bool,
    ) -> bool {
        if !can_share_with_main {
            return false;
        }
        // Tiled SDR (async PSD/PSB) is filled by the tiled strip worker; never cold-decode.
        if self.tiled_sdr_source_for_index(index).is_some() {
            return true;
        }
        if self.strip_main_loader_decode_in_flight(index) {
            return true;
        }
        if self.strip_main_sdr_decode_available_or_in_flight(index) {
            // Main already owns SDR pixels. Skip only while a strip handoff/sync path can
            // still fill the cache. After strip LRU eviction the oversized main texture
            // cannot sync directly and install will not re-run -- allow cold self-decode.
            if self
                .deferred_sdr_uploads
                .get(&index)
                .is_some_and(|decoded| !decoded.is_sdr_deferred_placeholder())
            {
                return true;
            }
            if self.strip_texture_cache_usable_for_direct_sync(index)
                && !self.strip_needs_detached_decode_from_main_texture_cache(index)
            {
                return true;
            }
            if let Some(path) = self.image_files.get(index)
                && (self.directory_tree_strip_cache.contains(path)
                    || self
                        .directory_tree_strip_pending_main_handoff
                        .contains_key(path)
                    || self.directory_tree_strip_generate_inflight.contains(path)
                    || self
                        .directory_tree_strip_pending_gpu_initial
                        .iter()
                        .any(|u| u.key.path == *path)
                    || self
                        .directory_tree_strip_pending_gpu_refined
                        .iter()
                        .any(|u| u.key.path == *path))
            {
                return true;
            }
            return false;
        }
        self.strip_prefetch_window_defers_to_main_loader(index)
    }

    /// True while the main loader is expected to decode this index soon (avoid duplicate strip slow path).
    fn strip_prefetch_window_defers_to_main_loader(&self, index: usize) -> bool {
        if !self.settings.preload {
            return false;
        }
        let cur = self.current_index;
        if self.main_loader_failed_indices.contains(&cur) {
            return self.loader.is_loading(index);
        }
        if index == cur {
            return !self.has_loaded_asset(cur) && self.loader.is_loading(cur);
        }
        if self.loader.is_loading(index) {
            return true;
        }
        let current_has_asset = self.has_loaded_asset(cur);
        let current_is_loading = self.loader.is_loading(cur);
        if crate::app::image_management::should_defer_neighbor_work_for_current_main(
            current_has_asset,
            current_is_loading,
        ) {
            return true;
        }
        false
    }

    /// True when the main window already has an SDR texture for this index but the
    /// directory-tree strip cache still needs a separate upload (detached nav viewport).
    fn strip_needs_detached_decode_from_main_texture_cache(&self, index: usize) -> bool {
        if !self.directory_tree_nav_is_detached() {
            return false;
        }
        if let Some(logical) = self.directory_tree_strip_logical_size(index) {
            return !self.strip_cache_is_valid_for_logical_index(index, logical);
        }
        !self.strip_cache_contains_index(index)
    }

    /// True when `texture_cache` can be cloned into the strip cache without paint-thread
    /// downsample (`try_sync_strip_from_texture_cache` size gate).
    fn strip_texture_cache_usable_for_direct_sync(&self, index: usize) -> bool {
        let Some(texture) = self.texture_cache.get(index) else {
            return false;
        };
        let size = texture.size();
        let preview_w = size[0] as u32;
        let preview_h = size[1] as u32;
        let strip_max_side = self
            .settings
            .directory_tree_list_preview_size
            .strip_max_side();
        if preview_w.max(preview_h) > strip_max_side {
            return false;
        }
        if let Some(logical) = self.directory_tree_strip_logical_size(index) {
            return crate::loader::preview_aspect_matches_logical(
                preview_w, preview_h, logical.0, logical.1,
            );
        }
        true
    }

    pub(crate) fn strip_index_needs_cold_thumbnail(&self, index: usize) -> bool {
        if index >= self.image_files.len() {
            return false;
        }
        if self.tiled_sdr_source_for_index(index).is_some() {
            return false;
        }
        if self.hdr_image_cache.get(&index).is_some_and(|hdr| {
            crate::loader::hdr_directory_tree_strip_cache_sync_viable(hdr.as_ref())
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
        // Oversized main-window textures (e.g. 320x320 animation on a 256 strip) cannot
        // sync via texture_cache clone; still need a cold strip decode/downsample.
        if !self.strip_main_loader_sdr_unreliable_for_strip(index)
            && self.strip_texture_cache_usable_for_direct_sync(index)
            && !self.strip_needs_detached_decode_from_main_texture_cache(index)
        {
            return false;
        }
        if self.strip_cold_awaiting_contains_index(index) {
            return false;
        }
        if self.strip_generate_inflight_contains_index(index) {
            return false;
        }
        if self.strip_cold_attempted_contains_index(index) {
            // Successful decodes that were LRU-evicted keep logical_sizes; allow visible retry.
            let evicted_after_success = !self.strip_cache_contains_index(index)
                && self.strip_cache_logical_sizes_contains_index(index);
            if !evicted_after_success {
                return false;
            }
        }
        if let Some(logical) = self.directory_tree_strip_logical_size(index) {
            if self.strip_cache_is_valid_for_logical_index(index, logical) {
                return false;
            }
        } else if self.strip_cache_contains_index(index) {
            return false;
        }
        true
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
        if bootstrap_visible {
            if !scroll_to_current_pending && let Some((start, end)) = visible_row_range {
                return (start..end.min(total)).collect();
            }
            return (0..total.min(BOOTSTRAP_STRIP_VISIBLE_ROW_CAP)).collect();
        }
        if let Some((start, end)) = visible_row_range {
            return (start..end.min(total)).collect();
        }
        Vec::new()
    }
}
