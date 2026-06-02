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

use crate::app::{ImageViewerApp, ScaleMode, SettingsTab, TransitionStyle};
use crate::hotkeys::model::{
    HotkeyActionId, KeyChord, action_id_to_str, default_hotkey_config_file,
};
use crate::ui::utils::{
    middle_truncate, path_display_box, settings_card, styled_button, styled_button_widget,
    themed_labeled_toggle,
};
use eframe::Frame;
use eframe::egui::{self, Color32, Context, Margin, Pos2, RichText, Vec2};
use rust_i18n::t;
use std::time::Instant;

const ABOUT_ICON_SIZE: f32 = 96.0;
const ABOUT_ICON_BYTES: &[u8] = include_bytes!("../../assets/icon.png");
const HDR_SLIDER_VALUE_WIDTH: f32 = 90.0;
const TRANSITIONS_SLIDER_VALUE_WIDTH: f32 = 72.0;
const MUSIC_SLIDER_VALUE_WIDTH: f32 = 60.0;
const SETTINGS_TAB_SIDEBAR_WIDTH: f32 = 124.0;
const SETTINGS_TAB_ITEM_HEIGHT: f32 = 34.0;
const HOTKEYS_INDEX_COL_WIDTH: f32 = 48.0;
const HOTKEYS_ACTION_COL_WIDTH: f32 = 230.0;
const HOTKEYS_KEY_COL_WIDTH: f32 = 320.0;
pub fn draw(app: &mut ImageViewerApp, ctx: &Context, frame: &Frame) {
    // [Point 19] Explanatory Comments:
    // The settings layout uses nested UI elements to achieve responsive alignment.
    // Specifically, path_display_box and certain groupings require fixed widths or
    // specific layout directions to prevent the egui window from collapsing or
    // expanding unexpectedly when content changes (e.g., long paths or slider movements).

    // Detect when settings panel is first opened in this session to refresh audio devices.
    if !app.last_show_settings {
        app.refresh_audio_devices();
    }
    app.last_show_settings = true;

    let mut open_dir = false;
    let mut open_music_file = false;
    let mut open_music_dir = false;
    let mut fullscreen_changed = false;
    let mut music_enabled_changed = false;

    egui::Window::new(t!("app.window_title"))
        .id(egui::Id::new("settings_window"))
        .default_pos({
            let screen = ctx.content_rect();
            let win_w = crate::constants::SETTINGS_WINDOW_DEFAULT_WIDTH;
            let win_h = crate::constants::SETTINGS_WINDOW_DEFAULT_HEIGHT;
            Pos2::new(
                (screen.center().x - win_w / 2.0).max(0.0),
                (screen.center().y - win_h / 2.0).max(0.0),
            )
        })
        .resizable([true, true])
        .collapsible(true)
        .frame(
            egui::Frame::window(&ctx.global_style())
                .fill(app.cached_palette.panel_bg)
                .shadow(egui::epaint::Shadow::NONE),
        )
        .min_width(crate::constants::SETTINGS_WINDOW_MIN_WIDTH)
        .default_width(crate::constants::SETTINGS_WINDOW_DEFAULT_WIDTH)
        .max_width(crate::constants::SETTINGS_WINDOW_MAX_WIDTH)
        .default_height(crate::constants::SETTINGS_WINDOW_DEFAULT_HEIGHT)
        .min_height(crate::constants::SETTINGS_WINDOW_MIN_HEIGHT)
        .default_size(egui::vec2(
            crate::constants::SETTINGS_WINDOW_DEFAULT_WIDTH,
            crate::constants::SETTINGS_WINDOW_DEFAULT_HEIGHT,
        ))
        .min_size(egui::vec2(
            crate::constants::SETTINGS_WINDOW_MIN_WIDTH,
            crate::constants::SETTINGS_WINDOW_MIN_HEIGHT,
        ))
        .show(ctx, |ui| {
            ui.visuals_mut().override_text_color = Some(app.cached_palette.text_normal);
            // Pin inner width: min prevents shrink when switching to sparse tabs; max prevents
            // wide tabs (e.g. Viewing) from growing [`Resize::desired_size`].
            ui.set_min_width(crate::constants::SETTINGS_WINDOW_DEFAULT_WIDTH);
            ui.set_max_width(ui.max_rect().width());
            draw_settings_body(
                app,
                ui,
                ctx,
                &mut open_dir,
                &mut fullscreen_changed,
                &mut open_music_file,
                &mut open_music_dir,
                &mut music_enabled_changed,
            );
        });

    if open_dir {
        app.open_directory_dialog(frame);
    }
    if open_music_file {
        app.open_music_file_dialog(frame);
    }
    if open_music_dir {
        app.open_music_dir_dialog(frame);
    }
    if fullscreen_changed {
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(app.settings.fullscreen));
    }
    if music_enabled_changed {
        if app.settings.play_music {
            app.restart_audio_if_enabled();
        } else {
            app.audio.stop();
            // Clear scan state so re-enabling triggers a fresh scan+play
            // (otherwise restart_audio_if_enabled short-circuits on the
            //  "already scanned this path" guard)
            app.cached_music_count = None;
            app.music_scan_path = None;
        }
        app.queue_save();
    }
}

