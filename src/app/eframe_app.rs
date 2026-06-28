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
use crate::ui::utils::setup_visuals_with_font_size;
use eframe::egui::{self, Context};
use rust_i18n::t;

use super::types::ImageViewerApp;

impl eframe::App for ImageViewerApp {
    fn on_exit(&mut self) {
        if self.settings.resume_last_image && !self.image_files.is_empty() {
            self.settings.last_viewed_image = Some(self.image_files[self.current_index].clone());
        }
        // Persist the last-known window placement. The HDR backend selection
        // (Rgba16Float vs Bgra8Unorm) is locked at swap-chain creation, so
        // remembering which monitor we closed on is what lets the user keep
        // testing HDR by simply reopening the app.
        if let Some(placement) = self.cached_window_placement {
            self.settings.window_maximized = placement.maximized;
            self.settings.window_outer_position = Some(placement.outer_position);
            self.settings.window_inner_size = Some(placement.inner_size);
            self.settings.window_maximized_screen_center = Some(placement.outer_center);
            if placement.maximized {
                self.settings.window_maximized_inner_size = Some(placement.inner_size);
                let restore_inner = self
                    .cached_restore_placement
                    .map(|p| p.inner_size)
                    .or(self.settings.window_restore_inner_size)
                    .unwrap_or(placement.inner_size);
                if let Some(restore) = self.cached_restore_placement {
                    self.settings.window_restore_outer_position = Some(restore.outer_position);
                    self.settings.window_restore_inner_size = Some(restore.inner_size);
                } else if let Some(pos) = Settings::valid_outer_position(placement.outer_position) {
                    self.settings.window_restore_outer_position = Some(pos);
                    self.settings.window_restore_inner_size = Some(restore_inner);
                } else if let Some(top_left) = Settings::restore_outer_top_left_for_screen_center(
                    placement.outer_center,
                    restore_inner,
                ) {
                    self.settings.window_restore_outer_position = Some(top_left);
                    self.settings.window_restore_inner_size = Some(restore_inner);
                }
            } else {
                self.settings.window_restore_outer_position = Some(placement.outer_position);
                self.settings.window_restore_inner_size = Some(placement.inner_size);
                self.settings.window_maximized_inner_size = None;
            }
        }
        self.persist_directory_tree_layout_to_settings();
        #[cfg(feature = "preload-debug")]
        log::info!(
            "[PreloadDebug][Panel] on_exit save folder={:?} list={:?} embedded={:?}",
            self.settings.directory_tree_folder_panel_width,
            self.settings.directory_tree_image_list_panel_width,
            self.settings.directory_tree_embedded_panel_width
        );
        if let Some(placement) = self.cached_directory_tree_window_placement {
            ImageViewerApp::persist_directory_tree_window_placement_to_settings(
                &mut self.settings,
                placement,
                self.cached_directory_tree_restore_placement,
            );
        }
        // Shut down the async saver thread first: dropping the sender closes the
        // channel, causing the saver's `recv()` loop to exit after finishing any
        // in-progress write. This eliminates the race between the saver and our
        // synchronous save below.
        let (dummy_tx, _) = crossbeam_channel::unbounded::<Settings>();
        let old_tx = std::mem::replace(&mut self.save_tx, dummy_tx);
        drop(old_tx);
        let (dummy_hotkey_tx, _) =
            crossbeam_channel::unbounded::<crate::hotkeys::model::HotkeyConfigFile>();
        let old_hotkey_tx = std::mem::replace(&mut self.hotkeys_save_tx, dummy_hotkey_tx);
        drop(old_hotkey_tx);
        let (dummy_context_menu_tx, _) =
            crossbeam_channel::unbounded::<crate::context_menu::model::ContextMenuConfigFile>();
        let old_context_menu_tx =
            std::mem::replace(&mut self.context_menu_save_tx, dummy_context_menu_tx);
        drop(old_context_menu_tx);

        // Wait for the saver thread to finish any in-progress I/O
        if let Some(handle) = self.saver_handle.take()
            && let Err(e) = handle.join()
        {
            log::error!("[on_exit] Saver thread panicked: {:?}", e);
        }
        if let Some(handle) = self.hotkeys_saver_handle.take()
            && let Err(e) = handle.join()
        {
            log::error!("[on_exit] Hotkeys saver thread panicked: {:?}", e);
        }
        if let Some(handle) = self.context_menu_saver_handle.take()
            && let Err(e) = handle.join()
        {
            log::error!("[on_exit] Context menu saver thread panicked: {:?}", e);
        }
        self.background_threads
            .join_all(crate::app::background_threads::BACKGROUND_THREAD_JOIN_TIMEOUT);
        log::debug!("[on_exit] Background thread join finished");
        self.directory_tree.join_workers();
        self.directory_tree
            .viewpaint_app
            .store(std::ptr::null_mut(), std::sync::atomic::Ordering::Release);

        if let Err(e) = self.settings.save() {
            log::error!("[on_exit] Failed to save settings: {}", e);
        }
        if let Err(e) = crate::hotkeys::io::save_hotkeys_file(&self.hotkeys_runtime.config) {
            log::error!("[on_exit] Failed to save hotkeys: {}", e);
        }
        if let Err(e) =
            crate::context_menu::io::save_context_menu_file(&self.context_menu_runtime.config)
        {
            log::error!("[on_exit] Failed to save context menu: {}", e);
        }

        if let (Some(info), Some(cache)) = (
            self.wgpu_adapter_info.as_ref(),
            self.wgpu_pipeline_cache.as_deref(),
        ) {
            crate::wgpu_pipeline_cache::persist(info, cache);
        }

        // Force-terminate BEFORE eframe tries to tear down GPU resources.
        // This avoids a DLL loader lock deadlock on Windows where:
        //   - rayon worker threads hold the loader lock during TLS cleanup
        //   - WIC's CCodecFactory destructor calls MFShutdown which waits for internal timer threads
        //   - main thread's D3D12 adapter drop calls FreeLibrary which needs the loader lock
        // Settings are already persisted above, so this is safe.
        #[cfg(all(target_os = "windows", not(feature = "legacy_win7")))]
        crate::startup::take_and_join_dx12_cache_validate_thread();

        // Explicitly drop tray icon state so it gets cleaned up from the taskbar before process termination.
        self.tray_state = None;
        crate::app::tray_handlers::clear_menu_ids();
        self.hidden_to_tray = false;
        self.pending_hide_to_tray = false;

        self.loader.prepare_for_process_exit();

        crate::startup::shutdown_logger();
        #[cfg(target_os = "windows")]
        std::process::exit(0);
        #[cfg(unix)]
        crate::startup::force_process_exit(0);
    }

