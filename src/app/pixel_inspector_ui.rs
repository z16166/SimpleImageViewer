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

//

use crate::app::ImageViewerApp;
use eframe::egui::{self, Align2, Color32, FontId, Rect, Vec2};

impl ImageViewerApp {
    pub(crate) fn draw_pixel_hover_tooltip(
        &mut self,
        ui: &mut egui::Ui,
        screen_rect: Rect,
        display_rect: Rect,
    ) {
        if !self.settings.show_pixel_inspector {
            return;
        }

        let Some(source) = &self.pixel_data_source else {
            return;
        };

        let Some(pointer_pos) = ui.input(|i| i.pointer.hover_pos()) else {
            return;
        };

        if !display_rect.contains(pointer_pos) {
            return;
        }

        let res = match self.current_image_res {
            Some(r) => r,
            None => return,
        };

        // Screen coordinate to image coordinate conversion
        let Some((img_x, img_y)) = crate::pixel_inspector::screen_to_image_coord(
            pointer_pos,
            display_rect,
            self.current_rotation,
            res.0,
            res.1,
        ) else {
            return;
        };

        // Monotonic optimization: avoid formatting text if the pointer hasn't moved
        let cache_hit = if let Some(cache) = &self.pixel_hover_cache {
            (cache.last_screen_pos - pointer_pos).length_sq()
                < crate::constants::PIXEL_POINTER_STATIONARY_THRESHOLD_SQ
                && (cache.zoom_factor - self.zoom_factor).abs() <= f32::EPSILON
                && cache.rotation_steps == self.current_rotation
                && cache.current_index == self.current_index
                && cache.pan_offset == self.pan_offset
        } else {
            false
        };

        let local_text;
        let display_text = if cache_hit {
            &self.pixel_hover_cache.as_ref().unwrap().display_text
        } else {
            let pixel_val = crate::pixel_inspector::sample_hover_pixel(
                source,
                self.current_index,
                img_x,
                img_y,
            );

            let rgba_str = match pixel_val {
                Some([r, g, b, a]) => format!("{:02X} {:02X} {:02X} {:02X}", r, g, b, a),
                None => "…".to_string(),
            };

            local_text = format!("X: {:<5} Y: {:<5}\nRGBA: {}", img_x, img_y, rgba_str);

            self.pixel_hover_cache = Some(crate::pixel_inspector::PixelHoverCache {
                last_screen_pos: pointer_pos,
                zoom_factor: self.zoom_factor,
                rotation_steps: self.current_rotation,
                current_index: self.current_index,
                pan_offset: self.pan_offset,
                display_text: local_text.clone(),
            });
            &local_text
        };

        let offset = Vec2::new(
            crate::constants::PIXEL_TOOLTIP_OFFSET,
            crate::constants::PIXEL_TOOLTIP_OFFSET,
        );
        let mut tooltip_pos = pointer_pos + offset;

        // Tooltip dimensions scaled proportionally with settings.font_size
        let font_size_ratio = self.settings.font_size / 16.0;
        let tooltip_w = crate::constants::PIXEL_TOOLTIP_WIDTH * font_size_ratio;
        let tooltip_h = crate::constants::PIXEL_TOOLTIP_HEIGHT * font_size_ratio;
        let tooltip_font_size = 10.0 * font_size_ratio;

        // Fit within screen bounds safely
        if tooltip_pos.x + tooltip_w > screen_rect.max.x {
            tooltip_pos.x = pointer_pos.x - tooltip_w - offset.x;
        }
        if tooltip_pos.y + tooltip_h > screen_rect.max.y {
            tooltip_pos.y = pointer_pos.y - tooltip_h - offset.y;
        }

        // Clamp to screen edges
        tooltip_pos.x = tooltip_pos
            .x
            .clamp(screen_rect.min.x, screen_rect.max.x - tooltip_w);
        tooltip_pos.y = tooltip_pos
            .y
            .clamp(screen_rect.min.y, screen_rect.max.y - tooltip_h);

        let painter = ui.painter();
        let rect = Rect::from_min_size(tooltip_pos, Vec2::new(tooltip_w, tooltip_h));

        let bg_color = Color32::from_rgba_unmultiplied(
            self.cached_palette.panel_bg.r(),
            self.cached_palette.panel_bg.g(),
            self.cached_palette.panel_bg.b(),
            240,
        );
        let text_color = self.cached_palette.text_normal;
        let border_color = self.cached_palette.widget_border;

        painter.rect(
            rect,
            egui::CornerRadius::same(3),
            bg_color,
            egui::Stroke::new(1.0_f32, border_color),
            egui::StrokeKind::Outside,
        );

        painter.text(
            rect.min
                + Vec2::new(
                    crate::constants::PIXEL_TOOLTIP_PADDING_X,
                    crate::constants::PIXEL_TOOLTIP_PADDING_Y,
                ),
            Align2::LEFT_TOP,
            display_text,
            FontId::monospace(tooltip_font_size),
            text_color,
        );
    }

