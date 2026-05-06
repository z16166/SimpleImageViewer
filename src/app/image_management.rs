use crate::app::{
    AnimationPlayback, FileOpResult, ImageViewerApp, PendingAnimUpload, TransitionStyle,
};
use crate::app::{MAX_PRELOAD_BACKWARD, MAX_PRELOAD_FORWARD};
use crate::loader::{
    DecodedImage, ImageData, LoadResult, LoaderOutput, PixelPlaneKind, PreviewPlane, PreviewResult,
    RenderShape as LoadedRenderShape, TileResult,
};
use crate::scanner::{self, ScanMessage};
use crate::tile_cache::TileManager;
use eframe::egui::{self, ColorImage, TextureOptions, Vec2};
use rand::seq::SliceRandom;
use rust_i18n::t;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

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
    Error {
        error: &'a String,
    },
}

impl<'a> ImageInstallPlan<'a> {
    fn from_load_result(load_result: &'a LoadResult) -> Self {
        let _preview_stage = load_result.preview_bundle.stage();
        let Ok(image_data) = load_result.result.as_ref() else {
            return Self::Error {
                error: load_result.result.as_ref().err().expect("load error"),
            };
        };

        match image_data.preferred_render_shape() {
            LoadedRenderShape::Static if image_data.has_plane(PixelPlaneKind::Hdr) => {
                Self::StaticHdr {
                    hdr: Arc::new(
                        image_data
                            .static_hdr()
                            .expect("static HDR image exposes HDR plane")
                            .clone(),
                    ),
                    fallback: image_data
                        .static_sdr()
                        .expect("static HDR image exposes SDR fallback plane"),
                    ultra_hdr_capacity_sensitive: load_result.ultra_hdr_capacity_sensitive,
                }
            }
            LoadedRenderShape::Static => Self::StaticSdr {
                decoded: image_data
                    .static_sdr()
                    .expect("static SDR image exposes SDR plane"),
            },
            LoadedRenderShape::Tiled => {
                let source = image_data
                    .tiled_sdr_source()
                    .expect("tiled image exposes SDR source");
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
                _ => unreachable!("animated render shape is only emitted by animated image data"),
            },
        }
    }
}

