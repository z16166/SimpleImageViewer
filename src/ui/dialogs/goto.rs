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

// ── Private state ────────────────────────────────────────────────────────────

/// Runtime state for the "Go to image #N" dialog.
pub struct State {
    input: String,
    needs_focus: bool,
    total: usize,
    current_index: usize,
}

impl State {
    pub fn new(total: usize, current_index: usize) -> Self {
        Self {
            input: String::new(),
            needs_focus: true,
            total,
            current_index,
        }
    }
}

// ── Rendering ────────────────────────────────────────────────────────────────

pub fn show(state: &mut State, ctx: &Context, palette: &ThemePalette) -> ModalResult {
    let mut result = ModalResult::Pending;

    const WIDTH: f32 = 320.0;
    const HEIGHT: f32 = 120.0;

    MovableModal::new("goto_dialog", t!("goto.title"))
        .resizable(false)
        .default_size([WIDTH, HEIGHT])
        .show(ctx, palette, |ui| {
            ui.label(
                RichText::new(t!("goto.hint", total = state.total.to_string()))
                    .color(palette.text_muted)
                    .small(),
            );
            ui.add_space(6.0);

            let resp = ui.add(
                egui::TextEdit::singleline(&mut state.input)
                    .desired_width(f32::INFINITY)
                    .hint_text(format!("{}", state.current_index + 1)),
            );

            if state.needs_focus {
                resp.request_focus();
                state.needs_focus = false;
            }

            if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                result = try_confirm(&state.input, state.total);
            }
            if ui.input(|i| i.key_pressed(Key::Escape)) {
                result = ModalResult::Dismissed;
            }

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if styled_button(ui, t!("btn.go"), palette).clicked() {
                    result = try_confirm(&state.input, state.total);
                }
                if styled_button(ui, &t!("btn.cancel").to_string(), palette).clicked() {
                    result = ModalResult::Dismissed;
                }
            });
        });

    result
}

// ── Private helpers ───────────────────────────────────────────────────────────

fn try_confirm(input: &str, total: usize) -> ModalResult {
    let raw: usize = input.trim().parse().unwrap_or(0);
    if raw >= 1 && total > 0 {
        ModalResult::Confirmed(ModalAction::GotoIndex((raw - 1).min(total - 1)))
    } else {
        ModalResult::Pending
    }
}
