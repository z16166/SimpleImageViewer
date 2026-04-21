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

use eframe::egui::{self, Color32, Context, RichText};
use crate::app::{ImageViewerApp, extract_xmp};
use crate::ui::utils::styled_button;
use rust_i18n::t;

pub fn draw(app: &mut ImageViewerApp, ctx: &Context) {
    if !app.show_xmp_window {
        return;
    }

    if app.cached_xmp_data.is_none() && !app.image_files.is_empty() {
        let path = &app.image_files[app.current_index];
        if let Some((data, raw)) = extract_xmp(path) {
            app.cached_xmp_data = Some(data);
            app.cached_xmp_xml = Some(raw);
        }
    }

    let mut close_xmp = false;
    let mut close_and_copy = false;
    egui::Window::new(t!("xmp.title").to_string())
        .id(egui::Id::new("xmp_window"))
        .collapsible(false)
        .resizable(true)
        .default_pos(ctx.input(|i| i.content_rect()).center() - egui::vec2(320.0, 240.0))
        .default_size([640.0, 500.0])
        .show(ctx, |ui| {
            ui.set_max_width(ui.available_width());
            if app.cached_xmp_data.is_none() {
                ui.add_space(10.0);
                ui.label(RichText::new(t!("xmp.no_data").to_string()).color(Color32::from_rgb(255, 180, 60)).strong());
            }

            egui::Panel::bottom("xmp_footer")
                .resizable(false)
                .show_inside(ui, |ui| {
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if let Some(xml_str) = &app.cached_xmp_xml {
                            if styled_button(ui, &t!("xmp.copy_text").to_string(), &app.cached_palette).clicked() {
                                close_and_copy = true;
                            }
                            if styled_button(ui, &t!("xmp.copy_xml").to_string(), &app.cached_palette).clicked() {
                                ctx.copy_text(xml_str.clone());
                                app.show_xmp_window = false;
                            }
                        }
                        if styled_button(ui, &t!("btn.close").to_string(), &app.cached_palette).clicked() {
                            close_xmp = true;
                        }
                    });
                    ui.add_space(10.0);
                });

            if let Some(data) = &app.cached_xmp_data {
                render_xmp_table(ui, data, &app.cached_palette);
            }
            ui.add_space(10.0);
        });

    if close_and_copy {
        if let Some(data) = &app.cached_xmp_data {
            let text = data.iter()
                .map(|(k, v)| format!("{}: {}", k, v))
                .collect::<Vec<_>>()
                .join("\n");
            ctx.copy_text(text);
        }
        app.show_xmp_window = false;
    }
    if close_xmp {
        app.show_xmp_window = false;
    }
}

fn render_xmp_table(ui: &mut egui::Ui, data: &[(String, String)], palette: &crate::theme::ThemePalette) {
    egui::CentralPanel::default().show_inside(ui, |ui| {
        use egui_extras::{Column, TableBuilder};
        egui::ScrollArea::horizontal().show(ui, |ui| {
            TableBuilder::new(ui)
                .striped(true)
                .resizable(true)
                .vscroll(true)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::initial(180.0).at_least(120.0))
                .column(Column::remainder().at_least(100.0))
                .body(|body| {
                    body.rows(24.0, data.len(), |mut row| {
                        let index = row.index();
                        let (k, v) = &data[index];
                        row.col(|ui| {
                            ui.label(RichText::new(k).color(palette.text_muted).monospace());
                        });
                        row.col(|ui| {
                            let _ = ui.selectable_label(false, RichText::new(v).color(palette.text_normal).monospace());
                        });
                    });
                });
        });
    });
}