    /// Background logic: scanning, loading, auto-switch, keyboard, timers.
    /// Called before each ui() call (and also when hidden but repaint requested).
    fn logic(&mut self, ctx: &Context, frame: &mut eframe::Frame, pass: eframe::LogicPass) {
        if pass.is_root() {
            self.refresh_preload_memory_plan();
        }
        if pass.is_root() && self.tick_raw_gpu_demosaic_completion(ctx, Some(frame)) {
            ctx.request_repaint();
            self.wake_root_for_logic();
        }
        if pass.is_root() && self.needs_process_loaded_images() {
            self.process_loaded_images(ctx, &mut Some(frame));
        }
        if self.should_run_logic_shared() {
            self.logic_shared(ctx, frame);
        } else if !pass.is_root() {
            return;
        }
        if pass.is_root() {
            self.logic_root_only(ctx, frame, &pass);
        }
    }

    fn post_rendering(
        &mut self,
        ctx: &Context,
        frame: &mut eframe::Frame,
        pass: eframe::LogicPass,
    ) -> bool {
        if pass.is_root() {
            let _ = self.tick_raw_gpu_demosaic_completion(ctx, Some(frame));
        }
        let needs_sync = pass.is_root() && self.raw_gpu_demosaic_needs_sync_present();
        if needs_sync {
            self.wake_root_for_logic();
        }
        needs_sync
    }

