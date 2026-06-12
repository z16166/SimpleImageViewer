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

use crate::app::ImageViewerApp;
use crate::hotkeys::model::{
    HotkeyActionId, KeyChord, action_id_to_str, default_hotkey_config_file,
};
use crate::ui::utils::{styled_button, styled_button_widget};
use eframe::egui::{self, Color32, Context, RichText};
use rust_i18n::t;
use std::time::Instant;

const HOTKEYS_INDEX_COL_WIDTH: f32 = 48.0;
const HOTKEYS_ACTION_COL_WIDTH: f32 = 230.0;
const HOTKEYS_KEY_COL_WIDTH: f32 = 320.0;

fn poll_hotkey_capture_from_input(ctx: &Context) -> Option<String> {
    let mut captured: Option<String> = None;
    ctx.input(|i| {
        for event in &i.events {
            match event {
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    let chord = KeyChord::from_input_event(*key, *modifiers);
                    captured = Some(chord.display_string());
                    break;
                }
                egui::Event::MouseWheel {
                    delta, modifiers, ..
                } => {
                    if let Some(chord) = KeyChord::from_wheel_input(delta.y, *modifiers) {
                        captured = Some(chord.display_string());
                        break;
                    }
                }
                egui::Event::PointerButton {
                    button,
                    pressed: false,
                    modifiers,
                    ..
                } => {
                    if let Some(chord) = KeyChord::from_pointer_button(*button, *modifiers)
                        && !(chord.requires_modifier() && chord.modifiers == 0)
                    {
                        captured = Some(chord.display_string());
                        break;
                    }
                }
                _ => {}
            }
        }
    });
    captured
}
pub(super) fn draw_hotkeys_tab_status(
    app: &ImageViewerApp,
    ui: &mut egui::Ui,
    preview: &crate::hotkeys::RuntimeHotkeyState,
    has_empty_key: bool,
    capture_pending: bool,
    apply_success: bool,
) {
    if apply_success {
        ui.add_space(4.0);
        ui.add(
            egui::Label::new(
                RichText::new(t!("hotkeys.apply_success"))
                    .color(app.cached_palette.accent2)
                    .strong(),
            )
            .wrap(),
        );
    }
    if capture_pending {
        ui.add_space(4.0);
        ui.add(
            egui::Label::new(
                RichText::new(t!("hotkeys.capture_hint"))
                    .color(app.cached_palette.button_primary)
                    .strong(),
            )
            .wrap(),
        );
    }
    if let Some(error) = &app.hotkeys_load_error {
        ui.add_space(4.0);
        ui.add(
            egui::Label::new(
                RichText::new(t!("hotkeys.load_failed", error = error.as_str()))
                    .color(ui.visuals().error_fg_color)
                    .strong(),
            )
            .wrap(),
        );
    }
    if has_empty_key {
        ui.add_space(4.0);
        ui.add(
            egui::Label::new(
                RichText::new(t!("hotkeys.empty_key_block_save"))
                    .color(ui.visuals().error_fg_color)
                    .strong(),
            )
            .wrap(),
        );
    }
    if !preview.conflicts.is_empty() {
        ui.add_space(4.0);
        ui.add(
            egui::Label::new(
                RichText::new(t!("hotkeys.conflict_block_save"))
                    .color(ui.visuals().error_fg_color)
                    .strong(),
            )
            .wrap(),
        );
        for conflict in &preview.conflicts {
            let action_names = conflict
                .actions
                .iter()
                .map(|it| localized_hotkey_action_label(*it))
                .collect::<Vec<_>>()
                .join(", ");
            ui.add(
                egui::Label::new(
                    RichText::new(format!("{}: {}", conflict.key, action_names))
                        .color(ui.visuals().error_fg_color),
                )
                .wrap(),
            );
        }
    }
    if !preview.warnings.is_empty() {
        ui.add_space(4.0);
        for warning in &preview.warnings {
            ui.add(
                egui::Label::new(
                    RichText::new(crate::app::localized_hotkey_warning(warning))
                        .color(app.cached_palette.accent2),
                )
                .wrap(),
            );
        }
    }
}

