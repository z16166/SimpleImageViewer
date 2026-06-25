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

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;

use eframe::egui::{self, ColorImage, TextureOptions};

use crate::loader::downsample_decoded_for_strip;
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
    /// Preload deferred SDR upload pixels without a dedicated strip tone-map pass.
    PreloadSdrFallback,
    /// ISO gain-map deferred baseline only (`PreviewStage::Initial`).
    IsoGainMapBaseline,
    /// Strip-sized decode from cold worker, tiled source, or static SDR install path.
    StripDecodedPixels,
    /// CPU tone-mapped HDR strip (`hdr_cache_sync`, preload install, fallback refinement).
    HdrToneMappedStrip,
    /// Composed HDR strip after iso-deferred gain-map apply (`PreviewStage::Refined` upgrade).
    HdrComposedStrip,
}

impl StripPreviewBufferTag {
    fn quality_rank(self, stage: PreviewStage) -> u16 {
        let base = match self {
            Self::SdrDeferredPlaceholder => 0,
            Self::MainWindowTextureCacheSdr => 1,
            Self::MainWindowTiledPreview => 2,
            Self::PreloadSdrFallback => 3,
            Self::IsoGainMapBaseline => 4,
            Self::StripDecodedPixels => 5,
            Self::HdrToneMappedStrip => 6,
            Self::HdrComposedStrip => 7,
        };
        let stage_bonus = match stage {
            PreviewStage::Initial => 0,
            PreviewStage::Refined => 1,
        };
        base * 2 + stage_bonus
    }
}

pub(crate) struct DirectoryTreeStripPreviewJobResult {
    pub index: usize,
    pub path: PathBuf,
    pub image_list_generation: u64,
    pub decoded: DecodedImage,
    pub logical: (u32, u32),
    pub stage: PreviewStage,
    pub buffer_tag: StripPreviewBufferTag,
}

/// Decoded strip thumbnail waiting for GPU upload during UI paint (not in `logic()`).
pub(crate) struct DirectoryTreeStripPendingGpuUpload {
    pub index: usize,
    pub decoded: DecodedImage,
    pub stage: PreviewStage,
    pub logical: Option<(u32, u32)>,
    pub buffer_tag: StripPreviewBufferTag,
}

/// Limit GPU texture uploads per paint pass (checklist #3).
pub(crate) const MAX_STRIP_GPU_UPLOADS_PER_PAINT: usize = 12;
pub(crate) const MAX_STRIP_PENDING_GPU_UPLOADS: usize = 256;

pub(crate) struct DirectoryTreeStripCache {
    textures: HashMap<usize, egui::TextureHandle>,
    preview_buffer_tag: HashMap<usize, StripPreviewBufferTag>,
    preview_stage: HashMap<usize, PreviewStage>,
    logical_sizes: HashMap<usize, (u32, u32)>,
    lru_order: VecDeque<usize>,
    gpu_revision: u64,
}

impl Default for DirectoryTreeStripCache {
    fn default() -> Self {
        Self {
            textures: HashMap::new(),
            preview_buffer_tag: HashMap::new(),
            preview_stage: HashMap::new(),
            logical_sizes: HashMap::new(),
            lru_order: VecDeque::new(),
            gpu_revision: 0,
        }
    }
}

impl DirectoryTreeStripCache {
    pub(crate) fn contains(&self, index: usize) -> bool {
        self.textures.contains_key(&index)
    }

    fn touch_lru(&mut self, index: usize) {
        if let Some(pos) = self.lru_order.iter().position(|&cached| cached == index) {
            self.lru_order.remove(pos);
        }
        self.lru_order.push_back(index);
    }

