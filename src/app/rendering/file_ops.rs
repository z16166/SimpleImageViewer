use crate::app::ImageViewerApp;
use crate::ui::dialogs::modal_state::ActiveModal;
use eframe::egui::{self, Vec2};
use rust_i18n::t;

impl ImageViewerApp {
    pub(crate) fn print_image(&mut self, ctx: &egui::Context, mode: crate::print::PrintMode) {
        use crate::print::{PrintJob, PrintMode, spawn_print_job};

        if self.image_files.is_empty() {
            return;
        }
        let path = self.image_files[self.current_index].clone();

        if self.is_printing.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }

        let is_tiled = self.tiled_canvas_matches_current_index();
        let mut crop_rect_pixels = None;
        let mut tile_pixel_buffer = None;
        let mut tile_full_width = 0u32;
        let mut tile_full_height = 0u32;

        if let Some(res) = self.current_image_res {
            let img_size = egui::vec2(res.0 as f32, res.1 as f32);
            let screen_rect = ctx.input(|i| i.content_rect());

            if mode == PrintMode::VisibleArea {
                let display_rect = self.compute_display_rect(img_size, screen_rect);
                let intersect = display_rect.intersect(screen_rect);
                if intersect.is_positive() {
                    let scale = img_size.x / display_rect.width();

                    let dx = (intersect.min.x - display_rect.min.x) * scale;
                    let dy = (intersect.min.y - display_rect.min.y) * scale;
                    let dw = intersect.width() * scale;
                    let dh = intersect.height() * scale;

                    crop_rect_pixels = Some([
                        dx.max(0.0) as u32,
                        dy.max(0.0) as u32,
                        dw.min(img_size.x - dx).max(1.0) as u32,
                        dh.min(img_size.y - dy).max(1.0) as u32,
                    ]);
                } else {
                    crop_rect_pixels = Some([0, 0, 1, 1]);
                }
            }

            // For tiled images: pass the Arc'd pixel buffer (cheap clone)
            // and dimensions. The background thread will do the actual
            // downsampling to avoid blocking the UI.
            if is_tiled {
                let tm = self.tile_manager.as_ref().unwrap();
                tile_pixel_buffer = tm.pixel_buffer_arc();
                tile_full_width = tm.full_width;
                tile_full_height = tm.full_height;
            }
        }

        let job = PrintJob {
            mode,
            original_path: path,
            crop_rect_pixels,
            is_tiled,
            tile_pixel_buffer,
            tile_full_width,
            tile_full_height,
        };

