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

use std::collections::{BTreeMap, HashMap};
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};

use eframe::egui::{self, ColorImage, TextureOptions};

use crate::constants::checked_rgba_buffer_len;
use crate::loader::{DecodedImage, PreviewStage, preview_aspect_matches_logical};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DirectoryTreeStripJobToken {
    /// Synchronous pixels copied from an already available preview/cache path.
    SynchronousUpload,
    /// Background worker attempt that must match active in-flight bookkeeping.
    Worker(NonZeroU64),
}

impl DirectoryTreeStripJobToken {
    pub(crate) fn worker_token(self) -> Option<NonZeroU64> {
        match self {
            Self::SynchronousUpload => None,
            Self::Worker(token) => Some(token),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DirectoryTreeStripJobKey {
    pub index: usize,
    pub path: PathBuf,
    pub image_list_generation: u64,
    pub job_token: DirectoryTreeStripJobToken,
}

pub(crate) struct DirectoryTreeStripPreviewSuccess {
    pub key: DirectoryTreeStripJobKey,
    pub decoded: DecodedImage,
    /// Full static SDR decode produced while generating a cold strip thumbnail.
    /// When present, the app can reuse it as a preloaded main image instead of
    /// decoding the same file again.
    pub reusable_full_decoded: Option<DecodedImage>,
    pub logical: (u32, u32),
    pub stage: PreviewStage,
    pub buffer_tag: StripPreviewBufferTag,
    /// `strip_max_side()` used when the worker last sized `decoded` for strip upload.
    pub strip_max_side_used: u32,
}

pub(crate) struct DirectoryTreeStripPreviewFailure {
    pub key: DirectoryTreeStripJobKey,
    pub reason: &'static str,
}

pub(crate) enum DirectoryTreeStripPreviewJobResult {
    Success(DirectoryTreeStripPreviewSuccess),
    /// Cold strip found no fast preview and slow primary decode was skipped; retry after preload.
    DeferredToMainLoader(DirectoryTreeStripPreviewFailure),
}

pub(crate) enum DirectoryTreeStripInflightReleaseKind {
    ClearAttempt,
    PermanentFailure,
}

pub(crate) struct DirectoryTreeStripInflightRelease {
    pub key: DirectoryTreeStripJobKey,
    pub kind: DirectoryTreeStripInflightReleaseKind,
}

pub(crate) struct StripThumbnailCacheRequest<'a> {
    pub index: usize,
    pub decoded: &'a DecodedImage,
    pub job_key: Option<DirectoryTreeStripJobKey>,
    pub stage: PreviewStage,
    pub logical_size: Option<(u32, u32)>,
    pub buffer_tag: StripPreviewBufferTag,
    pub strip_max_side_used: Option<u32>,
    pub ctx: &'a egui::Context,
    pub bypass_detach_queue: bool,
}

pub(crate) struct StripThumbnailCacheOwnedRequest<'a> {
    pub index: usize,
    pub decoded: DecodedImage,
    pub job_key: Option<DirectoryTreeStripJobKey>,
    pub stage: PreviewStage,
    pub logical_size: Option<(u32, u32)>,
    pub buffer_tag: StripPreviewBufferTag,
    pub strip_max_side_used: Option<u32>,
    pub ctx: &'a egui::Context,
    pub bypass_detach_queue: bool,
}

/// Decoded strip thumbnail waiting for GPU upload during UI paint (not in `logic()`).
pub(crate) struct DirectoryTreeStripPendingGpuUpload {
    pub key: DirectoryTreeStripJobKey,
    pub decoded: DecodedImage,
    /// Precomputed RGBA8 upload byte size for queue and per-frame budgets.
    pub upload_bytes: usize,
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

pub(crate) struct DirectoryTreeStripGpuUploadRequest {
    pub index: usize,
    pub decoded: DecodedImage,
    pub stage: PreviewStage,
    pub logical: Option<(u32, u32)>,
    pub buffer_tag: StripPreviewBufferTag,
    pub strip_max_side_used: Option<u32>,
    pub job_key: Option<DirectoryTreeStripJobKey>,
}

/// Limit GPU texture uploads per paint pass (checklist #3).
pub(crate) const MAX_STRIP_GPU_UPLOADS_PER_PAINT: usize = 12;
pub(crate) const MAX_STRIP_PENDING_GPU_UPLOADS: usize = 256;
const MAX_DIRECTORY_TREE_STRIP_PENDING_SIDE: usize = 256;
pub(crate) const DIRECTORY_TREE_STRIP_RGBA_BYTES_PER_PIXEL: usize = 4;
/// Pixel memory cap for one paint-thread strip GPU upload batch.
pub(crate) const MAX_STRIP_GPU_UPLOAD_BYTES_PER_PAINT: usize = MAX_STRIP_GPU_UPLOADS_PER_PAINT
    * MAX_DIRECTORY_TREE_STRIP_PENDING_SIDE
    * MAX_DIRECTORY_TREE_STRIP_PENDING_SIDE
    * DIRECTORY_TREE_STRIP_RGBA_BYTES_PER_PIXEL;

/// Pixel memory cap for strip uploads waiting for paint-thread GPU upload.
pub(crate) const MAX_STRIP_PENDING_GPU_UPLOAD_BYTES: usize = MAX_STRIP_PENDING_GPU_UPLOADS
    * MAX_DIRECTORY_TREE_STRIP_PENDING_SIDE
    * MAX_DIRECTORY_TREE_STRIP_PENDING_SIDE
    * DIRECTORY_TREE_STRIP_RGBA_BYTES_PER_PIXEL;

/// Path-keyed strip thumbnail cache.
///
/// Authoritative state is keyed by [`PathBuf`] so that image-list reorders and
/// scans do not require index remapping. The UI preview snapshot is projected
/// back to `HashMap<usize, _>` at publish time by mapping `image_files[i]`.
#[derive(Default)]
pub(crate) struct DirectoryTreeStripCache {
    textures: HashMap<PathBuf, egui::TextureHandle>,
    preview_buffer_tag: HashMap<PathBuf, StripPreviewBufferTag>,
    preview_stage: HashMap<PathBuf, PreviewStage>,
    logical_sizes: HashMap<PathBuf, (u32, u32)>,
    /// Access tick per path with a live texture; smallest tick is evicted first.
    /// Keyed as `(tick, path)` so [`BTreeMap::pop_first`] yields the LRU victim in O(log n).
    lru_tick: BTreeMap<(u64, PathBuf), ()>,
    /// Cached texture names to avoid repeated `format!` allocations (H-9/H-11).
    texture_names: HashMap<PathBuf, String>,
    lru_clock: u64,
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
    pub(crate) fn contains(&self, path: &Path) -> bool {
        self.textures.contains_key(path)
    }

