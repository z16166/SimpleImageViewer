use crate::app::{
    AnimationPlayback, FileOpResult, ImageViewerApp, PendingAnimUpload, TransitionStyle,
};
use crate::app::{MAX_PRELOAD_BACKWARD, MAX_PRELOAD_FORWARD};
use crate::loader::{DecodedImage, ImageData, LoadResult, LoaderOutput, PreviewResult, TileResult};
use crate::scanner::{self, ScanMessage};
use crate::tile_cache::{TileCoord, TileManager};
use eframe::egui::{self, ColorImage, TextureOptions, Vec2};
use rand::seq::SliceRandom;
use rust_i18n::t;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

impl ImageViewerApp {
    // ------------------------------------------------------------------
    // Directory loading
    // ------------------------------------------------------------------

    pub(crate) fn open_directory_dialog(&mut self) {
        let mut dialog = rfd::FileDialog::new();
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
        self.animation_cache.clear();
        self.animation = None;
        self.prev_texture = None;
        self.transition_start = None;
        self.tile_manager = None;
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

        if self.current_index != target_index {
            // Clear tiled rendering state when switching images
            self.tile_manager = None;
        }
        self.current_index = target_index;
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
            // The prefetch completed previously (or is still decoding in background).
            // We MUST update its generation to match the current navigation sequence,
            // otherwise its internal tile queue matching will fail.
            self.generation = self.generation.wrapping_add(1);
            self.loader.set_generation(self.generation);

            tm.generation = self.generation;
            self.current_image_res = Some((tm.full_width, tm.full_height));

            // Trigger deferred refinement now that this image is actively viewed.
            // Prefetched RAW images defer refinement to avoid ~400MB develop allocations
            // for images the user might never actually look at.
            tm.get_source()
                .request_refinement(self.current_index, self.generation);

            self.tile_manager = Some(tm);

            log::info!(
                "[App] Cache Hit: Restored prefetched TileManager for index {}",
                self.current_index
            );
        } else {
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

        // Always load the current image
        if !self.texture_cache.contains(cur) && !self.loader.is_loading(cur) {
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
            if self.texture_cache.contains(idx) || self.loader.is_loading(idx) {
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

    // ------------------------------------------------------------------
    // Background result processing
    // ------------------------------------------------------------------

    pub(crate) fn process_file_op_results(&mut self) {
        while let Ok(res) = self.file_op_rx.try_recv() {
            match res {
                FileOpResult::Delete(path, res) => {
                    if let Err(e) = res {
                        log::error!("Failed to delete {:?}: {}", path, e);
                        self.error_message =
                            Some(t!("status.delete_failed", err = e.to_string()).to_string());
                    } else {
                        log::info!("Successfully deleted {:?}", path);
                    }
                }
                FileOpResult::Exif(_path, data) => {
                    if let Some(crate::ui::dialogs::modal_state::ActiveModal::Exif(ref mut state)) =
                        self.active_modal
                    {
                        state.data = data;
                        state.loading = false;
                    }
                }
                FileOpResult::Xmp(_path, data) => {
                    if let Some(crate::ui::dialogs::modal_state::ActiveModal::Xmp(ref mut state)) =
                        self.active_modal
                    {
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

        // Drain all available messages this frame (non-blocking)
        loop {
            match rx.try_recv() {
                Ok(ScanMessage::Batch(mut batch)) => {
                    let is_first_batch = self.image_files.is_empty();
                    self.image_files.append(&mut batch);

                    let count = self.image_files.len();
                    self.status_message = t!("status.found", count = count.to_string()).to_string();

                    // On first batch: resolve initial position and start preloading immediately
                    if is_first_batch && count > 0 {
                        self.resolve_initial_position();
                        // Auto-close the settings panel only during the very first
                        // startup scan (images_ever_loaded == false). If the user is
                        // already browsing images and triggers a rescan from within the
                        // settings panel (e.g. toggling recursive scan), keep it open.
                        if !self.images_ever_loaded {
                            self.show_settings = false;
                        }
                        self.images_ever_loaded = true;
                        self.schedule_preloads(true);
                    }
                }
                Ok(ScanMessage::Done) => {
                    done = true;
                    self.scanning = false;

                    if self.image_files.is_empty() {
                        self.status_message = t!("status.not_found").to_string();
                    } else {
                        // Re-sort the full list now that all batches have arrived.
                        // Each batch was individually sorted, but interleaving from
                        // parallel workers means the combined list may not be sorted.
                        self.image_files.sort();

                        // CRITICAL: Global sort finished - all previous index-based caches
                        // and pending loads are now potentially stale/incorrect.
                        // We must bump generation and clear index-keyed state.
                        self.generation = self.generation.wrapping_add(1);
                        self.loader.set_generation(self.generation);

                        // Clear caches that depend on stable indices
                        self.texture_cache.clear_all();
                        self.prefetched_tiles.clear();
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
                Err(_) => break,
            }
        }

        // Put the receiver back if scanning is still in progress
        if !done {
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
                    &frame.pixels,
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

                    if is_current {
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
            } else {
                log::warn!(
                    "[App] Refined: Static mode encountered unexpectedly. Attempting to reload."
                );
                self.texture_cache.remove(idx);
                self.loader.request_load(
                    self.current_index,
                    self.generation,
                    self.image_files[self.current_index].clone(),
                    self.settings.raw_high_quality,
                );
            }

            self.loader.flush_tile_queue();
            ctx.request_repaint();
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
        }
    }

    pub(crate) fn handle_image_load_result(
        &mut self,
        load_result: LoadResult,
        ctx: &egui::Context,
    ) {
        let idx = load_result.index;
        match load_result.result.as_ref() {
            Ok(ImageData::Static(decoded)) => {
                let color_image = ColorImage::from_rgba_unmultiplied(
                    [decoded.width as usize, decoded.height as usize],
                    &decoded.pixels,
                );
                let name = format!("img_{}", idx);
                let handle = ctx.load_texture(name, color_image, TextureOptions::LINEAR);
                if let Some(evicted_idx) = self.texture_cache.insert(
                    idx,
                    handle,
                    decoded.width,
                    decoded.height,
                    false,
                    self.current_index,
                    self.image_files.len(),
                ) {
                    self.animation_cache.remove(&evicted_idx);
                }
                if idx == self.current_index {
                    self.current_image_res = Some((decoded.width, decoded.height));
                    if self
                        .animation
                        .as_ref()
                        .is_some_and(|a| a.image_index == idx)
                    {
                        self.animation = None;
                    }
                }
            }
            Ok(ImageData::Tiled(source)) => {
                // Upload preview into texture_cache so it persists across navigations.
                // Without this, flipping away and back would re-trigger a 300ms+ load.
                if let Some(preview) = load_result.preview.as_ref() {
                    // Update texture cache if it's empty OR if it currently holds a low-res preview.
                    // This ensures we can upgrade an EXIF thumbnail to an HQ preview while protecting full static images.
                    if !self.texture_cache.contains(idx)
                        || self.texture_cache.is_preview_placeholder(idx)
                    {
                        let color_image = ColorImage::from_rgba_unmultiplied(
                            [preview.width as usize, preview.height as usize],
                            &preview.pixels,
                        );
                        let name = format!("img_preview_{}", idx);
                        let handle = ctx.load_texture(name, color_image, TextureOptions::LINEAR);
                        if let Some(evicted_idx) = self.texture_cache.insert(
                            idx,
                            handle,
                            source.width(),
                            source.height(),
                            true,
                            self.current_index,
                            self.image_files.len(),
                        ) {
                            self.animation_cache.remove(&evicted_idx);
                        }
                    }
                }

                if idx == self.current_index {
                    self.current_image_res = Some((source.width(), source.height()));
                    crate::tile_cache::set_tile_size_for_image(source.width(), source.height());
                    let mut tm =
                        TileManager::with_source(idx, load_result.generation, Arc::clone(&source));

                    // Prefer existing cached texture (might be HQ) over the initial low-res preview
                    if let Some(cached_handle) = self.texture_cache.get(idx).cloned() {
                        tm.preview_texture = Some(cached_handle);
                    } else if let Some(preview) = load_result.preview.as_ref() {
                        self.setup_tile_manager(ctx, idx, &mut tm, preview.clone());
                    }

                    self.tile_manager = Some(tm);
                    self.animation = None;
                    if let Some(res) = self.current_image_res {
                        self.log_large_image(idx, res.0, res.1);
                    } else {
                        log::warn!(
                            "[UI] Attempted to log large image resolution, but res was None for index {}",
                            idx
                        );
                    }

                    // Trigger refinement ONLY for the actively-viewed image.
                    // Prefetched images stay at preview quality until navigated to.
                    source.request_refinement(idx, self.generation);
                } else {
                    // Preloading: create the TileManager and store it in prefetched_tiles
                    // so that when the user switches to this image, the source (and its
                    // background refined RAW data) is immediately available!
                    let mut tm =
                        TileManager::with_source(idx, load_result.generation, Arc::clone(source));

                    // Prefer existing cached texture (might be HQ) over the initial low-res preview
                    if let Some(cached_handle) = self.texture_cache.get(idx).cloned() {
                        tm.preview_texture = Some(cached_handle);
                    } else if let Some(preview) = load_result.preview.as_ref() {
                        self.setup_tile_manager(ctx, idx, &mut tm, preview.clone());
                    }
                    self.prefetched_tiles.insert(idx, tm);
                }
            }
            Ok(ImageData::Animated(frames)) => {
                // Upload first frame immediately
                if let Some(first) = frames.first() {
                    let color_image = ColorImage::from_rgba_unmultiplied(
                        [first.width as usize, first.height as usize],
                        &first.pixels,
                    );
                    let name = format!("img_{}", idx);
                    let handle = ctx.load_texture(name, color_image, TextureOptions::LINEAR);
                    if let Some(evicted_idx) = self.texture_cache.insert(
                        idx,
                        handle,
                        first.width,
                        first.height,
                        false,
                        self.current_index,
                        self.image_files.len(),
                    ) {
                        self.animation_cache.remove(&evicted_idx);
                    }
                    if idx == self.current_index {
                        self.current_image_res = Some((first.width, first.height));
                    }
                }

                // Defer remaining
                let cur = self.current_index;
                let n = self.image_files.len();
                let is_in_range = if n > 0 {
                    idx == cur
                        || idx == (cur + 1) % n
                        || (cur > 0 && idx == cur - 1)
                        || (cur == 0 && idx == n - 1)
                } else {
                    false
                };

                if is_in_range {
                    self.pending_anim_frames = Some(PendingAnimUpload {
                        image_index: idx,
                        frames: frames.clone(),
                        textures: Vec::new(),
                        delays: Vec::new(),
                        next_frame: 0,
                    });
                    ctx.request_repaint();
                }
            }
            Err(e) => {
                let path_str = self.image_files[idx].display().to_string();
                log::error!("Failed to load image at index {} ({}): {e}", idx, path_str);
                if idx == self.current_index {
                    self.error_message = Some(
                        t!("status.load_failed", path = path_str, err = e.to_string()).to_string(),
                    );
                }
            }
        }
    }

    pub(crate) fn handle_tile_load_result(
        &mut self,
        tile_result: TileResult,
        _ctx: &egui::Context,
    ) {
        let coord = TileCoord {
            col: tile_result.col,
            row: tile_result.row,
        };

        // Pixels are already in PIXEL_CACHE (inserted by the worker thread).
        // We only need to mark as no longer pending and trigger repaint for GPU upload.
        if let Some(ref mut tm) = self.tile_manager {
            if tm.image_index == tile_result.index {
                tm.pending_tiles.remove(&coord);
                // Trigger repaint so the next frame uploads this to GPU immediately
                _ctx.request_repaint();
            }
        }
    }

    pub(crate) fn handle_preview_update(&mut self, update: PreviewResult, ctx: &egui::Context) {
        // Apply HQ preview if it matches the currently displayed tile manager.
        // Also check prefetched tiles and update the texture cache for future navigations.
        match update.result {
            Ok(preview) => {
                // 1. Update current TileManager
                if let Some(ref mut tm) = self.tile_manager {
                    if tm.image_index == update.index && update.generation == tm.generation {
                        log::info!(
                            "[App] HQ preview applied for current index {} ({}x{})",
                            update.index,
                            preview.width,
                            preview.height
                        );
                        tm.set_preview(preview.clone(), ctx);
                        ctx.request_repaint();
                    }
                }

                // 2. Update prefetched TileManagers
                if let Some(tm) = self.prefetched_tiles.get_mut(&update.index) {
                    log::info!(
                        "[App] HQ preview applied for prefetched index {} ({}x{})",
                        update.index,
                        preview.width,
                        preview.height
                    );
                    tm.set_preview(preview.clone(), ctx);
                }

                // 3. Update global texture cache (so instant-flips also get HQ texture).
                // Only update if it's empty or currently holds a preview (don't downgrade full static images).
                if !self.texture_cache.contains(update.index)
                    || self.texture_cache.is_preview_placeholder(update.index)
                {
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
                        &preview.pixels,
                    );
                    let handle = ctx.load_texture(name, color_image, egui::TextureOptions::LINEAR);
                    self.texture_cache.insert(
                        update.index,
                        handle,
                        orig_w,
                        orig_h,
                        true, // is_tiled
                        self.current_index,
                        self.image_files.len(),
                    );
                }
            }
            Err(e) => {
                log::error!("Preview update failed for index {}: {}", update.index, e);
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
            &preview.pixels,
        );
        let preview_handle = ctx.load_texture(
            format!("preview_{}", idx),
            preview_img,
            egui::TextureOptions::LINEAR,
        );
        tm.preview_texture = Some(preview_handle);
    }
}
