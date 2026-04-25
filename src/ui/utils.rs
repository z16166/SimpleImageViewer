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

use crate::settings::Settings;
use crate::theme::ThemePalette;
use eframe::egui::{self, Align2, Color32, Context, FontId, Rect, Vec2};
use rust_i18n::t;

pub fn setup_visuals(ctx: &Context, settings: &Settings, palette: &ThemePalette) {
    let mut visuals = if palette.is_dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    visuals.window_fill = palette.panel_bg;
    visuals.panel_fill = palette.panel_bg;
    visuals.extreme_bg_color = palette.extreme_bg;
    visuals.faint_bg_color = palette.widget_bg;

    // Non-interactive (scrollbar tracks, separator lines, etc.)
    visuals.widgets.noninteractive.bg_fill = palette.widget_bg;
    visuals.widgets.noninteractive.weak_bg_fill = palette.widget_bg;
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, palette.widget_border);
    visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, palette.text_muted);

    // Inactive: bg_fill ??checkbox/scrollbar idle; weak_bg_fill ??button backgrounds
    visuals.widgets.inactive.bg_fill = if palette.is_dark {
        Color32::from_gray(85)
    } else {
        Color32::from_gray(210) // Slightly darker for better light-mode visibility (idle scrollbar)
    };
    visuals.widgets.inactive.weak_bg_fill = palette.widget_bg;
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, palette.widget_border);
    visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, palette.text_normal);

    // Harden opaque backgrounds for other states to avoid "Performance Mode" transparency glitches
    visuals.widgets.hovered.bg_fill = if palette.is_dark {
        Color32::from_gray(100)
    } else {
        Color32::from_gray(225)
    };
    visuals.widgets.active.bg_fill = if palette.is_dark {
        palette.widget_active
    } else {
        palette.accent
    };

    // Thematic hover background for menus and dropdowns
    if palette.is_dark {
        visuals.widgets.hovered.weak_bg_fill = palette.widget_hover;
        visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, Color32::WHITE);
    } else {
        // Light Mode: Very subtle tint + color the text itself to avoid "muddy" look
        let hover_base_color = palette.accent;
        visuals.widgets.hovered.weak_bg_fill = Color32::from_rgba_unmultiplied(
            hover_base_color.r(),
            hover_base_color.g(),
            hover_base_color.b(),
            20, // Very airy
        );
        visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, palette.accent); // The text turns indigo
    }

    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, palette.widget_border_hover);

    // Active: bg_fill ??scrollbar drag; weak_bg_fill ??button press
    visuals.widgets.active.bg_fill = palette.accent;
    visuals.widgets.active.weak_bg_fill = if palette.is_dark {
        palette.widget_active
    } else {
        Color32::from_rgba_unmultiplied(
            palette.accent.r(),
            palette.accent.g(),
            palette.accent.b(),
            50,
        )
    };
    visuals.widgets.active.bg_stroke = egui::Stroke::new(
        1.0,
        if palette.is_dark {
            Color32::WHITE
        } else {
            palette.accent
        },
    );
    visuals.widgets.active.fg_stroke = egui::Stroke::new(
        1.0,
        if palette.is_dark {
            Color32::WHITE
        } else {
            palette.accent
        },
    );

    // Selection (used in ComboBox current item and SelectableLabel)
    if palette.is_dark {
        // Dark Mode: keep selected states fully opaque and neutral to avoid
        // Windows "best performance" compositing glitches and unexpected blue highlights.
        visuals.selection.bg_fill = Color32::from_gray(78);
        visuals.selection.stroke = egui::Stroke::new(1.0, Color32::from_gray(210));
    } else {
        // Light Mode: Use a delicate outline + soft fill instead of a solid block
        // Increased thickness to 2.0 for better hierarchy as requested
        visuals.selection.bg_fill = Color32::from_rgba_unmultiplied(
            palette.accent2.r(),
            palette.accent2.g(),
            palette.accent2.b(),
            30,
        );
        visuals.selection.stroke = egui::Stroke::new(2.0, palette.accent2);
    }

    ctx.set_visuals(visuals);
    ctx.set_pixels_per_point(ctx.native_pixels_per_point().unwrap_or(1.0));

    let mut style = (*ctx.global_style()).clone();
    style.spacing.item_spacing = Vec2::new(
        crate::constants::UI_ITEM_SPACING_X,
        crate::constants::UI_ITEM_SPACING_Y,
    );
    style.spacing.button_padding = Vec2::new(10.0, 5.0);

    style.visuals.window_corner_radius = egui::CornerRadius::same(6);
    style.visuals.widgets.noninteractive.corner_radius = egui::CornerRadius::same(3);
    style.visuals.widgets.inactive.corner_radius = egui::CornerRadius::same(3);
    style.visuals.widgets.hovered.corner_radius = egui::CornerRadius::same(3);
    style.visuals.widgets.active.corner_radius = egui::CornerRadius::same(3);
    style.spacing.scroll.foreground_color = false;

    let size = settings.font_size;
    for id in style.text_styles.values_mut() {
        id.size = size;
    }
    if let Some(id) = style.text_styles.get_mut(&egui::TextStyle::Heading) {
        id.size = size * 1.25;
    }
    if let Some(id) = style.text_styles.get_mut(&egui::TextStyle::Small) {
        id.size = size * 0.8;
    }

    ctx.set_global_style(style);
}

