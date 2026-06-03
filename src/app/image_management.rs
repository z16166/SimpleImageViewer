use crate::app::{
    AnimationPlayback, FileOpResult, ImageViewerApp, PendingAnimUpload, TransitionStyle,
};
use crate::app::{MAX_PRELOAD_BACKWARD, MAX_PRELOAD_FORWARD};
use crate::loader::{
    DecodedImage, ImageData, LoadResult, LoaderOutput, PixelPlaneKind, PreviewPlane, PreviewResult,
    RenderShape as LoadedRenderShape, TileResult, source_key_for_path,
};
use crate::scanner::{self, ScanMessage};
use crate::tile_cache::TileManager;
use eframe::egui::{self, ColorImage, TextureOptions, Vec2};
use rand::seq::SliceRandom;
use rust_i18n::t;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn has_startup_target(
    initial_image: Option<&PathBuf>,
    resume_last_image: bool,
    last_viewed_image: Option<&PathBuf>,
) -> bool {
    initial_image.is_some() || (resume_last_image && last_viewed_image.is_some())
}

fn preserve_current_tile_manager_for_navigation(
    current_index: usize,
    target_index: usize,
    tile_manager: &mut Option<TileManager>,
    prefetched_tiles: &mut HashMap<usize, TileManager>,
) {
    if current_index != target_index {
        if let Some(tm) = tile_manager.take() {
            prefetched_tiles.insert(current_index, tm);
        }
    }
}

fn should_upload_tiled_bootstrap_preview(
    cache_contains_index: bool,
    cached_preview_max_side: Option<u32>,
    bootstrap_max_side: u32,
) -> bool {
    should_cache_tiled_sdr_preview(
        cache_contains_index,
        true,
        cached_preview_max_side,
        bootstrap_max_side,
    )
}

fn should_cache_tiled_sdr_preview(
    cache_contains_index: bool,
    is_preview_placeholder: bool,
    cached_preview_max_side: Option<u32>,
    preview_max_side: u32,
) -> bool {
    if !cache_contains_index {
        return true;
    }
    if !is_preview_placeholder {
        return false;
    }
    cached_preview_max_side.map_or(true, |cached_max| preview_max_side > cached_max)
}

fn should_cache_tiled_hdr_preview(
    cached_preview_max_side: Option<u32>,
    preview_max_side: u32,
) -> bool {
    cached_preview_max_side.map_or(true, |cached_max| preview_max_side > cached_max)
}

