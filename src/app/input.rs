use std::time::{Duration, Instant};
use eframe::egui::{self, Context, Key, Vec2};
use rust_i18n::t;
use crate::app::ImageViewerApp;
use crate::ui::{settings as ui_settings, hud as ui_hud, dialogs as ui_dialogs};
use crate::ui::utils::copy_file_to_clipboard;
use crate::app::extract_exif;

impl ImageViewerApp {
    pub(crate) fn handle_keyboard(&mut self, ctx: &Context) {
        // Collect flags to avoid borrow issues
        let mut nav_next = false;
        let mut nav_prev = false;
        let mut nav_first = false;
        let mut nav_last = false;
        let mut toggle_settings = false;
        let mut zoom_in = false;
        let mut zoom_out = false;
        let mut zoom_reset = false;
        let mut toggle_fullscreen = false;
        let mut toggle_scale_mode = false;
        let mut scroll_delta = egui::Vec2::ZERO;
        let mut zoom_delta = 1.0_f32;
        let mut is_ctrl_pressed = false;
        let mut is_alt_pressed = false;
        let mut mouse_pos: Option<egui::Pos2> = None;
        let mut toggle_auto_switch = false;
        let mut toggle_goto = false;
        let mut do_refresh = false;
        #[allow(unused_mut)]
        let mut do_quit = false;
        let mut do_delete = false;
        let mut do_permanent_delete = false;
        let mut do_print_full = false;
        let mut rotate_ccw = false;
        let mut rotate_cw = false;

        // Collect all modal flags to prevent deletion when a dialog is active
        // Collect all modal flags to prevent interaction when a dialog is active
        let any_modal_open = self.show_wallpaper_dialog 
            || self.show_goto 
            || self.show_exif_window
            || self.show_xmp_window;

        #[cfg(target_os = "windows")]
        let any_modal_open = any_modal_open || self.show_file_assoc_dialog;

        ctx.input(|i| {
            if i.key_pressed(Key::F5) {
                do_refresh = true;
            }
            if i.key_pressed(Key::Space) {
                toggle_auto_switch = true;
            }
            if i.key_pressed(Key::ArrowRight) || i.key_pressed(Key::ArrowDown) || i.key_pressed(Key::PageDown) {
                nav_next = true;
            }
            if i.key_pressed(Key::ArrowLeft) || i.key_pressed(Key::ArrowUp) || i.key_pressed(Key::PageUp) {
                nav_prev = true;
            }
            if i.key_pressed(Key::Home) {
                nav_first = true;
            }
            if i.key_pressed(Key::End) {
                nav_last = true;
            }
            // F1 is the ONLY key to toggle settings/options.
            if i.key_pressed(Key::F1) {
                toggle_settings = true;
            }
            // Escape: close modals or currently open settings. NEVER opens settings from main view.
            if i.key_pressed(Key::Escape) {
                if any_modal_open || self.show_settings {
                    toggle_settings = true; 
                } else if self.settings.fullscreen {
                    toggle_fullscreen = true;
                }
            }
            // Zoom keyboard: + / -
            if i.key_pressed(Key::Plus) || i.key_pressed(Key::Equals) {
                zoom_in = true;
            }
            if i.key_pressed(Key::Minus) {
                zoom_out = true;
            }
            // '*' reset zoom: catches Shift+8 (main keyboard) AND Numpad*
            for ev in &i.events {
                if let egui::Event::Text(text) = ev {
                    if text == "*" {
                        zoom_reset = true;
                    }
                }
            }
            // Mouse wheel collected here, guarded before application below
            scroll_delta = i.smooth_scroll_delta;
            zoom_delta = i.zoom_delta();
            is_ctrl_pressed = i.modifiers.command;
            is_alt_pressed = i.modifiers.alt;
            mouse_pos = i.pointer.latest_pos();
            // F11 / F — toggle fullscreen
            if i.key_pressed(Key::F11) || i.key_pressed(Key::F) {
                toggle_fullscreen = true;
            }
            // Z — toggle scale mode (Fit ↔ Original)
            if i.key_pressed(Key::Z) {
                toggle_scale_mode = true;
            }
            // G / Ctrl+G — goto image by index
            if i.key_pressed(Key::G) {
                toggle_goto = true;
            }
            // Rotation shortcuts: Ctrl+Left / Ctrl+Right
            if i.modifiers.command {
                if i.key_pressed(Key::ArrowLeft) {
                    rotate_ccw = true;
                    nav_prev = false; // Override navigation
                }
                if i.key_pressed(Key::ArrowRight) {
                    rotate_cw = true;
                    nav_next = false; // Override navigation
                }
            }
            if !any_modal_open {
                if i.modifiers.command && i.key_pressed(Key::P) {
                    do_print_full = true;
                }
            }
            // Delete / Shift+Delete (Main window only)
            if !any_modal_open {
                if i.key_pressed(Key::Delete) {
                    if i.modifiers.shift {
                        do_permanent_delete = true;
                    } else {
                        do_delete = true;
                    }
                }
            }
            // Quit shortcut: Cmd+Q on macOS, Ctrl+Q on Linux.
            // On Windows, Alt+F4 is standard and is handled by the OS — no code needed.
            #[cfg(not(target_os = "windows"))]
            if i.modifiers.command && i.key_pressed(Key::Q) {
                do_quit = true;
            }
        });

        if do_delete { self.delete_current_image(false); }
        if do_permanent_delete { self.delete_current_image(true); }
        if do_print_full { self.print_image(ctx, crate::print::PrintMode::FullImage); }

        if !any_modal_open {
            if do_refresh { self.load_directory(self.settings.last_image_dir.clone().unwrap_or_default()); }
            if nav_next { self.navigate_next(); }
            if nav_prev { self.navigate_prev(); }
            if nav_first { self.navigate_first(); }
            if nav_last { self.navigate_last(); }

            if zoom_in {
                self.zoom_factor = (self.zoom_factor * 1.1).min(20.0);
                self.generation = self.generation.wrapping_add(1);
                self.loader.set_generation(self.generation);
                if let Some(tm) = &mut self.tile_manager { tm.generation = self.generation; tm.pending_tiles.clear(); }
                self.loader.flush_tile_queue();
            }
            if zoom_out {
                self.zoom_factor = (self.zoom_factor / 1.1).max(0.05);
                self.generation = self.generation.wrapping_add(1);
                self.loader.set_generation(self.generation);
                if let Some(tm) = &mut self.tile_manager { tm.generation = self.generation; tm.pending_tiles.clear(); }
                self.loader.flush_tile_queue();
            }
            if zoom_reset {
                self.zoom_factor = 1.0;
                self.pan_offset = Vec2::ZERO;
                self.generation = self.generation.wrapping_add(1);
                self.loader.set_generation(self.generation);
                if let Some(tm) = &mut self.tile_manager { tm.generation = self.generation; tm.pending_tiles.clear(); }
                self.loader.flush_tile_queue();
            }
        }
        if toggle_settings {
            #[cfg(target_os = "windows")]
            if self.show_file_assoc_dialog {
                self.show_file_assoc_dialog = false;
            } else if self.show_exif_window {
                self.show_exif_window = false;
            } else if self.show_xmp_window {
                self.show_xmp_window = false;
            } else if self.show_wallpaper_dialog {
                self.show_wallpaper_dialog = false;
            } else if self.show_goto {
                self.show_goto = false;
            } else {
                self.show_settings = !self.show_settings;
            }
            #[cfg(not(target_os = "windows"))]
            {
                if self.show_exif_window {
                    self.show_exif_window = false;
                } else if self.show_xmp_window {
                    self.show_xmp_window = false;
                } else if self.show_wallpaper_dialog {
                    self.show_wallpaper_dialog = false;
                } else if self.show_goto {
                    self.show_goto = false;
                } else {
                    self.show_settings = !self.show_settings;
                }
            }
        }

        let ui_consuming_scroll = any_modal_open || self.show_settings || ctx.egui_wants_pointer_input();
        if !ui_consuming_scroll {
            if is_alt_pressed && scroll_delta.y.abs() > 0.0 {
                // Rotation with Alt + Mouse Wheel (steps of 90 degrees)
                let now = ctx.input(|i| i.time);
                if now - self.last_mouse_wheel_nav > 0.2 { // Reuse cooldown to prevent spinning
                    if scroll_delta.y > 0.0 {
                        rotate_ccw = true;
                    } else if scroll_delta.y < 0.0 {
                        rotate_cw = true;
                    }
                    self.last_mouse_wheel_nav = now;
                }
            } else if is_ctrl_pressed {
                // Zoom-to-cursor...
                if zoom_delta != 1.0 {
                    let old_zoom = self.zoom_factor;
                    self.zoom_factor = (self.zoom_factor * zoom_delta).clamp(0.05, 20.0);
                    let ratio = self.zoom_factor / old_zoom;

                    if let Some(mouse) = mouse_pos {
                        let screen_center = ctx.input(|i| i.content_rect()).center();
                        let d = mouse - screen_center;
                        // d * (1 - ratio) compensates for the scale change around the cursor
                        self.pan_offset = d * (1.0 - ratio) + self.pan_offset * ratio;
                    }
                    
                    self.generation = self.generation.wrapping_add(1);
                    self.loader.set_generation(self.generation);
                    if let Some(tm) = &mut self.tile_manager { tm.generation = self.generation; tm.pending_tiles.clear(); }
                    self.loader.flush_tile_queue();
                }
            } else if scroll_delta.y.abs() > 0.0 {
                // Navigation with debounce (cooldown) to prevent rapid flipping
                let now = ctx.input(|i| i.time);
                if now - self.last_mouse_wheel_nav > 0.2 { // 200ms cooldown
                    if scroll_delta.y > 0.0 {
                        self.navigate_prev();
                    } else {
                        self.navigate_next();
                    }
                    self.last_mouse_wheel_nav = now;
                }
            }
        }
        if toggle_fullscreen {
            self.settings.fullscreen = !self.settings.fullscreen;
            self.pending_fullscreen = Some(self.settings.fullscreen);
            self.queue_save();
        }
        if toggle_scale_mode {
            self.settings.scale_mode = self.settings.scale_mode.toggled();
            self.zoom_factor = 1.0;
            self.pan_offset = Vec2::ZERO;
            self.queue_save();
        }
        if toggle_auto_switch && !self.show_settings {
            if self.settings.auto_switch {
                self.slideshow_paused = !self.slideshow_paused;
                if !self.slideshow_paused {
                    self.last_switch_time = Instant::now();
                }
            }
            // If auto_switch is OFF, space does nothing — user must enable it via settings.
        }
        if toggle_goto && !self.image_files.is_empty() {
            self.show_goto = !self.show_goto;
            if self.show_goto {
                self.goto_input.clear();
                self.goto_needs_focus = true;
            }
        }
        if do_quit {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        // Apply rotation if requested (by keys OR mouse wheel)
        if rotate_ccw { self.apply_rotation_with_tracking(false, ctx); }
        if rotate_cw { self.apply_rotation_with_tracking(true, ctx); }
    }

    // ------------------------------------------------------------------
    // Auto-switch
    // ------------------------------------------------------------------

    pub(crate) fn check_auto_switch(&mut self) {
        if !self.settings.auto_switch || self.slideshow_paused || self.image_files.is_empty() {
            return;
        }
        let interval = Duration::from_secs_f32(self.settings.auto_switch_interval);
        if self.last_switch_time.elapsed() >= interval {
            let last = self.image_files.len() - 1;
            if !self.settings.loop_playback && self.current_index >= last {
                // Loop disabled: stop auto-switch at the last image
                return;
            }
            self.navigate_next();
        }
    }


    // ------------------------------------------------------------------
    // UI: Settings panel
    // ------------------------------------------------------------------

    pub(crate) fn draw_settings_panel(&mut self, ctx: &egui::Context) {
        ui_settings::draw(self, ctx);
    }

    pub(crate) fn draw_wallpaper_dialog(&mut self, ctx: &egui::Context) {
        ui_dialogs::wallpaper::draw(self, ctx);
    }

    pub(crate) fn draw_goto_dialog(&mut self, ctx: &egui::Context) {
        ui_dialogs::goto::draw(self, ctx);
    }

    #[cfg(target_os = "windows")]
    pub(crate) fn draw_file_assoc_dialog(&mut self, ctx: &egui::Context) {
        ui_dialogs::file_assoc::draw(self, ctx);
    }

    pub(crate) fn draw_music_hud_foreground(&mut self, ctx: &egui::Context) {
        ui_hud::draw(self, ctx);
    }

    // ------------------------------------------------------------------
    // UI: Image canvas
    // ------------------------------------------------------------------

    /// Shared content for the right-click context menu (used by the custom
    /// `egui::Area`-based popup in [`Self::draw_image_canvas_ui`]).
    pub(crate) fn draw_context_menu_items(&mut self, ui: &mut egui::Ui) {
        let path = &self.image_files[self.current_index];
        let path_str = path.to_string_lossy().to_string();

        if ui.button(t!("ctx.copy_path").to_string()).clicked() {
            ui.ctx().copy_text(path_str.clone());
            self.context_menu_pos = None;
        }

        if ui.button(t!("ctx.copy_file").to_string()).clicked() {
            copy_file_to_clipboard(&path_str);
            self.context_menu_pos = None;
        }

        ui.separator();

        if ui.button(t!("ctx.view_exif").to_string()).clicked() {
            self.cached_exif_data = extract_exif(path);
            self.show_exif_window = true;
            self.context_menu_pos = None;
        }

        if ui.button(t!("ctx.view_xmp").to_string()).clicked() {
            self.show_xmp_window = true;
            self.context_menu_pos = None;
        }

        ui.separator();

        if ui.button(t!("ctx.rotate_ccw").to_string()).clicked() {
            self.apply_rotation_with_tracking(false, ui.ctx());
            self.context_menu_pos = None;
        }
        
        if ui.button(t!("ctx.rotate_cw").to_string()).clicked() {
            self.apply_rotation_with_tracking(true, ui.ctx());
            self.context_menu_pos = None;
        }

        ui.separator();
        if ui
            .button(if cfg!(not(target_os = "windows")) {
                t!("ctx.print_pdf_full").to_string()
            } else {
                t!("ctx.print_full").to_string()
            })
            .clicked()
        {
            self.print_image(ui.ctx(), crate::print::PrintMode::FullImage);
            self.context_menu_pos = None;
        }
        if ui
            .button(if cfg!(not(target_os = "windows")) {
                t!("ctx.print_pdf_visible").to_string()
            } else {
                t!("ctx.print_visible").to_string()
            })
            .clicked()
        {
            self.print_image(ui.ctx(), crate::print::PrintMode::VisibleArea);
            self.context_menu_pos = None;
        }

        ui.separator();
        if ui
            .button(t!("ctx.set_wallpaper").to_string())
            .clicked()
        {
            self.show_wallpaper_dialog = true;
            if let Ok(p) = wallpaper::get() {
                self.current_system_wallpaper = Some(p);
            } else {
                self.current_system_wallpaper = Some("Unknown".to_string());
            }
            self.context_menu_pos = None;
        }

        ui.separator();
        let fs_label = if self.settings.fullscreen {
            t!("ctx.fullscreen_exit").to_string()
        } else {
            t!("ctx.fullscreen_enter").to_string()
        };
        if ui.button(fs_label).clicked() {
            self.settings.fullscreen = !self.settings.fullscreen;
            self.pending_fullscreen = Some(self.settings.fullscreen);
            self.queue_save();
            self.context_menu_pos = None;
        }
    }

}
