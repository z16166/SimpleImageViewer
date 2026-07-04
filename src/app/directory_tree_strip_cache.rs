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

use std::collections::HashMap;
use std::path::PathBuf;

use eframe::egui::{self, ColorImage, TextureOptions};

use crate::loader::{DecodedImage, PreviewStage, preview_aspect_matches_logical};

use crate::app::index_cache_permute::permute_usize_hashmap;

/// Maximum strip preview textures retained in memory (LRU eviction).
pub(crate) const DIRECTORY_TREE_STRIP_CACHE_MAX: usize = 128;

/// Provenance of pixels stored in the directory-tree strip cache.
///
/// Replacement decisions use tag quality rank, not decoded or texture dimensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum StripPreviewBufferTag {
    /// Black deferred SDR placeholder (`DecodedImage::sdr_deferred_placeholder`); never stored.
    SdrDeferredPlaceholder,
    /// Main-window `texture_cache` SDR clone (may be animated HDR placeholder or dark iso baseline).
    MainWindowTextureCacheSdr,
    /// Main-window tile-manager preview texture clone.
    MainWindowTiledPreview,
    /// Preload deferred SDR upload pixels, or embedded GPU-RAW bootstrap before float readback.
    PreloadSdrFallback,
    /// ISO gain-map deferred baseline only (`PreviewStage::Initial`).
    IsoGainMapBaseline,
    /// Strip-sized decode from cold worker, tiled source, or static SDR install path.
    StripDecodedPixels,
    /// CPU tone-mapped HDR strip (float `rgba_f32` tone-mapped on CPU).
    HdrToneMappedStrip,
}

impl StripPreviewBufferTag {
    /// Number of [`PreviewStage`] variants. Each tag occupies this many consecutive ranks
    /// so that stage upgrades within the same tag always increase the rank.
    const PREVIEW_STAGE_COUNT: u16 = 2;

    fn quality_rank(self, stage: PreviewStage) -> u16 {
        let base = match self {
            Self::SdrDeferredPlaceholder => 0,
            Self::MainWindowTextureCacheSdr => 1,
            Self::MainWindowTiledPreview => 2,
            Self::PreloadSdrFallback => 3,
            Self::IsoGainMapBaseline => 4,
            Self::StripDecodedPixels => 5,
            Self::HdrToneMappedStrip => 6,
        };
        let stage_bonus = match stage {
            PreviewStage::Initial => 0,
            PreviewStage::Refined => 1,
        };
        base * Self::PREVIEW_STAGE_COUNT + stage_bonus
    }
}

pub(crate) struct DirectoryTreeStripPreviewJobResult {
    pub index: usize,
    pub path: PathBuf,
    pub image_list_generation: u64,
    pub decoded: DecodedImage,
    /// Full static SDR decode produced while generating a cold strip thumbnail.
    /// When present, the app can reuse it as a preloaded main image instead of
    /// decoding the same file again.
    pub reusable_full_decoded: Option<DecodedImage>,
    pub logical: (u32, u32),
    pub stage: PreviewStage,
    pub buffer_tag: StripPreviewBufferTag,
    /// Cold strip found no fast preview and slow primary decode was skipped; retry after preload.
    pub cold_deferred_to_main_loader: bool,
    /// `strip_max_side()` used when the worker last sized `decoded` for strip upload.
    pub strip_max_side_used: u32,
}

/// Decoded strip thumbnail waiting for GPU upload during UI paint (not in `logic()`).
pub(crate) struct DirectoryTreeStripPendingGpuUpload {
    pub index: usize,
    pub decoded: DecodedImage,
    pub stage: PreviewStage,
    pub logical: Option<(u32, u32)>,
    pub buffer_tag: StripPreviewBufferTag,
    /// Monotonic insertion-order sequence number so that flush can merge the
    /// per-stage queues in FIFO order.
    pub seq: u64,
    /// Worker `strip_max_side` when known; `None` for sync paths (e.g. deferred SDR) that may
    /// still need background downsample before GPU upload.
    pub strip_max_side_used: Option<u32>,
}

/// Limit GPU texture uploads per paint pass (checklist #3).
pub(crate) const MAX_STRIP_GPU_UPLOADS_PER_PAINT: usize = 12;
pub(crate) const MAX_STRIP_PENDING_GPU_UPLOADS: usize = 256;

/// O(1) LRU order for strip cache indices (touch, remove, pop-oldest).
#[derive(Default)]
struct StripLruOrder {
    nodes: HashMap<usize, LruLinks>,
    head: Option<usize>,
    tail: Option<usize>,
}

#[derive(Clone, Copy)]
struct LruLinks {
    prev: Option<usize>,
    next: Option<usize>,
}

impl StripLruOrder {
    fn clear(&mut self) {
        self.nodes.clear();
        self.head = None;
        self.tail = None;
    }

    fn touch(&mut self, index: usize) {
        self.unlink(index);
        self.link_at_tail(index);
    }

    fn remove(&mut self, index: usize) {
        self.unlink(index);
    }

    fn pop_oldest(&mut self) -> Option<usize> {
        let oldest = self.head?;
        self.unlink(oldest);
        Some(oldest)
    }

    fn contains(&self, index: usize) -> bool {
        self.nodes.contains_key(&index)
    }

    fn rename(&mut self, from: usize, to: usize) {
        if from == to {
            return;
        }
        self.remove(to);
        let Some(links) = self.nodes.remove(&from) else {
            return;
        };
        if let Some(prev) = links.prev {
            self.nodes.get_mut(&prev).expect("LRU prev").next = Some(to);
        } else {
            self.head = Some(to);
        }
        if let Some(next) = links.next {
            self.nodes.get_mut(&next).expect("LRU next").prev = Some(to);
        } else {
            self.tail = Some(to);
        }
        self.nodes.insert(to, links);
    }

    fn retain(&mut self, mut keep: impl FnMut(usize) -> bool) {
        let ordered = self.ordered_indices();
        self.clear();
        for index in ordered {
            if keep(index) {
                self.link_at_tail(index);
            }
        }
    }

    fn partial_remap(&mut self, old_to_new: &[usize]) {
        let ordered = self.ordered_indices();
        self.clear();
        for index in ordered {
            if index < old_to_new.len() {
                let new_idx = old_to_new[index];
                if new_idx != usize::MAX {
                    self.link_at_tail(new_idx);
                }
            }
        }
    }