fn draw_slideshow_section(app: &mut ImageViewerApp, ui: &mut egui::Ui) {
    let palette = app.cached_palette.clone();
    settings_card(ui, &palette, t!("section.slideshow"), |ui| {
        let old_auto_switch = app.settings.auto_switch;
        if themed_labeled_toggle(
            ui,
            &mut app.settings.auto_switch,
            t!("label.auto_advance"),
            &palette,
        )
        .changed()
        {
            app.slideshow_paused = false;
        }
        if app.settings.auto_switch {
            ui.horizontal(|ui| {
                ui.label(t!("label.interval_sec"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add(
                        egui::DragValue::new(&mut app.settings.auto_switch_interval)
                            .range(0.5..=3600.0)
                            .speed(0.5),
                    );
                });
            });
            if themed_labeled_toggle(
                ui,
                &mut app.settings.random_slideshow_order,
                t!("label.random_slideshow_order"),
                &palette,
            )
            .changed()
            {
                app.invalidate_random_slideshow_order();
                app.queue_save();
            }
        }
        if old_auto_switch != app.settings.auto_switch {
            app.invalidate_random_slideshow_order();
            app.queue_save();
        }
    });
}

fn draw_hdr_section(app: &mut ImageViewerApp, ui: &mut egui::Ui) {
    let palette = app.cached_palette.clone();
    settings_card(ui, &palette, t!("section.hdr"), |ui| {
        if themed_labeled_toggle(
            ui,
            &mut app.settings.hdr_native_surface_enabled,
            t!("hdr.native_surface_enabled"),
            &palette,
        )
        .on_hover_text(t!("hdr.native_surface_restart_hint"))
        .changed()
        {
            app.queue_save();
        }

        let render_mode = crate::hdr::monitor::effective_render_output_mode(
            app.hdr_target_format,
            app.effective_hdr_monitor_selection().as_ref(),
        );
        let is_tone_mapped_sdr_output = matches!(
            render_mode,
            crate::hdr::renderer::HdrRenderOutputMode::SdrToneMapped
        );

        let old = (
            (
                app.settings.hdr_exposure_ev_native,
                app.settings.hdr_exposure_ev_sdr,
            ),
            app.settings.hdr_sdr_white_nits,
            app.settings.hdr_max_display_nits,
        );

        let exposure_slot = if is_tone_mapped_sdr_output {
            &mut app.settings.hdr_exposure_ev_sdr
        } else {
            &mut app.settings.hdr_exposure_ev_native
        };
        let hint = if is_tone_mapped_sdr_output {
            t!("hdr.exposure_hint_when_sdr_mapped_output")
        } else {
            t!("hdr.exposure_hint_when_native_hdr_output")
        };
        egui::Grid::new("hdr_settings_grid")
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                let item_spacing = ui.spacing().item_spacing.x;

                ui.label(t!("hdr.exposure_ev"));
                let track_w =
                    (ui.available_width() - HDR_SLIDER_VALUE_WIDTH - item_spacing).max(40.0);
                add_fixed_slider(
                    ui,
                    track_w,
                    HDR_SLIDER_VALUE_WIDTH,
                    egui::Slider::new(exposure_slot, -8.0..=8.0)
                        .step_by(0.1)
                        .suffix(" EV"),
                )
                .on_hover_text(hint);
                ui.end_row();

                ui.label(t!("hdr.sdr_white_nits"));
                let track_w =
                    (ui.available_width() - HDR_SLIDER_VALUE_WIDTH - item_spacing).max(40.0);
                add_fixed_slider(
                    ui,
                    track_w,
                    HDR_SLIDER_VALUE_WIDTH,
                    egui::Slider::new(&mut app.settings.hdr_sdr_white_nits, 80.0..=400.0)
                        .step_by(1.0)
                        .suffix(" nits"),
                )
                .on_hover_text(t!("hdr.sdr_white_hint"));
                ui.end_row();

                ui.label(t!("hdr.max_display_nits"));
                let track_w =
                    (ui.available_width() - HDR_SLIDER_VALUE_WIDTH - item_spacing).max(40.0);
                add_fixed_slider(
                    ui,
                    track_w,
                    HDR_SLIDER_VALUE_WIDTH,
                    egui::Slider::new(&mut app.settings.hdr_max_display_nits, 100.0..=10_000.0)
                        .logarithmic(true)
                        .suffix(" nits"),
                )
                .on_hover_text(t!("hdr.max_display_hint"));
                ui.end_row();
            });

        if old
            != (
                (
                    app.settings.hdr_exposure_ev_native,
                    app.settings.hdr_exposure_ev_sdr,
                ),
                app.settings.hdr_sdr_white_nits,
                app.settings.hdr_max_display_nits,
            )
        {
            app.settings.hdr_max_display_nits = app
                .settings
                .hdr_max_display_nits
                .max(app.settings.hdr_sdr_white_nits);
            app.hdr_renderer.tone_map = app.effective_hdr_tone_map_settings();
            app.loader
                .set_hdr_tone_map_settings(app.effective_hdr_tone_map_settings());
            app.refresh_ultra_hdr_decode_capacity(ui.ctx());
            app.queue_save();
            ui.ctx().request_repaint();
        }
    });
}

