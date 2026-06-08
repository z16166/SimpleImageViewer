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

#[cfg(feature = "preload-debug")]
macro_rules! preload_debug {
    ($($arg:tt)*) => {
        log::info!($($arg)*);
    };
}

#[cfg(not(feature = "preload-debug"))]
macro_rules! preload_debug {
    ($($arg:tt)*) => {};
}

mod cache_eviction;
mod directory;
mod hdr_state;
mod image_install;
mod loader_results;
mod navigation;
mod preload;
mod preview;
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

const BYTES_PER_MIB: usize = 1024 * 1024;
const LOW_TIER_SDR_UPLOAD_BUDGET_BYTES_PER_FRAME: usize = 16 * BYTES_PER_MIB;
const MEDIUM_TIER_SDR_UPLOAD_BUDGET_BYTES_PER_FRAME: usize = 32 * BYTES_PER_MIB;
const HIGH_TIER_SDR_UPLOAD_BUDGET_BYTES_PER_FRAME: usize = 64 * BYTES_PER_MIB;

fn sdr_upload_budget_bytes_per_frame(hardware_tier: crate::app::HardwareTier) -> usize {
    match hardware_tier {
        crate::app::HardwareTier::Low => LOW_TIER_SDR_UPLOAD_BUDGET_BYTES_PER_FRAME,
        crate::app::HardwareTier::Medium => MEDIUM_TIER_SDR_UPLOAD_BUDGET_BYTES_PER_FRAME,
        crate::app::HardwareTier::High => HIGH_TIER_SDR_UPLOAD_BUDGET_BYTES_PER_FRAME,
    }
}

fn decoded_rgba_bytes(width: u32, height: u32) -> usize {
    width as usize * height as usize * 4
}

fn should_upload_sdr_this_frame(
    is_current: bool,
    uploaded_bytes: usize,
    candidate_bytes: usize,
    max_bytes: usize,
) -> bool {
    is_current || uploaded_bytes == 0 || uploaded_bytes.saturating_add(candidate_bytes) <= max_bytes
}

fn should_defer_background_upload_during_transition(
    is_current: bool,
    is_transitioning: bool,
    transition_settled_at: Option<std::time::Instant>,
) -> bool {
    // Neighbor preloads can wait, but the navigation target (current index) must install during
    // the transition so GPU bindings are warm before the last frame — otherwise the transition
    // end flashes while deferred SDR/HDR uploads land (see fox.png 13→14 in preload logs).
    if is_current {
        return false;
    }
    if is_transitioning {
        return true;
    }
    // Keep neighbor/background uploads quiet for roughly one user-visible beat after the
    // static frame settles. The 1s window covers slow HDR fallback/preview arrivals observed
    // immediately after page-flip without making preloading feel disabled after navigation.
    const POST_TRANSITION_BACKGROUND_HOLD: std::time::Duration =
        std::time::Duration::from_millis(1000);
    transition_settled_at.is_some_and(|t| t.elapsed() < POST_TRANSITION_BACKGROUND_HOLD)
}

fn should_yield_background_result_for_pending_transition(
    is_current: bool,
    pending_transition_target: Option<usize>,
    current_index: usize,
) -> bool {
    !is_current && pending_transition_target == Some(current_index)
}

fn should_yield_background_result_for_post_transition_refinement(
    is_current: bool,
    transition_settled_at: Option<std::time::Instant>,
    current_refinement_pending: bool,
) -> bool {
    if is_current || !current_refinement_pending {
        return false;
    }
    // Give the current image's own refinement a short priority lane after transition settle.
    // 500ms is long enough for the queued current refinement to surface on busy folders, but
    // short enough that neighboring previews resume before the next deliberate navigation.
    const POST_TRANSITION_REFINEMENT_PRIORITY: std::time::Duration =
        std::time::Duration::from_millis(500);
    transition_settled_at.is_some_and(|t| t.elapsed() < POST_TRANSITION_REFINEMENT_PRIORITY)
}