    fn permute(&mut self, old_to_new: &[usize]) {
        let ordered = self.ordered_indices();
        self.clear();
        for index in ordered {
            if index < old_to_new.len() {
                self.link_at_tail(old_to_new[index]);
            }
        }
    }

    fn ordered_indices(&self) -> Vec<usize> {
        let mut out = Vec::with_capacity(self.nodes.len());
        let mut cur = self.head;
        while let Some(index) = cur {
            out.push(index);
            cur = self.nodes.get(&index).and_then(|links| links.next);
        }
        out
    }

    fn unlink(&mut self, index: usize) {
        let Some(links) = self.nodes.remove(&index) else {
            return;
        };
        match (links.prev, links.next) {
            (None, None) => {
                self.head = None;
                self.tail = None;
            }
            (None, Some(next)) => {
                self.head = Some(next);
                self.nodes.get_mut(&next).expect("LRU head next").prev = None;
            }
            (Some(prev), None) => {
                self.tail = Some(prev);
                self.nodes.get_mut(&prev).expect("LRU tail prev").next = None;
            }
            (Some(prev), Some(next)) => {
                self.nodes.get_mut(&prev).expect("LRU prev").next = Some(next);
                self.nodes.get_mut(&next).expect("LRU next").prev = Some(prev);
            }
        }
    }

    fn link_at_tail(&mut self, index: usize) {
        let links = LruLinks {
            prev: self.tail,
            next: None,
        };
        if let Some(tail) = self.tail {
            self.nodes.get_mut(&tail).expect("LRU tail").next = Some(index);
        } else {
            self.head = Some(index);
        }
        self.tail = Some(index);
        self.nodes.insert(index, links);
    }
}

#[derive(Default)]
pub(crate) struct DirectoryTreeStripCache {
    textures: HashMap<usize, egui::TextureHandle>,
    preview_buffer_tag: HashMap<usize, StripPreviewBufferTag>,
    preview_stage: HashMap<usize, PreviewStage>,
    logical_sizes: HashMap<usize, (u32, u32)>,
    lru_order: StripLruOrder,
    gpu_revision: u64,
}

pub(crate) struct StripDecodedUpsert<'a> {
    pub(crate) stage: PreviewStage,
    pub(crate) buffer_tag: StripPreviewBufferTag,
    pub(crate) logical_size: Option<(u32, u32)>,
    pub(crate) path: &'a std::path::Path,
    pub(crate) ctx: &'a egui::Context,
    pub(crate) strip_max_side: u32,
    pub(crate) strip_max_side_used: Option<u32>,
}

/// Whether `decoded` already fits the current strip setting and can upload without paint-thread
/// downsample (checklist #3).
pub(crate) fn strip_decoded_ready_for_gpu_upload(
    decoded: &DecodedImage,
    strip_max_side: u32,
    strip_max_side_used: Option<u32>,
) -> bool {
    if strip_max_side_used == Some(strip_max_side) {
        return true;
    }
    decoded.width.max(decoded.height) <= strip_max_side
}

impl DirectoryTreeStripCache {
    pub(crate) fn contains(&self, index: usize) -> bool {
        self.textures.contains_key(&index)
    }

    fn touch_lru(&mut self, index: usize) {
        self.lru_order.touch(index);
    }

    pub(crate) fn remove_index(&mut self, index: usize) {
        self.textures.remove(&index);
        self.preview_buffer_tag.remove(&index);
        self.preview_stage.remove(&index);
        self.logical_sizes.remove(&index);
        self.lru_order.remove(index);
    }

    pub(crate) fn textures(&self) -> &HashMap<usize, egui::TextureHandle> {
        &self.textures
    }

    pub(crate) fn logical_sizes(&self) -> &HashMap<usize, (u32, u32)> {
        &self.logical_sizes
    }

    pub(crate) fn gpu_revision(&self) -> u64 {
        self.gpu_revision
    }

    fn bump_gpu_revision(&mut self) {
        self.gpu_revision = self.gpu_revision.wrapping_add(1);
    }

    pub(crate) fn preview_dimensions(&self, index: usize) -> Option<(u32, u32)> {
        let handle = self.textures.get(&index)?;
        let size = handle.size();
        Some((size[0] as u32, size[1] as u32))
    }

    pub(crate) fn is_valid_for_logical(&self, index: usize, logical: (u32, u32)) -> bool {
        let Some((preview_w, preview_h)) = self.preview_dimensions(index) else {
            return false;
        };
        preview_aspect_matches_logical(preview_w, preview_h, logical.0, logical.1)
    }

    pub(crate) fn invalidate_if_invalid(&mut self, index: usize, logical: (u32, u32)) -> bool {
        if self.contains(index) && !self.is_valid_for_logical(index, logical) {
            #[cfg(feature = "preload-debug")]
            if let Some((preview_w, preview_h)) = self.preview_dimensions(index) {
                crate::preload_debug!(
                    "[PreloadDebug][StripCache] invalidate idx={} preview={}x{} logical={}x{}",
                    index,
                    preview_w,
                    preview_h,
                    logical.0,
                    logical.1
                );
            }
            self.remove_index(index);
            return true;
        }
        false
    }

    pub(crate) fn cached_buffer_tag(&self, index: usize) -> Option<StripPreviewBufferTag> {
        self.preview_buffer_tag.get(&index).copied()
    }

    pub(crate) fn cached_preview_stage(&self, index: usize) -> Option<PreviewStage> {
        self.preview_stage.get(&index).copied()
    }

    /// Write a strip texture into the cache after the caller has already
    /// acquired an [`egui::TextureHandle`] (by clone or by GPU upload).
    ///
    /// All call sites that update a strip thumbnail in memory flow through
    /// this single function, so `preload-debug` logging is centralized here.
    fn commit_strip_texture(
        &mut self,
        index: usize,
        texture: egui::TextureHandle,
        buffer_tag: StripPreviewBufferTag,
        stage: PreviewStage,
        logical_size: Option<(u32, u32)>,
        path: &std::path::Path,
    ) {
        let _ = path; // used by preload-debug logging below
        #[cfg(feature = "preload-debug")]
        let tex_size = texture.size();
        #[cfg(feature = "preload-debug")]
        let tex_w = tex_size[0];
        #[cfg(feature = "preload-debug")]
        let tex_h = tex_size[1];
        #[cfg(feature = "preload-debug")]
        let count_before = self.textures.len();

        if let Some(logical) = logical_size {
            self.logical_sizes.insert(index, logical);
        }
        self.textures.insert(index, texture);
        self.preview_buffer_tag.insert(index, buffer_tag);
        self.preview_stage.insert(index, stage);
        self.touch_lru(index);
        self.bump_gpu_revision();
        self.evict_if_needed();

        #[cfg(feature = "preload-debug")]
        crate::preload_debug!(
            "[PreloadDebug][StripCache] commit idx={} path={} tag={buffer_tag:?} stage={stage:?} \
             tex={tex_w}x{tex_h} logical={logical_size:?} \
             cache_count_before={count_before} cache_count_after={} rev={}",
            index,
            path.display(),
            self.textures.len(),
            self.gpu_revision
        );
    }

