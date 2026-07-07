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

//! Strip preview scheduling: worker spawns, texture-cache sync, cold candidates.

use std::sync::Arc;

use crate::app::ImageViewerApp;
use crate::app::directory_tree_strip_cache::{
    DirectoryTreeStripInflightReleaseKind, DirectoryTreeStripJobKey,
    DirectoryTreeStripPreviewFailure, DirectoryTreeStripPreviewJobResult,
    DirectoryTreeStripPreviewSuccess, StripPreviewBufferTag, decoded_rgba_size_valid,
};
use crate::app::image_management::should_defer_neighbor_work_for_current_main;
use crate::loader::DIRECTORY_TREE_STRIP_POOL;
use crate::loader::{
    DecodedImage, DirectoryTreeThumbDecodeOptions, DirectoryTreeThumbSlowPrimarySkipReason,
    PreviewStage, STRIP_DEFER_SLOW_EMBEDDED_SDR, STRIP_DEFER_SLOW_STATIC_FULL_DECODE,
    downsample_decoded_for_strip, generate_directory_tree_thumb_decode_from_path,
    preview_aspect_matches_logical,
};

#[cfg(target_os = "windows")]
use super::super::workers::ensure_strip_worker_com_initialized;
use super::{BOOTSTRAP_STRIP_VISIBLE_ROW_CAP, DIRECTORY_TREE_COLD_NEIGHBOR_RADIUS};

use super::send_strip_inflight_release;

impl ImageViewerApp {
    pub(crate) fn should_defer_neighbor_strip_for_current_main(&self, index: usize) -> bool {
        if index == self.current_index && !self.has_loaded_asset(self.current_index) {
            return true;
        }
        if index == self.current_index {
            return false;
        }
        if !should_defer_neighbor_work_for_current_main(
            self.has_loaded_asset(self.current_index),
            self.loader.is_loading(self.current_index),
        ) {
            return false;
        }
        // Neighbors with independent strip decode (no embedded-SDR sharing with main loader)
        // may run cheap cold paths in parallel instead of waiting for the current main decode.
        self.image_files
            .get(index)
            .is_some_and(|path| self.strip_path_benefits_from_main_loader_embedded_sdr_share(path))
    }

    pub(super) fn try_schedule_strip_from_preloaded_iso_baseline_with_pixels(
        &mut self,
        index: usize,
        width: u32,
        height: u32,
        baseline: Arc<Vec<u8>>,
    ) -> bool {
        if self.should_defer_neighbor_strip_for_current_main(index) {
            return false;
        }
        if !self.strip_needs_iso_baseline_sync_inner(index, true) {
            return false;
        }
        let Some(job_key) = self.begin_directory_tree_strip_job(index) else {
            return false;
        };
        let tx = self.directory_tree_strip_preview_tx.clone();
        let release_tx = self.directory_tree_strip_inflight_release_tx.clone();
        let max_side = self
            .settings
            .directory_tree_list_preview_size
            .strip_max_side();
        let root_wake = self.root_redraw_wake_handle();
        #[cfg(feature = "preload-debug")]
        crate::preload_debug_throttled!(
            &format!("strip:iso_baseline_submit:{index}"),
            crate::preload_debug::PRELOAD_DEBUG_THROTTLE_INTERVAL,
            "[PreloadDebug][Strip] pool submit idx={} kind=iso_baseline_sync max_side={}",
            index,
            max_side
        );
        DIRECTORY_TREE_STRIP_POOL.spawn(move || {
            let decoded = DecodedImage::from_arc(width, height, baseline);
            let strip = match downsample_decoded_for_strip(&decoded, max_side) {
                Ok(strip) => strip,
                Err(err) => {
                    log::debug!(
                        "[DirectoryTree] Strip ISO baseline sync failed for index {index}: {err}"
                    );
                    send_strip_inflight_release(
                        &release_tx,
                        job_key.clone(),
                        DirectoryTreeStripInflightReleaseKind::ClearAttempt,
                        root_wake.as_ref(),
                    );
                    return;
                }
            };
            if !preview_aspect_matches_logical(strip.width, strip.height, width, height) {
                send_strip_inflight_release(
                    &release_tx,
                    job_key.clone(),
                    DirectoryTreeStripInflightReleaseKind::ClearAttempt,
                    root_wake.as_ref(),
                );
                return;
            }
            let job =
                DirectoryTreeStripPreviewJobResult::Success(DirectoryTreeStripPreviewSuccess {
                    key: job_key.clone(),
                    decoded: strip,
                    reusable_full_decoded: None,
                    logical: (width, height),
                    stage: PreviewStage::Initial,
                    buffer_tag: StripPreviewBufferTag::IsoGainMapBaseline,
                    strip_max_side_used: max_side,
                });
            if tx.try_send(job).is_ok() {
                if let Some(wake) = root_wake {
                    wake();
                }
            } else {
                send_strip_inflight_release(
                    &release_tx,
                    job_key.clone(),
                    DirectoryTreeStripInflightReleaseKind::ClearAttempt,
                    root_wake.as_ref(),
                );
            }
        });
        true
    }

