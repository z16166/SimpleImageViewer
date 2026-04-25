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
use crate::ui::dialogs::modal_state::{ModalAction, ModalResult};
use crate::ui::utils::styled_button;
use eframe::egui::{self, Color32, Context, RichText};
use rust_i18n::t;

// ── Private state ─────────────────────────────────────────────────────────────

/// Runtime state for the Windows file association dialog.
///
/// Encapsulated here so that `formats` and `selections` are not visible
/// outside this module.  The dispatch layer reaches the data only through
/// [`State::new`], [`show`], and the snapshot extracted inside
/// `handle_modal_action` via `extract_selected`.
pub struct State {
    /// All recognised image format descriptors (snapshot of the global registry).
    formats: Vec<crate::formats::ImageFormat>,
    /// Parallel checkbox flags, one per entry in `formats`.
    selections: Vec<bool>,
}

impl State {
    /// Snapshot the global format registry and pre-select every format.
    pub fn new(formats: Vec<crate::formats::ImageFormat>) -> Self {
        let len = formats.len();
        Self {
            formats,
            selections: vec![true; len],
        }
    }

    /// Collect the extensions that are currently selected.
    ///
    /// Called by the dispatch layer after the user confirms, before the state
    /// is dropped.  This is the only way external code can read the selection.
    pub fn selected_extensions(&self) -> Vec<&str> {
        self.formats
            .iter()
            .zip(self.selections.iter())
            .filter(|(_, sel)| **sel)
            .map(|(fmt, _)| fmt.extension.as_str())
            .collect()
    }
}

// ── Rendering ─────────────────────────────────────────────────────────────────

/// Render the file association management modal for one frame.
pub fn show(state: &mut State, ctx: &Context, palette: &ThemePalette) -> ModalResult {
    let mut result = ModalResult::Pending;

    const WIDTH: f32 = 420.0;
    // Height estimate: label(18) + sp(8) + buttons(24) + sp(4) +
    //   ScrollArea max_height(400) + sp(8) + separator(1) + sp(4) + footer(24) ≈ 491px
    const HEIGHT_ESTIMATE: f32 = 491.0;

    MovableModal::new("file_assoc_dialog", t!("win.assoc_dialog_title"))
        .resizable(false)
        .default_size([WIDTH, HEIGHT_ESTIMATE])
        .min_size([320.0, 300.0])
        .show(ctx, palette, |ui| {
            ui.label(
                RichText::new(t!("win.assoc_dialog_msg").to_string()).color(palette.text_muted),
            );
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                if styled_button(ui, t!("btn.select_all"), palette).clicked() {
                    state.selections.iter_mut().for_each(|s| *s = true);
                }
                if styled_button(ui, t!("btn.deselect_all"), palette).clicked() {
                    state.selections.iter_mut().for_each(|s| *s = false);
                }
            });
            ui.add_space(4.0);

            egui::ScrollArea::vertical()
                .max_height(400.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.set_max_width(ui.available_width() - 16.0);
                    use crate::formats::FormatGroup;
                    for (group, key) in [
                        (FormatGroup::Standard, "win.group_standard"),
                        (FormatGroup::Pro, "win.group_pro"),
                        (FormatGroup::WicSystem, "win.group_wic_system"),
                        (FormatGroup::WicRaw, "win.group_wic_raw"),
                        (FormatGroup::Others, "win.group_others"),
                    ] {
                        render_format_group(ui, state, group, &t!(key), palette);
                    }
                });

            ui.add_space(8.0);
            ui.separator();
            ui.add_space(4.0);

            let selected_count = state.selections.iter().filter(|&&s| s).count();
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        selected_count > 0,
                        egui::Button::new(
                            RichText::new(t!(
                                "win.apply_formats",
                                count = selected_count.to_string()
                            ))
                            .color(Color32::WHITE),
                        )
                        .fill(palette.button_primary)
                        .corner_radius(egui::CornerRadius::same(4)),
                    )
                    .clicked()
                {
                    result = ModalResult::Confirmed(ModalAction::ApplyFileAssoc);
                }
                ui.add_space(8.0);
                if styled_button(ui, t!("win.btn_cancel"), palette).clicked() {
                    result = ModalResult::Dismissed;
                }
            });

            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                result = ModalResult::Dismissed;
            }
        });

    result
}

// ── Private helpers ───────────────────────────────────────────────────────────

fn render_format_group(
    ui: &mut egui::Ui,
    state: &mut State,
    group: crate::formats::FormatGroup,
    group_name: &str,
    palette: &ThemePalette,
) {
    let indices: Vec<usize> = state
        .formats
        .iter()
        .enumerate()
        .filter(|(_, f)| f.group == group)
        .map(|(i, _)| i)
        .collect();
    if indices.is_empty() {
        return;
    }

    ui.add_space(8.0);
    ui.label(RichText::new(group_name).strong().color(palette.accent2));
    ui.add_space(2.0);

    let cols = 5;
    let rows = (indices.len() + cols - 1) / cols;

    egui::Grid::new(format!("file_assoc_grid_{:?}", group))
        .num_columns(cols)
        .spacing([18.0, 4.0])
        .show(ui, |ui| {
            for row in 0..rows {
                for col in 0..cols {
                    let gi = row * cols + col;
                    if gi < indices.len() {
                        let fi = indices[gi];
                        let label = format!(".{}", state.formats[fi].extension);
                        let desc = state.formats[fi].description.clone();
                        ui.checkbox(&mut state.selections[fi], label)
                            .on_hover_text(&desc);
                    }
                }
                ui.end_row();
            }
        });
}