    fn commit_existing_strip_texture_update(
        &mut self,
        index: usize,
        buffer_tag: StripPreviewBufferTag,
        stage: PreviewStage,
        logical_size: Option<(u32, u32)>,
        path: &std::path::Path,
    ) {
        let _ = path; // used by preload-debug logging below
        if let Some(logical) = logical_size {
            self.logical_sizes.insert(index, logical);
        }
        self.preview_buffer_tag.insert(index, buffer_tag);
        self.preview_stage.insert(index, stage);
        self.touch_lru(index);
        self.bump_gpu_revision();

        #[cfg(feature = "preload-debug")]
        {
            let tex_size = self
                .textures
                .get(&index)
                .map(|texture| texture.size())
                .unwrap_or([0, 0]);
            crate::preload_debug!(
                "[PreloadDebug][StripCache] update-existing idx={} path={} tag={buffer_tag:?} \
                 stage={stage:?} tex={}x{} logical={logical_size:?} rev={}",
                index,
                path.display(),
                tex_size[0],
                tex_size[1],
                self.gpu_revision
            );
        }
    }

    /// Insert a strip texture from the main-window texture cache.
    /// Whether a main-window texture clone would upgrade the strip entry (no logging).
    pub(crate) fn strip_texture_handle_would_replace(
        &self,
        index: usize,
        stage: PreviewStage,
        buffer_tag: StripPreviewBufferTag,
        logical: Option<(u32, u32)>,
        preview_w: u32,
        preview_h: u32,
    ) -> bool {
        let cached_dims = self.preview_dimensions(index);
        evaluate_strip_preview_replace(&StripPreviewReplaceParams {
            index,
            source: "strip_texture_handle_probe",
            cached_tag: self.preview_buffer_tag.get(&index).copied(),
            cached_stage: self.preview_stage.get(&index).copied(),
            cached_logical: self.logical_sizes.get(&index).copied(),
            cached_preview_w: cached_dims.map(|(w, _)| w),
            cached_preview_h: cached_dims.map(|(_, h)| h),
            incoming_tag: buffer_tag,
            incoming_stage: stage,
            incoming_logical: logical,
            preview_w,
            preview_h,
            decoded: None,
        })
        .allows_replace()
    }

    /// Takes `&TextureHandle` to avoid cloning when the strip cache already
    /// holds an equal-or-better entry for this index. The clone only happens
    /// after [`decide_strip_preview_replace`] confirms the replacement.
    pub(crate) fn insert_from_texture_handle(
        &mut self,
        index: usize,
        texture: &egui::TextureHandle,
        stage: PreviewStage,
        buffer_tag: StripPreviewBufferTag,
        logical: Option<(u32, u32)>,
        path: &std::path::Path,
    ) -> bool {
        let size = texture.size();
        let preview_w = size[0] as u32;
        let preview_h = size[1] as u32;
        let cached_tag = self.preview_buffer_tag.get(&index).copied();
        let cached_stage = self.preview_stage.get(&index).copied();
        let cached_dims = self.preview_dimensions(index);
        if !decide_strip_preview_replace(&StripPreviewReplaceParams {
            index,
            source: "insert_from_texture_handle",
            cached_tag,
            cached_stage,
            cached_logical: self.logical_sizes.get(&index).copied(),
            cached_preview_w: cached_dims.map(|(w, _)| w),
            cached_preview_h: cached_dims.map(|(_, h)| h),
            incoming_tag: buffer_tag,
            incoming_stage: stage,
            incoming_logical: logical,
            preview_w,
            preview_h,
            decoded: None,
        }) {
            return false;
        }
        self.commit_strip_texture(index, texture.clone(), buffer_tag, stage, logical, path);
        true
    }

    pub(crate) fn upsert_from_decoded(
        &mut self,
        index: usize,
        decoded: &DecodedImage,
        upsert: StripDecodedUpsert<'_>,
    ) {
        let StripDecodedUpsert {
            stage,
            buffer_tag,
            logical_size,
            path,
            ctx,
            strip_max_side,
            strip_max_side_used,
        } = upsert;
        debug_assert!(
            strip_decoded_ready_for_gpu_upload(decoded, strip_max_side, strip_max_side_used),
            "upsert_from_decoded requires strip-sized pixels; schedule background resample first"
        );
        let cached_tag = self.preview_buffer_tag.get(&index).copied();
        let cached_stage = self.preview_stage.get(&index).copied();
        let cached_dims = self.preview_dimensions(index);
        if !decide_strip_preview_replace(&StripPreviewReplaceParams {
            index,
            source: "upsert_from_decoded",
            cached_tag,
            cached_stage,
            cached_logical: self.logical_sizes.get(&index).copied(),
            cached_preview_w: cached_dims.map(|(w, _)| w),
            cached_preview_h: cached_dims.map(|(_, h)| h),
            incoming_tag: buffer_tag,
            incoming_stage: stage,
            incoming_logical: logical_size,
            preview_w: decoded.width,
            preview_h: decoded.height,
            decoded: Some(decoded),
        }) {
            return;
        }
        let color_image = ColorImage::from_rgba_unmultiplied(
            [decoded.width as usize, decoded.height as usize],
            decoded.rgba(),
        );
        let thumb_size = [decoded.width as usize, decoded.height as usize];
        if self
            .textures
            .get(&index)
            .is_some_and(|handle| handle.size() == thumb_size)
        {
            if let Some(handle) = self.textures.get_mut(&index) {
                handle.set(color_image, TextureOptions::LINEAR);
            }
            self.commit_existing_strip_texture_update(index, buffer_tag, stage, logical_size, path);
            return;
        }
        let handle = ctx.load_texture(
            format!("dir_tree_strip_{index}"),
            color_image,
            TextureOptions::LINEAR,
        );
        self.commit_strip_texture(index, handle, buffer_tag, stage, logical_size, path);
    }

