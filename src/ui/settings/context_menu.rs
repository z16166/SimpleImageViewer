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

use crate::app::{ImageViewerApp, SettingsTab};
use crate::context_menu::model::{
    ContextMenuCommand, ContextMenuEntry, ContextMenuItemKind, EditableContextMenuEntry,
    EditableContextMenuEntryKind, builtin_descriptor, default_context_menu_config_file,
};
use crate::ui::utils::{styled_button, styled_button_widget};
use eframe::egui::{self, Context, RichText};
use rust_i18n::t;
use std::time::Instant;

const CONTEXT_MENU_INDEX_COL_WIDTH: f32 = 34.0;
const CONTEXT_MENU_LABEL_COL_WIDTH: f32 = 300.0;
const CONTEXT_MENU_ENABLED_COL_WIDTH: f32 = 82.0;
const CONTEXT_MENU_GRID_WIDTH: f32 =
    CONTEXT_MENU_INDEX_COL_WIDTH + CONTEXT_MENU_LABEL_COL_WIDTH + CONTEXT_MENU_ENABLED_COL_WIDTH;

pub(super) fn draw_context_menu_tab(app: &mut ImageViewerApp, ui: &mut egui::Ui, ctx: &Context) {
    let mut draft = app.context_menu_draft_config.clone();
    let mut draft_changed = false;
    let mut apply_clicked = false;
    let mut row_to_delete = None;
    let mut move_selected: Option<ContextMenuMove> = None;
    let mut edit_result: Option<ContextMenuEntry> = None;
    let mut selection_delta = 0;
    let mut selection_target: Option<ContextMenuSelectionTarget> = None;

    handle_context_menu_tab_shortcuts(
        app,
        ctx,
        &mut row_to_delete,
        &mut move_selected,
        &mut selection_delta,
        &mut selection_target,
    );
    if !ctx.input(|i| i.pointer.primary_down()) {
        app.context_menu_drag_row = None;
    }
    if selection_delta != 0 {
        app.context_menu_selected_row = context_menu_selection_after_arrow(
            app.context_menu_selected_row,
            draft.items.len(),
            selection_delta,
        );
        app.context_menu_scroll_to_selected = true;
    }
    if let Some(target) = selection_target {
        app.context_menu_selected_row = match target {
            ContextMenuSelectionTarget::Home => context_menu_selection_home(draft.items.len()),
            ContextMenuSelectionTarget::End => context_menu_selection_end(draft.items.len()),
        };
        app.context_menu_scroll_to_selected = true;
    }

    ui.vertical(|ui| {
        ui.add_space(8.0);
        ui.label(
            RichText::new(t!("section.context_menu"))
                .color(app.cached_palette.accent2)
                .strong(),
        );
        ui.add_space(4.0);
        if app.context_menu_apply_success_at.is_some() {
            ui.add_space(4.0);
            ui.label(
                RichText::new(t!("context_menu.apply_success"))
                    .color(app.cached_palette.button_primary)
                    .strong(),
            );
        }
        if let Some(error) = &app.context_menu_apply_error {
            ui.add_space(4.0);
            ui.label(
                RichText::new(error)
                    .color(ui.visuals().error_fg_color)
                    .strong(),
            );
        }

        let footer_h = 58.0;
        let available_h = (ui.available_height() - footer_h).max(120.0);
        let list_width = (CONTEXT_MENU_GRID_WIDTH + 28.0).min(ui.available_width());
        ui.allocate_ui_with_layout(
            egui::vec2(list_width, available_h),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                egui::Grid::new("context_menu_header_grid")
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
                        draw_header(
                            ui,
                            CONTEXT_MENU_INDEX_COL_WIDTH,
                            &t!("context_menu.column_index"),
                        );
                        draw_header(
                            ui,
                            CONTEXT_MENU_LABEL_COL_WIDTH,
                            &t!("context_menu.column_label"),
                        );
                        draw_header(
                            ui,
                            CONTEXT_MENU_ENABLED_COL_WIDTH,
                            &t!("context_menu.column_enabled"),
                        );
                        ui.end_row();
                    });
                egui::ScrollArea::vertical()
                    .id_salt("context_menu_tab_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        egui::Grid::new("context_menu_grid")
                            .num_columns(3)
                            .spacing([0.0, 2.0])
                            .striped(false)
                            .show(ui, |ui| {
                                for row_idx in 0..draft.items.len() {
                                    let selected = app.context_menu_selected_row == Some(row_idx);
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
                                    let font = egui::TextStyle::Body.resolve(ui.style());

                                    let label =
                                        localized_context_menu_entry_label(&draft.items[row_idx]);
                                    let (index_rect, index_resp) = ui.allocate_exact_size(
                                        egui::vec2(CONTEXT_MENU_INDEX_COL_WIDTH, 24.0),
                                        egui::Sense::click_and_drag(),
                                    );
                                    let (label_rect, label_resp) = ui.allocate_exact_size(
                                        egui::vec2(CONTEXT_MENU_LABEL_COL_WIDTH, 24.0),
                                        egui::Sense::click_and_drag(),
                                    );
                                    let (enabled_rect, _) = ui.allocate_exact_size(
                                        egui::vec2(CONTEXT_MENU_ENABLED_COL_WIDTH, 24.0),
                                        egui::Sense::click(),
                                    );
                                    let row_clicked = index_resp.clicked() || label_resp.clicked();
                                    let row_drag_started =
                                        index_resp.drag_started() || label_resp.drag_started();
                                    let row_drag_stopped =
                                        index_resp.drag_stopped() || label_resp.drag_stopped();
                                    let row_rect = index_rect.union(label_rect);
                                    let scroll_rect = row_rect.union(enabled_rect);
                                    let full_row_rect = scroll_rect;
                                    ui.painter().rect_filled(full_row_rect, 0.0, cell_fill);
                                    if app.context_menu_drag_row == Some(row_idx) {
                                        ui.painter().rect_stroke(
                                            full_row_rect.shrink(1.0),
                                            3.0,
                                            egui::Stroke::new(
                                                1.0_f32,
                                                app.cached_palette.button_primary,
                                            ),
                                            egui::StrokeKind::Outside,
                                        );
                                    }
                                    ui.painter().text(
                                        egui::pos2(index_rect.left() + 8.0, index_rect.center().y),
                                        egui::Align2::LEFT_CENTER,
                                        (row_idx + 1).to_string(),
                                        font.clone(),
                                        text_color,
                                    );
                                    ui.painter().text(
                                        egui::pos2(label_rect.left() + 8.0, label_rect.center().y),
                                        egui::Align2::LEFT_CENTER,
                                        &label,
                                        font.clone(),
                                        text_color,
                                    );
                                    let enabled_clicked = if draft.items[row_idx].kind
                                        == ContextMenuItemKind::Separator
                                    {
                                        false
                                    } else {
                                        ui.put(
                                            enabled_rect,
                                            egui::Checkbox::without_text(
                                                &mut draft.items[row_idx].enabled,
                                            ),
                                        )
                                        .clicked()
                                    };
                                    let row_resp = ui.interact(
                                        row_rect,
                                        egui::Id::new(("context_menu_row", row_idx)),
                                        egui::Sense::click_and_drag(),
                                    );
                                    if selected && app.context_menu_scroll_to_selected {
                                        ui.scroll_to_rect(scroll_rect, None);
                                        app.context_menu_scroll_to_selected = false;
                                    }
                                    if enabled_clicked {
                                        draft_changed = true;
                                        app.context_menu_apply_error = None;
                                    }
                                    if row_clicked || enabled_clicked || row_resp.clicked() {
                                        app.context_menu_selected_row = Some(row_idx);
                                        app.context_menu_scroll_to_selected = false;
                                    }
                                    if row_drag_started || row_resp.drag_started() {
                                        app.context_menu_drag_row = Some(row_idx);
                                        app.context_menu_selected_row = Some(row_idx);
                                        app.context_menu_scroll_to_selected = false;
                                    }
                                    if let Some(from_idx) = app.context_menu_drag_row
                                        && from_idx != row_idx
                                        && ctx
                                            .input(|i| i.pointer.interact_pos())
                                            .is_some_and(|pos| row_rect.contains(pos))
                                        && from_idx < draft.items.len()
                                    {
                                        let entry = draft.items.remove(from_idx);
                                        draft.items.insert(row_idx, entry);
                                        app.context_menu_drag_row = Some(row_idx);
                                        app.context_menu_selected_row = Some(row_idx);
                                        app.context_menu_scroll_to_selected = false;
                                        draft_changed = true;
                                    }
                                    if row_drag_stopped || row_resp.drag_stopped() {
                                        app.context_menu_drag_row = None;
                                    }
                                    ui.end_row();
                                }
                            });
                    });
            },
        );

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if styled_button(ui, t!("context_menu.add"), &app.cached_palette).clicked() {
                app.context_menu_edit_target = None;
                app.context_menu_edit_draft = EditableContextMenuEntry::default();
                app.context_menu_edit_dialog_open = true;
            }
            let can_delete = app
                .context_menu_selected_row
                .and_then(|idx| draft.items.get(idx))
                .is_some_and(can_delete_context_menu_entry);
            if ui
                .add_enabled(
                    can_delete,
                    styled_button_widget(t!("context_menu.delete"), &app.cached_palette),
                )
                .clicked()
            {
                row_to_delete = app.context_menu_selected_row;
            }
            let can_modify = app
                .context_menu_selected_row
                .and_then(|idx| draft.items.get(idx))
                .is_some_and(|entry| entry.kind != ContextMenuItemKind::Builtin);
            if ui
                .add_enabled(
                    can_modify,
                    styled_button_widget(t!("context_menu.modify"), &app.cached_palette),
                )
                .clicked()
            {
                if let Some(idx) = app.context_menu_selected_row
                    && let Some(entry) = draft.items.get(idx)
                {
                    app.context_menu_edit_target = Some(idx);
                    app.context_menu_edit_draft = editable_context_menu_entry_from_entry(entry);
                    app.context_menu_edit_dialog_open = true;
                }
            }
            if styled_button(ui, t!("context_menu.restore_defaults"), &app.cached_palette).clicked()
            {
                let action = context_menu_restore_defaults_action();
                draft = default_context_menu_config_file();
                app.context_menu_selected_row = None;
                app.context_menu_apply_success_at = None;
                app.context_menu_apply_error = None;
                draft_changed |= action.draft_changed;
                apply_clicked |= action.apply_clicked;
                ui.ctx().request_repaint();
            }
            if styled_button(ui, t!("context_menu.apply"), &app.cached_palette).clicked() {
                apply_clicked = true;
            }
            if styled_button(ui, t!("context_menu.help"), &app.cached_palette).clicked() {
                app.context_menu_help_open = true;
            }
        });
    });

    if app.context_menu_help_open {
        draw_context_menu_help_dialog(app, ctx);
    }

    if let Some(idx) = row_to_delete
        && idx < draft.items.len()
        && can_delete_context_menu_entry(&draft.items[idx])
    {
        draft.items.remove(idx);
        app.context_menu_selected_row = if draft.items.is_empty() {
            None
        } else {
            Some(idx.min(draft.items.len() - 1))
        };
        draft_changed = true;
    }

    if let Some(movement) = move_selected
        && let Some(idx) = app.context_menu_selected_row
        && idx < draft.items.len()
    {
        let new_idx = match movement {
            ContextMenuMove::Up => idx.saturating_sub(1),
            ContextMenuMove::Down => (idx + 1).min(draft.items.len() - 1),
            ContextMenuMove::Top => 0,
            ContextMenuMove::Bottom => draft.items.len() - 1,
        };
        if new_idx != idx {
            let entry = draft.items.remove(idx);
            draft.items.insert(new_idx, entry);
            app.context_menu_selected_row = Some(new_idx);
            app.context_menu_scroll_to_selected =
                context_menu_should_scroll_after_row_move(idx, new_idx);
            draft_changed = true;
        }
    }

    if app.context_menu_edit_dialog_open {
        edit_result = draw_context_menu_edit_dialog(app, ctx);
    }

    if let Some(entry) = edit_result {
        if let Some(idx) = app.context_menu_edit_target {
            if idx < draft.items.len() && draft.items[idx].kind != ContextMenuItemKind::Builtin {
                draft.items[idx] = entry;
                app.context_menu_selected_row = Some(idx);
            }
        } else {
            let insert_at = app
                .context_menu_selected_row
                .map(|idx| (idx + 1).min(draft.items.len()))
                .unwrap_or(draft.items.len());
            draft.items.insert(insert_at, entry);
            app.context_menu_selected_row = Some(insert_at);
        }
        app.context_menu_edit_dialog_open = false;
        app.context_menu_edit_target = None;
        app.context_menu_edit_draft = EditableContextMenuEntry::default();
        draft_changed = true;
    }

    let validated = crate::context_menu::rebuild_runtime_state(&draft);
    app.context_menu_draft_config = validated.config.clone();
    if draft_changed {
        app.context_menu_apply_success_at = None;
        app.context_menu_apply_error = None;
    }
    if apply_clicked {
        if crate::context_menu::validate::has_enabled_action_items(&draft.items) {
            app.context_menu_runtime = validated;
            app.context_menu_apply_success_at = Some(Instant::now());
            app.context_menu_apply_error = None;
            app.queue_context_menu_save();
        } else {
            app.context_menu_apply_success_at = None;
            app.context_menu_apply_error = Some(t!("context_menu.empty_block_apply").to_string());
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum ContextMenuMove {
    Up,
    Down,
    Top,
    Bottom,
}

#[derive(Debug, Clone, Copy)]
enum ContextMenuSelectionTarget {
    Home,
    End,
}

fn handle_context_menu_tab_shortcuts(
    app: &ImageViewerApp,
    ctx: &Context,
    row_to_delete: &mut Option<usize>,
    move_selected: &mut Option<ContextMenuMove>,
    selection_delta: &mut i32,
    selection_target: &mut Option<ContextMenuSelectionTarget>,
) {
    if app.settings_tab != SettingsTab::ContextMenu || app.context_menu_edit_dialog_open {
        return;
    }
    ctx.input(|i| {
        if i.key_pressed(egui::Key::W) {
            *move_selected = Some(ContextMenuMove::Up);
        } else if i.key_pressed(egui::Key::S) {
            *move_selected = Some(ContextMenuMove::Down);
        } else if i.key_pressed(egui::Key::A) {
            *move_selected = Some(ContextMenuMove::Top);
        } else if i.key_pressed(egui::Key::D) {
            *move_selected = Some(ContextMenuMove::Bottom);
        } else if i.key_pressed(egui::Key::Delete) {
            *row_to_delete = app.context_menu_selected_row;
        } else if i.key_pressed(egui::Key::ArrowUp) {
            *selection_delta = -1;
        } else if i.key_pressed(egui::Key::ArrowDown) {
            *selection_delta = 1;
        } else if i.key_pressed(egui::Key::Home) {
            *selection_target = Some(ContextMenuSelectionTarget::Home);
        } else if i.key_pressed(egui::Key::End) {
            *selection_target = Some(ContextMenuSelectionTarget::End);
        }
    });
}

fn context_menu_selection_after_arrow(
    selected: Option<usize>,
    row_count: usize,
    delta: i32,
) -> Option<usize> {
    if row_count == 0 {
        return None;
    }
    let Some(current) = selected.map(|idx| idx.min(row_count - 1)) else {
        return Some(0);
    };
    if delta < 0 {
        Some(current.saturating_sub(1))
    } else if delta > 0 {
        Some((current + 1).min(row_count - 1))
    } else {
        Some(current)
    }
}

fn context_menu_selection_home(row_count: usize) -> Option<usize> {
    (row_count > 0).then_some(0)
}

fn context_menu_selection_end(row_count: usize) -> Option<usize> {
    row_count.checked_sub(1)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ContextMenuDraftAction {
    draft_changed: bool,
    apply_clicked: bool,
}

fn context_menu_restore_defaults_action() -> ContextMenuDraftAction {
    ContextMenuDraftAction {
        draft_changed: true,
        apply_clicked: false,
    }
}

fn can_delete_context_menu_entry(entry: &ContextMenuEntry) -> bool {
    entry.kind != ContextMenuItemKind::Builtin
}

fn localized_context_menu_entry_label(entry: &ContextMenuEntry) -> String {
    match entry.kind {
        ContextMenuItemKind::Builtin => entry
            .builtin_id
            .as_deref()
            .and_then(builtin_descriptor)
            .map(|desc| t!(desc.label_key).to_string())
            .unwrap_or_else(|| t!("context_menu.unknown_builtin").to_string()),
        ContextMenuItemKind::Separator => t!("context_menu.separator").to_string(),
        ContextMenuItemKind::Custom => entry.label.clone(),
    }
}

fn editable_context_menu_entry_from_entry(entry: &ContextMenuEntry) -> EditableContextMenuEntry {
    match entry.kind {
        ContextMenuItemKind::Separator => EditableContextMenuEntry {
            kind: EditableContextMenuEntryKind::Separator,
            label: String::new(),
            command: ContextMenuCommand::Executable {
                path: String::new(),
            },
        },
        ContextMenuItemKind::Custom => EditableContextMenuEntry {
            kind: EditableContextMenuEntryKind::Custom,
            label: entry.label.clone(),
            command: entry
                .command
                .clone()
                .unwrap_or(ContextMenuCommand::Executable {
                    path: String::new(),
                }),
        },
        ContextMenuItemKind::Builtin => EditableContextMenuEntry::default(),
    }
}

fn draw_context_menu_edit_dialog(
    app: &mut ImageViewerApp,
    ctx: &Context,
) -> Option<ContextMenuEntry> {
    let mut result = None;
    let dialog_size = egui::vec2(520.0, 300.0);
    let min_content_width = 480.0;
    let browse_button_width = 96.0;
    let input_gap = 8.0;
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
    let title = if app.context_menu_edit_target.is_some() {
        t!("context_menu.modify")
    } else {
        t!("context_menu.add")
    };

    egui::Window::new(title)
        .collapsible(false)
        .resizable([true, true])
        .default_pos(default_pos)
        .default_size(dialog_size)
        .min_width(dialog_size.x)
        .min_height(dialog_size.y)
        .show(ctx, |ui| {
            let content_width = ui.available_width().max(min_content_width);
            ui.label(t!("context_menu.item_type"));
            ui.horizontal(|ui| {
                ui.radio_value(
                    &mut app.context_menu_edit_draft.kind,
                    EditableContextMenuEntryKind::Separator,
                    t!("context_menu.type_separator"),
                );
                ui.radio_value(
                    &mut app.context_menu_edit_draft.kind,
                    EditableContextMenuEntryKind::Custom,
                    t!("context_menu.type_custom"),
                );
            });

            let is_custom =
                app.context_menu_edit_draft.kind == EditableContextMenuEntryKind::Custom;
            ui.add_enabled_ui(is_custom, |ui| {
                ui.add_space(8.0);
                ui.label(t!("context_menu.action_name"));
                ui.add_sized(
                    [content_width, 24.0],
                    egui::TextEdit::singleline(&mut app.context_menu_edit_draft.label),
                );
                ui.add_space(8.0);
                ui.label(t!("context_menu.action_kind"));
                let mut use_exe = matches!(
                    app.context_menu_edit_draft.command,
                    ContextMenuCommand::Executable { .. }
                );
                ui.horizontal(|ui| {
                    if ui
                        .radio_value(&mut use_exe, true, t!("context_menu.action_executable"))
                        .clicked()
                    {
                        app.context_menu_edit_draft.command =
                            context_menu_command_after_action_kind_click(
                                app.context_menu_edit_draft.command.clone(),
                                true,
                            );
                    }
                    if ui
                        .radio_value(&mut use_exe, false, t!("context_menu.action_command_line"))
                        .clicked()
                    {
                        app.context_menu_edit_draft.command =
                            context_menu_command_after_action_kind_click(
                                app.context_menu_edit_draft.command.clone(),
                                false,
                            );
                    }
                });
                ui.add_space(6.0);
                match &mut app.context_menu_edit_draft.command {
                    ContextMenuCommand::Executable { path } => {
                        ui.label(t!("context_menu.exe_path"));
                        ui.horizontal(|ui| {
                            ui.add_sized(
                                [content_width - browse_button_width - input_gap, 24.0],
                                egui::TextEdit::singleline(path),
                            );
                            if ui
                                .add_sized(
                                    [browse_button_width, 30.0],
                                    styled_button_widget(
                                        t!("context_menu.browse"),
                                        &app.cached_palette,
                                    ),
                                )
                                .clicked()
                            {
                                app.context_menu_exe_browse_requested = true;
                            }
                        });
                    }
                    ContextMenuCommand::CommandLine { template } => {
                        ui.label(t!("context_menu.command_line"));
                        ui.add_sized(
                            [content_width, 24.0],
                            egui::TextEdit::singleline(&mut template.template),
                        );
                        ui.add(egui::Label::new(t!("context_menu.command_line_hint")).wrap());
                    }
                }
            });

            ui.add_space(12.0);
            ui.horizontal(|ui| {
                let valid =
                    context_menu_edit_draft_to_entry(&app.context_menu_edit_draft).is_some();
                if ui
                    .add_enabled(
                        valid,
                        styled_button_widget(t!("btn.ok"), &app.cached_palette),
                    )
                    .clicked()
                {
                    result = context_menu_edit_draft_to_entry(&app.context_menu_edit_draft);
                }
                if styled_button(ui, t!("btn.cancel"), &app.cached_palette).clicked() {
                    app.context_menu_edit_dialog_open = false;
                    app.context_menu_edit_target = None;
                    app.context_menu_edit_draft = EditableContextMenuEntry::default();
                }
            });
        });

    result
}

fn context_menu_edit_draft_to_entry(draft: &EditableContextMenuEntry) -> Option<ContextMenuEntry> {
    match draft.kind {
        EditableContextMenuEntryKind::Separator => Some(ContextMenuEntry::separator()),
        EditableContextMenuEntryKind::Custom => {
            let label = draft.label.trim();
            if label.is_empty() || !draft.command.is_valid() {
                return None;
            }
            Some(ContextMenuEntry {
                kind: ContextMenuItemKind::Custom,
                enabled: true,
                builtin_id: None,
                label: label.to_string(),
                command: Some(draft.command.clone()),
            })
        }
    }
}

fn draw_context_menu_help_dialog(app: &mut ImageViewerApp, ctx: &Context) {
    let mut open = app.context_menu_help_open;
    let mut close_clicked = false;
    let dialog_size = egui::vec2(430.0, 250.0);
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
    egui::Window::new(t!("context_menu.help_title"))
        .collapsible(false)
        .resizable(false)
        .default_pos(default_pos)
        .default_size(dialog_size)
        .open(&mut open)
        .show(ctx, |ui| {
            ui.set_min_width(390.0);
            ui.label(t!("context_menu.help_drag"));
            ui.add_space(8.0);
            context_menu_help_row(ui, "W", &t!("context_menu.help_move_up"));
            context_menu_help_row(ui, "S", &t!("context_menu.help_move_down"));
            context_menu_help_row(ui, "A", &t!("context_menu.help_move_top"));
            context_menu_help_row(ui, "D", &t!("context_menu.help_move_bottom"));
            context_menu_help_row(ui, "Arrow Up / Down", &t!("context_menu.help_select_rows"));
            context_menu_help_row(ui, "Home / End", &t!("context_menu.help_select_ends"));
            context_menu_help_row(ui, "Del", &t!("context_menu.help_delete"));
            ui.add_space(12.0);
            if styled_button(ui, t!("btn.close"), &app.cached_palette).clicked() {
                close_clicked = true;
            }
        });
    app.context_menu_help_open = open && !close_clicked;
}

fn context_menu_help_row(ui: &mut egui::Ui, key: &str, description: &str) {
    ui.horizontal(|ui| {
        ui.add_sized([120.0, 20.0], egui::Label::new(RichText::new(key).strong()));
        ui.label(description);
    });
}

fn context_menu_command_after_action_kind_click(
    command: ContextMenuCommand,
    clicked_executable: bool,
) -> ContextMenuCommand {
    match (&command, clicked_executable) {
        (ContextMenuCommand::Executable { .. }, true)
        | (ContextMenuCommand::CommandLine { .. }, false) => command,
        (_, true) => ContextMenuCommand::Executable {
            path: String::new(),
        },
        (_, false) => ContextMenuCommand::CommandLine {
            template: crate::context_menu::model::CommandTemplate::new(String::new()),
        },
    }
}

fn context_menu_should_scroll_after_row_move(old_idx: usize, new_idx: usize) -> bool {
    old_idx != new_idx
}

#[cfg(test)]
mod context_menu_settings_tests {
    use super::*;

    #[test]
    fn arrow_navigation_moves_selection_without_reordering() {
        assert_eq!(context_menu_selection_after_arrow(Some(2), 5, -1), Some(1));
        assert_eq!(context_menu_selection_after_arrow(Some(2), 5, 1), Some(3));
        assert_eq!(context_menu_selection_after_arrow(Some(0), 5, -1), Some(0));
        assert_eq!(context_menu_selection_after_arrow(Some(4), 5, 1), Some(4));
        assert_eq!(context_menu_selection_after_arrow(None, 5, 1), Some(0));
        assert_eq!(context_menu_selection_after_arrow(None, 0, 1), None);
    }

    #[test]
    fn home_end_navigation_selects_first_and_last_row() {
        assert_eq!(context_menu_selection_home(5), Some(0));
        assert_eq!(context_menu_selection_home(0), None);
        assert_eq!(context_menu_selection_end(5), Some(4));
        assert_eq!(context_menu_selection_end(0), None);
    }

    #[test]
    fn restore_defaults_changes_draft_without_applying() {
        assert_eq!(
            context_menu_restore_defaults_action(),
            ContextMenuDraftAction {
                draft_changed: true,
                apply_clicked: false,
            }
        );
    }

    #[test]
    fn clicking_current_action_kind_preserves_command_text() {
        let exe = ContextMenuCommand::Executable {
            path: "C:/Program Files/App/App.exe".to_string(),
        };
        assert_eq!(
            context_menu_command_after_action_kind_click(exe.clone(), true),
            exe
        );

        let command = ContextMenuCommand::CommandLine {
            template: crate::context_menu::model::CommandTemplate::new(
                "\"C:/Program Files/App/App.exe\" \"%1\"".to_string(),
            ),
        };
        assert_eq!(
            context_menu_command_after_action_kind_click(command.clone(), false),
            command
        );

        assert!(matches!(
            context_menu_command_after_action_kind_click(command.clone(), true),
            ContextMenuCommand::Executable { path } if path.is_empty()
        ));
        assert!(matches!(
            context_menu_command_after_action_kind_click(exe, false),
            ContextMenuCommand::CommandLine { template } if template.template.is_empty()
        ));
    }

    #[test]
    fn moving_selected_row_requests_scroll_to_new_focus_row() {
        assert!(context_menu_should_scroll_after_row_move(8, 0));
        assert!(context_menu_should_scroll_after_row_move(8, 9));
        assert!(!context_menu_should_scroll_after_row_move(8, 8));
    }
}
