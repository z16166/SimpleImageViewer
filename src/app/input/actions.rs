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

use super::{AutoSwitchStep, auto_switch_step};
use crate::app::ImageViewerApp;
use crate::constants::KEYBOARD_NAV_MIN_INTERVAL_SECS;
use crate::ui::dialogs::modal_state::ActiveModal;
use eframe::egui::{self, Context, Vec2};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum AppAction {
    Next,
    Prev,
    First,
    Last,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    ToggleSettings,
    ToggleFullscreen,
    ToggleScaleMode,
    ToggleOSD,
    RotateCW,
    RotateCCW,
    HdrExposureUp,
    HdrExposureDown,
    Delete,
    PermanentDelete,
    Print,
    ToggleGoto,
    ToggleAutoSwitch,
    RefreshFileList,
    #[cfg(not(target_os = "windows"))]
    Quit,
    SelectPixelRegion,
    ExitFullscreen,
    CopyTo,
    CutTo,
    ToggleTray,
    PickDirectory,
}

impl ImageViewerApp {
    pub(crate) fn dispatch_action(&mut self, action: AppAction, ctx: &Context) {
        // During a refresh scan the file list is being rebuilt: block all actions
        // that dereference image_files by index to avoid out-of-bounds panics or
        // navigating into stale/incomplete list state.
        if self.refresh_scan_in_progress {
            match action {
                AppAction::Next
                | AppAction::Prev
                | AppAction::First
                | AppAction::Last
                | AppAction::Delete
                | AppAction::PermanentDelete
                | AppAction::Print
                | AppAction::ToggleGoto
                | AppAction::ToggleAutoSwitch
                | AppAction::CopyTo
                | AppAction::CutTo => return,
                _ => {}
            }
        }
        match action {
            AppAction::Next => {
                let now = ctx.input(|i| i.time);
                let allow = match self.last_keyboard_nav {
                    None => true,
                    Some(t) => now - t >= KEYBOARD_NAV_MIN_INTERVAL_SECS,
                };
                if allow {
                    self.last_keyboard_nav = Some(now);
                    self.navigate_next(ctx);
                }
            }
            AppAction::Prev => {
                let now = ctx.input(|i| i.time);
                let allow = match self.last_keyboard_nav {
                    None => true,
                    Some(t) => now - t >= KEYBOARD_NAV_MIN_INTERVAL_SECS,
                };
                if allow {
                    self.last_keyboard_nav = Some(now);
                    self.navigate_prev(ctx);
                }
            }
            AppAction::First => self.navigate_first(ctx),
            AppAction::Last => self.navigate_last(ctx),
            AppAction::ZoomIn => {
                self.set_zoom_factor((self.zoom_factor * 1.1).min(20.0));
                self.invalidate_tile_requests_for_view_change();
            }
            AppAction::ZoomOut => {
                self.set_zoom_factor((self.zoom_factor / 1.1).max(0.05));
                self.invalidate_tile_requests_for_view_change();
            }
            AppAction::ZoomReset => {
                self.set_zoom_factor(1.0);
                self.pan_offset = Vec2::ZERO;
                self.invalidate_tile_requests_for_view_change();
            }
            AppAction::ToggleSettings => self.show_settings = !self.show_settings,
            AppAction::ToggleFullscreen => {
                self.settings.fullscreen = !self.settings.fullscreen;
                self.pending_fullscreen = Some(self.settings.fullscreen);
                self.queue_save();
            }
            AppAction::ToggleScaleMode => {
                self.settings.scale_mode = self.settings.scale_mode.toggled();
                self.set_zoom_factor(1.0);
                self.pan_offset = Vec2::ZERO;
                self.queue_save();
            }
            AppAction::ToggleOSD => {
                self.settings.show_osd = !self.settings.show_osd;
                self.queue_save();
            }
            AppAction::RotateCCW => self.apply_rotation_with_tracking(false, ctx),
            AppAction::RotateCW => self.apply_rotation_with_tracking(true, ctx),
            AppAction::HdrExposureUp => {
                const STEP_EV: f32 = 0.5;
                self.adjust_hdr_exposure_by_ev(STEP_EV, ctx);
            }
            AppAction::HdrExposureDown => {
                const STEP_EV: f32 = 0.5;
                self.adjust_hdr_exposure_by_ev(-STEP_EV, ctx);
            }
            AppAction::Delete => self.request_delete_current_image(false),
            AppAction::PermanentDelete => self.delete_current_image(true),
            AppAction::Print => self.print_image(ctx, crate::print::PrintMode::FullImage),
            AppAction::ToggleGoto => {
                if !self.image_files.is_empty() {
                    self.active_modal =
                        Some(ActiveModal::Goto(crate::ui::dialogs::goto::State::new(
                            self.image_files.len(),
                            self.current_index,
                        )));
                }
            }
            AppAction::ToggleAutoSwitch => {
                if self.settings.auto_switch {
                    self.slideshow_paused = !self.slideshow_paused;
                    if !self.slideshow_paused {
                        self.last_switch_time = Instant::now();
                    }
                }
            }
            AppAction::RefreshFileList => {
                self.start_refresh_file_list();
            }
            #[cfg(not(target_os = "windows"))]
            AppAction::Quit => {
                self.explicit_quit = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            AppAction::SelectPixelRegion => {
                // Handled directly in rendering/mod.rs canvas layer
            }
            AppAction::ExitFullscreen => {
                if self.settings.fullscreen {
                    self.settings.fullscreen = false;
                    self.pending_fullscreen = Some(false);
                    self.queue_save();
                }
            }
            AppAction::CopyTo => {
                if !self.image_files.is_empty() {
                    self.active_modal = Some(ActiveModal::FileCopyCut(
                        crate::ui::dialogs::file_copy_cut::State::new(
                            false,
                            self.settings.last_copy_cut_dir.clone(),
                            self.copy_cut_overwrite_if_exists,
                        ),
                    ));
                }
            }
            AppAction::CutTo => {
                if !self.image_files.is_empty() {
                    self.active_modal = Some(ActiveModal::FileCopyCut(
                        crate::ui::dialogs::file_copy_cut::State::new(
                            true,
                            self.settings.last_copy_cut_dir.clone(),
                            self.copy_cut_overwrite_if_exists,
                        ),
                    ));
                }
            }
            AppAction::ToggleTray => {
                self.minimize_to_tray_from_hotkey(ctx);
            }
            AppAction::PickDirectory => {
                // open_directory_dialog requires &eframe::Frame which is not available here;
                // set a flag that is consumed in logic() where frame is accessible.
                self.pending_open_directory = true;
            }
        }
    }

    // ------------------------------------------------------------------
    // Auto-switch
    // ------------------------------------------------------------------

    pub(crate) fn check_auto_switch(&mut self, ctx: &egui::Context) {
        if self.refresh_scan_in_progress
            || !self.settings.auto_switch
            || self.slideshow_paused
            || self.image_files.is_empty()
        {
            return;
        }
        if self.settings.random_slideshow_order && self.scanning {
            return;
        }
        let interval = Duration::from_secs_f32(self.settings.auto_switch_interval);
        if self.last_switch_time.elapsed() >= interval {
            match auto_switch_step(
                self.image_files.len(),
                self.current_index,
                self.settings.random_slideshow_order,
                self.random_slideshow_order_ready,
            ) {
                AutoSwitchStep::Stop => {
                    // Loop disabled: stop auto-switch at the last image.
                }
                AutoSwitchStep::NavigateTo(idx) => self.navigate_to(idx, ctx),
                AutoSwitchStep::ShuffleToFirst => self.shuffle_slideshow_order_to_first(),
            }
        }
    }
}