    fn touch_lru(&mut self, path: &Path) {
        debug_assert!(
            self.textures.contains_key(path),
            "touch_lru requires an existing strip texture entry"
        );
        // Remove old tick entry for this path (if any) — O(n) over 128 entries, negligible.
        self.lru_tick.retain(|(_, p), _| p != path);
        self.lru_clock = self.lru_clock.wrapping_add(1);
        self.lru_tick
            .insert((self.lru_clock, path.to_path_buf()), ());
    }

    /// Mark a cached strip entry recently used so LRU eviction skips visible rows.
    pub(crate) fn touch_cached_path(&mut self, path: &Path) {
        if self.textures.contains_key(path) {
            self.touch_lru(path);
        }
    }

    pub(crate) fn remove_path(&mut self, path: &Path) {
        self.textures.remove(path);
        self.preview_buffer_tag.remove(path);
        self.preview_stage.remove(path);
        self.logical_sizes.remove(path);
        self.texture_names.remove(path);
        self.lru_tick.retain(|(_, p), _| p != path);
    }

    /// Test/debug accessor for the path-keyed texture map. Production publish goes through
    /// [`Self::project_index_maps`] instead of reading this directly.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn textures(&self) -> &HashMap<PathBuf, egui::TextureHandle> {
        &self.textures
    }

    pub(crate) fn logical_sizes(&self) -> &HashMap<PathBuf, (u32, u32)> {
        &self.logical_sizes
    }

    pub(crate) fn gpu_revision(&self) -> u64 {
        self.gpu_revision
    }

    fn bump_gpu_revision(&mut self) {
        self.gpu_revision = self.gpu_revision.wrapping_add(1);
    }

    pub(crate) fn preview_dimensions(&self, path: &Path) -> Option<(u32, u32)> {
        let handle = self.textures.get(path)?;
        let size = handle.size();
        Some((size[0] as u32, size[1] as u32))
    }

