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

use crate::app::ImageViewerApp;
#[cfg(target_os = "windows")]
use crate::ui::utils::styled_button;
use crate::ui::utils::{SettingsCardStyle, settings_card_styled};
#[cfg(target_os = "windows")]
use eframe::egui::RichText;
use eframe::egui::{self, Margin};
use rust_i18n::t;

pub(super) fn draw_system_tab(app: &mut ImageViewerApp, ui: &mut egui::Ui) {
    let pane_w = ui.max_rect().width();
    const SIDE_PAD: f32 = 12.0;
    let card_w = 460.0_f32.min(pane_w - 2.0 * SIDE_PAD).max(60.0);

    ui.allocate_ui_with_layout(
        egui::vec2(pane_w, ui.available_height()),
        egui::Layout::top_down(egui::Align::Center),
        |ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(card_w, 0.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_min_width(card_w);
                    ui.set_max_width(card_w);
                    draw_general_section(app, ui);

                    #[cfg(target_os = "windows")]
                    {
                        ui.add_space(12.0);
                        draw_windows_section(app, ui);
                    }
                },
            );
        },
    );
}

fn draw_general_section(app: &mut ImageViewerApp, ui: &mut egui::Ui) {
    let palette = app.cached_palette.clone();
    settings_card_styled(
        ui,
        &palette,
        t!("section.system_general"),
        SettingsCardStyle {
            inner_margin: Margin {
                left: 10,
                right: 10,
                top: 8,
                bottom: 16,
            },
            pin_content_min_width: true,
        },
        |ui| {
            let mut val = app.settings.minimize_to_tray_on_close;
            if ui
                .checkbox(&mut val, t!("label.minimize_to_tray_on_close"))
                .changed()
            {
                app.settings.minimize_to_tray_on_close = val;
                if !val {
                    if app.hidden_to_tray {
                        app.show_main_window_from_tray(ui.ctx());
                    }
                    app.pending_hide_to_tray = false;
                    app.tray_state = None;
                }
                app.queue_save();
            }
        },
    );
}

#[cfg(target_os = "windows")]
fn draw_windows_section(app: &mut ImageViewerApp, ui: &mut egui::Ui) {
    let palette = app.cached_palette.clone();
    settings_card_styled(
        ui,
        &palette,
        t!("section.system_windows"),
        SettingsCardStyle {
            inner_margin: Margin {
                left: 10,
                right: 10,
                top: 8,
                bottom: 16,
            },
            pin_content_min_width: true,
        },
        |ui| {
            ui.label(RichText::new(t!("win.register_hint")).color(palette.text_muted));
            ui.add_space(8.0);

            let row_w = ui.available_width();
            let button_h = ui.spacing().interact_size.y;
            let row_h = button_h + 10.0;
            let (row_rect, _) =
                ui.allocate_exact_size(egui::vec2(row_w, row_h), egui::Sense::hover());

            let buttons_w_id = egui::Id::new("system_tab_button_group_width");
            let measured_w: f32 = ui.ctx().data(|d| d.get_temp(buttons_w_id).unwrap_or(0.0));
            let group_w = if measured_w > 0.0 {
                measured_w.min(row_w)
            } else {
                row_w.min(320.0)
            };
            let group_x = row_rect.left() + ((row_w - group_w) / 2.0).max(0.0);
            let group_y = row_rect.center().y - button_h / 2.0;
            let group_rect = egui::Rect::from_min_size(
                egui::pos2(group_x, group_y),
                egui::vec2(group_w, button_h),
            );
            let mut group_ui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(group_rect)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
            );

            let r1 = styled_button(&mut group_ui, t!("win.assoc_formats"), &palette);
            if r1.clicked()
                && let Ok(reg) = crate::formats::get_registry().read()
            {
                let formats = reg.formats.clone();
                app.active_modal = Some(crate::ui::dialogs::modal_state::ActiveModal::FileAssoc(
                    crate::ui::dialogs::file_assoc::State::new(formats),
                ));
            }
            let r2 = styled_button(&mut group_ui, t!("win.remove_assoc"), &palette);
            if r2.clicked() {
                app.active_modal = Some(crate::ui::dialogs::modal_state::ActiveModal::Confirm(
                    crate::ui::dialogs::confirm::State::remove_file_assoc(
                        t!("win.confirm_remove_title"),
                        t!("win.confirm_remove_msg"),
                    ),
                ));
            }
            let actual_w = r2.rect.right() - r1.rect.left();
            ui.ctx().data_mut(|d| d.insert_temp(buttons_w_id, actual_w));
        },
    );
}
