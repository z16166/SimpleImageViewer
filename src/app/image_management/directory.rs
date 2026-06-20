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

const SCAN_STALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
const SCAN_RESULT_CHANNEL_BOUND: usize = 64;

impl ImageViewerApp {
    pub(crate) fn open_directory_dialog(&mut self, frame: &eframe::Frame) {
        self.request_folder_picker(
            frame,
            crate::app::folder_picker::FolderPickerPurpose::ImageDirectory,
            self.settings.last_image_dir.clone(),
        );
    }

    pub(crate) fn apply_picked_image_directory(&mut self, dir: PathBuf) {
        if self.settings.show_directory_tree_nav {
            self.initialize_directory_tree_root(dir.clone());
        } else {
            self.settings.browse_mode = crate::settings::BrowseMode::Linear;
            self.settings.tree_nav_root_dir = None;
            self.settings.tree_nav_selected_dir = None;
        }
        self.load_directory(dir);
        self.queue_save();
    }

    pub(crate) fn load_directory(&mut self, dir: PathBuf) {
        #[cfg(feature = "preload-debug")]
        let load_started = std::time::Instant::now();
        // Abandon an in-progress F5 refresh before starting a new directory scan; otherwise
        // `process_scan_results` treats completion as refresh and skips new-directory reset.
        self.finish_refresh_scan_state();
        // Cancel any in-flight scan and drop its receiver before list state is reset, so stale
        // batches cannot be processed after `refresh_scan_in_progress` is cleared above.
        if let Some(cancel) = self.scan_cancel.take() {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.scan_rx = None;

        #[cfg(feature = "preload-debug")]
        let after_cancel_ms = crate::preload_debug::elapsed_ms(load_started);

        if self.settings.browse_mode == crate::settings::BrowseMode::Tree {
            if self.settings.tree_nav_root_dir.is_none() {
                self.settings.tree_nav_root_dir = Some(dir.clone());
                self.settings.last_image_dir = Some(dir.clone());
            }
            self.settings.tree_nav_selected_dir = Some(dir.clone());
        } else {
            self.settings.last_image_dir = Some(dir.clone());
        }
        self.invalidate_random_slideshow_order();
        self.image_files.clear();
        self.file_byte_len_by_index.clear();
        self.file_modified_unix_by_index.clear();
        self.set_current_index(0);
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
        self.set_current_image_resolution(None);
        self.raw_metadata.clear();
        self.current_file_name.clear();
        self.osd.invalidate();
        self.loader.cancel_all();
        self.generation = self.generation.wrapping_add(1);
        self.loader.set_generation(self.generation);
        self.prefetch_prev_generation = None;
        self.pending_preload_after_directory_scan = false;
        self.directory_tree_strip_bootstrap_after_scan = false;
        if self.settings.browse_mode == crate::settings::BrowseMode::Tree {
            self.invalidate_directory_tree_strip_after_image_list_reorder();
        }
        #[cfg(feature = "preload-debug")]
        let after_cleanup_ms = crate::preload_debug::elapsed_ms(load_started);
        self.pan_offset = Vec2::ZERO;
        // Match `navigate_to` / file-open semantics: prior folder's manual zoom and rotation
        // must not carry over (fit scale is multiplied by `zoom_factor`, so a leftover ~7.5×
        // reads as ~232% OSD instead of ~31% on a fresh directory).
        self.set_zoom_factor(1.0);
        self.current_rotation = 0;
        self.error_message = None;
        self.is_font_error = false;
        let dir_name = dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        if crate::scanner::is_non_browsable_system_directory(&dir) {
            self.scanning = false;
            self.status_message = t!("directory_tree.skip_scan", dir = dir_name).to_string();
            return;
        }

        self.scan_generation = self.scan_generation.wrapping_add(1);
        let scan_generation = self.scan_generation;

        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.scan_cancel = Some(Arc::clone(&cancel));

        let (tx, rx) = crossbeam_channel::bounded(SCAN_RESULT_CHANNEL_BOUND);
        self.scan_rx = Some(rx);
        self.scan_results_pending_since = Some(std::time::Instant::now());
        self.scanning = true;
        self.status_message = t!("status.scanning", dir = dir_name).to_string();
        let recursive = self.effective_scan_recursive();
        let paired = self.settings.paired_raw_jpeg_handling;
        #[cfg(feature = "preload-debug")]
        {
            crate::preload_debug!(
                "[PreloadDebug][Scan] load_directory spawn: dir={} recursive={} paired={:?} gen={} cancel_phase_ms={} cleanup_phase_ms={} total_before_spawn_ms={}",
                dir.display(),
                recursive,
                paired,
                scan_generation,
                after_cancel_ms,
                after_cleanup_ms,
                crate::preload_debug::elapsed_ms(load_started)
            );
        }
        scanner::scan_directory(
            dir,
            recursive,
            paired,
            scan_generation,
            tx,
            cancel,
            self.root_redraw_wake_handle(),
        );
        // Scan thread may finish before this returns; drain without blocking (checklist #3).
        self.process_scan_results();
        self.wake_root_for_logic();
    }

    /// Refresh the image file list for the current directory (bound to F5).
    pub(crate) fn start_refresh_file_list(&mut self) {
        // Guard: ignore if a directory scan or a previous refresh is already running.
        if self.scanning || self.refresh_scan_in_progress {
            log::debug!("[RefreshFileList] Ignored: scan already in progress");
            return;
        }
        let Some(dir) = self.current_browse_directory() else {
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

        // HDR caches: unified cleanup keeps side maps (e.g. pending key index) in sync.
        let to_remove_hdr: std::collections::HashSet<usize> = self
            .hdr_image_cache
            .keys()
            .chain(self.hdr_tiled_source_cache.keys())
            .chain(self.hdr_tiled_preview_cache.keys())
            .copied()
            .filter(|&idx| idx != keep)
            .collect();
        for idx in to_remove_hdr {
            self.remove_hdr_image_resources(idx);
        }

        self.hdr_sdr_fallback_indices.retain(|&idx| idx == keep);
        self.hdr_placeholder_fallback_indices
            .retain(|&idx| idx == keep);
        self.hdr_raw_gpu_demosaic_pending_indices
            .retain(|&idx| idx == keep);
        self.gpu_demosaic_failed_indices.retain(|&idx| idx == keep);
        self.hdr_raw_gpu_demosaic_pending_key_index
            .retain(|_, idx| *idx == keep);
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

        self.pending_anim_frames.retain(|&idx, _| idx == keep);

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
        self.file_modified_unix_by_index.clear();
        self.set_current_index(0);
        self.error_message = None;
        self.is_font_error = false;
        self.invalidate_random_slideshow_order();

        let dir_name = dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        // Cancel any previous (already-running) scan.
        if let Some(cancel) = self.scan_cancel.take() {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.scan_rx = None;
        self.scan_generation = self.scan_generation.wrapping_add(1);
        let scan_generation = self.scan_generation;
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.scan_cancel = Some(Arc::clone(&cancel));
        let (tx, rx) = crossbeam_channel::bounded(SCAN_RESULT_CHANNEL_BOUND);
        self.scan_rx = Some(rx);
        self.scan_results_pending_since = Some(std::time::Instant::now());
        self.scanning = true;
        self.status_message = t!("status.scanning", dir = dir_name).to_string();
        scanner::scan_directory(
            dir,
            self.effective_scan_recursive(),
            self.settings.paired_raw_jpeg_handling,
            scan_generation,
            tx,
            cancel,
            self.root_redraw_wake_handle(),
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
        if self.scanning {
            if let Some(since) = self.scan_results_pending_since {
                if since.elapsed() > SCAN_STALL_TIMEOUT {
                    log::warn!(
                        "[Scan] timed out after {}s (gen={}); cancelling",
                        SCAN_STALL_TIMEOUT.as_secs(),
                        self.scan_generation
                    );
                    if let Some(cancel) = self.scan_cancel.take() {
                        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                    self.scan_rx = None;
                    self.scanning = false;
                    self.scan_results_pending_since = None;
                    self.status_message = t!("directory_tree.scan_timeout").to_string();
                    if self.refresh_scan_in_progress {
                        self.finish_refresh_scan_state();
                    }
                }
            }
        }

        if self.scanning && self.scan_rx.is_none() {
            log::error!(
                "[Scan] scanning=true but scan_rx is None; clearing stuck scan state (gen={})",
                self.scan_generation
            );
            self.scanning = false;
            self.scan_results_pending_since = None;
            return;
        }

        if self.scan_rx.is_none() {
            return;
        }

        #[cfg(feature = "preload-debug")]
        let drain_started = std::time::Instant::now();

        let active_generation = self.scan_generation;
        let mut done = false;
        let mut first_batch_current_load_pending = false;
        #[cfg(feature = "preload-debug")]
        let mut batch_count = 0usize;

        // Drain all available messages this frame (non-blocking). Keep `scan_rx` in place
        // until Done/disconnect so we never lose the receiver while `scanning` is true.
        loop {
            let Some(rx) = self.scan_rx.as_ref() else {
                break;
            };
            let msg = match rx.try_recv() {
                Ok(msg) => msg,
                Err(crossbeam_channel::TryRecvError::Empty) => break,
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    done = true;
                    self.scanning = false;
                    self.scan_results_pending_since = None;
                    if self.image_files.is_empty() {
                        self.status_message = t!("status.not_found").to_string();
                    }
                    if self.refresh_scan_in_progress {
                        self.refresh_anchor_path = None;
                        log::warn!("[RefreshFileList] Scan thread disconnected; refresh aborted");
                        self.finish_refresh_scan_state();
                    }
                    break;
                }
            };

            #[cfg(feature = "preload-debug")]
            if let Some(since) = self.scan_results_pending_since.take() {
                crate::preload_debug!(
                    "[PreloadDebug][Scan] results drained after spawn wait_ms={}",
                    crate::preload_debug::elapsed_ms(since)
                );
            }

            match msg {
                ScanMessage::Batch { generation, files } if generation == active_generation => {
                    #[cfg(feature = "preload-debug")]
                    {
                        batch_count += 1;
                        crate::preload_debug!(
                            "[PreloadDebug][Scan] process batch #{batch_count}: added={} total={} drain_ms={}",
                            files.len(),
                            self.image_files.len() + files.len(),
                            crate::preload_debug::elapsed_ms(drain_started)
                        );
                    }
                    let is_first_batch = self.image_files.is_empty();
                    for (path, len, modified_unix) in files {
                        self.image_files.push(path);
                        self.file_byte_len_by_index.push(len);
                        self.file_modified_unix_by_index.push(modified_unix);
                    }

                    let count = self.image_files.len();
                    self.status_message =
                        t!("status.scanning_found", count = count.to_string()).to_string();

                    if is_first_batch && count > 0 {
                        if !self.refresh_scan_in_progress {
                            self.resolve_initial_position();
                            self.maybe_prefetch_startup_raw_open();
                        }
                        if !self.images_ever_loaded {
                            self.show_settings = false;
                        }
                        self.images_ever_loaded = true;
                        first_batch_current_load_pending = true;
                    }
                }
                ScanMessage::Batch { generation, .. } => {
                    log::debug!(
                        "[Scan] ignoring stale batch: message_gen={generation} active_gen={active_generation}"
                    );
                }
                ScanMessage::Done {
                    generation,
                    sorted_files,
                } if generation == active_generation => {
                    self.scan_results_pending_since = None;
                    #[cfg(feature = "preload-debug")]
                    let done_started = std::time::Instant::now();
                    done = true;
                    self.scanning = false;

                    if self.image_files.is_empty() && sorted_files.is_empty() {
                        self.status_message = t!("status.not_found").to_string();
                        self.finish_refresh_scan_state();
                    } else {
                        let old_files =
                            if self.settings.browse_mode == crate::settings::BrowseMode::Tree {
                                self.image_files.clone()
                            } else {
                                Vec::new()
                            };
                        if !sorted_files.is_empty() {
                            let mut paths = Vec::with_capacity(sorted_files.len());
                            let mut sizes = Vec::with_capacity(sorted_files.len());
                            let mut modified = Vec::with_capacity(sorted_files.len());
                            for (path, len, mtime) in sorted_files {
                                paths.push(path);
                                sizes.push(len);
                                modified.push(mtime);
                            }
                            self.image_files = paths;
                            self.file_byte_len_by_index = sizes;
                            self.file_modified_unix_by_index = modified;
                        } else {
                            debug_assert_eq!(
                                self.image_files.len(),
                                self.file_byte_len_by_index.len()
                            );
                            debug_assert_eq!(
                                self.image_files.len(),
                                self.file_modified_unix_by_index.len()
                            );
                            let mut combined: Vec<(PathBuf, u64, Option<i64>)> =
                                std::mem::take(&mut self.image_files)
                                    .into_iter()
                                    .zip(std::mem::take(&mut self.file_byte_len_by_index))
                                    .zip(std::mem::take(&mut self.file_modified_unix_by_index))
                                    .map(|((path, len), modified)| (path, len, modified))
                                    .collect();
                            combined.sort_by(|a, b| a.0.cmp(&b.0));
                            let mut paths = Vec::with_capacity(combined.len());
                            let mut sizes = Vec::with_capacity(combined.len());
                            let mut modified = Vec::with_capacity(combined.len());
                            for (path, len, mtime) in combined {
                                paths.push(path);
                                sizes.push(len);
                                modified.push(mtime);
                            }
                            self.image_files = paths;
                            self.file_byte_len_by_index = sizes;
                            self.file_modified_unix_by_index = modified;
                        }
                        #[cfg(feature = "preload-debug")]
                        crate::preload_debug!(
                            "[PreloadDebug][Scan] process done sort_ms={} files={}",
                            crate::preload_debug::elapsed_ms(done_started),
                            self.image_files.len()
                        );

                        if self.settings.browse_mode == crate::settings::BrowseMode::Tree {
                            let new_files = self.image_files.clone();
                            self.reorder_directory_tree_strip_after_image_list_change(
                                &old_files, &new_files,
                            );
                        }

                        if self.refresh_scan_in_progress {
                            if let Some(anchor) = self.refresh_anchor_path.take() {
                                if let Some(new_idx) = self.find_index_for_path(&anchor) {
                                    self.relocate_index_keyed_cache(0, new_idx);
                                    self.clear_index_keyed_state_after_list_reorder_except_index(
                                        new_idx,
                                    );
                                    self.invalidate_random_slideshow_order();
                                    self.set_current_index(new_idx);
                                } else {
                                    self.clear_index_keyed_state_after_list_reorder();
                                    self.invalidate_random_slideshow_order();
                                    self.set_current_index(0);

                                    let fallback_path = self.image_files[0].clone();
                                    self.loader.request_load(
                                        0,
                                        self.generation,
                                        fallback_path,
                                        self.settings.raw_high_quality,
                                        self.raw_demosaic_mode_for_index(0),
                                    );
                                }
                            } else {
                                self.clear_index_keyed_state_after_list_reorder();
                                self.invalidate_random_slideshow_order();
                                self.resolve_initial_position();
                            }
                        } else {
                            #[cfg(feature = "preload-debug")]
                            let before_clear_ms = crate::preload_debug::elapsed_ms(done_started);
                            self.clear_index_keyed_state_after_list_reorder();
                            #[cfg(feature = "preload-debug")]
                            crate::preload_debug!(
                                "[PreloadDebug][Scan] process done clear_index_keyed_state_ms={}",
                                crate::preload_debug::elapsed_ms(done_started) - before_clear_ms
                            );
                            self.invalidate_random_slideshow_order();

                            self.set_zoom_factor(1.0);
                            self.pan_offset = Vec2::ZERO;
                            self.current_rotation = 0;

                            self.resolve_initial_position();
                        }

                        self.refresh_current_file_name();

                        let count = self.image_files.len();
                        self.status_message =
                            t!("status.found", count = count.to_string()).to_string();
                        if self.defer_main_preload_for_directory_tree_list() {
                            self.pending_preload_after_directory_scan = true;
                            self.directory_tree_strip_bootstrap_after_scan = true;
                        } else {
                            self.schedule_preloads(true);
                        }

                        self.finish_refresh_scan_state();
                    }
                    #[cfg(feature = "preload-debug")]
                    crate::preload_debug!(
                        "[PreloadDebug][Scan] process done complete: files={} done_handler_ms={} drain_total_ms={}",
                        self.image_files.len(),
                        crate::preload_debug::elapsed_ms(done_started),
                        crate::preload_debug::elapsed_ms(drain_started)
                    );
                    break;
                }
                ScanMessage::Done { generation, .. } => {
                    log::debug!(
                        "[Scan] ignoring stale done: message_gen={generation} active_gen={active_generation}"
                    );
                }
            }
        }

        if first_batch_current_load_pending && !done {
            if self.defer_main_preload_for_directory_tree_list() {
                self.pending_preload_after_directory_scan = true;
                self.directory_tree_strip_bootstrap_after_scan = true;
            } else {
                self.schedule_current_image_load_if_needed();
            }
        }

        if done {
            self.scan_rx = None;
            self.scan_cancel = None;
        }
    }

    pub(crate) fn find_index_for_path(&self, path: &std::path::Path) -> Option<usize> {
        find_index_for_path_impl(&self.image_files, path)
    }

    pub(crate) fn defer_main_preload_for_directory_tree_list(&self) -> bool {
        self.settings.browse_mode == crate::settings::BrowseMode::Tree
            && self.settings.show_directory_tree_nav
            && self.settings.directory_tree_show_list_previews
            && !self.refresh_scan_in_progress
    }

    /// Resolve the starting image index from initial_image or resume settings.
    pub(crate) fn resolve_initial_position(&mut self) {
        if let Some(ref path) = self.initial_image {
            if let Some(pos) = self.find_index_for_path(path) {
                self.set_current_index(pos);
            }
            if !self.scanning {
                self.initial_image = None;
            }
        } else if self.settings.resume_last_image {
            if let Some(last_path) = &self.settings.last_viewed_image {
                if let Some(pos) = self.find_index_for_path(last_path) {
                    self.set_current_index(pos);
                }
            }
        }
    }
}
