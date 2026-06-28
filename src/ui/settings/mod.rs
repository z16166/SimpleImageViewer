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

mod about;
mod appearance;
mod context_menu;
mod hotkeys;
mod library;
mod music;
mod slideshow;
mod system;
mod viewing;

use crate::app::{ImageViewerApp, SettingsTab};
use eframe::Frame;
use eframe::egui::{self, Color32, Context, Pos2, RichText};
use rust_i18n::t;

const SETTINGS_TAB_SIDEBAR_WIDTH: f32 = 124.0;
const SETTINGS_TAB_ITEM_HEIGHT: f32 = 34.0;

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
                &mut SettingsDialogActions {
                    open_dir: &mut open_dir,
                    fullscreen_changed: &mut fullscreen_changed,
                    open_music_file: &mut open_music_file,
                    open_music_dir: &mut open_music_dir,
                    music_enabled_changed: &mut music_enabled_changed,
                },
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
    if std::mem::take(&mut app.context_menu_exe_browse_requested) {
        app.request_context_menu_executable_picker(frame);
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

struct SettingsDialogActions<'a> {
    open_dir: &'a mut bool,
    fullscreen_changed: &'a mut bool,
    open_music_file: &'a mut bool,
    open_music_dir: &'a mut bool,
    music_enabled_changed: &'a mut bool,
}

fn draw_settings_body(
    app: &mut ImageViewerApp,
    ui: &mut egui::Ui,
    ctx: &Context,
    actions: &mut SettingsDialogActions<'_>,
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
            actions,
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
                    actions,
                );
            });
    }
}

fn draw_settings_tabs(app: &mut ImageViewerApp, ui: &mut egui::Ui) {
    ui.vertical(|ui| {
        for tab in SettingsTab::ALL {
            let selected = app.settings_tab == tab;
            let label = t!(tab.label_key()).to_string();
            let mut text = RichText::new(label);
            let mut button =
                egui::Button::selectable(selected, text.clone()).frame_when_inactive(true);
            if selected {
                let palette = &app.cached_palette;
                let fill = settings_tab_selected_fill(palette);
                let stroke = if palette.is_dark {
                    egui::Stroke::new(1.0_f32, palette.accent)
                } else {
                    egui::Stroke::new(1.0_f32, palette.widget_border_hover)
                };
                text = text.strong().color(if palette.is_dark {
                    Color32::WHITE
                } else {
                    palette.text_normal
                });
                button = egui::Button::selectable(true, text)
                    .frame_when_inactive(true)
                    .fill(fill)
                    .stroke(stroke);
            }
            if ui
                .add_sized([ui.available_width(), SETTINGS_TAB_ITEM_HEIGHT], button)
                .clicked()
            {
                app.settings_tab = tab;
            }
            ui.add_space(2.0);
        }
    });
}

fn settings_tab_selected_fill(palette: &crate::theme::ThemePalette) -> Color32 {
    Color32::from_rgba_unmultiplied(
        palette.accent2.r(),
        palette.accent2.g(),
        palette.accent2.b(),
        if palette.is_dark { 40 } else { 30 },
    )
}

pub(super) enum SliderTrackMode {
    /// Caller-specified track width (e.g. grid rows that size the slider outside `add_slider`).
    #[allow(dead_code)]
    Fixed(f32),
    Elastic,
}

pub(super) fn add_slider(
    ui: &mut egui::Ui,
    value_width: f32,
    slider: egui::Slider<'_>,
    track_mode: SliderTrackMode,
) -> egui::Response {
    ui.scope(|ui| {
        let track_width = match track_mode {
            SliderTrackMode::Fixed(width) => width,
            SliderTrackMode::Elastic => {
                (ui.available_width() - value_width - ui.spacing().item_spacing.x).max(40.0)
            }
        };
        ui.spacing_mut().slider_width = track_width;
        ui.spacing_mut().interact_size.x = value_width;
        ui.add(slider)
    })
    .inner
}

pub(super) fn grid_label(ui: &mut egui::Ui, text: impl Into<egui::WidgetText>) {
    ui.allocate_ui_with_layout(
        egui::vec2(0.0, ui.spacing().interact_size.y),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.label(text);
        },
    );
}

pub(super) fn grid_control(ui: &mut egui::Ui, row_h: f32, add_control: impl FnOnce(&mut egui::Ui)) {
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), row_h),
        egui::Layout::left_to_right(egui::Align::Center),
        add_control,
    );
}

fn draw_active_settings_tab(
    app: &mut ImageViewerApp,
    ui: &mut egui::Ui,
    ctx: &Context,
    actions: &mut SettingsDialogActions<'_>,
) {
    match app.settings_tab {
        SettingsTab::Library => library::draw_library_tab(app, ui, actions.open_dir),
        SettingsTab::Viewing => viewing::draw_viewing_tab(app, ui, actions.fullscreen_changed),
        SettingsTab::Slideshow => slideshow::draw_slideshow_tab(app, ui),
        SettingsTab::Music => music::draw_music_tab(
            app,
            ui,
            actions.open_music_file,
            actions.open_music_dir,
            actions.music_enabled_changed,
        ),
        SettingsTab::Appearance => appearance::draw(app, ui, ctx),
        SettingsTab::Hotkeys => hotkeys::draw_hotkeys_tab(app, ui, ctx),
        SettingsTab::ContextMenu => context_menu::draw_context_menu_tab(app, ui, ctx),
        SettingsTab::System => system::draw_system_tab(app, ui),
        SettingsTab::About => about::draw_about_tab(app, ui),
    }
}
