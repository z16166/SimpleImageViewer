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

use crate::app::ImageViewerApp;
use crate::settings::PairedRawJpegHandling;
use crate::ui::utils::{path_display_box, settings_card, styled_button, themed_labeled_toggle};
use eframe::egui::{self, RichText};
use rust_i18n::t;

const PAIRED_RAW_JPEG_COMBO_WIDTH: f32 = 180.0;

pub(super) fn draw_library_tab(app: &mut ImageViewerApp, ui: &mut egui::Ui, open_dir: &mut bool) {
    ui.vertical(|ui| {
        draw_library_controls(app, ui, open_dir);
    });
}

fn draw_library_controls(app: &mut ImageViewerApp, ui: &mut egui::Ui, open_dir: &mut bool) {
    let palette = app.cached_palette.clone();
    settings_card(ui, &palette, t!("section.directory"), |ui| {
        let dir_full = app
            .settings
            .last_image_dir
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned());
        let dir_empty = app.settings.last_image_dir.is_none();
        let dir_label = if dir_empty {
            t!("label.no_dir").to_string()
        } else {
            dir_full.clone().unwrap_or_default()
        };
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if styled_button(ui, t!("btn.pick"), &palette).clicked() {
                    *open_dir = true;
                }
                ui.add_space(4.0);
                if styled_button(ui, t!("btn.refresh"), &palette).clicked() {
                    if let Some(dir) = app.settings.last_image_dir.clone() {
                        app.load_directory(dir);
                    }
                }

                let box_w = (ui.available_width() - 16.0).max(20.0);
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    let resp = path_display_box(ui, &dir_label, dir_empty, box_w, &palette);
                    if let Some(full) = &dir_full {
                        resp.on_hover_text(full.as_str());
                    }
                });
            });
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(t!("library.images")).color(palette.text_muted));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(app.image_files.len().to_string());
            });
        });

        let scan_status = if app.scanning {
            app.status_message.clone()
        } else if app.settings.last_image_dir.is_some() {
            t!("library.scan_idle").to_string()
        } else {
            t!("library.scan_no_directory").to_string()
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new(t!("library.scan_status")).color(palette.text_muted));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.horizontal(|ui| {
                    if app.scanning {
                        ui.spinner();
                    }
                    ui.label(scan_status);
                });
            });
        });

        ui.add_space(4.0);
        let old_recursive = app.settings.recursive;
        themed_labeled_toggle(
            ui,
            &mut app.settings.recursive,
            t!("label.recursive_scan"),
            &palette,
        );
        if !old_recursive && app.settings.recursive {
            app.settings.recursive = false;
            app.active_modal = Some(crate::ui::dialogs::modal_state::ActiveModal::Confirm(
                crate::ui::dialogs::confirm::State::recursive_scan(
                    t!("win.confirm_recursive_title").to_string(),
                    t!("win.confirm_recursive_msg").to_string(),
                ),
            ));
        }
        if old_recursive && !app.settings.recursive {
            if let Some(dir) = app.settings.last_image_dir.clone() {
                app.load_directory(dir);
            }
            app.queue_save();
        }

        if themed_labeled_toggle(
            ui,
            &mut app.settings.preload,
            t!("label.enable_preload"),
            &palette,
        )
        .changed()
        {
            app.queue_save();
        }

        let old_pair_handling = app.settings.paired_raw_jpeg_handling;
        ui.horizontal(|ui| {
            ui.label(t!("label.paired_raw_jpeg_handling"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                egui::ComboBox::from_id_salt("paired_raw_jpeg_handling_combo")
                    .width(PAIRED_RAW_JPEG_COMBO_WIDTH)
                    .selected_text(app.settings.paired_raw_jpeg_handling.label())
                    .show_ui(ui, |ui| {
                        ui.set_min_width(PAIRED_RAW_JPEG_COMBO_WIDTH);
                        ui.selectable_value(
                            &mut app.settings.paired_raw_jpeg_handling,
                            PairedRawJpegHandling::ShowBoth,
                            PairedRawJpegHandling::ShowBoth.label(),
                        );
                        ui.selectable_value(
                            &mut app.settings.paired_raw_jpeg_handling,
                            PairedRawJpegHandling::SkipRaw,
                            PairedRawJpegHandling::SkipRaw.label(),
                        );
                        ui.selectable_value(
                            &mut app.settings.paired_raw_jpeg_handling,
                            PairedRawJpegHandling::SkipJpeg,
                            PairedRawJpegHandling::SkipJpeg.label(),
                        );
                    });
            });
        });
        if old_pair_handling != app.settings.paired_raw_jpeg_handling {
            if let Some(dir) = app.settings.last_image_dir.clone() {
                app.load_directory(dir);
            }
            app.queue_save();
        }

        if themed_labeled_toggle(
            ui,
            &mut app.settings.resume_last_image,
            t!("label.resume_last"),
            &palette,
        )
        .changed()
        {
            app.queue_save();
        }
    });
}