    /// Tray close interception must run here, not in [`Self::logic`].
    ///
    /// The eframe fork calls `logic` before `integration.update` applies the frame's
    /// `RawInput`, so `ctx.input().close_requested()` in `logic` is one frame stale.
    /// `raw_input_hook` runs inside `update` with the current input, immediately before
    /// `run_ui`, so `CancelClose` is visible to epi_integration's shutdown guard.
    fn raw_input_hook(&mut self, ctx: &Context, raw_input: &mut egui::RawInput) {
        if raw_input.viewport_id != egui::ViewportId::ROOT {
            return;
        }
        let root_close_requested = raw_input
            .viewports
            .get(&egui::ViewportId::ROOT)
            .is_some_and(|info| info.close_requested());
        self.handle_tray_close_in_raw_input(ctx, root_close_requested);
    }

    /// Draw the UI. In eframe 0.34 this is the required method; `ui` is called
    /// with the root `Ui` for the window's central area.
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        // Theme/visual sync runs here (ROOT ui pass), not in logic(). With the eframe fork,
        // logic() also runs when a deferred child viewport repaints; pixels_per_point and
        // style there are wrong and would corrupt dark-theme widget colors on the main window.
        self.sync_theme_and_visuals(&ctx);
        self.sync_directory_tree_keyboard_focus_with_viewports(&ctx);
        // Draw embedded tree before keyboard so the file list can consume arrow keys first.
        self.draw_embedded_directory_tree_panel(ui);
        // Keyboard must run from ROOT ui(), not logic(): logic() runs before integration.update
        // applies raw_input and also runs on deferred child repaints with the wrong pass input.
        self.handle_keyboard(&ctx);

        self.prepare_directory_tree_file_list_viewport(&ctx);

        // Draw image canvas (fills the remaining central area)
        self.draw_image_canvas_ui(ui, frame);

        if self.is_printing.load(std::sync::atomic::Ordering::Relaxed) {
            egui::Window::new(if cfg!(not(target_os = "windows")) {
                t!("print.title_pdf")
            } else {
                t!("print.title")
            })
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(&ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(t!("print.processing"));
                });
            });

            if let Some(rx) = &self.print_status_rx {
                while let Ok(msg) = rx.try_recv() {
                    if let Some(m) = msg {
                        self.status_message = t!("print.failed", err = m).into();
                    }
                }
            }
        } else if let Some(rx) = self.print_status_rx.take() {
            while let Ok(msg) = rx.try_recv() {
                if let Some(m) = msg {
                    self.status_message = t!("print.failed", err = m).into();
                }
            }
        }

        // Settings panel overlay.
        // Suppressed while a modal dialog is open: the modal dialog's backdrop
        // only dims visually (Order::Background); to achieve true modality we
        // must prevent the settings panel from being rendered (and thus from
        // receiving input) while a dialog is on screen.
        self.open_startup_hotkeys_alert_if_needed();

        let modal_open = self.active_modal.is_some();
        if self.show_settings && !modal_open {
            self.draw_settings_panel(&ctx, frame);
        } else if !self.show_settings {
            self.last_show_settings = false;
        }

        // Detect modal transitions: None → Some means a new dialog just opened.
        // Incrementing modal_generation makes the egui::Window Id unique for this
        // opening — egui has no position memory from previous openings, so the
        // dialog always appears at the calculated center position.
        {
            let id = egui::Id::new(crate::ui::dialogs::modal_state::ID_PREV_HAD_MODAL);
            let had_modal = ctx.data(|d| d.get_temp::<bool>(id).unwrap_or(false));
            let has_modal = self.active_modal.is_some();
            if has_modal && !had_modal {
                self.modal_generation = self.modal_generation.wrapping_add(1);
            }
            ctx.data_mut(|d| d.insert_temp(id, has_modal));
        }

        // Dispatch the single active modal dialog (MovableModal handles the overlay)
        self.dispatch_active_modal(&ctx, frame);

        // ── Music HUD (Foreground Layer) ─────────────────────────────────
        self.draw_music_hud_foreground(&ctx);
    }

    fn take_pending_auxiliary_viewport_repaint(
        &mut self,
        _ctx: &egui::Context,
    ) -> Option<egui::ViewportId> {
        self.take_pending_directory_tree_repaint()
    }
}

