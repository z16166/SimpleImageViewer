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

use egui::{Align2, Color32, FontId, Vec2, Rect, CornerRadius, Stroke, RichText, StrokeKind};
use std::time::Instant;
use crate::theme::ThemePalette;
use rust_i18n::t;

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
}

pub struct OsdRenderer {
    cached_hud: Option<String>,
    last_state: Option<OsdState>,
}

impl OsdRenderer {
    pub fn new() -> Self {
        Self {
            cached_hud: None,
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
            let hud = format!(
                "{} / {}    {}    {}%    {}×{}    [{}]",
                state.index + 1,
                state.total,
                file_name,
                state.zoom_pct,
                state.res.0,
                state.res.1,
                state.mode,
            );

            self.cached_hud = Some(hud);
            self.last_state = Some(state.clone());
        }

        if let Some(hud) = &self.cached_hud {
            let hud_pos = screen_rect.left_bottom() + Vec2::new(12.0, -12.0);
            ui.painter().text(
                hud_pos,
                Align2::LEFT_BOTTOM,
                hud,
                FontId::proportional(12.0),
                palette.osd_text,
            );
        }

        // Display persistence error if active
        if let Some((err, _)) = save_error {
            let err_pos = screen_rect.left_bottom() + Vec2::new(12.0, -32.0);
            ui.painter().text(
                err_pos,
                Align2::LEFT_BOTTOM,
                t!("error.settings_save_failed", error = err).to_string(),
                FontId::proportional(13.0),
                Color32::from_rgb(255, 100, 100),
            );
        }
    }

    /// Renders a modern music HUD at the bottom center of the screen.
    pub fn render_music_hud(
        &mut self,
        ui: &mut egui::Ui,
        screen_rect: egui::Rect,
        state: &OsdState,
        palette: &ThemePalette,
    ) -> egui::Rect {
        if state.total_duration_ms == 0 || state.current_track.is_none() {
            return egui::Rect::NOTHING;
        }

        let hud_width = crate::constants::MUSIC_HUD_WIDTH;
        let hud_height = crate::constants::MUSIC_HUD_HEIGHT;
        let hud_pos = screen_rect.center_bottom() + Vec2::new(0.0, crate::constants::MUSIC_HUD_BOTTOM_OFFSET);
        
        let hud_rect = Rect::from_center_size(hud_pos, Vec2::new(hud_width, hud_height));
        
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
                    let short_text = if text.chars().count() > crate::constants::MUSIC_HUD_MAX_CHARS {
                        format!("{}...", text.chars().take(crate::constants::MUSIC_HUD_TRUNCATE_LEN).collect::<String>())
                    } else {
                        text.to_string()
                    };
                    
                    // Force high contrast: use accent2 but ensure it's readable against the dark hud background.
                    // In light themes, accent2 might be dark, so we brighten it or use white.
                    ui.label(RichText::new(format!("♪ {}", short_text))
                        .color(palette.accent2.linear_multiply(crate::constants::MUSIC_HUD_CONTRAST_BOOST).to_opaque()) // Boost contrast for dark HUD
                        .small()
                        .strong());
                }
                
                ui.add_space(2.0);

                // Progress Slider row
                ui.horizontal(|ui| {
                    let mut pos = state.current_pos_ms as f32 / 1000.0;
                    let total = state.total_duration_ms as f32 / 1000.0;
                    
                    let cur_str = format!("{:02}:{:02}", (pos as u32)/60, (pos as u32)%60);
                    let tot_str = format!("{:02}:{:02}", (total as u32)/60, (total as u32)%60);
                    
                    ui.label(RichText::new(cur_str).small().color(palette.text_muted));
                    
                    ui.spacing_mut().slider_width = ui.available_width() - 40.0;
                    let resp = ui.add(
                        egui::Slider::new(&mut pos, 0.0..=total)
                            .show_value(false)
                            .trailing_fill(true)
                    );

                    // Draw CUE Markers on the slider track
                    if state.total_duration_ms > 0 && !state.cue_markers.is_empty() {
                        let painter = ui.painter();
                        let slider_rect = resp.rect;
                        
                        for (idx, &marker_ms) in state.cue_markers.iter().enumerate() {
                            if marker_ms >= state.total_duration_ms { continue; }
                            let ratio = (marker_ms as f32 / state.total_duration_ms as f32).clamp(0.0, 1.0);
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
                        ui.memory_mut(|mem| mem.data.insert_temp(egui::Id::new(crate::constants::ID_PENDING_SEEK), pos));
                    }
                });
            });
        });

        hud_rect
    }

    pub fn render_loading_hint(&self, ui: &egui::Ui, screen_rect: egui::Rect, palette: &ThemePalette) {
        ui.painter().text(
            screen_rect.center() - Vec2::new(0.0, 20.0),
            Align2::CENTER_BOTTOM,
            t!("status.loading").to_string(),
            FontId::proportional(16.0),
            palette.text_muted,
        );
    }
}