    pub(crate) fn is_valid_for_logical(&self, path: &Path, logical: (u32, u32)) -> bool {
        let Some((preview_w, preview_h)) = self.preview_dimensions(path) else {
            return false;
        };
        preview_aspect_matches_logical(preview_w, preview_h, logical.0, logical.1)
    }

    pub(crate) fn invalidate_if_invalid(&mut self, path: &Path, logical: (u32, u32)) -> bool {
        if self.contains(path) && !self.is_valid_for_logical(path, logical) {
            #[cfg(feature = "preload-debug")]
            if let Some((preview_w, preview_h)) = self.preview_dimensions(path) {
                crate::preload_debug!(
                    "[PreloadDebug][StripCache] invalidate path={} preview={}x{} logical={}x{}",
                    path.display(),
                    preview_w,
                    preview_h,
                    logical.0,
                    logical.1
                );
            }
            self.remove_path(path);
            return true;
        }
        false
    }

    pub(crate) fn cached_buffer_tag(&self, path: &Path) -> Option<StripPreviewBufferTag> {
        self.preview_buffer_tag.get(path).copied()
    }

    pub(crate) fn cached_preview_stage(&self, path: &Path) -> Option<PreviewStage> {
        self.preview_stage.get(path).copied()
    }

    /// Write a strip texture into the cache after the caller has already
    /// acquired an [`egui::TextureHandle`] (by clone or by GPU upload).
    ///
    /// All call sites that update a strip thumbnail in memory flow through
    /// this single function, so `preload-debug` logging is centralized here.
    fn commit_strip_texture(
        &mut self,
        texture: egui::TextureHandle,
        buffer_tag: StripPreviewBufferTag,
        stage: PreviewStage,
        logical_size: Option<(u32, u32)>,
        path: &Path,
    ) {
        #[cfg(feature = "preload-debug")]
        let tex_size = texture.size();
        #[cfg(feature = "preload-debug")]
        let tex_w = tex_size[0];
        #[cfg(feature = "preload-debug")]
        let tex_h = tex_size[1];
        #[cfg(feature = "preload-debug")]
        let count_before = self.textures.len();

        if let Some(logical) = logical_size {
            self.logical_sizes.insert(path.to_path_buf(), logical);
        }
        self.textures.insert(path.to_path_buf(), texture);
        self.preview_buffer_tag
            .insert(path.to_path_buf(), buffer_tag);
        self.preview_stage.insert(path.to_path_buf(), stage);
        self.touch_lru(path);
        self.bump_gpu_revision();
        self.evict_if_needed();

        #[cfg(feature = "preload-debug")]
        crate::preload_debug!(
            "[PreloadDebug][StripCache] commit path={} tag={buffer_tag:?} stage={stage:?} \
             tex={tex_w}x{tex_h} logical={logical_size:?} \
             cache_count_before={count_before} cache_count_after={} rev={}",
            path.display(),
            self.textures.len(),
            self.gpu_revision
        );
    }

