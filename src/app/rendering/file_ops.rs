use crate::app::ImageViewerApp;
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

        let is_tiled = self.tile_manager.is_some();
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

    pub(crate) fn delete_current_image(&mut self, permanent: bool) {
        if self.image_files.is_empty() {
            return;
        }

        let original_index = self.current_index;
        let path_to_delete = self.image_files[original_index].clone();

        // Final sanity check: make sure file still exists
        if !path_to_delete.exists() {
            // Just remove from list if it's already gone
            self.image_files.remove(self.current_index);
        } else {
            // CRITICAL: Drop all resources holding the file BEFORE attempting to delete it.
            // On Windows, WIC's IStream and memmap2 will keep the file locked if we don't drop them.
            self.current_image_res = None;
            self.tile_manager = None;
            self.animation = None;
            self.texture_cache.clear_all();
            self.clear_hdr_image_state();
            self.animation_cache.clear();
            self.prev_texture = None;

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
        }

        // Deletion shifts indices, so every index-keyed cache must be rebuilt.
        self.texture_cache.clear_all();
        self.clear_hdr_image_state();
        self.animation_cache.clear();
        self.prefetched_tiles.clear();

        if self.image_files.is_empty() {
            self.current_index = 0;
            self.status_message = t!("status.no_images_left").to_string();
            self.current_image_res = None;
            self.animation = None;
            self.current_hdr_image = None;
            self.prev_texture = None;
            self.transition_start = None;
            // Close any open EXIF/XMP modal since the image is gone
            self.active_modal = None;
        } else {
            // Adjust current_index if we were at the last element
            if self.current_index >= self.image_files.len() {
                self.current_index = self.image_files.len() - 1;
            }

            // Reset state for new image
            self.animation = None;
            self.prev_texture = None;
            self.transition_start = None;
            self.current_rotation = 0;
            self.zoom_factor = 1.0;
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

        // Force HUD update
        self.osd.invalidate();
    }
}