pub(super) fn draw_hotkeys_tab(app: &mut ImageViewerApp, ui: &mut egui::Ui, ctx: &Context) {
    let mut draft = app.hotkeys_draft_config.clone();
    let mut should_save = false;
    let mut row_to_delete: Option<(usize, usize)> = None;
    let mut row_to_add: Option<(HotkeyActionId, String)> = None;
    let mut set_key_target: Option<(HotkeyActionId, usize, usize)> = None;
    let mut add_row_start_capture = false;

    ui.vertical(|ui| {
        ui.add_space(8.0);
        ui.label(
            RichText::new(t!("section.hotkeys"))
                .color(app.cached_palette.accent2)
                .strong(),
        );
        ui.add_space(4.0);

        let preview = crate::hotkeys::rebuild_runtime_state(&draft);
        let conflict_keys: std::collections::HashSet<String> = preview
            .conflicts
            .iter()
            .map(|conflict| conflict.key.clone())
            .collect();
        let has_empty_key = draft
            .bindings
            .iter()
            .any(|entry| entry.keys.iter().any(|key| key.trim().is_empty()));
        ui.add_space(8.0);
        let capture_pending = app.hotkeys_capture_target.is_some() || set_key_target.is_some();
        let apply_success = app.hotkeys_apply_success_at.is_some();
        draw_hotkeys_tab_status(
            app,
            ui,
            &preview,
            has_empty_key,
            capture_pending,
            apply_success,
        );
        let footer_h = 54.0;
        let available_h = (ui.available_height() - footer_h).max(80.0);
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), available_h),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                egui::Grid::new("hotkeys_header_grid")
                    .num_columns(3)
                    .spacing([0.0, 2.0])
                    .striped(false)
                    .show(ui, |ui| {
                        let header_font = egui::TextStyle::Body.resolve(ui.style());
                        let header_color = ui.visuals().text_color();
                        let draw_header = |ui: &mut egui::Ui, width: f32, text: &str| {
                            let (rect, _) = ui
                                .allocate_exact_size(egui::vec2(width, 18.0), egui::Sense::hover());
                            ui.painter().text(
                                egui::pos2(rect.left() + 8.0, rect.center().y),
                                egui::Align2::LEFT_CENTER,
                                text,
                                header_font.clone(),
                                header_color,
                            );
                        };
                        draw_header(ui, HOTKEYS_INDEX_COL_WIDTH, &t!("hotkeys.column_index"));
                        draw_header(ui, HOTKEYS_ACTION_COL_WIDTH, &t!("hotkeys.column_action"));
                        draw_header(ui, HOTKEYS_KEY_COL_WIDTH, &t!("hotkeys.column_key"));
                        ui.end_row();
                    });
                egui::ScrollArea::vertical()
                    .id_salt("hotkeys_tab_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let mut rows: Vec<(usize, usize, HotkeyActionId, String)> = Vec::new();
                        for (entry_idx, entry) in draft.bindings.iter().enumerate() {
                            let Some(action_id) =
                                crate::hotkeys::model::action_id_from_str(&entry.action_id)
                            else {
                                continue;
                            };
                            if entry.keys.is_empty() {
                                rows.push((entry_idx, 0, action_id, String::new()));
                            } else {
                                for (key_idx, key_text) in entry.keys.iter().enumerate() {
                                    rows.push((entry_idx, key_idx, action_id, key_text.clone()));
                                }
                            }
                        }

                        egui::Grid::new("hotkeys_grid")
                            .num_columns(3)
                            .spacing([0.0, 2.0])
                            .striped(false)
                            .show(ui, |ui| {
                                for (row_idx, (entry_idx, key_idx, action_id, key_text)) in
                                    rows.iter().enumerate()
                                {
                                    let selected =
                                        app.hotkeys_selected_row == Some((*entry_idx, *key_idx));
                                    let zebra_fill = if row_idx % 2 == 0 {
                                        ui.visuals().faint_bg_color.gamma_multiply(0.18)
                                    } else {
                                        egui::Color32::TRANSPARENT
                                    };
                                    let cell_fill = if selected {
                                        ui.visuals().selection.bg_fill
                                    } else {
                                        zebra_fill
                                    };
                                    let text_color = if selected {
                                        ui.visuals().selection.stroke.color
                                    } else {
                                        ui.visuals().text_color()
                                    };
                                    let key_is_empty = key_text.trim().is_empty();
                                    let key_has_error =
                                        key_is_empty || conflict_keys.contains(key_text);
                                    let key_fill = if key_has_error && !selected {
                                        app.cached_palette.widget_active
                                    } else {
                                        cell_fill
                                    };
                                    let key_text_color = if key_has_error {
                                        ui.visuals().error_fg_color
                                    } else {
                                        text_color
                                    };
                                    let font = egui::TextStyle::Body.resolve(ui.style());

                                    let draw_cell = |ui: &mut egui::Ui,
                                                     width: f32,
                                                     text: &str,
                                                     fill: Color32,
                                                     color: Color32|
                                     -> bool {
                                        let (rect, response) = ui.allocate_exact_size(
                                            egui::vec2(width, 24.0),
                                            egui::Sense::click(),
                                        );
                                        ui.painter().rect_filled(rect, 0.0, fill);
                                        ui.painter().text(
                                            egui::pos2(rect.left() + 8.0, rect.center().y),
                                            egui::Align2::LEFT_CENTER,
                                            text,
                                            font.clone(),
                                            color,
                                        );
                                        response.clicked()
                                    };
                                    let index_clicked = draw_cell(
                                        ui,
                                        HOTKEYS_INDEX_COL_WIDTH,
                                        &(row_idx + 1).to_string(),
                                        cell_fill,
                                        text_color,
                                    );
                                    let action_label = localized_hotkey_action_label(*action_id);
                                    let action_clicked = draw_cell(
                                        ui,
                                        HOTKEYS_ACTION_COL_WIDTH,
                                        &action_label,
                                        cell_fill,
                                        text_color,
                                    );
                                    let key_clicked = draw_cell(
                                        ui,
                                        HOTKEYS_KEY_COL_WIDTH,
                                        key_text,
                                        key_fill,
                                        key_text_color,
                                    );

                                    if index_clicked || action_clicked || key_clicked {
                                        app.hotkeys_selected_row = Some((*entry_idx, *key_idx));
                                    }
                                    ui.end_row();
                                }
                            });
                    });
            },
        );

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if styled_button(ui, t!("hotkeys.add_row"), &app.cached_palette).clicked() {
                app.reset_hotkeys_add_row_dialog_state();
                app.hotkeys_add_row_dialog_open = true;
            }

            let can_delete_selected = if let Some((entry_idx, key_idx)) = app.hotkeys_selected_row {
                if entry_idx < draft.bindings.len() {
                    let action_id = draft.bindings[entry_idx].action_id.clone();
                    let key_count_for_action: usize = draft
                        .bindings
                        .iter()
                        .filter(|it| it.action_id == action_id)
                        .map(|it| it.keys.len().max(1))
                        .sum();
                    key_idx < draft.bindings[entry_idx].keys.len() && key_count_for_action > 1
                } else {
                    false
                }
            } else {
                false
            };
            if ui
                .add_enabled(
                    can_delete_selected,
                    styled_button_widget(t!("hotkeys.delete_row"), &app.cached_palette),
                )
                .clicked()
            {
                row_to_delete = app.hotkeys_selected_row;
            }
            let can_set_key_selected = if let Some((entry_idx, key_idx)) = app.hotkeys_selected_row
            {
                entry_idx < draft.bindings.len() && key_idx < draft.bindings[entry_idx].keys.len()
            } else {
                false
            };
            if ui
                .add_enabled(
                    can_set_key_selected,
                    styled_button_widget(t!("hotkeys.set_key"), &app.cached_palette),
                )
                .clicked()
            {
                if let Some((entry_idx, key_idx)) = app.hotkeys_selected_row
                    && let Some(binding) = draft.bindings.get(entry_idx)
                    && let Some(action_id) =
                        crate::hotkeys::model::action_id_from_str(&binding.action_id)
                {
                    set_key_target = Some((action_id, entry_idx, key_idx));
                }
            }

            if styled_button(ui, t!("hotkeys.restore_defaults"), &app.cached_palette).clicked() {
                draft = default_hotkey_config_file();
                app.hotkeys_selected_row = None;
                should_save = true;
            }
            if styled_button(ui, t!("hotkeys.apply"), &app.cached_palette).clicked() {
                let validated = crate::hotkeys::rebuild_runtime_state(&draft);
                if !has_empty_key && validated.conflicts.is_empty() {
                    app.hotkeys_runtime = validated;
                    app.hotkeys_load_error = None;
                    app.hotkeys_apply_success_at = Some(Instant::now());
                    should_save = true;
                } else {
                    app.hotkeys_apply_success_at = None;
                }
            }
        });
    });

    if let Some(target) = set_key_target {
        app.hotkeys_capture_target = Some(target);
    }

    if app.hotkeys_add_row_dialog_open {
        let dialog_size = egui::vec2(380.0, 260.0);
        let viewport_size = ctx.input(|i| {
            i.viewport()
                .inner_rect
                .map(|r| r.size())
                .unwrap_or_else(|| egui::vec2(1024.0, 720.0))
        });
        let default_pos = egui::pos2(
            ((viewport_size.x - dialog_size.x) * 0.5).max(0.0),
            ((viewport_size.y - dialog_size.y) * 0.5).max(0.0),
        );
        // Do not bind `.open(...)`: outside clicks (including ComboBox popups) would close the
        // window and can click through to OK beneath the dropdown.
        egui::Window::new(t!("hotkeys.add_row"))
            .collapsible(false)
            .resizable(false)
            .default_pos(default_pos)
            .default_size(dialog_size)
            .show(ctx, |ui| {
                ui.set_min_width(340.0);
                ui.label(t!("hotkeys.select_action"));
                ui.add_space(6.0);
                let action_before = app.hotkeys_add_row_action;
                egui::ComboBox::from_id_salt("hotkeys_add_row_action")
                    .selected_text(localized_hotkey_action_label(app.hotkeys_add_row_action))
                    .width(ui.available_width())
                    .show_ui(ui, |ui| {
                        ui.set_min_width(280.0);
                        for desc in crate::hotkeys::model::all_action_descriptors() {
                            ui.selectable_value(
                                &mut app.hotkeys_add_row_action,
                                desc.id,
                                localized_hotkey_action_label(desc.id),
                            );
                        }
                    });
                if app.hotkeys_add_row_action != action_before {
                    app.hotkeys_add_row_captured_key = None;
                    app.hotkeys_add_row_capture_active = false;
                    app.hotkeys_add_row_need_key_hint = false;
                }
                ui.add_space(10.0);
                ui.label(t!("hotkeys.column_key"));
                let key_label = app
                    .hotkeys_add_row_captured_key
                    .as_deref()
                    .filter(|k| !k.trim().is_empty())
                    .map(|k| k.to_string())
                    .unwrap_or_else(|| t!("hotkeys.no_keys").to_string());
                ui.add(egui::Label::new(RichText::new(key_label).strong()).wrap());
                ui.add_space(6.0);
                if ui
                    .add(styled_button_widget(
                        t!("hotkeys.set_key"),
                        &app.cached_palette,
                    ))
                    .clicked()
                {
                    add_row_start_capture = true;
                    app.hotkeys_add_row_capture_active = true;
                    app.hotkeys_add_row_need_key_hint = false;
                }
                if app.hotkeys_add_row_capture_active {
                    ui.add_space(4.0);
                    ui.add(
                        egui::Label::new(
                            RichText::new(t!("hotkeys.capture_hint"))
                                .color(app.cached_palette.button_primary)
                                .strong(),
                        )
                        .wrap(),
                    );
                }
                if app.hotkeys_add_row_need_key_hint {
                    ui.add_space(4.0);
                    ui.add(
                        egui::Label::new(
                            RichText::new(t!("hotkeys.add_row_need_key"))
                                .color(ui.visuals().error_fg_color)
                                .strong(),
                        )
                        .wrap(),
                    );
                }
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if styled_button(ui, t!("btn.ok"), &app.cached_palette).clicked() {
                        if app
                            .hotkeys_add_row_captured_key
                            .as_ref()
                            .is_some_and(|k| !k.trim().is_empty())
                        {
                            row_to_add = Some((
                                app.hotkeys_add_row_action,
                                app.hotkeys_add_row_captured_key.clone().unwrap_or_default(),
                            ));
                            app.hotkeys_add_row_dialog_open = false;
                            app.reset_hotkeys_add_row_dialog_state();
                        } else {
                            app.hotkeys_add_row_need_key_hint = true;
                            app.hotkeys_add_row_capture_active = false;
                        }
                    }
                    if styled_button(ui, t!("btn.cancel"), &app.cached_palette).clicked() {
                        app.hotkeys_add_row_dialog_open = false;
                        app.reset_hotkeys_add_row_dialog_state();
                    }
                });
            });
    }

    if let Some((action_id, key_text)) = row_to_add {
        let action_id_text = action_id_to_str(action_id).to_string();
        if let Some((entry_idx, entry)) = draft
            .bindings
            .iter_mut()
            .enumerate()
            .find(|(_, entry)| entry.action_id == action_id_text)
        {
            entry.keys.push(key_text.clone());
            let key_idx = entry.keys.len().saturating_sub(1);
            app.hotkeys_selected_row = Some((entry_idx, key_idx));
        } else {
            draft
                .bindings
                .push(crate::hotkeys::model::HotkeyBindingEntry {
                    action_id: action_id_to_str(action_id).to_string(),
                    keys: vec![key_text],
                    enabled: true,
                    comment: String::new(),
                });
            if let Some(new_idx) = draft.bindings.len().checked_sub(1) {
                app.hotkeys_selected_row = Some((new_idx, 0));
            }
        }
        should_save = true;
    }

    // Defer capture until the next frame so Set Key / Record Key clicks are not recorded as hotkeys.
    let defer_key_capture = set_key_target.is_some() || add_row_start_capture;

    if let Some((entry_idx, key_idx)) = row_to_delete {
        if entry_idx < draft.bindings.len() {
            let action_id = draft.bindings[entry_idx].action_id.clone();
            let key_count_for_action: usize = draft
                .bindings
                .iter()
                .filter(|it| it.action_id == action_id)
                .map(|it| it.keys.len().max(1))
                .sum();
            if key_count_for_action > 1 {
                if key_idx < draft.bindings[entry_idx].keys.len() {
                    draft.bindings[entry_idx].keys.remove(key_idx);
                }
                if draft.bindings[entry_idx].keys.is_empty() {
                    draft.bindings.remove(entry_idx);
                }
                app.hotkeys_selected_row = None;
                should_save = true;
            }
        }
    }

    if app.hotkeys_add_row_capture_active && !defer_key_capture {
        if let Some(key_text) = poll_hotkey_capture_from_input(ctx) {
            app.hotkeys_add_row_captured_key = Some(key_text);
            app.hotkeys_add_row_capture_active = false;
        }
    }

    if let Some((target, entry_idx, key_idx)) = app.hotkeys_capture_target
        && !defer_key_capture
    {
        if let Some(key_text) = poll_hotkey_capture_from_input(ctx) {
            if entry_idx < draft.bindings.len() {
                let entry = &mut draft.bindings[entry_idx];
                entry.action_id = action_id_to_str(target).to_string();
                while entry.keys.len() <= key_idx {
                    entry.keys.push(String::new());
                }
                entry.keys[key_idx] = key_text;
            } else {
                draft
                    .bindings
                    .push(crate::hotkeys::model::HotkeyBindingEntry {
                        action_id: action_id_to_str(target).to_string(),
                        keys: vec![key_text],
                        enabled: true,
                        comment: String::new(),
                    });
            }
            app.hotkeys_capture_target = None;
            should_save = true;
        }
    }
    let validated = crate::hotkeys::rebuild_runtime_state(&draft);
    let has_empty_key = validated
        .config
        .bindings
        .iter()
        .any(|entry| entry.keys.iter().any(|key| key.trim().is_empty()));
    if !has_empty_key && validated.conflicts.is_empty() && should_save {
        app.hotkeys_runtime = validated.clone();
        app.hotkeys_draft_config = validated.config.clone();
        app.hotkeys_load_error = None;
        app.queue_hotkeys_save();
    } else {
        app.hotkeys_draft_config = draft;
    }
}

