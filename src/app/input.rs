use crate::app::ImageViewerApp;
use crate::constants::KEYBOARD_NAV_MIN_INTERVAL_SECS;
use crate::hotkeys::model::{HotkeyActionId, HotkeyLogicalKey, KeyChord};
use crate::ui::dialogs::modal_state::{ActiveModal, ModalResult};
use crate::ui::utils::copy_file_to_clipboard;
use crate::ui::{hud as ui_hud, settings as ui_settings};
use eframe::egui::{self, Context, Event, Key, MouseWheelUnit, Vec2};
use rust_i18n::t;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AutoSwitchStep {
    Stop,
    NavigateTo(usize),
    ShuffleToFirst,
}

struct WheelHotkeyMatch {
    action: AppAction,
    normalized_delta_y: f32,
}

pub(crate) fn auto_switch_step(
    image_count: usize,
    current_index: usize,
    random_order: bool,
    random_order_ready: bool,
) -> AutoSwitchStep {
    if image_count <= 1 {
        return AutoSwitchStep::Stop;
    }
    if random_order && !random_order_ready {
        return AutoSwitchStep::ShuffleToFirst;
    }

    let last = image_count - 1;
    if current_index >= last {
        // Playback always loops; the loop_playback setting has been removed.
        if random_order {
            return AutoSwitchStep::ShuffleToFirst;
        }
    }

    AutoSwitchStep::NavigateTo((current_index + 1) % image_count)
}

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
        let mut action: Option<AppAction> = None;
        let capturing = self.is_hotkey_capture_active();
        ctx.input(|i| {
            if !capturing {
                action = self.map_key_to_action(i);
            }
            // Escape closes settings unless a hotkey capture session is active (allows ESC binding).
            if !capturing && i.key_pressed(Key::Escape) {
                self.show_settings = false;
            }
        });

        if let Some(act) = action {
            if act == AppAction::ToggleSettings {
                self.dispatch_action(act, ctx);
            }
        }
    }

    /// Layer 1: Input handling for the main window (normal operation).
    fn handle_main_window_input(&mut self, ctx: &Context) {
        let mut action: Option<AppAction> = None;

        ctx.input(|i| {
            action = self.map_key_to_action(i);
        });

        // If OSD was toggled via Tab, we also clear focus to prevent egui focus-trapping.
        if action == Some(AppAction::ToggleOSD) {
            ctx.memory_mut(|mem| mem.request_focus(egui::Id::NULL));
            ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Tab));
        }

        if let Some(act) = action {
            self.dispatch_action(act, ctx);
        }
    }

    /// Mouse wheel for image navigation/zoom. Called from [`super::rendering::draw_image_canvas_ui`]
    /// after the central panel is built so scroll deltas are not dropped by pointer-hover guards
    /// in [`Self::handle_main_window_input`].
    pub(crate) fn handle_main_window_wheel_input(&mut self, ctx: &Context) {
        if self.active_modal.is_some() || self.show_settings {
            return;
        }

        let mouse_pos = ctx.input(|i| i.pointer.latest_pos());
        let Some(wheel_match) = self.map_wheel_to_action(ctx) else {
            return;
        };
        self.dispatch_wheel_action(ctx, wheel_match, mouse_pos);
    }

    fn map_key_to_action(&self, i: &egui::InputState) -> Option<AppAction> {
        for ev in &i.events {
            if let egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } = ev
            {
                let chord = KeyChord::from_input_event(*key, *modifiers);
                if let Some(action_id) = self.hotkeys_runtime.map.get(&chord).copied() {
                    return Some(app_action_from_hotkey_action_id(action_id));
                }
            }
        }

        // Some keyboard layouts report zoom keys as text input rather than plain key presses.
        let current_mods = get_modifiers_mask(i.modifiers);
        for ev in &i.events {
            if let egui::Event::Text(text) = ev {
                let logical = text_event_to_hotkey_logical_key(text);
                if let Some(logical) = logical {
                    let chord = KeyChord {
                        modifiers: current_mods,
                        key: logical,
                    };
                    if let Some(action_id) = self.hotkeys_runtime.map.get(&chord).copied() {
                        return Some(app_action_from_hotkey_action_id(action_id));
                    }
                }
            }
        }

        None
    }

    pub(crate) fn map_pointer_button_to_action(&self, ctx: &Context) -> Option<AppAction> {
        ctx.input(|i| {
            for event in &i.events {
                let Event::PointerButton {
                    button,
                    pressed: false,
                    modifiers,
                    ..
                } = event
                else {
                    continue;
                };
                let Some(chord) = KeyChord::from_pointer_button(*button, *modifiers) else {
                    continue;
                };
                if let Some(action_id) = self.hotkeys_runtime.map.get(&chord).copied() {
                    return Some(app_action_from_hotkey_action_id(action_id));
                }
            }
            None
        })
    }

    fn map_wheel_to_action(&self, ctx: &Context) -> Option<WheelHotkeyMatch> {
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

    fn dispatch_wheel_action(
        &mut self,
        ctx: &Context,
        wheel_match: WheelHotkeyMatch,
        mouse_pos: Option<egui::Pos2>,
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
                self.zoom_at_mouse(ctx, factor, mouse_pos);
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

    fn zoom_at_mouse(&mut self, ctx: &Context, factor: f32, mouse_pos: Option<egui::Pos2>) {
        if factor == 1.0 {
            return;
        }
        let old_zoom = self.zoom_factor;
        self.set_zoom_factor((self.zoom_factor * factor).clamp(0.05, 20.0));
        let ratio = self.zoom_factor / old_zoom;

        if let Some(mouse) = mouse_pos {
            let screen_center = ctx.input(|i| i.content_rect()).center();
            let d = mouse - screen_center;
            self.pan_offset = d * (1.0 - ratio) + self.pan_offset * ratio;
        }
        self.invalidate_tile_requests_for_view_change();
    }

    /// Applies ±½ EV using the same rule as the settings exposure slider
    /// (`crate::hdr::monitor::effective_render_output_mode`: native HDR exposes
    /// `hdr_exposure_ev_native`, tone-mapped SDR output exposes `hdr_exposure_ev_sdr`).
    fn adjust_hdr_exposure_by_ev(&mut self, delta_ev: f32, ctx: &Context) {
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
                | AppAction::ToggleAutoSwitch => return,
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

    // ------------------------------------------------------------------
    // UI: Settings panel
    // ------------------------------------------------------------------

    pub(crate) fn draw_settings_panel(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        ui_settings::draw(self, ctx, frame);
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
                // Clear current modal BEFORE executing action, allowing the action
                // to trigger a new modal (e.g. success/info dialogs).
                self.active_modal = None;
                self.handle_modal_action(action, ctx);
            }
        }
    }

    /// Execute the confirmed action from a modal dialog.
    fn handle_modal_action(
        &mut self,
        action: crate::ui::dialogs::modal_state::ModalAction,
        ctx: &egui::Context,
    ) {
        use crate::ui::dialogs::confirm::ConfirmTag;
        use crate::ui::dialogs::modal_state::ModalAction;
        match action {
            ModalAction::GotoIndex(idx) => {
                self.navigate_to(idx, ctx);
            }
            ModalAction::SetWallpaper { mode, target } => {
                if !self.image_files.is_empty() {
                    let path = self.image_files[self.current_index].clone();
                    crate::ui::dialogs::wallpaper::apply(path, &mode, &target);
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
                #[cfg(target_os = "windows")]
                ConfirmTag::RemoveFileAssoc => {
                    crate::windows_utils::unregister_file_associations();
                    // Optional: show a success message? User didn't ask but it's consistent.
                }
                ConfirmTag::InfoOnly => {
                    // Do nothing, the modal is already dismissed.
                }
                ConfirmTag::RemoteRecycleDelete => {
                    self.delete_current_image(false);
                }
            },
            #[cfg(target_os = "windows")]
            ModalAction::ApplyFileAssoc(selected) => {
                // Extensions are now passed as a payload, avoiding dependency on active_modal state.
                let selected_refs: Vec<&str> = selected.iter().map(|s| s.as_str()).collect();
                crate::windows_utils::register_file_associations(&selected_refs);

                // Show custom "Success" info dialog instead of system rfd message.
                self.active_modal = Some(crate::ui::dialogs::modal_state::ActiveModal::Confirm(
                    crate::ui::dialogs::confirm::State::info(
                        t!("win.assoc_done_title"),
                        t!("win.assoc_done_msg"),
                    ),
                ));
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
        let path = self.image_files[self.current_index].clone();
        let mut drew_action = false;
        let mut pending_separator = false;
        let item_count = self.context_menu_runtime.config.items.len();
        for idx in 0..item_count {
            let item = &self.context_menu_runtime.config.items[idx];
            match item.kind {
                crate::context_menu::model::ContextMenuItemKind::Separator => {
                    if drew_action {
                        pending_separator = true;
                    }
                }
                crate::context_menu::model::ContextMenuItemKind::Builtin => {
                    if !item.enabled {
                        continue;
                    }
                    let Some(id) = item.builtin_id.as_deref() else {
                        continue;
                    };
                    let Some(desc) = crate::context_menu::model::builtin_descriptor(id) else {
                        continue;
                    };
                    if pending_separator {
                        ui.separator();
                        pending_separator = false;
                    }
                    let label = if desc.id == "toggle_fullscreen" {
                        if self.settings.fullscreen {
                            t!("ctx.fullscreen_exit").to_string()
                        } else {
                            t!("ctx.fullscreen_enter").to_string()
                        }
                    } else if desc.id == "print_current" && cfg!(not(target_os = "windows")) {
                        t!("ctx.print_pdf_full").to_string()
                    } else {
                        t!(desc.label_key).to_string()
                    };
                    if ui.button(label).clicked() {
                        self.run_builtin_context_menu_action(desc.id, &path, ui);
                    }
                    drew_action = true;
                }
                crate::context_menu::model::ContextMenuItemKind::Custom => {
                    if !item.enabled {
                        continue;
                    }
                    if pending_separator {
                        ui.separator();
                        pending_separator = false;
                    }
                    if ui.button(item.label.as_str()).clicked() {
                        if let Some(command) = item.command.clone() {
                            self.run_custom_context_menu_action(&command, &path);
                        }
                        self.context_menu_pos = None;
                    }
                    drew_action = true;
                }
            }
        }
    }

    fn run_builtin_context_menu_action(
        &mut self,
        id: &str,
        path: &std::path::Path,
        ui: &mut egui::Ui,
    ) {
        let path_str = path.to_string_lossy().to_string();
        match id {
            "copy_path" => ui.ctx().copy_text(path_str),
            "copy_file" => copy_file_to_clipboard(&path_str),
            "view_exif" => {
                self.active_modal = Some(ActiveModal::Exif(
                    crate::ui::dialogs::exif::State::new_loading(path.to_path_buf()),
                ));
                if let Err(e) = self
                    .lightweight_file_op_tx
                    .send(crate::app::LightweightFileOpJob::Exif(path.to_path_buf()))
                {
                    log::warn!("EXIF context-menu job queue send failed: {}", e);
                }
            }
            "view_xmp" => {
                self.active_modal = Some(ActiveModal::Xmp(
                    crate::ui::dialogs::xmp::State::new_loading(path.to_path_buf()),
                ));
                if let Err(e) = self
                    .lightweight_file_op_tx
                    .send(crate::app::LightweightFileOpJob::Xmp(path.to_path_buf()))
                {
                    log::warn!("XMP context-menu job queue send failed: {}", e);
                }
            }
            "zoom_in" => self.dispatch_action(AppAction::ZoomIn, ui.ctx()),
            "zoom_out" => self.dispatch_action(AppAction::ZoomOut, ui.ctx()),
            "zoom_reset" => self.dispatch_action(AppAction::ZoomReset, ui.ctx()),
            "toggle_scale_mode" => self.dispatch_action(AppAction::ToggleScaleMode, ui.ctx()),
            "toggle_osd" => self.dispatch_action(AppAction::ToggleOSD, ui.ctx()),
            "rotate_ccw" => self.apply_rotation_with_tracking(false, ui.ctx()),
            "rotate_cw" => self.apply_rotation_with_tracking(true, ui.ctx()),
            "hdr_exposure_up" => self.dispatch_action(AppAction::HdrExposureUp, ui.ctx()),
            "hdr_exposure_down" => self.dispatch_action(AppAction::HdrExposureDown, ui.ctx()),
            "delete_to_recycle_bin" => self.dispatch_action(AppAction::Delete, ui.ctx()),
            "permanent_delete" => self.dispatch_action(AppAction::PermanentDelete, ui.ctx()),
            "print_current" => self.print_image(ui.ctx(), crate::print::PrintMode::FullImage),
            "print_visible" => self.print_image(ui.ctx(), crate::print::PrintMode::VisibleArea),
            "set_wallpaper" => {
                self.active_modal = Some(ActiveModal::Wallpaper(
                    crate::ui::dialogs::wallpaper::State::new_loading(),
                ));
                if let Err(e) = self
                    .lightweight_file_op_tx
                    .send(crate::app::LightweightFileOpJob::Wallpaper)
                {
                    log::warn!("Wallpaper context-menu job queue send failed: {}", e);
                }
            }
            "toggle_fullscreen" => self.dispatch_action(AppAction::ToggleFullscreen, ui.ctx()),
            "exit_fullscreen" => self.dispatch_action(AppAction::ExitFullscreen, ui.ctx()),
            _ => log::warn!("Unknown context menu builtin action: {}", id),
        }
        self.context_menu_pos = None;
    }

    fn run_custom_context_menu_action(
        &mut self,
        command: &crate::context_menu::model::ContextMenuCommand,
        path: &std::path::Path,
    ) {
        let result = match command {
            crate::context_menu::model::ContextMenuCommand::Executable { path: exe } => {
                std::process::Command::new(exe)
                    .arg(path)
                    .spawn()
                    .map(|_| ())
            }
            crate::context_menu::model::ContextMenuCommand::CommandLine { .. } => {
                let Some(line) = command.command_line_for_image(path) else {
                    return;
                };
                #[cfg(target_os = "windows")]
                {
                    std::process::Command::new("cmd")
                        .arg("/C")
                        .arg(line)
                        .spawn()
                        .map(|_| ())
                }
                #[cfg(not(target_os = "windows"))]
                {
                    std::process::Command::new("sh")
                        .arg("-c")
                        .arg(line)
                        .spawn()
                        .map(|_| ())
                }
            }
        };
        if let Err(e) = result {
            log::warn!("Custom context-menu command failed: {}", e);
            self.active_modal = Some(ActiveModal::Confirm(
                crate::ui::dialogs::confirm::State::info(
                    t!("context_menu.command_failed_title"),
                    t!("context_menu.command_failed_msg", error = e.to_string()),
                ),
            ));
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
    ExitFullscreen,
}

#[cfg(test)]
struct HotkeyBinding {
    modifiers: u8, // Bitmask: Bit 0=Ctrl/Cmd, 1=Shift, 2=Alt
    key: egui::Key,
}

// Modifier bitmask constants
#[cfg(test)]
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

#[cfg(test)]
const HOTKEY_MAP: &[HotkeyBinding] = &[
    // --- Group 1: High Priority (Complex Modifiers) ---
    HotkeyBinding {
        modifiers: M_SHIFT,
        key: egui::Key::Delete,
    },
    HotkeyBinding {
        modifiers: M_CTRL,
        key: egui::Key::ArrowLeft,
    },
    HotkeyBinding {
        modifiers: M_CTRL,
        key: egui::Key::ArrowRight,
    },
    HotkeyBinding {
        modifiers: M_CTRL,
        key: egui::Key::ArrowUp,
    },
    HotkeyBinding {
        modifiers: M_CTRL,
        key: egui::Key::ArrowDown,
    },
    HotkeyBinding {
        modifiers: M_CTRL,
        key: egui::Key::P,
    },
    #[cfg(not(target_os = "windows"))]
    HotkeyBinding {
        modifiers: M_CTRL,
        key: egui::Key::Q,
    },
    // --- Group 2: Simple Navigation / Control ---
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::ArrowRight,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::ArrowDown,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::PageDown,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::ArrowLeft,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::ArrowUp,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::PageUp,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::Home,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::End,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::Space,
    },
    // --- Group 3: Functional Keys ---
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::Tab,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::F1,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::F11,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::F,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::Z,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::G,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::Delete,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::Escape,
    },
    // Zoom
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::Plus,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::Equals,
    },
    HotkeyBinding {
        modifiers: M_NONE,
        key: egui::Key::Minus,
    },
];

fn app_action_from_hotkey_action_id(action: HotkeyActionId) -> AppAction {
    match action {
        HotkeyActionId::NextImage => AppAction::Next,
        HotkeyActionId::PrevImage => AppAction::Prev,
        HotkeyActionId::FirstImage => AppAction::First,
        HotkeyActionId::LastImage => AppAction::Last,
        HotkeyActionId::ZoomIn => AppAction::ZoomIn,
        HotkeyActionId::ZoomOut => AppAction::ZoomOut,
        HotkeyActionId::ZoomReset => AppAction::ZoomReset,
        HotkeyActionId::ToggleSettings => AppAction::ToggleSettings,
        HotkeyActionId::ToggleFullscreen => AppAction::ToggleFullscreen,
        HotkeyActionId::ToggleScaleMode => AppAction::ToggleScaleMode,
        HotkeyActionId::ToggleOsd => AppAction::ToggleOSD,
        HotkeyActionId::RotateCw => AppAction::RotateCW,
        HotkeyActionId::RotateCcw => AppAction::RotateCCW,
        HotkeyActionId::HdrExposureUp => AppAction::HdrExposureUp,
        HotkeyActionId::HdrExposureDown => AppAction::HdrExposureDown,
        HotkeyActionId::DeleteToRecycleBin => AppAction::Delete,
        HotkeyActionId::PermanentDelete => AppAction::PermanentDelete,
        HotkeyActionId::PrintCurrent => AppAction::Print,
        HotkeyActionId::ToggleGoto => AppAction::ToggleGoto,
        HotkeyActionId::ToggleSlideshow => AppAction::ToggleAutoSwitch,
        HotkeyActionId::RefreshFileList => AppAction::RefreshFileList,
        #[cfg(not(target_os = "windows"))]
        HotkeyActionId::Quit => AppAction::Quit,
        HotkeyActionId::ExitFullscreen => AppAction::ExitFullscreen,
    }
}

fn text_event_to_hotkey_logical_key(text: &str) -> Option<HotkeyLogicalKey> {
    crate::hotkeys::model::parse_logical_key_name(text)
}

#[cfg(test)]
mod tests {
    use super::{
        AutoSwitchStep, HOTKEY_MAP, app_action_from_hotkey_action_id, auto_switch_step,
        text_event_to_hotkey_logical_key,
    };
    use crate::hotkeys::model::{HotkeyLogicalKey, keychord_from_legacy_binding};
    use eframe::egui::Key;
    use std::collections::HashSet;

    #[test]
    fn auto_switch_uses_existing_order_when_random_is_disabled() {
        assert_eq!(
            auto_switch_step(5, 1, false, false),
            AutoSwitchStep::NavigateTo(2)
        );
    }

    #[test]
    fn auto_switch_stops_when_there_is_only_one_image() {
        assert_eq!(auto_switch_step(1, 0, true, false), AutoSwitchStep::Stop);
    }

    #[test]
    fn random_auto_switch_starts_by_shuffling_to_first_image() {
        assert_eq!(
            auto_switch_step(5, 1, true, false),
            AutoSwitchStep::ShuffleToFirst
        );
    }

    #[test]
    fn random_auto_switch_reshuffles_before_next_loop() {
        assert_eq!(
            auto_switch_step(5, 4, true, true),
            AutoSwitchStep::ShuffleToFirst
        );
    }

    #[test]
    fn auto_switch_loops_at_end_when_random_is_disabled() {
        assert_eq!(
            auto_switch_step(5, 4, false, true),
            AutoSwitchStep::NavigateTo(0)
        );
    }

    #[test]
    fn legacy_hotkey_map_has_no_conflicts() {
        let mut seen = HashSet::new();
        for binding in HOTKEY_MAP {
            let chord = keychord_from_legacy_binding(binding.modifiers, binding.key);
            assert!(
                seen.insert(chord),
                "duplicate legacy chord: {:?}",
                chord.display_string()
            );
        }
    }

    #[test]
    fn all_runtime_actions_map_to_app_actions() {
        for desc in crate::hotkeys::model::all_action_descriptors() {
            let _app_action = app_action_from_hotkey_action_id(desc.id);
        }
    }

    #[test]
    fn text_event_mapping_reuses_hotkey_key_parser() {
        assert_eq!(
            text_event_to_hotkey_logical_key("+"),
            Some(HotkeyLogicalKey::Text("+"))
        );
        assert_eq!(
            text_event_to_hotkey_logical_key("1"),
            Some(HotkeyLogicalKey::Egui(Key::Num1))
        );
        assert_eq!(
            text_event_to_hotkey_logical_key("M"),
            Some(HotkeyLogicalKey::Egui(Key::M))
        );
    }
}