fn draw_settings_body(
    app: &mut ImageViewerApp,
    ui: &mut egui::Ui,
    ctx: &Context,
    open_dir: &mut bool,
    fullscreen_changed: &mut bool,
    open_music_file: &mut bool,
    open_music_dir: &mut bool,
    music_enabled_changed: &mut bool,
) {
    // Use allocate_exact_size so the Resize widget records exactly (body_w × body_h) as
    // content_size — tab content's min_rect (which may be wider, e.g. Viewing two-column
    // grid) never propagates to the Window's Resize and cannot widen the dialog.
    let body_h = ui.available_height();
    let body_w = ui.available_width();
    let body_rect = {
        let (r, _) = ui.allocate_exact_size(egui::vec2(body_w, body_h), egui::Sense::hover());
        r
    };
    let mut body_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(body_rect)
            .layout(egui::Layout::left_to_right(egui::Align::Min)),
    );

    let row_h = body_ui.max_rect().height();

    // Sidebar — also exact so it can't force horizontal growth.
    let sidebar_rect = {
        let (r, _) = body_ui.allocate_exact_size(
            egui::vec2(SETTINGS_TAB_SIDEBAR_WIDTH, row_h),
            egui::Sense::hover(),
        );
        r
    };
    let mut sidebar_ui = body_ui.new_child(
        egui::UiBuilder::new()
            .max_rect(sidebar_rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    sidebar_ui.set_min_height(row_h);
    draw_settings_tabs(app, &mut sidebar_ui);

    body_ui.separator();

    // Content column — pin exact size so tab content overflows inside, not outward.
    let content_w = body_ui.available_width();
    let content_rect = {
        let (r, _) =
            body_ui.allocate_exact_size(egui::vec2(content_w, row_h), egui::Sense::hover());
        r
    };
    let mut content_ui = body_ui.new_child(
        egui::UiBuilder::new()
            .max_rect(content_rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    // Clip painting to content_rect: prevents wide widgets (sliders, ComboBoxes) from
    // rendering over the sidebar when the window is dragged narrower than card width.
    content_ui.set_clip_rect(content_rect.intersect(body_ui.clip_rect()));
    content_ui.set_min_height(row_h);

    let sparse_tab = matches!(app.settings_tab, SettingsTab::About | SettingsTab::System);
    if sparse_tab {
        draw_active_settings_tab(
            app,
            &mut content_ui,
            ctx,
            open_dir,
            fullscreen_changed,
            open_music_file,
            open_music_dir,
            music_enabled_changed,
        );
    } else {
        egui::ScrollArea::vertical()
            .id_salt(("settings_tab_content", app.settings_tab.label_key()))
            .auto_shrink([false, false])
            .show(&mut content_ui, |ui| {
                draw_active_settings_tab(
                    app,
                    ui,
                    ctx,
                    open_dir,
                    fullscreen_changed,
                    open_music_file,
                    open_music_dir,
                    music_enabled_changed,
                );
            });
    }
}

fn draw_settings_tabs(app: &mut ImageViewerApp, ui: &mut egui::Ui) {
    ui.vertical(|ui| {
        for tab in SettingsTab::ALL {
            let selected = app.settings_tab == tab;
            if ui
                .add_sized(
                    [ui.available_width(), SETTINGS_TAB_ITEM_HEIGHT],
                    egui::Button::selectable(selected, t!(tab.label_key()).to_string()),
                )
                .clicked()
            {
                app.settings_tab = tab;
            }
            ui.add_space(2.0);
        }
    });
}

fn add_fixed_slider(
    ui: &mut egui::Ui,
    track_width: f32,
    value_width: f32,
    slider: egui::Slider<'_>,
) -> egui::Response {
    ui.scope(|ui| {
        ui.spacing_mut().slider_width = track_width;
        ui.spacing_mut().interact_size.x = value_width;
        ui.add(slider)
    })
    .inner
}

fn grid_label(ui: &mut egui::Ui, text: impl Into<egui::WidgetText>) {
    ui.allocate_ui_with_layout(
        egui::vec2(0.0, ui.spacing().interact_size.y),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.label(text);
        },
    );
}

fn draw_active_settings_tab(
    app: &mut ImageViewerApp,
    ui: &mut egui::Ui,
    ctx: &Context,
    open_dir: &mut bool,
    fullscreen_changed: &mut bool,
    open_music_file: &mut bool,
    open_music_dir: &mut bool,
    music_enabled_changed: &mut bool,
) {
    match app.settings_tab {
        SettingsTab::Library => draw_library_tab(app, ui, open_dir),
        SettingsTab::Viewing => draw_viewing_tab(app, ui, fullscreen_changed),
        SettingsTab::Slideshow => draw_slideshow_tab(app, ui),
        SettingsTab::Music => draw_music_tab(
            app,
            ui,
            open_music_file,
            open_music_dir,
            music_enabled_changed,
        ),
        SettingsTab::Appearance => crate::ui::settings_appearance::draw(app, ui, ctx),
        SettingsTab::Hotkeys => draw_hotkeys_tab(app, ui, ctx),
        #[cfg(target_os = "windows")]
        SettingsTab::System => draw_system_tab(app, ui),
        #[cfg(not(target_os = "windows"))]
        SettingsTab::System => {}
        SettingsTab::About => draw_about_tab(app, ui),
    }
}

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

fn draw_hotkeys_tab_status(
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
                RichText::new(t!("hotkeys.capture_hint")).color(app.cached_palette.text_muted),
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

fn draw_hotkeys_tab(app: &mut ImageViewerApp, ui: &mut egui::Ui, ctx: &Context) {
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
        let capture_pending =
            app.hotkeys_capture_target.is_some() || set_key_target.is_some();
        let apply_success = app.hotkeys_apply_success_at.is_some();
        let status_rows = apply_success as usize
            + capture_pending as usize
            + app.hotkeys_load_error.is_some() as usize
            + has_empty_key as usize
            + (!preview.conflicts.is_empty()) as usize
            + preview.conflicts.len()
            + preview.warnings.len();
        let status_h = if status_rows > 0 {
            8.0 + 20.0 * status_rows.min(6) as f32
        } else {
            0.0
        };
        draw_hotkeys_tab_status(
            app,
            ui,
            &preview,
            has_empty_key,
            capture_pending,
            apply_success,
        );
        let footer_h = 54.0;
        let available_h = (ui.available_height() - footer_h - status_h).max(80.0);
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), available_h),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
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
                                let header_font = egui::TextStyle::Body.resolve(ui.style());
                                let header_color = ui.visuals().text_color();
                                let draw_header = |ui: &mut egui::Ui, width: f32, text: &str| {
                                    let (rect, _) = ui.allocate_exact_size(
                                        egui::vec2(width, 18.0),
                                        egui::Sense::hover(),
                                    );
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
                                    HOTKEYS_INDEX_COL_WIDTH,
                                    &t!("hotkeys.column_index"),
                                );
                                draw_header(
                                    ui,
                                    HOTKEYS_ACTION_COL_WIDTH,
                                    &t!("hotkeys.column_action"),
                                );
                                draw_header(ui, HOTKEYS_KEY_COL_WIDTH, &t!("hotkeys.column_key"));
                                ui.end_row();

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
                ui.add(
                    egui::Label::new(RichText::new(key_label).strong()).wrap(),
                );
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
                                .color(app.cached_palette.text_muted),
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
        #[cfg(not(target_os = "windows"))]
        HotkeyActionId::Quit => "hotkeys.action.quit_app",
        HotkeyActionId::ExitFullscreen => "hotkeys.action.exit_fullscreen",
    };
    t!(key).to_string()
}