    pub(crate) fn draw_pixel_inspector_canvas_feedback(
        &self,
        ui: &mut egui::Ui,
        display_rect: Rect,
    ) {
        let res = match self.current_image_res {
            Some(r) => r,
            None => return,
        };

        let painter = ui.painter();

        // Case 1: First point selected, drawing preview rectangle to current pointer
        if self.active_modal.is_none() {
            if let Some((x0, y0)) = self.pixel_region_first_point {
                let p0 = crate::pixel_inspector::image_to_screen_coord(
                    (x0, y0),
                    display_rect,
                    self.current_rotation,
                    res.0,
                    res.1,
                );

                // Draw first point marker
                painter.circle_stroke(p0, 4.0_f32, egui::Stroke::new(1.5_f32, Color32::WHITE));
                painter.circle_stroke(p0, 2.0_f32, egui::Stroke::new(1.0_f32, Color32::BLACK));

                // Draw preview rectangle to hover position
                if let Some(pointer_pos) = ui.input(|i| i.pointer.hover_pos()) {
                    if display_rect.contains(pointer_pos) {
                        let rect = Rect::from_two_pos(p0, pointer_pos);

                        // Contrast strokes
                        painter.rect_stroke(
                            rect,
                            egui::CornerRadius::ZERO,
                            egui::Stroke::new(1.5_f32, Color32::WHITE),
                            egui::StrokeKind::Outside,
                        );
                        painter.rect_stroke(
                            rect.expand(1.0_f32),
                            egui::CornerRadius::ZERO,
                            egui::Stroke::new(1.0_f32, Color32::BLACK),
                            egui::StrokeKind::Outside,
                        );
                    }
                }
            }
        }

        // Case 2: Modal is open, draw the finalized selection rect
        if let Some(crate::ui::dialogs::modal_state::ActiveModal::PixelRegion(ref state)) =
            self.active_modal
        {
            let p0 = crate::pixel_inspector::image_to_screen_coord(
                (state.x0, state.y0),
                display_rect,
                self.current_rotation,
                res.0,
                res.1,
            );
            // x1, y1 are exclusive, so the last pixel is (x1-1, y1-1)
            let last_x = state.x1.saturating_sub(1);
            let last_y = state.y1.saturating_sub(1);
            let p1 = crate::pixel_inspector::image_to_screen_coord(
                (last_x, last_y),
                display_rect,
                self.current_rotation,
                res.0,
                res.1,
            );

            let rect = Rect::from_two_pos(p0, p1);

            // Contrast strokes
            painter.rect_stroke(
                rect,
                egui::CornerRadius::ZERO,
                egui::Stroke::new(1.5_f32, Color32::WHITE),
                egui::StrokeKind::Outside,
            );
            painter.rect_stroke(
                rect.expand(1.0_f32),
                egui::CornerRadius::ZERO,
                egui::Stroke::new(1.0_f32, Color32::BLACK),
                egui::StrokeKind::Outside,
            );
        }
    }

