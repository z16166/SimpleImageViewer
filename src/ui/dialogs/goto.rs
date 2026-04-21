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

use eframe::egui::{self, Align2, Color32, Context, Key, RichText};
use crate::app::ImageViewerApp;
use crate::ui::utils::styled_button;
use rust_i18n::t;

pub fn draw(app: &mut ImageViewerApp, ctx: &Context) {
    if !app.show_goto {
        return;
    }

    let total = app.image_files.len();
    if total == 0 {
        app.show_goto = false;
        return;
    }

    let mut do_close = false;
    let mut do_jump = false;

    egui::Window::new(t!("goto.title"))
        .id(egui::Id::new("goto_window"))
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .resizable(false)
        .collapsible(false)
        .frame(
            egui::Frame::window(&ctx.global_style())
                .fill(app.cached_palette.panel_bg)
                .shadow(egui::epaint::Shadow::NONE),
        )
        .fixed_size([320.0, 120.0])
        .show(ctx, |ui| {
            ui.visuals_mut().override_text_color = Some(Color32::WHITE);
            ui.add_space(6.0);
            ui.label(
                RichText::new(t!("goto.hint", total = total.to_string()))
                    .color(app.cached_palette.text_muted)
                    .small(),
            );
            ui.add_space(6.0);

            let resp = ui.add(
                egui::TextEdit::singleline(&mut app.goto_input)
                    .desired_width(f32::INFINITY)
                    .hint_text(format!("{}", app.current_index + 1)),
            );

            // Auto-focus the text field when the dialog first opens
            if app.goto_needs_focus {
                resp.request_focus();
                app.goto_needs_focus = false;
            }

            // Enter key confirms; Escape closes
            if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                do_jump = true;
            }
            if ui.input(|i| i.key_pressed(Key::Escape)) {
                do_close = true;
            }

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if styled_button(ui, t!("btn.go"), &app.cached_palette).clicked() {
                    do_jump = true;
                }
                if styled_button(ui, &t!("btn.cancel").to_string(), &app.cached_palette).clicked() {
                    do_close = true;
                }
            });
        });

    if do_jump {
        let raw: usize = app.goto_input.trim().parse().unwrap_or(0);
        // Input is 1-based; clamp to valid range
        if raw >= 1 {
            let idx = (raw - 1).min(total - 1);
            app.show_goto = false;
            app.navigate_to(idx);
        }
    }
    if do_close {
        app.show_goto = false;
    }
}