    pub(crate) fn remove_index(&mut self, index: usize) {
        self.textures.remove(&index);
        self.preview_buffer_tag.remove(&index);
        self.preview_stage.remove(&index);
        self.logical_sizes.remove(&index);
        if let Some(pos) = self.lru_order.iter().position(|&cached| cached == index) {
            self.lru_order.remove(pos);
        }
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

    pub(crate) fn insert_from_texture_handle(
        &mut self,
        index: usize,
        texture: egui::TextureHandle,
        stage: PreviewStage,
        buffer_tag: StripPreviewBufferTag,
        logical: Option<(u32, u32)>,
        _current_index: usize,
        _total_count: usize,
    ) {
        let size = texture.size();
        let preview_w = size[0] as u32;
        let preview_h = size[1] as u32;
        if let Some((logical_w, logical_h)) = logical
            && !preview_aspect_matches_logical(preview_w, preview_h, logical_w, logical_h)
        {
            return;
        }
        let cached_tag = self.preview_buffer_tag.get(&index).copied();
        let cached_stage = self.preview_stage.get(&index).copied();
        let cached_logical = self.logical_sizes.get(&index).copied();
        if !should_replace_strip_texture(
            cached_tag,
            cached_stage,
            cached_logical,
            buffer_tag,
            stage,
            logical,
            preview_w,
            preview_h,
        ) {
            #[cfg(feature = "preload-debug")]
            crate::preload_debug!(
                "[PreloadDebug][StripCache] insert skip idx={} reason=should_replace_false \
                 cached_tag={cached_tag:?} incoming_tag={buffer_tag:?} tex={preview_w}x{preview_h}",
                index
            );
            return;
        }
        if let Some((logical_w, logical_h)) = logical {
            self.logical_sizes.insert(index, (logical_w, logical_h));
        }
        self.textures.insert(index, texture);
        self.preview_buffer_tag.insert(index, buffer_tag);
        self.preview_stage.insert(index, stage);
        self.touch_lru(index);
        self.bump_gpu_revision();
        self.evict_if_needed();
    }

    pub(crate) fn upsert_from_decoded(
        &mut self,
        index: usize,
        decoded: &DecodedImage,
        stage: PreviewStage,
        buffer_tag: StripPreviewBufferTag,
        logical_size: Option<(u32, u32)>,
        ctx: &egui::Context,
        _current_index: usize,
        _total_count: usize,
        strip_max_side: u32,
    ) {
        if decoded.is_sdr_deferred_placeholder() || buffer_tag == StripPreviewBufferTag::SdrDeferredPlaceholder
        {
            #[cfg(feature = "preload-debug")]
            crate::preload_debug!(
                "[PreloadDebug][StripCache] upsert skip idx={} reason=black_placeholder",
                index
            );
            return;
        }
        let cached_tag = self.preview_buffer_tag.get(&index).copied();
        let cached_stage = self.preview_stage.get(&index).copied();
        let cached_logical = self.logical_sizes.get(&index).copied();
        if !should_replace_strip_preview(
            cached_tag,
            cached_stage,
            cached_logical,
            decoded,
            buffer_tag,
            stage,
            logical_size,
        ) {
            #[cfg(feature = "preload-debug")]
            crate::preload_debug!(
                "[PreloadDebug][StripCache] upsert skip idx={} reason=should_replace_false \
                 cached_tag={cached_tag:?} cached_stage={cached_stage:?} \
                 incoming_tag={buffer_tag:?} new={}x{} stage={stage:?} logical={logical_size:?}",
                index,
                decoded.width,
                decoded.height
            );
            return;
        }
        let thumb = match downsample_decoded_for_strip(decoded, strip_max_side) {
            Ok(thumb) => thumb,
            Err(err) => {
                log::warn!(
                    "[DirectoryTree] Strip thumbnail downsample failed for index {index}: {err}"
                );
                #[cfg(feature = "preload-debug")]
                crate::preload_debug!(
                    "[PreloadDebug][StripCache] upsert skip idx={} reason=downsample_err err={err}",
                    index
                );
                return;
            }
        };
        let color_image = ColorImage::from_rgba_unmultiplied(
            [thumb.width as usize, thumb.height as usize],
            thumb.rgba(),
        );
        let handle = ctx.load_texture(
            format!("dir_tree_strip_{index}"),
            color_image,
            TextureOptions::LINEAR,
        );
        if let Some(logical) = logical_size {
            self.logical_sizes.insert(index, logical);
        }
        self.textures.insert(index, handle);
        self.preview_buffer_tag.insert(index, buffer_tag);
        self.preview_stage.insert(index, stage);
        self.touch_lru(index);
        self.bump_gpu_revision();
        #[cfg(feature = "preload-debug")]
        let count_before_evict = self.textures.len();
        self.evict_if_needed();
        #[cfg(feature = "preload-debug")]
        crate::preload_debug!(
            "[PreloadDebug][StripCache] upsert ok idx={} tag={buffer_tag:?} tex={}x{} logical={logical_size:?} \
             cache_count={} evicted={} rev={}",
            index,
            thumb.width,
            thumb.height,
            self.textures.len(),
            count_before_evict.saturating_sub(self.textures.len()),
            self.gpu_revision
        );
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
        self.lru_order.retain(|idx| *idx != to);
        let mut found = false;
        for entry in &mut self.lru_order {
            if *entry == from {
                *entry = to;
                found = true;
            }
        }
        if !found && self.textures.contains_key(&to) {
            self.touch_lru(to);
        }
    }

    #[allow(dead_code)]
    pub(crate) fn partial_remap(&mut self, old_to_new: &[usize]) {
        remap_partial_hashmap(&mut self.textures, old_to_new);
        remap_partial_hashmap(&mut self.preview_buffer_tag, old_to_new);
        remap_partial_hashmap(&mut self.preview_stage, old_to_new);
        remap_partial_hashmap(&mut self.logical_sizes, old_to_new);
        self.lru_order.retain_mut(|index| {
            if *index < old_to_new.len() {
                let new_idx = old_to_new[*index];
                if new_idx != usize::MAX {
                    *index = new_idx;
                    return true;
                }
            }
            false
        });
    }

    pub(crate) fn permute(&mut self, old_to_new: &[usize]) {
        permute_usize_hashmap(&mut self.textures, old_to_new);
        permute_usize_hashmap(&mut self.preview_buffer_tag, old_to_new);
        permute_usize_hashmap(&mut self.preview_stage, old_to_new);
        permute_usize_hashmap(&mut self.logical_sizes, old_to_new);
        for index in &mut self.lru_order {
            if *index < old_to_new.len() {
                *index = old_to_new[*index];
            }
        }
    }

    pub(crate) fn retain(&mut self, mut keep: impl FnMut(usize) -> bool) {
        self.textures.retain(|index, _| keep(*index));
        self.preview_buffer_tag.retain(|index, _| keep(*index));
        self.preview_stage.retain(|index, _| keep(*index));
        self.logical_sizes.retain(|index, _| keep(*index));
        self.lru_order.retain(|index| keep(*index));
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
            let Some(idx) = self.lru_order.pop_front() else {
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

pub(crate) fn strip_preview_quality_rank(
    tag: StripPreviewBufferTag,
    stage: PreviewStage,
) -> u16 {
    tag.quality_rank(stage)
}

pub(crate) fn should_replace_strip_preview(
    cached_tag: Option<StripPreviewBufferTag>,
    cached_stage: Option<PreviewStage>,
    cached_logical: Option<(u32, u32)>,
    decoded: &DecodedImage,
    incoming_tag: StripPreviewBufferTag,
    stage: PreviewStage,
    logical_size: Option<(u32, u32)>,
) -> bool {
    if incoming_tag == StripPreviewBufferTag::SdrDeferredPlaceholder
        || decoded.is_sdr_deferred_placeholder()
    {
        return false;
    }
    if !decoded_rgba_size_valid(decoded) {
        return false;
    }
    if let Some((logical_w, logical_h)) = logical_size
        && !preview_aspect_matches_logical(decoded.width, decoded.height, logical_w, logical_h)
    {
        return false;
    }
    if logical_size.is_some_and(|logical| cached_logical != Some(logical)) {
        return true;
    }
    match cached_tag {
        None => true,
        Some(cached) => {
            let incoming_rank = strip_preview_quality_rank(incoming_tag, stage);
            let cached_rank =
                strip_preview_quality_rank(cached, cached_stage.unwrap_or(PreviewStage::Initial));
            incoming_rank > cached_rank
        }
    }
}

pub(crate) fn should_replace_strip_texture(
    cached_tag: Option<StripPreviewBufferTag>,
    cached_stage: Option<PreviewStage>,
    cached_logical: Option<(u32, u32)>,
    incoming_tag: StripPreviewBufferTag,
    stage: PreviewStage,
    logical_size: Option<(u32, u32)>,
    preview_w: u32,
    preview_h: u32,
) -> bool {
    if incoming_tag == StripPreviewBufferTag::SdrDeferredPlaceholder {
        return false;
    }
    if let Some((logical_w, logical_h)) = logical_size
        && !preview_aspect_matches_logical(preview_w, preview_h, logical_w, logical_h)
    {
        return false;
    }
    if logical_size.is_some_and(|logical| cached_logical != Some(logical)) {
        return true;
    }
    match cached_tag {
        None => true,
        Some(cached) => {
            let incoming_rank = strip_preview_quality_rank(incoming_tag, stage);
            let cached_rank =
                strip_preview_quality_rank(cached, cached_stage.unwrap_or(PreviewStage::Initial));
            incoming_rank > cached_rank
        }
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::*;
    use crate::loader::downsample_decoded_for_strip;

    #[test]
    fn downsample_decoded_for_strip_keeps_small_images_as_is() {
        let decoded = DecodedImage::new(64, 48, vec![0; 64 * 48 * 4]);
        let out = downsample_decoded_for_strip(&decoded, 128).expect("downsample");
        assert!(matches!(out, Cow::Borrowed(_)));
        assert_eq!(out.width, 64);
        assert_eq!(out.height, 48);
    }

    #[test]
    fn downsample_decoded_for_strip_scales_large_images() {
        let decoded = DecodedImage::new(512, 256, vec![0; 512 * 256 * 4]);
        let out = downsample_decoded_for_strip(&decoded, 128).expect("downsample");
        assert!(matches!(out, Cow::Owned(_)));
        assert_eq!(out.width, 128);
        assert_eq!(out.height, 64);
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
                PreviewStage::Refined,
                StripPreviewBufferTag::StripDecodedPixels,
                None,
                &ctx,
                0,
                total,
                128,
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
            None,
            &bootstrap,
            StripPreviewBufferTag::StripDecodedPixels,
            PreviewStage::Initial,
            Some((8000, 2000)),
        ));
        assert!(!should_replace_strip_preview(
            None,
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
            Some((480, 270)),
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
            PreviewStage::Initial,
            StripPreviewBufferTag::StripDecodedPixels,
            Some((512, 256)),
            &ctx,
            0,
            1,
            128,
        );
        assert!(cache.contains(0));
        let black = DecodedImage::new_sdr_deferred_placeholder(512, 256, vec![0; 512 * 256 * 4]);
        cache.upsert_from_decoded(
            0,
            &black,
            PreviewStage::Refined,
            StripPreviewBufferTag::SdrDeferredPlaceholder,
            Some((512, 256)),
            &ctx,
            0,
            1,
            128,
        );
        assert!(cache.contains(0));
        assert_eq!(cache.preview_dimensions(0), Some((128, 64)));
    }

    #[test]
    fn strip_cache_rejects_black_refined_over_initial_preview() {
        let good = DecodedImage::new(128, 64, vec![200; 128 * 64 * 4]);
        let black =
            DecodedImage::new_sdr_deferred_placeholder(4096, 2048, vec![0; 4096 * 2048 * 4]);
        assert!(!should_replace_strip_preview(
            Some(StripPreviewBufferTag::StripDecodedPixels),
            Some(PreviewStage::Initial),
            None,
            &black,
            StripPreviewBufferTag::SdrDeferredPlaceholder,
            PreviewStage::Refined,
            Some((4096, 2048)),
        ));
        assert!(should_replace_strip_preview(
            Some(StripPreviewBufferTag::StripDecodedPixels),
            Some(PreviewStage::Initial),
            None,
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
            logical,
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
            logical,
            &black_placeholder,
            StripPreviewBufferTag::SdrDeferredPlaceholder,
            PreviewStage::Initial,
            logical,
        ));
        let refined = DecodedImage::new(128, 128, vec![220; 128 * 128 * 4]);
        assert!(should_replace_strip_preview(
            Some(StripPreviewBufferTag::IsoGainMapBaseline),
            Some(PreviewStage::Initial),
            logical,
            &refined,
            StripPreviewBufferTag::HdrComposedStrip,
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
            Some((4807, 3205)),
            &fallback,
            StripPreviewBufferTag::StripDecodedPixels,
            PreviewStage::Refined,
            Some((4807, 3205)),
        ));
    }

    #[test]
    fn strip_cache_allows_replace_when_logical_size_changes() {
        let fallback = DecodedImage::new(128, 85, vec![180; 128 * 85 * 4]);
        assert!(should_replace_strip_preview(
            Some(StripPreviewBufferTag::StripDecodedPixels),
            Some(PreviewStage::Refined),
            Some((4704, 3136)),
            &fallback,
            StripPreviewBufferTag::StripDecodedPixels,
            PreviewStage::Refined,
            Some((4807, 3205)),
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
            PreviewStage::Refined,
            StripPreviewBufferTag::StripDecodedPixels,
            Some((640, 320)),
            &ctx,
            0,
            1,
            128,
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
            handle,
            PreviewStage::Refined,
            StripPreviewBufferTag::MainWindowTextureCacheSdr,
            Some((80, 80)),
            0,
            1,
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
                PreviewStage::Refined,
                StripPreviewBufferTag::StripDecodedPixels,
                None,
                &ctx,
                0,
                3,
                128,
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
            PreviewStage::Refined,
            StripPreviewBufferTag::StripDecodedPixels,
            None,
            &ctx,
            0,
            1,
            128,
        );
        cache.relocate(0, 5);
        assert!(cache.contains(5));
        assert!(!cache.contains(0));
    }
}
