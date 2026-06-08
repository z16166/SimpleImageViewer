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

use super::*;

impl ImageViewerApp {
    pub(crate) fn open_directory_dialog(&mut self, frame: &eframe::Frame) {
        let mut dialog = crate::app::rfd_parent::file_dialog_for_main_window(frame);
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
        self.prev_transition_rect = None;
        self.transition_start = None;
        self.tile_manager = None;
        self.prefetched_tiles.clear();
        crate::tile_cache::PIXEL_CACHE.lock().clear();
        self.current_image_res = None;
        self.raw_osd_by_index.clear();
        self.current_osd_file_name.clear();
        self.osd.set_supplemental_lines(None, None);
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
        scanner::scan_directory(
            dir,
            self.settings.recursive,
            self.settings.skip_raw_if_jpeg_exists,
            tx,
            cancel,
        );
    }

    /// Refresh the image file list for the current directory (bound to F5).
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
        self.prev_transition_rect = None;
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
        scanner::scan_directory(
            dir,
            self.settings.recursive,
            self.settings.skip_raw_if_jpeg_exists,
            tx,
            cancel,
        );
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
                            // For refresh scans, initial_image is kept None; for startup scans,
                            // resolve_initial_position() delays clearing initial_image to None until
                            // scanning is complete so that it survives the final sorted Done pass.
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

                                self.refresh_current_osd_file_name();

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
            if !self.scanning {
                self.initial_image = None;
            }
        } else if self.settings.resume_last_image {
            if let Some(last_path) = &self.settings.last_viewed_image {
                if let Some(pos) = self.find_index_for_path(last_path) {
                    self.current_index = pos;
                }
            }
        }
    }
}
