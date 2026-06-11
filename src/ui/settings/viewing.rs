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

use super::slideshow;
use crate::app::{ImageViewerApp, ScaleMode};
use crate::ui::utils::{settings_card, themed_labeled_toggle};
use eframe::egui::{self, Vec2};
use rust_i18n::t;

pub(super) fn draw_viewing_tab(
    app: &mut ImageViewerApp,
    ui: &mut egui::Ui,
    fullscreen_changed: &mut bool,
) {
    let palette = app.cached_palette.clone();
    settings_card(ui, &palette, t!("section.display"), |ui| {
        let old_fullscreen = app.settings.fullscreen;
        themed_labeled_toggle(
            ui,
            &mut app.settings.fullscreen,
            t!("label.fullscreen"),
            &palette,
        );
        if old_fullscreen != app.settings.fullscreen {
            *fullscreen_changed = true;
        }

        ui.add_space(6.0);
        // Scale Mode: label left, ComboBox right-aligned (mirrors toggle layout).
        // Z key (ToggleScaleMode action) cycles through variants; the ComboBox reflects
        // the current value automatically each frame — no extra sync needed.
        ui.horizontal(|ui| {
            ui.label(t!("label.scale_mode"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let old_scale = app.settings.scale_mode;
                let selected_text = match app.settings.scale_mode {
                    ScaleMode::FitToWindow => t!("scale.fit").to_string(),
                    ScaleMode::OriginalSize => t!("scale.original").to_string(),
                };
                egui::ComboBox::from_id_salt("scale_mode_combo")
                    .selected_text(selected_text)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut app.settings.scale_mode,
                            ScaleMode::FitToWindow,
                            t!("scale.fit").to_string(),
                        );
                        ui.selectable_value(
                            &mut app.settings.scale_mode,
                            ScaleMode::OriginalSize,
                            t!("scale.original").to_string(),
                        );
                    });
                if old_scale != app.settings.scale_mode {
                    app.zoom_factor = 1.0;
                    app.pan_offset = Vec2::ZERO;
                    app.queue_save();
                }
            });
        });
        ui.add_space(6.0);
        if themed_labeled_toggle(
            ui,
            &mut app.settings.show_pixel_inspector,
            t!("label.show_pixel_inspector"),
            &palette,
        )
        .changed()
        {
            app.queue_save();
            if app.settings.show_pixel_inspector {
                app.refresh_pixel_data_source_for_current_index();
                if app.pixel_data_source.is_none() && !app.image_files.is_empty() {
                    app.loader.request_load(
                        app.current_index,
                        app.generation,
                        app.image_files[app.current_index].clone(),
                        app.settings.raw_high_quality,
                    );
                }
            }
        }

        if themed_labeled_toggle(
            ui,
            &mut app.settings.show_osd,
            t!("label.show_osd"),
            &palette,
        )
        .changed()
        {
            app.queue_save();
        }
        if themed_labeled_toggle(
            ui,
            &mut app.settings.raw_high_quality,
            t!("label.raw_high_quality"),
            &palette,
        )
        .on_hover_text(t!("hint.raw_high_quality"))
        .changed()
        {
            app.reload_current();
            app.queue_save();
        }
    });

    ui.add_space(8.0);
    slideshow::draw_hdr_settings_if_available(app, ui);
}
