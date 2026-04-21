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

use eframe::egui::{self, Context, RichText};
use crate::app::ImageViewerApp;
use crate::ui::utils::styled_button;
use rust_i18n::t;

pub fn draw(app: &mut ImageViewerApp, ctx: &Context) {
    if !app.show_wallpaper_dialog {
        return;
    }

    let mut do_close = false;
    let mut do_set = false;

    egui::Window::new(t!("wallpaper.title"))
        .id(egui::Id::new("wallpaper_window"))
        .default_pos(ctx.input(|i| i.content_rect()).center() - egui::vec2(260.0, 160.0))
        .resizable(true)
        .collapsible(false)
        .frame(
            egui::Frame::window(&ctx.global_style())
                .fill(app.cached_palette.panel_bg)
                .shadow(egui::epaint::Shadow::NONE),
        )
        .default_size([520.0, 320.0])
        .show(ctx, |ui| {
            ui.visuals_mut().override_text_color = Some(app.cached_palette.text_normal);
            ui.add_space(8.0);

            if let Some(ref current) = app.current_system_wallpaper {
                ui.label(RichText::new(t!("wallpaper.current")).color(app.cached_palette.text_muted).small());
                egui::ScrollArea::horizontal()
                    .id_salt("curr_wp_scroll")
                    .min_scrolled_height(24.0)
                    .show(ui, |ui| {
                        ui.vertical(|ui| {
                            ui.add_space(2.0);
                            ui.add(egui::Label::new(current).selectable(true).wrap_mode(egui::TextWrapMode::Extend));
                            ui.add_space(4.0);
                        });
                    });
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(8.0);
            }

            let path = app.image_files[app.current_index].to_string_lossy().into_owned();
            ui.label(RichText::new(t!("wallpaper.new_path")).color(app.cached_palette.text_muted).small());
            egui::ScrollArea::horizontal()
                .id_salt("new_wp_scroll")
                .min_scrolled_height(24.0)
                .show(ui, |ui| {
                    ui.vertical(|ui| {
                        ui.add_space(2.0);
                        ui.add(egui::Label::new(&path).selectable(true).wrap_mode(egui::TextWrapMode::Extend));
                        ui.add_space(4.0);
                    });
                });
            
            if let Some((w, h)) = app.current_image_res {
                ui.add_space(4.0);
                ui.label(RichText::new(t!("wallpaper.resolution")).color(app.cached_palette.text_muted).small());
                ui.label(format!("{} × {}", w, h));
            }

            ui.add_space(12.0);
            ui.separator();
            ui.add_space(8.0);
            ui.label(RichText::new(t!("wallpaper.mode")).color(app.cached_palette.accent2).strong());

            ui.vertical(|ui| {
                ui.radio_value(&mut app.selected_wallpaper_mode, "Crop".to_string(), t!("wallpaper.crop").to_string());
                ui.radio_value(&mut app.selected_wallpaper_mode, "Fit".to_string(), t!("wallpaper.fit").to_string());
                ui.radio_value(&mut app.selected_wallpaper_mode, "Stretch".to_string(), t!("wallpaper.stretch").to_string());
                ui.radio_value(&mut app.selected_wallpaper_mode, "Tile".to_string(), t!("wallpaper.tile").to_string());
                ui.radio_value(&mut app.selected_wallpaper_mode, "Center".to_string(), t!("wallpaper.center").to_string());
                ui.radio_value(&mut app.selected_wallpaper_mode, "Span".to_string(), t!("wallpaper.span").to_string());
            });

            ui.add_space(16.0);
            ui.horizontal(|ui| {
                if styled_button(ui, &t!("btn.set_wallpaper").to_string(), &app.cached_palette).clicked() {
                    do_set = true;
                }
                if styled_button(ui, &t!("btn.cancel").to_string(), &app.cached_palette).clicked() {
                    do_close = true;
                }
            });
        });

    if do_set {
        let path = app.image_files[app.current_index].clone();
        let mode_str = app.selected_wallpaper_mode.clone();
        
        let mode = match mode_str.as_str() {
            "Center" => wallpaper::Mode::Center,
            "Crop" => wallpaper::Mode::Crop,
            "Fit" => wallpaper::Mode::Fit,
            "Span" => wallpaper::Mode::Span,
            "Stretch" => wallpaper::Mode::Stretch,
            "Tile" => wallpaper::Mode::Tile,
            _ => wallpaper::Mode::Crop,
        };

        std::thread::spawn(move || {
            let _ = wallpaper::set_mode(mode);
            if let Err(e) = wallpaper::set_from_path(path.to_str().unwrap_or_default()) {
                log::error!("Failed to set wallpaper: {e}");
            }
        });
        
        do_close = true;
    }

    if do_close {
        app.show_wallpaper_dialog = false;
    }
}