fn localized_hotkey_action_label(action_id: HotkeyActionId) -> String {
    let key = match action_id {
        HotkeyActionId::NextImage => "hotkeys.action.next_image",
        HotkeyActionId::PrevImage => "hotkeys.action.prev_image",
        HotkeyActionId::FirstImage => "hotkeys.action.first_image",
        HotkeyActionId::LastImage => "hotkeys.action.last_image",
        HotkeyActionId::ZoomIn => "hotkeys.action.zoom_in",
        HotkeyActionId::ZoomOut => "hotkeys.action.zoom_out",
        HotkeyActionId::ZoomReset => "hotkeys.action.zoom_reset",
        HotkeyActionId::ToggleSettings => "hotkeys.action.toggle_settings",
        HotkeyActionId::ToggleFullscreen => "hotkeys.action.toggle_fullscreen",
        HotkeyActionId::ToggleScaleMode => "hotkeys.action.toggle_scale_mode",
        HotkeyActionId::ToggleOsd => "hotkeys.action.toggle_osd",
        HotkeyActionId::RotateCw => "hotkeys.action.rotate_cw",
        HotkeyActionId::RotateCcw => "hotkeys.action.rotate_ccw",
        HotkeyActionId::HdrExposureUp => "hotkeys.action.hdr_exposure_up",
        HotkeyActionId::HdrExposureDown => "hotkeys.action.hdr_exposure_down",
        HotkeyActionId::DeleteToRecycleBin => "hotkeys.action.delete_to_recycle_bin",
        HotkeyActionId::PermanentDelete => "hotkeys.action.permanent_delete",
        HotkeyActionId::PrintCurrent => "hotkeys.action.print_current",
        HotkeyActionId::ToggleGoto => "hotkeys.action.toggle_goto",
        HotkeyActionId::ToggleSlideshow => "hotkeys.action.toggle_slideshow",
        HotkeyActionId::RefreshFileList => "hotkeys.action.refresh_file_list",
        #[cfg(not(target_os = "windows"))]
        HotkeyActionId::Quit => "hotkeys.action.quit_app",
        HotkeyActionId::ExitFullscreen => "hotkeys.action.exit_fullscreen",
        HotkeyActionId::SelectPixelRegion => "hotkeys.action.select_pixel_region",
        HotkeyActionId::CopyTo => "hotkeys.action.copy_to",
        HotkeyActionId::CutTo => "hotkeys.action.cut_to",
        HotkeyActionId::ToggleTray => "hotkeys.action.toggle_tray",
    };
    t!(key).to_string()
}