    pub(crate) fn relocate(&mut self, from: usize, to: usize) {
        if from == to {
            return;
        }
        debug_assert!(
            !self.textures.contains_key(&to),
            "relocate: target {to} already in cache"
        );
        if let Some(tex) = self.textures.remove(&from) {
            self.textures.insert(to, tex);
        }
        if let Some(tag) = self.preview_buffer_tag.remove(&from) {
            self.preview_buffer_tag.insert(to, tag);
        }
        if let Some(stage) = self.preview_stage.remove(&from) {
            self.preview_stage.insert(to, stage);
        }
        if let Some(logical) = self.logical_sizes.remove(&from) {
            self.logical_sizes.insert(to, logical);
        }
        if self.lru_order.contains(from) {
            self.lru_order.rename(from, to);
        } else if self.textures.contains_key(&to) {
            self.touch_lru(to);
        } else {
            self.lru_order.remove(to);
        }
    }

    #[allow(dead_code)]
    pub(crate) fn partial_remap(&mut self, old_to_new: &[usize]) {
        remap_partial_hashmap(&mut self.textures, old_to_new);
        remap_partial_hashmap(&mut self.preview_buffer_tag, old_to_new);
        remap_partial_hashmap(&mut self.preview_stage, old_to_new);
        remap_partial_hashmap(&mut self.logical_sizes, old_to_new);
        self.lru_order.partial_remap(old_to_new);
    }

    pub(crate) fn permute(&mut self, old_to_new: &[usize]) {
        permute_usize_hashmap(&mut self.textures, old_to_new);
        permute_usize_hashmap(&mut self.preview_buffer_tag, old_to_new);
        permute_usize_hashmap(&mut self.preview_stage, old_to_new);
        permute_usize_hashmap(&mut self.logical_sizes, old_to_new);
        self.lru_order.permute(old_to_new);
    }

    pub(crate) fn retain(&mut self, mut keep: impl FnMut(usize) -> bool) {
        self.textures.retain(|index, _| keep(*index));
        self.preview_buffer_tag.retain(|index, _| keep(*index));
        self.preview_stage.retain(|index, _| keep(*index));
        self.logical_sizes.retain(|index, _| keep(*index));
        self.lru_order.retain(&mut keep);
    }

    /// Drop GPU-backed egui textures after a wgpu surface format hot-swap. CPU-side
    /// logical sizes are kept so regeneration can validate aspect ratio.
    pub(crate) fn clear_gpu_textures(&mut self) {
        self.textures.clear();
        self.preview_buffer_tag.clear();
        self.preview_stage.clear();
        self.lru_order.clear();
        self.bump_gpu_revision();
    }

    pub(crate) fn clear_all(&mut self) {
        self.textures.clear();
        self.preview_buffer_tag.clear();
        self.preview_stage.clear();
        self.logical_sizes.clear();
        self.lru_order.clear();
        self.bump_gpu_revision();
    }

    fn evict_if_needed(&mut self) {
        let mut evicted = false;
        while self.textures.len() > DIRECTORY_TREE_STRIP_CACHE_MAX {
            let Some(idx) = self.lru_order.pop_oldest() else {
                break;
            };
            if self.textures.contains_key(&idx) {
                #[cfg(feature = "preload-debug")]
                crate::preload_debug!(
                    "[PreloadDebug][StripCache] lru evict idx={} cache_count={}",
                    idx,
                    self.textures.len().saturating_sub(1)
                );
                self.textures.remove(&idx);
                self.preview_buffer_tag.remove(&idx);
                self.preview_stage.remove(&idx);
                self.logical_sizes.remove(&idx);
                evicted = true;
            }
        }
        if evicted {
            self.bump_gpu_revision();
        }
    }
}

#[allow(dead_code)]
fn remap_partial_hashmap<T>(map: &mut HashMap<usize, T>, old_to_new: &[usize]) {
    let taken = std::mem::take(map);
    for (old_idx, value) in taken {
        if old_idx < old_to_new.len() {
            let new_idx = old_to_new[old_idx];
            if new_idx != usize::MAX {
                map.insert(new_idx, value);
            }
        }
    }
}

pub(crate) fn decoded_rgba_size_valid(decoded: &DecodedImage) -> bool {
    decoded.rgba().len() == decoded.width as usize * decoded.height as usize * 4
}

pub(crate) fn strip_preview_quality_rank(tag: StripPreviewBufferTag, stage: PreviewStage) -> u16 {
    tag.quality_rank(stage)
}

/// Buffer tag for HDR-related strip previews.
///
/// Tag reflects the strip buffer itself, not the main-window SDR fallback. When the strip
/// decoded image is a deferred/black placeholder, use [`StripPreviewBufferTag::SdrDeferredPlaceholder`]
/// (lowest rank) rather than [`StripPreviewBufferTag::HdrToneMappedStrip`], so replace logic keeps
/// any existing real preview.
pub(crate) fn strip_buffer_tag_for_hdr_preview(
    hdr_has_float_pixels: bool,
    decoded_is_deferred_placeholder: bool,
) -> StripPreviewBufferTag {
    if decoded_is_deferred_placeholder {
        return StripPreviewBufferTag::SdrDeferredPlaceholder;
    }
    if hdr_has_float_pixels {
        return StripPreviewBufferTag::HdrToneMappedStrip;
    }
    // Bootstrap or other real SDR fallback before float HDR readback — not tone-mapped yet.
    StripPreviewBufferTag::PreloadSdrFallback
}

/// Inputs for a single strip-preview replace decision (decoded or texture path).
#[allow(dead_code)] // several fields are read only by preload-debug logging
pub(crate) struct StripPreviewReplaceParams<'a> {
    pub index: usize,
    pub source: &'static str,
    pub cached_tag: Option<StripPreviewBufferTag>,
    pub cached_stage: Option<PreviewStage>,
    pub cached_logical: Option<(u32, u32)>,
    pub cached_preview_w: Option<u32>,
    pub cached_preview_h: Option<u32>,
    pub incoming_tag: StripPreviewBufferTag,
    pub incoming_stage: PreviewStage,
    pub incoming_logical: Option<(u32, u32)>,
    pub preview_w: u32,
    pub preview_h: u32,
    pub decoded: Option<&'a DecodedImage>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StripPreviewReplaceOutcome {
    AllowEmptySlot,
    AllowHigherRank {
        cached_rank: u16,
        incoming_rank: u16,
    },
    RejectDeferredPlaceholder,
    RejectBlackPlaceholderTag,
    RejectInvalidRgba,
    RejectAspectMismatch,
    RejectRankNotHigher {
        cached_rank: u16,
        incoming_rank: u16,
    },
}

