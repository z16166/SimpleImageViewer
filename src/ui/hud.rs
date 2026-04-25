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
use eframe::egui::{self, Context, Rect, Sense, Vec2};
use std::time::{Duration, Instant};

pub fn draw(app: &mut ImageViewerApp, ctx: &Context) {
    if !app.settings.play_music || !app.settings.show_music_osd {
        return;
    }

    let screen_rect = ctx.input(|i| i.content_rect());

    let mut cur_ms = app.audio.get_pos_ms();
    // Smart seek locking logic for HUD (match target or 30s timeout)
    if let Some(target_ms) = app.music_seeking_target_ms {
        let diff = (cur_ms as i64 - target_ms as i64).abs();
        let timed_out = app
            .music_seek_timeout
            .map_or(false, |t| t.elapsed().as_secs() >= 30);
        if diff < 2000 || timed_out {
            app.music_seeking_target_ms = None;
            app.music_seek_timeout = None;
        } else {
            cur_ms = target_ms;
        }
    }

    let is_active =
        app.music_hud_last_activity.elapsed().as_secs() < crate::constants::MUSIC_HUD_IDLE_SECONDS;

    // Wake-up: mouse proximity to bottom center hotzone
    {
        let hud_width = crate::constants::MUSIC_HUD_WIDTH;
        let hud_pos =
            screen_rect.center_bottom() + Vec2::new(0.0, crate::constants::MUSIC_HUD_BOTTOM_OFFSET);
        let hud_rect = Rect::from_center_size(
            hud_pos,
            Vec2::new(hud_width, crate::constants::MUSIC_HUD_HEIGHT),
        );

        if let Some(ptr) = ctx.input(|i| i.pointer.hover_pos()) {
            let in_hotzone = ptr.y > screen_rect.bottom() - 140.0
                && (ptr.x - screen_rect.center().x).abs() < (hud_width / 2.0);
            if hud_rect.contains(ptr) || in_hotzone {
                app.music_hud_last_activity = Instant::now();
            }
        }
    }

    // Only render when active and audio is loaded
    if !is_active || app.audio.get_duration_ms() == 0 || app.audio.get_current_track().is_none() {
        return;
    }

    let music_state = crate::ui::osd::OsdState {
        index: app.current_index,
        total: app.image_files.len(),
        zoom_pct: 0,
        res: (0, 0),
        mode: String::new(),
        current_track: app.audio.get_current_track(),
        metadata: app.audio.get_metadata(),
        current_cue_track: app.audio.get_current_cue_track(),
        current_pos_ms: cur_ms,
        total_duration_ms: app.audio.get_duration_ms(),
        cue_markers: app.audio.get_cue_markers(),
    };

    // HUD position
    let hud_base_pos = screen_rect.center_bottom()
        + Vec2::new(
            -(crate::constants::MUSIC_HUD_WIDTH / 2.0),
            crate::constants::MUSIC_HUD_BOTTOM_OFFSET - (crate::constants::MUSIC_HUD_HEIGHT / 2.0),
        );
    let hud_pos = hud_base_pos + app.music_hud_drag_offset;

    let _area_resp = egui::Area::new(egui::Id::new("music_hud_foreground"))
        .order(egui::Order::Foreground)
        .fixed_pos(hud_pos)
        .interactable(true)
        .show(ctx, |ui| {
            let (rect, resp) = ui.allocate_exact_size(
                Vec2::new(
                    crate::constants::MUSIC_HUD_WIDTH,
                    crate::constants::MUSIC_HUD_HEIGHT,
                ),
                Sense::click_and_drag(),
            );
            if resp.hovered() {
                app.music_hud_last_activity = Instant::now();
            }
            if resp.dragged() {
                app.music_hud_drag_offset += resp.drag_delta();
                app.music_hud_last_activity = Instant::now();
            }
            if resp.double_clicked() {
                app.music_hud_drag_offset = Vec2::ZERO;
                app.music_hud_last_activity = Instant::now();
            }
            let mut child_ui = ui.new_child(egui::UiBuilder::new().max_rect(rect));
            app.osd.render_music_hud(
                &mut child_ui,
                screen_rect,
                &music_state,
                &app.cached_palette,
            );
        });

    // Handle seek
    if let Some(target_s) = ctx.memory_mut(|mem| {
        mem.data
            .remove_temp::<f32>(egui::Id::new(crate::constants::ID_PENDING_SEEK))
    }) {
        app.audio.seek(Duration::from_secs_f32(target_s));
        app.music_seeking_target_ms = Some((target_s * 1000.0) as u64);
        app.music_seek_timeout = Some(Instant::now());
        app.music_hud_last_activity = Instant::now();
    }
}
