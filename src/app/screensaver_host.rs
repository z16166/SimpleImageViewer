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
        // Keep the logical fullscreen flag for chrome-less layout, but do not
        // re-issue winit Borderless fullscreen: that only covers one monitor
        // and fights the explicit multi-monitor cover path below.
        if self.run_mode.is_screensaver_run() && !self.settings.fullscreen {
            self.settings.fullscreen = true;
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

        #[cfg(target_os = "windows")]
        {
            // Wait until the native HWND exists (window may still be hidden on first frames).
            let hwnd = crate::ipc::current_process_visible_main_window()
                .or_else(crate::ipc::current_process_main_window);
            let Some(hwnd) = hwnd else {
                return;
            };

            let cover = match self.screensaver_settings.display {
                ScreensaverDisplayPolicy::All => {
                    crate::screensaver::windows_register::virtual_screen_rect()
                }
                ScreensaverDisplayPolicy::Primary => {
                    crate::screensaver::windows_register::primary_monitor_rect()
                }
            };
            let Some(cover) = cover else {
                log::warn!(
                    "[screensaver] display policy {:?} could not resolve a cover rect",
                    self.screensaver_settings.display
                );
                // Fall back so we do not retry forever with a broken display query.
                self.screensaver_display_policy_applied = true;
                return;
            };

            // Leave any single-monitor borderless fullscreen before covering the target rect.
            // Otherwise winit keeps the window clipped to one display.
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));

            match crate::screensaver::windows_register::cover_window_to_rect(hwnd, cover) {
                Ok(()) => {
                    self.screensaver_display_policy_applied = true;
                    log::info!(
                        "[screensaver] display policy {:?} covered hwnd={hwnd:#x} rect=({},{}) {}x{}",
                        self.screensaver_settings.display,
                        cover.x,
                        cover.y,
                        cover.width,
                        cover.height
                    );
                    ctx.request_repaint();
                }
                Err(e) => {
                    log::warn!("[screensaver] cover_window_to_rect failed ({e}); retry next frame");
                }
            }
        }

        #[cfg(not(target_os = "windows"))]
        {
            // Non-Windows: keep winit borderless fullscreen (single-monitor) as best-effort.
            self.screensaver_display_policy_applied = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
            if self.screensaver_settings.display == ScreensaverDisplayPolicy::Primary {
                log::info!(
                    "[screensaver] primary display policy requested (non-Windows best-effort fullscreen)"
                );
            } else {
                log::info!(
                    "[screensaver] all-display policy on non-Windows uses OS single-monitor fullscreen"
                );
            }
        }
    }
}