impl ImageViewerApp {
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
            self.settings.hdr_tone_map_settings(),
            self.hdr_capabilities.output_mode,
            self.hdr_monitor_state.selection(),
        )
    }

    pub(crate) fn refresh_ultra_hdr_decode_capacity(&mut self, ctx: &egui::Context) {
        const CAPACITY_EPSILON: f32 = 0.001;
        let next_capacity = self.effective_ultra_hdr_decode_capacity();
        if (next_capacity - self.ultra_hdr_decode_capacity).abs() <= CAPACITY_EPSILON {
            return;
        }

        let previous_capacity = self.ultra_hdr_decode_capacity;
        self.ultra_hdr_decode_capacity = next_capacity;
        self.loader.set_hdr_target_capacity(next_capacity);
        self.loader
            .set_hdr_tone_map_settings(self.settings.hdr_tone_map_settings());
        log::info!(
            "[HDR] ultra_hdr_decode_capacity changed {:.3} -> {:.3}",
            previous_capacity,
            next_capacity
        );

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
        if crate::app::capacity_refresh_should_cancel_loads(&refresh) {
            self.loader.cancel_all();
        }
        if refresh.indices_to_invalidate.is_empty() {
            ctx.request_repaint();
            return;
        }

        for idx in &refresh.indices_to_invalidate {
            self.texture_cache.remove(*idx);
            self.prefetched_tiles.remove(idx);
            if let Ok(mut cache) = crate::tile_cache::PIXEL_CACHE.lock() {
                cache.remove_image(*idx);
            }
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

    fn handle_texture_cache_eviction(&mut self, evicted_idx: usize) {
        self.animation_cache.remove(&evicted_idx);
        self.remove_hdr_image_index(evicted_idx);
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
        self.image_files.clear();
        self.current_index = 0;
        self.texture_cache.clear_all();
        self.clear_hdr_image_state();
        self.animation_cache.clear();
        self.animation = None;
        self.prev_texture = None;
        self.transition_start = None;
        self.tile_manager = None;
        self.prefetched_tiles.clear();
        if let Ok(mut cache) = crate::tile_cache::PIXEL_CACHE.lock() {
            cache.clear();
        }
        self.current_image_res = None;
        self.loader.cancel_all();
        self.pan_offset = Vec2::ZERO;
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
        if self.image_files.is_empty() {
            return;
        }

        let target_index = new_index % self.image_files.len();
        if target_index == self.current_index {
            return;
        }

        // Setup transition if enabled
        if self.settings.transition_style != TransitionStyle::None {
            if self.settings.transition_style == TransitionStyle::Random {
                // Pick a random style from the pool using rand for uniform distribution
                let pool = TransitionStyle::RANDOM_POOL;
                self.active_transition = *pool
                    .choose(&mut rand::thread_rng())
                    .unwrap_or(&TransitionStyle::Fade);
            } else {
                self.active_transition = self.settings.transition_style;
            }

            if let Some(tex) = self.texture_cache.get(self.current_index) {
                self.prev_texture = Some(tex.clone());
                self.transition_start = Some(Instant::now());
                // Handle wrap-around logic for direction
                self.is_next = target_index > self.current_index
                    || (target_index == 0 && self.current_index == self.image_files.len() - 1);
            }
        } else {
            self.active_transition = TransitionStyle::None;
        }

        preserve_current_tile_manager_for_navigation(
            self.current_index,
            target_index,
            &mut self.tile_manager,
            &mut self.prefetched_tiles,
        );
        self.current_index = target_index;
        self.current_hdr_image = self
            .hdr_image_cache
            .get(&self.current_index)
            .cloned()
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
            self.animation = Some(AnimationPlayback {
                image_index: cached_anim.image_index,
                textures: cached_anim.textures.clone(),
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

            log::info!(
                "[App] Cache Hit: Restored prefetched TileManager for index {} (prefetch_gen={} → current_gen={})",
                self.current_index, prefetch_gen, self.generation
            );
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

        // Housekeeping: evict stale prefetched TileManagers to prevent memory leaks
        let len = self.image_files.len();
        self.prefetched_tiles.retain(|&idx, _| {
            if len == 0 {
                return false;
            }
            let dist_forward = (idx + len - self.current_index % len) % len;
            let dist_backward = (self.current_index + len - idx % len) % len;
            let circular_distance = dist_forward.min(dist_backward);

            // Keep tiles only within distance 2
            circular_distance <= 2
        });

        self.schedule_preloads(true);
        self.loader.discard_pending_stale_outputs(self.generation);
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

            let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);

            // After the guaranteed first image, enforce the byte budget
            if count > 0 && new_bytes + file_size > budget {
                break;
            }

            self.loader.request_load(
                idx,
                self.generation,
                path.clone(),
                self.settings.raw_high_quality,
            );
            count += 1;
            new_bytes += file_size;
        }
    }

    fn has_loaded_asset(&self, index: usize) -> bool {
        current_image_has_loaded_asset(
            self.texture_cache.contains(index),
            self.hdr_image_cache.contains_key(&index),
            self.hdr_tiled_source_cache.contains_key(&index),
            self.animation_cache.contains_key(&index),
        )
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
                            self.image_files.insert(original_idx, path);
                        } else {
                            self.image_files.push(path);
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
                FileOpResult::Wallpaper(current) => {
                    if let Some(crate::ui::dialogs::modal_state::ActiveModal::Wallpaper(
                        ref mut state,
                    )) = self.active_modal
                    {
                        state.current_system_wallpaper = current;
                        state.loading = false;
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

        // Drain all available messages this frame (non-blocking)
        loop {
            match rx.try_recv() {
                Ok(msg) => {
                    match msg {
                        ScanMessage::Batch(mut batch) => {
                            let is_first_batch = self.image_files.is_empty();
                            self.image_files.append(&mut batch);

                            let count = self.image_files.len();
                            self.status_message =
                                t!("status.found", count = count.to_string()).to_string();

                            // On first batch: resolve initial position and start preloading immediately
                            if is_first_batch && count > 0 {
                                self.resolve_initial_position();
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
                            } else {
                                // Re-sort the full list now that all batches have arrived.
                                self.image_files.sort();

                                // CRITICAL: Global sort finished - all previous index-based caches
                                // and pending loads are now potentially stale/incorrect.
                                // We must bump generation and clear index-keyed state.
                                self.generation = self.generation.wrapping_add(1);
                                self.loader.set_generation(self.generation);

                                // Clear all state that depends on stable indices
                                self.texture_cache.clear_all();
                                self.clear_hdr_image_state();
                                self.prefetched_tiles.clear();
                                self.animation = None;
                                self.animation_cache.clear();
                                self.pending_anim_frames = None;
                                self.tile_manager = None;
                                if let Ok(mut cache) = crate::tile_cache::PIXEL_CACHE.lock() {
                                    cache.clear();
                                }

                                // Re-resolve position after global sort (indices may have shifted)
                                self.resolve_initial_position();

                                let count = self.image_files.len();
                                self.status_message =
                                    t!("status.found", count = count.to_string()).to_string();
                                self.schedule_preloads(true);
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
                    break;
                }
            }
        }

        if should_schedule_first_batch_preload(
            first_batch_preload_pending,
            self.image_files.len(),
            done,
        ) {
            self.schedule_preloads(true);
        }

        if !done {
            // Put the receiver back if scanning is still in progress
            self.scan_rx = Some(rx);
        }
    }

    /// Resolve the starting image index from initial_image or resume settings.
    pub(crate) fn resolve_initial_position(&mut self) {
        if let Some(ref path) = self.initial_image {
            // Fast path: try direct path comparison first (no syscalls)
            let found = self.image_files.iter().position(|p| p == path);
            let found = found.or_else(|| {
                // Fallback: canonicalize only the target, then compare
                // with case-insensitive file names to handle path variations
                // without calling canonicalize() on every file in the list.
                let target = path.canonicalize().unwrap_or_else(|_| path.clone());
                let target_name = target
                    .file_name()
                    .map(|n| n.to_string_lossy().to_lowercase());
                self.image_files.iter().position(|p| {
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
            });
            if let Some(pos) = found {
                self.current_index = pos;
            }
            self.initial_image = None;
        } else if self.settings.resume_last_image {
            if let Some(last_path) = &self.settings.last_viewed_image {
                if let Some(pos) = self.image_files.iter().position(|p| p == last_path) {
                    self.current_index = pos;
                }
            }
        }
    }

    /// Process results from the background ImageLoader.
    pub(crate) fn process_loaded_images(&mut self, ctx: &egui::Context) {
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
                    delays: std::mem::take(&mut pending.delays),
                    current_frame: 0,
                    frame_start: Instant::now(),
                };

                if idx == self.current_index {
                    self.animation = Some(AnimationPlayback {
                        image_index: playback.image_index,
                        textures: playback.textures.clone(),
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
                    let is_current = idx == self.current_index;
                    let gen_match = load_result.generation == self.generation;

                    // CRITICAL: Drop any stale results, even for the current index.
                    // This prevents a race where deleting an image reuses the index
                    // but a late decode from the deleted file arrives and overwrites
                    // the new current image state.
                    if !gen_match {
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

                    self.handle_image_load_result(load_result, ctx);
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
                    if update.generation != self.generation {
                        continue;
                    }
                    if !is_current && uploads_this_frame >= GLOBAL_UPLOAD_QUOTA {
                        self.loader
                            .repush(LoaderOutput::HdrSdrFallback(update));
                        ctx.request_repaint();
                        break;
                    }
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
    }

    /// Handles a Refined notification: bumps generation so TileManager
    /// re-fetches tiles from the newly developed high-resolution buffer.
    fn handle_refined_notification(&mut self, idx: usize, gen_id: u64, ctx: &egui::Context) {
        if idx == self.current_index && gen_id == self.generation {
            log::info!("[App] Refined image notification for index={}", idx);

            if let Ok(mut cache) = crate::tile_cache::PIXEL_CACHE.lock() {
                cache.remove_image(idx);
            }

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
            if let Ok(mut cache) = crate::tile_cache::PIXEL_CACHE.lock() {
                cache.remove_image(idx);
            }
            self.prefetched_tiles.remove(&idx);
            self.texture_cache.remove(idx);
            self.remove_hdr_image_index(idx);
        }
    }

    pub(crate) fn handle_image_load_result(
        &mut self,
        load_result: LoadResult,
        ctx: &egui::Context,
    ) {
        let idx = load_result.index;
        let generation = load_result.generation;
        let preview_bundle = load_result.preview_bundle.clone();

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
            ImageInstallPlan::Error { error } => {
                self.install_image_error(idx, error);
            }
        }
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
        self.upload_static_sdr_texture(idx, decoded, format!("img_{idx}"), ctx);
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
        ultra_hdr_capacity_sensitive: bool,
        ctx: &egui::Context,
    ) {
        self.remove_hdr_image_index(idx);
        self.hdr_image_cache.insert(idx, Arc::clone(&hdr));
        if ultra_hdr_capacity_sensitive {
            self.ultra_hdr_capacity_sensitive_indices.insert(idx);
        }

        self.upload_static_sdr_texture(idx, fallback, format!("img_hdr_fallback_{idx}"), ctx);

        if idx == self.current_index {
            self.current_image_res = Some((hdr.width, hdr.height));
            self.current_hdr_image = Some(crate::app::CurrentHdrImage::new(idx, Arc::clone(&hdr)));
            self.tile_manager = None;
            self.clear_current_animation_for_index(idx);
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
        self.upload_static_sdr_texture(
            idx,
            &update.fallback,
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
            self.upload_static_sdr_texture(idx, &decoded, format!("img_{idx}"), ctx);
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
                frames: frames.to_vec(),
                textures: Vec::new(),
                delays: Vec::new(),
                next_frame: 0,
            });
            ctx.request_repaint();
        }
    }

    fn install_image_error(&mut self, idx: usize, error: &str) {
        let path_str = self.image_files[idx].display().to_string();
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
            let file_name = self.image_files[update.index]
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
            let file_name = self.image_files[update.index]
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown");
            log::info!(
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
                        log::info!(
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
                        log::info!(
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
                log::warn!(
                    "Preview update for index {} carried no SDR preview plane",
                    update.index
                );
            }
        }
    }

    pub(crate) fn log_large_image(&self, idx: usize, w: u32, h: u32) {
        let file_name = self.image_files[idx]
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
        let file_name = self.image_files[idx]
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

fn should_schedule_first_batch_preload(
    is_first_batch: bool,
    count: usize,
    scan_done: bool,
) -> bool {
    is_first_batch && count > 0 && !scan_done
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

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

        cache_hdr_tiled_preview_state(7, 7, &mut cache, &mut current, Some(Arc::clone(&initial)), "test.exr");
        cache_hdr_tiled_preview_state(7, 7, &mut cache, &mut current, Some(Arc::clone(&refined)), "test.exr");
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
    fn first_batch_preload_waits_when_scan_done_is_already_available() {
        assert!(!should_schedule_first_batch_preload(true, 3, true));
        assert!(should_schedule_first_batch_preload(true, 3, false));
        assert!(!should_schedule_first_batch_preload(false, 3, false));
        assert!(!should_schedule_first_batch_preload(true, 0, false));
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
}
