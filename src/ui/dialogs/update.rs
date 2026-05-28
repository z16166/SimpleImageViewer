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

pub struct State {
    pub candidate: crate::update::core::UpdateCandidate,
}

impl State {
    pub fn new(candidate: crate::update::core::UpdateCandidate) -> Self {
        Self { candidate }
    }
}

pub fn show(state: &mut State, ctx: &Context, palette: &ThemePalette) -> ModalResult {
    let mut result = ModalResult::Pending;
    let candidate = &state.candidate;

    MovableModal::new("update_dialog", t!("update.dialog_title").to_string())
        .default_size([620.0, 480.0])
        .min_size([520.0, 360.0])
        .show(ctx, palette, |ui| {
            ui.label(
                RichText::new(t!(
                    "update.latest_version",
                    version = candidate.version.clone()
                ))
                .color(palette.accent2)
                .strong(),
            );
            ui.label(t!(
                "update.current_version",
                version = crate::update::core::current_version()
            ));
            if !candidate.published_at.is_empty() {
                ui.label(t!(
                    "update.release_date",
                    date = candidate.published_at.clone()
                ));
            }
            ui.label(t!(
                "update.asset_size",
                size = format_bytes(candidate.asset_size)
            ));
            ui.add_space(8.0);
            ui.label(RichText::new(t!("update.changelog")).strong());
            egui::ScrollArea::vertical()
                .max_height(220.0)
                .show(ui, |ui| {
                    ui.label(if candidate.release_notes.trim().is_empty() {
                        candidate.release_page_url.as_str()
                    } else {
                        candidate.release_notes.as_str()
                    });
                });
            ui.add_space(12.0);
            if !cfg!(target_os = "windows") {
                ui.label(
                    RichText::new(t!("update.unsupported_platform")).color(palette.text_muted),
                );
            }
            ui.horizontal(|ui| {
                if cfg!(target_os = "windows")
                    && ui
                        .add(
                            egui::Button::new(
                                RichText::new(t!("btn.update_now")).color(egui::Color32::WHITE),
                            )
                            .fill(palette.button_primary)
                            .corner_radius(egui::CornerRadius::same(4)),
                        )
                        .clicked()
                {
                    result = ModalResult::Confirmed(
                        crate::ui::dialogs::modal_state::ModalAction::StartUpdate,
                    );
                }
                if styled_button(ui, t!("btn.open_release_page"), palette).clicked() {
                    result = ModalResult::Confirmed(
                        crate::ui::dialogs::modal_state::ModalAction::OpenUpdateReleasePage,
                    );
                }
                if styled_button(ui, t!("btn.ignore_version"), palette).clicked() {
                    result = ModalResult::Confirmed(
                        crate::ui::dialogs::modal_state::ModalAction::IgnoreUpdateVersion,
                    );
                }
                if styled_button(ui, t!("btn.later"), palette).clicked() {
                    result = ModalResult::Dismissed;
                }
            });
        });

    result
}

fn format_bytes(bytes: u64) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    if bytes == 0 {
        return "-".to_string();
    }
    format!("{:.1} MiB", bytes as f64 / MIB)
}
