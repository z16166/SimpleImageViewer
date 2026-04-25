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

/// Runtime state for the XMP metadata viewer dialog.
///
/// Both `data` and `xml` are private implementation details — the dispatch
/// layer only needs to call [`State::from_path`] and [`show`].
pub struct State {
    /// Parsed XMP key-value pairs, or `None` if no XMP metadata was found.
    data: Option<Vec<(String, String)>>,
    /// Raw XML string, available for the "Copy XML" button.
    xml: Option<String>,
}

impl State {
    /// Create state by extracting XMP from `path`.
    ///
    /// If XMP extraction fails the state is still valid — the dialog will
    /// show a "no XMP data" message.
    pub fn from_path(path: &std::path::Path) -> Self {
        match crate::app::extract_xmp(path) {
            Some((data, xml)) => Self {
                data: Some(data),
                xml: Some(xml),
            },
            None => Self {
                data: None,
                xml: None,
            },
        }
    }
}

// ── Rendering ─────────────────────────────────────────────────────────────────

/// Render the XMP metadata viewer modal for one frame.
pub fn show(state: &State, ctx: &Context, palette: &ThemePalette) -> ModalResult {
    let mut result = ModalResult::Pending;
    let mut copy_text: Option<String> = None;
    let mut copy_xml: Option<String> = None;

    const WIDTH: f32 = 640.0;
    const HEIGHT: f32 = 500.0;

    MovableModal::new("xmp_dialog", t!("xmp.title"))
        .default_size([WIDTH, HEIGHT])
        .min_size([400.0, 200.0])
        .show(ctx, palette, |ui| {
            // ── No-data notice ───────────────────────────────────────────────────
            if state.data.is_none() {
                ui.add_space(10.0);
                ui.label(
                    RichText::new(t!("xmp.no_data").to_string())
                        .color(Color32::from_rgb(255, 180, 60))
                        .strong(),
                );
                ui.add_space(10.0);
            }

            // ── Fixed bottom bar: Copy + Close ────────────────────────────────
            egui::Panel::bottom("xmp_footer")
                .resizable(false)
                .show_inside(ui, |ui| {
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if state.data.is_some() {
                            if styled_button(ui, &t!("xmp.copy_text").to_string(), palette)
                                .clicked()
                            {
                                copy_text = state.data.as_ref().map(|d| {
                                    d.iter()
                                        .map(|(k, v)| format!("{}: {}", k, v))
                                        .collect::<Vec<_>>()
                                        .join("\n")
                                });
                                result = ModalResult::Dismissed;
                            }
                            if let Some(xml) = &state.xml {
                                if styled_button(ui, &t!("xmp.copy_xml").to_string(), palette)
                                    .clicked()
                                {
                                    copy_xml = Some(xml.clone());
                                    result = ModalResult::Dismissed;
                                }
                            }
                        }
                        if styled_button(ui, &t!("btn.close").to_string(), palette).clicked() {
                            result = ModalResult::Dismissed;
                        }
                    });
                    ui.add_space(6.0);
                });

            // ── Scrollable data table fills remaining space ───────────────────
            egui::CentralPanel::default().show_inside(ui, |ui| {
                if let Some(data) = &state.data {
                    render_table(ui, data, palette);
                }
            });

            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                result = ModalResult::Dismissed;
            }
        });

    if let Some(text) = copy_text {
        ctx.copy_text(text);
    }
    if let Some(xml) = copy_xml {
        ctx.copy_text(xml);
    }

    result
}

// ── Private helpers ───────────────────────────────────────────────────────────

fn render_table(ui: &mut egui::Ui, data: &[(String, String)], palette: &ThemePalette) {
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
                    let (k, v) = &data[row.index()];
                    row.col(|ui| {
                        ui.label(RichText::new(k).color(palette.text_muted).monospace());
                    });
                    row.col(|ui| {
                        let _ = ui.selectable_label(
                            false,
                            RichText::new(v).color(palette.text_normal).monospace(),
                        );
                    });
                });
            });
    });
}