fn draw_library_tab(app: &mut ImageViewerApp, ui: &mut egui::Ui, open_dir: &mut bool) {
    ui.vertical(|ui| {
        draw_library_controls(app, ui, open_dir);
    });
}

fn draw_library_controls(app: &mut ImageViewerApp, ui: &mut egui::Ui, open_dir: &mut bool) {
    let palette = app.cached_palette.clone();
    settings_card(ui, &palette, t!("section.directory"), |ui| {
        let dir_full = app
            .settings
            .last_image_dir
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned());
        let dir_empty = app.settings.last_image_dir.is_none();
        let dir_label = if dir_empty {
            t!("label.no_dir").to_string()
        } else {
            dir_full.clone().unwrap_or_default()
        };
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if styled_button(ui, t!("btn.pick"), &palette).clicked() {
                    *open_dir = true;
                }
                ui.add_space(4.0);
                if styled_button(ui, t!("btn.refresh"), &palette).clicked() {
                    if let Some(dir) = app.settings.last_image_dir.clone() {
                        app.load_directory(dir);
                    }
                }

                let box_w = (ui.available_width() - 16.0).max(20.0);
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    let resp = path_display_box(ui, &dir_label, dir_empty, box_w, &palette);
                    if let Some(full) = &dir_full {
                        resp.on_hover_text(full.as_str());
                    }
                });
            });
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(t!("library.images")).color(palette.text_muted));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(app.image_files.len().to_string());
            });
        });

        let scan_status = if app.scanning {
            app.status_message.clone()
        } else if app.settings.last_image_dir.is_some() {
            t!("library.scan_idle").to_string()
        } else {
            t!("library.scan_no_directory").to_string()
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new(t!("library.scan_status")).color(palette.text_muted));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.horizontal(|ui| {
                    if app.scanning {
                        ui.spinner();
                    }
                    ui.label(scan_status);
                });
            });
        });

        ui.add_space(4.0);
        let old_recursive = app.settings.recursive;
        themed_labeled_toggle(
            ui,
            &mut app.settings.recursive,
            t!("label.recursive_scan"),
            &palette,
        );
        if !old_recursive && app.settings.recursive {
            app.settings.recursive = false;
            app.active_modal = Some(crate::ui::dialogs::modal_state::ActiveModal::Confirm(
                crate::ui::dialogs::confirm::State::recursive_scan(
                    t!("win.confirm_recursive_title").to_string(),
                    t!("win.confirm_recursive_msg").to_string(),
                ),
            ));
        }
        if old_recursive && !app.settings.recursive {
            if let Some(dir) = app.settings.last_image_dir.clone() {
                app.load_directory(dir);
            }
            app.queue_save();
        }

        if themed_labeled_toggle(
            ui,
            &mut app.settings.preload,
            t!("label.enable_preload"),
            &palette,
        )
        .changed()
        {
            app.queue_save();
        }

        if themed_labeled_toggle(
            ui,
            &mut app.settings.resume_last_image,
            t!("label.resume_last"),
            &palette,
        )
        .changed()
        {
            app.queue_save();
        }
    });
}

fn draw_viewing_tab(app: &mut ImageViewerApp, ui: &mut egui::Ui, fullscreen_changed: &mut bool) {
    let palette = app.cached_palette.clone();
    settings_card(ui, &palette, t!("section.display"), |ui| {
        let old_fullscreen = app.settings.fullscreen;
        themed_labeled_toggle(
            ui,
            &mut app.settings.fullscreen,
            t!("label.fullscreen"),
            &palette,
        );
        if old_fullscreen != app.settings.fullscreen {
            *fullscreen_changed = true;
        }

        ui.add_space(6.0);
        // Scale Mode: label left, ComboBox right-aligned (mirrors toggle layout).
        // Z key (ToggleScaleMode action) cycles through variants; the ComboBox reflects
        // the current value automatically each frame — no extra sync needed.
        ui.horizontal(|ui| {
            ui.label(t!("label.scale_mode"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let old_scale = app.settings.scale_mode;
                let selected_text = match app.settings.scale_mode {
                    ScaleMode::FitToWindow => t!("scale.fit").to_string(),
                    ScaleMode::OriginalSize => t!("scale.original").to_string(),
                };
                egui::ComboBox::from_id_salt("scale_mode_combo")
                    .selected_text(selected_text)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut app.settings.scale_mode,
                            ScaleMode::FitToWindow,
                            t!("scale.fit").to_string(),
                        );
                        ui.selectable_value(
                            &mut app.settings.scale_mode,
                            ScaleMode::OriginalSize,
                            t!("scale.original").to_string(),
                        );
                    });
                if old_scale != app.settings.scale_mode {
                    app.zoom_factor = 1.0;
                    app.pan_offset = Vec2::ZERO;
                    app.queue_save();
                }
            });
        });
        ui.add_space(4.0);
        ui.label(RichText::new(t!("label.z_toggle_hint")).color(palette.text_muted));

        ui.add_space(8.0);
        if themed_labeled_toggle(
            ui,
            &mut app.settings.show_osd,
            t!("label.show_osd"),
            &palette,
        )
        .changed()
        {
            app.queue_save();
        }
        if themed_labeled_toggle(
            ui,
            &mut app.settings.raw_high_quality,
            t!("label.raw_high_quality"),
            &palette,
        )
        .on_hover_text(t!("hint.raw_high_quality"))
        .changed()
        {
            app.reload_current();
            app.queue_save();
        }
    });

    ui.add_space(8.0);
    draw_hdr_settings_if_available(app, ui);
}

