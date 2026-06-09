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
                        self.set_current_index(original_idx);
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
}
