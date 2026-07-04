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

use super::{AppAction, app_action_from_hotkey_action_id};
use crate::app::ImageViewerApp;
use crate::hotkeys::model::KeyChord;
use eframe::egui::{self, Context, Event, MouseWheelUnit, Rect};

pub(crate) struct WheelHotkeyMatch {
    action: AppAction,
    normalized_delta_y: f32,
}

impl ImageViewerApp {
    /// Mouse wheel for image navigation/zoom. Called from [`super::rendering::draw_image_canvas_ui`]
    /// after the central panel is built so scroll deltas are not dropped by pointer-hover guards
    /// in [`super::keyboard::ImageViewerApp::handle_main_window_input`].
    pub(crate) fn handle_main_window_wheel_input(&mut self, ctx: &Context, canvas_rect: Rect) {
        if self.active_modal.is_some() || self.show_settings {
            return;
        }
        if self.directory_tree_nav_blocks_main_window_wheel(ctx) {
            return;
        }

        let mouse_pos = ctx.input(|i| i.pointer.latest_pos());
        let Some(wheel_match) = self.map_wheel_to_action(ctx) else {
            return;
        };
        self.dispatch_wheel_action(ctx, wheel_match, mouse_pos, canvas_rect);
    }

    pub(crate) fn map_wheel_to_action(&self, ctx: &Context) -> Option<WheelHotkeyMatch> {
        let line_scroll_speed = ctx.options(|o| o.input_options.line_scroll_speed);
        ctx.input(|i| {
            for event in &i.events {
                let Event::MouseWheel {
                    unit,
                    delta,
                    modifiers,
                    ..
                } = event
                else {
                    continue;
                };
                let Some(chord) = KeyChord::from_wheel_input(delta.y, *modifiers) else {
                    continue;
                };
                if let Some(action_id) = self.hotkeys_runtime.map.get(&chord).copied() {
                    let normalized_delta_y = match unit {
                        MouseWheelUnit::Line => delta.y * line_scroll_speed,
                        MouseWheelUnit::Page => delta.y * i.viewport_rect().height(),
                        MouseWheelUnit::Point => delta.y,
                    };
                    return Some(WheelHotkeyMatch {
                        action: app_action_from_hotkey_action_id(action_id),
                        normalized_delta_y,
                    });
                }
            }
            None
        })
    }

    pub(crate) fn dispatch_wheel_action(
        &mut self,
        ctx: &Context,
        wheel_match: WheelHotkeyMatch,
        mouse_pos: Option<egui::Pos2>,
        canvas_rect: Rect,
    ) {
        match wheel_match.action {
            AppAction::Next | AppAction::Prev => {
                let now = ctx.input(|i| i.time);
                if now - self.last_mouse_wheel_nav > 0.2 {
                    match wheel_match.action {
                        AppAction::Next => self.navigate_next(ctx),
                        AppAction::Prev => self.navigate_prev(ctx),
                        _ => unreachable!(),
                    }
                    self.last_mouse_wheel_nav = now;
                }
            }
            AppAction::ZoomIn | AppAction::ZoomOut => {
                let scroll_zoom_speed = ctx.options(|o| o.input_options.scroll_zoom_speed);
                let factor =
                    (scroll_zoom_speed * wheel_match.normalized_delta_y.abs().max(1.0)).exp();
                let factor = if wheel_match.action == AppAction::ZoomOut {
                    1.0 / factor
                } else {
                    factor
                };
                self.zoom_at_mouse(factor, mouse_pos, canvas_rect);
            }
            AppAction::RotateCW | AppAction::RotateCCW => {
                let now = ctx.input(|i| i.time);
                if now - self.last_mouse_wheel_nav > 0.2 {
                    let clockwise = wheel_match.action == AppAction::RotateCW;
                    self.apply_rotation_with_tracking(clockwise, ctx);
                    self.last_mouse_wheel_nav = now;
                }
            }
            action => self.dispatch_action(action, ctx),
        }
    }

    fn zoom_at_mouse(&mut self, factor: f32, mouse_pos: Option<egui::Pos2>, canvas_rect: Rect) {
        if factor == 1.0 {
            return;
        }
        let old_zoom = self.zoom_factor;
        self.set_zoom_factor((self.zoom_factor * factor).clamp(0.05, 20.0));
        let ratio = self.zoom_factor / old_zoom;

        if let Some(mouse) = mouse_pos {
            self.pan_offset = crate::app::rendering::geometry::zoom_pan_offset_for_screen_point(
                mouse,
                canvas_rect,
                ratio,
                self.pan_offset,
            );
        }
        self.invalidate_tile_requests_for_view_change();
    }

    /// Applies ±½ EV using the same rule as the settings exposure slider
    /// (`crate::hdr::monitor::effective_render_output_mode`: native HDR exposes
    /// `hdr_exposure_ev_native`, tone-mapped SDR output exposes `hdr_exposure_ev_sdr`).
    pub(crate) fn adjust_hdr_exposure_by_ev(&mut self, delta_ev: f32, ctx: &Context) {
        let slot = match crate::hdr::monitor::effective_render_output_mode(
            self.hdr_target_format,
            self.effective_hdr_monitor_selection().as_ref(),
        ) {
            crate::hdr::renderer::HdrRenderOutputMode::SdrToneMapped => {
                &mut self.settings.hdr_exposure_ev_sdr
            }
            _ => &mut self.settings.hdr_exposure_ev_native,
        };
        *slot = (*slot + delta_ev).clamp(-8.0, 8.0);
        self.sync_hdr_tone_map_settings();
        self.queue_save();
        ctx.request_repaint();
    }
}
