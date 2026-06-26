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
    DirectoryTreeStripPreviewJobResult, StripPreviewBufferTag,
};
use crate::loader::DIRECTORY_TREE_STRIP_POOL;
use crate::loader::{
    DecodedImage, PreviewStage, downsample_decoded_for_strip,
    generate_directory_tree_thumb_from_path,
    preview_aspect_matches_logical,
};

#[cfg(target_os = "windows")]
use super::super::workers::ensure_strip_worker_com_initialized;
use super::{
    BOOTSTRAP_STRIP_VISIBLE_ROW_CAP, DIRECTORY_TREE_COLD_NEIGHBOR_RADIUS, MAX_COLD_STRIP_SCHEDULE_PER_FRAME,
};

use super::send_strip_inflight_release;

impl ImageViewerApp {

    pub(super) fn try_schedule_strip_from_preloaded_iso_baseline_with_pixels(
        &mut self,
        index: usize,
        width: u32,
        height: u32,
        baseline: Arc<Vec<u8>>,
    ) -> bool {
        if !self.strip_needs_iso_baseline_sync_inner(index, true) {
            return false;
        }
        let Some(list) = self.directory_tree.list.try_lock() else {
            return false;
        };
        let list_generation = list.image_list_generation;
        drop(list);

        self.directory_tree_strip_generate_inflight.insert(index);
        let tx = self.directory_tree_strip_preview_tx.clone();
        let release_tx = self.directory_tree_strip_inflight_release_tx.clone();
        let path = self.image_files[index].clone();
        let max_side = self
            .settings
            .directory_tree_list_preview_size
            .strip_max_side();
        let root_wake = self.root_redraw_wake_handle();
        #[cfg(feature = "preload-debug")]
        crate::preload_debug!(
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
                    send_strip_inflight_release(&release_tx, index);
                    return;
                }
            };
            if !preview_aspect_matches_logical(strip.width, strip.height, width, height) {
                send_strip_inflight_release(&release_tx, index);
                return;
            }
            let job = DirectoryTreeStripPreviewJobResult {
                index,
                path,
                image_list_generation: list_generation,
                decoded: strip,
                logical: (width, height),
                stage: PreviewStage::Initial,
                buffer_tag: StripPreviewBufferTag::IsoGainMapBaseline,
            };
            if tx.try_send(job).is_ok() {
                if let Some(wake) = root_wake {
                    wake();
                }
            } else {
                send_strip_inflight_release(&release_tx, index);
            }
        });
        true
    }


    pub(crate) fn try_schedule_strip_from_hdr_image_cache(&mut self, index: usize) -> bool {
        let Some(hdr) = self.hdr_image_cache.get(&index).cloned() else {
            return false;
        };
        if !self.strip_needs_hdr_cache_sync_for_hdr(index, hdr.as_ref()) {
            return false;
        }
        let Some(list) = self.directory_tree.list.try_lock() else {
            return false;
        };
        let list_generation = list.image_list_generation;
        drop(list);

        let fallback = self.strip_fallback_for_hdr_cache_sync(index, hdr.as_ref());
        let fallback_is_deferred_placeholder = fallback.is_sdr_deferred_placeholder();
        let target_tag = crate::app::directory_tree_strip_cache::strip_buffer_tag_for_hdr_preview(
            !hdr.rgba_f32.is_empty(),
            fallback_is_deferred_placeholder,
            false,
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

        self.directory_tree_strip_generate_inflight.insert(index);
        let tx = self.directory_tree_strip_preview_tx.clone();
        let release_tx = self.directory_tree_strip_inflight_release_tx.clone();
        let path = self.image_files[index].clone();
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
                    send_strip_inflight_release(&release_tx, index);
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
                    fallback_is_deferred_placeholder,
                    decoded.is_sdr_deferred_placeholder(),
                    false,
                );
            let job = DirectoryTreeStripPreviewJobResult {
                index,
                path,
                image_list_generation: list_generation,
                decoded,
                logical,
                stage,
                buffer_tag,
            };
            if tx.try_send(job).is_ok() {
                if let Some(wake) = root_wake {
                    wake();
                }
            } else {
                send_strip_inflight_release(&release_tx, index);
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
        self.directory_tree_strip_cache.insert_from_texture_handle(
            index,
            texture,
            crate::loader::PreviewStage::Refined,
            StripPreviewBufferTag::MainWindowTiledPreview,
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
        self.directory_tree_strip_cache.insert_from_texture_handle(
            index,
            texture,
            crate::loader::PreviewStage::Refined,
            StripPreviewBufferTag::MainWindowTextureCacheSdr,
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


    pub(crate) fn try_schedule_strip_compose_upgrade(&mut self, index: usize) {
        if !self.strip_needs_compose_upgrade(index) {
            return;
        }
        let Some(hdr) = self.hdr_image_cache.get(&index).cloned() else {
            return;
        };
        let Some(list) = self.directory_tree.list.try_lock() else {
            return;
        };
        let list_generation = list.image_list_generation;
        drop(list);

        self.directory_tree_strip_generate_inflight.insert(index);
        let tx = self.directory_tree_strip_preview_tx.clone();
        let release_tx = self.directory_tree_strip_inflight_release_tx.clone();
        let path = self.image_files[index].clone();
        let max_side = self
            .settings
            .directory_tree_list_preview_size
            .strip_max_side();
        let root_wake = self.root_redraw_wake_handle();
        #[cfg(feature = "preload-debug")]
        crate::preload_debug!(
            "[PreloadDebug][Strip] pool submit idx={} kind=compose_upgrade max_side={}",
            index,
            max_side
        );
        DIRECTORY_TREE_STRIP_POOL.spawn(move || {
            let decoded = match crate::loader::directory_tree_strip_composed_from_iso_deferred(
                hdr.as_ref(),
                max_side,
            ) {
                Ok(decoded) => decoded,
                Err(err) => {
                    log::debug!(
                        "[DirectoryTree] Strip compose upgrade failed for index {index}: {err}"
                    );
                    send_strip_inflight_release(&release_tx, index);
                    return;
                }
            };
            let logical = (hdr.width, hdr.height);
            let job = DirectoryTreeStripPreviewJobResult {
                index,
                path,
                image_list_generation: list_generation,
                decoded,
                logical,
                stage: crate::loader::PreviewStage::Refined,
                buffer_tag: StripPreviewBufferTag::HdrComposedStrip,
            };
            if tx.try_send(job).is_ok() {
                if let Some(wake) = root_wake {
                    wake();
                }
            } else {
                send_strip_inflight_release(&release_tx, index);
            }
        });
    }


    pub(super) fn collect_cold_strip_thumbnail_candidates(
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
        let mut ordered = Vec::with_capacity(schedule_budget.min(MAX_COLD_STRIP_SCHEDULE_PER_FRAME));
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
            #[cfg(feature = "preload-debug")]
            {
                let use_fast_gain_map = path.extension().is_some_and(|ext| {
                    let ext = ext.to_string_lossy().to_ascii_lowercase();
                    ext == "avif" || ext == "avifs" || ext == "jxl"
                });
                crate::preload_debug!(
                    "[PreloadDebug][Strip] cold worker start idx={} path={} fast={}",
                    index,
                    path.display(),
                    use_fast_gain_map
                );
            }
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
                buffer_tag: StripPreviewBufferTag::StripDecodedPixels,
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
                buffer_tag: StripPreviewBufferTag::StripDecodedPixels,
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

}