fn draw_slideshow_tab(app: &mut ImageViewerApp, ui: &mut egui::Ui) {
    draw_slideshow_section(app, ui);
    ui.add_space(8.0);
    draw_transitions_section(app, ui);
}

fn draw_transitions_section(app: &mut ImageViewerApp, ui: &mut egui::Ui) {
    let palette = app.cached_palette.clone();
    settings_card(ui, &palette, t!("section.transitions"), |ui| {
        let bp = ui.spacing().button_padding;
        let control_h = ui.text_style_height(&egui::TextStyle::Body) + 2.0 * bp.y;
        ui.style_mut().spacing.interact_size.y = control_h;

        // Grid: col-0 = labels (left-aligned, uniform width), col-1 = controls (fill to right edge).
        let old_style = app.settings.transition_style;
        let old_ms = app.settings.transition_ms;
        egui::Grid::new("transitions_grid")
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                ui.label(t!("label.style"));
                let avail = ui.available_width();
                egui::ComboBox::from_id_salt("transition_style")
                    .selected_text(app.settings.transition_style.label())
                    .width(avail)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut app.settings.transition_style,
                            TransitionStyle::None,
                            TransitionStyle::None.label(),
                        );
                        ui.selectable_value(
                            &mut app.settings.transition_style,
                            TransitionStyle::Fade,
                            TransitionStyle::Fade.label(),
                        );
                        ui.selectable_value(
                            &mut app.settings.transition_style,
                            TransitionStyle::ZoomFade,
                            TransitionStyle::ZoomFade.label(),
                        );
                        ui.selectable_value(
                            &mut app.settings.transition_style,
                            TransitionStyle::Slide,
                            TransitionStyle::Slide.label(),
                        );
                        ui.selectable_value(
                            &mut app.settings.transition_style,
                            TransitionStyle::Push,
                            TransitionStyle::Push.label(),
                        );
                        ui.selectable_value(
                            &mut app.settings.transition_style,
                            TransitionStyle::PageFlip,
                            TransitionStyle::PageFlip.label(),
                        );
                        ui.selectable_value(
                            &mut app.settings.transition_style,
                            TransitionStyle::Ripple,
                            TransitionStyle::Ripple.label(),
                        );
                        ui.selectable_value(
                            &mut app.settings.transition_style,
                            TransitionStyle::Curtain,
                            TransitionStyle::Curtain.label(),
                        );
                        ui.selectable_value(
                            &mut app.settings.transition_style,
                            TransitionStyle::Random,
                            TransitionStyle::Random.label(),
                        );
                    });
                ui.end_row();

                if app.settings.transition_style != TransitionStyle::None {
                    ui.label(t!("label.duration"));
                    let item_spacing = ui.spacing().item_spacing.x;
                    let avail = ui.available_width();
                    let value_w = TRANSITIONS_SLIDER_VALUE_WIDTH;
                    let track_w = (avail - value_w - item_spacing).max(40.0);
                    add_fixed_slider(
                        ui,
                        track_w,
                        value_w,
                        egui::Slider::new(&mut app.settings.transition_ms, 50..=2000).suffix("ms"),
                    );
                    ui.end_row();
                }
            });
        if old_style != app.settings.transition_style || old_ms != app.settings.transition_ms {
            app.queue_save();
        }
    });
}

