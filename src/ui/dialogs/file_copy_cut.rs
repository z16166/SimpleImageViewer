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
use eframe::egui::{self, Context, Key, RichText};
use rust_i18n::t;
use std::path::PathBuf;

pub struct State {
    pub input: String,
    pub needs_focus: bool,
    pub is_cut: bool,
}

impl State {
    pub fn new(is_cut: bool, initial_dir: Option<PathBuf>) -> Self {
        Self {
            input: initial_dir
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
            needs_focus: true,
            is_cut,
        }
    }
}

pub fn show(state: &mut State, ctx: &Context, palette: &ThemePalette) -> ModalResult {
    let mut result = ModalResult::Pending;

    const DEFAULT_DIALOG_WIDTH: f32 = 440.0;
    const DEFAULT_DIALOG_HEIGHT: f32 = 150.0;

    let title = if state.is_cut {
        t!("file_copy_cut.title_cut").to_string()
    } else {
        t!("file_copy_cut.title_copy").to_string()
    };

    MovableModal::new("file_copy_cut_dialog", title)
        .resizable(true)
        .default_size([DEFAULT_DIALOG_WIDTH, DEFAULT_DIALOG_HEIGHT])
        .show(ctx, palette, |ui| {
            ui.label(
                RichText::new(t!("file_copy_cut.prompt"))
                    .color(palette.text_muted)
                    .small(),
            );
            ui.add_space(6.0);

            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if styled_button(ui, t!("file_copy_cut.browse"), palette).clicked() {
                        let mut dialog = rfd::FileDialog::new();
                        if !state.input.trim().is_empty() {
                            dialog = dialog.set_directory(std::path::Path::new(state.input.trim()));
                        }
                        if let Some(dir) = dialog.pick_folder() {
                            state.input = dir.to_string_lossy().into_owned();
                        }
                    }

                    let text_edit = egui::TextEdit::singleline(&mut state.input)
                        .desired_width(ui.available_width() - 8.0);
                    let resp = ui.add(text_edit);

                    if state.needs_focus {
                        resp.request_focus();
                        state.needs_focus = false;
                    }

                    if resp.has_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        result = try_confirm(state);
                    }
                });
            });

            if ui.input(|i| i.key_pressed(Key::Escape)) {
                result = ModalResult::Dismissed;
            }

            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if styled_button(ui, t!("btn.ok"), palette).clicked() {
                    result = try_confirm(state);
                }
                if styled_button(ui, t!("btn.cancel"), palette).clicked() {
                    result = ModalResult::Dismissed;
                }
            });
        });

    result
}

fn try_confirm(state: &State) -> ModalResult {
    let target = state.input.trim();
    if target.is_empty() {
        return ModalResult::Pending;
    }
    let path = std::path::Path::new(target);
    if path.is_file() {
        return ModalResult::Pending;
    }
    ModalResult::Confirmed(ModalAction::FileCopyCut {
        is_cut: state.is_cut,
        target_dir: PathBuf::from(target),
    })
}
