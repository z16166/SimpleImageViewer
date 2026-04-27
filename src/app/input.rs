use crate::app::ImageViewerApp;
use crate::ui::dialogs::modal_state::{ActiveModal, ModalResult};
use crate::ui::utils::copy_file_to_clipboard;
use crate::ui::{hud as ui_hud, settings as ui_settings};
use eframe::egui::{self, Context, Key, Vec2};
use rust_i18n::t;
use std::time::{Duration, Instant};

impl ImageViewerApp {
    pub(crate) fn handle_keyboard(&mut self, ctx: &Context) {
        // High-level layer detection
        if self.active_modal.is_some() {
            self.handle_modal_input(ctx);
        } else if self.show_settings {
            self.handle_settings_input(ctx);
        } else {
            self.handle_main_window_input(ctx);
        }
    }

    /// Layer 3: Input handling when a modal dialog is active.
    fn handle_modal_input(&mut self, ctx: &Context) {
        ctx.input(|i| {
            // Escape always dismisses any modal
            if i.key_pressed(Key::Escape) {
                self.active_modal = None;
                return;
            }
        });
    }

    /// Layer 2: Input handling when the non-modal settings panel is open.
    fn handle_settings_input(&mut self, ctx: &Context) {
        ctx.input(|i| {
            // Escape closes settings
            if i.key_pressed(Key::Escape) {
                self.show_settings = false;
            }
        });
    }

