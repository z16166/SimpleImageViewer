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
    pub(crate) fn dispatch_active_modal(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
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
            Some(ActiveModal::PixelRegion(state)) => {
                crate::ui::dialogs::pixel_region_dialog::show(state, ctx, &self.cached_palette)
            }
            Some(ActiveModal::FileCopyCut(state)) => {
                crate::ui::dialogs::file_copy_cut::show(state, ctx, &self.cached_palette)
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

        if let Some(crate::ui::dialogs::modal_state::ActiveModal::FileCopyCut(state)) =
            self.active_modal.as_mut()
        {
            if state.browse_folder_requested {
                state.browse_folder_requested = false;
                let starting = if state.input.trim().is_empty() {
                    None
                } else {
                    Some(std::path::PathBuf::from(state.input.trim()))
                };
                self.request_folder_picker(
                    frame,
                    crate::app::folder_picker::FolderPickerPurpose::FileCopyCutModal,
                    starting,
                );
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
            ModalAction::FileCopyCut {
                is_cut,
                target_dir,
                overwrite_if_exists,
            } => {
                self.copy_cut_overwrite_if_exists = overwrite_if_exists;
                self.settings.last_copy_cut_dir = Some(target_dir.clone());
                self.queue_save();
                if is_cut {
                    self.cut_current_image_to(target_dir, overwrite_if_exists);
                } else {
                    self.copy_current_image_to(target_dir, overwrite_if_exists);
                }
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
    /// `egui::Area`-based popup in [`Self::paint_image_context_menu_if_open`]).
    pub(crate) fn draw_context_menu_items(&mut self, ui: &mut egui::Ui) {
        self.ensure_context_menu_label_cache();

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
                    let clicked = {
                        let cache = self
                            .context_menu_label_cache
                            .as_ref()
                            .expect("label cache must exist while menu is open");
                        let Some(label) = cache.labels.get(idx).and_then(|label| label.as_deref())
                        else {
                            continue;
                        };
                        ui.button(label).clicked()
                    };
                    if clicked {
                        let path = self.image_files[self.current_index].clone();
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
                            let path = self.image_files[self.current_index].clone();
                            self.run_custom_context_menu_action(&command, &path);
                        }
                        self.clear_image_context_menu();
                    }
                    drew_action = true;
                }
            }
        }
    }

    /// Open the image context menu when the user secondary-clicks inside `open_zone`.
    pub(crate) fn try_open_image_context_menu(
        &mut self,
        ctx: &egui::Context,
        open_zone: Option<egui::Rect>,
        allow_open: bool,
    ) {
        if !allow_open || self.image_files.is_empty() {
            return;
        }
        if !ctx.input(|i| i.pointer.secondary_clicked()) {
            return;
        }
        let Some(pos) = ctx.input(|i| i.pointer.interact_pos()) else {
            return;
        };
        let Some(zone) = open_zone else {
            return;
        };
        if !zone.contains(pos) {
            return;
        }
        self.context_menu_pos = Some(pos);
        self.context_menu_viewport = Some(ctx.viewport_id());
        self.rebuild_context_menu_label_cache();
    }

    fn clear_image_context_menu(&mut self) {
        self.context_menu_pos = None;
        self.context_menu_viewport = None;
        self.context_menu_label_cache = None;
    }

    pub(crate) fn rebuild_context_menu_label_cache(&mut self) {
        let mut labels = Vec::with_capacity(self.context_menu_runtime.config.items.len());
        for item in &self.context_menu_runtime.config.items {
            labels.push(match item.kind {
                crate::context_menu::model::ContextMenuItemKind::Separator
                | crate::context_menu::model::ContextMenuItemKind::Custom => None,
                crate::context_menu::model::ContextMenuItemKind::Builtin => item
                    .builtin_id
                    .as_deref()
                    .and_then(crate::context_menu::model::builtin_descriptor)
                    .map(|desc| self.builtin_context_menu_label(desc.id, desc.label_key)),
            });
        }
        self.context_menu_label_cache = Some(crate::app::types::ContextMenuLabelCache {
            labels,
            fullscreen: self.settings.fullscreen,
            language: self.settings.language.clone(),
        });
    }

    fn ensure_context_menu_label_cache(&mut self) {
        let stale = self.context_menu_label_cache.as_ref().is_none_or(|cache| {
            cache.fullscreen != self.settings.fullscreen || cache.language != self.settings.language
        });
        if stale {
            self.rebuild_context_menu_label_cache();
        }
    }

    fn builtin_context_menu_label(&self, id: &str, label_key: &str) -> String {
        if id == "toggle_fullscreen" {
            if self.settings.fullscreen {
                t!("ctx.fullscreen_exit").to_string()
            } else {
                t!("ctx.fullscreen_enter").to_string()
            }
        } else if id == "print_current" && cfg!(not(target_os = "windows")) {
            t!("ctx.print_pdf_full").to_string()
        } else {
            t!(label_key).to_string()
        }
    }

    /// Paint the custom image context menu when it belongs to this viewport.
    pub(crate) fn paint_image_context_menu_if_open(&mut self, ctx: &egui::Context) {
        if self.image_files.is_empty() {
            return;
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.clear_image_context_menu();
        }
        if self.context_menu_pos.is_none() {
            return;
        }
        if self
            .context_menu_viewport
            .is_some_and(|viewport| viewport != ctx.viewport_id())
        {
            return;
        }

        let pos = self.context_menu_pos.unwrap_or_default();
        let menu_id = egui::Id::new("custom_image_ctx_menu").with(&self.settings.language);
        let area_resp = egui::Area::new(menu_id)
            .kind(egui::UiKind::Menu)
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .sense(egui::Sense::hover())
            .show(ctx, |ui| {
                egui::Frame::menu(ui.style()).show(ui, |ui| {
                    ui.with_layout(egui::Layout::top_down_justified(egui::Align::LEFT), |ui| {
                        self.draw_context_menu_items(ui)
                    });
                });
            });

        let menu_rect = area_resp.response.rect;
        let interact_pos = ctx.input(|i| i.pointer.interact_pos());
        if ctx.input(|i| i.pointer.primary_clicked()) {
            if let Some(pp) = interact_pos {
                if !menu_rect.contains(pp) {
                    self.clear_image_context_menu();
                }
            }
        }
        if area_resp.response.should_close() {
            self.clear_image_context_menu();
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
            "copy_to" => self.dispatch_action(AppAction::CopyTo, ui.ctx()),
            "cut_to" => self.dispatch_action(AppAction::CutTo, ui.ctx()),
            _ => log::warn!("Unknown context menu builtin action: {}", id),
        }
        self.clear_image_context_menu();
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