fn draw_music_tab(
    app: &mut ImageViewerApp,
    ui: &mut egui::Ui,
    open_music_file: &mut bool,
    open_music_dir: &mut bool,
    music_enabled_changed: &mut bool,
) {
    ui.vertical(|ui| {
        let palette = app.cached_palette.clone();
        settings_card(ui, &palette, t!("section.music"), |ui| {
            let old_play_music = app.settings.play_music;
            let old_show_music_osd = app.settings.show_music_osd;

            themed_labeled_toggle(
                ui,
                &mut app.settings.play_music,
                t!("label.play_music"),
                &palette,
            );
            if app.settings.play_music {
                themed_labeled_toggle(
                    ui,
                    &mut app.settings.show_music_osd,
                    t!("label.show_music_osd"),
                    &palette,
                );
            }

            if old_play_music != app.settings.play_music
                || old_show_music_osd != app.settings.show_music_osd
            {
                if old_play_music != app.settings.play_music {
                    *music_enabled_changed = true;
                }
                app.music_hud_last_activity = Instant::now();
                app.queue_save();
            }

            if app.settings.play_music {
                ui.add_space(2.0);
                let music_full = app
                    .settings
                    .music_path
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned());
                let music_empty = app.settings.music_path.is_none();
                let music_label = if music_empty {
                    t!("label.no_music").to_string()
                } else {
                    music_full.clone().unwrap_or_default()
                };
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if styled_button(ui, t!("btn.pick_dir"), &palette).clicked() {
                            *open_music_dir = true;
                        }
                        ui.add_space(4.0);
                        if styled_button(ui, t!("btn.pick_file"), &palette).clicked() {
                            *open_music_file = true;
                        }
                        let box_w = (ui.available_width() - 16.0).max(20.0);
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            let resp =
                                path_display_box(ui, &music_label, music_empty, box_w, &palette);
                            if let Some(full) = &music_full {
                                resp.on_hover_text(full.as_str());
                            }
                        });
                    });
                });
                if app.settings.music_path.is_some() {
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        if app.scanning_music {
                            ui.spinner();
                            ui.label(RichText::new(t!("music.scanning")).color(palette.text_muted));
                        } else if let Some(count) = app.cached_music_count {
                            if count > 0 {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(t!(
                                            "music.files_ready",
                                            count = count.to_string()
                                        ))
                                        .color(palette.accent2),
                                    )
                                    .truncate(),
                                );

                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.spacing_mut().item_spacing.x = 4.0;
                                        let has_tracks = app.audio.has_tracks();

                                        if styled_button(ui, "⏭", &app.cached_palette)
                                            .on_hover_text(t!("music.next_file"))
                                            .clicked()
                                        {
                                            app.audio.next_file();
                                            app.music_hud_last_activity = Instant::now();
                                        }
                                        let resp = ui.add_enabled(
                                            has_tracks,
                                            styled_button_widget("⏩", &app.cached_palette),
                                        );
                                        if resp.on_hover_text(t!("music.next_track")).clicked() {
                                            app.audio.next_track();
                                            app.music_hud_last_activity = Instant::now();
                                        }
                                        let play_icon = if app.settings.music_paused {
                                            "▶"
                                        } else {
                                            "⏸"
                                        };
                                        if styled_button(ui, play_icon, &app.cached_palette)
                                            .on_hover_text(t!("music.play_pause"))
                                            .clicked()
                                        {
                                            app.settings.music_paused = !app.settings.music_paused;
                                            if app.settings.music_paused {
                                                app.audio.pause();
                                            } else {
                                                app.audio.play();
                                            }
                                            app.queue_save();
                                            app.music_hud_last_activity = Instant::now();
                                        }
                                        let resp = ui.add_enabled(
                                            has_tracks,
                                            styled_button_widget("⏪", &app.cached_palette),
                                        );
                                        if resp.on_hover_text(t!("music.prev_track")).clicked() {
                                            app.audio.prev_track();
                                            app.music_hud_last_activity = Instant::now();
                                        }
                                        if styled_button(ui, "⏮", &app.cached_palette)
                                            .on_hover_text(t!("music.prev_file"))
                                            .clicked()
                                        {
                                            app.audio.prev_file();
                                            app.music_hud_last_activity = Instant::now();
                                        }
                                    },
                                );
                            } else {
                                ui.label(
                                    RichText::new(t!("music.no_audio"))
                                        .color(Color32::from_rgb(255, 180, 60)),
                                );
                            }
                        }
                    });
                }
            }
        });

        if app.settings.play_music {
            let filename = app.audio.get_current_track();
            let metadata = app.audio.get_metadata();

            ui.add_space(8.0);
            settings_card(ui, &palette, t!("section.music_playback"), |ui| {
                if let Some(f) = filename {
                    ui.horizontal(|ui| {
                        let status = if app.settings.music_paused {
                            t!("music.paused").to_string()
                        } else {
                            t!("music.playing").to_string()
                        };
                        ui.label(RichText::new(status).color(app.cached_palette.text_muted));
                        let short_f = middle_truncate(&f, 40);
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!("[{short_f}]"))
                                    .color(app.cached_palette.text_muted),
                            )
                            .truncate(),
                        )
                        .on_hover_text(&f);
                    });
                    if let Some(m) = metadata {
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!("  │  {m}"))
                                    .color(app.cached_palette.accent2)
                                    .italics(),
                            )
                            .truncate(),
                        );
                    }
                    ui.add_space(2.0);
                    let mut cur_ms = app.audio.get_pos_ms();
                    let tot_ms = app.audio.get_duration_ms();

                    if let Some(target_ms) = app.music_seeking_target_ms {
                        let diff = (cur_ms as i64 - target_ms as i64).abs();
                        let timed_out = app
                            .music_seek_timeout
                            .map_or(false, |t| t.elapsed().as_secs() >= 30);
                        if diff < 2000 || timed_out {
                            app.music_seeking_target_ms = None;
                            app.music_seek_timeout = None;
                        } else {
                            cur_ms = target_ms;
                        }
                    }

                    if tot_ms > 0 {
                        let mut pos_s = cur_ms as f32 / 1000.0;
                        let total_s = tot_ms as f32 / 1000.0;

                        ui.horizontal(|ui| {
                            ui.spacing_mut().interact_size.x = 6.0;

                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    // Right timestamp placed first (rightmost)
                                    ui.label(
                                        RichText::new(format!(
                                            "{:02}:{:02}",
                                            (total_s as u32) / 60,
                                            (total_s as u32) % 60
                                        ))
                                        .color(app.cached_palette.text_muted),
                                    );

                                    // Switch to LTR for left timestamp + slider
                                    ui.with_layout(
                                        egui::Layout::left_to_right(egui::Align::Center),
                                        |ui| {
                                            // Left timestamp
                                            ui.label(
                                                RichText::new(format!(
                                                    "{:02}:{:02}",
                                                    (pos_s as u32) / 60,
                                                    (pos_s as u32) % 60
                                                ))
                                                .color(app.cached_palette.text_muted),
                                            );

                                            // Slider fills all remaining space
                                            ui.spacing_mut().slider_width = ui.available_width();
                                            let resp = ui.add(
                                                egui::Slider::new(&mut pos_s, 0.0..=total_s)
                                                    .show_value(false)
                                                    .trailing_fill(true),
                                            );

                                            // Draw CUE markers on the slider
                                            let markers = app.audio.get_cue_markers();
                                            if !markers.is_empty() && tot_ms > 0 {
                                                let current_cue_idx =
                                                    app.audio.get_current_cue_track();
                                                let painter = ui.painter();
                                                let slider_rect = resp.rect;

                                                for (idx, &marker_ms) in markers.iter().enumerate()
                                                {
                                                    if marker_ms > tot_ms {
                                                        continue;
                                                    }
                                                    let ratio = (marker_ms as f32 / tot_ms as f32)
                                                        .clamp(0.0, 1.0);
                                                    let x = slider_rect.left()
                                                        + ratio * slider_rect.width();
                                                    let center =
                                                        egui::pos2(x, slider_rect.center().y);

                                                    let is_current = current_cue_idx == Some(idx);
                                                    let color = if is_current {
                                                        app.cached_palette.accent2
                                                    } else {
                                                        app.cached_palette
                                                            .text_muted
                                                            .gamma_multiply(0.6)
                                                    };
                                                    let radius = if is_current { 2.5 } else { 1.5 };
                                                    painter.circle_filled(center, radius, color);
                                                }
                                            }

                                            if resp.drag_stopped()
                                                || (resp.clicked() && !resp.dragged())
                                            {
                                                app.audio.seek(std::time::Duration::from_secs_f32(
                                                    pos_s,
                                                ));
                                                app.music_seeking_target_ms =
                                                    Some((pos_s * 1000.0) as u64);
                                                app.music_seek_timeout = Some(Instant::now());
                                                app.music_hud_last_activity = Instant::now();
                                            }
                                        },
                                    );
                                },
                            );
                        });
                    }
                }

                ui.add_space(4.0);
                ui.scope(|ui| {
                    let bp = ui.spacing().button_padding;
                    let control_h = ui.text_style_height(&egui::TextStyle::Body) + 2.0 * bp.y;
                    ui.style_mut().spacing.interact_size.y = control_h;

                    egui::Grid::new("music_device_volume_grid")
                        .num_columns(2)
                        .spacing([8.0, 6.0])
                        .show(ui, |ui| {
                            grid_label(
                                ui,
                                RichText::new(t!("music.output_device"))
                                    .color(app.cached_palette.text_muted),
                            );
                            ui.horizontal(|ui| {
                                let btn_w = control_h;
                                let combo_w =
                                    (ui.available_width() - btn_w - ui.spacing().item_spacing.x)
                                        .max(40.0);

                                let current_dev = app
                                    .settings
                                    .audio_device
                                    .clone()
                                    .unwrap_or_else(|| t!("music.default_device").to_string());

                                // Combo first, refresh button on the right.
                                egui::ComboBox::from_id_salt("audio_device_select")
                                    .selected_text(&current_dev)
                                    .width(combo_w)
                                    .truncate()
                                    .show_ui(ui, |ui| {
                                        let default_label = t!("music.default_device").to_string();
                                        if ui
                                            .selectable_label(
                                                app.settings.audio_device.is_none(),
                                                &default_label,
                                            )
                                            .clicked()
                                        {
                                            app.settings.audio_device = None;
                                            app.audio.set_device(None);
                                            app.queue_save();
                                            app.music_hud_last_activity = Instant::now();
                                        }
                                        for dev in &app.cached_audio_devices {
                                            let is_selected =
                                                app.settings.audio_device.as_ref() == Some(dev);
                                            let short_name = middle_truncate(dev, 40);
                                            if ui
                                                .selectable_label(is_selected, short_name)
                                                .clicked()
                                            {
                                                app.settings.audio_device = Some(dev.clone());
                                                app.audio.set_device(Some(dev.clone()));
                                                app.queue_save();
                                                app.music_hud_last_activity = Instant::now();
                                            }
                                        }
                                    });

                                if ui
                                    .add_sized(egui::vec2(btn_w, btn_w), egui::Button::new("⟲"))
                                    .on_hover_text(t!("music.refresh_devices"))
                                    .clicked()
                                {
                                    app.refresh_audio_devices();
                                }
                            });
                            ui.end_row();

                            grid_label(
                                ui,
                                RichText::new(t!("label.volume"))
                                    .color(app.cached_palette.text_muted),
                            );
                            let old_vol = app.settings.volume;
                            let track_w = (ui.available_width()
                                - MUSIC_SLIDER_VALUE_WIDTH
                                - ui.spacing().item_spacing.x)
                                .max(40.0);
                            let resp = add_fixed_slider(
                                ui,
                                track_w,
                                MUSIC_SLIDER_VALUE_WIDTH,
                                egui::Slider::new(&mut app.settings.volume, 0.0..=1.0)
                                    .show_value(true)
                                    .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
                            );
                            if (old_vol - app.settings.volume).abs() > 0.001 {
                                app.audio.set_volume(app.settings.volume);
                            }
                            if resp.drag_stopped() || (resp.changed() && !resp.dragged()) {
                                app.queue_save();
                            }
                            ui.end_row();
                        });
                });
                if let Some(err) = app.audio.take_error() {
                    ui.label(
                        RichText::new(t!("music.audio_error", err = err))
                            .color(Color32::from_rgb(255, 100, 100)),
                    );
                }
            }); // close settings_card "Playback"
        }
    });
}

