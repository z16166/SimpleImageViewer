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
use eframe::egui::{self, RichText};
use rust_i18n::t;

const ABOUT_ICON_SIZE: f32 = 96.0;
const ABOUT_ICON_BYTES: &[u8] = include_bytes!("../../../assets/icon.png");

pub(super) fn draw_about_tab(app: &mut ImageViewerApp, ui: &mut egui::Ui) {
    crate::ui::utils::center_in_settings_panel(ui, 440.0, |ui| {
        ui.vertical_centered(|ui| {
            draw_about_icon(app, ui);
            ui.add_space(8.0);
            ui.label(
                RichText::new(t!("app.title"))
                    .color(app.cached_palette.accent2)
                    .size(20.0)
                    .strong(),
            );
            ui.label(
                RichText::new(t!("about.version", version = env!("CARGO_PKG_VERSION")))
                    .color(app.cached_palette.text_muted),
            );
            ui.label(RichText::new(t!("about.copyright")).color(app.cached_palette.text_muted));
            ui.hyperlink_to(
                "https://github.com/z16166/SimpleImageViewer/releases",
                "https://github.com/z16166/SimpleImageViewer/releases",
            );
        });
    });
}

fn draw_about_icon(app: &mut ImageViewerApp, ui: &mut egui::Ui) {
    if app.about_icon_texture.is_none() {
        if let Ok(image) = image::load_from_memory(ABOUT_ICON_BYTES) {
            let rgba = image.into_rgba8();
            let size = [rgba.width() as usize, rgba.height() as usize];
            let pixels = rgba.into_raw();
            let color_image = egui::ColorImage::from_rgba_unmultiplied(size, &pixels);
            app.about_icon_texture = Some(ui.ctx().load_texture(
                "settings_about_icon",
                color_image,
                egui::TextureOptions::LINEAR,
            ));
        }
    }

    if let Some(texture) = &app.about_icon_texture {
        ui.image((texture.id(), egui::vec2(ABOUT_ICON_SIZE, ABOUT_ICON_SIZE)));
    } else {
        ui.label(
            RichText::new("🖼")
                .size(ABOUT_ICON_SIZE * 0.5)
                .color(app.cached_palette.accent2),
        );
    }
}
