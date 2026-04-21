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
use crate::app::ImageViewerApp;
use crate::ui::utils::styled_button;
use rust_i18n::t;

pub fn draw(app: &mut ImageViewerApp, ctx: &Context) {
    if !app.show_file_assoc_dialog {
        return;
    }

    // Dark background overlay
    let screen_rect = ctx.input(|i| i.content_rect());
    let bg_layer = egui::LayerId::new(egui::Order::Background, egui::Id::new("file_assoc_bg"));
    ctx.layer_painter(bg_layer).add(
        egui::Shape::rect_filled(
            screen_rect,
            egui::CornerRadius::ZERO,
            Color32::from_black_alpha(180),
        ),
    );

    let mut do_apply = false;
    let mut do_cancel = false;

    egui::Window::new(t!("win.assoc_dialog_title"))
        .id(egui::Id::new("assoc_dialog"))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .default_width(420.0)
        .frame(
            egui::Frame::window(&ctx.global_style())
                .fill(app.cached_palette.panel_bg)
                .shadow(egui::epaint::Shadow::NONE),
        )
        .show(ctx, |ui| {
            ui.visuals_mut().override_text_color = Some(app.cached_palette.text_normal);

            ui.label(
                RichText::new(t!("win.assoc_dialog_msg").to_string())
                    .color(app.cached_palette.text_muted),
            );
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                if styled_button(ui, t!("btn.select_all"), &app.cached_palette).clicked() {
                    for sel in app.file_assoc_selections.iter_mut() {
                        *sel = true;
                    }
                }
                if styled_button(ui, t!("btn.deselect_all"), &app.cached_palette).clicked() {
                    for sel in app.file_assoc_selections.iter_mut() {
                        *sel = false;
                    }
                }
            });
            ui.add_space(4.0);

            egui::ScrollArea::vertical()
                .max_height(400.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.set_max_width(ui.available_width() - 16.0);
                use crate::formats::FormatGroup;
                let groups = [
                    (FormatGroup::Standard, "Standard Formats"),
                    (FormatGroup::Pro, "Professional (PS/TIFF/HEIF)"),
                    (FormatGroup::WicSystem, "Windows System (WIC)"),
                    (FormatGroup::WicRaw, "Camera RAW (WIC)"),
                    (FormatGroup::Others, "Other Formats"),
                ];

                for (group, group_name) in groups {
                    render_format_group(ui, app, group, group_name);
                }
            });

            ui.add_space(8.0);
            ui.separator();
            ui.add_space(4.0);

            let selected_count = app.file_assoc_selections.iter().filter(|&&s| s).count();

            ui.horizontal(|ui| {
                let apply_enabled = selected_count > 0;
                if ui
                    .add_enabled(
                        apply_enabled,
                        egui::Button::new(
                            RichText::new(t!("win.apply_formats", count = selected_count.to_string()))
                                .color(Color32::WHITE)
                        )
                        .fill(app.cached_palette.accent)
                        .corner_radius(egui::CornerRadius::same(4)),
                    )
                    .clicked()
                {
                    do_apply = true;
                }
                ui.add_space(8.0);
                if styled_button(ui, t!("win.btn_cancel"), &app.cached_palette).clicked() {
                    do_cancel = true;
                }
            });
        });

    if do_apply {
        let selected: Vec<&str> = app.file_assoc_formats
            .iter()
            .zip(app.file_assoc_selections.iter())
            .filter(|(_, sel)| **sel)
            .map(|(fmt, _)| fmt.extension.as_str())
            .collect();
        crate::windows_utils::register_file_associations(&selected);
        app.show_file_assoc_dialog = false;

        rfd::MessageDialog::new()
            .set_title(t!("win.assoc_done_title").to_string())
            .set_description(t!("win.assoc_done_msg").to_string())
            .set_buttons(rfd::MessageButtons::Ok)
            .set_level(rfd::MessageLevel::Info)
            .show();
    }
    if do_cancel {
        app.show_file_assoc_dialog = false;
    }
}

fn render_format_group(ui: &mut egui::Ui, app: &mut ImageViewerApp, group: crate::formats::FormatGroup, group_name: &str) {
    let group_indices: Vec<usize> = app.file_assoc_formats.iter()
        .enumerate()
        .filter(|(_, f)| f.group == group)
        .map(|(i, _)| i)
        .collect();

    if group_indices.is_empty() { return; }

    ui.add_space(8.0);
    ui.label(RichText::new(group_name).strong().color(app.cached_palette.accent2));
    ui.add_space(2.0);

    let cols = 5;
    let rows = (group_indices.len() + cols - 1) / cols;

    egui::Grid::new(format!("file_assoc_grid_{:?}", group))
        .num_columns(cols)
        .spacing([18.0, 4.0])
        .show(ui, |ui| {
            for row in 0..rows {
                for col in 0..cols {
                    let grid_idx = row * cols + col;
                    if grid_idx < group_indices.len() {
                        let fmt_idx = group_indices[grid_idx];
                        let fmt = &app.file_assoc_formats[fmt_idx];
                        let label = format!(".{}", fmt.extension);
                        ui.checkbox(&mut app.file_assoc_selections[fmt_idx], label)
                            .on_hover_text(&fmt.description);
                    }
                }
                ui.end_row();
            }
        });
}
