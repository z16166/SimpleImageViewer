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

//! Screensaver run/preview host behavior (input exit, cursor hide, preview embed).

use std::time::Duration;

use eframe::egui::{self, Context};

use super::types::ImageViewerApp;
#[cfg(target_os = "windows")]
use crate::startup::run_mode::PlatformWindowHandle;
use crate::startup::run_mode::{AppRunMode, SaverPhase};

impl ImageViewerApp {
    /// Per-frame host work for screensaver Run/Preview skins.
    pub(crate) fn tick_screensaver_host(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        // Keep fullscreen enforced for run mode.
        if self.run_mode.is_screensaver_run() && !self.settings.fullscreen {
            self.settings.fullscreen = true;
            self.pending_fullscreen = Some(true);
        }

        self.apply_display_policy_once(ctx);

        // Hide cursor in run mode.
        if self.run_mode.is_screensaver_run() {
            ctx.set_cursor_icon(egui::CursorIcon::None);
        }

        // Pace repaints for power-save.
        if self.screensaver_settings.uses_power_save() {
            let fps = self.screensaver_settings.max_fps.max(5.0);
            let interval = Duration::from_secs_f32(1.0 / fps);
            ctx.request_repaint_after(interval);
        } else {
            ctx.request_repaint_after(Duration::from_millis(33));
        }

        self.try_embed_screensaver_preview_once(ctx);

        if self.should_exit_screensaver_on_input(ctx) {
            log::info!("[screensaver] input detected; exiting host process");
            self.explicit_quit = true;
            self.teardown_tray_ui();
            crate::startup::shutdown_logger();
            crate::startup::force_process_exit(0);
        }
    }

    fn try_embed_screensaver_preview_once(&mut self, ctx: &Context) {
        if self.screensaver_preview_embedded {
            return;
        }
        #[cfg(target_os = "windows")]
        {
            let parent = match self.run_mode {
                AppRunMode::Screensaver {
                    phase: SaverPhase::Preview,
                    parent: Some(PlatformWindowHandle::Win32(parent)),
                    ..
                } => parent,
                _ => return,
            };
            let child = crate::ipc::current_process_visible_main_window()
                .or_else(crate::ipc::current_process_main_window);
            let Some(child) = child else {
                return;
            };
            match crate::screensaver::windows_register::try_embed_preview_window(child, parent) {
                Ok(()) => {
                    self.screensaver_preview_embedded = true;
                    log::info!(
                        "[screensaver] embedded preview hwnd={child:#x} under parent={parent:#x}"
                    );
                    ctx.request_repaint();
                }
                Err(e) => {
                    log::warn!("[screensaver] preview embed failed: {e}");
                    // Avoid retry spam every frame when the parent is invalid.
                    self.screensaver_preview_embedded = true;
                }
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = ctx;
            if matches!(
                self.run_mode,
                AppRunMode::Screensaver {
                    phase: SaverPhase::Preview,
                    ..
                }
            ) {
                self.screensaver_preview_embedded = true;
            }
        }
    }

    fn should_exit_screensaver_on_input(&mut self, ctx: &Context) -> bool {
        if !self.run_mode.is_screensaver_run() {
            return false;
        }
        if !self.screensaver_settings.exit_on_input {
            return false;
        }
        let grace = Duration::from_millis(self.screensaver_settings.input_grace_ms);
        if self.screensaver_started_at.elapsed() < grace {
            // Still seed pointer baseline during grace so the first post-grace delta is small.
            if let Some(pos) = ctx.input(|i| i.pointer.latest_pos()) {
                self.screensaver_last_pointer_pos = Some(pos);
            }
            return false;
        }

        let threshold = self.screensaver_settings.pointer_move_threshold_px;
        ctx.input(|i| {
            if i.pointer.any_pressed() || i.pointer.any_released() {
                return true;
            }
            if !i.raw.events.is_empty() {
                for ev in &i.raw.events {
                    match ev {
                        egui::Event::Key { pressed: true, .. } => return true,
                        egui::Event::PointerButton { pressed: true, .. } => return true,
                        egui::Event::MouseWheel { .. } => return true,
                        egui::Event::Text(_) => return true,
                        _ => {}
                    }
                }
            }
            false
        }) || self.pointer_move_exceeded(ctx, threshold)
    }

    fn pointer_move_exceeded(&mut self, ctx: &Context, threshold: f32) -> bool {
        let Some(pos) = ctx.input(|i| i.pointer.latest_pos()) else {
            return false;
        };
        let exceeded = if let Some(prev) = self.screensaver_last_pointer_pos {
            let d = pos - prev;
            d.length() >= threshold
        } else {
            false
        };
        self.screensaver_last_pointer_pos = Some(pos);
        exceeded
    }

    fn apply_display_policy_once(&mut self, ctx: &Context) {
        use crate::screensaver::ScreensaverDisplayPolicy;
        if !self.run_mode.is_screensaver_run() {
            return;
        }
        if self.screensaver_display_policy_applied {
            return;
        }
        self.screensaver_display_policy_applied = true;
        if self.screensaver_settings.display != ScreensaverDisplayPolicy::Primary {
            return;
        }
        // Best-effort: pin the fullscreen run window to the primary monitor origin.
        // True multi-monitor spanning for All remains OS/window-manager dependent.
        #[cfg(target_os = "windows")]
        if let Some(origin) = crate::screensaver::windows_register::primary_monitor_origin() {
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(origin));
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
            log::info!(
                "[screensaver] primary display policy: outer position -> ({}, {})",
                origin.x,
                origin.y
            );
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = ctx;
            log::info!(
                "[screensaver] primary display policy requested (non-Windows best-effort noop)"
            );
        }
    }
}
