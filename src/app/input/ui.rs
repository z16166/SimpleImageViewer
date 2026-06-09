use super::AppAction;
use crate::app::ImageViewerApp;
use crate::ui::dialogs::modal_state::{ActiveModal, ModalResult};
use crate::ui::utils::copy_file_to_clipboard;
use crate::ui::{hud as ui_hud, settings as ui_settings};
use eframe::egui::{self};
use rust_i18n::t;

impl ImageViewerApp {
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