fn draw_about_tab(app: &mut ImageViewerApp, ui: &mut egui::Ui) {
    crate::ui::utils::center_in_settings_panel(ui, 440.0, |ui| {
        ui.vertical_centered(|ui| {
            draw_about_icon(app, ui);
            ui.add_space(8.0);
            ui.label(
                RichText::new(t!("app.title"))
                    .color(app.cached_palette.accent2)
                    .size(20.0)
                    .strong(),
            );
            ui.label(
                RichText::new(t!("about.version", version = env!("CARGO_PKG_VERSION")))
                    .color(app.cached_palette.text_muted),
            );
            ui.label(RichText::new(t!("about.copyright")).color(app.cached_palette.text_muted));
            ui.hyperlink_to(
                "https://github.com/z16166/SimpleImageViewer/releases",
                "https://github.com/z16166/SimpleImageViewer/releases",
            );
        });
    });
}

fn draw_about_icon(app: &mut ImageViewerApp, ui: &mut egui::Ui) {
    if app.about_icon_texture.is_none() {
        if let Ok(image) = image::load_from_memory(ABOUT_ICON_BYTES) {
            let rgba = image.into_rgba8();
            let size = [rgba.width() as usize, rgba.height() as usize];
            let pixels = rgba.into_raw();
            let color_image = egui::ColorImage::from_rgba_unmultiplied(size, &pixels);
            app.about_icon_texture = Some(ui.ctx().load_texture(
                "settings_about_icon",
                color_image,
                egui::TextureOptions::LINEAR,
            ));
        }
    }

    if let Some(texture) = &app.about_icon_texture {
        ui.image((texture.id(), egui::vec2(ABOUT_ICON_SIZE, ABOUT_ICON_SIZE)));
    } else {
        ui.label(
            RichText::new("🖼")
                .size(ABOUT_ICON_SIZE * 0.5)
                .color(app.cached_palette.accent2),
        );
    }
}