    pub(crate) fn try_schedule_strip_from_hdr_image_cache(&mut self, index: usize) -> bool {
        if self.should_defer_neighbor_strip_for_current_main(index) {
            return false;
        }
        let Some(hdr) = self.hdr_image_cache.get(&index).cloned() else {
            return false;
        };
        if !self.strip_needs_hdr_cache_sync_for_hdr(index, hdr.as_ref()) {
            return false;
        }
        let fallback = self.strip_fallback_for_hdr_cache_sync(index, hdr.as_ref());
        let target_tag = crate::app::directory_tree_strip_cache::strip_buffer_tag_for_hdr_preview(
            !hdr.rgba_f32.is_empty(),
            false,
        );
        if target_tag == StripPreviewBufferTag::SdrDeferredPlaceholder {
            #[cfg(feature = "preload-debug")]
            crate::preload_debug!(
                "[PreloadDebug][Strip] skip hdr_cache_sync idx={} reason=deferred_placeholder_tag",
                index
            );
            return false;
        }
        let stage = PreviewStage::Refined;

        let Some(job_key) = self.begin_directory_tree_strip_job(index) else {
            return false;
        };
        let tx = self.directory_tree_strip_preview_tx.clone();
        let release_tx = self.directory_tree_strip_inflight_release_tx.clone();
        let max_side = self
            .settings
            .directory_tree_list_preview_size
            .strip_max_side();
        let root_wake = self.root_redraw_wake_handle();
        let fallback_logical = (fallback.width, fallback.height);
        let hdr_has_float_pixels = !hdr.rgba_f32.is_empty();
        #[cfg(feature = "preload-debug")]
        crate::preload_debug!(
            "[PreloadDebug][Strip] pool submit idx={} kind=hdr_cache_sync max_side={} target_tag={target_tag:?}",
            index,
            max_side
        );
        DIRECTORY_TREE_STRIP_POOL.spawn(move || {
            let decoded = match crate::loader::directory_tree_strip_from_hdr_or_fallback(
                hdr.as_ref(),
                &fallback,
                max_side,
            ) {
                Ok(decoded) => decoded,
                Err(err) => {
                    log::debug!(
                        "[DirectoryTree] Strip HDR cache sync failed for index {index}: {err}"
                    );
                    send_strip_inflight_release(
                        &release_tx,
                        job_key.clone(),
                        DirectoryTreeStripInflightReleaseKind::ClearAttempt,
                        root_wake.as_ref(),
                    );
                    return;
                }
            };
            let logical = if hdr_has_float_pixels {
                (hdr.width, hdr.height)
            } else {
                crate::loader::directory_tree_strip_logical_for_preview(
                    hdr.width,
                    hdr.height,
                    fallback_logical.0,
                    fallback_logical.1,
                    decoded.width,
                    decoded.height,
                    false,
                )
            };
            let buffer_tag =
                crate::app::directory_tree_strip_cache::strip_buffer_tag_for_hdr_preview(
                    hdr_has_float_pixels,
                    decoded.is_sdr_deferred_placeholder(),
                );
            let job =
                DirectoryTreeStripPreviewJobResult::Success(DirectoryTreeStripPreviewSuccess {
                    key: job_key.clone(),
                    decoded,
                    reusable_full_decoded: None,
                    logical,
                    stage,
                    buffer_tag,
                    strip_max_side_used: max_side,
                });
            if tx.try_send(job).is_ok() {
                if let Some(wake) = root_wake {
                    wake();
                }
            } else {
                send_strip_inflight_release(
                    &release_tx,
                    job_key.clone(),
                    DirectoryTreeStripInflightReleaseKind::ClearAttempt,
                    root_wake.as_ref(),
                );
            }
        });
        true
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
        if !self
            .directory_tree_strip_cache
            .strip_texture_handle_would_replace(
                index,
                crate::loader::PreviewStage::Refined,
                StripPreviewBufferTag::MainWindowTiledPreview,
                Some(logical),
                preview_w,
                preview_h,
            )
        {
            return;
        }
        let _ = self.directory_tree_strip_cache.insert_from_texture_handle(
            index,
            texture,
            crate::loader::PreviewStage::Refined,
            StripPreviewBufferTag::MainWindowTiledPreview,
            Some(logical),
            &self.image_files[index],
        );
    }

