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
use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, RichText, Stroke, StrokeKind, Vec2,
};
use rust_i18n::t;
use std::time::Instant;

/// Match `hint.keyboard` in `rendering/mod.rs` (FontId::proportional(13.0)).
const OSD_KEYBOARD_HINT_FONT_PX: f32 = 13.0;

fn text_width(ui: &egui::Ui, text: &str, font_id: FontId) -> f32 {
    ui.painter()
        .layout_no_wrap(text.to_owned(), font_id, Color32::PLACEHOLDER)
        .size()
        .x
}

fn truncate_to_width(ui: &egui::Ui, text: &str, font_id: FontId, max_width: f32) -> String {
    let ellipsis = "…";
    if max_width <= 0.0 {
        return ellipsis.to_string();
    }
    if text_width(ui, text, font_id.clone()) <= max_width {
        return text.to_string();
    }
    let n = text.chars().count();
    let mut lo = 0usize;
    let mut hi = n;
    while lo < hi {
        let mid = (lo + hi + 1) / 2;
        let prefix: String = text.chars().take(mid).collect();
        let candidate = format!("{prefix}{ellipsis}");
        if text_width(ui, &candidate, font_id.clone()) <= max_width {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    if lo == 0 {
        return ellipsis.to_string();
    }
    format!("{}{ellipsis}", text.chars().take(lo).collect::<String>())
}

/// Parameters that affect the OSD status text.
#[derive(PartialEq, Clone)]
pub struct OsdState {
    pub index: usize,
    pub total: usize,
    pub zoom_pct: u32,
    pub res: (u32, u32),
    pub mode: String,
    pub current_track: Option<String>,
    pub metadata: Option<String>,
    pub current_cue_track: Option<usize>,
    pub current_pos_ms: u64,
    pub total_duration_ms: u64,
    pub cue_markers: Vec<u64>,
    pub hdr_status: Option<String>,
}

pub struct OsdRenderer {
    cached_hud: Option<String>,
    cached_hdr_line: Option<String>,
    last_state: Option<OsdState>,
}

impl OsdRenderer {
    pub fn new() -> Self {
        Self {
            cached_hud: None,
            cached_hdr_line: None,
            last_state: None,
        }
    }

    pub fn invalidate(&mut self) {
        self.last_state = None;
    }

    pub fn render(
        &mut self,
        ui: &mut egui::Ui,
        screen_rect: egui::Rect,
        state: &OsdState,
        file_name: &str,
        palette: &ThemePalette,
        save_error: &Option<(String, Instant)>,
    ) {
        if self.last_state.as_ref() != Some(state) {
            let main = format!(
                "{} / {}    {}    {}%    {}×{}    [{}]",
                state.index + 1,
                state.total,
                file_name,
                state.zoom_pct,
                state.res.0,
                state.res.1,
                state.mode,
            );
            let hdr = state.hdr_status.clone();
            self.cached_hud = Some(main);
            self.cached_hdr_line = hdr;
            self.last_state = Some(state.clone());
        }

        let font = FontId::proportional(crate::constants::OSD_TEXT_SIZE);
        let hint_font = FontId::proportional(OSD_KEYBOARD_HINT_FONT_PX);
        let hint_w = text_width(ui, &t!("hint.keyboard").to_string(), hint_font);
        let max_w = (screen_rect.width()
            - crate::constants::OSD_MARGIN * 2.0
            - hint_w
            - crate::constants::OSD_HINT_GAP)
            .max(64.0);

        if let Some(main) = &self.cached_hud {
            let main_trunc = truncate_to_width(ui, main, font.clone(), max_w);
            let base_pos = screen_rect.left_bottom()
                + Vec2::new(crate::constants::OSD_MARGIN, -crate::constants::OSD_MARGIN);
            ui.painter().text(
                base_pos,
                Align2::LEFT_BOTTOM,
                main_trunc,
                font.clone(),
                palette.osd_text,
            );

            if let Some(hdr) = &self.cached_hdr_line {
                let hdr_line = format!("[{hdr}]");
                let hdr_trunc = truncate_to_width(ui, &hdr_line, font.clone(), max_w);
                let hdr_pos = base_pos
                    + Vec2::new(
                        0.0,
                        -(crate::constants::OSD_TEXT_SIZE + crate::constants::OSD_HDR_LINE_GAP),
                    );
                ui.painter().text(
                    hdr_pos,
                    Align2::LEFT_BOTTOM,
                    hdr_trunc,
                    font,
                    palette.osd_text,
                );
            }
        }

        // Display persistence error if active
        if let Some((err, _)) = save_error {
            let err_offset_y = if self.cached_hdr_line.is_some() {
                crate::constants::OSD_ERROR_OFFSET + crate::constants::OSD_ERROR_EXTRA_WHEN_HDR_LINE
            } else {
                crate::constants::OSD_ERROR_OFFSET
            };
            let err_pos = screen_rect.left_bottom()
                + Vec2::new(crate::constants::OSD_MARGIN, -err_offset_y);
            ui.painter().text(
                err_pos,
                Align2::LEFT_BOTTOM,
                t!("error.settings_save_failed", error = err).to_string(),
                FontId::proportional(crate::constants::OSD_ERROR_TEXT_SIZE),
                Color32::from_rgb(255, 100, 100),
            );
        }
    }

    /// Renders a modern music HUD at the bottom center of the screen.
    pub fn render_music_hud(
        &mut self,
        ui: &mut egui::Ui,
        _screen_rect: egui::Rect,
        state: &OsdState,
        palette: &ThemePalette,
    ) -> egui::Rect {
        if state.total_duration_ms == 0 || state.current_track.is_none() {
            return egui::Rect::NOTHING;
        }

        // Use the rect provided by the parent Area/UI, NOT a hard-coded screen position.
        // This allows the HUD to be repositioned by dragging the Area.
        let hud_rect = ui.max_rect();

        // Premium glassmorphism background
        ui.painter().add(egui::Shape::rect_filled(
            hud_rect,
            CornerRadius::same(8),
            Color32::from_black_alpha(160),
        ));
        ui.painter().rect_stroke(
            hud_rect,
            CornerRadius::same(8),
            Stroke::new(1.0, palette.accent2.linear_multiply(0.3)),
            StrokeKind::Outside,
        );

        let inner_rect = hud_rect.shrink(10.0);
        ui.scope_builder(egui::UiBuilder::new().max_rect(inner_rect), |ui| {
            ui.vertical(|ui| {
                let display_text = state.metadata.as_deref().or(state.current_track.as_deref());
                if let Some(text) = display_text {
                    let short_text = if text.chars().count() > crate::constants::MUSIC_HUD_MAX_CHARS
                    {
                        format!(
                            "{}...",
                            text.chars()
                                .take(crate::constants::MUSIC_HUD_TRUNCATE_LEN)
                                .collect::<String>()
                        )
                    } else {
                        text.to_string()
                    };

                    // Force high contrast: use accent2 but ensure it's readable against the dark hud background.
                    // In light themes, accent2 might be dark, so we brighten it or use white.
                    ui.label(
                        RichText::new(format!("♪ {}", short_text))
                            .color(
                                palette
                                    .accent2
                                    .linear_multiply(crate::constants::MUSIC_HUD_CONTRAST_BOOST)
                                    .to_opaque(),
                            ) // Boost contrast for dark HUD
                            .small()
                            .strong(),
                    );
                }

                ui.add_space(2.0);

                // Progress Slider row
                ui.horizontal(|ui| {
                    let mut pos = state.current_pos_ms as f32 / 1000.0;
                    let total = state.total_duration_ms as f32 / 1000.0;

                    let cur_str = format!("{:02}:{:02}", (pos as u32) / 60, (pos as u32) % 60);
                    let tot_str = format!("{:02}:{:02}", (total as u32) / 60, (total as u32) % 60);

                    ui.label(RichText::new(cur_str).small().color(palette.text_muted));

                    ui.spacing_mut().slider_width =
                        ui.available_width() - crate::constants::SLIDER_WIDTH_LABEL_OFFSET;
                    let resp = ui.add(
                        egui::Slider::new(&mut pos, 0.0..=total)
                            .show_value(false)
                            .trailing_fill(true),
                    );

                    // Draw CUE Markers on the slider track
                    if state.total_duration_ms > 0 && !state.cue_markers.is_empty() {
                        let painter = ui.painter();
                        let slider_rect = resp.rect;

                        for (idx, &marker_ms) in state.cue_markers.iter().enumerate() {
                            if marker_ms >= state.total_duration_ms {
                                continue;
                            }
                            let ratio =
                                (marker_ms as f32 / state.total_duration_ms as f32).clamp(0.0, 1.0);
                            let x = slider_rect.left() + ratio * slider_rect.width();
                            let center = egui::pos2(x, slider_rect.center().y);

                            let is_current = state.current_cue_track == Some(idx);
                            let color = if is_current {
                                palette.accent2
                            } else {
                                palette.text_muted.gamma_multiply(0.6)
                            };
                            let radius = if is_current { 2.5 } else { 1.5 };

                            painter.circle_filled(center, radius, color);
                        }
                    }

                    ui.label(RichText::new(tot_str).small().color(palette.text_muted));

                    if resp.drag_stopped() {
                        ui.memory_mut(|mem| {
                            mem.data
                                .insert_temp(egui::Id::new(crate::constants::ID_PENDING_SEEK), pos)
                        });
                    }
                });
            });
        });

        hud_rect
    }

    pub fn render_loading_hint(
        &self,
        ui: &egui::Ui,
        screen_rect: egui::Rect,
        palette: &ThemePalette,
    ) {
        ui.painter().text(
            screen_rect.center() - Vec2::new(0.0, 20.0),
            Align2::CENTER_BOTTOM,
            t!("status.loading").to_string(),
            FontId::proportional(crate::constants::LOADING_HINT_TEXT_SIZE),
            palette.text_muted,
        );
    }
}
