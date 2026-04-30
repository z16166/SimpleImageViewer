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

use crate::theme::ThemePalette;
use crate::ui::dialogs::MovableModal;
use crate::ui::dialogs::modal_state::ModalResult;
use crate::ui::utils::styled_button;
use eframe::egui::{self, Color32, Context, RichText};
use rust_i18n::t;

// ── Private state ─────────────────────────────────────────────────────────────

/// Runtime state for the EXIF data viewer dialog.
pub struct State {
    pub path: std::path::PathBuf,
    /// Parsed EXIF key-value pairs, or `None` if the image has no EXIF data.
    pub data: Option<Vec<(String, String)>>,
    pub loading: bool,
}

impl State {
    /// Create state in loading mode.
    pub fn new_loading(path: std::path::PathBuf) -> Self {
        Self {
            path,
            data: None,
            loading: true,
        }
    }
}

// ── Rendering ─────────────────────────────────────────────────────────────────

pub fn show(state: &State, ctx: &Context, palette: &ThemePalette) -> ModalResult {
    let mut result = ModalResult::Pending;
    let mut copy_text: Option<String> = None;

    const WIDTH: f32 = 600.0;
    const HEIGHT: f32 = 400.0;

    MovableModal::new("exif_dialog", t!("exif.title"))
        .default_size([WIDTH, HEIGHT])
        .min_size([400.0, 200.0])
        .show(ctx, palette, |ui| {
            // ── No-data notice ───────────────────────────────────────────────
            // ── Loading state ───────────────────────────────────────────────
            if state.loading {
                ui.add_space(20.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(t!("exif.loading").to_string());
                });
                ui.add_space(20.0);
            } else if state.data.is_none() {
                // ── No-data notice ───────────────────────────────────────────────
                ui.add_space(10.0);
                ui.label(
                    RichText::new(t!("exif.no_data").to_string())
                        .color(Color32::from_rgb(255, 180, 60))
                        .strong(),
                );
                ui.add_space(10.0);
            }

            // ── Fixed bottom bar: Copy + Close ────────────────────────────────
            egui::Panel::bottom("exif_footer")
                .resizable(false)
                .show_inside(ui, |ui| {
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if state.data.is_some() {
                            if styled_button(ui, &t!("exif.copy").to_string(), palette).clicked() {
                                copy_text = state.data.as_ref().map(|d| {
                                    d.iter()
                                        .map(|(k, v)| format!("{}: {}", k, v))
                                        .collect::<Vec<_>>()
                                        .join("\n")
                                });
                                result = ModalResult::Dismissed;
                            }
                        }
                        if styled_button(ui, &t!("btn.close").to_string(), palette).clicked() {
                            result = ModalResult::Dismissed;
                        }
                    });
                    ui.add_space(6.0);
                });

            if let Some(data) = &state.data {
                render_table(ui, data, palette);
            }

            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                result = ModalResult::Dismissed;
            }
        });

    if let Some(text) = copy_text {
        ctx.copy_text(text);
    }

    result
}

// ── Private helpers ───────────────────────────────────────────────────────────

fn render_table(ui: &mut egui::Ui, data: &[(String, String)], palette: &ThemePalette) {
    egui::ScrollArea::both()
        .id_salt("exif_scroll")
        .show(ui, |ui| {
            egui::Grid::new("exif_grid")
                .striped(true)
                .num_columns(2)
                .spacing([20.0, 8.0])
                .min_col_width(120.0)
                .show(ui, |ui| {
                    for (k, v) in data {
                        // Key column
                        ui.label(RichText::new(k).color(palette.text_muted).monospace());

                        // Value column
                        ui.add(
                            egui::Label::new(
                                RichText::new(v).color(palette.text_normal).monospace(),
                            )
                            .selectable(true),
                        );

                        ui.end_row();
                    }
                });
        });
}