    /// Layer 1: Input handling for the main window (normal operation).
    fn handle_main_window_input(&mut self, ctx: &Context) {
        let wants_ptr = ctx.egui_is_using_pointer() || ctx.is_pointer_over_egui();

        // 1. Collect flags and inputs
        let mut action: Option<AppAction> = None;
        let mut scroll_delta = egui::Vec2::ZERO;
        let mut zoom_delta = 1.0_f32;
        let mut is_ctrl_pressed = false;
        let mut is_alt_pressed = false;
        let mut mouse_pos: Option<egui::Pos2> = None;

        ctx.input(|i| {
            scroll_delta = i.smooth_scroll_delta;
            zoom_delta = i.zoom_delta();
            is_ctrl_pressed = i.modifiers.command;
            is_alt_pressed = i.modifiers.alt;
            mouse_pos = i.pointer.latest_pos();

            action = self.map_key_to_action(i);
        });

        // If OSD was toggled via Tab, we also clear focus to prevent egui focus-trapping.
        if action == Some(AppAction::ToggleOSD) {
            ctx.memory_mut(|mem| mem.request_focus(egui::Id::NULL));
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Tab));
        }

        // 2. Dispatch Keyboard Actions
        if let Some(act) = action {
            self.dispatch_action(act, ctx);
        }

        // 3. Dispatch Mouse Actions (Scroll/Zoom)
        if !wants_ptr {
            self.handle_mouse_input(
                ctx,
                scroll_delta,
                zoom_delta,
                is_ctrl_pressed,
                is_alt_pressed,
                mouse_pos,
            );
        }
    }

    /// Future-proofing: Map a key press to a logical application action.
    /// This is where we will eventually plug in user-configurable hotkeys.
    /// Map a key press to a logical application action using a prioritized static lookup table.
    ///
    /// [Design Choice: Flat Sorted Array]
    /// For a small number of hotkeys (~30-50), a linear scan of a pre-sorted array is faster
    /// than a HashMap due to CPU cache locality and zero hashing overhead. The array is sorted
    /// by modifier complexity (more modifiers first) to ensure exact matches take priority
    /// over simple ones (e.g., Ctrl+Left overrides Left).
    fn map_key_to_action(&self, i: &egui::InputState) -> Option<AppAction> {
        let current_mods = get_modifiers_mask(i.modifiers);

        for binding in HOTKEY_MAP {
            if i.key_pressed(binding.key) && current_mods == binding.modifiers {
                return Some(binding.action);
            }
        }

        // Special case for asterisk which comes via Text event in some egui versions
        for ev in &i.events {
            if let egui::Event::Text(text) = ev {
                if text == "*" {
                    return Some(AppAction::ZoomReset);
                }
            }
        }

        None
    }

    fn dispatch_action(&mut self, action: AppAction, ctx: &Context) {
        match action {
            AppAction::Next => self.navigate_next(),
            AppAction::Prev => self.navigate_prev(),
            AppAction::First => self.navigate_first(),
            AppAction::Last => self.navigate_last(),
            AppAction::ZoomIn => {
                self.zoom_factor = (self.zoom_factor * 1.1).min(20.0);
                self.update_loader_generation();
            }
            AppAction::ZoomOut => {
                self.zoom_factor = (self.zoom_factor / 1.1).max(0.05);
                self.update_loader_generation();
            }
            AppAction::ZoomReset => {
                self.zoom_factor = 1.0;
                self.pan_offset = Vec2::ZERO;
                self.update_loader_generation();
            }
            AppAction::ToggleSettings => self.show_settings = !self.show_settings,
            AppAction::ToggleFullscreen => {
                self.settings.fullscreen = !self.settings.fullscreen;
                self.pending_fullscreen = Some(self.settings.fullscreen);
                self.queue_save();
            }
            AppAction::ToggleScaleMode => {
                self.settings.scale_mode = self.settings.scale_mode.toggled();
                self.zoom_factor = 1.0;
                self.pan_offset = Vec2::ZERO;
                self.queue_save();
            }
            AppAction::ToggleOSD => {
                self.settings.show_osd = !self.settings.show_osd;
                self.queue_save();
            }
            AppAction::RotateCCW => self.apply_rotation_with_tracking(false, ctx),
            AppAction::RotateCW => self.apply_rotation_with_tracking(true, ctx),
            AppAction::Delete => self.delete_current_image(false),
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
            #[cfg(not(target_os = "windows"))]
            AppAction::Quit => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            AppAction::ExitFullscreen => {
                if self.settings.fullscreen {
                    self.settings.fullscreen = false;
                    self.pending_fullscreen = Some(false);
                    self.queue_save();
                }
            }
        }
    }

    fn update_loader_generation(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.loader.set_generation(self.generation);
        if let Some(tm) = &mut self.tile_manager {
            tm.generation = self.generation;
            tm.pending_tiles.clear();
        }
        self.loader.flush_tile_queue();
    }

    fn handle_mouse_input(
        &mut self,
        ctx: &Context,
        scroll_delta: Vec2,
        zoom_delta: f32,
        is_ctrl_pressed: bool,
        is_alt_pressed: bool,
        mouse_pos: Option<egui::Pos2>,
    ) {
        if is_alt_pressed && scroll_delta.y.abs() > 0.0 {
            // Rotation with Alt + Mouse Wheel
            let now = ctx.input(|i| i.time);
            if now - self.last_mouse_wheel_nav > 0.2 {
                self.apply_rotation_with_tracking(scroll_delta.y < 0.0, ctx);
                self.last_mouse_wheel_nav = now;
            }
        } else if is_ctrl_pressed {
            // Zoom-to-cursor
            if zoom_delta != 1.0 {
                let old_zoom = self.zoom_factor;
                self.zoom_factor = (self.zoom_factor * zoom_delta).clamp(0.05, 20.0);
                let ratio = self.zoom_factor / old_zoom;

                if let Some(mouse) = mouse_pos {
                    let screen_center = ctx.input(|i| i.content_rect()).center();
                    let d = mouse - screen_center;
                    self.pan_offset = d * (1.0 - ratio) + self.pan_offset * ratio;
                }
                self.update_loader_generation();
            }
        } else if scroll_delta.y.abs() > 0.0 {
            // Navigation with mouse wheel
            let now = ctx.input(|i| i.time);
            if now - self.last_mouse_wheel_nav > 0.2 {
                if scroll_delta.y > 0.0 {
                    self.navigate_prev();
                } else {
                    self.navigate_next();
                }
                self.last_mouse_wheel_nav = now;
            }
        }
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

    /// Dispatch rendering for the currently active modal dialog, and process
    /// any [`ModalResult`] it returns to mutate app state accordingly.
    pub(crate) fn dispatch_active_modal(&mut self, ctx: &egui::Context) {
        // Store the current generation so MovableModal can build a unique Window Id.
        let modal_gen = self.modal_generation;
        ctx.data_mut(|d| {
            d.insert_temp(
                egui::Id::new(crate::ui::dialogs::modal_state::ID_MODAL_GENERATION),
                modal_gen,
            )
        });

        let result = match &mut self.active_modal {
            None => return,
            Some(ActiveModal::Confirm(state)) => {
                crate::ui::dialogs::confirm::show(state, ctx, &self.cached_palette)
            }
            Some(ActiveModal::Goto(state)) => {
                crate::ui::dialogs::goto::show(state, ctx, &self.cached_palette)
            }
            Some(ActiveModal::Wallpaper(state)) => {
                let path = if !self.image_files.is_empty() {
                    self.image_files[self.current_index]
                        .to_string_lossy()
                        .into_owned()
                } else {
                    String::new()
                };
                crate::ui::dialogs::wallpaper::show(
                    state,
                    &path,
                    self.current_image_res,
                    ctx,
                    &self.cached_palette,
                )
            }
            Some(ActiveModal::Exif(state)) => {
                crate::ui::dialogs::exif::show(state, ctx, &self.cached_palette)
            }
            Some(ActiveModal::Xmp(state)) => {
                crate::ui::dialogs::xmp::show(state, ctx, &self.cached_palette)
            }
            #[cfg(target_os = "windows")]
            Some(ActiveModal::FileAssoc(state)) => {
                crate::ui::dialogs::file_assoc::show(state, ctx, &self.cached_palette)
            }
        };

        match result {
            ModalResult::Pending => { /* keep open */ }
            ModalResult::Dismissed => {
                self.active_modal = None;
            }
            ModalResult::Confirmed(action) => {
                self.handle_modal_action(action, ctx);
                self.active_modal = None;
            }
        }
    }

    /// Execute the confirmed action from a modal dialog.
    fn handle_modal_action(
        &mut self,
        action: crate::ui::dialogs::modal_state::ModalAction,
        _ctx: &egui::Context,
    ) {
        use crate::ui::dialogs::confirm::ConfirmTag;
        use crate::ui::dialogs::modal_state::ModalAction;
        match action {
            ModalAction::GotoIndex(idx) => {
                self.navigate_to(idx);
            }
            ModalAction::SetWallpaper(mode_str) => {
                if !self.image_files.is_empty() {
                    let path = self.image_files[self.current_index].clone();
                    crate::ui::dialogs::wallpaper::apply(path, &mode_str);
                }
            }
            ModalAction::ConfirmTagged(tag) => match tag {
                ConfirmTag::EnableRecursiveScan => {
                    self.settings.recursive = true;
                    if let Some(dir) = self.settings.last_image_dir.clone() {
                        self.load_directory(dir);
                    }
                    self.queue_save();
                }
            },
            #[cfg(target_os = "windows")]
            ModalAction::ApplyFileAssoc => {
                if let Some(ActiveModal::FileAssoc(state)) = &self.active_modal {
                    let selected = state.selected_extensions();
                    crate::windows_utils::register_file_associations(&selected);
                }
                rfd::MessageDialog::new()
                    .set_title(t!("win.assoc_done_title").to_string())
                    .set_description(t!("win.assoc_done_msg").to_string())
                    .set_buttons(rfd::MessageButtons::Ok)
                    .set_level(rfd::MessageLevel::Info)
                    .show();
            }
        }
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
            // EXIF loading is fully encapsulated in exif::State::from_path
            self.active_modal = Some(ActiveModal::Exif(
                crate::ui::dialogs::exif::State::from_path(path),
            ));
            self.context_menu_pos = None;
        }

        if ui.button(t!("ctx.view_xmp").to_string()).clicked() {
            // XMP loading is fully encapsulated in xmp::State::from_path
            self.active_modal = Some(ActiveModal::Xmp(crate::ui::dialogs::xmp::State::from_path(
                path,
            )));
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
        if ui.button(t!("ctx.set_wallpaper").to_string()).clicked() {
            let current_wallpaper = wallpaper::get().ok();
            self.active_modal = Some(ActiveModal::Wallpaper(
                crate::ui::dialogs::wallpaper::State::new(current_wallpaper),
            ));
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
    Delete,
    PermanentDelete,
    Print,
    ToggleGoto,
    ToggleAutoSwitch,
    #[cfg(not(target_os = "windows"))]
    Quit,
    ExitFullscreen,
}

struct HotkeyBinding {
    modifiers: u8, // Bitmask: Bit 0=Ctrl/Cmd, 1=Shift, 2=Alt
    key: egui::Key,
    action: AppAction,
}

// Modifier bitmask constants
const M_NONE: u8 = 0;
const M_CTRL: u8 = 1;
const M_SHIFT: u8 = 2;
const M_ALT: u8 = 4;

/// Helper to convert egui's complex Modifiers struct into a simple bitmask.
/// This normalizes "command" and "ctrl" to a single bit for reliable matching.
fn get_modifiers_mask(m: egui::Modifiers) -> u8 {
    let mut mask = 0;
    if m.ctrl || m.command {
        mask |= M_CTRL;
    }
    if m.shift {
        mask |= M_SHIFT;
    }
    if m.alt {
        mask |= M_ALT;
    }
    mask
}

const HOTKEY_MAP: &[HotkeyBinding] = &[
    // --- Group 1: High Priority (Complex Modifiers) ---
    HotkeyBinding {
        modifiers: M_SHIFT,
        key: egui::Key::Delete,
        action: AppAction::PermanentDelete,
    },
    HotkeyBinding {
        modifiers: M_CTRL,
        key: egui::Key::ArrowLeft,
        action: AppAction::RotateCCW,
    },
    HotkeyBinding {
        modifiers: M_CTRL,
        key: egui::Key::ArrowRight,
        action: AppAction::RotateCW,
    },
    HotkeyBinding {
        modifiers: M_CTRL,
        key: egui::Key::P,
        action: AppAction::Print,
    },
    #[cfg(not(target_os = "windows"))]
    HotkeyBinding {
        modifiers: M_CTRL,
        key: egui::Key::Q,
        action: AppAction::Quit,
    },
    // --- Group 2: Simple Navigation / Control ---
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::ArrowRight,
        action: AppAction::Next,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::ArrowDown,
        action: AppAction::Next,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::PageDown,
        action: AppAction::Next,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::ArrowLeft,
        action: AppAction::Prev,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::ArrowUp,
        action: AppAction::Prev,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::PageUp,
        action: AppAction::Prev,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::Home,
        action: AppAction::First,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::End,
        action: AppAction::Last,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::Space,
        action: AppAction::ToggleAutoSwitch,
    },
    // --- Group 3: Functional Keys ---
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::Tab,
        action: AppAction::ToggleOSD,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::F1,
        action: AppAction::ToggleSettings,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::F11,
        action: AppAction::ToggleFullscreen,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::F,
        action: AppAction::ToggleFullscreen,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::Z,
        action: AppAction::ToggleScaleMode,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::G,
        action: AppAction::ToggleGoto,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::Delete,
        action: AppAction::Delete,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::Escape,
        action: AppAction::ExitFullscreen,
    },
    // Zoom
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::Plus,
        action: AppAction::ZoomIn,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::Equals,
        action: AppAction::ZoomIn,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::Minus,
        action: AppAction::ZoomOut,
    },
];