impl StripPreviewReplaceOutcome {
    fn allows_replace(self) -> bool {
        matches!(self, Self::AllowEmptySlot | Self::AllowHigherRank { .. })
    }

    #[cfg(feature = "preload-debug")]
    fn reason_label(self) -> &'static str {
        match self {
            Self::AllowEmptySlot => "empty_slot",
            Self::AllowHigherRank { .. } => "higher_rank",
            Self::RejectDeferredPlaceholder => "sdr_deferred_placeholder",
            Self::RejectBlackPlaceholderTag => "black_placeholder_tag",
            Self::RejectInvalidRgba => "invalid_rgba",
            Self::RejectAspectMismatch => "aspect_mismatch",
            Self::RejectRankNotHigher { .. } => "rank_not_higher",
        }
    }
}

fn evaluate_strip_preview_replace(
    params: &StripPreviewReplaceParams<'_>,
) -> StripPreviewReplaceOutcome {
    if params.incoming_tag == StripPreviewBufferTag::SdrDeferredPlaceholder {
        return StripPreviewReplaceOutcome::RejectBlackPlaceholderTag;
    }
    if let Some(decoded) = params.decoded {
        if decoded.is_sdr_deferred_placeholder() {
            return StripPreviewReplaceOutcome::RejectDeferredPlaceholder;
        }
        if !decoded_rgba_size_valid(decoded) {
            return StripPreviewReplaceOutcome::RejectInvalidRgba;
        }
    }
    if let Some((logical_w, logical_h)) = params.incoming_logical
        && !preview_aspect_matches_logical(params.preview_w, params.preview_h, logical_w, logical_h)
    {
        return StripPreviewReplaceOutcome::RejectAspectMismatch;
    }
    match params.cached_tag {
        None => StripPreviewReplaceOutcome::AllowEmptySlot,
        Some(cached) => {
            let incoming_rank =
                strip_preview_quality_rank(params.incoming_tag, params.incoming_stage);
            let cached_rank = strip_preview_quality_rank(
                cached,
                params.cached_stage.unwrap_or(PreviewStage::Initial),
            );
            if incoming_rank > cached_rank {
                StripPreviewReplaceOutcome::AllowHigherRank {
                    cached_rank,
                    incoming_rank,
                }
            } else {
                StripPreviewReplaceOutcome::RejectRankNotHigher {
                    cached_rank,
                    incoming_rank,
                }
            }
        }
    }
}

#[cfg(feature = "preload-debug")]
fn strip_preview_rgba_debug_hint(decoded: &DecodedImage) -> String {
    let rgba = decoded.rgba();
    let sample_bytes = rgba.len().min(4096);
    let mut max_luma = 0_u8;
    let mut nonzero = 0_usize;
    for px in rgba[..sample_bytes].chunks_exact(4) {
        let luma = px[0].max(px[1]).max(px[2]);
        max_luma = max_luma.max(luma);
        if luma > 0 {
            nonzero += 1;
        }
    }
    format!(
        "placeholder={} max_luma={max_luma} nonzero_sample_px={nonzero}",
        decoded.is_sdr_deferred_placeholder()
    )
}

#[cfg(feature = "preload-debug")]
fn log_strip_preview_replace_decision(
    params: &StripPreviewReplaceParams<'_>,
    outcome: StripPreviewReplaceOutcome,
) {
    let decision = if outcome.allows_replace() {
        "allow"
    } else {
        "reject"
    };
    let incoming_rank = strip_preview_quality_rank(params.incoming_tag, params.incoming_stage);
    let cached_rank = params.cached_tag.map(|tag| {
        strip_preview_quality_rank(tag, params.cached_stage.unwrap_or(PreviewStage::Initial))
    });
    let aspect_ok = params.incoming_logical.is_none_or(|(lw, lh)| {
        preview_aspect_matches_logical(params.preview_w, params.preview_h, lw, lh)
    });
    let cached_aspect_ok = match (
        params.cached_preview_w,
        params.cached_preview_h,
        params.cached_logical,
    ) {
        (Some(cw), Some(ch), Some((lw, lh))) => preview_aspect_matches_logical(cw, ch, lw, lh),
        _ => false,
    };
    let pixel_hint = params
        .decoded
        .map(strip_preview_rgba_debug_hint)
        .unwrap_or_else(|| "n/a".to_string());
    crate::preload_debug!(
        "[PreloadDebug][StripReplace] idx={} source={} decision={decision} reason={} \
         cached_tag={:?} cached_stage={:?} cached_rank={cached_rank:?} \
         cached_tex={}x{} cached_logical={:?} cached_aspect_ok={cached_aspect_ok} \
         incoming_tag={:?} incoming_stage={:?} incoming_rank={incoming_rank} \
         incoming_tex={}x{} incoming_logical={:?} aspect_ok={aspect_ok} pixel_hint={pixel_hint}",
        params.index,
        params.source,
        outcome.reason_label(),
        params.cached_tag,
        params.cached_stage,
        params.cached_preview_w.unwrap_or(0),
        params.cached_preview_h.unwrap_or(0),
        params.cached_logical,
        params.incoming_tag,
        params.incoming_stage,
        params.preview_w,
        params.preview_h,
        params.incoming_logical,
    );
}