fn cache_hdr_tiled_preview_state(
    idx: usize,
    current_index: usize,
    cache: &mut HashMap<usize, Arc<crate::hdr::types::HdrImageBuffer>>,
    current: &mut Option<crate::app::CurrentHdrImage>,
    preview: Option<Arc<crate::hdr::types::HdrImageBuffer>>,
    file_name: &str,
) {
    let Some(preview) = preview else {
        return;
    };
    let preview_max_side = preview.width.max(preview.height);
    let cached_preview_max_side = cache
        .get(&idx)
        .map(|cached| cached.width.max(cached.height));
    if !should_cache_tiled_hdr_preview(cached_preview_max_side, preview_max_side) {
        log::debug!(
            "[App] [{}] Ignored HDR tiled preview for index {} ({}x{}), cached max side {:?}",
            file_name,
            idx,
            preview.width,
            preview.height,
            cached_preview_max_side
        );
        return;
    }

    log::info!(
        "[App] [{}] Cached HDR tiled preview for index {} ({}x{}, cached max side {:?})",
        file_name,
        idx,
        preview.width,
        preview.height,
        cached_preview_max_side
    );
    cache.insert(idx, Arc::clone(&preview));
    if idx == current_index {
        *current = Some(crate::app::CurrentHdrImage::new(idx, preview));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AssetUpdateKind {
    ImageLoaded,
    PreviewUpgraded,
    TileReady,
    RefinedFullPlane,
}

fn should_request_repaint_for_asset_update(
    kind: AssetUpdateKind,
    is_current: bool,
    update_requests_repaint: bool,
) -> bool {
    match kind {
        AssetUpdateKind::ImageLoaded | AssetUpdateKind::PreviewUpgraded => is_current,
        AssetUpdateKind::TileReady => is_current && update_requests_repaint,
        AssetUpdateKind::RefinedFullPlane => is_current,
    }
}

fn image_file_size_pairs_with_missing_sizes_as_zero(
    image_files: Vec<PathBuf>,
    file_byte_len_by_index: Vec<u64>,
) -> Vec<(PathBuf, u64)> {
    image_files
        .into_iter()
        .zip(
            file_byte_len_by_index
                .into_iter()
                .chain(std::iter::repeat(0)),
        )
        .collect()
}

fn build_tiled_manager_with_best_preview(
    index: usize,
    generation: u64,
    source: Arc<dyn crate::loader::TiledImageSource>,
    cached_handle: Option<egui::TextureHandle>,
) -> TileManager {
    let mut tm = TileManager::with_source(index, generation, source);
    tm.preview_texture = cached_handle;
    tm
}

fn current_hdr_tiled_preview_matches_index(
    current: Option<&crate::app::CurrentHdrImage>,
    index: usize,
) -> bool {
    current.is_some_and(|current| current.image_for_index(index).is_some())
}

fn should_reset_transition_when_source_texture_missing(has_source_texture: bool) -> bool {
    !has_source_texture
}

fn transition_preroll_duration(transition_ms: u32) -> Duration {
    if transition_ms == 0 {
        return Duration::ZERO;
    }
    // Avoid the first-frame "stationary old frame" flash by starting animation
    // slightly in-progress.
    let max_ms = (transition_ms as u64).saturating_sub(1);
    Duration::from_millis(16_u64.min(max_ms))
}

fn can_start_pending_transition(
    target: Option<usize>,
    current_index: usize,
    target_is_render_ready: bool,
) -> bool {
    target == Some(current_index) && target_is_render_ready
}

fn should_start_transition_immediately(target_has_texture: bool, has_source_texture: bool) -> bool {
    target_has_texture && has_source_texture
}

fn target_is_render_ready(
    has_sdr_texture: bool,
    has_hdr_plane: bool,
    sdr_fallback_is_placeholder: bool,
) -> bool {
    if has_hdr_plane {
        return true;
    }
    has_sdr_texture && !sdr_fallback_is_placeholder
}

fn navigation_is_forward(current_index: usize, target_index: usize, total: usize) -> bool {
    if total == 0 || current_index == target_index {
        return true;
    }
    let forward_steps = (target_index + total - current_index) % total;
    let backward_steps = (current_index + total - target_index) % total;
    forward_steps <= backward_steps
}

fn source_key_matches_index(
    image_files: &[PathBuf],
    index: usize,
    source_key: crate::loader::SourceKey,
) -> bool {
    image_files
        .get(index)
        .is_some_and(|path| source_key_for_path(path) == source_key)
}

fn output_mode_is_hdr(mode: crate::hdr::types::HdrOutputMode) -> bool {
    mode != crate::hdr::types::HdrOutputMode::SdrToneMapped
}

fn output_mode_crosses_hdr_sdr_boundary(
    previous: crate::hdr::types::HdrOutputMode,
    next: crate::hdr::types::HdrOutputMode,
) -> bool {
    output_mode_is_hdr(previous) != output_mode_is_hdr(next)
}

fn select_transition_source<T: Clone>(
    current: Option<T>,
    current_has_placeholder_fallback: bool,
    previous: Option<T>,
) -> Option<T> {
    if !current_has_placeholder_fallback && current.is_some() {
        current
    } else {
        previous
    }
}

fn select_transition_source_texture(
    current_source_texture: Option<egui::TextureHandle>,
    current_has_placeholder_fallback: bool,
    previous_transition_source: Option<egui::TextureHandle>,
) -> Option<egui::TextureHandle> {
    select_transition_source(
        current_source_texture,
        current_has_placeholder_fallback,
        previous_transition_source,
    )
}

fn select_transition_source_hdr(
    current_hdr_image: Option<Arc<crate::hdr::types::HdrImageBuffer>>,
    current_has_placeholder_fallback: bool,
    previous_transition_hdr_image: Option<Arc<crate::hdr::types::HdrImageBuffer>>,
) -> Option<Arc<crate::hdr::types::HdrImageBuffer>> {
    select_transition_source(
        current_hdr_image,
        current_has_placeholder_fallback,
        previous_transition_hdr_image,
    )
}

fn invalidate_tile_manager_requests_for_view_change(
    tile_manager: &mut Option<TileManager>,
) -> bool {
    if let Some(tm) = tile_manager {
        tm.generation = tm.generation.wrapping_add(1);
        tm.pending_tiles.clear();
        true
    } else {
        false
    }
}

const HDR_CAPACITY_STALE_EPSILON: f32 = 0.001;

/// True when an HDR load result used a different Ultra HDR decode capacity than the viewer now expects.
pub(crate) fn hdr_load_result_capacity_is_stale(
    load_result: &LoadResult,
    current_ultra_hdr_decode_capacity: f32,
) -> bool {
    load_result.ultra_hdr_capacity_sensitive
        && matches!(
            &load_result.result,
            Ok(crate::loader::ImageData::Hdr { .. }
                | crate::loader::ImageData::HdrTiled { .. }
                | crate::loader::ImageData::HdrAnimated(_))
        )
        && (load_result.target_hdr_capacity - current_ultra_hdr_decode_capacity).abs()
            > HDR_CAPACITY_STALE_EPSILON
}

enum ImageInstallPlan<'a> {
    StaticSdr {
        decoded: &'a DecodedImage,
    },
    StaticHdr {
        hdr: Arc<crate::hdr::types::HdrImageBuffer>,
        fallback: &'a DecodedImage,
        ultra_hdr_capacity_sensitive: bool,
    },
    Tiled {
        source: Arc<dyn crate::loader::TiledImageSource>,
        hdr_source: Option<Arc<dyn crate::hdr::tiled::HdrTiledSource>>,
        hdr_preview: Option<Arc<crate::hdr::types::HdrImageBuffer>>,
        hdr_sdr_fallback: bool,
        ultra_hdr_capacity_sensitive: bool,
    },
    Animated {
        frames: &'a [crate::loader::AnimationFrame],
    },
    HdrAnimated {
        frames: &'a [crate::loader::HdrAnimationFrame],
        ultra_hdr_capacity_sensitive: bool,
    },
    Error {
        error: &'a str,
    },
}

impl<'a> ImageInstallPlan<'a> {
    fn from_load_result(load_result: &'a LoadResult) -> Self {
        let _preview_stage = load_result.preview_bundle.stage();
        let image_data = match &load_result.result {
            Ok(img) => img,
            Err(error) => {
                return Self::Error {
                    error: error.as_str(),
                };
            }
        };

        match image_data.preferred_render_shape() {
            LoadedRenderShape::Static if image_data.has_plane(PixelPlaneKind::Hdr) => {
                let Some(hdr) = image_data.static_hdr() else {
                    return Self::Error {
                        error: "Static HDR image is missing the HDR plane",
                    };
                };
                let Some(fallback) = image_data.static_sdr() else {
                    return Self::Error {
                        error: "Static HDR image is missing the SDR fallback plane",
                    };
                };
                Self::StaticHdr {
                    hdr: Arc::new(hdr.clone()),
                    fallback,
                    ultra_hdr_capacity_sensitive: load_result.ultra_hdr_capacity_sensitive,
                }
            }
            LoadedRenderShape::Static => {
                let Some(decoded) = image_data.static_sdr() else {
                    return Self::Error {
                        error: "Static SDR image is missing the SDR plane",
                    };
                };
                Self::StaticSdr { decoded }
            }
            LoadedRenderShape::Tiled => {
                let Some(source) = image_data.tiled_sdr_source() else {
                    return Self::Error {
                        error: "Tiled image is missing the SDR source",
                    };
                };
                let hdr_source = image_data.tiled_hdr_source().cloned();
                let hdr_preview = load_result
                    .preview_bundle
                    .plane(PixelPlaneKind::Hdr)
                    .and_then(|plane| {
                        let _kind = plane.kind();
                        let _dimensions = plane.dimensions();
                        match plane {
                            PreviewPlane::Hdr(preview) => Some(preview),
                            PreviewPlane::Sdr(_) => None,
                        }
                    });
                let hdr_sdr_fallback = hdr_source.is_some() || source.is_hdr_sdr_fallback();

                Self::Tiled {
                    source: Arc::clone(source),
                    hdr_source,
                    hdr_preview,
                    hdr_sdr_fallback,
                    ultra_hdr_capacity_sensitive: load_result.ultra_hdr_capacity_sensitive,
                }
            }
            LoadedRenderShape::Animated => match image_data {
                ImageData::Animated(frames) => Self::Animated { frames },
                ImageData::HdrAnimated(frames) => Self::HdrAnimated {
                    frames,
                    ultra_hdr_capacity_sensitive: load_result.ultra_hdr_capacity_sensitive,
                },
                _ => unreachable!("animated render shape is only emitted by animated image data"),
            },
        }
    }
}

impl ImageViewerApp {
    pub(crate) fn trigger_current_hdr_fallback_refinement_if_needed(&mut self) {
        if self
            .hdr_placeholder_fallback_indices
            .contains(&self.current_index)
        {
            if self
                .hdr_in_flight_fallback_refinements
                .contains(&self.current_index)
            {
                return;
            }
            if let Some(hdr) = self.hdr_image_cache.get(&self.current_index).cloned() {
                let source_key = source_key_for_path(&self.image_files[self.current_index]);
                self.hdr_in_flight_fallback_refinements
                    .insert(self.current_index);
                self.loader.trigger_hdr_sdr_fallback_refinement(
                    self.current_index,
                    self.generation,
                    hdr,
                    source_key,
                );
            }
        }
    }

    pub(crate) fn invalidate_random_slideshow_order(&mut self) {
        self.random_slideshow_order_ready = false;
    }

    fn shuffle_current_image_list_preserving_pairs(&mut self) {
        let mut combined = image_file_size_pairs_with_missing_sizes_as_zero(
            std::mem::take(&mut self.image_files),
            std::mem::take(&mut self.file_byte_len_by_index),
        );
        combined.shuffle(&mut rand::thread_rng());
        let (paths, sizes): (Vec<_>, Vec<_>) = combined.into_iter().unzip();
        self.image_files = paths;
        self.file_byte_len_by_index = sizes;
    }

    fn clear_index_keyed_state_after_list_reorder(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.loader.set_generation(self.generation);
        self.loader.cancel_all();
        self.texture_cache.clear_all();
        self.clear_hdr_image_state();
        self.prefetched_tiles.clear();
        self.animation = None;
        self.animation_cache.clear();
        self.pending_anim_frames = None;
        self.tile_manager = None;
        self.current_image_res = None;
        self.prev_texture = None;
        self.prev_hdr_image = None;
        self.transition_start = None;
        self.prefetch_prev_generation = None;
        crate::tile_cache::PIXEL_CACHE.lock().clear();
    }

    fn relocate_index_keyed_cache(&mut self, from: usize, to: usize) {
        if from == to {
            return;
        }
        // 1. Texture cache
        self.texture_cache.relocate(from, to);

        // 2. HDR caches
        if let Some(hdr) = self.hdr_image_cache.remove(&from) {
            self.hdr_image_cache.insert(to, hdr);
        }
        if let Some(src) = self.hdr_tiled_source_cache.remove(&from) {
            self.hdr_tiled_source_cache.insert(to, src);
        }
        if let Some(prev) = self.hdr_tiled_preview_cache.remove(&from) {
            self.hdr_tiled_preview_cache.insert(to, prev);
        }

        // 3. Fallback sets
        if self.hdr_sdr_fallback_indices.remove(&from) {
            self.hdr_sdr_fallback_indices.insert(to);
        }
        if self.hdr_placeholder_fallback_indices.remove(&from) {
            self.hdr_placeholder_fallback_indices.insert(to);
        }
        if self.hdr_in_flight_fallback_refinements.remove(&from) {
            self.hdr_in_flight_fallback_refinements.insert(to);
        }
        if self.ultra_hdr_capacity_sensitive_indices.remove(&from) {
            self.ultra_hdr_capacity_sensitive_indices.insert(to);
        }

        // 4. Deferred uploads
        if let Some(upload) = self.deferred_sdr_uploads.remove(&from) {
            self.deferred_sdr_uploads.insert(to, upload);
        }

        // 5. Prefetched tiles / animations
        if let Some(mut tiles) = self.prefetched_tiles.remove(&from) {
            tiles.image_index = to;
            self.prefetched_tiles.insert(to, tiles);
        }
        if let Some(mut anim) = self.animation_cache.remove(&from) {
            anim.image_index = to;
            self.animation_cache.insert(to, anim);
        }
        if let Some(ref mut anim) = self.animation {
            if anim.image_index == from {
                anim.image_index = to;
            }
        }

        // 6. Current HDR image states
        if let Some(ref mut curr) = self.current_hdr_image {
            if curr.index == from {
                curr.index = to;
            }
        }
        if let Some(ref mut curr) = self.current_hdr_tiled_image {
            if curr.index == from {
                curr.index = to;
            }
        }
        if let Some(ref mut curr) = self.current_hdr_tiled_preview {
            if curr.index == from {
                curr.index = to;
            }
        }

        // 7. Tile manager index
        if let Some(ref mut manager) = self.tile_manager {
            if manager.image_index == from {
                manager.image_index = to;
            }
        }

        // 8. Global tile pixel cache
        crate::tile_cache::PIXEL_CACHE
            .lock()
            .relocate_image(from, to);
    }

    fn clear_index_keyed_state_after_list_reorder_except_index(&mut self, except_idx: usize) {
        self.generation = self.generation.wrapping_add(1);
        self.loader.set_generation(self.generation);
        self.loader.cancel_all();

        // 1. Texture cache: remove everything except except_idx
        let to_remove_tex: Vec<usize> = self
            .texture_cache
            .textures
            .keys()
            .copied()
            .filter(|&idx| idx != except_idx)
            .collect();
        for idx in to_remove_tex {
            self.texture_cache.remove(idx);
        }

        // 2. HDR caches
        let to_remove_hdr: Vec<usize> = self
            .hdr_image_cache
            .keys()
            .copied()
            .filter(|&idx| idx != except_idx)
            .collect();
        for idx in to_remove_hdr {
            self.hdr_image_cache.remove(&idx);
        }

        let to_remove_tiled_source: Vec<usize> = self
            .hdr_tiled_source_cache
            .keys()
            .copied()
            .filter(|&idx| idx != except_idx)
            .collect();
        for idx in to_remove_tiled_source {
            self.hdr_tiled_source_cache.remove(&idx);
        }

        let to_remove_tiled_preview: Vec<usize> = self
            .hdr_tiled_preview_cache
            .keys()
            .copied()
            .filter(|&idx| idx != except_idx)
            .collect();
        for idx in to_remove_tiled_preview {
            self.hdr_tiled_preview_cache.remove(&idx);
        }

        self.hdr_sdr_fallback_indices
            .retain(|&idx| idx == except_idx);
        self.hdr_placeholder_fallback_indices
            .retain(|&idx| idx == except_idx);
        self.hdr_in_flight_fallback_refinements
            .retain(|&idx| idx == except_idx);
        self.deferred_sdr_uploads
            .retain(|&idx, _| idx == except_idx);
        self.ultra_hdr_capacity_sensitive_indices
            .retain(|&idx| idx == except_idx);

        // 3. Prefetched tiles, animation cache
        self.prefetched_tiles.retain(|&idx, _| idx == except_idx);
        self.animation_cache.retain(|&idx, _| idx == except_idx);

        // 4. Other states
        if let Some(ref anim) = self.animation {
            if anim.image_index != except_idx {
                self.animation = None;
            }
        }
        self.pending_anim_frames = None;

        // Keep self.tile_manager if its index matches except_idx
        if let Some(ref manager) = self.tile_manager {
            if manager.image_index != except_idx {
                self.tile_manager = None;
            }
        }

        self.prev_texture = None;
        self.prev_hdr_image = None;
        self.transition_start = None;
        self.pending_transition_target = None;
        self.prefetch_prev_generation = None;

        // Clear only non-except_idx entries from the global tile pixel cache
        crate::tile_cache::PIXEL_CACHE
            .lock()
            .remove_images_except(except_idx);
    }

    pub(crate) fn finish_refresh_scan_state(&mut self) {
        if self.refresh_scan_in_progress {
            self.refresh_scan_in_progress = false;
            self.refresh_anchor_path = None;
            if self.refresh_scan_slideshow_was_playing {
                self.slideshow_paused = false;
                self.last_switch_time = Instant::now();
                self.refresh_scan_slideshow_was_playing = false;
            }
            log::info!("[RefreshFileList] Refresh scan finished/cleaned up");
        }
    }

    pub(crate) fn shuffle_slideshow_order_to_first(&mut self) {
        if self.image_files.is_empty() {
            self.random_slideshow_order_ready = false;
            return;
        }

        self.shuffle_current_image_list_preserving_pairs();
        self.clear_index_keyed_state_after_list_reorder();

        self.current_index = 0;
        self.current_rotation = 0;
        self.zoom_factor = 1.0;
        self.pan_offset = Vec2::ZERO;
        self.error_message = None;
        self.is_font_error = false;
        self.random_slideshow_order_ready = true;
        self.last_switch_time = Instant::now();

        self.loader.request_load(
            self.current_index,
            self.generation,
            self.image_files[self.current_index].clone(),
            self.settings.raw_high_quality,
        );
        self.schedule_preloads(true);
    }

    /// True when the active tile pyramid belongs to the image at [`Self::current_index`].
    ///
    /// If [`Self::tile_manager`] is `Some` but its [`TileManager::image_index`] does not
    /// match the current folder index, the pyramid is stale (e.g. a late install race or
    /// a path that forgot to drop tiles). The UI may still draw via the standard/animation
    /// path using `texture_cache`, but HDR OSD and render-plan routing must treat the view
    /// as non-tiled — otherwise `current_hdr_render_path` returns `None` and the HDR/SDR
    /// status line disappears until the stale manager is cleared.
    pub(crate) fn tiled_canvas_matches_current_index(&self) -> bool {
        self.tile_manager
            .as_ref()
            .is_some_and(|tm| tm.image_index == self.current_index)
    }

    pub(crate) fn invalidate_tile_requests_for_view_change(&mut self) {
        if invalidate_tile_manager_requests_for_view_change(&mut self.tile_manager) {
            self.loader.flush_tile_queue();
        }
    }

    pub(crate) fn effective_ultra_hdr_decode_capacity(&self) -> f32 {
        crate::app::ultra_hdr_decode_capacity_for_output_mode(
            self.effective_hdr_tone_map_settings(),
            self.hdr_capabilities.output_mode,
            self.effective_hdr_monitor_selection().as_ref(),
        )
    }

    pub(crate) fn sync_hdr_tone_map_settings(&mut self) {
        let tone = self.effective_hdr_tone_map_settings();
        self.hdr_renderer.tone_map = tone;
        self.loader.set_hdr_tone_map_settings(tone);
    }

    pub(crate) fn refresh_ultra_hdr_decode_capacity(&mut self, ctx: &egui::Context) {
        const CAPACITY_EPSILON: f32 = 0.001;
        let next_output_mode = self.hdr_capabilities.output_mode;
        let next_capacity = self.effective_ultra_hdr_decode_capacity();
        let crosses_hdr_sdr_boundary = output_mode_crosses_hdr_sdr_boundary(
            self.ultra_hdr_decode_output_mode,
            next_output_mode,
        );
        if (next_capacity - self.ultra_hdr_decode_capacity).abs() <= CAPACITY_EPSILON
            && !crosses_hdr_sdr_boundary
        {
            return;
        }

        let previous_capacity = self.ultra_hdr_decode_capacity;
        let previous_output_mode = self.ultra_hdr_decode_output_mode;
        self.ultra_hdr_decode_capacity = next_capacity;
        self.ultra_hdr_decode_output_mode = next_output_mode;
        self.loader.set_hdr_target_capacity(next_capacity);
        self.loader
            .set_hdr_tone_map_settings(self.effective_hdr_tone_map_settings());
        log::info!(
            "[HDR] ultra_hdr_decode_capacity changed {:.3} -> {:.3}; output_mode {:?} -> {:?}",
            previous_capacity,
            next_capacity,
            previous_output_mode,
            next_output_mode
        );

        if crosses_hdr_sdr_boundary {
            log::info!(
                "[HDR] HDR/SDR output boundary changed; invalidating in-flight/preload state and reloading current image"
            );
            self.reload_current_after_hdr_sdr_output_boundary_change();
            ctx.request_repaint();
            return;
        }

        self.invalidate_ultra_hdr_capacity_sensitive_state(ctx);
    }

    fn invalidate_ultra_hdr_capacity_sensitive_state(&mut self, ctx: &egui::Context) {
        let static_hdr_indices: std::collections::HashSet<_> =
            self.hdr_image_cache.keys().copied().collect();
        let hdr_tiled_indices: std::collections::HashSet<_> =
            self.hdr_tiled_source_cache.keys().copied().collect();
        let refresh = crate::app::plan_ultra_hdr_capacity_refresh(
            self.current_index,
            &static_hdr_indices,
            &hdr_tiled_indices,
            &self.hdr_sdr_fallback_indices,
            &self.ultra_hdr_capacity_sensitive_indices,
        );

        // Always cancel in-flight loads when capacity changes.  The original guard
        // only cancelled when there were cached HDR images to invalidate, but during
        // early startup the caches are empty while workers are already running with the
        // *old* capacity snapshot.  Those stale workers must be evicted so that the
        // re-scheduled preloads below use the updated capacity.
        self.loader.cancel_all();
        self.clear_preloaded_assets_for_capacity_change();

        if refresh.indices_to_invalidate.is_empty() {
            // No cached HDR images to evict, but we still need to reschedule preloads
            // so they pick up the new capacity (e.g. monitor probe completed mid-load).
            if !self.image_files.is_empty() {
                self.schedule_preloads(true);
            }
            ctx.request_repaint();
            return;
        }

        for idx in &refresh.indices_to_invalidate {
            self.texture_cache.remove(*idx);
            self.prefetched_tiles.remove(idx);
            crate::tile_cache::PIXEL_CACHE.lock().remove_image(*idx);
            self.remove_hdr_image_index(*idx);
        }

        if refresh.reload_current && !self.image_files.is_empty() {
            self.generation = self.generation.wrapping_add(1);
            self.loader.set_generation(self.generation);
            self.tile_manager = None;
            self.current_image_res = None;
            self.animation = None;
            self.loader.request_load(
                self.current_index,
                self.generation,
                self.image_files[self.current_index].clone(),
                self.settings.raw_high_quality,
            );
        }

        if crate::app::capacity_refresh_should_reschedule_preloads(&refresh) {
            self.schedule_preloads(true);
        }
        ctx.request_repaint();
    }

    pub(crate) fn clear_hdr_image_state(&mut self) {
        self.hdr_image_cache.clear();
        self.hdr_tiled_source_cache.clear();
        self.hdr_tiled_preview_cache.clear();
        self.hdr_sdr_fallback_indices.clear();
        self.hdr_placeholder_fallback_indices.clear();
        self.hdr_in_flight_fallback_refinements.clear();
        self.deferred_sdr_uploads.clear();
        self.ultra_hdr_capacity_sensitive_indices.clear();
        self.current_hdr_image = None;
        self.current_hdr_tiled_image = None;
        self.current_hdr_tiled_preview = None;
    }

    pub(crate) fn remove_hdr_image_index(&mut self, index: usize) {
        self.hdr_image_cache.remove(&index);
        self.hdr_tiled_source_cache.remove(&index);
        self.hdr_tiled_preview_cache.remove(&index);
        self.hdr_sdr_fallback_indices.remove(&index);
        self.hdr_placeholder_fallback_indices.remove(&index);
        self.hdr_in_flight_fallback_refinements.remove(&index);
        self.deferred_sdr_uploads.remove(&index);
        self.ultra_hdr_capacity_sensitive_indices.remove(&index);
        if self
            .current_hdr_image
            .as_ref()
            .is_some_and(|current| current.image_for_index(index).is_some())
        {
            self.current_hdr_image = None;
        }
        if self
            .current_hdr_tiled_image
            .as_ref()
            .is_some_and(|current| current.source_for_index(index).is_some())
        {
            self.current_hdr_tiled_image = None;
        }
        if current_hdr_tiled_preview_matches_index(self.current_hdr_tiled_preview.as_ref(), index) {
            self.current_hdr_tiled_preview = None;
        }
    }

    /// First HDR still for `index` from static cache, completed animation cache, or in-flight
    /// deferred animation uploads.
    pub(crate) fn first_cached_hdr_still_for_index(
        &self,
        index: usize,
    ) -> Option<Arc<crate::hdr::types::HdrImageBuffer>> {
        first_cached_hdr_still_for_index(
            &self.hdr_image_cache,
            &self.animation_cache,
            self.pending_anim_frames.as_ref(),
            index,
        )
    }

    /// Returns the best available HDR still for `index`, falling back to the tiled
    /// downsampled preview or in-memory current preview when no full-resolution static HDR entry exists.
    ///
    /// Used at navigation time to populate `prev_hdr_image` so that tiled HDR images
    /// can serve as the background during image transitions.
    pub(crate) fn first_cached_hdr_or_tiled_preview_for_index(
        &self,
        index: usize,
    ) -> Option<Arc<crate::hdr::types::HdrImageBuffer>> {
        first_cached_hdr_or_tiled_preview_for_index(
            &self.hdr_image_cache,
            &self.animation_cache,
            self.pending_anim_frames.as_ref(),
            &self.hdr_tiled_preview_cache,
            self.current_hdr_tiled_preview.as_ref(),
            index,
        )
    }

    fn handle_texture_cache_eviction(&mut self, evicted_idx: usize) {
        self.animation_cache.remove(&evicted_idx);
        self.remove_hdr_image_index(evicted_idx);
    }

    fn clear_preloaded_assets_for_capacity_change(&mut self) {
        let current = self.current_index;
        let mut indices = std::collections::BTreeSet::new();
        indices.extend(self.texture_cache.textures.keys().copied());
        indices.extend(self.prefetched_tiles.keys().copied());
        indices.extend(self.hdr_image_cache.keys().copied());
        indices.extend(self.hdr_tiled_source_cache.keys().copied());
        indices.extend(self.hdr_tiled_preview_cache.keys().copied());
        indices.extend(self.deferred_sdr_uploads.keys().copied());
        indices.extend(self.animation_cache.keys().copied());
        indices.extend(self.hdr_sdr_fallback_indices.iter().copied());
        indices.extend(self.hdr_placeholder_fallback_indices.iter().copied());
        indices.extend(self.ultra_hdr_capacity_sensitive_indices.iter().copied());

        let pixel_cache_indices: std::collections::HashSet<usize> = indices
            .iter()
            .copied()
            .filter(|&idx| idx != current)
            .collect();
        crate::tile_cache::PIXEL_CACHE
            .lock()
            .remove_images(&pixel_cache_indices);

        for idx in indices {
            if idx == current {
                continue;
            }
            self.texture_cache.remove(idx);
            self.prefetched_tiles.remove(&idx);
            self.animation_cache.remove(&idx);
            self.deferred_sdr_uploads.remove(&idx);
            self.remove_hdr_image_index(idx);
        }
    }

    fn reload_current_after_hdr_sdr_output_boundary_change(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.loader.set_generation(self.generation);
        self.loader.cancel_all();
        self.clear_preloaded_assets_for_capacity_change();

        if self.image_files.is_empty() {
            return;
        }

        let idx = self.current_index;
        self.texture_cache.remove(idx);
        self.prefetched_tiles.remove(&idx);
        crate::tile_cache::PIXEL_CACHE.lock().remove_image(idx);
        self.remove_hdr_image_index(idx);
        self.tile_manager = None;
        self.current_image_res = None;
        self.animation = None;
        self.pending_anim_frames = None;
        self.prev_texture = None;
        self.prev_hdr_image = None;
        self.transition_start = None;
        self.pending_transition_target = None;
        self.prefetch_prev_generation = None;

        self.loader.request_load(
            idx,
            self.generation,
            self.image_files[idx].clone(),
            self.settings.raw_high_quality,
        );
    }

    /// Circular index distance used for preload tile / CPU cache retention.
    const PREFETCH_WINDOW_DISTANCE: usize = 2;

    fn evict_distant_prefetch_caches(&mut self) {
        let len = self.image_files.len();
        let within_window = |idx: usize| {
            prefetch_window_contains(self.current_index, len, idx, Self::PREFETCH_WINDOW_DISTANCE)
        };

        // Track distant indices from prefetched_tiles eviction so we can clean their textures & metadata too
        let mut distant_indices = Vec::new();

        self.prefetched_tiles.retain(|&idx, _| {
            let keep = within_window(idx);
            if !keep {
                distant_indices.push(idx);
            }
            keep
        });

        self.deferred_sdr_uploads
            .retain(|&idx, _| within_window(idx));

        // Gather distant static HDR images
        let distant_hdr: Vec<usize> = self
            .hdr_image_cache
            .keys()
            .copied()
            .filter(|&idx| !within_window(idx))
            .collect();
        distant_indices.extend(distant_hdr);

        // Gather distant tiled HDR image sources. This ensures tiled HDR sources (like gain-map JPEGs)
        // are correctly evicted and do not leak in hdr_tiled_source_cache, which would cause
        // subsequent visits to trigger has_loaded_asset() but fail to construct the TileManager,
        // hanging the UI on loading.
        let distant_tiled_hdr: Vec<usize> = self
            .hdr_tiled_source_cache
            .keys()
            .copied()
            .filter(|&idx| !within_window(idx))
            .collect();
        distant_indices.extend(distant_tiled_hdr);

        // Deduplicate the combined list of indices to evict
        distant_indices.sort_unstable();
        distant_indices.dedup();

        for idx in distant_indices {
            self.texture_cache.remove(idx);
            self.remove_hdr_image_index(idx);
        }
    }

    // ------------------------------------------------------------------
    // Directory loading
    // ------------------------------------------------------------------

    pub(crate) fn open_directory_dialog(&mut self, frame: &eframe::Frame) {
        let mut dialog = super::rfd_parent::file_dialog_for_main_window(frame);
        if let Some(ref dir) = self.settings.last_image_dir.clone() {
            dialog = dialog.set_directory(dir);
        }
        if let Some(dir) = dialog.pick_folder() {
            self.load_directory(dir);
            self.queue_save();
        }
    }

    pub(crate) fn load_directory(&mut self, dir: PathBuf) {
        self.settings.last_image_dir = Some(dir.clone());
        self.invalidate_random_slideshow_order();
        self.image_files.clear();
        self.file_byte_len_by_index.clear();
        self.current_index = 0;
        self.texture_cache.clear_all();
        self.clear_hdr_image_state();
        self.animation_cache.clear();
        self.animation = None;
        self.prev_texture = None;
        self.prev_hdr_image = None;
        self.transition_start = None;
        self.tile_manager = None;
        self.prefetched_tiles.clear();
        crate::tile_cache::PIXEL_CACHE.lock().clear();
        self.current_image_res = None;
        self.loader.cancel_all();
        self.pan_offset = Vec2::ZERO;
        // Match `navigate_to` / file-open semantics: prior folder's manual zoom and rotation
        // must not carry over (fit scale is multiplied by `zoom_factor`, so a leftover ~7.5×
        // reads as ~232% OSD instead of ~31% on a fresh directory).
        self.zoom_factor = 1.0;
        self.current_rotation = 0;
        self.error_message = None;
        self.is_font_error = false;
        self.scanning = true;
        let dir_name = dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        self.status_message = t!("status.scanning", dir = dir_name).to_string();

        // Cancel previous scan if any
        if let Some(cancel) = self.scan_cancel.take() {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }

        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.scan_cancel = Some(Arc::clone(&cancel));

        let (tx, rx) = crossbeam_channel::unbounded();
        self.scan_rx = Some(rx);
        scanner::scan_directory(dir, self.settings.recursive, tx, cancel);
    }

    // ------------------------------------------------------------------
    // F5 Refresh file list
    // ------------------------------------------------------------------

    /// Refresh the image file list for the current directory (bound to F5).
    ///
    /// Compared to [`Self::load_directory`] this variant:
    /// - Guards against re-entry while a scan is already running.
    /// - Preserves the current image's GPU texture and tile manager so the
    ///   canvas keeps rendering during the scan instead of going blank.
    /// - Evicts all *other* preloaded caches (non-current texture entries,
    ///   HDR planes, animation frames, prefetched tiles, pixel tile cache)
    ///   so stale data doesn't linger with wrong index keys.
    /// - Does **not** reset zoom / pan / rotation.
    /// - Pauses slideshow playback and restores it when the scan finishes.
    pub(crate) fn start_refresh_file_list(&mut self) {
        // Guard: ignore if a directory scan or a previous refresh is already running.
        if self.scanning || self.refresh_scan_in_progress {
            log::debug!("[RefreshFileList] Ignored: scan already in progress");
            return;
        }
        let Some(dir) = self.settings.last_image_dir.clone() else {
            log::debug!("[RefreshFileList] Ignored: no directory configured");
            return;
        };

        // If the list is empty there is no "current file" to anchor to; fall back
        // to a regular directory load so the UI behaves like the first open.
        if self.image_files.is_empty() {
            self.load_directory(dir);
            return;
        }

        log::info!("[RefreshFileList] Starting refresh scan of {:?}", dir);

        // Save current file as anchor so it survives multi-batch scans,
        // and do not set initial_image so process_scan_results first-batch doesn't consume it.
        let current_file = self.image_files[self.current_index].clone();
        self.refresh_anchor_path = Some(current_file);
        self.initial_image = None;

        // Pause slideshow and record state for restoration on completion.
        let slideshow_was_playing = self.settings.auto_switch && !self.slideshow_paused;
        self.refresh_scan_slideshow_was_playing = slideshow_was_playing;
        if slideshow_was_playing {
            self.slideshow_paused = true;
        }

        self.refresh_scan_in_progress = true;

        // Cancel all in-flight background loads; the index space is about to change.
        self.loader.cancel_all();
        self.generation = self.generation.wrapping_add(1);
        self.loader.set_generation(self.generation);

        // ------------------------------------------------------------------
        // Selectively evict preload state: keep only the current image entry
        // so the canvas continues rendering while the scan runs.
        // ------------------------------------------------------------------
        let keep = self.current_index;

        // GPU texture cache: remove all entries except current.
        let to_remove_tex: Vec<usize> = self
            .texture_cache
            .textures
            .keys()
            .copied()
            .filter(|&idx| idx != keep)
            .collect();
        for idx in to_remove_tex {
            self.texture_cache.remove(idx);
        }

        // HDR caches: remove/retain all non-current entries using fine-grained cleanups
        // to avoid mixing redundant cleanup logic.
        let to_remove_hdr: Vec<usize> = self
            .hdr_image_cache
            .keys()
            .copied()
            .filter(|&idx| idx != keep)
            .collect();
        for idx in to_remove_hdr {
            self.hdr_image_cache.remove(&idx);
        }

        let to_remove_tiled_source: Vec<usize> = self
            .hdr_tiled_source_cache
            .keys()
            .copied()
            .filter(|&idx| idx != keep)
            .collect();
        for idx in to_remove_tiled_source {
            self.hdr_tiled_source_cache.remove(&idx);
        }

        let to_remove_tiled_preview: Vec<usize> = self
            .hdr_tiled_preview_cache
            .keys()
            .copied()
            .filter(|&idx| idx != keep)
            .collect();
        for idx in to_remove_tiled_preview {
            self.hdr_tiled_preview_cache.remove(&idx);
        }

        self.hdr_sdr_fallback_indices.retain(|&idx| idx == keep);
        self.hdr_placeholder_fallback_indices
            .retain(|&idx| idx == keep);
        self.hdr_in_flight_fallback_refinements
            .retain(|&idx| idx == keep);
        self.deferred_sdr_uploads.retain(|&idx, _| idx == keep);
        self.ultra_hdr_capacity_sensitive_indices
            .retain(|&idx| idx == keep);

        // Prefetched tile managers, animations: non-current only.
        self.prefetched_tiles.retain(|&idx, _| idx == keep);
        self.animation_cache.retain(|&idx, _| idx == keep);

        // Tile pixel cache: retain the current image's tiles so they don't have to be reloaded,
        // keeping consistency with clear_index_keyed_state_after_list_reorder_except_index.
        crate::tile_cache::PIXEL_CACHE
            .lock()
            .remove_images_except(keep);

        // Clear transition/pending state that references old indices.
        self.prev_texture = None;
        self.prev_hdr_image = None;
        self.transition_start = None;
        self.pending_transition_target = None;
        self.prefetch_prev_generation = None;

        // Pending animation upload is tied to a specific index; drop it.
        self.pending_anim_frames = None;

        // Keep self.tile_manager — it is keyed by image_index, and
        // tiled_canvas_matches_current_index() guards its usage, so it will
        // remain valid until the new current_index is resolved and a fresh
        // TileManager is installed.
        // Relocate all kept state to index 0 so that it matches current_index during scan.
        self.relocate_index_keyed_cache(keep, 0);

        // ------------------------------------------------------------------
        // Reset list state and start the background scan.
        // ------------------------------------------------------------------
        self.image_files.clear();
        self.file_byte_len_by_index.clear();
        self.current_index = 0;
        self.error_message = None;
        self.is_font_error = false;
        self.scanning = true;
        self.invalidate_random_slideshow_order();

        let dir_name = dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        self.status_message = t!("status.scanning", dir = dir_name).to_string();

        // Cancel any previous (already-running) scan.
        if let Some(cancel) = self.scan_cancel.take() {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.scan_cancel = Some(Arc::clone(&cancel));
        let (tx, rx) = crossbeam_channel::unbounded();
        self.scan_rx = Some(rx);
        scanner::scan_directory(dir, self.settings.recursive, tx, cancel);
    }

    // ------------------------------------------------------------------
    // Navigation
    // ------------------------------------------------------------------

    pub(crate) fn reload_current(&mut self) {
        if self.image_files.is_empty() {
            return;
        }

        // Only trigger reload if the current file is a RAW format, as the setting only affects RAW.
        let is_raw = self
            .image_files
            .get(self.current_index)
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .map(|ext| crate::raw_processor::is_raw_extension(ext))
            .unwrap_or(false);

        if !is_raw {
            return;
        }

        self.generation = self.generation.wrapping_add(1);
        self.loader.set_generation(self.generation);

        // Cancel all ongoing background tasks (like heavy RAW development)
        // to immediately free up resources for the new loading request.
        self.loader.cancel_all();

        // Clear current image from all relevant caches to force a fresh reload from disk
        self.texture_cache.remove(self.current_index);
        self.remove_hdr_image_index(self.current_index);
        self.prefetched_tiles.remove(&self.current_index);
        self.tile_manager = None;
        self.current_image_res = None;
        self.animation = None;

        let path = self.image_files[self.current_index].clone();
        self.loader.request_load(
            self.current_index,
            self.generation,
            path,
            self.settings.raw_high_quality,
        );

        // Re-schedule preloads to update nearby images with the new setting as well
        self.schedule_preloads(true);
    }

    pub(crate) fn navigate_to(&mut self, new_index: usize) {
        if self.refresh_scan_in_progress || self.image_files.is_empty() {
            return;
        }

        let previous_index = self.current_index;
        let target_index = new_index % self.image_files.len();
        if target_index == self.current_index {
            return;
        }
        let preload_forward =
            navigation_is_forward(previous_index, target_index, self.image_files.len());

        // Setup transition if enabled. We defer transition start until the target
        // texture is actually ready to draw, avoiding black/stale-frame flashes.
        if self.settings.transition_style != TransitionStyle::None {
            let now = Instant::now();
            if self.settings.transition_style == TransitionStyle::Random {
                // Pick a random style from the pool using rand for uniform distribution
                let pool = TransitionStyle::RANDOM_POOL;
                self.active_transition = *pool
                    .choose(&mut rand::thread_rng())
                    .unwrap_or(&TransitionStyle::Fade);
            } else {
                self.active_transition = self.settings.transition_style;
            }

            let source_tex = self.texture_cache.get(self.current_index).cloned();
            let source_hdr = self.first_cached_hdr_or_tiled_preview_for_index(self.current_index);
            // Always overwrite transition source. If current index has no texture
            // (e.g. decode failed and only error text is shown), keeping an older
            // prev_texture can make unrelated stale pixels flash during next navigation.
            self.prev_texture = select_transition_source_texture(
                source_tex,
                self.hdr_placeholder_fallback_indices
                    .contains(&self.current_index),
                self.prev_texture.clone(),
            );
            self.prev_hdr_image = select_transition_source_hdr(
                source_hdr,
                self.hdr_placeholder_fallback_indices
                    .contains(&self.current_index),
                self.prev_hdr_image.clone(),
            );
            // Handle wrap-around logic for direction
            self.is_next = target_index > self.current_index
                || (target_index == 0 && self.current_index == self.image_files.len() - 1);
            self.transition_start = None;
            let source_has_texture = self.prev_texture.is_some() || self.prev_hdr_image.is_some();
            let target_has_texture = self.texture_cache.contains(target_index);
            let target_has_hdr_plane = self.hdr_image_cache.contains_key(&target_index)
                || self.hdr_tiled_source_cache.contains_key(&target_index);
            let target_placeholder_only = self
                .hdr_placeholder_fallback_indices
                .contains(&target_index);
            if should_start_transition_immediately(
                target_is_render_ready(
                    target_has_texture,
                    target_has_hdr_plane,
                    target_placeholder_only,
                ),
                source_has_texture,
            ) {
                self.transition_start =
                    Some(now - transition_preroll_duration(self.settings.transition_ms));
                self.pending_transition_target = None;
            } else {
                self.pending_transition_target = Some(target_index);
            }

            if should_reset_transition_when_source_texture_missing(
                self.prev_texture.is_some() || self.prev_hdr_image.is_some(),
            ) {
                // No texture available for the source frame: avoid reusing stale
                // transition state from previous navigation.
                self.prev_texture = None;
                self.prev_hdr_image = None;
                self.pending_transition_target = None;
            }
        } else {
            let source_tex = self.texture_cache.get(self.current_index).cloned();
            let source_hdr = self.first_cached_hdr_or_tiled_preview_for_index(self.current_index);
            let source_has_texture = source_tex.is_some() || source_hdr.is_some();
            let target_has_texture = self.texture_cache.contains(target_index);
            let target_has_hdr_plane = self.hdr_image_cache.contains_key(&target_index)
                || self.hdr_tiled_source_cache.contains_key(&target_index);
            let target_placeholder_only = self
                .hdr_placeholder_fallback_indices
                .contains(&target_index);
            self.active_transition = TransitionStyle::None;
            self.transition_start = None;
            self.prev_texture = select_transition_source_texture(
                source_tex,
                self.hdr_placeholder_fallback_indices
                    .contains(&self.current_index),
                self.prev_texture.clone(),
            );
            self.prev_hdr_image = select_transition_source_hdr(
                source_hdr,
                self.hdr_placeholder_fallback_indices
                    .contains(&self.current_index),
                self.prev_hdr_image.clone(),
            );
            self.pending_transition_target = if !target_is_render_ready(
                target_has_texture,
                target_has_hdr_plane,
                target_placeholder_only,
            ) && source_has_texture
            {
                Some(target_index)
            } else {
                None
            };
            if should_reset_transition_when_source_texture_missing(
                self.prev_texture.is_some() || self.prev_hdr_image.is_some(),
            ) {
                self.prev_texture = None;
                self.prev_hdr_image = None;
                self.pending_transition_target = None;
            }
        }

        preserve_current_tile_manager_for_navigation(
            self.current_index,
            target_index,
            &mut self.tile_manager,
            &mut self.prefetched_tiles,
        );
        self.current_index = target_index;
        self.current_hdr_image = self
            .first_cached_hdr_still_for_index(self.current_index)
            .map(|image| crate::app::CurrentHdrImage::new(self.current_index, image));
        self.current_hdr_tiled_image = self
            .hdr_tiled_source_cache
            .get(&self.current_index)
            .cloned()
            .map(|source| crate::app::CurrentHdrTiledImage::new(self.current_index, source));
        self.current_hdr_tiled_preview = self
            .hdr_tiled_preview_cache
            .get(&self.current_index)
            .cloned()
            .map(|image| crate::app::CurrentHdrImage::new(self.current_index, image));
        self.current_rotation = 0;
        self.zoom_factor = 1.0;
        self.pan_offset = Vec2::ZERO;
        self.animation = None;

        // Update resolution if already in cache (for immediate low-res display)
        if self.texture_cache.contains(self.current_index) {
            if let Some((w, h)) = self.texture_cache.get_original_res(self.current_index) {
                self.current_image_res = Some((w, h));
            } else if let Some(texture) = self.texture_cache.get(self.current_index) {
                let size = texture.size();
                self.current_image_res = Some((size[0] as u32, size[1] as u32));
            }
        } else {
            self.current_image_res = None;
        }

        self.last_switch_time = Instant::now();
        self.error_message = None;
        self.is_font_error = false;
        // Close any open EXIF/XMP modal — it shows data for the previous image
        if matches!(
            self.active_modal,
            Some(crate::ui::dialogs::modal_state::ActiveModal::Exif(_))
                | Some(crate::ui::dialogs::modal_state::ActiveModal::Xmp(_))
        ) {
            self.active_modal = None;
        }

        // Try to pull from predictive cache if available
        if let Some(cached_anim) = self.animation_cache.get(&self.current_index) {
            if let Some(hdr_frames) = &cached_anim.hdr_frames {
                if let Some(hdr) = hdr_frames.first() {
                    self.current_hdr_image = Some(crate::app::CurrentHdrImage::new(
                        self.current_index,
                        Arc::clone(hdr),
                    ));
                }
            }
            self.animation = Some(AnimationPlayback {
                image_index: cached_anim.image_index,
                textures: cached_anim.textures.clone(),
                hdr_frames: cached_anim.hdr_frames.clone(),
                delays: cached_anim.delays.clone(),
                current_frame: 0,
                frame_start: Instant::now(),
            });
        }

        // Check if we have a prefetched TileManager ready to use!
        if let Some(mut tm) = self.prefetched_tiles.remove(&self.current_index) {
            // We successfully hit the cache!
            // Save the prefetch-phase generation before incrementing. Any in-flight HQ preview
            // tasks (HDR or SDR) were spawned with this old generation. We record it so that
            // handle_preview_update() can accept their results instead of discarding them as
            // stale — avoiding a from-scratch re-render of huge EXR/JXL files.
            let prefetch_gen = self.generation;
            self.generation = self.generation.wrapping_add(1);
            self.loader.set_generation(self.generation);
            self.prefetch_prev_generation = Some(prefetch_gen);

            tm.generation = self.generation;
            self.current_image_res = Some((tm.full_width, tm.full_height));

            // Trigger deferred refinement for RAW sources (LibRaw demosaic).
            // HDR tiled sources: in-flight prefetch tasks carry `prefetch_gen` and will be
            // accepted by handle_preview_update via prefetch_prev_generation — no re-spawn needed.
            tm.get_source()
                .request_refinement(self.current_index, self.generation);

            self.tile_manager = Some(tm);

            log::debug!(
                "[App] Cache Hit: Restored prefetched TileManager for index {} (prefetch_gen={} → current_gen={})",
                self.current_index,
                prefetch_gen,
                self.generation
            );
        } else if self.has_loaded_asset(self.current_index) {
            // Decoded during preload (HDR cache and/or deferred SDR pixels) — avoid re-decoding.
            self.prefetch_prev_generation = None;
            self.generation = self.generation.wrapping_add(1);
            self.loader.set_generation(self.generation);
            let is_tiled = self
                .texture_cache
                .is_preview_placeholder(self.current_index);
            if is_tiled && self.tile_manager.is_none() {
                // Defensive fallback for any tiled preview (SDR or HDR with missing source cache)
                // that doesn't have a TileManager installed.
                if let Some((w, h)) = self.texture_cache.get_original_res(self.current_index) {
                    self.current_image_res = Some((w, h));
                }
                self.loader.request_load(
                    self.current_index,
                    self.generation,
                    self.image_files[self.current_index].clone(),
                    self.settings.raw_high_quality,
                );
            } else if let Some(hdr) = self.hdr_image_cache.get(&self.current_index) {
                self.current_image_res = Some((hdr.width, hdr.height));
            } else if let Some(src) = self.hdr_tiled_source_cache.get(&self.current_index) {
                self.current_image_res = Some((src.width(), src.height()));
                // Defensive fallback: if it is a tiled HDR image but the TileManager is missing,
                // trigger a request_load to rebuild the TileManager.
                if self.tile_manager.is_none() {
                    self.loader.request_load(
                        self.current_index,
                        self.generation,
                        self.image_files[self.current_index].clone(),
                        self.settings.raw_high_quality,
                    );
                }
            } else if let Some(decoded) = self.deferred_sdr_uploads.get(&self.current_index) {
                self.current_image_res = Some((decoded.width, decoded.height));
            }
        } else {
            // Cache miss: fresh load required. Clear any leftover prefetch_prev_generation
            // so handle_preview_update doesn't erroneously accept stale old-gen results.
            self.prefetch_prev_generation = None;
            // ALWAYS increment generation on every navigation and request a fresh load.
            // This ensures TileManager is re-initialized for large images and
            // low-res thumbnails are upgraded to full resolution.
            self.generation = self.generation.wrapping_add(1);
            self.loader.set_generation(self.generation);
            self.loader.request_load(
                self.current_index,
                self.generation,
                self.image_files[self.current_index].clone(),
                self.settings.raw_high_quality,
            );
        }

        // Housekeeping: evict distant prefetch CPU caches (tiles, deferred SDR, static HDR).
        self.evict_distant_prefetch_caches();

        self.schedule_preloads(preload_forward);
        // When a prefetch hit occurred, also_keep_preview preserves any Preview result for the
        // current index that still carries the old prefetch generation — it may have arrived in
        // the channel between the generation bump and now and must not be thrown away.
        let also_keep = self
            .prefetch_prev_generation
            .map(|old_gen| (self.current_index, old_gen));
        self.loader
            .discard_pending_stale_outputs(self.generation, also_keep);
        self.trigger_current_hdr_fallback_refinement_if_needed();
    }

    pub(crate) fn navigate_next(&mut self) {
        if self.image_files.is_empty() {
            return;
        }
        let idx = (self.current_index + 1) % self.image_files.len();
        self.navigate_to(idx);
    }

    pub(crate) fn navigate_prev(&mut self) {
        if self.image_files.is_empty() {
            return;
        }
        let idx = if self.current_index == 0 {
            self.image_files.len() - 1
        } else {
            self.current_index - 1
        };
        self.navigate_to(idx);
    }

    pub(crate) fn navigate_first(&mut self) {
        self.navigate_to(0);
    }

    pub(crate) fn navigate_last(&mut self) {
        if !self.image_files.is_empty() {
            let last = self.image_files.len() - 1;
            self.navigate_to(last);
        }
    }

    // ------------------------------------------------------------------
    // Preloading
    // ------------------------------------------------------------------

    pub(crate) fn schedule_preloads(&mut self, forward: bool) {
        let n = self.image_files.len();
        if n == 0 {
            return;
        }
        let cur = self.current_index;

        // Always load the current image unless any renderable representation is already cached.
        // HDR tiled images often have no SDR texture_cache entry, so checking only texture_cache
        // would re-submit expensive EXR preview generation after the initial load is processed.
        if !self.has_loaded_asset(cur) && !self.loader.is_loading(cur, self.generation) {
            let path = self.image_files[cur].clone();
            self.loader
                .request_load(cur, self.generation, path, self.settings.raw_high_quality);
        }

        if !self.settings.preload {
            return;
        }

        // Determine the "primary" and "secondary" directions.
        // Primary gets the larger budget; secondary gets the smaller one.
        let (primary_max, primary_budget, secondary_max, secondary_budget) = if forward {
            (
                MAX_PRELOAD_FORWARD,
                self.preload_budget_forward,
                MAX_PRELOAD_BACKWARD,
                self.preload_budget_backward,
            )
        } else {
            (
                MAX_PRELOAD_BACKWARD,
                self.preload_budget_backward,
                MAX_PRELOAD_FORWARD,
                self.preload_budget_forward,
            )
        };

        // Collect indices for each direction
        let primary_indices: Vec<usize> = (1..=n.min(primary_max + 10)) // +10 headroom to skip tiled images
            .map(|i| {
                if forward {
                    (cur + i) % n
                } else {
                    (cur + n - i) % n
                }
            })
            .collect();

        let secondary_indices: Vec<usize> = (1..=n.min(secondary_max + 10))
            .map(|i| {
                if forward {
                    (cur + n - i) % n
                } else {
                    (cur + i) % n
                }
            })
            .collect();

        self.preload_direction(primary_indices, primary_max, primary_budget);
        self.preload_direction(secondary_indices, secondary_max, secondary_budget);
    }

    /// Preload images from a list of candidate indices, respecting count and byte limits.
    /// Rule 1: Always preload at least 1 non-tiled image (guaranteed minimum).
    /// Rule 2: Stop if count >= max_count OR cumulative NEW file size >= budget.
    /// Tiled-candidate images are skipped entirely (they use on-demand tile loading).
    /// Already-cached images occupy a count slot (preventing over-reach) but
    /// do NOT consume byte budget (no new memory allocation occurs).
    pub(crate) fn preload_direction(
        &mut self,
        candidates: Vec<usize>,
        max_count: usize,
        budget: u64,
    ) {
        let mut count = 0usize;
        let mut new_bytes = 0u64;

        for idx in candidates {
            if count >= max_count {
                break;
            }

            // Already cached or in-flight: occupies a slot but costs nothing new.
            if self.has_loaded_asset(idx) || self.loader.is_loading(idx, self.generation) {
                count += 1;
                continue;
            }

            let path = &self.image_files[idx];

            let file_size = self.file_byte_len_by_index.get(idx).copied().unwrap_or(0);

            // After the guaranteed first image, enforce the byte budget.
            // Sizes come from the scanner thread; unknown (0) skips the byte gate.
            // Compressed on-disk size understates decoded RGBA footprint (HEIC/JPEG often 10–20×).
            let decode_budget_bytes = if file_size > 0 {
                file_size.saturating_mul(12)
            } else {
                0
            };
            if count > 0
                && decode_budget_bytes > 0
                && new_bytes.saturating_add(decode_budget_bytes) > budget
            {
                break;
            }

            self.loader.request_load(
                idx,
                self.generation,
                path.clone(),
                self.settings.raw_high_quality,
            );
            count += 1;
            new_bytes += decode_budget_bytes.max(file_size);
        }
    }

    fn has_loaded_asset(&self, index: usize) -> bool {
        let has_static_hdr = self.hdr_image_cache.contains_key(&index);
        let has_hdr_tiled_source = self.hdr_tiled_source_cache.contains_key(&index);
        let has_hdr_plane = has_static_hdr || has_hdr_tiled_source;
        if !hdr_fallback_asset_is_loaded(
            self.hdr_sdr_fallback_indices.contains(&index),
            has_hdr_plane,
        ) {
            return false;
        }
        current_image_has_loaded_asset(
            self.texture_cache.contains(index),
            has_static_hdr,
            has_hdr_tiled_source,
            self.animation_cache.contains_key(&index),
        ) || self.deferred_sdr_uploads.contains_key(&index)
    }

    // ------------------------------------------------------------------
    // Background result processing
    // ------------------------------------------------------------------

    pub(crate) fn process_file_op_results(&mut self) {
        while let Ok(res) = self.file_op_rx.try_recv() {
            match res {
                FileOpResult::Delete(path, original_idx, res) => {
                    if let Err(e) = res {
                        log::error!("Failed to delete {:?}: {}", path, e);
                        self.error_message =
                            Some(t!("status.delete_failed", err = e.to_string()).to_string());

                        // ROLLBACK: Restore the file to the in-memory list if it failed to delete.
                        // We use the original index to maintain order.
                        if original_idx <= self.image_files.len() {
                            self.image_files.insert(original_idx, path.clone());
                            let sz = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                            self.file_byte_len_by_index.insert(original_idx, sz);
                        } else {
                            self.image_files.push(path.clone());
                            let sz = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                            self.file_byte_len_by_index.push(sz);
                        }

                        // Restore viewer state to ensure consistency.
                        // We jump back to the file that failed to delete to ensure the index is valid.
                        self.current_index = original_idx;
                        self.generation = self.generation.wrapping_add(1);
                        self.loader.set_generation(self.generation);
                        self.status_message =
                            t!("status.found", count = self.image_files.len().to_string())
                                .to_string();
                        self.images_ever_loaded = true;
                        self.schedule_preloads(true);
                    } else {
                        log::info!("Successfully deleted {:?}", path);
                    }
                }
                FileOpResult::Exif(path, data) => {
                    if let Some(crate::ui::dialogs::modal_state::ActiveModal::Exif(ref mut state)) =
                        self.active_modal
                    {
                        if state.path == path {
                            state.data = data;
                            state.loading = false;
                        }
                    }
                }
                FileOpResult::Xmp(path, data) => {
                    if let Some(crate::ui::dialogs::modal_state::ActiveModal::Xmp(ref mut state)) =
                        self.active_modal
                    {
                        if state.path == path {
                            if let Some((d, x)) = data {
                                state.data = Some(d);
                                state.xml = Some(x);
                            } else {
                                state.data = None;
                                state.xml = None;
                            }
                            state.loading = false;
                        }
                    }
                }
                FileOpResult::Wallpaper {
                    current,
                    monitors,
                    supports_per_monitor,
                } => {
                    if let Some(crate::ui::dialogs::modal_state::ActiveModal::Wallpaper(
                        ref mut state,
                    )) = self.active_modal
                    {
                        state.apply_wallpaper_probe(current, monitors, supports_per_monitor);
                    }
                }
            }
        }
    }

    pub(crate) fn process_scan_results(&mut self) {
        let rx = match self.scan_rx.take() {
            Some(rx) => rx,
            None => return,
        };

        let mut done = false;
        let mut first_batch_preload_pending = false;
        let startup_target_pending = has_startup_target(
            self.initial_image.as_ref(),
            self.settings.resume_last_image,
            self.settings.last_viewed_image.as_ref(),
        );

        // Drain all available messages this frame (non-blocking)
        loop {
            match rx.try_recv() {
                Ok(msg) => {
                    match msg {
                        ScanMessage::Batch(batch) => {
                            let is_first_batch = self.image_files.is_empty();
                            for (path, len) in batch {
                                self.image_files.push(path);
                                self.file_byte_len_by_index.push(len);
                            }

                            let count = self.image_files.len();
                            self.status_message =
                                t!("status.found", count = count.to_string()).to_string();

                            // On first batch: resolve initial position and start preloading.
                            // For refresh scans, initial_image is kept None so that
                            // resolve_initial_position() does not consume (and reset to None)
                            // the anchor before the final sorted Done pass.
                            if is_first_batch && count > 0 {
                                if !self.refresh_scan_in_progress {
                                    self.resolve_initial_position();
                                }
                                // Auto-close the settings panel only during the very first
                                // startup scan (images_ever_loaded == false).
                                if !self.images_ever_loaded {
                                    self.show_settings = false;
                                }
                                self.images_ever_loaded = true;
                                first_batch_preload_pending = true;
                            }
                        }
                        ScanMessage::Done => {
                            done = true;
                            self.scanning = false;

                            if self.image_files.is_empty() {
                                self.status_message = t!("status.not_found").to_string();
                                // Bug fix: clear refresh state even when directory is empty,
                                // otherwise refresh_scan_in_progress stays true forever and
                                // blocks all navigation and future F5 presses.
                                self.finish_refresh_scan_state();
                            } else {
                                // Re-sort the full list now that all batches have arrived.
                                debug_assert_eq!(
                                    self.image_files.len(),
                                    self.file_byte_len_by_index.len()
                                );
                                let mut combined: Vec<(PathBuf, u64)> =
                                    std::mem::take(&mut self.image_files)
                                        .into_iter()
                                        .zip(std::mem::take(&mut self.file_byte_len_by_index))
                                        .collect();
                                combined.sort_by(|a, b| a.0.cmp(&b.0));
                                let (paths, sizes): (Vec<_>, Vec<_>) = combined.into_iter().unzip();
                                self.image_files = paths;
                                self.file_byte_len_by_index = sizes;

                                if self.refresh_scan_in_progress {
                                    // Refresh path: relocate using the stable anchor path so that
                                    // the position survives multi-batch scans. Then clear all other
                                    // index-keyed states except the resolved new_idx.
                                    if let Some(anchor) = self.refresh_anchor_path.take() {
                                        // Find where the anchor file landed after sorting.
                                        if let Some(new_idx) = self.find_index_for_path(&anchor) {
                                            // Relocate kept state from temporary index 0 to new_idx.
                                            self.relocate_index_keyed_cache(0, new_idx);

                                            // Wipe all other index-keyed states except the current resolved image at new_idx.
                                            self.clear_index_keyed_state_after_list_reorder_except_index(new_idx);
                                            self.invalidate_random_slideshow_order();

                                            self.current_index = new_idx;
                                        } else {
                                            // Anchor file was deleted or not found in the new list:
                                            // wipe all index-keyed states completely and fall back to index 0.
                                            self.clear_index_keyed_state_after_list_reorder();
                                            self.invalidate_random_slideshow_order();
                                            self.current_index = 0;

                                            // Request loading of the fallback index 0 file
                                            let fallback_path = self.image_files[0].clone();
                                            self.loader.request_load(
                                                0,
                                                self.generation,
                                                fallback_path,
                                                self.settings.raw_high_quality,
                                            );
                                        }
                                    } else {
                                        // anchor path not set (e.g. list was empty at F5 time)
                                        self.clear_index_keyed_state_after_list_reorder();
                                        self.invalidate_random_slideshow_order();
                                        self.resolve_initial_position();
                                    }
                                } else {
                                    // CRITICAL: Global sort finished; all index-keyed caches and
                                    // pending loads may now point at the wrong file.
                                    self.clear_index_keyed_state_after_list_reorder();
                                    self.invalidate_random_slideshow_order();

                                    // Regular new-directory scan: reset pan/zoom/rotation.
                                    self.zoom_factor = 1.0;
                                    self.pan_offset = Vec2::ZERO;
                                    self.current_rotation = 0;

                                    // Re-resolve position after global sort.
                                    self.resolve_initial_position();
                                }

                                let count = self.image_files.len();
                                self.status_message =
                                    t!("status.found", count = count.to_string()).to_string();
                                self.schedule_preloads(true);

                                self.finish_refresh_scan_state();
                            }
                            break;
                        }
                    }
                }
                Err(crossbeam_channel::TryRecvError::Empty) => break,
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    done = true;
                    self.scanning = false;
                    if self.image_files.is_empty() {
                        self.status_message = t!("status.not_found").to_string();
                    }
                    // Scan thread disconnected unexpectedly: clean up refresh state if active
                    // and restore slideshow so playback is not left permanently paused.
                    if self.refresh_scan_in_progress {
                        self.refresh_anchor_path = None;
                        log::warn!("[RefreshFileList] Scan thread disconnected; refresh aborted");
                        self.finish_refresh_scan_state();
                    }
                    break;
                }
            }
        }

        if !self.refresh_scan_in_progress
            && should_schedule_first_batch_preload(
                first_batch_preload_pending,
                self.image_files.len(),
                done,
                startup_target_pending,
            )
        {
            self.schedule_preloads(true);
        }

        if !done {
            // Put the receiver back if scanning is still in progress
            self.scan_rx = Some(rx);
        }
    }

    pub(crate) fn find_index_for_path(&self, path: &std::path::Path) -> Option<usize> {
        find_index_for_path_impl(&self.image_files, path)
    }

    /// Resolve the starting image index from initial_image or resume settings.
    pub(crate) fn resolve_initial_position(&mut self) {
        if let Some(ref path) = self.initial_image {
            if let Some(pos) = self.find_index_for_path(path) {
                self.current_index = pos;
            }
            self.initial_image = None;
        } else if self.settings.resume_last_image {
            if let Some(last_path) = &self.settings.last_viewed_image {
                if let Some(pos) = self.find_index_for_path(last_path) {
                    self.current_index = pos;
                }
            }
        }
    }

    /// Process results from the background ImageLoader.
    pub(crate) fn process_loaded_images(&mut self, ctx: &egui::Context) {
        self.flush_deferred_sdr_upload_for_current(ctx);

        // ── 1. Continue uploading deferred animation frames (max 8 per tick) ──
        const ANIM_UPLOAD_QUOTA: usize = 8;
        if let Some(ref mut pending) = self.pending_anim_frames {
            let mut uploaded = 0;
            while pending.next_frame < pending.frames.len() && uploaded < ANIM_UPLOAD_QUOTA {
                let i = pending.next_frame;
                let frame = &pending.frames[i];
                let color_image = ColorImage::from_rgba_unmultiplied(
                    [frame.width as usize, frame.height as usize],
                    frame.rgba(),
                );
                let name = format!("anim_{}_{}", pending.image_index, i);
                let handle = ctx.load_texture(name, color_image, TextureOptions::LINEAR);
                pending.textures.push(handle);
                pending.delays.push(frame.delay);
                pending.next_frame += 1;
                uploaded += 1;
            }

            // Check if all frames have been uploaded
            if pending.next_frame >= pending.frames.len() {
                let idx = pending.image_index;

                // Build the final AnimationPlayback from the now-complete upload
                let playback = AnimationPlayback {
                    image_index: idx,
                    textures: std::mem::take(&mut pending.textures),
                    hdr_frames: pending.hdr_frames.clone(),
                    delays: std::mem::take(&mut pending.delays),
                    current_frame: 0,
                    frame_start: Instant::now(),
                };

                if idx == self.current_index {
                    if let Some(hdr_frames) = &playback.hdr_frames {
                        if let Some(hdr) = hdr_frames.first() {
                            self.current_hdr_image =
                                Some(crate::app::CurrentHdrImage::new(idx, Arc::clone(hdr)));
                        }
                    }
                    self.animation = Some(AnimationPlayback {
                        image_index: playback.image_index,
                        textures: playback.textures.clone(),
                        hdr_frames: playback.hdr_frames.clone(),
                        delays: playback.delays.clone(),
                        current_frame: 0,
                        frame_start: Instant::now(),
                    });
                }
                self.animation_cache.insert(idx, playback);
                self.pending_anim_frames = None;
            } else {
                // More frames remain — ask for another repaint
                ctx.request_repaint();
            }
        }

        // ── 2. Process results from the background ImageLoader ──
        //
        // Generation vs `prefetch_prev_generation` (why Preview is special):
        // `handle_preview_update` accepts HQ preview results whose `generation` equals
        // `prefetch_prev_generation` for the current index, because refinement can finish after
        // we bump `self.generation` when promoting a prefetched `TileManager`. `LoaderOutput::Image`
        // uses no analogous bypass: decoded images are keyed to the generation from the active
        // `request_load` / refinement request (`do_load` tracks that generation), so they either match
        // `self.generation` in `gen_match` below or must be dropped; extending the prefetch survivor rule
        // here would widen the stale-result window without a matching in-flight Image pipeline.
        //
        // QUOTA DESIGN:
        //   - We count each ctx.load_texture() call as one "upload slot".
        //   - Tile results and Refined notifications do NOT consume slots
        //     (they don't call load_texture on the main thread path).
        //   - The current image is always allowed through, regardless of quota,
        //     so switching images is never blocked by background preload traffic.
        //   - When quota is reached, the polled-but-unprocessed item is pushed
        //     back via repush() so it is the first thing processed next frame.
        const GLOBAL_UPLOAD_QUOTA: usize = 3;
        let mut uploads_this_frame: usize = 0;

        while let Some(output) = self.loader.poll() {
            match output {
                LoaderOutput::Image(load_result) => {
                    let idx = load_result.index;
                    let generation = load_result.generation;
                    let is_current = idx == self.current_index;
                    let gen_match = generation == self.generation;

                    // CRITICAL: Drop any stale results, even for the current index.
                    // This prevents a race where deleting an image reuses the index
                    // but a late decode from the deleted file arrives and overwrites
                    // the new current image state.
                    if !gen_match {
                        self.loader.finish_image_request(idx, generation);
                        continue;
                    }
                    if !source_key_matches_index(&self.image_files, idx, load_result.source_key) {
                        log::warn!(
                            "[App] Image result discarded (source key mismatch): index={} generation={}",
                            idx,
                            generation
                        );
                        self.loader.finish_image_request(idx, generation);
                        continue;
                    }

                    // DESIGN: The current image ALWAYS bypasses the upload quota.
                    //
                    // Rationale: when the user switches images, they have an immediate
                    // expectation to see the new image. If background preloads have already
                    // consumed the frame budget, deferring the current image would show a
                    // blank/stale frame — a hard visible stutter. Preloaded images, by
                    // contrast, are invisible to the user until they navigate to them, so
                    // a one-frame delay is imperceptible.
                    //
                    // Trade-off: in the worst case (current image arrives the same frame as
                    // N preload results), the current image causes one extra load_texture
                    // beyond the quota. This is acceptable: it happens at most once per
                    // navigation event, not on every frame.
                    if !is_current && uploads_this_frame >= GLOBAL_UPLOAD_QUOTA {
                        self.loader.repush(LoaderOutput::Image(load_result));
                        ctx.request_repaint();
                        break;
                    }

                    self.loader.finish_image_request(idx, generation);
                    if let Some((requeue_idx, requeue_gen, requeue_path)) =
                        self.handle_image_load_result(load_result, ctx)
                    {
                        // The slot was just freed by finish_image_request above; it is now safe to
                        // re-queue.  The loader holds the current (correct) HDR capacity.
                        self.loader.request_load(
                            requeue_idx,
                            requeue_gen,
                            requeue_path,
                            self.settings.raw_high_quality,
                        );
                    }
                    uploads_this_frame += 1;

                    if should_request_repaint_for_asset_update(
                        AssetUpdateKind::ImageLoaded,
                        is_current,
                        false,
                    ) {
                        ctx.request_repaint();
                    }
                }

                LoaderOutput::Preview(preview_update) => {
                    let preview_is_current = preview_update.index == self.current_index;
                    if !source_key_matches_index(
                        &self.image_files,
                        preview_update.index,
                        preview_update.source_key,
                    ) {
                        log::warn!(
                            "[App] Preview update discarded (source key mismatch): index={} generation={}",
                            preview_update.index,
                            preview_update.generation
                        );
                        continue;
                    }

                    // DESIGN: Mirror the Image bypass — the current image's HQ preview
                    // also skips the quota.
                    //
                    // Rationale: the Preview message carries the refined high-quality
                    // thumbnail that replaces the initial blurry EXIF preview (the
                    // "blurry→sharp" transition the user can see). Deferring it even
                    // one frame makes the refinement visually slower with no benefit,
                    // because the pixel data is already in memory at this point.
                    // Only background-prefetched previews should be quota-limited.
                    if !preview_is_current && uploads_this_frame >= GLOBAL_UPLOAD_QUOTA {
                        self.loader.repush(LoaderOutput::Preview(preview_update));
                        ctx.request_repaint();
                        break;
                    }
                    self.handle_preview_update(preview_update, ctx);
                    uploads_this_frame += 1;
                }

                LoaderOutput::Tile(tile_result) => {
                    // Tile signals are free: pixels live in PIXEL_CACHE; GPU upload
                    // happens lazily in the tile rendering pass, not here.
                    self.handle_tile_load_result(tile_result, ctx);
                }

                LoaderOutput::Refined(idx, gen_id) => {
                    // Metadata-only notification — no load_texture call here.
                    self.handle_refined_notification(idx, gen_id, ctx);
                }

                LoaderOutput::HdrSdrFallback(update) => {
                    let is_current = update.index == self.current_index;
                    if !source_key_matches_index(&self.image_files, update.index, update.source_key)
                    {
                        self.hdr_in_flight_fallback_refinements
                            .remove(&update.index);
                        log::warn!(
                            "[App] HDR SDR fallback discarded (source key mismatch): index={} generation={}",
                            update.index,
                            update.generation
                        );
                        continue;
                    }
                    if !is_current && uploads_this_frame >= GLOBAL_UPLOAD_QUOTA {
                        self.loader.repush(LoaderOutput::HdrSdrFallback(update));
                        ctx.request_repaint();
                        break;
                    }
                    self.hdr_in_flight_fallback_refinements
                        .remove(&update.index);
                    self.handle_hdr_sdr_fallback_update(update, ctx);
                    uploads_this_frame += 1;
                    if should_request_repaint_for_asset_update(
                        AssetUpdateKind::ImageLoaded,
                        is_current,
                        false,
                    ) {
                        ctx.request_repaint();
                    }
                }
            }

            // Secondary quota check after each processed item.
            if uploads_this_frame >= GLOBAL_UPLOAD_QUOTA {
                ctx.request_repaint();
                break;
            }
        }

        // Start any deferred transition exactly when the target texture is ready.
        // This runs AFTER processing loader outputs so we don't render one static
        // frame in between "texture became ready" and "transition started".
        if can_start_pending_transition(
            self.pending_transition_target,
            self.current_index,
            target_is_render_ready(
                self.texture_cache.contains(self.current_index),
                self.current_hdr_image
                    .as_ref()
                    .is_some_and(|current| current.image_for_index(self.current_index).is_some())
                    || self.hdr_image_cache.contains_key(&self.current_index)
                    || self
                        .hdr_tiled_source_cache
                        .contains_key(&self.current_index),
                self.hdr_placeholder_fallback_indices
                    .contains(&self.current_index),
            ),
        ) {
            if self.active_transition != TransitionStyle::None {
                self.transition_start =
                    Some(Instant::now() - transition_preroll_duration(self.settings.transition_ms));
            } else {
                // No-transition mode uses `prev_texture` only as a one-frame safety net while
                // waiting for the target texture. Once current texture is ready, release it
                // immediately instead of keeping an extra stale handle until next navigation.
                self.prev_texture = None;
                self.prev_hdr_image = None;
            }
            self.pending_transition_target = None;
        }
    }

    /// Handles a Refined notification: bumps generation so TileManager
    /// re-fetches tiles from the newly developed high-resolution buffer.
    fn handle_refined_notification(&mut self, idx: usize, gen_id: u64, ctx: &egui::Context) {
        if idx == self.current_index && gen_id == self.generation {
            log::info!("[App] Refined image notification for index={}", idx);

            crate::tile_cache::PIXEL_CACHE.lock().remove_image(idx);

            self.generation = self.generation.wrapping_add(1);
            self.loader.set_generation(self.generation);

            if let Some(tm) = &mut self.tile_manager {
                log::info!("[App] Refined: Tiled mode — forcing tile upgrade to high definition");
                tm.generation = self.generation;
                tm.pending_tiles.clear();
                self.texture_cache.remove(idx);
                self.remove_hdr_image_index(idx);
            } else {
                log::warn!(
                    "[App] Refined: Static mode encountered unexpectedly. Attempting to reload."
                );
                self.texture_cache.remove(idx);
                self.remove_hdr_image_index(idx);
                self.loader.request_load(
                    self.current_index,
                    self.generation,
                    self.image_files[self.current_index].clone(),
                    self.settings.raw_high_quality,
                );
            }

            self.loader.flush_tile_queue();
            if should_request_repaint_for_asset_update(
                AssetUpdateKind::RefinedFullPlane,
                true,
                false,
            ) {
                ctx.request_repaint();
            }
        } else {
            // Non-current image refined in background OR stale refinement result.

            // CRITICAL: If it's the current index but the generation doesn't match,
            // it's a stale result from a previous visit. We MUST NOT evict the
            // CURRENT texture cache, otherwise the screen will flicker or go blank.
            if idx == self.current_index {
                log::info!(
                    "[App] Refined: ignoring stale background update for current index {} (gen {} vs current {})",
                    idx,
                    gen_id,
                    self.generation
                );
                return;
            }

            log::info!(
                "[App] Refined: background update for index {} (not current). Invalidating caches.",
                idx
            );
            crate::tile_cache::PIXEL_CACHE.lock().remove_image(idx);
            self.prefetched_tiles.remove(&idx);
            self.texture_cache.remove(idx);
            self.remove_hdr_image_index(idx);
        }
    }

    /// Returns `Some((idx, generation, path))` when the result was stale (wrong HDR capacity) and
    /// the caller must re-queue **after** calling `finish_image_request` to clear the loading-map
    /// slot.  Calling `loader.request_load` before `finish_image_request` would silently drop the
    /// re-queue because the slot appears occupied.
    pub(crate) fn handle_image_load_result(
        &mut self,
        load_result: LoadResult,
        ctx: &egui::Context,
    ) -> Option<(usize, u64, std::path::PathBuf)> {
        let idx = load_result.index;
        let generation = load_result.generation;
        let preview_bundle = load_result.preview_bundle.clone();

        // Stale-capacity guard: if a capacity-sensitive HDR result arrived with a different
        // HDR capacity than the one currently active (e.g. the display monitor was detected
        // after the worker thread read the capacity snapshot), discard this result and ask the
        // caller to re-queue a fresh load once it has released the loading-map slot.
        //
        // NOTE: do NOT call loader.request_load() here — the loading-map slot for this
        // (index, generation) is still occupied until finish_image_request() is called by
        // the caller.  Calling request_load() now would hit the dedup guard in request_load
        // and silently return without spawning a new worker, causing a permanent hang.
        if hdr_load_result_capacity_is_stale(&load_result, self.ultra_hdr_decode_capacity) {
            log::info!(
                "[HDR] Stale-capacity result for index={}: decoded_capacity={:.3} != current={:.3}; will re-queue after slot is freed.",
                idx,
                load_result.target_hdr_capacity,
                self.ultra_hdr_decode_capacity
            );
            if !self.image_files.is_empty() && idx < self.image_files.len() {
                return Some((idx, generation, self.image_files[idx].clone()));
            }
            return None;
        }

        match ImageInstallPlan::from_load_result(&load_result) {
            ImageInstallPlan::StaticSdr { decoded } => {
                self.install_static_sdr_image(idx, decoded, ctx);
            }
            ImageInstallPlan::StaticHdr {
                hdr,
                fallback,
                ultra_hdr_capacity_sensitive,
            } => {
                self.install_static_hdr_image(
                    idx,
                    hdr,
                    fallback,
                    load_result.sdr_fallback_is_placeholder,
                    ultra_hdr_capacity_sensitive,
                    ctx,
                );
            }
            ImageInstallPlan::Tiled {
                source,
                hdr_source,
                hdr_preview,
                hdr_sdr_fallback,
                ultra_hdr_capacity_sensitive,
            } => {
                self.install_tiled_image(
                    idx,
                    generation,
                    source,
                    hdr_source,
                    preview_bundle.sdr(),
                    hdr_preview,
                    hdr_sdr_fallback,
                    ultra_hdr_capacity_sensitive,
                    ctx,
                );
            }
            ImageInstallPlan::Animated { frames } => {
                self.install_animated_image(idx, frames, ctx);
            }
            ImageInstallPlan::HdrAnimated {
                frames,
                ultra_hdr_capacity_sensitive,
            } => {
                self.install_hdr_animated_image(idx, frames, ultra_hdr_capacity_sensitive, ctx);
            }
            ImageInstallPlan::Error { error } => {
                self.install_image_error(idx, error);
            }
        }
        None
    }

    fn upload_static_sdr_texture(
        &mut self,
        idx: usize,
        decoded: &DecodedImage,
        texture_name: String,
        ctx: &egui::Context,
    ) {
        let color_image = ColorImage::from_rgba_unmultiplied(
            [decoded.width as usize, decoded.height as usize],
            decoded.rgba(),
        );
        let handle = ctx.load_texture(texture_name, color_image, TextureOptions::LINEAR);
        if let Some(evicted_idx) = self.texture_cache.insert(
            idx,
            handle,
            decoded.width,
            decoded.height,
            false,
            self.current_index,
            self.image_files.len(),
        ) {
            self.handle_texture_cache_eviction(evicted_idx);
        }
        // Preload may have queued pixels for this index; GPU upload makes them redundant.
        self.deferred_sdr_uploads.remove(&idx);
    }

    fn queue_or_upload_static_sdr_texture(
        &mut self,
        idx: usize,
        decoded: &DecodedImage,
        texture_name: String,
        ctx: &egui::Context,
    ) {
        if idx == self.current_index {
            self.upload_static_sdr_texture(idx, decoded, texture_name, ctx);
        } else {
            self.deferred_sdr_uploads.insert(idx, decoded.clone());
        }
    }

    fn flush_deferred_sdr_upload_for_current(&mut self, ctx: &egui::Context) {
        if !self.deferred_sdr_uploads.contains_key(&self.current_index) {
            return;
        }
        if self.texture_cache.contains(self.current_index) {
            self.deferred_sdr_uploads.remove(&self.current_index);
            return;
        }
        let Some(decoded) = self.deferred_sdr_uploads.remove(&self.current_index) else {
            return;
        };
        let is_hdr_fallback = self.hdr_sdr_fallback_indices.contains(&self.current_index);
        let texture_name = if is_hdr_fallback {
            format!("img_hdr_fallback_{}", self.current_index)
        } else {
            format!("img_{}", self.current_index)
        };
        self.upload_static_sdr_texture(self.current_index, &decoded, texture_name, ctx);
        self.current_image_res = Some((decoded.width, decoded.height));
    }

    fn clear_current_animation_for_index(&mut self, idx: usize) {
        if self
            .animation
            .as_ref()
            .is_some_and(|animation| animation.image_index == idx)
        {
            self.animation = None;
        }
    }

    fn install_static_sdr_image(
        &mut self,
        idx: usize,
        decoded: &DecodedImage,
        ctx: &egui::Context,
    ) {
        self.remove_hdr_image_index(idx);
        self.queue_or_upload_static_sdr_texture(idx, decoded, format!("img_{idx}"), ctx);
        if idx == self.current_index {
            self.current_image_res = Some((decoded.width, decoded.height));
            self.tile_manager = None;
            self.clear_current_animation_for_index(idx);
        }
    }

    fn install_static_hdr_image(
        &mut self,
        idx: usize,
        hdr: Arc<crate::hdr::types::HdrImageBuffer>,
        fallback: &DecodedImage,
        sdr_fallback_is_placeholder: bool,
        ultra_hdr_capacity_sensitive: bool,
        ctx: &egui::Context,
    ) {
        self.remove_hdr_image_index(idx);
        self.hdr_image_cache.insert(idx, Arc::clone(&hdr));
        self.hdr_sdr_fallback_indices.insert(idx);
        if sdr_fallback_is_placeholder {
            self.hdr_placeholder_fallback_indices.insert(idx);
        } else {
            self.hdr_placeholder_fallback_indices.remove(&idx);
        }
        if ultra_hdr_capacity_sensitive {
            self.ultra_hdr_capacity_sensitive_indices.insert(idx);
        }

        self.queue_or_upload_static_sdr_texture(
            idx,
            fallback,
            format!("img_hdr_fallback_{idx}"),
            ctx,
        );

        if idx == self.current_index {
            self.current_image_res = Some((hdr.width, hdr.height));
            self.current_hdr_image = Some(crate::app::CurrentHdrImage::new(idx, Arc::clone(&hdr)));
            self.tile_manager = None;
            self.clear_current_animation_for_index(idx);

            if sdr_fallback_is_placeholder {
                if !self.hdr_in_flight_fallback_refinements.contains(&idx) {
                    let source_key = source_key_for_path(&self.image_files[idx]);
                    self.hdr_in_flight_fallback_refinements.insert(idx);
                    self.loader.trigger_hdr_sdr_fallback_refinement(
                        idx,
                        self.generation,
                        Arc::clone(&hdr),
                        source_key,
                    );
                }
            }
        }
    }

    fn handle_hdr_sdr_fallback_update(
        &mut self,
        update: crate::loader::HdrSdrFallbackResult,
        ctx: &egui::Context,
    ) {
        let idx = update.index;
        if !self.hdr_image_cache.contains_key(&idx) {
            return;
        }
        let Some(fallback_image) = update.fallback else {
            return;
        };
        self.hdr_sdr_fallback_indices.insert(idx);
        self.hdr_placeholder_fallback_indices.remove(&idx);
        self.queue_or_upload_static_sdr_texture(
            idx,
            &fallback_image,
            format!("img_hdr_fallback_{idx}"),
            ctx,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn install_tiled_image(
        &mut self,
        idx: usize,
        generation: u64,
        source: Arc<dyn crate::loader::TiledImageSource>,
        hdr_source: Option<Arc<dyn crate::hdr::tiled::HdrTiledSource>>,
        sdr_preview: Option<&DecodedImage>,
        hdr_preview: Option<Arc<crate::hdr::types::HdrImageBuffer>>,
        hdr_sdr_fallback: bool,
        ultra_hdr_capacity_sensitive: bool,
        ctx: &egui::Context,
    ) {
        self.remove_hdr_image_index(idx);
        if let Some(hdr_source) = hdr_source.as_ref() {
            self.hdr_tiled_source_cache
                .insert(idx, Arc::clone(hdr_source));
            self.cache_hdr_tiled_preview(idx, hdr_preview);
        }
        if hdr_sdr_fallback {
            self.hdr_sdr_fallback_indices.insert(idx);
        }
        if ultra_hdr_capacity_sensitive {
            self.ultra_hdr_capacity_sensitive_indices.insert(idx);
        }

        self.upload_tiled_bootstrap_preview(ctx, idx, sdr_preview, source.width(), source.height());

        let mut tm = build_tiled_manager_with_best_preview(
            idx,
            generation,
            Arc::clone(&source),
            self.texture_cache.get(idx).cloned(),
        );
        self.attach_initial_preview_if_needed(ctx, idx, &mut tm, sdr_preview);

        if idx == self.current_index {
            if let Some(hdr_source) = hdr_source {
                self.current_hdr_tiled_image =
                    Some(crate::app::CurrentHdrTiledImage::new(idx, hdr_source));
            }
            self.current_image_res = Some((source.width(), source.height()));
            crate::tile_cache::set_tile_size_for_image(source.width(), source.height());
            self.tile_manager = Some(tm);
            self.animation = None;
            self.log_large_image(idx, source.width(), source.height());
            source.request_refinement(idx, self.generation);
        } else {
            self.prefetched_tiles.insert(idx, tm);
        }
    }

    fn install_animated_image(
        &mut self,
        idx: usize,
        frames: &[crate::loader::AnimationFrame],
        ctx: &egui::Context,
    ) {
        self.remove_hdr_image_index(idx);
        if let Some(first) = frames.first() {
            let decoded = DecodedImage::from_arc(first.width, first.height, first.arc_pixels());
            self.queue_or_upload_static_sdr_texture(idx, &decoded, format!("img_{idx}"), ctx);
            if idx == self.current_index {
                self.current_image_res = Some((first.width, first.height));
                self.tile_manager = None;
            }
        }

        let cur = self.current_index;
        let n = self.image_files.len();
        let is_in_range = n > 0
            && (idx == cur
                || idx == (cur + 1) % n
                || (cur > 0 && idx == cur - 1)
                || (cur == 0 && idx == n - 1));

        if is_in_range {
            self.pending_anim_frames = Some(PendingAnimUpload {
                image_index: idx,
                hdr_frames: None,
                frames: frames.to_vec(),
                textures: Vec::new(),
                delays: Vec::new(),
                next_frame: 0,
            });
            ctx.request_repaint();
        }
    }

    fn install_hdr_animated_image(
        &mut self,
        idx: usize,
        frames: &[crate::loader::HdrAnimationFrame],
        ultra_hdr_capacity_sensitive: bool,
        ctx: &egui::Context,
    ) {
        self.remove_hdr_image_index(idx);
        let hdr_frames: Vec<Arc<crate::hdr::types::HdrImageBuffer>> = frames
            .iter()
            .map(|frame| Arc::new(frame.hdr.clone()))
            .collect();
        if let Some(first_hdr) = hdr_frames.first() {
            // Preload / first navigation reads `hdr_image_cache` before deferred anim uploads
            // finish populating `animation_cache`. Without this, HDR displays fall back to the
            // black SDR placeholder until `pending_anim_frames` completes (dark → bright flash).
            self.hdr_image_cache.insert(idx, Arc::clone(first_hdr));
        }
        self.hdr_sdr_fallback_indices.insert(idx);
        if ultra_hdr_capacity_sensitive {
            self.ultra_hdr_capacity_sensitive_indices.insert(idx);
        }

        if let Some(first) = frames.first() {
            self.queue_or_upload_static_sdr_texture(
                idx,
                &first.fallback,
                format!("img_hdr_anim_fallback_{idx}"),
                ctx,
            );
            if idx == self.current_index {
                self.current_image_res = Some((first.width(), first.height()));
                self.current_hdr_image = Some(crate::app::CurrentHdrImage::new(
                    idx,
                    Arc::clone(&hdr_frames[0]),
                ));
                self.tile_manager = None;
                self.clear_current_animation_for_index(idx);
            }
        }

        let sdr_frames: Vec<crate::loader::AnimationFrame> = frames
            .iter()
            .map(|frame| {
                crate::loader::AnimationFrame::new(
                    frame.width(),
                    frame.height(),
                    frame.fallback.rgba().to_vec(),
                    frame.delay,
                )
            })
            .collect();

        let cur = self.current_index;
        let n = self.image_files.len();
        let is_in_range = n > 0
            && (idx == cur
                || idx == (cur + 1) % n
                || (cur > 0 && idx == cur - 1)
                || (cur == 0 && idx == n - 1));

        if is_in_range {
            self.pending_anim_frames = Some(PendingAnimUpload {
                image_index: idx,
                hdr_frames: Some(hdr_frames),
                frames: sdr_frames,
                textures: Vec::new(),
                delays: Vec::new(),
                next_frame: 0,
            });
            ctx.request_repaint();
        }
    }

    fn install_image_error(&mut self, idx: usize, error: &str) {
        let path_str = self
            .image_files
            .get(idx)
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| format!("[index {idx} absent after rescan]"));
        log::error!(
            "Failed to load image at index {} ({}): {error}",
            idx,
            path_str
        );
        if idx == self.current_index {
            self.error_message =
                Some(t!("status.load_failed", path = path_str, err = error).to_string());
        }
    }

    pub(crate) fn handle_tile_load_result(
        &mut self,
        tile_result: TileResult,
        _ctx: &egui::Context,
    ) {
        // SDR pixels are already in PIXEL_CACHE; HDR pixels are already in the
        // HdrTiledSource cache. Either way, clear the shared pending marker.
        if let Some(ref mut tm) = self.tile_manager {
            if tm.image_index == tile_result.index && tm.generation == tile_result.generation {
                tm.pending_tiles.remove(&tile_result.pending_key());
                if should_request_repaint_for_asset_update(
                    AssetUpdateKind::TileReady,
                    true,
                    tile_result.should_request_repaint(),
                ) {
                    _ctx.request_repaint();
                }
            }
        }
    }

    pub(crate) fn handle_preview_update(&mut self, update: PreviewResult, ctx: &egui::Context) {
        let Some(path_for_logs) = self.image_files.get(update.index) else {
            log::warn!(
                "[App] Preview update discarded (index {} out of range; list len {})",
                update.index,
                self.image_files.len()
            );
            return;
        };

        // CRITICAL: Drop any stale preview results.
        // This prevents out-of-date HQ previews from repopulating the cache after
        // a directory rescan (which shifts indices) or file deletion.
        //
        // Exception: when a prefetched TileManager is promoted to current, we save the
        // old generation in `prefetch_prev_generation`. In-flight tasks from the prefetch
        // phase carry that old generation — we accept their results rather than discarding
        // them and re-doing the (potentially expensive) render from scratch.
        let is_prefetch_survivor = update.index == self.current_index
            && self.prefetch_prev_generation == Some(update.generation);

        if update.generation != self.generation && !is_prefetch_survivor {
            let file_name = path_for_logs
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown");
            log::warn!(
                "[App] [{}] Preview update discarded (stale generation): {} vs current {}",
                file_name,
                update.generation,
                self.generation
            );
            return;
        }

        // Once we have accepted the prefetch-survivor result, clear the slot so future
        // results with the old generation are correctly rejected.
        if is_prefetch_survivor {
            let file_name = path_for_logs
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown");
            log::debug!(
                "[App] [{}] Accepted in-flight prefetch preview (gen={} → promoted gen={})",
                file_name,
                update.generation,
                self.generation
            );
            self.prefetch_prev_generation = None;
        }

        if update.preview_bundle.hdr().is_some() {
            self.cache_hdr_tiled_preview(update.index, update.preview_bundle.hdr().cloned());
            if should_request_repaint_for_asset_update(
                AssetUpdateKind::PreviewUpgraded,
                update.index == self.current_index,
                false,
            ) {
                ctx.request_repaint();
            }
        }

        // Apply HQ preview if it matches the currently displayed tile manager.
        // Also check prefetched tiles and update the texture cache for future navigations.
        let preview = update.preview_bundle.sdr().cloned();
        match (preview, update.error) {
            (Some(preview), _) => {
                // 1. Update current TileManager
                if let Some(ref mut tm) = self.tile_manager {
                    // Accept if generation matches, OR if this is a prefetch-survivor result
                    // (update.generation == old prefetch gen, tm.generation == new promoted gen).
                    if tm.image_index == update.index
                        && (update.generation == tm.generation || is_prefetch_survivor)
                    {
                        log::debug!(
                            "[App] HQ preview applied for current index {} ({}x{})",
                            update.index,
                            preview.width,
                            preview.height
                        );
                        tm.set_preview(preview.clone(), ctx);
                        if should_request_repaint_for_asset_update(
                            AssetUpdateKind::PreviewUpgraded,
                            true,
                            false,
                        ) {
                            ctx.request_repaint();
                        }
                    }
                }

                // 2. Update prefetched TileManagers (survivor results won't match here since
                // the TileManager was already promoted out of prefetched_tiles, skip for them).
                if !is_prefetch_survivor {
                    if let Some(tm) = self.prefetched_tiles.get_mut(&update.index) {
                        if update.generation == tm.generation {
                            log::debug!(
                                "[App] HQ preview applied for prefetched index {} ({}x{})",
                                update.index,
                                preview.width,
                                preview.height
                            );
                            tm.set_preview(preview.clone(), ctx);
                        }
                    }
                } // end !is_prefetch_survivor

                // 3. Update global texture cache (so instant-flips also get HQ texture).
                // Only update if it's empty or currently holds a preview (don't downgrade full static images).
                if should_cache_tiled_sdr_preview(
                    self.texture_cache.contains(update.index),
                    self.texture_cache.is_preview_placeholder(update.index),
                    self.texture_cache.cached_preview_max_side(update.index),
                    preview.width.max(preview.height),
                ) {
                    // Preserve the TRUE image dimensions (e.g. 11648×8736) when updating the preview texture.
                    // Without this, a small preview (e.g. 160×120 EXIF thumbnail) would overwrite
                    // original_res, causing the OSD to display wildly wrong zoom percentages (e.g. 16000%).
                    let (orig_w, orig_h) = self
                        .texture_cache
                        .get_original_res(update.index)
                        .unwrap_or((preview.width, preview.height));

                    let name = format!("img_hq_preview_{}", update.index);
                    let color_image = egui::ColorImage::from_rgba_unmultiplied(
                        [preview.width as usize, preview.height as usize],
                        preview.rgba(),
                    );
                    let handle = ctx.load_texture(name, color_image, egui::TextureOptions::LINEAR);
                    if let Some(evicted_idx) = self.texture_cache.insert(
                        update.index,
                        handle,
                        orig_w,
                        orig_h,
                        true, // is_tiled
                        self.current_index,
                        self.image_files.len(),
                    ) {
                        self.handle_texture_cache_eviction(evicted_idx);
                    }
                }
            }
            (None, Some(error)) => {
                log::error!(
                    "Preview update failed for index {}: {}",
                    update.index,
                    error
                );
            }
            (None, None) => {
                if update.preview_bundle.hdr().is_some() {
                    // Refined HQ for native-HDR / high headroom: loader omits the SDR preview plane
                    // (checklist: avoid generating SDR refinement when not needed). HDR was applied above.
                    log::debug!(
                        "Preview update for index {} is HDR-only (no SDR plane)",
                        update.index
                    );
                } else {
                    log::warn!(
                        "Preview update for index {} carried no SDR preview plane",
                        update.index
                    );
                }
            }
        }
    }

    pub(crate) fn log_large_image(&self, idx: usize, w: u32, h: u32) {
        let Some(path) = self.image_files.get(idx) else {
            log::debug!(
                "[App] Skipped large-image log (index {}, {}×{}) — file list shorter than index",
                idx,
                w,
                h
            );
            return;
        };
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");
        log::info!(
            "[{}] Large image detected: {}x{} ({:.1} MP) — tiled mode active",
            file_name,
            w,
            h,
            (w as f64 * h as f64) / 1_000_000.0
        );
    }

    pub(crate) fn setup_tile_manager(
        &self,
        ctx: &egui::Context,
        idx: usize,
        tm: &mut TileManager,
        preview: DecodedImage,
    ) {
        let preview_img = egui::ColorImage::from_rgba_unmultiplied(
            [preview.width as usize, preview.height as usize],
            preview.rgba(),
        );
        let preview_handle = ctx.load_texture(
            format!("preview_{}", idx),
            preview_img,
            egui::TextureOptions::LINEAR,
        );
        tm.preview_texture = Some(preview_handle);
    }

    fn upload_tiled_bootstrap_preview(
        &mut self,
        ctx: &egui::Context,
        idx: usize,
        preview: Option<&DecodedImage>,
        full_width: u32,
        full_height: u32,
    ) {
        let Some(preview) = preview else {
            return;
        };

        let bootstrap_max = preview.width.max(preview.height);
        if !should_upload_tiled_bootstrap_preview(
            self.texture_cache.contains(idx),
            self.texture_cache.cached_preview_max_side(idx),
            bootstrap_max,
        ) {
            return;
        }

        let color_image = ColorImage::from_rgba_unmultiplied(
            [preview.width as usize, preview.height as usize],
            preview.rgba(),
        );
        let name = format!("img_preview_{}", idx);
        let handle = ctx.load_texture(name, color_image, TextureOptions::LINEAR);
        if let Some(evicted_idx) = self.texture_cache.insert(
            idx,
            handle,
            full_width,
            full_height,
            true,
            self.current_index,
            self.image_files.len(),
        ) {
            self.handle_texture_cache_eviction(evicted_idx);
        }
    }

    fn cache_hdr_tiled_preview(
        &mut self,
        idx: usize,
        preview: Option<Arc<crate::hdr::types::HdrImageBuffer>>,
    ) {
        let Some(path) = self.image_files.get(idx) else {
            log::warn!(
                "[App] Skipped HDR tiled preview cache for index {} (out of range; list len {})",
                idx,
                self.image_files.len()
            );
            return;
        };
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();
        cache_hdr_tiled_preview_state(
            idx,
            self.current_index,
            &mut self.hdr_tiled_preview_cache,
            &mut self.current_hdr_tiled_preview,
            preview,
            &file_name,
        );
    }

    fn attach_initial_preview_if_needed(
        &self,
        ctx: &egui::Context,
        idx: usize,
        tm: &mut TileManager,
        preview: Option<&DecodedImage>,
    ) {
        if tm.preview_texture.is_none() {
            if let Some(preview) = preview {
                self.setup_tile_manager(ctx, idx, tm, preview.clone());
            }
        }
    }
}

fn current_image_has_loaded_asset(
    has_sdr_texture: bool,
    has_static_hdr: bool,
    has_hdr_tiled_source: bool,
    has_animation: bool,
) -> bool {
    has_sdr_texture || has_static_hdr || has_hdr_tiled_source || has_animation
}

fn hdr_fallback_asset_is_loaded(has_hdr_fallback: bool, has_hdr_plane: bool) -> bool {
    !has_hdr_fallback || has_hdr_plane
}

fn prefetch_circular_distance(current_index: usize, image_count: usize, candidate: usize) -> usize {
    if image_count == 0 {
        return usize::MAX;
    }
    let dist_forward = (candidate + image_count - current_index % image_count) % image_count;
    let dist_backward = (current_index + image_count - candidate % image_count) % image_count;
    dist_forward.min(dist_backward)
}

fn prefetch_window_contains(
    current_index: usize,
    image_count: usize,
    candidate: usize,
    max_distance: usize,
) -> bool {
    prefetch_circular_distance(current_index, image_count, candidate) <= max_distance
}

fn should_schedule_first_batch_preload(
    is_first_batch: bool,
    count: usize,
    scan_done: bool,
    startup_target_pending: bool,
) -> bool {
    is_first_batch && count > 0 && !scan_done && !startup_target_pending
}

fn first_cached_hdr_still_for_index(
    hdr_image_cache: &HashMap<usize, Arc<crate::hdr::types::HdrImageBuffer>>,
    animation_cache: &HashMap<usize, AnimationPlayback>,
    pending_anim_frames: Option<&PendingAnimUpload>,
    index: usize,
) -> Option<Arc<crate::hdr::types::HdrImageBuffer>> {
    if let Some(image) = hdr_image_cache.get(&index) {
        return Some(Arc::clone(image));
    }
    if let Some(anim) = animation_cache.get(&index) {
        if let Some(frame) = anim.hdr_frames.as_ref().and_then(|frames| frames.first()) {
            return Some(Arc::clone(frame));
        }
    }
    pending_anim_frames.and_then(|pending| {
        if pending.image_index != index {
            return None;
        }
        pending
            .hdr_frames
            .as_ref()
            .and_then(|frames| frames.first())
            .cloned()
    })
}

/// Returns the best available HDR still for `index`.
///
/// Priority:
/// 1. Static HDR from `hdr_image_cache` / `animation_cache` / `pending_anim_frames`
///    (full-resolution; already used by the Phase 1 static transition path).
/// 2. Tiled HDR downsampled preview from `hdr_tiled_preview_cache`.
/// 3. In-memory `current_hdr_tiled_preview` as a last-resort fallback when not yet cached.
///
/// Returns `None` when none of the above are available — the transition degrades gracefully
/// (no previous-image background is shown), matching existing behaviour.
fn first_cached_hdr_or_tiled_preview_for_index(
    hdr_image_cache: &HashMap<usize, Arc<crate::hdr::types::HdrImageBuffer>>,
    animation_cache: &HashMap<usize, AnimationPlayback>,
    pending_anim_frames: Option<&PendingAnimUpload>,
    hdr_tiled_preview_cache: &HashMap<usize, Arc<crate::hdr::types::HdrImageBuffer>>,
    current_hdr_tiled_preview: Option<&crate::app::CurrentHdrImage>,
    index: usize,
) -> Option<Arc<crate::hdr::types::HdrImageBuffer>> {
    first_cached_hdr_still_for_index(hdr_image_cache, animation_cache, pending_anim_frames, index)
        .or_else(|| hdr_tiled_preview_cache.get(&index).cloned())
        .or_else(|| {
            current_hdr_tiled_preview
                .and_then(|curr| curr.image_for_index(index))
                .cloned()
        })
}

fn find_index_for_path_impl(image_files: &[PathBuf], path: &std::path::Path) -> Option<usize> {
    // Fast path: try direct path comparison first (no syscalls)
    let found = image_files.iter().position(|p| p == path);
    found.or_else(|| {
        // Fallback: canonicalize only the target, then compare
        // with case-insensitive file names to handle path variations
        // without calling canonicalize() on every file in the list.
        let target = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let target_name = target
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase());
        image_files.iter().position(|p| {
            if let Some(ref tn) = target_name {
                if let Some(name) = p.file_name() {
                    if name.to_string_lossy().to_lowercase() == *tn {
                        return p.parent() == target.parent()
                            || p.canonicalize().ok().as_ref() == Some(&target);
                    }
                }
            }
            false
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{HardwareTier, SettingsTab};
    use crate::audio::AudioPlayer;
    use crate::loader::PreviewBundle;
    use crate::loader::{ImageLoader, TextureCache};
    use crate::settings::Settings;
    use crate::theme::{SystemThemeCache, ThemePalette};
    use std::collections::{HashMap, HashSet};

    #[test]
    fn prefetch_window_distance_matches_circular_neighbors() {
        assert!(prefetch_window_contains(0, 100, 0, 2));
        assert!(prefetch_window_contains(0, 100, 2, 2));
        assert!(!prefetch_window_contains(0, 100, 3, 2));
        assert!(prefetch_window_contains(50, 100, 48, 2));
        assert!(!prefetch_window_contains(50, 100, 47, 2));
    }

    #[test]
    fn output_mode_boundary_changes_only_when_crossing_hdr_and_sdr() {
        use crate::hdr::types::HdrOutputMode;

        assert!(output_mode_crosses_hdr_sdr_boundary(
            HdrOutputMode::SdrToneMapped,
            HdrOutputMode::WindowsScRgb
        ));
        assert!(output_mode_crosses_hdr_sdr_boundary(
            HdrOutputMode::MacOsEdr,
            HdrOutputMode::SdrToneMapped
        ));
        assert!(!output_mode_crosses_hdr_sdr_boundary(
            HdrOutputMode::WindowsScRgb,
            HdrOutputMode::MacOsEdr
        ));
        assert!(!output_mode_crosses_hdr_sdr_boundary(
            HdrOutputMode::SdrToneMapped,
            HdrOutputMode::SdrToneMapped
        ));
    }

    struct DummyTiledSource {
        width: u32,
        height: u32,
    }

    impl crate::loader::TiledImageSource for DummyTiledSource {
        fn width(&self) -> u32 {
            self.width
        }

        fn height(&self) -> u32 {
            self.height
        }

        fn extract_tile(&self, _x: u32, _y: u32, w: u32, h: u32) -> Arc<Vec<u8>> {
            Arc::new(vec![0; w as usize * h as usize * 4])
        }

        fn generate_preview(&self, _max_w: u32, _max_h: u32) -> (u32, u32, Vec<u8>) {
            (1, 1, vec![0, 0, 0, 255])
        }

        fn full_pixels(&self) -> Option<Arc<Vec<u8>>> {
            None
        }
    }

    #[test]
    fn first_cached_hdr_still_prefers_static_cache_then_animation_then_pending() {
        use crate::hdr::types::{HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat};
        use std::sync::Arc;
        use std::time::Instant;

        let mk = |tag: u8| {
            Arc::new(HdrImageBuffer {
                width: 1,
                height: 1,
                format: HdrPixelFormat::Rgba32Float,
                color_space: HdrColorSpace::LinearSrgb,
                metadata: HdrImageMetadata::default(),
                rgba_f32: Arc::new(vec![tag as f32, 0.0, 0.0, 1.0]),
            })
        };

        let static_hdr = mk(1);
        let anim_hdr = mk(2);
        let pending_hdr = mk(3);

        let mut hdr_image_cache = HashMap::new();
        hdr_image_cache.insert(0, Arc::clone(&static_hdr));
        assert_eq!(
            first_cached_hdr_still_for_index(&hdr_image_cache, &HashMap::new(), None, 0)
                .map(|b| b.rgba_f32[0]),
            Some(1.0)
        );

        hdr_image_cache.clear();
        let mut animation_cache = HashMap::new();
        animation_cache.insert(
            1,
            AnimationPlayback {
                image_index: 1,
                textures: Vec::new(),
                hdr_frames: Some(vec![Arc::clone(&anim_hdr)]),
                delays: Vec::new(),
                current_frame: 0,
                frame_start: Instant::now(),
            },
        );
        assert_eq!(
            first_cached_hdr_still_for_index(&hdr_image_cache, &animation_cache, None, 1)
                .map(|b| b.rgba_f32[0]),
            Some(2.0)
        );

        let pending = PendingAnimUpload {
            image_index: 2,
            hdr_frames: Some(vec![Arc::clone(&pending_hdr)]),
            frames: Vec::new(),
            textures: Vec::new(),
            delays: Vec::new(),
            next_frame: 0,
        };
        assert_eq!(
            first_cached_hdr_still_for_index(&hdr_image_cache, &HashMap::new(), Some(&pending), 2)
                .map(|b| b.rgba_f32[0]),
            Some(3.0)
        );
        assert!(
            first_cached_hdr_still_for_index(&hdr_image_cache, &HashMap::new(), Some(&pending), 9)
                .is_none()
        );
    }

    #[test]
    fn navigation_preserves_current_tile_manager_for_restore() {
        let source = Arc::new(DummyTiledSource {
            width: 4096,
            height: 4096,
        });
        let mut tile_manager = Some(TileManager::with_source(7, 42, source));
        let mut prefetched_tiles = HashMap::new();

        preserve_current_tile_manager_for_navigation(
            7,
            8,
            &mut tile_manager,
            &mut prefetched_tiles,
        );

        assert!(tile_manager.is_none());
        assert!(prefetched_tiles.contains_key(&7));
        assert_eq!(prefetched_tiles.get(&7).unwrap().generation, 42);
    }

    #[test]
    fn tiled_bootstrap_preview_replaces_only_missing_or_smaller_cached_preview() {
        assert!(should_upload_tiled_bootstrap_preview(false, None, 512));
        assert!(should_upload_tiled_bootstrap_preview(true, None, 512));
        assert!(should_upload_tiled_bootstrap_preview(true, Some(128), 512));
        assert!(!should_upload_tiled_bootstrap_preview(
            true,
            Some(1024),
            512
        ));
        assert!(!should_upload_tiled_bootstrap_preview(true, Some(512), 512));
    }

    #[test]
    fn tiled_hdr_preview_replaces_only_missing_or_smaller_cached_preview() {
        assert!(should_cache_tiled_hdr_preview(None, 1024));
        assert!(should_cache_tiled_hdr_preview(Some(1024), 4096));
        assert!(!should_cache_tiled_hdr_preview(Some(4096), 1024));
        assert!(!should_cache_tiled_hdr_preview(Some(4096), 4096));
    }

    #[test]
    fn current_hdr_tiled_preview_updates_only_when_larger_preview_is_cached() {
        let initial = Arc::new(crate::hdr::types::HdrImageBuffer {
            width: 512,
            height: 256,
            format: crate::hdr::types::HdrPixelFormat::Rgba32Float,
            color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
            metadata: crate::hdr::types::HdrImageMetadata::from_color_space(
                crate::hdr::types::HdrColorSpace::LinearSrgb,
            ),
            rgba_f32: Arc::new(vec![0.0; 512 * 256 * 4]),
        });
        let refined = Arc::new(crate::hdr::types::HdrImageBuffer {
            width: 4096,
            height: 2048,
            format: crate::hdr::types::HdrPixelFormat::Rgba32Float,
            color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
            metadata: crate::hdr::types::HdrImageMetadata::from_color_space(
                crate::hdr::types::HdrColorSpace::LinearSrgb,
            ),
            rgba_f32: Arc::new(vec![0.0; 4]),
        });
        let smaller = Arc::new(crate::hdr::types::HdrImageBuffer {
            width: 1024,
            height: 512,
            format: crate::hdr::types::HdrPixelFormat::Rgba32Float,
            color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
            metadata: crate::hdr::types::HdrImageMetadata::from_color_space(
                crate::hdr::types::HdrColorSpace::LinearSrgb,
            ),
            rgba_f32: Arc::new(vec![0.0; 4]),
        });
        let mut cache = HashMap::new();
        let mut current = None;

        cache_hdr_tiled_preview_state(
            7,
            7,
            &mut cache,
            &mut current,
            Some(Arc::clone(&initial)),
            "test.exr",
        );
        cache_hdr_tiled_preview_state(
            7,
            7,
            &mut cache,
            &mut current,
            Some(Arc::clone(&refined)),
            "test.exr",
        );
        cache_hdr_tiled_preview_state(7, 7, &mut cache, &mut current, Some(smaller), "test.exr");

        let cached = cache.get(&7).expect("preview should be cached");
        assert_eq!(cached.width, 4096);
        let current = current
            .as_ref()
            .and_then(|preview| preview.image_for_index(7))
            .expect("current preview should match image index");
        assert_eq!(current.width, 4096);
    }

    #[test]
    fn tiled_sdr_preview_cache_policy_preserves_full_images_and_larger_previews() {
        assert!(should_cache_tiled_sdr_preview(false, false, None, 512));
        assert!(!should_cache_tiled_sdr_preview(true, false, Some(128), 512));
        assert!(should_cache_tiled_sdr_preview(true, true, None, 512));
        assert!(should_cache_tiled_sdr_preview(true, true, Some(128), 512));
        assert!(!should_cache_tiled_sdr_preview(true, true, Some(512), 512));
        assert!(!should_cache_tiled_sdr_preview(true, true, Some(1024), 512));
    }

    #[test]
    fn current_image_load_guard_treats_hdr_tiled_source_as_loaded() {
        assert!(current_image_has_loaded_asset(false, true, false, false));
        assert!(current_image_has_loaded_asset(false, false, true, false));
        assert!(current_image_has_loaded_asset(false, false, false, true));
        assert!(!current_image_has_loaded_asset(false, false, false, false));
    }

    #[test]
    fn hdr_fallback_texture_without_hdr_plane_is_not_loaded_asset() {
        assert!(hdr_fallback_asset_is_loaded(false, false));
        assert!(hdr_fallback_asset_is_loaded(true, true));
        assert!(!hdr_fallback_asset_is_loaded(true, false));
    }

    #[test]
    fn first_batch_preload_waits_when_scan_done_is_already_available() {
        assert!(!should_schedule_first_batch_preload(true, 3, true, false));
        assert!(should_schedule_first_batch_preload(true, 3, false, false));
        assert!(!should_schedule_first_batch_preload(false, 3, false, false));
        assert!(!should_schedule_first_batch_preload(true, 0, false, false));
    }

    #[test]
    fn first_batch_preload_waits_for_startup_target() {
        assert!(!should_schedule_first_batch_preload(true, 3, false, true));
    }

    #[test]
    fn startup_target_detects_explicit_image_or_resume_image() {
        let explicit = PathBuf::from("explicit.jpg");
        let resumed = PathBuf::from("resumed.jpg");

        assert!(has_startup_target(Some(&explicit), false, None));
        assert!(has_startup_target(None, true, Some(&resumed)));
        assert!(!has_startup_target(None, false, Some(&resumed)));
        assert!(!has_startup_target(None, true, None));
    }

    #[test]
    fn transition_preroll_starts_one_frame_in_progress() {
        assert_eq!(transition_preroll_duration(0), Duration::ZERO);
        assert_eq!(transition_preroll_duration(5), Duration::from_millis(4));
        assert_eq!(transition_preroll_duration(16), Duration::from_millis(15));
        assert_eq!(transition_preroll_duration(800), Duration::from_millis(16));
    }

    #[test]
    fn pending_transition_starts_only_when_target_texture_is_ready() {
        assert!(!can_start_pending_transition(Some(7), 6, true));
        assert!(!can_start_pending_transition(Some(7), 7, false));
        assert!(can_start_pending_transition(Some(7), 7, true));
    }

    #[test]
    fn target_render_ready_requires_hdr_plane_or_non_placeholder_sdr() {
        assert!(target_is_render_ready(true, false, false));
        assert!(!target_is_render_ready(true, false, true));
        assert!(target_is_render_ready(false, true, true));
        assert!(!target_is_render_ready(false, false, false));
    }

    #[test]
    fn transition_can_start_immediately_when_target_is_already_cached() {
        assert!(should_start_transition_immediately(true, true));
        assert!(!should_start_transition_immediately(false, true));
        assert!(!should_start_transition_immediately(true, false));
    }

    #[test]
    fn transition_source_texture_selection_clears_when_current_missing() {
        assert!(select_transition_source_texture(None, false, None).is_none());
    }

    #[test]
    fn transition_source_texture_skips_placeholder_fallback_frames() {
        assert!(select_transition_source_texture(None, true, None).is_none());
    }

    #[test]
    fn transition_source_selection_reuses_previous_when_current_unusable() {
        assert!(select_transition_source_texture(None, false, None).is_none());
        assert!(select_transition_source_texture(None, true, None).is_none());
    }

    #[test]
    fn transition_source_hdr_selection_clears_when_current_missing() {
        assert!(select_transition_source_hdr(None, false, None).is_none());
    }

    #[test]
    fn transition_source_hdr_skips_placeholder_fallback_frames() {
        assert!(select_transition_source_hdr(None, true, None).is_none());
    }

    #[test]
    fn transition_source_hdr_selection_reuses_previous_when_current_unusable() {
        let dummy_hdr = Arc::new(crate::hdr::types::HdrImageBuffer {
            width: 1,
            height: 1,
            format: crate::hdr::types::HdrPixelFormat::Rgba32Float,
            color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
            metadata: crate::hdr::types::HdrImageMetadata::from_color_space(
                crate::hdr::types::HdrColorSpace::LinearSrgb,
            ),
            rgba_f32: Arc::new(vec![0.0; 4]),
        });

        let res = select_transition_source_hdr(None, true, Some(Arc::clone(&dummy_hdr)));
        assert!(res.is_some());
        assert_eq!(res.unwrap().width, 1);
    }

    #[test]
    fn transition_source_hdr_happy_path_returns_current() {
        let dummy_hdr = Arc::new(crate::hdr::types::HdrImageBuffer {
            width: 1,
            height: 1,
            format: crate::hdr::types::HdrPixelFormat::Rgba32Float,
            color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
            metadata: crate::hdr::types::HdrImageMetadata::from_color_space(
                crate::hdr::types::HdrColorSpace::LinearSrgb,
            ),
            rgba_f32: Arc::new(vec![0.0; 4]),
        });

        let res = select_transition_source_hdr(Some(Arc::clone(&dummy_hdr)), false, None);
        assert!(res.is_some());
        assert_eq!(res.unwrap().width, 1);
    }

    #[test]
    fn transition_none_keeps_source_frame_until_target_is_ready() {
        assert!(!should_reset_transition_when_source_texture_missing(true));
        assert!(should_reset_transition_when_source_texture_missing(false));
    }

    #[test]
    fn navigation_direction_matches_wrap_aware_next_prev_behavior() {
        assert!(navigation_is_forward(1, 2, 10));
        assert!(!navigation_is_forward(2, 1, 10));
        assert!(navigation_is_forward(9, 0, 10));
        assert!(!navigation_is_forward(0, 9, 10));
    }

    #[test]
    fn image_file_size_pairs_keep_known_sizes_and_fill_missing_sizes() {
        let paths = vec![
            PathBuf::from("a.jpg"),
            PathBuf::from("b.jpg"),
            PathBuf::from("c.jpg"),
        ];

        let pairs = image_file_size_pairs_with_missing_sizes_as_zero(paths, vec![10, 20]);

        assert_eq!(
            pairs,
            vec![
                (PathBuf::from("a.jpg"), 10),
                (PathBuf::from("b.jpg"), 20),
                (PathBuf::from("c.jpg"), 0),
            ]
        );
    }

    #[test]
    fn asset_update_repaint_policy_centralizes_current_and_tile_rules() {
        assert!(should_request_repaint_for_asset_update(
            AssetUpdateKind::ImageLoaded,
            true,
            false
        ));
        assert!(!should_request_repaint_for_asset_update(
            AssetUpdateKind::ImageLoaded,
            false,
            false
        ));
        assert!(should_request_repaint_for_asset_update(
            AssetUpdateKind::PreviewUpgraded,
            true,
            false
        ));
        assert!(!should_request_repaint_for_asset_update(
            AssetUpdateKind::PreviewUpgraded,
            false,
            false
        ));
        assert!(should_request_repaint_for_asset_update(
            AssetUpdateKind::TileReady,
            true,
            true
        ));
        assert!(!should_request_repaint_for_asset_update(
            AssetUpdateKind::TileReady,
            true,
            false
        ));
        assert!(should_request_repaint_for_asset_update(
            AssetUpdateKind::RefinedFullPlane,
            true,
            false
        ));
    }

    #[test]
    fn tiled_manager_install_helper_preserves_source_and_generation() {
        let source: Arc<dyn crate::loader::TiledImageSource> = Arc::new(DummyTiledSource {
            width: 1024,
            height: 768,
        });

        let tm = build_tiled_manager_with_best_preview(9, 17, source, None);

        assert_eq!(tm.image_index, 9);
        assert_eq!(tm.generation, 17);
        assert_eq!(tm.full_width, 1024);
        assert_eq!(tm.full_height, 768);
    }

    #[test]
    fn current_hdr_tiled_preview_match_is_index_scoped() {
        let image = Arc::new(crate::hdr::types::HdrImageBuffer {
            width: 1,
            height: 1,
            format: crate::hdr::types::HdrPixelFormat::Rgba32Float,
            color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
            metadata: crate::hdr::types::HdrImageMetadata::from_color_space(
                crate::hdr::types::HdrColorSpace::LinearSrgb,
            ),
            rgba_f32: Arc::new(vec![0.0, 0.0, 0.0, 1.0]),
        });
        let current = crate::app::CurrentHdrImage::new(4, image);

        assert!(current_hdr_tiled_preview_matches_index(Some(&current), 4));
        assert!(!current_hdr_tiled_preview_matches_index(Some(&current), 5));
        assert!(!current_hdr_tiled_preview_matches_index(None, 4));
    }

    #[test]
    fn view_change_invalidates_only_tile_manager_generation() {
        let source: Arc<dyn crate::loader::TiledImageSource> = Arc::new(DummyTiledSource {
            width: 1024,
            height: 768,
        });
        let mut tile_manager = Some(TileManager::with_source(4, 9, source));
        let loader_generation = 3;

        assert!(invalidate_tile_manager_requests_for_view_change(
            &mut tile_manager
        ));

        let tile_manager = tile_manager.expect("tile manager should remain installed");
        assert_eq!(tile_manager.generation, 10);
        assert_eq!(loader_generation, 3);
    }

    #[test]
    fn hdr_load_result_capacity_is_stale_when_sensitive_hdr_mismatch() {
        let load = LoadResult {
            index: 0,
            generation: 1,
            source_key: 0,
            result: Ok(crate::loader::ImageData::Hdr {
                hdr: crate::hdr::types::HdrImageBuffer {
                    width: 1,
                    height: 1,
                    format: crate::hdr::types::HdrPixelFormat::Rgba32Float,
                    color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
                    metadata: crate::hdr::types::HdrImageMetadata::default(),
                    rgba_f32: Arc::new(vec![0.0; 4]),
                },
                fallback: crate::loader::DecodedImage::new(1, 1, vec![0, 0, 0, 255]),
            }),
            preview_bundle: PreviewBundle::initial(),
            ultra_hdr_capacity_sensitive: true,
            sdr_fallback_is_placeholder: false,
            target_hdr_capacity: 1.0,
        };
        assert!(hdr_load_result_capacity_is_stale(&load, 2.0));
        assert!(!hdr_load_result_capacity_is_stale(&load, 1.0));
    }

    #[test]
    fn hdr_load_result_capacity_is_stale_ignores_non_sensitive_loads() {
        let load = LoadResult {
            index: 0,
            generation: 1,
            source_key: 0,
            result: Ok(crate::loader::ImageData::Hdr {
                hdr: crate::hdr::types::HdrImageBuffer {
                    width: 1,
                    height: 1,
                    format: crate::hdr::types::HdrPixelFormat::Rgba32Float,
                    color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
                    metadata: crate::hdr::types::HdrImageMetadata::default(),
                    rgba_f32: Arc::new(vec![0.0; 4]),
                },
                fallback: crate::loader::DecodedImage::new(1, 1, vec![0, 0, 0, 255]),
            }),
            preview_bundle: PreviewBundle::initial(),
            ultra_hdr_capacity_sensitive: false,
            sdr_fallback_is_placeholder: false,
            target_hdr_capacity: 1.0,
        };
        assert!(!hdr_load_result_capacity_is_stale(&load, 4.0));
    }

    #[test]
    fn test_find_index_for_path_impl_matching() {
        let files = vec![
            PathBuf::from("H:/images/photo1.jpg"),
            PathBuf::from("H:/images/photo2.PNG"),
            PathBuf::from("H:/images/SUBDIR/photo3.jpg"),
        ];

        // 1. Exact match
        assert_eq!(
            find_index_for_path_impl(&files, &PathBuf::from("H:/images/photo1.jpg")),
            Some(0)
        );

        // 2. Case variation on extension
        assert_eq!(
            find_index_for_path_impl(&files, &PathBuf::from("H:/images/photo2.png")),
            Some(1)
        );

        // 3. Slash variations (Windows vs Unix style)
        assert_eq!(
            find_index_for_path_impl(&files, &PathBuf::from("H:\\images\\photo1.jpg")),
            Some(0)
        );

        // 4. Case variation in filename/directory
        assert_eq!(
            find_index_for_path_impl(&files, &PathBuf::from("h:/images/PHOTO2.png")),
            Some(1)
        );

        // 5. Subdirectory matching
        assert_eq!(
            find_index_for_path_impl(&files, &PathBuf::from("H:\\images\\SUBDIR\\photo3.jpg")),
            Some(2)
        );

        // 6. Not found
        assert_eq!(
            find_index_for_path_impl(&files, &PathBuf::from("H:/images/nonexistent.jpg")),
            None
        );
    }

    #[test]
    fn first_cached_hdr_or_tiled_preview_selection_rules() {
        use crate::hdr::types::{HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat};
        use std::sync::Arc;

        let mk = |tag: u8| {
            Arc::new(HdrImageBuffer {
                width: 1,
                height: 1,
                format: HdrPixelFormat::Rgba32Float,
                color_space: HdrColorSpace::LinearSrgb,
                metadata: HdrImageMetadata::default(),
                rgba_f32: Arc::new(vec![tag as f32, 0.0, 0.0, 1.0]),
            })
        };

        let static_hdr = mk(1);
        let preview_hdr = mk(4);

        let mut hdr_image_cache = HashMap::new();
        let mut hdr_tiled_preview_cache = HashMap::new();

        // 1. None cached -> returns None
        assert!(
            first_cached_hdr_or_tiled_preview_for_index(
                &hdr_image_cache,
                &HashMap::new(),
                None,
                &hdr_tiled_preview_cache,
                None,
                0
            )
            .is_none()
        );

        // 2. Only preview cached -> returns preview
        hdr_tiled_preview_cache.insert(0, Arc::clone(&preview_hdr));
        assert_eq!(
            first_cached_hdr_or_tiled_preview_for_index(
                &hdr_image_cache,
                &HashMap::new(),
                None,
                &hdr_tiled_preview_cache,
                None,
                0
            )
            .map(|b| b.rgba_f32[0]),
            Some(4.0)
        );

        // 3. Both cached -> prefers full static image (from first_cached_hdr_still_for_index)
        hdr_image_cache.insert(0, Arc::clone(&static_hdr));
        assert_eq!(
            first_cached_hdr_or_tiled_preview_for_index(
                &hdr_image_cache,
                &HashMap::new(),
                None,
                &hdr_tiled_preview_cache,
                None,
                0
            )
            .map(|b| b.rgba_f32[0]),
            Some(1.0)
        );

        // 4. current_hdr_tiled_preview fallback when caches hit miss
        hdr_tiled_preview_cache.clear();
        hdr_image_cache.clear();
        let fallback_hdr = mk(5);
        let current_image = crate::app::CurrentHdrImage::new(0, Arc::clone(&fallback_hdr));
        assert_eq!(
            first_cached_hdr_or_tiled_preview_for_index(
                &hdr_image_cache,
                &HashMap::new(),
                None,
                &hdr_tiled_preview_cache,
                Some(&current_image),
                0
            )
            .map(|b| b.rgba_f32[0]),
            Some(5.0)
        );
    }

    fn make_test_app() -> ImageViewerApp {
        let (file_op_tx, file_op_rx) = crossbeam_channel::unbounded();
        let (lightweight_file_op_tx, _lightweight_file_op_rx) = crossbeam_channel::unbounded();
        let (save_tx, _save_rx) = crossbeam_channel::unbounded();
        let (_save_error_tx, save_error_rx) = crossbeam_channel::unbounded();
        let (_ipc_tx, ipc_rx) = crossbeam_channel::unbounded();

        let hotkeys_draft_config = crate::hotkeys::model::default_hotkey_config_file();
        let hotkeys_runtime = crate::hotkeys::rebuild_runtime_state(&hotkeys_draft_config);
        let (hotkeys_save_tx, _hotkeys_save_rx) = crossbeam_channel::unbounded();
        let (_hotkeys_save_error_tx, hotkeys_save_error_rx) = crossbeam_channel::unbounded();

        #[cfg(target_os = "linux")]
        let requested_vulkan_hdr_metadata = eframe::egui_wgpu::RequestedVulkanHdrMetadata::new();
        #[cfg(target_os = "linux")]
        let last_vulkan_hdr_metadata = None;

        ImageViewerApp {
            settings: Settings::default(),
            image_files: Vec::new(),
            file_byte_len_by_index: Vec::new(),
            current_index: 0,
            initial_image: None,
            scanning: false,
            hardware_tier: HardwareTier::Medium,
            loader: ImageLoader::new(),
            texture_cache: TextureCache::new(10),
            hdr_capabilities: crate::hdr::capabilities::HdrCapabilities::sdr("test"),
            hdr_renderer: crate::hdr::renderer::HdrImageRenderer::new(),
            hdr_target_format: None,
            hdr_monitor_state: crate::hdr::monitor::HdrMonitorState::default(),
            cached_window_placement: None,
            cached_restore_placement: None,
            requested_target_format: eframe::egui_wgpu::RequestedSurfaceFormat::new(),
            active_target_format: eframe::egui_wgpu::ActiveSurfaceFormat::new(),
            requested_rgb10a2_pq_encode: eframe::egui_wgpu::RequestedRgb10a2PqEncode::new(),
            gamma22_display_scale: eframe::egui_wgpu::Gamma22DisplayScale::new(),
            vulkan_wsi_hdr_gates: eframe::egui_wgpu::VulkanWsiHdrGatesMailbox::new(),
            #[cfg(target_os = "linux")]
            requested_vulkan_hdr_metadata,
            #[cfg(target_os = "linux")]
            last_vulkan_hdr_metadata,
            last_logged_swap_chain_format_request: None,
            rgb10a2_pq_encode_requested: false,
            ultra_hdr_decode_capacity: 1.0,
            ultra_hdr_decode_output_mode: crate::hdr::types::HdrOutputMode::SdrToneMapped,
            current_hdr_image: None,
            hdr_image_cache: HashMap::new(),
            current_hdr_tiled_image: None,
            hdr_tiled_source_cache: HashMap::new(),
            current_hdr_tiled_preview: None,
            hdr_tiled_preview_cache: HashMap::new(),
            hdr_sdr_fallback_indices: HashSet::new(),
            hdr_placeholder_fallback_indices: HashSet::new(),
            hdr_in_flight_fallback_refinements: HashSet::new(),
            deferred_sdr_uploads: HashMap::new(),
            ultra_hdr_capacity_sensitive_indices: HashSet::new(),
            animation: None,
            pan_offset: Vec2::ZERO,
            zoom_factor: 1.0,
            last_switch_time: Instant::now(),
            slideshow_paused: true,
            random_slideshow_order_ready: false,
            audio: AudioPlayer::new(),
            music_seeking_target_ms: None,
            music_seek_timeout: None,
            music_hud_last_activity: Instant::now(),
            show_settings: false,
            last_show_settings: false,
            settings_tab: SettingsTab::Library,
            about_icon_texture: None,
            images_ever_loaded: false,
            status_message: String::new(),
            error_message: None,
            is_font_error: false,
            modal_generation: 0,
            pending_fullscreen: None,
            font_families: Vec::new(),
            font_families_rx: None,
            temp_font_size: None,
            generation: 0,
            prefetch_prev_generation: None,
            cached_music_count: None,
            cached_pixels_per_point: 1.0,
            active_modal: None,
            music_scan_rx: None,
            scanning_music: false,
            music_scan_cancel: None,
            music_scan_path: None,
            scan_rx: None,
            scan_cancel: None,
            current_image_res: None,
            prev_texture: None,
            prev_hdr_image: None,
            transition_start: None,
            pending_transition_target: None,
            is_next: true,
            active_transition: TransitionStyle::None,
            osd: crate::ui::osd::OsdRenderer::new(),
            last_minimized: false,
            last_frame_time: Instant::now(),
            ipc_rx,
            animation_cache: HashMap::new(),
            tile_manager: None,
            prefetched_tiles: HashMap::new(),
            theme_cache: SystemThemeCache::default(),
            cached_palette: ThemePalette::dark(),
            is_printing: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            print_status_rx: None,
            pending_anim_frames: None,
            file_op_rx,
            file_op_tx,
            lightweight_file_op_tx,
            last_mouse_wheel_nav: 0.0,
            last_keyboard_nav: None,
            save_tx,
            save_error_rx,
            last_save_error: None,
            saver_handle: None,
            preload_budget_forward: 100 * 1024 * 1024,
            preload_budget_backward: 100 * 1024 * 1024,
            context_menu_pos: None,
            current_rotation: 0,
            tile_upload_quota: 32,
            cached_audio_devices: Vec::new(),
            music_hud_drag_offset: Vec2::ZERO,
            hotkeys_runtime,
            hotkeys_draft_config,
            hotkeys_save_error_rx,
            hotkeys_save_tx,
            hotkeys_saver_handle: None,
            last_hotkeys_save_error: None,
            hotkeys_apply_success_at: None,
            hotkeys_load_error: None,
            startup_hotkeys_alert_shown: false,
            hotkeys_capture_target: None,
            hotkeys_selected_row: None,
            hotkeys_add_row_dialog_open: false,
            hotkeys_add_row_action: crate::hotkeys::model::HotkeyActionId::NextImage,
            hotkeys_add_row_capture_active: false,
            hotkeys_add_row_captured_key: None,
            hotkeys_add_row_need_key_hint: false,
            refresh_scan_in_progress: false,
            refresh_scan_slideshow_was_playing: false,
            refresh_anchor_path: None,
        }
    }

    struct DummyHdrTiledSource {
        width: u32,
        height: u32,
    }

    impl crate::hdr::tiled::HdrTiledSource for DummyHdrTiledSource {
        fn source_kind(&self) -> crate::hdr::tiled::HdrTiledSourceKind {
            crate::hdr::tiled::HdrTiledSourceKind::InMemory
        }
        fn width(&self) -> u32 {
            self.width
        }
        fn height(&self) -> u32 {
            self.height
        }
        fn color_space(&self) -> crate::hdr::types::HdrColorSpace {
            crate::hdr::types::HdrColorSpace::LinearSrgb
        }
        fn generate_hdr_preview(
            &self,
            _max_w: u32,
            _max_h: u32,
        ) -> Result<crate::hdr::types::HdrImageBuffer, String> {
            Ok(crate::hdr::types::HdrImageBuffer {
                width: 1,
                height: 1,
                format: crate::hdr::types::HdrPixelFormat::Rgba32Float,
                color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
                metadata: crate::hdr::types::HdrImageMetadata::default(),
                rgba_f32: Arc::new(vec![0.0; 4]),
            })
        }
        fn generate_sdr_preview(
            &self,
            _max_w: u32,
            _max_h: u32,
        ) -> Result<(u32, u32, Vec<u8>), String> {
            Ok((1, 1, vec![0, 0, 0, 255]))
        }
        fn extract_tile_rgba32f_arc(
            &self,
            _x: u32,
            _y: u32,
            width: u32,
            height: u32,
        ) -> Result<Arc<crate::hdr::tiled::HdrTileBuffer>, String> {
            Ok(Arc::new(crate::hdr::tiled::HdrTileBuffer::new(
                width,
                height,
                crate::hdr::types::HdrColorSpace::LinearSrgb,
                Arc::new(vec![0.0; width as usize * height as usize * 4]),
            )))
        }
    }

    impl crate::loader::TiledImageSource for DummyHdrTiledSource {
        fn width(&self) -> u32 {
            self.width
        }
        fn height(&self) -> u32 {
            self.height
        }
        fn extract_tile(&self, _x: u32, _y: u32, w: u32, h: u32) -> Arc<Vec<u8>> {
            Arc::new(vec![0; w as usize * h as usize * 4])
        }
        fn generate_preview(&self, _max_w: u32, _max_h: u32) -> (u32, u32, Vec<u8>) {
            (1, 1, vec![0, 0, 0, 255])
        }
        fn full_pixels(&self) -> Option<Arc<Vec<u8>>> {
            None
        }
    }

    #[test]
    fn evict_distant_prefetch_caches_evicts_all_distant_prefetched_tiled_and_hdr_resources() {
        use eframe::egui;

        let mut app = make_test_app();
        app.image_files = vec![
            PathBuf::from("img0.jpg"),
            PathBuf::from("img1.jpg"),
            PathBuf::from("img2.jpg"),
            PathBuf::from("img3.jpg"),
            PathBuf::from("img4.jpg"),
            PathBuf::from("img5.jpg"),
            PathBuf::from("img6.jpg"),
        ];
        // Circular distance checking: length = 7.
        // Current index is 0. Prefetch window is 2, so indices 0, 1, 2, 5, 6 are within the window.
        // Index 3 and 4 are distant.
        app.current_index = 0;

        let dummy_source = Arc::new(DummyHdrTiledSource {
            width: 1024,
            height: 768,
        });

        // 1. Setup a tiled image for index 3 (distant)
        let tm3 = TileManager::with_source(
            3,
            42,
            Arc::clone(&dummy_source) as Arc<dyn crate::loader::TiledImageSource>,
        );
        app.prefetched_tiles.insert(3, tm3);
        app.hdr_tiled_source_cache.insert(
            3,
            Arc::clone(&dummy_source) as Arc<dyn crate::hdr::tiled::HdrTiledSource>,
        );
        app.hdr_sdr_fallback_indices.insert(3);

        // Put preview texture in texture_cache
        let ctx = egui::Context::default();
        let color_image = egui::ColorImage::from_rgba_unmultiplied([1, 1], &[0, 0, 0, 255]);
        let handle = ctx.load_texture("test_tex", color_image, egui::TextureOptions::LINEAR);
        app.texture_cache.insert(3, handle, 1024, 768, true, 0, 7);

        // Assert they are populated before eviction
        assert!(app.prefetched_tiles.contains_key(&3));
        assert!(app.hdr_tiled_source_cache.contains_key(&3));
        assert!(app.hdr_sdr_fallback_indices.contains(&3));
        assert!(app.texture_cache.contains(3));

        // 2. Perform eviction
        app.evict_distant_prefetch_caches();

        // 3. Assert they are cleared after eviction
        assert!(!app.prefetched_tiles.contains_key(&3));
        assert!(!app.hdr_tiled_source_cache.contains_key(&3));
        assert!(!app.hdr_sdr_fallback_indices.contains(&3));
        assert!(!app.texture_cache.contains(3));
    }

    #[test]
    fn navigate_to_tiled_preview_without_tile_manager_triggers_load() {
        use eframe::egui;

        let mut app = make_test_app();
        app.image_files = vec![PathBuf::from("img0.jpg"), PathBuf::from("img1.jpg")];
        app.current_index = 0;

        // Put a tiled preview texture in texture_cache for index 1
        let ctx = egui::Context::default();
        let color_image = egui::ColorImage::from_rgba_unmultiplied([1, 1], &[0, 0, 0, 255]);
        let handle = ctx.load_texture("test_tex", color_image, egui::TextureOptions::LINEAR);
        // insert with tiled=true
        app.texture_cache.insert(1, handle, 2048, 1536, true, 0, 2);

        // We have no tiled source, and no TileManager
        assert!(app.texture_cache.contains(1));
        assert!(app.texture_cache.is_preview_placeholder(1));
        assert!(app.tile_manager.is_none());
        assert!(!app.prefetched_tiles.contains_key(&1));
        assert!(!app.hdr_tiled_source_cache.contains_key(&1));

        // Now navigate to index 1
        app.navigate_to(1);

        // Verify it sets resolution backfill and triggers a loader request
        assert_eq!(app.current_image_res, Some((2048, 1536)));
        assert!(app.loader.is_loading(1, app.generation));
    }
}
