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

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::PathBuf;

use eframe::egui::{self, ColorImage, TextureOptions};
use image::imageops::FilterType;

use crate::loader::{
    DecodedImage, PreviewStage, decoded_looks_like_black_placeholder,
    preview_aspect_matches_logical,
};

use crate::app::index_cache_permute::permute_usize_hashmap;

/// Maximum strip preview textures retained in memory (ring-distance eviction).
///
/// Eviction scans all cached indices when over cap (`evict_if_needed` is O(n) per insert).
/// The cap stays at 128 by design so this scan is cheap; raise only with an LRU structure.
pub(crate) const DIRECTORY_TREE_STRIP_CACHE_MAX: usize = 128;

pub(crate) struct DirectoryTreeStripPreviewJobResult {
    pub index: usize,
    pub path: PathBuf,
    pub image_list_generation: u64,
    pub decoded: DecodedImage,
    pub logical: (u32, u32),
    pub stage: PreviewStage,
}

/// Decoded strip thumbnail waiting for GPU upload during UI paint (not in `logic()`).
pub(crate) struct DirectoryTreeStripPendingGpuUpload {
    pub index: usize,
    pub decoded: DecodedImage,
    pub stage: PreviewStage,
    pub logical: Option<(u32, u32)>,
}

/// Limit GPU texture uploads per paint pass (checklist #3).
pub(crate) const MAX_STRIP_GPU_UPLOADS_PER_PAINT: usize = 4;

pub(crate) struct DirectoryTreeStripCache {
    textures: HashMap<usize, egui::TextureHandle>,
    preview_max_side: HashMap<usize, u32>,
    preview_stage: HashMap<usize, PreviewStage>,
    logical_sizes: HashMap<usize, (u32, u32)>,
    gpu_revision: u64,
}

impl Default for DirectoryTreeStripCache {
    fn default() -> Self {
        Self {
            textures: HashMap::new(),
            preview_max_side: HashMap::new(),
            preview_stage: HashMap::new(),
            logical_sizes: HashMap::new(),
            gpu_revision: 0,
        }
    }
}

