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
    pub(crate) fn process_file_op_results(&mut self) {
        while let Ok(res) = self.file_op_rx.try_recv() {
            match res {
                FileOpResult::Delete {
                    path,
                    original_index: original_idx,
                    original_size,
                    result: res,
                } => {
                    if let Err(e) = res {
                        log::error!("Failed to delete {:?}: {}", path, e);
                        self.error_message =
                            Some(t!("status.delete_failed", err = e.to_string()).to_string());

                        // ROLLBACK: Restore the file to the in-memory list if it failed to delete.
                        // We use the original index to maintain order.
                        if original_idx <= self.image_files.len() {
                            self.image_files.insert(original_idx, path.clone());
                            self.file_byte_len_by_index
                                .insert(original_idx, original_size);
                        } else {
                            self.image_files.push(path.clone());
                            self.file_byte_len_by_index.push(original_size);
                        }

                        // Restore viewer state and clear caches to ensure consistency.
                        self.set_current_index(original_idx);

                        // Rebuild caches and clear old transform state to avoid rendering glitches
                        self.texture_cache.clear_all();
                        self.clear_hdr_image_state();
                        self.animation_cache.clear();
                        self.prefetched_tiles.clear();

                        self.animation = None;
                        self.prev_texture = None;
                        self.prev_hdr_image = None;
                        self.prev_transition_rect = None;
                        self.transition_start = None;
                        self.current_rotation = 0;
                        self.set_zoom_factor(1.0);
                        self.pan_offset = egui::Vec2::ZERO;

                        self.generation = self.generation.wrapping_add(1);
                        self.loader.set_generation(self.generation);
                        self.status_message =
                            t!("status.found", count = self.image_files.len().to_string())
                                .to_string();
                        self.images_ever_loaded = true;
                        self.loader.request_load(
                            self.current_index,
                            self.generation,
                            self.image_files[self.current_index].clone(),
                            self.settings.raw_high_quality,
                        );
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
                FileOpResult::CopyTo {
                    src_path,
                    target_dir: dest_dir,
                    result,
                } => match result {
                    Ok(()) => {
                        log::info!("Successfully copied {:?} to {:?}", src_path, dest_dir);
                        self.status_message = t!("status.copy_success").to_string();
                    }
                    Err(err) => {
                        log::error!("Failed to copy {:?}: {:?}", src_path, err);
                        let err_msg = err.localized_message();
                        self.active_modal =
                            Some(crate::ui::dialogs::modal_state::ActiveModal::Confirm(
                                crate::ui::dialogs::confirm::State::info(
                                    t!("file_copy_cut.error_title"),
                                    t!("file_copy_cut.copy_failed_msg", error = err_msg),
                                ),
                            ));
                    }
                },
                FileOpResult::CutTo {
                    src_path,
                    target_dir: dest_dir,
                    original_index: original_idx,
                    original_size,
                    result,
                } => match result {
                    Ok(()) => {
                        log::info!("Successfully cut {:?} to {:?}", src_path, dest_dir);
                        self.status_message = t!("status.cut_success").to_string();
                    }
                    Err(err) => {
                        log::error!("Failed to cut {:?}: {:?}", src_path, err);

                        // ROLLBACK: Restore the file to the in-memory list if it failed to move.
                        if original_idx <= self.image_files.len() {
                            self.image_files.insert(original_idx, src_path.clone());
                            self.file_byte_len_by_index
                                .insert(original_idx, original_size);
                        } else {
                            self.image_files.push(src_path.clone());
                            self.file_byte_len_by_index.push(original_size);
                        }

                        // Restore viewer state and clear caches to ensure consistency.
                        self.set_current_index(original_idx);

                        // Rebuild caches and clear old transform state to avoid rendering glitches
                        self.texture_cache.clear_all();
                        self.clear_hdr_image_state();
                        self.animation_cache.clear();
                        self.prefetched_tiles.clear();

                        self.animation = None;
                        self.prev_texture = None;
                        self.prev_hdr_image = None;
                        self.prev_transition_rect = None;
                        self.transition_start = None;
                        self.current_rotation = 0;
                        self.set_zoom_factor(1.0);
                        self.pan_offset = egui::Vec2::ZERO;

                        self.generation = self.generation.wrapping_add(1);
                        self.loader.set_generation(self.generation);
                        self.images_ever_loaded = true; // Mark as loaded so it renders immediately
                        self.loader.request_load(
                            self.current_index,
                            self.generation,
                            self.image_files[self.current_index].clone(),
                            self.settings.raw_high_quality,
                        );
                        self.schedule_preloads(true);

                        let err_msg = err.localized_message();
                        self.active_modal =
                            Some(crate::ui::dialogs::modal_state::ActiveModal::Confirm(
                                crate::ui::dialogs::confirm::State::info(
                                    t!("file_copy_cut.error_title"),
                                    t!("file_copy_cut.cut_failed_msg", error = err_msg),
                                ),
                            ));
                    }
                },
            }
        }
    }
}