        let (tx, rx) = crossbeam_channel::unbounded();
        self.print_status_rx = Some(rx);
        spawn_print_job(job, self.is_printing.clone(), tx);
    }

    /// Delete the current image, showing a confirmation dialog first when moving a
    /// remote/network file to the Recycle Bin may fail.
    pub(crate) fn request_delete_current_image(&mut self, permanent: bool) {
        if self.image_files.is_empty() {
            return;
        }

        if !permanent {
            let path = self.image_files[self.current_index].clone();
            if crate::path_location::is_remote_path(&path) {
                self.active_modal = Some(ActiveModal::Confirm(
                    crate::ui::dialogs::confirm::State::remote_recycle_delete(
                        t!("delete.remote_title"),
                        t!("delete.remote_msg"),
                    ),
                ));
                return;
            }
        }

        self.delete_current_image(permanent);
    }

    pub(crate) fn delete_current_image(&mut self, permanent: bool) {
        if self.image_files.is_empty() {
            return;
        }

        let original_index = self.current_index;
        let path_to_delete = self.image_files[original_index].clone();
        self.invalidate_random_slideshow_order();

        // Final sanity check: make sure file still exists
        if !path_to_delete.exists() {
            // Just remove from list if it's already gone
            self.image_files.remove(self.current_index);
            if self.current_index < self.file_byte_len_by_index.len() {
                self.file_byte_len_by_index.remove(self.current_index);
            }
        } else {
            // CRITICAL: Drop all resources holding the file BEFORE attempting to delete it.
            // On Windows, WIC's IStream and memmap2 will keep the file locked if we don't drop them.
            self.set_current_image_resolution(None);
            self.tile_manager = None;
            self.animation = None;
            self.texture_cache.clear_all();
            self.clear_hdr_image_state();
            self.animation_cache.clear();
            self.prev_texture = None;
            self.prev_hdr_image = None;
            self.prev_transition_rect = None;

            // Successfully unlinked from UI, now delete in background
            let tx = self.file_op_tx.clone();

            std::thread::spawn(move || {
                // Yield briefly to give the OS a moment to flush handles (especially memory mapped files)
                std::thread::sleep(std::time::Duration::from_millis(20));

                let result = if permanent {
                    std::fs::remove_file(&path_to_delete).map_err(|e| e.to_string())
                } else {
                    trash::delete(&path_to_delete).map_err(|e| e.to_string())
                };

                let _ = tx.send(crate::app::FileOpResult::Delete(
                    path_to_delete,
                    original_index,
                    result,
                ));
            });

            self.image_files.remove(original_index);
            if original_index < self.file_byte_len_by_index.len() {
                self.file_byte_len_by_index.remove(original_index);
            }
        }

        // Deletion shifts indices, so every index-keyed cache must be rebuilt.
        self.texture_cache.clear_all();
        self.clear_hdr_image_state();
        self.animation_cache.clear();
        self.prefetched_tiles.clear();

        if self.image_files.is_empty() {
            self.set_current_index(0);
            self.status_message = t!("status.no_images_left").to_string();
            self.set_current_image_resolution(None);
            self.animation = None;
            self.current_hdr_image = None;
            self.prev_texture = None;
            self.prev_hdr_image = None;
            self.prev_transition_rect = None;
            self.transition_start = None;
            // Close any open EXIF/XMP modal since the image is gone
            self.active_modal = None;
        } else {
            // Adjust current_index if we were at the last element
            if self.current_index >= self.image_files.len() {
                self.set_current_index(self.image_files.len() - 1);
            }

            // Reset state for new image
            self.animation = None;
            self.prev_texture = None;
            self.prev_hdr_image = None;
            self.prev_transition_rect = None;
            self.transition_start = None;
            self.current_rotation = 0;
            self.set_zoom_factor(1.0);
            self.pan_offset = Vec2::ZERO;
            // Close any open EXIF/XMP modal since we've moved to a new image
            if matches!(
                self.active_modal,
                Some(crate::ui::dialogs::modal_state::ActiveModal::Exif(_))
                    | Some(crate::ui::dialogs::modal_state::ActiveModal::Xmp(_))
            ) {
                self.active_modal = None;
            }
            self.error_message = None;
            self.is_font_error = false;

            self.generation = self.generation.wrapping_add(1);
            self.loader.set_generation(self.generation);
            self.loader.request_load(
                self.current_index,
                self.generation,
                self.image_files[self.current_index].clone(),
                self.settings.raw_high_quality,
            );
            self.schedule_preloads(true);
        }

        // Force HUD layout refresh after file list mutation.
        self.invalidate_view_text_layout();
    }

    pub(crate) fn copy_current_image_to(&mut self, target_dir: std::path::PathBuf) {
        if self.image_files.is_empty() {
            return;
        }
        let src_path = self.image_files[self.current_index].clone();
        let tx = self.file_op_tx.clone();

        std::thread::spawn(move || {
            let result = (|| {
                std::fs::create_dir_all(&target_dir)
                    .map_err(|e| crate::app::types::FileOpError::CreateDirFailed(e.to_string()))?;
                let filename = src_path
                    .file_name()
                    .ok_or(crate::app::types::FileOpError::InvalidSource)?;
                let dest_path = target_dir.join(filename);

                if dest_path.exists() {
                    return Err(crate::app::types::FileOpError::TargetFileExists);
                }

                std::fs::copy(&src_path, &dest_path)
                    .map(|_| ())
                    .map_err(|e| crate::app::types::FileOpError::CopyFailed(e.to_string()))
            })();
            let _ = tx.send(crate::app::FileOpResult::CopyTo(
                src_path, target_dir, result,
            ));
        });
    }

    pub(crate) fn cut_current_image_to(&mut self, target_dir: std::path::PathBuf) {
        if self.image_files.is_empty() {
            return;
        }
        let original_index = self.current_index;
        let src_path = self.image_files[original_index].clone();
        self.invalidate_random_slideshow_order();

        // CRITICAL: Drop all resources holding the file before move
        self.set_current_image_resolution(None);
        self.tile_manager = None;
        self.animation = None;
        self.texture_cache.clear_all();
        self.clear_hdr_image_state();
        self.animation_cache.clear();
        self.prev_texture = None;
        self.prev_hdr_image = None;
        self.prev_transition_rect = None;

        let tx = self.file_op_tx.clone();
        std::thread::spawn(move || {
            // Yield briefly to give the OS a moment to flush handles (especially memory mapped files)
            std::thread::sleep(std::time::Duration::from_millis(50));

            let result = (|| {
                std::fs::create_dir_all(&target_dir)
                    .map_err(|e| crate::app::types::FileOpError::CreateDirFailed(e.to_string()))?;
                let filename = src_path
                    .file_name()
                    .ok_or(crate::app::types::FileOpError::InvalidSource)?;
                let dest_path = target_dir.join(filename);

                if dest_path.exists() {
                    return Err(crate::app::types::FileOpError::TargetFileExists);
                }

                // Try std::fs::rename first (efficient within the same drive)
                if std::fs::rename(&src_path, &dest_path).is_err() {
                    // Fall back to copy + delete (for cross-device/cross-drive moves)
                    std::fs::copy(&src_path, &dest_path)
                        .map_err(|e| crate::app::types::FileOpError::MoveFailed(e.to_string()))?;
                    std::fs::remove_file(&src_path).map_err(|e| {
                        crate::app::types::FileOpError::RemoveSourceFailed(e.to_string())
                    })?;
                }
                Ok(())
            })();

            let _ = tx.send(crate::app::FileOpResult::CutTo(
                src_path,
                target_dir,
                original_index,
                result,
            ));
        });

        // Remove from memory list immediately
        self.image_files.remove(original_index);
        if original_index < self.file_byte_len_by_index.len() {
            self.file_byte_len_by_index.remove(original_index);
        }

        // Deletion/cut shifts indices, so every index-keyed cache must be rebuilt.
        self.texture_cache.clear_all();
        self.clear_hdr_image_state();
        self.animation_cache.clear();
        self.prefetched_tiles.clear();

        if self.image_files.is_empty() {
            self.set_current_index(0);
            self.status_message = t!("status.no_images_left").to_string();
            self.set_current_image_resolution(None);
            self.animation = None;
            self.current_hdr_image = None;
            self.prev_texture = None;
            self.prev_hdr_image = None;
            self.prev_transition_rect = None;
            self.transition_start = None;
            self.active_modal = None;
        } else {
            // Adjust current_index if we were at the last element
            if self.current_index >= self.image_files.len() {
                self.set_current_index(self.image_files.len() - 1);
            }

            // Reset state for new image
            self.animation = None;
            self.prev_texture = None;
            self.prev_hdr_image = None;
            self.prev_transition_rect = None;
            self.transition_start = None;
            self.current_rotation = 0;
            self.set_zoom_factor(1.0);
            self.pan_offset = Vec2::ZERO;
            if matches!(
                self.active_modal,
                Some(crate::ui::dialogs::modal_state::ActiveModal::Exif(_))
                    | Some(crate::ui::dialogs::modal_state::ActiveModal::Xmp(_))
            ) {
                self.active_modal = None;
            }
            self.error_message = None;
            self.is_font_error = false;

            self.generation = self.generation.wrapping_add(1);
            self.loader.set_generation(self.generation);
            self.loader.request_load(
                self.current_index,
                self.generation,
                self.image_files[self.current_index].clone(),
                self.settings.raw_high_quality,
            );
            self.schedule_preloads(true);
        }

        self.invalidate_view_text_layout();
    }
}