impl DirectoryTreeStripCache {
    pub(crate) fn contains(&self, index: usize) -> bool {
        self.textures.contains_key(&index)
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
            self.textures.remove(&index);
            self.preview_max_side.remove(&index);
            self.preview_stage.remove(&index);
            self.logical_sizes.remove(&index);
            return true;
        }
        false
    }

    pub(crate) fn cached_preview_max_side(&self, index: usize) -> Option<u32> {
        self.preview_max_side.get(&index).copied()
    }

    pub(crate) fn insert_from_texture_handle(
        &mut self,
        index: usize,
        texture: egui::TextureHandle,
        stage: PreviewStage,
        preview_max_side: u32,
        logical: Option<(u32, u32)>,
        current_index: usize,
        total_count: usize,
    ) {
        if let Some((logical_w, logical_h)) = logical {
            let size = texture.size();
            if !preview_aspect_matches_logical(size[0] as u32, size[1] as u32, logical_w, logical_h)
            {
                return;
            }
            self.logical_sizes.insert(index, (logical_w, logical_h));
        }
        self.textures.insert(index, texture);
        self.preview_max_side.insert(index, preview_max_side);
        self.preview_stage.insert(index, stage);
        self.bump_gpu_revision();
        self.evict_if_needed(current_index, total_count);
    }

    pub(crate) fn upsert_from_decoded(
        &mut self,
        index: usize,
        decoded: &DecodedImage,
        stage: PreviewStage,
        logical_size: Option<(u32, u32)>,
        ctx: &egui::Context,
        current_index: usize,
        total_count: usize,
        strip_max_side: u32,
    ) {
        if decoded_looks_like_black_placeholder(decoded) {
            self.textures.remove(&index);
            self.preview_max_side.remove(&index);
            self.preview_stage.remove(&index);
            self.bump_gpu_revision();
            return;
        }
        if !should_replace_strip_thumbnail(
            self.preview_max_side.get(&index).copied(),
            self.preview_stage.get(&index).copied(),
            decoded,
            stage,
            logical_size,
        ) {
            return;
        }
        let thumb = match downsample_decoded_for_strip(decoded, strip_max_side) {
            Ok(thumb) => thumb,
            Err(err) => {
                log::warn!(
                    "[DirectoryTree] Strip thumbnail downsample failed for index {index}: {err}"
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
        let preview_max_side = decoded.width.max(decoded.height);
        if let Some(logical) = logical_size {
            self.logical_sizes.insert(index, logical);
        }
        self.textures.insert(index, handle);
        self.preview_max_side.insert(index, preview_max_side);
        self.preview_stage.insert(index, stage);
        self.bump_gpu_revision();
        self.evict_if_needed(current_index, total_count);
    }

    pub(crate) fn relocate(&mut self, from: usize, to: usize) {
        if from == to {
            return;
        }
        if let Some(tex) = self.textures.remove(&from) {
            self.textures.insert(to, tex);
        }
        if let Some(max_side) = self.preview_max_side.remove(&from) {
            self.preview_max_side.insert(to, max_side);
        }
        if let Some(stage) = self.preview_stage.remove(&from) {
            self.preview_stage.insert(to, stage);
        }
        if let Some(logical) = self.logical_sizes.remove(&from) {
            self.logical_sizes.insert(to, logical);
        }
    }

    pub(crate) fn permute(&mut self, old_to_new: &[usize]) {
        permute_usize_hashmap(&mut self.textures, old_to_new);
        permute_usize_hashmap(&mut self.preview_max_side, old_to_new);
        permute_usize_hashmap(&mut self.preview_stage, old_to_new);
        permute_usize_hashmap(&mut self.logical_sizes, old_to_new);
    }

    pub(crate) fn retain(&mut self, mut keep: impl FnMut(usize) -> bool) {
        self.textures.retain(|index, _| keep(*index));
        self.preview_max_side.retain(|index, _| keep(*index));
        self.preview_stage.retain(|index, _| keep(*index));
        self.logical_sizes.retain(|index, _| keep(*index));
    }

    /// Drop GPU-backed egui textures after a wgpu surface format hot-swap. CPU-side
    /// logical sizes are kept so regeneration can validate aspect ratio.
    pub(crate) fn clear_gpu_textures(&mut self) {
        self.textures.clear();
        self.preview_max_side.clear();
        self.preview_stage.clear();
        self.bump_gpu_revision();
    }

    pub(crate) fn clear_all(&mut self) {
        self.textures.clear();
        self.preview_max_side.clear();
        self.preview_stage.clear();
        self.logical_sizes.clear();
        self.bump_gpu_revision();
    }

    /// Drop the list-preview entry farthest from `current_index` on the circular file list.
    ///
    /// Intentionally O(n) over at most [`DIRECTORY_TREE_STRIP_CACHE_MAX`] entries; see const docs.
    fn evict_if_needed(&mut self, current_index: usize, total_count: usize) {
        if total_count == 0 {
            return;
        }
        while self.textures.len() > DIRECTORY_TREE_STRIP_CACHE_MAX {
            let to_remove = self.textures.keys().copied().max_by_key(|&idx| {
                if total_count == 0 {
                    (idx as isize - current_index as isize).unsigned_abs()
                } else {
                    let forward = (idx + total_count - current_index) % total_count;
                    let backward = (current_index + total_count - idx) % total_count;
                    forward.min(backward)
                }
            });
            let Some(idx) = to_remove else {
                break;
            };
            self.textures.remove(&idx);
            self.preview_max_side.remove(&idx);
            self.preview_stage.remove(&idx);
            self.logical_sizes.remove(&idx);
            self.bump_gpu_revision();
        }
    }
}

pub(crate) fn decoded_rgba_size_valid(decoded: &DecodedImage) -> bool {
    decoded.rgba().len() == decoded.width as usize * decoded.height as usize * 4
}

pub(crate) fn should_replace_strip_thumbnail(
    cached_max_side: Option<u32>,
    cached_stage: Option<PreviewStage>,
    decoded: &DecodedImage,
    stage: PreviewStage,
    logical_size: Option<(u32, u32)>,
) -> bool {
    if decoded_looks_like_black_placeholder(decoded) {
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
    let new_max_side = decoded.width.max(decoded.height);
    match cached_max_side {
        None => true,
        Some(cached_max_side) => {
            if stage == PreviewStage::Refined && cached_stage == Some(PreviewStage::Initial) {
                return true;
            }
            new_max_side > cached_max_side
        }
    }
}

pub(crate) fn downsample_decoded_for_strip<'a>(
    decoded: &'a DecodedImage,
    max_side: u32,
) -> Result<Cow<'a, DecodedImage>, String> {
    let max_dim = decoded.width.max(decoded.height);
    if max_dim <= max_side {
        return Ok(Cow::Borrowed(decoded));
    }
    let src = decoded.clone().into_rgba8_image()?;
    let scale = max_side as f32 / max_dim as f32;
    let out_w = ((decoded.width as f32 * scale).round() as u32).max(1);
    let out_h = ((decoded.height as f32 * scale).round() as u32).max(1);
    let resized = image::imageops::resize(&src, out_w, out_h, FilterType::Triangle);
    Ok(Cow::Owned(DecodedImage::from(resized)))
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn strip_cache_evicts_farthest_neighbor_first() {
        let ctx = egui::Context::default();
        let mut cache = DirectoryTreeStripCache::default();
        let total = DIRECTORY_TREE_STRIP_CACHE_MAX + 5;
        for index in 0..total {
            let decoded = DecodedImage::new(32, 32, vec![255; 32 * 32 * 4]);
            cache.upsert_from_decoded(
                index,
                &decoded,
                PreviewStage::Refined,
                None,
                &ctx,
                0,
                total,
                128,
            );
        }
        assert_eq!(cache.textures().len(), DIRECTORY_TREE_STRIP_CACHE_MAX);
        assert!(cache.contains(0));
        assert!(cache.contains(1));
    }

    #[test]
    fn strip_cache_rejects_preview_with_mismatched_aspect() {
        let bootstrap = DecodedImage::new(512, 512, vec![0; 512 * 512 * 4]);
        assert!(!should_replace_strip_thumbnail(
            None,
            None,
            &bootstrap,
            PreviewStage::Initial,
            Some((8000, 2000)),
        ));
        assert!(!should_replace_strip_thumbnail(
            None,
            None,
            &bootstrap,
            PreviewStage::Refined,
            Some((800, 8000)),
        ));
    }

    #[test]
    fn strip_cache_upgrades_refined_preview_over_initial_bootstrap() {
        let refined = DecodedImage::new(2048, 512, vec![180; 2048 * 512 * 4]);
        assert!(should_replace_strip_thumbnail(
            Some(512),
            Some(PreviewStage::Initial),
            &refined,
            PreviewStage::Refined,
            Some((8000, 2000)),
        ));
    }

    #[test]
    fn strip_cache_rejects_black_refined_over_initial_preview() {
        let good = DecodedImage::new(128, 64, vec![200; 128 * 64 * 4]);
        let black = DecodedImage::new(4096, 2048, vec![0; 4096 * 2048 * 4]);
        assert!(!should_replace_strip_thumbnail(
            Some(128),
            Some(PreviewStage::Initial),
            &black,
            PreviewStage::Refined,
            Some((4096, 2048)),
        ));
        assert!(should_replace_strip_thumbnail(
            Some(128),
            Some(PreviewStage::Initial),
            &good,
            PreviewStage::Refined,
            Some((4096, 2048)),
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
        cache.insert_from_texture_handle(0, handle, PreviewStage::Refined, 8, Some((80, 80)), 0, 1);
        assert!(cache.contains(0));
        assert_eq!(cache.gpu_revision(), 1);
    }
}