fn background_upload_quota_after_transition(
    default_quota: usize,
    transition_settled_at: Option<std::time::Instant>,
) -> usize {
    // For the first few seconds after a transition, allow only one background GPU upload per
    // frame. This drains nearby preloads steadily while avoiding the burst that originally
    // caused visible hitches on large HDR/JPEG/RAW folders.
    const POST_TRANSITION_THROTTLE: std::time::Duration = std::time::Duration::from_millis(3000);
    if transition_settled_at.is_some_and(|t| t.elapsed() < POST_TRANSITION_THROTTLE) {
        1
    } else {
        default_quota
    }
}

fn should_space_background_upload_after_transition(
    is_current: bool,
    transition_settled_at: Option<std::time::Instant>,
    last_background_upload_at: Option<std::time::Instant>,
) -> bool {
    if is_current {
        return false;
    }
    // Pair the 3s throttle window with a 250ms minimum gap between non-current uploads. That
    // spreads large texture uploads across many frames (~4/s) instead of letting a single quiet
    // frame trigger another burst right after the animation.
    const POST_TRANSITION_SPACING_WINDOW: std::time::Duration =
        std::time::Duration::from_millis(3000);
    const POST_TRANSITION_BACKGROUND_UPLOAD_SPACING: std::time::Duration =
        std::time::Duration::from_millis(250);
    transition_settled_at.is_some_and(|settled| {
        settled.elapsed() < POST_TRANSITION_SPACING_WINDOW
            && last_background_upload_at
                .is_some_and(|last| last.elapsed() < POST_TRANSITION_BACKGROUND_UPLOAD_SPACING)
    })
}

#[cfg(test)]
fn should_defer_refinement_during_transition(is_transitioning: bool) -> bool {
    // Refined SDR fallbacks and HQ preview swaps for the current image still wait until the
    // transition finishes; those mid-animation updates can flash dim SDR over HDR page-flip.
    is_transitioning
}

fn should_defer_preview_update_during_transition(is_current: bool, is_transitioning: bool) -> bool {
    // Current tiled preview upgrades improve the image already being drawn and should not be
    // held behind the same background-upload gate used for neighboring previews.
    !is_current && is_transitioning
}

fn preview_result_has_sdr_upload(update: &crate::loader::PreviewResult) -> bool {
    update.preview_bundle.sdr().is_some()
}

fn should_drop_placeholder_sdr_transition_source(
    placeholder: bool,
    has_hdr: bool,
    hdr_output_available: bool,
) -> bool {
    placeholder && has_hdr && hdr_output_available
}

/// Hold off refined SDR fallback GPU uploads for the navigation target briefly after the
/// transition animation ends. Applying them on the same frame as `transition_start` clears
/// re-uploads the 8-bit cache and retriggers ISO/Apple HDR compose (see preload logs:
/// `install hdr_sdr_fallback` immediately after the last defer loop).
pub(crate) fn should_defer_hdr_sdr_fallback_install(
    is_current: bool,
    is_transitioning: bool,
    transition_settled_at: Option<std::time::Instant>,
) -> bool {
    if !is_current {
        return false;
    }
    if is_transitioning {
        return true;
    }
    const POST_TRANSITION_REFINEMENT_HOLD: std::time::Duration =
        std::time::Duration::from_millis(50);
    transition_settled_at.is_some_and(|t| t.elapsed() < POST_TRANSITION_REFINEMENT_HOLD)
}

const MIN_AVAILABLE_MEMORY_FOR_BACKGROUND_PRELOAD_MB: u64 = 1024;
const MAX_AVAILABLE_MEMORY_FOR_BACKGROUND_PRELOAD_MB: u64 = 4096;
const BACKGROUND_PRELOAD_MEMORY_RESERVE_DIVISOR: u64 = 5;

fn background_preload_memory_guard_threshold_mb(total_memory_mb: u64) -> u64 {
    let proportional_reserve =
        total_memory_mb.saturating_div(BACKGROUND_PRELOAD_MEMORY_RESERVE_DIVISOR);
    proportional_reserve.clamp(
        MIN_AVAILABLE_MEMORY_FOR_BACKGROUND_PRELOAD_MB,
        MAX_AVAILABLE_MEMORY_FOR_BACKGROUND_PRELOAD_MB,
    )
}