pub fn setup_fonts(ctx: &Context, settings: &Settings) -> bool {
    let mut fonts = egui::FontDefinitions::default();
    let mut font_loaded = false;
    let mut user_font_failed = false;

    if settings.font_family != "System Default" {
        use font_kit::family_name::FamilyName;
        use font_kit::properties::Properties;
        use font_kit::source::SystemSource;

        let source = SystemSource::new();
        if let Ok(handle) = source.select_best_match(
            &[FamilyName::Title(settings.font_family.clone())],
            &Properties::new(),
        ) {
            if let Ok(data) = handle.load() {
                let bytes = data.copy_font_data().map(|d| d.to_vec());
                if let Some(bytes) = bytes {
                    if is_font_safe(&bytes) {
                        fonts.font_data.insert(
                            "UserFont".to_owned(),
                            std::sync::Arc::new(egui::FontData::from_owned(bytes)),
                        );
                        if let Some(family) =
                            fonts.families.get_mut(&egui::FontFamily::Proportional)
                        {
                            family.insert(0, "UserFont".to_owned());
                        }
                        if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
                            family.insert(0, "UserFont".to_owned());
                        }
                        font_loaded = true;
                    } else {
                        log::warn!("[UI] Skipping unreliable font: {}", settings.font_family);
                        user_font_failed = true;
                    }
                } else {
                    user_font_failed = true;
                }
            } else {
                user_font_failed = true;
            }
        } else {
            user_font_failed = true;
        }
    }

    // CJK Fallback
    #[cfg(target_os = "windows")]
    let win_fonts: String = {
        let root = std::env::var("WINDIR")
            .or_else(|_| std::env::var("SystemRoot"))
            .unwrap_or_else(|_| r"C:\Windows".to_string());
        format!(r"{}\Fonts", root)
    };
    #[cfg(target_os = "windows")]
    let candidates: Vec<String> = vec![
        format!(r"{}\msyh.ttc", win_fonts),
        format!(r"{}\msyhbd.ttc", win_fonts),
        format!(r"{}\simsun.ttc", win_fonts),
    ];
    #[cfg(target_os = "macos")]
    let candidates: Vec<String> = vec![
        "/System/Library/Fonts/PingFang.ttc".to_string(),
        "/Library/Fonts/Arial Unicode.ttf".to_string(),
    ];
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let candidates: Vec<String> = vec![
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc".to_string(),
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc".to_string(),
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc".to_string(),
    ];
    for path in candidates {
        if let Ok(data) = std::fs::read(&path) {
            if is_font_safe(&data) {
                fonts.font_data.insert(
                    "CJK".to_owned(),
                    std::sync::Arc::new(egui::FontData::from_owned(data)),
                );
                fonts
                    .families
                    .entry(egui::FontFamily::Proportional)
                    .or_default()
                    .push("CJK".to_owned());
                fonts
                    .families
                    .entry(egui::FontFamily::Monospace)
                    .or_default()
                    .push("CJK".to_owned());
                font_loaded = true;
                break;
            }
        }
    }

    if font_loaded {
        ctx.set_fonts(fonts);
    }

    !user_font_failed
}

