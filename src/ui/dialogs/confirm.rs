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
use eframe::egui::{self, Context, RichText};
use rust_i18n::t;

// ── Private state ─────────────────────────────────────────────────────────────

/// Runtime state for a generic confirm/cancel dialog.
///
/// This dialog replaces any use of `rfd::MessageDialog` for simple yes/no
/// confirmations, ensuring consistent behaviour across all platforms
/// (including Linux where rfd may lack system dialog support).
pub struct State {
    /// Dialog title (shown as a heading).
    title: String,
    /// Body message (shown as a label below the title).
    message: String,
    /// Label on the confirm button (e.g. "Enable", "Delete", "OK").
    confirm_label: String,
    /// Label on the cancel button.
    cancel_label: String,
    /// Opaque tag identifying which operation triggered the dialog.
    /// The dispatch layer (handle_modal_action) uses this to decide what to
    /// do when the user confirms.
    pub(crate) tag: ConfirmTag,
}

/// Identifies the operation being confirmed so the dispatch layer knows what
/// to execute on `Confirmed`.
///
/// Adding a new confirmation flow: add a variant here, open the dialog with
/// that variant as `tag`, and add a match arm in `handle_modal_action`.
#[derive(Clone, Debug, PartialEq)]
pub enum ConfirmTag {
    /// User is enabling recursive directory scan.
    EnableRecursiveScan,
}

impl State {
    /// Build state for the "enable recursive scan" confirmation.
    pub fn recursive_scan(title: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            message: message.into(),
            confirm_label: t!("btn.ok").to_string(),
            cancel_label: t!("btn.cancel").to_string(),
            tag: ConfirmTag::EnableRecursiveScan,
        }
    }
}

// ── Rendering ─────────────────────────────────────────────────────────────────

/// Render the generic confirm dialog for one frame.
///
/// Returns [`ModalResult::Confirmed`] with a zero-payload action when the user
/// clicks the confirm button, or [`ModalResult::Dismissed`] on cancel/Escape.
/// The actual `ModalAction` variant is resolved in `handle_modal_action` using
/// `state.tag`.
pub fn show(state: &State, ctx: &Context, palette: &ThemePalette) -> ModalResult {
    let mut result = ModalResult::Pending;

    const WIDTH: f32 = 400.0;
    const HEIGHT: f32 = 160.0;

    MovableModal::new("confirm_dialog", state.title.clone())
        .resizable(false)
        .default_size([WIDTH, HEIGHT])
        .show(ctx, palette, |ui| {
            // Warning icon + message
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new("⚠")
                        .size(18.0)
                        .color(egui::Color32::from_rgb(255, 180, 60)),
                );
                ui.add_space(4.0);
                ui.label(RichText::new(&state.message).color(palette.text_normal));
            });
            ui.add_space(16.0);

            ui.horizontal(|ui| {
                if ui
                    .add(
                        egui::Button::new(
                            RichText::new(&state.confirm_label).color(egui::Color32::WHITE),
                        )
                        .fill(palette.button_primary)
                        .corner_radius(egui::CornerRadius::same(4)),
                    )
                    .clicked()
                {
                    result = ModalResult::Confirmed(
                        crate::ui::dialogs::modal_state::ModalAction::ConfirmTagged(
                            state.tag.clone(),
                        ),
                    );
                }
                ui.add_space(8.0);
                if styled_button(ui, &state.cancel_label, palette).clicked() {
                    result = ModalResult::Dismissed;
                }
            });

            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                result = ModalResult::Dismissed;
            }
        });

    result
}
