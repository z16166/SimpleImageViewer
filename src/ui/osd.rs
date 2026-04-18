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

use egui::{Align2, Color32, FontId, Vec2};
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
        ui: &egui::Ui,
        screen_rect: egui::Rect,
        state: OsdState,
        file_name: &str,
        palette: &ThemePalette,
        save_error: &Option<(String, Instant)>,
    ) {
        if self.last_state.as_ref() != Some(&state) {
            let mut hud = format!(
                "{} / {}    {}    {}%    {}×{}    [{}]",
                state.index + 1,
                state.total,
                file_name,
                state.zoom_pct,
                state.res.0,
                state.res.1,
                state.mode,
            );

            if let Some(ref track) = state.current_track {
                hud.push_str(&format!("    ♪ {}", track));
            }

            self.cached_hud = Some(hud);
            self.last_state = Some(state);
        }

        if let Some(hud) = &self.cached_hud {
            let hud_pos = screen_rect.left_bottom() + Vec2::new(12.0, -12.0);
            ui.painter().text(
                hud_pos,
                Align2::LEFT_BOTTOM,
                hud,
                FontId::proportional(13.0),
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