/// Single gate for strip-preview replacement; logs the full decision under `preload-debug`.
pub(crate) fn decide_strip_preview_replace(params: &StripPreviewReplaceParams<'_>) -> bool {
    let outcome = evaluate_strip_preview_replace(params);
    #[cfg(feature = "preload-debug")]
    log_strip_preview_replace_decision(params, outcome);
    outcome.allows_replace()
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn should_replace_strip_preview(
    cached_tag: Option<StripPreviewBufferTag>,
    cached_stage: Option<PreviewStage>,
    decoded: &DecodedImage,
    incoming_tag: StripPreviewBufferTag,
    stage: PreviewStage,
    logical_size: Option<(u32, u32)>,
) -> bool {
    evaluate_strip_preview_replace(&StripPreviewReplaceParams {
        index: usize::MAX,
        source: "should_replace_strip_preview_test",
        cached_tag,
        cached_stage,
        cached_logical: None,
        cached_preview_w: None,
        cached_preview_h: None,
        incoming_tag,
        incoming_stage: stage,
        incoming_logical: logical_size,
        preview_w: decoded.width,
        preview_h: decoded.height,
        decoded: Some(decoded),
    })
    .allows_replace()
}

#[allow(dead_code)]
pub(crate) fn should_replace_strip_texture(
    cached_tag: Option<StripPreviewBufferTag>,
    cached_stage: Option<PreviewStage>,
    incoming_tag: StripPreviewBufferTag,
    stage: PreviewStage,
    logical_size: Option<(u32, u32)>,
    preview_w: u32,
    preview_h: u32,
) -> bool {
    evaluate_strip_preview_replace(&StripPreviewReplaceParams {
        index: usize::MAX,
        source: "should_replace_strip_texture_test",
        cached_tag,
        cached_stage,
        cached_logical: None,
        cached_preview_w: None,
        cached_preview_h: None,
        incoming_tag,
        incoming_stage: stage,
        incoming_logical: logical_size,
        preview_w,
        preview_h,
        decoded: None,
    })
    .allows_replace()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::downsample_decoded_for_strip;
    use std::path::Path;

    #[test]
    fn strip_decoded_ready_for_gpu_upload_matches_worker_max_side() {
        let decoded = DecodedImage::new(256, 128, vec![0; 256 * 128 * 4]);
        assert!(strip_decoded_ready_for_gpu_upload(&decoded, 256, Some(256)));
        assert!(!strip_decoded_ready_for_gpu_upload(
            &decoded,
            128,
            Some(256)
        ));
    }

    #[test]
    fn strip_decoded_ready_for_gpu_upload_accepts_pixels_that_fit_current_setting() {
        let small = DecodedImage::new(64, 48, vec![0; 64 * 48 * 4]);
        assert!(strip_decoded_ready_for_gpu_upload(&small, 128, None));
        assert!(strip_decoded_ready_for_gpu_upload(&small, 256, Some(128)));
        let large = DecodedImage::new(256, 128, vec![0; 256 * 128 * 4]);
        assert!(!strip_decoded_ready_for_gpu_upload(&large, 128, None));
    }

    #[test]
    fn downsample_decoded_for_strip_keeps_small_images_as_is() {
        let decoded = DecodedImage::new(64, 48, vec![0; 64 * 48 * 4]);
        let out = downsample_decoded_for_strip(&decoded, 128).expect("downsample");
        assert_eq!(out.width, 64);
        assert_eq!(out.height, 48);
    }

    #[test]
    fn downsample_decoded_for_strip_scales_large_images() {
        let decoded = DecodedImage::new(512, 256, vec![0; 512 * 256 * 4]);
        let out = downsample_decoded_for_strip(&decoded, 128).expect("downsample");
        assert_eq!(out.width, 128);
        assert_eq!(out.height, 64);
    }

    #[test]
    fn strip_buffer_tag_marks_decoded_placeholder_as_deferred() {
        assert_eq!(
            strip_buffer_tag_for_hdr_preview(false, true),
            StripPreviewBufferTag::SdrDeferredPlaceholder
        );
    }

    #[test]
    fn strip_buffer_tag_uses_preload_fallback_for_real_sdr_strip_before_float_readback() {
        assert_eq!(
            strip_buffer_tag_for_hdr_preview(false, false),
            StripPreviewBufferTag::PreloadSdrFallback
        );
    }

    #[test]
    fn strip_buffer_tag_uses_hdr_tone_mapped_when_float_pixels_ready() {
        assert_eq!(
            strip_buffer_tag_for_hdr_preview(true, false),
            StripPreviewBufferTag::HdrToneMappedStrip
        );
    }

    #[test]
    fn deferred_placeholder_rank_loses_to_main_window_tiled_preview() {
        let cached_rank = strip_preview_quality_rank(
            StripPreviewBufferTag::MainWindowTiledPreview,
            PreviewStage::Refined,
        );
        let placeholder_rank = strip_preview_quality_rank(
            StripPreviewBufferTag::SdrDeferredPlaceholder,
            PreviewStage::Refined,
        );
        assert!(placeholder_rank < cached_rank);
    }

    #[test]
    fn strip_cache_touch_preserves_lru_order_for_eviction() {
        let ctx = egui::Context::default();
        let mut cache = DirectoryTreeStripCache::default();
        for index in 0..DIRECTORY_TREE_STRIP_CACHE_MAX {
            let decoded = DecodedImage::new(16, 16, vec![index as u8; 16 * 16 * 4]);
            cache.upsert_from_decoded(
                index,
                &decoded,
                StripDecodedUpsert {
                    stage: PreviewStage::Initial,
                    buffer_tag: StripPreviewBufferTag::StripDecodedPixels,
                    logical_size: None,
                    path: Path::new("/test/strip.jpg"),
                    ctx: &ctx,
                    strip_max_side: 128,
                    strip_max_side_used: Some(128),
                },
            );
        }
        // Upgrade index 0 (oldest) to Refined so it is touched to MRU.
        let touch = DecodedImage::new(16, 16, vec![255; 16 * 16 * 4]);
        cache.upsert_from_decoded(
            0,
            &touch,
            StripDecodedUpsert {
                stage: PreviewStage::Refined,
                buffer_tag: StripPreviewBufferTag::StripDecodedPixels,
                logical_size: None,
                path: Path::new("/test/strip.jpg"),
                ctx: &ctx,
                strip_max_side: 128,
                strip_max_side_used: Some(128),
            },
        );
        let overflow = DecodedImage::new(16, 16, vec![128; 16 * 16 * 4]);
        cache.upsert_from_decoded(
            DIRECTORY_TREE_STRIP_CACHE_MAX,
            &overflow,
            StripDecodedUpsert {
                stage: PreviewStage::Initial,
                buffer_tag: StripPreviewBufferTag::StripDecodedPixels,
                logical_size: None,
                path: Path::new("/test/strip.jpg"),
                ctx: &ctx,
                strip_max_side: 128,
                strip_max_side_used: Some(128),
            },
        );
        assert!(cache.contains(0));
        assert!(!cache.contains(1));
    }

    #[test]
    fn strip_cache_evicts_oldest_lru_first() {
        let ctx = egui::Context::default();
        let mut cache = DirectoryTreeStripCache::default();
        let total = DIRECTORY_TREE_STRIP_CACHE_MAX + 5;
        for index in 0..total {
            let decoded = DecodedImage::new(32, 32, vec![255; 32 * 32 * 4]);
            cache.upsert_from_decoded(
                index,
                &decoded,
                StripDecodedUpsert {
                    stage: PreviewStage::Refined,
                    buffer_tag: StripPreviewBufferTag::StripDecodedPixels,
                    logical_size: None,
                    path: Path::new("/test/strip.jpg"),
                    ctx: &ctx,
                    strip_max_side: 128,
                    strip_max_side_used: Some(128),
                },
            );
        }
        assert_eq!(cache.textures().len(), DIRECTORY_TREE_STRIP_CACHE_MAX);
        assert!(!cache.contains(0));
        assert!(cache.contains(total - 1));
    }

    #[test]
    fn strip_cache_rejects_preview_with_mismatched_aspect() {
        let bootstrap = DecodedImage::new(512, 512, vec![0; 512 * 512 * 4]);
        assert!(!should_replace_strip_preview(
            None,
            None,
            &bootstrap,
            StripPreviewBufferTag::StripDecodedPixels,
            PreviewStage::Initial,
            Some((8000, 2000)),
        ));
        assert!(!should_replace_strip_preview(
            None,
            None,
            &bootstrap,
            StripPreviewBufferTag::HdrToneMappedStrip,
            PreviewStage::Refined,
            Some((800, 8000)),
        ));
    }

    #[test]
    fn strip_cache_upgrades_hdr_tone_mapped_over_texture_cache_sdr() {
        let tone_mapped = DecodedImage::new(128, 72, vec![180; 128 * 72 * 4]);
        assert!(should_replace_strip_preview(
            Some(StripPreviewBufferTag::MainWindowTextureCacheSdr),
            Some(PreviewStage::Refined),
            &tone_mapped,
            StripPreviewBufferTag::HdrToneMappedStrip,
            PreviewStage::Refined,
            Some((480, 270)),
        ));
    }

    #[test]
    fn strip_cache_black_upsert_preserves_existing_thumbnail() {
        let ctx = egui::Context::default();
        let mut cache = DirectoryTreeStripCache::default();
        let good = DecodedImage::new(128, 64, vec![200; 128 * 64 * 4]);
        cache.upsert_from_decoded(
            0,
            &good,
            StripDecodedUpsert {
                stage: PreviewStage::Initial,
                buffer_tag: StripPreviewBufferTag::StripDecodedPixels,
                logical_size: Some((512, 256)),
                path: Path::new("/test/strip.jpg"),
                ctx: &ctx,
                strip_max_side: 128,
                strip_max_side_used: Some(128),
            },
        );
        assert!(cache.contains(0));
        let black = DecodedImage::new_sdr_deferred_placeholder(512, 256, vec![0; 512 * 256 * 4]);
        cache.upsert_from_decoded(
            0,
            &black,
            StripDecodedUpsert {
                stage: PreviewStage::Refined,
                buffer_tag: StripPreviewBufferTag::SdrDeferredPlaceholder,
                logical_size: Some((512, 256)),
                path: Path::new("/test/strip.jpg"),
                ctx: &ctx,
                strip_max_side: 128,
                strip_max_side_used: Some(128),
            },
        );
        assert!(cache.contains(0));
        assert_eq!(cache.preview_dimensions(0), Some((128, 64)));
    }

    #[test]
    fn strip_cache_reuses_texture_for_same_size_quality_upgrade() {
        let ctx = egui::Context::default();
        let mut cache = DirectoryTreeStripCache::default();
        let initial = DecodedImage::new(64, 64, vec![120; 64 * 64 * 4]);
        cache.upsert_from_decoded(
            0,
            &initial,
            StripDecodedUpsert {
                stage: PreviewStage::Initial,
                buffer_tag: StripPreviewBufferTag::PreloadSdrFallback,
                logical_size: Some((64, 64)),
                path: Path::new("/test/strip.jpg"),
                ctx: &ctx,
                strip_max_side: 128,
                strip_max_side_used: Some(128),
            },
        );
        let first_id = cache.textures().get(&0).expect("initial texture").id();

        let refined = DecodedImage::new(64, 64, vec![220; 64 * 64 * 4]);
        cache.upsert_from_decoded(
            0,
            &refined,
            StripDecodedUpsert {
                stage: PreviewStage::Refined,
                buffer_tag: StripPreviewBufferTag::StripDecodedPixels,
                logical_size: Some((64, 64)),
                path: Path::new("/test/strip.jpg"),
                ctx: &ctx,
                strip_max_side: 128,
                strip_max_side_used: Some(128),
            },
        );

        let second_id = cache.textures().get(&0).expect("refined texture").id();
        assert_eq!(first_id, second_id);
    }

    #[test]
    fn strip_cache_rejects_black_refined_over_initial_preview() {
        let good = DecodedImage::new(128, 64, vec![200; 128 * 64 * 4]);
        let black =
            DecodedImage::new_sdr_deferred_placeholder(4096, 2048, vec![0; 4096 * 2048 * 4]);
        assert!(!should_replace_strip_preview(
            Some(StripPreviewBufferTag::StripDecodedPixels),
            Some(PreviewStage::Initial),
            &black,
            StripPreviewBufferTag::SdrDeferredPlaceholder,
            PreviewStage::Refined,
            Some((4096, 2048)),
        ));
        assert!(should_replace_strip_preview(
            Some(StripPreviewBufferTag::StripDecodedPixels),
            Some(PreviewStage::Initial),
            &good,
            StripPreviewBufferTag::HdrToneMappedStrip,
            PreviewStage::Refined,
            Some((4096, 2048)),
        ));
    }

    #[test]
    fn strip_cache_cold_initial_does_not_replace_refined() {
        let logical = Some((512_u32, 512_u32));
        let cold = DecodedImage::new(128, 128, vec![200; 128 * 128 * 4]);
        assert!(!should_replace_strip_preview(
            Some(StripPreviewBufferTag::StripDecodedPixels),
            Some(PreviewStage::Refined),
            &cold,
            StripPreviewBufferTag::StripDecodedPixels,
            PreviewStage::Initial,
            logical,
        ));
        let black_placeholder =
            DecodedImage::new_sdr_deferred_placeholder(150, 150, vec![0; 150 * 150 * 4]);
        assert!(!should_replace_strip_preview(
            Some(StripPreviewBufferTag::HdrToneMappedStrip),
            Some(PreviewStage::Refined),
            &black_placeholder,
            StripPreviewBufferTag::SdrDeferredPlaceholder,
            PreviewStage::Initial,
            logical,
        ));
        let refined = DecodedImage::new(128, 128, vec![220; 128 * 128 * 4]);
        assert!(should_replace_strip_preview(
            Some(StripPreviewBufferTag::IsoGainMapBaseline),
            Some(PreviewStage::Initial),
            &refined,
            StripPreviewBufferTag::HdrToneMappedStrip,
            PreviewStage::Refined,
            logical,
        ));
    }

    #[test]
    fn strip_cache_rejects_same_tag_tier_refresh_by_dimensions() {
        let fallback = DecodedImage::new(4807, 3205, vec![180; 4807 * 3205 * 4]);
        assert!(!should_replace_strip_preview(
            Some(StripPreviewBufferTag::StripDecodedPixels),
            Some(PreviewStage::Refined),
            &fallback,
            StripPreviewBufferTag::StripDecodedPixels,
            PreviewStage::Refined,
            Some((4807, 3205)),
        ));
    }

    #[test]
    fn strip_cache_logical_size_change_does_not_bypass_rank() {
        let fallback = DecodedImage::new(128, 85, vec![180; 128 * 85 * 4]);
        assert!(!should_replace_strip_preview(
            Some(StripPreviewBufferTag::StripDecodedPixels),
            Some(PreviewStage::Refined),
            &fallback,
            StripPreviewBufferTag::StripDecodedPixels,
            PreviewStage::Refined,
            Some((4807, 3205)),
        ));
    }

    #[test]
    fn strip_cache_cold_initial_does_not_replace_hdr_refined_on_logical_drift() {
        let cold = DecodedImage::new(128, 96, vec![200; 128 * 96 * 4]);
        assert!(!should_replace_strip_preview(
            Some(StripPreviewBufferTag::HdrToneMappedStrip),
            Some(PreviewStage::Refined),
            &cold,
            StripPreviewBufferTag::StripDecodedPixels,
            PreviewStage::Initial,
            Some((3648, 2736)),
        ));
        assert!(should_replace_strip_preview(
            Some(StripPreviewBufferTag::StripDecodedPixels),
            Some(PreviewStage::Initial),
            &cold,
            StripPreviewBufferTag::HdrToneMappedStrip,
            PreviewStage::Refined,
            Some((3718, 2778)),
        ));
    }

    #[test]
    fn strip_preview_accepts_panorama_after_integer_rounding() {
        assert!(preview_aspect_matches_logical(3, 128, 1000, 50_000));
    }

    #[test]
    fn clear_gpu_textures_keeps_logical_sizes_for_regeneration() {
        let ctx = egui::Context::default();
        let mut cache = DirectoryTreeStripCache::default();
        let decoded = DecodedImage::new(64, 32, vec![128; 64 * 32 * 4]);
        cache.upsert_from_decoded(
            0,
            &decoded,
            StripDecodedUpsert {
                stage: PreviewStage::Refined,
                buffer_tag: StripPreviewBufferTag::StripDecodedPixels,
                logical_size: Some((640, 320)),
                path: Path::new("/test/strip.jpg"),
                ctx: &ctx,
                strip_max_side: 128,
                strip_max_side_used: Some(128),
            },
        );
        assert!(cache.contains(0));
        cache.clear_gpu_textures();
        assert!(!cache.contains(0));
        assert_eq!(cache.logical_sizes().get(&0), Some(&(640, 320)));
    }

    #[test]
    fn insert_from_texture_handle_bumps_gpu_revision() {
        let ctx = egui::Context::default();
        let mut cache = DirectoryTreeStripCache::default();
        assert_eq!(cache.gpu_revision(), 0);
        let rgba = vec![255u8; 8 * 8 * 4];
        let handle = ctx.load_texture(
            "strip_insert_test",
            egui::ColorImage::from_rgba_unmultiplied([8, 8], &rgba),
            egui::TextureOptions::LINEAR,
        );
        cache.insert_from_texture_handle(
            0,
            &handle,
            PreviewStage::Refined,
            StripPreviewBufferTag::MainWindowTextureCacheSdr,
            Some((80, 80)),
            Path::new("/test/strip.jpg"),
        );
        assert!(cache.contains(0));
        assert_eq!(cache.gpu_revision(), 1);
    }

    #[test]
    fn partial_remap_handles_swap_without_losing_entries() {
        let ctx = egui::Context::default();
        let mut cache = DirectoryTreeStripCache::default();
        for index in 0..3 {
            let fill = ((index + 1) * 40) as u8;
            let decoded = DecodedImage::new(16, 16, vec![fill; 16 * 16 * 4]);
            cache.upsert_from_decoded(
                index,
                &decoded,
                StripDecodedUpsert {
                    stage: PreviewStage::Refined,
                    buffer_tag: StripPreviewBufferTag::StripDecodedPixels,
                    logical_size: None,
                    path: Path::new("/test/strip.jpg"),
                    ctx: &ctx,
                    strip_max_side: 128,
                    strip_max_side_used: Some(128),
                },
            );
        }
        // Swap indices 1 and 2.
        let old_to_new = vec![0, 2, 1];
        cache.partial_remap(&old_to_new);
        assert!(cache.contains(0));
        assert!(cache.contains(1));
        assert!(cache.contains(2));
    }

    #[test]
    fn relocate_moves_cached_entry_to_new_index() {
        let ctx = egui::Context::default();
        let mut cache = DirectoryTreeStripCache::default();
        let decoded = DecodedImage::new(16, 16, vec![1; 16 * 16 * 4]);
        cache.upsert_from_decoded(
            0,
            &decoded,
            StripDecodedUpsert {
                stage: PreviewStage::Refined,
                buffer_tag: StripPreviewBufferTag::StripDecodedPixels,
                logical_size: None,
                path: Path::new("/test/strip.jpg"),
                ctx: &ctx,
                strip_max_side: 128,
                strip_max_side_used: Some(128),
            },
        );
        cache.relocate(0, 5);
        assert!(cache.contains(5));
        assert!(!cache.contains(0));
    }
}