    pub(crate) fn try_sync_strip_from_texture_cache(&mut self, index: usize) {
        // GPU texture clone only; no CPU decode — do not defer while the current main loads.
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
        if self.strip_main_loader_sdr_unreliable_for_strip(index) {
            #[cfg(feature = "preload-debug")]
            crate::preload_debug!(
                "[PreloadDebug][Strip] skip texture_cache sync idx={} reason=unreliable_main_sdr",
                index
            );
            return;
        }
        let Some(logical) = self.directory_tree_strip_logical_size(index) else {
            return;
        };
        let Some(texture) = self.texture_cache.get(index) else {
            return;
        };
        let size = texture.size();
        let preview_w = size[0] as u32;
        let preview_h = size[1] as u32;
        if !preview_aspect_matches_logical(preview_w, preview_h, logical.0, logical.1) {
            return;
        }
        if !self
            .directory_tree_strip_cache
            .strip_texture_handle_would_replace(
                index,
                crate::loader::PreviewStage::Refined,
                StripPreviewBufferTag::MainWindowTextureCacheSdr,
                Some(logical),
                preview_w,
                preview_h,
            )
        {
            return;
        }
        if self.directory_tree_strip_cache.insert_from_texture_handle(
            index,
            texture,
            crate::loader::PreviewStage::Refined,
            StripPreviewBufferTag::MainWindowTextureCacheSdr,
            Some(logical),
            &self.image_files[index],
        ) {
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
    }

    fn try_push_cold_strip_candidate(
        &mut self,
        index: usize,
        total: usize,
        schedule_budget: usize,
        bootstrap_visible: bool,
        visible_row_range: Option<(usize, usize)>,
    ) -> bool {
        let in_visible_bootstrap = bootstrap_visible
            && visible_row_range.is_some_and(|(start, end)| index >= start && index < end);
        let bootstrap_bypass_defer = in_visible_bootstrap && index != self.current_index;
        if self.should_defer_neighbor_strip_for_current_main(index) && !bootstrap_bypass_defer {
            return false;
        }
        if index < total && !self.strip_cold_seen_scratch.contains(&index) {
            self.strip_cold_seen_scratch.push(index);
            if self.strip_index_needs_cold_thumbnail(index) {
                self.strip_cold_candidates_scratch.push(index);
            }
        }
        self.strip_cold_candidates_scratch.len() >= schedule_budget
    }

    pub(super) fn collect_cold_strip_thumbnail_candidates(
        &mut self,
        visible_row_range: Option<(usize, usize)>,
        scroll_to_current_pending: bool,
        bootstrap_visible: bool,
        schedule_budget: usize,
    ) -> usize {
        self.strip_cold_candidates_scratch.clear();
        self.strip_cold_seen_scratch.clear();

        let total = self.image_files.len();
        if total == 0 || schedule_budget == 0 {
            return 0;
        }
        if scroll_to_current_pending && !bootstrap_visible {
            return 0;
        }
        let current = self.current_index.min(total.saturating_sub(1));

        if bootstrap_visible {
            if let Some((start, end)) = visible_row_range {
                for index in start..end.min(total) {
                    if self.try_push_cold_strip_candidate(
                        index,
                        total,
                        schedule_budget,
                        bootstrap_visible,
                        visible_row_range,
                    ) {
                        return self.strip_cold_candidates_scratch.len();
                    }
                }
            }
            for index in 0..total.min(BOOTSTRAP_STRIP_VISIBLE_ROW_CAP) {
                if self.try_push_cold_strip_candidate(
                    index,
                    total,
                    schedule_budget,
                    bootstrap_visible,
                    visible_row_range,
                ) {
                    return self.strip_cold_candidates_scratch.len();
                }
            }
            return self.strip_cold_candidates_scratch.len();
        }

        if let Some((start, end)) = visible_row_range {
            for index in start..end.min(total) {
                if self.try_push_cold_strip_candidate(
                    index,
                    total,
                    schedule_budget,
                    bootstrap_visible,
                    visible_row_range,
                ) {
                    return self.strip_cold_candidates_scratch.len();
                }
            }
        }

        if !bootstrap_visible
            && self.try_push_cold_strip_candidate(
                current,
                total,
                schedule_budget,
                bootstrap_visible,
                visible_row_range,
            )
        {
            return self.strip_cold_candidates_scratch.len();
        }

        for delta in 1..=DIRECTORY_TREE_COLD_NEIGHBOR_RADIUS {
            if self.try_push_cold_strip_candidate(
                current.saturating_sub(delta),
                total,
                schedule_budget,
                bootstrap_visible,
                visible_row_range,
            ) {
                return self.strip_cold_candidates_scratch.len();
            }
            if current + delta < total
                && self.try_push_cold_strip_candidate(
                    current + delta,
                    total,
                    schedule_budget,
                    bootstrap_visible,
                    visible_row_range,
                )
            {
                return self.strip_cold_candidates_scratch.len();
            }
        }

        self.strip_cold_candidates_scratch.len()
    }

    pub(crate) fn try_generate_cold_directory_tree_strip_thumbnail(&mut self, index: usize) {
        if self.should_defer_neighbor_strip_for_current_main(index) {
            #[cfg(feature = "preload-debug")]
            crate::preload_debug!(
                "[PreloadDebug][Strip] skip cold idx={} reason=current_main_in_flight",
                index
            );
            return;
        }
        if !self.strip_index_needs_cold_thumbnail(index) {
            return;
        }
        let path = self.image_files[index].clone();
        let shares_main_embedded_sdr =
            self.strip_path_benefits_from_main_loader_embedded_sdr_share(&path);
        let skip_slow_embedded_sdr_primary =
            self.strip_cold_skip_slow_embedded_sdr_primary(index) && shares_main_embedded_sdr;
        let shares_main_static_full_decode =
            self.strip_cold_static_full_decode_can_share_with_main(index, &path);
        let skip_slow_static_full_decode = self
            .strip_cold_skip_slow_static_full_decode_primary(index, shares_main_static_full_decode);
        let slow_primary_skip_reason = if skip_slow_embedded_sdr_primary {
            DirectoryTreeThumbSlowPrimarySkipReason::EmbeddedSdr
        } else if skip_slow_static_full_decode {
            DirectoryTreeThumbSlowPrimarySkipReason::StaticFullDecode
        } else {
            DirectoryTreeThumbSlowPrimarySkipReason::None
        };
        let defer_iso_baseline = self.strip_embedded_sdr_master_mode_active()
            && slow_primary_skip_reason == DirectoryTreeThumbSlowPrimarySkipReason::EmbeddedSdr;
        self.directory_tree_strip_cold_attempted.insert(index);
        let Some(job_key) = self.begin_directory_tree_strip_job(index) else {
            self.directory_tree_strip_cold_attempted.remove(&index);
            return;
        };
        if shares_main_static_full_decode {
            self.directory_tree_strip_static_full_decode_inflight
                .insert(index);
        }
        let tx = self.directory_tree_strip_preview_tx.clone();
        let release_tx = self.directory_tree_strip_inflight_release_tx.clone();
        let root_wake = self.root_redraw_wake_handle();
        let max_side = self
            .settings
            .directory_tree_list_preview_size
            .strip_max_side();
        #[cfg(feature = "preload-debug")]
        crate::preload_debug!(
            "[PreloadDebug][Strip] pool submit idx={} path={} kind=cold max_side={}",
            index,
            path.display(),
            max_side
        );
        DIRECTORY_TREE_STRIP_POOL.spawn(move || {
            #[cfg(target_os = "windows")]
            let com_ok = ensure_strip_worker_com_initialized();
            #[cfg(not(target_os = "windows"))]
            let com_ok = true;

            let mut decoded = DecodedImage::new(0, 0, Vec::new());
            let mut reusable_full_decoded = None;
            let mut logical = (0u32, 0u32);
            let mut buffer_tag = StripPreviewBufferTag::StripDecodedPixels;
            let stage = PreviewStage::Initial;
            let mut cold_deferred_to_main_loader = false;
            if com_ok {
                let decode_options = DirectoryTreeThumbDecodeOptions {
                    skip_slow_primary: slow_primary_skip_reason,
                    defer_iso_gain_map_baseline: defer_iso_baseline,
                };
                match generate_directory_tree_thumb_decode_from_path(
                    &path,
                    max_side,
                    decode_options,
                ) {
                    Ok(strip_decode) => {
                        decoded = strip_decode.preview;
                        logical = strip_decode.logical_size;
                        reusable_full_decoded = strip_decode.reusable_full;
                        if strip_decode.from_embedded_sdr_preview {
                            buffer_tag = StripPreviewBufferTag::PreloadSdrFallback;
                        }
                    }
                    Err(err)
                        if err == STRIP_DEFER_SLOW_EMBEDDED_SDR
                            || err == STRIP_DEFER_SLOW_STATIC_FULL_DECODE =>
                    {
                        cold_deferred_to_main_loader = true;
                        #[cfg(feature = "preload-debug")]
                        crate::preload_debug!(
                            "[PreloadDebug][Strip] cold deferred idx={} reason=await_main_loader_primary",
                            index
                        );
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
            #[cfg(feature = "preload-debug")]
            crate::preload_debug!(
                "[PreloadDebug][Strip] cold worker done idx={} out={}x{} logical={}x{} aspect_ok={} placeholder={} from_embedded_sdr={} buffer_tag={:?} stage={:?}",
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
                decoded.is_sdr_deferred_placeholder(),
                buffer_tag == StripPreviewBufferTag::PreloadSdrFallback,
                buffer_tag,
                stage
            );
            let send_result = if cold_deferred_to_main_loader {
                let job = DirectoryTreeStripPreviewJobResult::DeferredToMainLoader(
                    DirectoryTreeStripPreviewFailure {
                        key: job_key.clone(),
                        reason: "await_main_loader_primary",
                    },
                );
                tx.try_send(job)
            } else if decoded.width == 0
                || decoded.height == 0
                || !decoded_rgba_size_valid(&decoded)
                || !preview_aspect_matches_logical(decoded.width, decoded.height, logical.0, logical.1)
            {
                send_strip_inflight_release(
                    &release_tx,
                    job_key.clone(),
                    DirectoryTreeStripInflightReleaseKind::PermanentFailure,
                    root_wake.as_ref(),
                );
                return;
            } else {
                let job = DirectoryTreeStripPreviewJobResult::Success(DirectoryTreeStripPreviewSuccess {
                    key: job_key.clone(),
                    decoded,
                    reusable_full_decoded,
                    logical,
                    stage,
                    buffer_tag,
                    strip_max_side_used: max_side,
                });
                tx.try_send(job)
            };
            if send_result.is_ok() {
                if let Some(wake) = &root_wake {
                    wake();
                }
            } else if let Err(err) = send_result {
                log::warn!(
                    "[DirectoryTree] Cold strip preview result dropped for index {index}: {err}"
                );
                send_strip_inflight_release(
                    &release_tx,
                    job_key.clone(),
                    DirectoryTreeStripInflightReleaseKind::ClearAttempt,
                    root_wake.as_ref(),
                );
            }
        });
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

        #[cfg(feature = "preload-debug")]
        let path = self.image_files.get(index).cloned().unwrap_or_default();
        self.directory_tree_strip_tiled_attempted.insert(index);
        let Some(job_key) = self.begin_directory_tree_strip_job(index) else {
            self.directory_tree_strip_tiled_attempted.remove(&index);
            return;
        };
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
            if decoded.width == 0
                || decoded.height == 0
                || !decoded_rgba_size_valid(&decoded)
                || !preview_aspect_matches_logical(
                    decoded.width,
                    decoded.height,
                    logical.0,
                    logical.1,
                )
            {
                send_strip_inflight_release(
                    &release_tx,
                    job_key.clone(),
                    DirectoryTreeStripInflightReleaseKind::PermanentFailure,
                    root_wake.as_ref(),
                );
                return;
            }
            let job =
                DirectoryTreeStripPreviewJobResult::Success(DirectoryTreeStripPreviewSuccess {
                    key: job_key.clone(),
                    decoded,
                    reusable_full_decoded: None,
                    logical,
                    stage: PreviewStage::Refined,
                    buffer_tag: StripPreviewBufferTag::StripDecodedPixels,
                    strip_max_side_used: max_side,
                });
            let send_result = tx.try_send(job);
            if send_result.is_ok() {
                if let Some(wake) = &root_wake {
                    wake();
                }
            } else if let Err(err) = send_result {
                log::warn!("[DirectoryTree] Strip preview result dropped for index {index}: {err}");
                send_strip_inflight_release(
                    &release_tx,
                    job_key.clone(),
                    DirectoryTreeStripInflightReleaseKind::ClearAttempt,
                    root_wake.as_ref(),
                );
            }
        });
    }

    /// Re-downsample strip pixels on the worker pool when the current `strip_max_side` no longer
    /// matches what produced `decoded` (e.g. user changed list preview size before GPU flush).
    pub(super) fn schedule_strip_pending_gpu_resample(
        &mut self,
        index: usize,
        decoded: DecodedImage,
        stage: PreviewStage,
        logical: Option<(u32, u32)>,
        buffer_tag: StripPreviewBufferTag,
        source_job_key: Option<DirectoryTreeStripJobKey>,
    ) -> bool {
        if !self.directory_tree_list_previews_active() || index >= self.image_files.len() {
            return false;
        }
        if let Some(key) = source_job_key.as_ref()
            && !self.directory_tree_strip_key_matches_current_list(key)
        {
            self.clear_strip_preview_attempt_state_for_key(key);
            return false;
        }
        if self.directory_tree_strip_generate_inflight.contains(&index) {
            return false;
        }
        let Some(job_key) = self.begin_directory_tree_strip_job(index) else {
            return false;
        };
        let tx = self.directory_tree_strip_preview_tx.clone();
        let release_tx = self.directory_tree_strip_inflight_release_tx.clone();
        let max_side = self
            .settings
            .directory_tree_list_preview_size
            .strip_max_side();
        let root_wake = self.root_redraw_wake_handle();
        let logical = logical.unwrap_or((decoded.width, decoded.height));
        #[cfg(feature = "preload-debug")]
        crate::preload_debug!(
            "[PreloadDebug][Strip] pool submit idx={} kind=pending_gpu_resample max_side={}",
            index,
            max_side
        );
        DIRECTORY_TREE_STRIP_POOL.spawn(move || {
            let strip = match downsample_decoded_for_strip(&decoded, max_side) {
                Ok(strip) => strip,
                Err(err) => {
                    log::warn!(
                        "[DirectoryTree] Strip pending GPU resample failed for index {index}: {err}"
                    );
                    send_strip_inflight_release(
                        &release_tx,
                        job_key.clone(),
                        DirectoryTreeStripInflightReleaseKind::ClearAttempt,
                        root_wake.as_ref(),
                    );
                    return;
                }
            };
            if !preview_aspect_matches_logical(strip.width, strip.height, logical.0, logical.1) {
                send_strip_inflight_release(
                    &release_tx,
                    job_key.clone(),
                    DirectoryTreeStripInflightReleaseKind::ClearAttempt,
                    root_wake.as_ref(),
                );
                return;
            }
            let job =
                DirectoryTreeStripPreviewJobResult::Success(DirectoryTreeStripPreviewSuccess {
                    key: job_key.clone(),
                    decoded: strip,
                    reusable_full_decoded: None,
                    logical,
                    stage,
                    buffer_tag,
                    strip_max_side_used: max_side,
                });
            if tx.try_send(job).is_ok() {
                if let Some(wake) = root_wake {
                    wake();
                }
            } else {
                send_strip_inflight_release(
                    &release_tx,
                    job_key.clone(),
                    DirectoryTreeStripInflightReleaseKind::ClearAttempt,
                    root_wake.as_ref(),
                );
            }
        });
        true
    }
}