pub fn is_font_safe(data: &[u8]) -> bool {
    ttf_parser::Face::parse(data, 0).is_ok()
}

pub fn get_system_font_families() -> Vec<String> {
    use font_kit::source::SystemSource;
    let source = SystemSource::new();
    let mut families = source.all_families().unwrap_or_default();
    families.sort();
    families.insert(0, "System Default".to_string());
    families
}

pub fn copy_file_to_clipboard(path: &str) {
    use clipboard_rs::{Clipboard, ClipboardContext};
    if let Ok(ctx) = ClipboardContext::new() {
        let _ = ctx.set_files(vec![path.to_string()]);
    }
}

pub fn middle_truncate(s: &str, max_chars: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max_chars {
        return s.to_string();
    }
    let half = (max_chars.saturating_sub(3)) / 2;
    let chars: Vec<char> = s.chars().collect();
    let start: String = chars.iter().take(half).collect();
    let end: String = chars.iter().skip(char_count - half).collect();
    format!("{}...{}", start, end)
}

pub fn styled_button(
    ui: &mut egui::Ui,
    label: impl Into<egui::WidgetText>,
    palette: &ThemePalette,
) -> egui::Response {
    ui.add(styled_button_widget(label, palette))
}

pub fn styled_button_widget<'a>(
    label: impl Into<egui::WidgetText> + 'a,
    palette: &'a ThemePalette,
) -> impl egui::Widget + 'a {
    let label = label.into();
    move |ui: &mut egui::Ui| {
        ui.scope(|ui| {
            let visuals = &mut ui.style_mut().visuals;
            if palette.is_dark {
                visuals.widgets.inactive.weak_bg_fill = palette.widget_bg;
                visuals.widgets.inactive.bg_stroke =
                    egui::Stroke::new(1.0, Color32::from_gray(100));
                visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, Color32::WHITE);

                visuals.widgets.hovered.weak_bg_fill = palette.widget_hover;
                visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.5, Color32::from_gray(180));
                visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, Color32::WHITE);

                ui.add(
                    egui::Button::new(label.color(Color32::WHITE))
                        .corner_radius(egui::CornerRadius::same(3)),
                )
            } else {
                visuals.widgets.inactive.weak_bg_fill = Color32::from_rgba_unmultiplied(
                    palette.accent.r(),
                    palette.accent.g(),
                    palette.accent.b(),
                    10,
                );
                visuals.widgets.inactive.bg_stroke = egui::Stroke::new(0.5, palette.accent);
                visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, palette.accent);

                visuals.widgets.hovered.weak_bg_fill = Color32::from_rgba_unmultiplied(
                    palette.accent.r(),
                    palette.accent.g(),
                    palette.accent.b(),
                    40,
                );
                visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, palette.accent);
                visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, palette.accent);

                ui.add(
                    egui::Button::new(label.color(palette.accent))
                        .corner_radius(egui::CornerRadius::same(3)),
                )
            }
        })
        .inner
    }
}

pub fn path_display_box(
    ui: &mut egui::Ui,
    text: impl Into<egui::WidgetText>,
    is_placeholder: bool,
    width: f32,
    palette: &ThemePalette,
) -> egui::Response {
    let text = text.into();
    let text_color = if is_placeholder {
        palette.text_muted
    } else {
        palette.text_normal
    };
    let frame_resp = egui::Frame::new()
        .fill(palette.widget_bg)
        .inner_margin(egui::Margin::symmetric(6, 4))
        .corner_radius(egui::CornerRadius::same(4))
        .stroke(egui::Stroke::new(1.0, palette.widget_border))
        .show(ui, |ui| {
            ui.set_width(width);
            ui.add(egui::Label::new(text.color(text_color).small()).truncate());
        });
    frame_resp.response
}

pub fn draw_empty_hint(ui: &mut egui::Ui, rect: Rect, palette: &ThemePalette) {
    ui.painter().text(
        rect.center() - Vec2::new(0.0, 12.0),
        Align2::CENTER_CENTER,
        "🖼",
        FontId::proportional(48.0),
        palette.hint_icon,
    );
    ui.painter().text(
        rect.center() + Vec2::new(0.0, 30.0),
        Align2::CENTER_CENTER,
        t!("hint.no_images").to_string(),
        FontId::proportional(16.0),
        palette.hint_text,
    );
}