fn draw_hdr_settings_if_available(app: &mut ImageViewerApp, ui: &mut egui::Ui) {
    #[cfg(not(target_os = "linux"))]
    {
        ui.add_space(8.0);
        draw_hdr_section(app, ui);
    }
    #[cfg(target_os = "linux")]
    {
        ui.add_space(8.0);
        if crate::hdr::platform::linux_native_hdr_platform_eligible() {
            draw_hdr_section(app, ui);
        } else {
            ui.label(
                RichText::new(t!("hdr.wayland_only_hint")).color(app.cached_palette.text_muted),
            );
        }
    }
}

#[cfg(target_os = "windows")]
fn draw_system_tab(app: &mut ImageViewerApp, ui: &mut egui::Ui) {
    let pane_w = ui.max_rect().width();
    const SIDE_PAD: f32 = 12.0;
    let card_w = 460.0_f32.min(pane_w - 2.0 * SIDE_PAD).max(60.0);

    ui.allocate_ui_with_layout(
        egui::vec2(pane_w, ui.available_height()),
        egui::Layout::top_down(egui::Align::Center),
        |ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(card_w, 0.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_min_width(card_w);
                    ui.set_max_width(card_w);
                    draw_windows_section(app, ui);
                },
            );
        },
    );
}

#[cfg(target_os = "windows")]
fn draw_windows_section(app: &mut ImageViewerApp, ui: &mut egui::Ui) {
    let palette = app.cached_palette.clone();
    system_settings_card(ui, &palette, t!("section.system_windows"), |ui| {
        ui.label(RichText::new(t!("win.register_hint")).color(palette.text_muted));
        ui.add_space(8.0);

        let row_w = ui.available_width();
        let button_h = ui.spacing().interact_size.y;
        let row_h = button_h + 10.0;
        let (row_rect, _) = ui.allocate_exact_size(egui::vec2(row_w, row_h), egui::Sense::hover());

        let buttons_w_id = egui::Id::new("system_tab_button_group_width");
        let measured_w: f32 = ui.ctx().data(|d| d.get_temp(buttons_w_id).unwrap_or(0.0));
        let group_w = if measured_w > 0.0 {
            measured_w.min(row_w)
        } else {
            row_w.min(320.0)
        };
        let group_x = row_rect.left() + ((row_w - group_w) / 2.0).max(0.0);
        let group_y = row_rect.center().y - button_h / 2.0;
        let group_rect =
            egui::Rect::from_min_size(egui::pos2(group_x, group_y), egui::vec2(group_w, button_h));
        let mut group_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(group_rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );

        let r1 = styled_button(&mut group_ui, t!("win.assoc_formats"), &palette);
        if r1.clicked() {
            if let Ok(reg) = crate::formats::get_registry().read() {
                let formats = reg.formats.clone();
                app.active_modal = Some(crate::ui::dialogs::modal_state::ActiveModal::FileAssoc(
                    crate::ui::dialogs::file_assoc::State::new(formats),
                ));
            }
        }
        let r2 = styled_button(&mut group_ui, t!("win.remove_assoc"), &palette);
        if r2.clicked() {
            app.active_modal = Some(crate::ui::dialogs::modal_state::ActiveModal::Confirm(
                crate::ui::dialogs::confirm::State::remove_file_assoc(
                    t!("win.confirm_remove_title"),
                    t!("win.confirm_remove_msg"),
                ),
            ));
        }
        let actual_w = r2.rect.right() - r1.rect.left();
        ui.ctx().data_mut(|d| d.insert_temp(buttons_w_id, actual_w));
    });
}

#[cfg(target_os = "windows")]
fn system_settings_card<R>(
    ui: &mut egui::Ui,
    palette: &crate::theme::ThemePalette,
    title: impl Into<String>,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    egui::Frame::new()
        .fill(
            palette
                .widget_bg
                .gamma_multiply(if palette.is_dark { 0.55 } else { 0.9 }),
        )
        .stroke(egui::Stroke::new(1.0_f32, palette.widget_border))
        .corner_radius(egui::CornerRadius::same(4))
        .inner_margin(Margin {
            left: 10,
            right: 10,
            top: 8,
            bottom: 16,
        })
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(RichText::new(title).color(palette.accent2).strong());
            ui.add_space(6.0);
            add_contents(ui)
        })
        .inner
}