    fn commit_existing_strip_texture_update(
        &mut self,
        buffer_tag: StripPreviewBufferTag,
        stage: PreviewStage,
        logical_size: Option<(u32, u32)>,
        path: &Path,
    ) {
        if let Some(logical) = logical_size {
            self.logical_sizes.insert(path.to_path_buf(), logical);
        }
        self.preview_buffer_tag
            .insert(path.to_path_buf(), buffer_tag);
        self.preview_stage.insert(path.to_path_buf(), stage);
        self.touch_lru(path);
        self.bump_gpu_revision();

        #[cfg(feature = "preload-debug")]
        {
            let tex_size = self
                .textures
                .get(path)
                .map(|texture| texture.size())
                .unwrap_or([0, 0]);
            crate::preload_debug!(
                "[PreloadDebug][StripCache] update-existing path={} tag={buffer_tag:?} \
                 stage={stage:?} tex={}x{} logical={logical_size:?} rev={}",
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
        path: &Path,
        stage: PreviewStage,
        buffer_tag: StripPreviewBufferTag,
        logical: Option<(u32, u32)>,
        preview_w: u32,
        preview_h: u32,
    ) -> bool {
        let cached_dims = self.preview_dimensions(path);
        evaluate_strip_preview_replace(&StripPreviewReplaceParams {
            path,
            source: "strip_texture_handle_probe",
            cached_tag: self.preview_buffer_tag.get(path).copied(),
            cached_stage: self.preview_stage.get(path).copied(),
            cached_logical: self.logical_sizes.get(path).copied(),
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
    /// holds an equal-or-better entry for this path. The clone only happens
    /// after [`decide_strip_preview_replace`] confirms the replacement.
    pub(crate) fn insert_from_texture_handle(
        &mut self,
        texture: &egui::TextureHandle,
        stage: PreviewStage,
        buffer_tag: StripPreviewBufferTag,
        logical: Option<(u32, u32)>,
        path: &Path,
    ) -> bool {
        let size = texture.size();
        let preview_w = size[0] as u32;
        let preview_h = size[1] as u32;
        let cached_tag = self.preview_buffer_tag.get(path).copied();
        let cached_stage = self.preview_stage.get(path).copied();
        let cached_dims = self.preview_dimensions(path);
        if !decide_strip_preview_replace(&StripPreviewReplaceParams {
            path,
            source: "insert_from_texture_handle",
            cached_tag,
            cached_stage,
            cached_logical: self.logical_sizes.get(path).copied(),
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
        self.commit_strip_texture(texture.clone(), buffer_tag, stage, logical, path);
        true
    }

    pub(crate) fn upsert_from_decoded(
        &mut self,
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
        // Critical precondition (review-checklist #31): must hold in release too -- callers that
        // reach here with full-size pixels should have queued a background resample instead.
        assert!(
            strip_decoded_ready_for_gpu_upload(decoded, strip_max_side, strip_max_side_used),
            "upsert_from_decoded requires strip-sized pixels; schedule background resample first"
        );
        let cached_tag = self.preview_buffer_tag.get(path).copied();
        let cached_stage = self.preview_stage.get(path).copied();
        let cached_dims = self.preview_dimensions(path);
        if !decide_strip_preview_replace(&StripPreviewReplaceParams {
            path,
            source: "upsert_from_decoded",
            cached_tag,
            cached_stage,
            cached_logical: self.logical_sizes.get(path).copied(),
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
        // Same-size refreshes keep the existing TextureId and use handle.set(); recreating
        // textures after HDR swap-chain hot-swap can fail to display on some backends.
        if self
            .textures
            .get(path)
            .is_some_and(|handle| handle.size() == thumb_size)
        {
            if let Some(handle) = self.textures.get_mut(path) {
                handle.set(color_image, TextureOptions::LINEAR);
            }
            self.commit_existing_strip_texture_update(buffer_tag, stage, logical_size, path);
            return;
        }
        let name = self
            .texture_names
            .entry(path.to_path_buf())
            .or_insert_with(|| format!("dir_tree_strip::{}", path.display()));
        let handle = ctx.load_texture(name.as_str(), color_image, TextureOptions::LINEAR);
        self.commit_strip_texture(handle, buffer_tag, stage, logical_size, path);
    }

    pub(crate) fn retain(&mut self, mut keep: impl FnMut(&Path) -> bool) {
        self.textures.retain(|path, _| keep(path));
        self.preview_buffer_tag.retain(|path, _| keep(path));
        self.preview_stage.retain(|path, _| keep(path));
        self.logical_sizes.retain(|path, _| keep(path));
        self.lru_tick.retain(|(_, p), _| keep(p));
        self.texture_names.retain(|path, _| keep(path));
    }

    /// Project the path-keyed cache into index-keyed maps for the UI preview snapshot.
    ///
    /// `path_to_index` maps the current `image_files` paths to their row index; entries
    /// whose path is not in the map (no longer in the list) are dropped.
    ///
    /// Returns sparse index→value Vecs (one entry per matched cache item, at most
    /// `DIRECTORY_TREE_STRIP_CACHE_MAX` = 128) to avoid allocating dense N-element
    /// Vecs for the entire file list. The caller merges these into the visible-row
    /// snapshot Vec (see [`publish_preview_snapshot`]).
    pub(crate) fn project_index_maps(
        &self,
        path_to_index: &HashMap<PathBuf, usize>,
    ) -> ProjectedStripPreview {
        if path_to_index.is_empty() {
            return ProjectedStripPreview {
                textures: Vec::new(),
                logical_sizes: Vec::new(),
                buffer_tags: Vec::new(),
            };
        }
        let mut textures: Vec<(usize, egui::TextureHandle)> = Vec::new();
        let mut logical_sizes: Vec<(usize, (u32, u32))> = Vec::new();
        let mut buffer_tags: Vec<(usize, StripPreviewBufferTag)> = Vec::new();
        for (path, handle) in &self.textures {
            if let Some(&index) = path_to_index.get(path) {
                textures.push((index, handle.clone()));
                if let Some(&size) = self.logical_sizes.get(path) {
                    logical_sizes.push((index, size));
                }
                if let Some(&tag) = self.preview_buffer_tag.get(path) {
                    buffer_tags.push((index, tag));
                }
            }
        }
        ProjectedStripPreview {
            textures,
            logical_sizes,
            buffer_tags,
        }
    }

    /// Drop GPU-backed egui textures after a wgpu surface format hot-swap. CPU-side
    /// logical sizes are kept so regeneration can validate aspect ratio.
    pub(crate) fn clear_gpu_textures(&mut self) {
        self.textures.clear();
        self.preview_buffer_tag.clear();
        self.preview_stage.clear();
        self.lru_tick.clear();
        self.texture_names.clear();
        self.bump_gpu_revision();
    }

    pub(crate) fn clear_all(&mut self) {
        self.textures.clear();
        self.preview_buffer_tag.clear();
        self.preview_stage.clear();
        self.logical_sizes.clear();
        self.lru_tick.clear();
        self.texture_names.clear();
        self.bump_gpu_revision();
    }

    fn evict_if_needed(&mut self) {
        while self.textures.len() > DIRECTORY_TREE_STRIP_CACHE_MAX {
            let Some(((_, victim), _)) = self.lru_tick.pop_first() else {
                break;
            };
            #[cfg(feature = "preload-debug")]
            crate::preload_debug!(
                "[PreloadDebug][StripCache] lru evict path={} cache_count={}",
                victim.display(),
                self.textures.len().saturating_sub(1)
            );
            self.textures.remove(&victim);
            self.preview_buffer_tag.remove(&victim);
            self.preview_stage.remove(&victim);
            // Keep logical_sizes so visible rows can cold-regenerate after LRU eviction.
            self.bump_gpu_revision();
        }
    }
}

/// Sparse index→value projection of the strip cache (one entry per matched cache item).
///
/// Unlike the dense snapshot Vecs (which are sized to the visible row count), this
/// holds at most [`DIRECTORY_TREE_STRIP_CACHE_MAX`] (128) entries and avoids allocating
/// per-frame Vecs of size proportional to the full file list.
pub(crate) struct ProjectedStripPreview {
    pub textures: Vec<(usize, egui::TextureHandle)>,
    pub logical_sizes: Vec<(usize, (u32, u32))>,
    pub buffer_tags: Vec<(usize, StripPreviewBufferTag)>,
}

pub(crate) fn decoded_rgba_size_valid(decoded: &DecodedImage) -> bool {
    checked_rgba_buffer_len(decoded.width as usize, decoded.height as usize)
        .is_some_and(|expected_len| decoded.rgba().len() == expected_len)
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
    pub path: &'a Path,
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
        "[PreloadDebug][StripReplace] path={} source={} decision={decision} reason={} \
         cached_tag={:?} cached_stage={:?} cached_rank={cached_rank:?} \
         cached_tex={}x{} cached_logical={:?} cached_aspect_ok={cached_aspect_ok} \
         incoming_tag={:?} incoming_stage={:?} incoming_rank={incoming_rank} \
         incoming_tex={}x{} incoming_logical={:?} aspect_ok={aspect_ok} pixel_hint={pixel_hint}",
        params.path.display(),
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
        path: Path::new("<test>"),
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
        path: Path::new("<test>"),
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
    use std::path::PathBuf;

    fn strip_test_path(index: usize) -> PathBuf {
        PathBuf::from(format!("/test/strip-{index}.jpg"))
    }

    #[test]
    fn decoded_rgba_size_valid_rejects_overflowing_dimensions() {
        let decoded = DecodedImage::new(1_u32 << 31, 1_u32 << 31, Vec::new());
        assert!(!decoded_rgba_size_valid(&decoded));
    }

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
                &decoded,
                StripDecodedUpsert {
                    stage: PreviewStage::Initial,
                    buffer_tag: StripPreviewBufferTag::StripDecodedPixels,
                    logical_size: None,
                    path: &strip_test_path(index),
                    ctx: &ctx,
                    strip_max_side: 128,
                    strip_max_side_used: Some(128),
                },
            );
        }
        // Upgrade path 0 (oldest) to Refined so it is touched to MRU.
        let touch = DecodedImage::new(16, 16, vec![255; 16 * 16 * 4]);
        cache.upsert_from_decoded(
            &touch,
            StripDecodedUpsert {
                stage: PreviewStage::Refined,
                buffer_tag: StripPreviewBufferTag::StripDecodedPixels,
                logical_size: None,
                path: &strip_test_path(0),
                ctx: &ctx,
                strip_max_side: 128,
                strip_max_side_used: Some(128),
            },
        );
        let overflow = DecodedImage::new(16, 16, vec![128; 16 * 16 * 4]);
        cache.upsert_from_decoded(
            &overflow,
            StripDecodedUpsert {
                stage: PreviewStage::Initial,
                buffer_tag: StripPreviewBufferTag::StripDecodedPixels,
                logical_size: None,
                path: &strip_test_path(DIRECTORY_TREE_STRIP_CACHE_MAX),
                ctx: &ctx,
                strip_max_side: 128,
                strip_max_side_used: Some(128),
            },
        );
        assert!(cache.contains(&strip_test_path(0)));
        assert!(!cache.contains(&strip_test_path(1)));
    }

    #[test]
    fn strip_cache_evicts_oldest_lru_first() {
        let ctx = egui::Context::default();
        let mut cache = DirectoryTreeStripCache::default();
        let total = DIRECTORY_TREE_STRIP_CACHE_MAX + 5;
        for index in 0..total {
            let decoded = DecodedImage::new(32, 32, vec![255; 32 * 32 * 4]);
            cache.upsert_from_decoded(
                &decoded,
                StripDecodedUpsert {
                    stage: PreviewStage::Refined,
                    buffer_tag: StripPreviewBufferTag::StripDecodedPixels,
                    logical_size: None,
                    path: &strip_test_path(index),
                    ctx: &ctx,
                    strip_max_side: 128,
                    strip_max_side_used: Some(128),
                },
            );
        }
        assert_eq!(cache.textures().len(), DIRECTORY_TREE_STRIP_CACHE_MAX);
        assert!(!cache.contains(&strip_test_path(0)));
        assert!(cache.contains(&strip_test_path(total - 1)));
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
            &good,
            StripDecodedUpsert {
                stage: PreviewStage::Initial,
                buffer_tag: StripPreviewBufferTag::StripDecodedPixels,
                logical_size: Some((512, 256)),
                path: &strip_test_path(0),
                ctx: &ctx,
                strip_max_side: 128,
                strip_max_side_used: Some(128),
            },
        );
        assert!(cache.contains(&strip_test_path(0)));
        let black = DecodedImage::new_sdr_deferred_placeholder(512, 256, vec![0; 512 * 256 * 4]);
        cache.upsert_from_decoded(
            &black,
            StripDecodedUpsert {
                stage: PreviewStage::Refined,
                buffer_tag: StripPreviewBufferTag::SdrDeferredPlaceholder,
                logical_size: Some((512, 256)),
                path: &strip_test_path(0),
                ctx: &ctx,
                strip_max_side: 128,
                strip_max_side_used: Some(128),
            },
        );
        assert!(cache.contains(&strip_test_path(0)));
        assert_eq!(
            cache.preview_dimensions(&strip_test_path(0)),
            Some((128, 64))
        );
    }

    #[test]
    fn strip_cache_reuses_texture_for_same_size_quality_upgrade() {
        let ctx = egui::Context::default();
        let mut cache = DirectoryTreeStripCache::default();
        let initial = DecodedImage::new(64, 64, vec![120; 64 * 64 * 4]);
        cache.upsert_from_decoded(
            &initial,
            StripDecodedUpsert {
                stage: PreviewStage::Initial,
                buffer_tag: StripPreviewBufferTag::PreloadSdrFallback,
                logical_size: Some((64, 64)),
                path: &strip_test_path(0),
                ctx: &ctx,
                strip_max_side: 128,
                strip_max_side_used: Some(128),
            },
        );
        let first_id = cache
            .textures()
            .get(&strip_test_path(0))
            .expect("initial texture")
            .id();

        let refined = DecodedImage::new(64, 64, vec![220; 64 * 64 * 4]);
        cache.upsert_from_decoded(
            &refined,
            StripDecodedUpsert {
                stage: PreviewStage::Refined,
                buffer_tag: StripPreviewBufferTag::StripDecodedPixels,
                logical_size: Some((64, 64)),
                path: &strip_test_path(0),
                ctx: &ctx,
                strip_max_side: 128,
                strip_max_side_used: Some(128),
            },
        );

        let second_id = cache
            .textures()
            .get(&strip_test_path(0))
            .expect("refined texture")
            .id();
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
            &decoded,
            StripDecodedUpsert {
                stage: PreviewStage::Refined,
                buffer_tag: StripPreviewBufferTag::StripDecodedPixels,
                logical_size: Some((640, 320)),
                path: &strip_test_path(0),
                ctx: &ctx,
                strip_max_side: 128,
                strip_max_side_used: Some(128),
            },
        );
        assert!(cache.contains(&strip_test_path(0)));
        cache.clear_gpu_textures();
        assert!(!cache.contains(&strip_test_path(0)));
        assert_eq!(
            cache.logical_sizes().get(&strip_test_path(0)),
            Some(&(640, 320))
        );
    }

    #[test]
    fn lru_eviction_keeps_logical_sizes_for_regeneration() {
        let ctx = egui::Context::default();
        let mut cache = DirectoryTreeStripCache::default();
        for index in 0..DIRECTORY_TREE_STRIP_CACHE_MAX + 1 {
            let decoded = DecodedImage::new(8, 8, vec![128; 8 * 8 * 4]);
            cache.upsert_from_decoded(
                &decoded,
                StripDecodedUpsert {
                    stage: PreviewStage::Initial,
                    buffer_tag: StripPreviewBufferTag::StripDecodedPixels,
                    logical_size: Some((800, 600)),
                    path: &strip_test_path(index),
                    ctx: &ctx,
                    strip_max_side: 128,
                    strip_max_side_used: Some(128),
                },
            );
        }
        assert!(!cache.contains(&strip_test_path(0)));
        assert_eq!(
            cache.logical_sizes().get(&strip_test_path(0)),
            Some(&(800, 600))
        );
        assert!(cache.contains(&strip_test_path(DIRECTORY_TREE_STRIP_CACHE_MAX)));
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
            &handle,
            PreviewStage::Refined,
            StripPreviewBufferTag::MainWindowTextureCacheSdr,
            Some((80, 80)),
            &strip_test_path(0),
        );
        assert!(cache.contains(&strip_test_path(0)));
        assert_eq!(cache.gpu_revision(), 1);
    }

    #[test]
    fn project_index_maps_maps_paths_to_current_indices() {
        let ctx = egui::Context::default();
        let mut cache = DirectoryTreeStripCache::default();
        let decoded = DecodedImage::new(16, 16, vec![7; 16 * 16 * 4]);
        cache.upsert_from_decoded(
            &decoded,
            StripDecodedUpsert {
                stage: PreviewStage::Refined,
                buffer_tag: StripPreviewBufferTag::StripDecodedPixels,
                logical_size: Some((160, 160)),
                path: &strip_test_path(0),
                ctx: &ctx,
                strip_max_side: 128,
                strip_max_side_used: Some(128),
            },
        );
        // Path that lives at index 0 in the cache now sits at index 3 in the list.
        let mut path_to_index = HashMap::new();
        path_to_index.insert(strip_test_path(0), 3usize);
        let projected = cache.project_index_maps(&path_to_index);
        assert!(projected.textures.iter().any(|(i, _)| *i == 3));
        assert!(
            projected
                .logical_sizes
                .iter()
                .any(|(i, s)| *i == 3 && *s == (160, 160))
        );
        assert!(
            projected
                .buffer_tags
                .iter()
                .any(|(i, t)| *i == 3 && *t == StripPreviewBufferTag::StripDecodedPixels)
        );
        // A path not present in the list is dropped from the projection.
        let empty = cache.project_index_maps(&HashMap::new());
        assert!(empty.textures.is_empty());
    }
}