impl ImageViewerApp {
    /// System-theme trailing detection and DPI-driven style refresh. Must run from the ROOT
    /// `ui()` pass only (see comment in `ui()`).
    fn sync_theme_and_visuals(&mut self, ctx: &Context) {
        let mut style_changed = false;

        if let Some(new_palette) = self
            .settings
            .theme
            .resolve_if_changed(&mut self.theme_cache)
        {
            self.cached_palette = new_palette;
            style_changed = true;
        }

        let ppp = ctx.pixels_per_point();
        if (ppp - self.cached_pixels_per_point).abs() > 0.001 {
            self.cached_pixels_per_point = ppp;
            style_changed = true;
        }

        if style_changed {
            setup_visuals_with_font_size(
                ctx,
                &self.settings,
                &self.cached_palette,
                self.settings.font_size,
            );
            self.sync_directory_tree_theme_snapshot();
            self.mark_directory_tree_repaint_pending();
            self.request_directory_tree_viewport_repaint(ctx);
        }
    }

    /// Re-apply theme palette, egui visuals/fonts, and repaint auxiliary viewports.
    pub(crate) fn refresh_global_ui_style(&mut self, ctx: &Context) {
        if let Some(new_palette) = self
            .settings
            .theme
            .resolve_if_changed(&mut self.theme_cache)
        {
            self.cached_palette = new_palette;
        }
        setup_visuals_with_font_size(
            ctx,
            &self.settings,
            &self.cached_palette,
            self.settings.font_size,
        );
        self.sync_directory_tree_theme_snapshot();
        ctx.request_repaint();
        self.mark_directory_tree_repaint_pending();
        self.request_directory_tree_viewport_repaint(ctx);
        self.wake_root_for_logic();
    }

    fn build_tray_state(was_maximized: bool) -> Option<super::types::TrayState> {
        let icon_data = crate::startup::icon::load_icon();
        let icon =
            match tray_icon::Icon::from_rgba(icon_data.rgba, icon_data.width, icon_data.height) {
                Ok(icon) => icon,
                Err(e) => {
                    log::error!("Failed to convert tray icon: {:?}", e);
                    return None;
                }
            };

        let show_item = tray_icon::menu::MenuItem::new(&t!("tray.show_window"), true, None);
        let settings_item = tray_icon::menu::MenuItem::new(&t!("tray.settings"), true, None);
        let quit_item = tray_icon::menu::MenuItem::new(&t!("tray.quit"), true, None);
        let show_item_id = show_item.id().clone();
        let settings_item_id = settings_item.id().clone();
        let quit_item_id = quit_item.id().clone();

        let tray_menu = tray_icon::menu::Menu::new();
        let _ = tray_menu.append_items(&[
            &show_item,
            &settings_item,
            &tray_icon::menu::PredefinedMenuItem::separator(),
            &quit_item,
        ]);

        match tray_icon::TrayIconBuilder::new()
            .with_menu(Box::new(tray_menu))
            .with_menu_on_left_click(false)
            .with_tooltip(&t!("app.name"))
            .with_icon(icon)
            .build()
        {
            Ok(t) => {
                crate::app::tray_handlers::set_menu_ids(
                    show_item_id,
                    settings_item_id,
                    quit_item_id,
                );
                Some(super::types::TrayState {
                    _tray_icon: t,
                    was_maximized,
                })
            }
            Err(e) => {
                log::error!("Failed to build tray icon: {:?}", e);
                None
            }
        }
    }