fn should_skip_background_preloads_for_memory(
    available_memory_mb: u64,
    total_memory_mb: u64,
) -> bool {
    available_memory_mb < background_preload_memory_guard_threshold_mb(total_memory_mb)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreloadBudgetDecision {
    Request,
    SkipCandidate,
    StopDirection,
}

const LARGE_FILE_TILED_PRELOAD_CANDIDATE_BYTES: u64 = 64 * 1024 * 1024;
const NEAR_BUDGET_PRELOAD_NUMERATOR: u64 = 3;
const NEAR_BUDGET_PRELOAD_DENOMINATOR: u64 = 2;
const PRELOAD_DECODE_SIZE_MULTIPLIER: u64 = 12;

fn estimate_preload_decode_bytes(file_size: u64) -> u64 {
    if file_size > 0 {
        // Compressed JPEG/HEIC/TIFF/PSD sources routinely expand by an order of
        // magnitude once represented as RGBA. 12x is intentionally conservative:
        // it avoids the old "one large compressed file becomes several full
        // decoded frames" memory spike while still admitting small nearby images.
        file_size.saturating_mul(PRELOAD_DECODE_SIZE_MULTIPLIER)
    } else {
        0
    }
}

fn should_request_oversized_preload_candidate(
    file_size: u64,
    candidate_bytes: u64,
    budget: u64,
) -> bool {
    // Oversized candidates are usually skipped so background preloading cannot
    // decode several full RGBA images at once. Two cases are still worth probing:
    // 1. "near budget" files (<= 1.5x) where the estimate is only slightly over;
    // 2. very large files, which often become disk-backed tiled sources and only
    //    need a lightweight bootstrap preview. Each accepted oversized candidate
    //    is charged as a full budget slot by `preload_direction`.
    let near_budget_limit = budget
        .saturating_mul(NEAR_BUDGET_PRELOAD_NUMERATOR)
        .saturating_div(NEAR_BUDGET_PRELOAD_DENOMINATOR);
    candidate_bytes > budget
        && (candidate_bytes <= near_budget_limit
            || file_size >= LARGE_FILE_TILED_PRELOAD_CANDIDATE_BYTES)
}

fn decide_preload_for_budget(
    count: usize,
    new_bytes: u64,
    candidate_bytes: u64,
    budget: u64,
) -> PreloadBudgetDecision {
    if candidate_bytes == 0 || new_bytes.saturating_add(candidate_bytes) <= budget {
        return PreloadBudgetDecision::Request;
    }
    if count == 0 || new_bytes == 0 {
        PreloadBudgetDecision::SkipCandidate
    } else {
        PreloadBudgetDecision::StopDirection
    }
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

fn transition_direction_is_next(current_index: usize, target_index: usize, total: usize) -> bool {
    navigation_is_forward(current_index, target_index, total)
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

#[cfg(test)]
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

#[cfg(test)]
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

#[cfg(test)]
fn select_transition_source_hdr(
    current_hdr_image: Option<Arc<crate::hdr::types::HdrImageBuffer>>,
    current_has_placeholder_fallback: bool,
    previous_transition_hdr_image: Option<Arc<crate::hdr::types::HdrImageBuffer>>,
) -> Option<Arc<crate::hdr::types::HdrImageBuffer>> {
    // HDR float planes are always authoritative for the outgoing frame. The placeholder flag
    // only applies to the companion SDR fallback texture.
    if current_hdr_image.is_some() {
        return current_hdr_image;
    }
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
        sdr_preview: Option<&'a DecodedImage>,
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
                    sdr_preview: load_result.preview_bundle.sdr(),
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

    fn estimated_sdr_upload_bytes(&self) -> usize {
        match self {
            Self::StaticSdr { decoded }
            | Self::StaticHdr {
                fallback: decoded, ..
            } => decoded_rgba_bytes(decoded.width, decoded.height),
            Self::Tiled { sdr_preview, .. } => sdr_preview
                .map(|preview| decoded_rgba_bytes(preview.width, preview.height))
                .unwrap_or(0),
            Self::Animated { .. } | Self::HdrAnimated { .. } | Self::Error { .. } => 0,
        }
    }
}
impl ImageViewerApp {
    pub(crate) fn trigger_current_hdr_fallback_refinement_if_needed(&mut self) {
        if self.transition_start.is_some() {
            return;
        }
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
mod tests;
