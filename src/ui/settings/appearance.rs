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

//! Appearance settings tab — local view/update split (`AppearanceMsg` + `update()`).

use crate::app::{AppTheme, ImageViewerApp};
use crate::theme::ThemePalette;
use crate::ui::utils::{settings_card, setup_fonts, setup_visuals, stable_selectable_label};
use eframe::egui::{self, Color32, Context, RichText};
use rust_i18n::t;

const SLIDER_VALUE_WIDTH: f32 = 70.0;

#[derive(Debug, Clone)]
enum AppearanceMsg {
    FontSizePreview(f32),
    FontSizeCommit(f32),
    FontFamilyChanged(String),
    LanguageChanged(String),
    ThemeChanged(AppTheme),
}

pub(super) fn draw(app: &mut ImageViewerApp, ui: &mut egui::Ui, egui_ctx: &Context) {
    for msg in view(app, ui) {
        update(app, egui_ctx, msg);
    }
}

fn update(app: &mut ImageViewerApp, egui_ctx: &Context, msg: AppearanceMsg) {
    match msg {
        AppearanceMsg::FontSizePreview(size) => {
            app.temp_font_size = Some(size);
        }
        AppearanceMsg::FontSizeCommit(size) => {
            app.settings.font_size = size;
            app.temp_font_size = None;
            setup_visuals(egui_ctx, &app.settings, &app.cached_palette);
            app.queue_save();
        }
        AppearanceMsg::FontFamilyChanged(family) => {
            let old_family = app.settings.font_family.clone();
            app.settings.font_family = family;
            app.is_font_error = false;
            if !setup_fonts(egui_ctx, &app.settings) {
                app.settings.font_family = old_family;
                app.is_font_error = true;
            } else {
                app.queue_save();
            }
        }
        AppearanceMsg::LanguageChanged(lang) => {
            app.settings.language = lang;
            rust_i18n::set_locale(&app.settings.language);
            egui_ctx.send_viewport_cmd(egui::ViewportCommand::Title(t!("app.title").to_string()));
            app.osd.on_language_changed();
            app.cached_keyboard_hint = rust_i18n::t!("hint.keyboard").to_string();
            app.refresh_tray_after_language_change(egui_ctx);
            app.queue_save();
        }
        AppearanceMsg::ThemeChanged(theme) => {
            app.settings.theme = theme;
            app.queue_save();
        }
    }
}

fn view(app: &ImageViewerApp, ui: &mut egui::Ui) -> Vec<AppearanceMsg> {
    let palette = app.cached_palette.clone();
    let msgs = view_card(app, ui, &palette);
    view_font_error_label(app, ui);
    msgs
}

fn view_font_error_label(app: &ImageViewerApp, ui: &mut egui::Ui) {
    if app.is_font_error {
        ui.label(
            RichText::new(t!("label.font_load_error")).color(Color32::from_rgb(255, 100, 100)),
        );
    }
}

fn view_card(
    app: &ImageViewerApp,
    ui: &mut egui::Ui,
    palette: &ThemePalette,
) -> Vec<AppearanceMsg> {
    let mut msgs = Vec::new();
    settings_card(ui, palette, t!("section.font"), |inner| {
        msgs.extend(view_form_grid(app, inner));
    });
    msgs
}

fn view_form_grid(app: &ImageViewerApp, ui: &mut egui::Ui) -> Vec<AppearanceMsg> {
    let mut msgs = Vec::new();
    let row_h = ui.spacing().interact_size.y;

    egui::Grid::new("appearance_settings_grid")
        .num_columns(2)
        .spacing([8.0, 6.0])
        .show(ui, |ui| {
            super::grid_label(ui, t!("label.interface_size"));
            super::grid_control(ui, row_h, |ui| {
                msgs.extend(view_font_size_slider(app, ui));
            });
            ui.end_row();

            super::grid_label(ui, t!("label.interface_font"));
            super::grid_control(ui, row_h, |ui| {
                ui.push_id("font_selection_area", |ui| {
                    msgs.extend(view_font_family_combo(app, ui));
                });
            });
            ui.end_row();

            super::grid_label(ui, t!("section.language"));
            super::grid_control(ui, row_h, |ui| {
                msgs.extend(view_language_combo(app, ui));
            });
            ui.end_row();

            super::grid_label(ui, t!("section.theme"));
            super::grid_control(ui, row_h, |ui| {
                msgs.extend(view_theme_combo(app, ui));
            });
            ui.end_row();
        });

    msgs
}