    fn show_main_window_from_tray_viewport(ctx: &Context, was_maximized: bool) {
        crate::ipc::unhide_main_window();
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        if was_maximized {
            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        crate::ipc::force_foreground();
    }

    fn focus_main_window(ctx: &Context) {
        // Win32 foreground first while the tray click is still fresh, then sync egui state.
        crate::ipc::force_foreground();
        Self::focus_and_unminimize_window(ctx);
    }

    pub(crate) fn quit_process_now(&mut self) -> ! {
        <Self as eframe::App>::on_exit(self);
        crate::startup::force_process_exit(0);
    }

    fn ensure_tray_icon(&mut self, was_maximized: bool) -> bool {
        if self.tray_state.is_none()
            && let Some(state) = Self::build_tray_state(was_maximized)
        {
            self.tray_state = Some(state);
        }

        if let Some(state) = &mut self.tray_state {
            state.was_maximized = was_maximized;
            true
        } else {
            false
        }
    }

    fn is_system_shutting_down() -> bool {
        #[cfg(target_os = "windows")]
        {
            use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_SHUTTINGDOWN};
            unsafe { GetSystemMetrics(SM_SHUTTINGDOWN) != 0 }
        }
        #[cfg(not(target_os = "windows"))]
        false
    }

    /// Intercept ROOT close for minimize-to-tray. Called from [`eframe::App::raw_input_hook`].
    pub(crate) fn handle_tray_close_in_raw_input(
        &mut self,
        ctx: &Context,
        root_close_requested: bool,
    ) {
        if root_close_requested
            && !self.explicit_quit
            && self.settings.minimize_to_tray_on_close
            && !Self::is_system_shutting_down()
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if !self.hidden_to_tray && !self.pending_hide_to_tray {
                self.prepare_hide_to_tray(ctx);
            }
        }

        if self.pending_hide_to_tray {
            self.finish_hide_to_tray(ctx);
        }
    }

    pub(crate) fn prepare_hide_to_tray(&mut self, ctx: &Context) {
        self.explicit_quit = false; // Reset explicit quit flag
        let was_maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        if self.ensure_tray_icon(was_maximized) {
            self.pending_hide_to_tray = true;
            ctx.request_repaint();
        }
    }

    pub(crate) fn finish_hide_to_tray(&mut self, ctx: &Context) {
        self.pending_hide_to_tray = false;
        self.hidden_to_tray = true;
        self.hide_detached_directory_tree_viewport_if_active(ctx);
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        crate::ipc::hide_main_window();
    }

    pub(crate) fn minimize_to_tray(&mut self, ctx: &Context) {
        self.prepare_hide_to_tray(ctx);
        if self.pending_hide_to_tray {
            self.finish_hide_to_tray(ctx);
        }
    }

    /// Rebuild the tray icon/menu after locale change. When already minimized to tray,
    /// replaces the tray in place so the user is not left with a hidden window and no icon.
    pub(crate) fn refresh_tray_after_language_change(&mut self, ctx: &Context) {
        let Some(old) = self.tray_state.take() else {
            return;
        };
        let was_maximized = old.was_maximized;
        match Self::build_tray_state(was_maximized) {
            Some(state) => self.tray_state = Some(state),
            None => {
                log::warn!("Failed to rebuild tray after language change; restoring main window");
                crate::app::tray_handlers::clear_menu_ids();
                self.hidden_to_tray = false;
                self.pending_hide_to_tray = false;
                Self::show_main_window_from_tray_viewport(ctx, was_maximized);
            }
        }
    }

    pub(crate) fn show_main_window_from_tray(&mut self, ctx: &Context) {
        self.explicit_quit = false; // Reset explicit quit flag when restoring
        let was_maximized = self
            .tray_state
            .as_ref()
            .map(|state| state.was_maximized)
            .unwrap_or(false);
        if self.hidden_to_tray || self.pending_hide_to_tray {
            self.hidden_to_tray = false;
            self.pending_hide_to_tray = false;
            Self::show_main_window_from_tray_viewport(ctx, was_maximized);
            self.show_detached_directory_tree_viewport_if_active(ctx);
        } else if self.tray_state.is_some() {
            Self::focus_main_window(ctx);
        }
    }

    pub(crate) fn minimize_to_tray_from_hotkey(&mut self, ctx: &Context) {
        if !self.hidden_to_tray {
            self.minimize_to_tray(ctx);
        }
    }
}
