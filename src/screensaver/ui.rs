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

//! Compact screensaver settings page (main settings dialog + `/c` host).

use crate::app::ImageViewerApp;
use crate::screensaver::{
    ScreensaverDisplayPolicy, ScreensaverPerformanceProfile, ScreensaverSettings,
};
use crate::ui::utils::{settings_card, stable_selectable_value, themed_labeled_toggle};
use eframe::egui;
use rust_i18n::t;

fn slider_or_drag_committed(resp: &egui::Response) -> bool {
    resp.drag_stopped() || (resp.changed() && !resp.dragged())
}

/// Draw the screensaver settings tab body into `ui`.
pub fn draw_screensaver_tab(app: &mut ImageViewerApp, ui: &mut egui::Ui) {
    let mut dirty = false;
    let palette = app.cached_palette;

    settings_card(ui, &palette, t!("section.screensaver_source"), |ui| {
        let source_label = app
            .screensaver_settings
            .primary_source()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| t!("label.no_dir").to_string());
        let empty = app.screensaver_settings.sources.is_empty();
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(t!("btn.pick").to_string()).clicked() {
                    app.pending_open_screensaver_directory = true;
                }
                ui.add_space(4.0);
                let box_w = (ui.available_width() - 8.0).max(20.0);
                let resp = ui.add_sized(
                    [box_w, ui.spacing().interact_size.y],
                    egui::Label::new(if empty {
                        egui::RichText::new(source_label).weak()
                    } else {
                        egui::RichText::new(source_label)
                    })
                    .truncate(),
                );
                if let Some(full) = app.screensaver_settings.primary_source() {
                    resp.on_hover_text(full.to_string_lossy());
                }
            });
        });
        if themed_labeled_toggle(
            ui,
            &mut app.screensaver_settings.recursive,
            t!("label.screensaver_recursive"),
            &palette,
        )
        .changed()
        {
            dirty = true;
        }
    });

    settings_card(ui, &palette, t!("section.screensaver_playback"), |ui| {
        let interval_resp = ui
            .horizontal(|ui| {
                ui.label(t!("label.interval_sec"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add(
                        egui::DragValue::new(&mut app.screensaver_settings.interval_secs)
                            .range(0.5..=3600.0)
                            .speed(0.5),
                    )
                })
                .inner
            })
            .inner;
        if slider_or_drag_committed(&interval_resp) {
            dirty = true;
        }
        if themed_labeled_toggle(
            ui,
            &mut app.screensaver_settings.random_order,
            t!("label.random_slideshow_order"),
            &palette,
        )
        .changed()
        {
            dirty = true;
        }
        if themed_labeled_toggle(
            ui,
            &mut app.screensaver_settings.exit_on_input,
            t!("label.screensaver_exit_on_input"),
            &palette,
        )
        .changed()
        {
            dirty = true;
        }

        ui.horizontal(|ui| {
            ui.label(t!("label.screensaver_display"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mut display = app.screensaver_settings.display;
                let changed_all = stable_selectable_value(
                    ui,
                    &mut display,
                    ScreensaverDisplayPolicy::All,
                    t!("label.screensaver_display_all"),
                )
                .changed();
                let changed_primary = stable_selectable_value(
                    ui,
                    &mut display,
                    ScreensaverDisplayPolicy::Primary,
                    t!("label.screensaver_display_primary"),
                )
                .changed();
                if changed_all || changed_primary {
                    app.screensaver_settings.display = display;
                    dirty = true;
                }
            });
        });
    });

    settings_card(ui, &palette, t!("section.screensaver_performance"), |ui| {
        ui.horizontal(|ui| {
            ui.label(t!("label.screensaver_performance"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mut profile = app.screensaver_settings.performance;
                let c1 = stable_selectable_value(
                    ui,
                    &mut profile,
                    ScreensaverPerformanceProfile::PowerSave,
                    t!("label.screensaver_power_save"),
                )
                .changed();
                let c2 = stable_selectable_value(
                    ui,
                    &mut profile,
                    ScreensaverPerformanceProfile::Quality,
                    t!("label.screensaver_quality"),
                )
                .changed();
                if c1 || c2 {
                    app.screensaver_settings.performance = profile;
                    dirty = true;
                }
            });
        });
        if themed_labeled_toggle(
            ui,
            &mut app.screensaver_settings.allow_hdr,
            t!("label.screensaver_allow_hdr"),
            &palette,
        )
        .changed()
        {
            dirty = true;
        }
    });

    #[cfg(target_os = "windows")]
    {
        settings_card(ui, &palette, t!("section.screensaver_windows"), |ui| {
            ui.label(t!("screensaver.windows_hint"));
            ui.add_space(4.0);
            if ui
                .button(t!("btn.screensaver_install_system").to_string())
                .clicked()
            {
                match crate::screensaver::windows_register::install_system_screensaver() {
                    Ok(msg) => {
                        app.screensaver_status_message = Some(msg);
                    }
                    Err(e) => {
                        app.screensaver_status_message = Some(e);
                    }
                }
            }
            if ui
                .button(t!("btn.screensaver_set_active").to_string())
                .clicked()
            {
                match crate::screensaver::windows_register::set_as_active_screensaver() {
                    Ok(msg) => {
                        app.screensaver_status_message = Some(msg);
                    }
                    Err(e) => {
                        app.screensaver_status_message = Some(e);
                    }
                }
            }
            if let Some(msg) = &app.screensaver_status_message {
                ui.add_space(4.0);
                ui.label(msg.as_str());
            }
        });
    }

    if dirty {
        app.screensaver_settings = app.screensaver_settings.clone().normalized();
        app.queue_screensaver_save();
    }
}

/// Minimal standalone config host used by Windows `/c` and `--phase=config`.
pub fn draw_screensaver_config_window(app: &mut ImageViewerApp, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    egui::CentralPanel::default().show_inside(ui, |ui| {
        ui.heading(t!("settings_tab.screensaver"));
        ui.separator();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                draw_screensaver_tab(app, ui);
            });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button(t!("btn.close").to_string()).clicked() {
                // Persist immediately so `/c` close is durable even if on_exit is skipped.
                if let Err(e) = app.screensaver_settings.save() {
                    log::error!("[screensaver] config save failed: {e}");
                }
                app.explicit_quit = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        });
    });
}

/// Seed viewer `Settings` runtime fields from screensaver config for a run/preview session.
pub fn apply_screensaver_to_viewer_settings(
    viewer: &mut crate::settings::Settings,
    saver: &ScreensaverSettings,
) {
    viewer.auto_switch = true;
    viewer.auto_switch_interval = saver.interval_secs;
    viewer.random_slideshow_order = saver.random_order;
    viewer.recursive = saver.recursive;
    viewer.fullscreen = true;
    viewer.play_music = false;
    viewer.show_osd = false;
    viewer.show_music_osd = false;
    viewer.show_pixel_inspector = false;
    viewer.show_directory_tree_nav = false;
    viewer.minimize_to_tray_on_close = false;
    // Keep resume / last_viewed out of the screensaver session.
    viewer.resume_last_image = false;
    // Power-save keeps neighbor decode on; Quality can leave user preload prefs alone.
    // The soft preload_neighbors hint is reserved for a future loader radius override.
    if saver.uses_power_save() {
        viewer.preload = saver.preload_neighbors > 0;
    }
    if !saver.allow_hdr || saver.uses_power_save() {
        viewer.hdr_native_surface_enabled = false;
    }
    if let Some(dir) = saver.primary_source() {
        viewer.set_current_browse_directory(dir.clone(), false);
        viewer.transient_image_dir = Some(dir.clone());
    }
}