fn view_font_size_slider(app: &ImageViewerApp, ui: &mut egui::Ui) -> Vec<AppearanceMsg> {
    let mut out = Vec::new();
    let mut current = app.temp_font_size.unwrap_or(app.settings.font_size);
    let resp = super::add_slider(
        ui,
        SLIDER_VALUE_WIDTH,
        egui::Slider::new(&mut current, 12.0..=32.0).step_by(1.0),
        super::SliderTrackMode::Elastic,
    );
    if resp.dragged() {
        out.push(AppearanceMsg::FontSizePreview(current));
    } else if resp.drag_stopped() || (resp.changed() && !resp.dragged()) {
        out.push(AppearanceMsg::FontSizeCommit(current));
    }
    out
}

fn view_font_family_combo(app: &ImageViewerApp, ui: &mut egui::Ui) -> Vec<AppearanceMsg> {
    let control_w = ui.available_width();
    let selected = app.settings.font_family.clone();
    let selected_label = font_family_label(&selected);
    let mut out = Vec::new();

    egui::ComboBox::from_id_salt("font_family")
        .selected_text(selected_label)
        .width(control_w)
        .show_ui(ui, |ui| {
            for family in &app.font_families {
                let label = font_family_label(family);
                if stable_selectable_label(ui, family == &selected, label).clicked() {
                    out.push(AppearanceMsg::FontFamilyChanged(family.clone()));
                }
            }
        });

    out
}

fn view_language_combo(app: &ImageViewerApp, ui: &mut egui::Ui) -> Vec<AppearanceMsg> {
    let control_w = ui.available_width();
    let selected = app.settings.language.clone();
    let selected_label = language_label(&selected);
    let mut out = Vec::new();

    egui::ComboBox::from_id_salt("language_select")
        .selected_text(selected_label)
        .width(control_w)
        .show_ui(ui, |ui| {
            for (code, label) in language_options() {
                if stable_selectable_label(ui, code == selected.as_str(), &label).clicked() {
                    out.push(AppearanceMsg::LanguageChanged(code.to_string()));
                }
            }
        });

    out
}

fn view_theme_combo(app: &ImageViewerApp, ui: &mut egui::Ui) -> Vec<AppearanceMsg> {
    let control_w = ui.available_width();
    let selected = app.settings.theme;
    let mut out = Vec::new();

    egui::ComboBox::from_id_salt("app_theme_select")
        .selected_text(theme_label(selected))
        .width(control_w)
        .show_ui(ui, |ui| {
            for (theme, label) in theme_options() {
                if stable_selectable_label(ui, selected == theme, &label).clicked() {
                    out.push(AppearanceMsg::ThemeChanged(theme));
                }
            }
        });

    out
}

fn font_family_label(family: &str) -> String {
    if family == "System Default" {
        t!("label.system_default").to_string()
    } else {
        family.to_string()
    }
}

fn language_label(code: &str) -> String {
    match code {
        "en" => t!("lang.en").to_string(),
        "zh-CN" => t!("lang.zh_cn").to_string(),
        "zh-TW" => t!("lang.zh_tw").to_string(),
        "zh-HK" => t!("lang.zh_hk").to_string(),
        other => other.to_string(),
    }
}

fn language_options() -> [(&'static str, String); 4] {
    [
        ("en", t!("lang.en").to_string()),
        ("zh-CN", t!("lang.zh_cn").to_string()),
        ("zh-TW", t!("lang.zh_tw").to_string()),
        ("zh-HK", t!("lang.zh_hk").to_string()),
    ]
}

fn theme_label(theme: AppTheme) -> String {
    match theme {
        AppTheme::Dark => t!("theme.dark").to_string(),
        AppTheme::Light => t!("theme.light").to_string(),
        AppTheme::System => t!("theme.system").to_string(),
    }
}

fn theme_options() -> [(AppTheme, String); 3] {
    [
        (AppTheme::Dark, t!("theme.dark").to_string()),
        (AppTheme::Light, t!("theme.light").to_string()),
        (AppTheme::System, t!("theme.system").to_string()),
    ]
}
