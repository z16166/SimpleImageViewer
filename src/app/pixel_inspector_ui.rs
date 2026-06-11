// Simple Image Viewer - A high-performance, cross-platform image viewer
// Copyright (C) 2024-2026 Simple Image Viewer Contributors
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
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
            (cache.last_screen_pos - pointer_pos).length_sq() < 0.01
        } else {
            false
        };

        let display_text = if cache_hit {
            self.pixel_hover_cache
                .as_ref()
                .unwrap()
                .display_text
                .clone()
        } else {
            let pixel_val = crate::pixel_inspector::sample_hover_pixel(
                source,
                self.current_index,
                img_x,
                img_y,
            );

            let rgba_str = match pixel_val {
                Some([r, g, b, a]) => format!("{:02X}{:02X}{:02X}{:02X}", r, g, b, a),
                None => "…".to_string(),
            };

            let text = format!("X: {:<5} Y: {:<5}\nRGBA: {}", img_x, img_y, rgba_str);

            self.pixel_hover_cache = Some(crate::app::types::PixelHoverCache {
                last_screen_pos: pointer_pos,
                display_text: text.clone(),
            });
            text
        };

        let offset = Vec2::new(
            crate::constants::PIXEL_TOOLTIP_OFFSET,
            crate::constants::PIXEL_TOOLTIP_OFFSET,
        );
        let mut tooltip_pos = pointer_pos + offset;

        // Tooltip dimensions
        let tooltip_w = 120.0;
        let tooltip_h = 32.0;

        // Fit within screen bounds safely
        if tooltip_pos.x + tooltip_w > screen_rect.max.x {
            tooltip_pos.x = pointer_pos.x - tooltip_w - offset.x;
        }
        if tooltip_pos.y + tooltip_h > screen_rect.max.y {
            tooltip_pos.y = pointer_pos.y - tooltip_h - offset.y;
        }

        let painter = ui.painter();
        let rect = Rect::from_min_size(tooltip_pos, Vec2::new(tooltip_w, tooltip_h));

        painter.rect(
            rect,
            egui::CornerRadius::same(3),
            Color32::from_black_alpha(180),
            egui::Stroke::new(1.0_f32, Color32::from_white_alpha(40)),
            egui::StrokeKind::Outside,
        );

        painter.text(
            rect.min + Vec2::new(6.0, 4.0),
            Align2::LEFT_TOP,
            &display_text,
            FontId::monospace(10.0),
            Color32::WHITE,
        );
    }

    pub(crate) fn draw_pixel_inspector_canvas_feedback(
        &self,
        ui: &mut egui::Ui,
        _screen_rect: Rect,
        display_rect: Rect,
    ) {
        if !self.settings.show_pixel_inspector {
            return;
        }

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

    pub(crate) fn handle_pixel_region_click(
        &mut self,
        pos: egui::Pos2,
        _screen_rect: Rect,
        display_rect: Rect,
    ) {
        if !self.settings.show_pixel_inspector {
            return;
        }

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
            let x_min = x0.min(img_x);
            let x_max = x0.max(img_x);
            let y_min = y0.min(img_y);
            let y_max = y0.max(img_y);

            // Clamp maximum dimensions to 128x128 for UI responsiveness and stability
            let x_max = x_max.min(x_min + 127);
            let y_max = y_max.min(y_min + 127);

            let rect = crate::app::types::PixelRegionRect {
                x0: x_min,
                y0: y_min,
                x1: x_max + 1,
                y1: y_max + 1,
            };

            self.pixel_region_selection = Some(rect);
            self.pixel_region_first_point = None;

            self.open_pixel_region_dialog(rect);
        } else {
            // First click
            self.pixel_region_first_point = Some((img_x, img_y));
        }
    }

    pub(crate) fn open_pixel_region_dialog(&mut self, rect: crate::app::types::PixelRegionRect) {
        let (tx, rx) = crossbeam_channel::unbounded();
        let source = self.pixel_data_source.clone();

        if let Some(src) = source {
            std::thread::spawn(move || {
                let pixels = crate::pixel_inspector::extract_region(
                    &src, rect.x0, rect.y0, rect.x1, rect.y1,
                );
                let _ = tx.send(pixels);
            });
        }

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
        if !self.settings.show_pixel_inspector {
            return false;
        }

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
