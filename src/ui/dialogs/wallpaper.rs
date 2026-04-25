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
use eframe::egui::{self, Context, RichText};
use rust_i18n::t;

// ── Private state ─────────────────────────────────────────────────────────────

/// Runtime state for the wallpaper mode selector dialog.
///
/// Fields are private; other modules can only create an instance via
/// [`State::new`] and cannot inspect or mutate the internals directly.
pub struct State {
    /// The wallpaper fitting mode chosen by the radio buttons.
    selected_mode: String,
    /// Path of the currently active desktop wallpaper (display only).
    current_system_wallpaper: Option<String>,
}

impl State {
    /// Create state for a freshly-opened wallpaper dialog.
    ///
    /// `current_system_wallpaper` is read from the OS at open time and shown
    /// as an informational label; it is never mutated by the dialog itself.
    pub fn new(current_system_wallpaper: Option<String>) -> Self {
        Self {
            selected_mode: "Crop".to_string(),
            current_system_wallpaper,
        }
    }
}

// ── Rendering ─────────────────────────────────────────────────────────────────

/// Render the wallpaper mode selector modal for one frame.
pub fn show(
    state: &mut State,
    current_image_path: &str,
    current_image_res: Option<(u32, u32)>,
    ctx: &Context,
    palette: &ThemePalette,
) -> ModalResult {
    let mut result = ModalResult::Pending;

    const WIDTH: f32 = 520.0;
    const HEIGHT: f32 = 320.0;

    MovableModal::new("wallpaper_dialog", t!("wallpaper.title"))
        .default_size([WIDTH, HEIGHT])
        .min_size([400.0, 240.0])
        .show(ctx, palette, |ui| {
            if let Some(ref current) = state.current_system_wallpaper {
                ui.label(
                    RichText::new(t!("wallpaper.current"))
                        .color(palette.text_muted)
                        .small(),
                );
                egui::ScrollArea::horizontal()
                    .id_salt("curr_wp_scroll")
                    .min_scrolled_height(24.0)
                    .show(ui, |ui| {
                        ui.vertical(|ui| {
                            ui.add_space(2.0);
                            ui.add(
                                egui::Label::new(current)
                                    .selectable(true)
                                    .wrap_mode(egui::TextWrapMode::Extend),
                            );
                            ui.add_space(4.0);
                        });
                    });
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(8.0);
            }

            ui.label(
                RichText::new(t!("wallpaper.new_path"))
                    .color(palette.text_muted)
                    .small(),
            );
            egui::ScrollArea::horizontal()
                .id_salt("new_wp_scroll")
                .min_scrolled_height(24.0)
                .show(ui, |ui| {
                    ui.vertical(|ui| {
                        ui.add_space(2.0);
                        ui.add(
                            egui::Label::new(current_image_path)
                                .selectable(true)
                                .wrap_mode(egui::TextWrapMode::Extend),
                        );
                        ui.add_space(4.0);
                    });
                });

            if let Some((w, h)) = current_image_res {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(t!("wallpaper.resolution"))
                        .color(palette.text_muted)
                        .small(),
                );
                ui.label(format!("{} × {}", w, h));
            }

            ui.add_space(12.0);
            ui.separator();
            ui.add_space(8.0);
            ui.label(
                RichText::new(t!("wallpaper.mode"))
                    .color(palette.accent2)
                    .strong(),
            );

            ui.vertical(|ui| {
                ui.radio_value(
                    &mut state.selected_mode,
                    "Crop".to_string(),
                    t!("wallpaper.crop").to_string(),
                );
                ui.radio_value(
                    &mut state.selected_mode,
                    "Fit".to_string(),
                    t!("wallpaper.fit").to_string(),
                );
                ui.radio_value(
                    &mut state.selected_mode,
                    "Stretch".to_string(),
                    t!("wallpaper.stretch").to_string(),
                );
                ui.radio_value(
                    &mut state.selected_mode,
                    "Tile".to_string(),
                    t!("wallpaper.tile").to_string(),
                );
                ui.radio_value(
                    &mut state.selected_mode,
                    "Center".to_string(),
                    t!("wallpaper.center").to_string(),
                );
                ui.radio_value(
                    &mut state.selected_mode,
                    "Span".to_string(),
                    t!("wallpaper.span").to_string(),
                );
            });

            ui.add_space(16.0);
            ui.horizontal(|ui| {
                if styled_button(ui, &t!("btn.set_wallpaper").to_string(), palette).clicked() {
                    result = ModalResult::Confirmed(ModalAction::SetWallpaper(
                        state.selected_mode.clone(),
                    ));
                }
                if styled_button(ui, &t!("btn.cancel").to_string(), palette).clicked() {
                    result = ModalResult::Dismissed;
                }
            });

            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                result = ModalResult::Dismissed;
            }
        });

    result
}

// ── Side-effect helper ────────────────────────────────────────────────────────

/// Spawn a background thread to actually change the desktop wallpaper.
///
/// This is kept here (not in the dispatch layer) because it is an
/// implementation detail of the wallpaper operation, not a generic concern.
pub fn apply(path: std::path::PathBuf, mode_str: &str) {
    let mode = str_to_mode(mode_str);
    std::thread::spawn(move || {
        let _ = wallpaper::set_mode(mode);
        if let Err(e) = wallpaper::set_from_path(path.to_str().unwrap_or_default()) {
            log::error!("Failed to set wallpaper: {e}");
        }
    });
}

fn str_to_mode(s: &str) -> wallpaper::Mode {
    match s {
        "Center" => wallpaper::Mode::Center,
        "Crop" => wallpaper::Mode::Crop,
        "Fit" => wallpaper::Mode::Fit,
        "Span" => wallpaper::Mode::Span,
        "Stretch" => wallpaper::Mode::Stretch,
        "Tile" => wallpaper::Mode::Tile,
        _ => wallpaper::Mode::Crop,
    }
}