    pub(crate) fn handle_pixel_region_click(&mut self, pos: egui::Pos2, display_rect: Rect) {
        let res = match self.current_image_res {
            Some(r) => r,
            None => return,
        };

        let Some((img_x, img_y)) = crate::pixel_inspector::screen_to_image_coord(
            pos,
            display_rect,
            self.current_rotation,
            res.0,
            res.1,
        ) else {
            return;
        };

        if let Some((x0, y0)) = self.pixel_region_first_point {
            // Second click: finalize selection
            match crate::pixel_inspector::validate_pixel_region(
                x0,
                y0,
                img_x,
                img_y,
                crate::constants::PIXEL_REGION_MAX_DIM,
            ) {
                Err(crate::pixel_inspector::PixelRegionValidationError::Empty) => {
                    self.pixel_region_first_point = None;
                    self.active_modal =
                        Some(crate::ui::dialogs::modal_state::ActiveModal::Confirm(
                            crate::ui::dialogs::confirm::State::info(
                                rust_i18n::t!("pixel_inspector.warning"),
                                rust_i18n::t!("pixel_inspector.region_empty"),
                            ),
                        ));
                }
                Err(crate::pixel_inspector::PixelRegionValidationError::TooLarge { w, h }) => {
                    self.pixel_region_first_point = None;
                    self.active_modal =
                        Some(crate::ui::dialogs::modal_state::ActiveModal::Confirm(
                            crate::ui::dialogs::confirm::State::info(
                                rust_i18n::t!("pixel_inspector.warning"),
                                rust_i18n::t!(
                                    "pixel_inspector.region_too_large",
                                    w = w.to_string(),
                                    h = h.to_string(),
                                    max = crate::constants::PIXEL_REGION_MAX_DIM.to_string()
                                ),
                            ),
                        ));
                }
                Ok(rect) => {
                    self.pixel_region_first_point = None;
                    self.open_pixel_region_dialog(rect);
                }
            }
        } else {
            // First click
            self.pixel_region_first_point = Some((img_x, img_y));
            if self.pixel_data_source.is_none() {
                self.loader.request_load(
                    self.current_index,
                    self.image_files[self.current_index].clone(),
                    self.settings.raw_high_quality,
                    self.raw_demosaic_mode_for_index(self.current_index),
                );
            }
        }
    }

    pub(crate) fn open_pixel_region_dialog(
        &mut self,
        rect: crate::pixel_inspector::PixelRegionRect,
    ) {
        let source = self.pixel_data_source.clone();

        let Some(src) = source else {
            self.active_modal = Some(crate::ui::dialogs::modal_state::ActiveModal::Confirm(
                crate::ui::dialogs::confirm::State::info(
                    rust_i18n::t!("pixel_inspector.warning"),
                    rust_i18n::t!("pixel_inspector.not_ready"),
                ),
            ));
            return;
        };

        let (tx, rx) = crossbeam_channel::bounded(1);
        std::thread::spawn(move || {
            let pixels =
                crate::pixel_inspector::extract_region(&src, rect.x0, rect.y0, rect.x1, rect.y1);
            let _ = tx.send(pixels);
        });

        let state = crate::ui::dialogs::pixel_region_dialog::State {
            x0: rect.x0,
            y0: rect.y0,
            x1: rect.x1,
            y1: rect.y1,
            pixels: None,
            load_rx: Some(rx),
        };

        self.active_modal = Some(crate::ui::dialogs::modal_state::ActiveModal::PixelRegion(
            state,
        ));
    }

    pub(crate) fn is_pixel_region_selection_active(&self, ctx: &egui::Context) -> bool {
        if self.pixel_region_first_point.is_some() {
            return true;
        }

        ctx.input(|i| {
            if i.pointer.primary_down() {
                let modifiers = i.modifiers;
                let mask = crate::app::input::get_modifiers_mask(modifiers);
                let chord = crate::hotkeys::model::KeyChord {
                    modifiers: mask,
                    key: crate::hotkeys::model::HotkeyLogicalKey::MouseLeft,
                };
                if self.hotkeys_runtime.map.get(&chord).copied()
                    == Some(crate::hotkeys::model::HotkeyActionId::SelectPixelRegion)
                {
                    return true;
                }
            }
            false
        })
    }
}
