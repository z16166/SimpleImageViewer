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
use crate::ui::utils::{
    middle_truncate, path_display_box, setup_fonts, setup_visuals, styled_button,
    styled_button_widget,
};
use crate::update::core::{GITHUB_RELEASES_PAGE, ProxyType};
use eframe::Frame;
use eframe::egui::{self, Color32, Context, Pos2, RichText, Vec2};
use rust_i18n::t;
use std::time::Instant;

const ABOUT_ICON_SIZE: f32 = 96.0;
const ABOUT_ICON_BYTES: &[u8] = include_bytes!("../../assets/icon.png");

pub fn draw(app: &mut ImageViewerApp, ctx: &Context, frame: &Frame) {
    // [Point 19] Explanatory Comments:
    // The settings layout uses nested UI elements to achieve responsive alignment.
    // Specifically, path_display_box and certain groupings require fixed widths or
    // specific layout directions to prevent the egui window from collapsing or
    // expanding unexpectedly when content changes (e.g., long paths or slider movements).

    // Detect when settings panel is opened to refresh audio devices
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
        .default_pos(Pos2::new(
            crate::constants::SETTINGS_WINDOW_DEFAULT_POS[0],
            crate::constants::SETTINGS_WINDOW_DEFAULT_POS[1],
        ))
        .resizable(true)
        .collapsible(true)
        .frame(
            egui::Frame::window(&ctx.global_style())
                .fill(app.cached_palette.panel_bg)
                .shadow(egui::epaint::Shadow::NONE),
        )
        .min_width(crate::constants::SETTINGS_WINDOW_MIN_WIDTH)
        .default_width(crate::constants::SETTINGS_WINDOW_DEFAULT_WIDTH)
        .max_width(crate::constants::SETTINGS_WINDOW_MAX_WIDTH)
        .show(ctx, |ui| {
            ui.visuals_mut().override_text_color = Some(app.cached_palette.text_normal);

            draw_settings_tabs(app, ui);
            ui.separator();
            egui::ScrollArea::vertical()
                .id_salt("settings_tab_content")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    draw_active_settings_tab(
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
    // ── Slideshow ────────────────────────────────────────────
    ui.label(
        RichText::new(t!("section.slideshow"))
            .color(app.cached_palette.accent2)
            .strong(),
    );
    ui.add_space(2.0);

    let old_auto_switch = app.settings.auto_switch;
    if ui
        .checkbox(&mut app.settings.auto_switch, t!("label.auto_advance"))
        .changed()
    {
        app.slideshow_paused = false;
    }
    if app.settings.auto_switch {
        ui.horizontal(|ui| {
            ui.label(t!("label.interval_sec"));
            ui.add(
                egui::DragValue::new(&mut app.settings.auto_switch_interval)
                    .range(0.5..=3600.0)
                    .speed(0.5),
            );
        });
        ui.horizontal(|ui| {
            ui.checkbox(&mut app.settings.loop_playback, t!("label.loop_wrap"));
            ui.add_space(12.0);
            if ui
                .checkbox(
                    &mut app.settings.random_slideshow_order,
                    t!("label.random_slideshow_order"),
                )
                .changed()
            {
                app.invalidate_random_slideshow_order();
                app.queue_save();
            }
        });
    }
    if old_auto_switch != app.settings.auto_switch {
        app.invalidate_random_slideshow_order();
        app.queue_save();
    }
}

fn draw_hdr_section(app: &mut ImageViewerApp, ui: &mut egui::Ui) {
    ui.label(
        RichText::new(t!("section.hdr"))
            .color(app.cached_palette.accent2)
            .strong(),
    );
    ui.add_space(2.0);

    if ui
        .checkbox(
            &mut app.settings.hdr_native_surface_enabled,
            t!("hdr.native_surface_enabled"),
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

    ui.horizontal(|ui| {
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
        ui.label(t!("hdr.exposure_ev"));
        ui.add(
            egui::Slider::new(exposure_slot, -8.0..=8.0)
                .step_by(0.1)
                .suffix(" EV"),
        )
        .on_hover_text(hint);
    });
    ui.horizontal(|ui| {
        ui.label(t!("hdr.sdr_white_nits"));
        ui.add(
            egui::Slider::new(&mut app.settings.hdr_sdr_white_nits, 80.0..=400.0)
                .step_by(1.0)
                .suffix(" nits"),
        )
        .on_hover_text(t!("hdr.sdr_white_hint"));
    });
    ui.horizontal(|ui| {
        ui.label(t!("hdr.max_display_nits"));
        ui.add(
            egui::Slider::new(&mut app.settings.hdr_max_display_nits, 100.0..=10_000.0)
                .logarithmic(true)
                .suffix(" nits"),
        )
        .on_hover_text(t!("hdr.max_display_hint"));
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
}

fn draw_updates_section(app: &mut ImageViewerApp, ui: &mut egui::Ui) {
    ui.label(
        RichText::new(t!("section.updates"))
            .color(app.cached_palette.accent2)
            .strong(),
    );
    ui.add_space(2.0);

    let before = app.settings.updates.clone();
    ui.horizontal(|ui| {
        ui.checkbox(&mut app.settings.updates.enabled, t!("label.update_check"));
        if styled_button(ui, t!("btn.check_now"), &app.cached_palette).clicked() {
            app.start_update_check(true);
        }
    });
    ui.checkbox(
        &mut app.settings.updates.proxy.enabled,
        t!("label.update_proxy"),
    )
    .on_hover_text(t!("update.proxy_hint"));
    if app.settings.updates.proxy.enabled {
        ui.horizontal(|ui| {
            ui.label(t!("label.update_proxy_type"));
            egui::ComboBox::from_id_salt("update_proxy_type")
                .selected_text(t!(app.settings.updates.proxy.proxy_type.label_key()).to_string())
                .show_ui(ui, |ui| {
                    for proxy_type in ProxyType::ALL {
                        ui.selectable_value(
                            &mut app.settings.updates.proxy.proxy_type,
                            proxy_type,
                            t!(proxy_type.label_key()).to_string(),
                        );
                    }
                });
        });
        ui.horizontal(|ui| {
            ui.label(t!("label.update_proxy_host"));
            ui.text_edit_singleline(&mut app.settings.updates.proxy.host);
        });
        ui.horizontal(|ui| {
            ui.label(t!("label.update_proxy_port"));
            ui.add(egui::DragValue::new(&mut app.settings.updates.proxy.port).range(0..=65535));
        });
    }
    if app.settings.updates != before {
        app.queue_save();
    }
}

fn draw_settings_tabs(app: &mut ImageViewerApp, ui: &mut egui::Ui) {
    ui.horizontal_wrapped(|ui| {
        for tab in SettingsTab::ALL {
            let selected = app.settings_tab == tab;
            if ui
                .add(egui::Button::selectable(
                    selected,
                    t!(tab.label_key()).to_string(),
                ))
                .clicked()
            {
                app.settings_tab = tab;
            }
        }
    });
    ui.add_space(4.0);
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
        SettingsTab::Music => draw_music_tab(
            app,
            ui,
            open_music_file,
            open_music_dir,
            music_enabled_changed,
        ),
        SettingsTab::Appearance => draw_appearance_tab(app, ui, ctx),
        SettingsTab::Updates => draw_updates_tab(app, ui),
        #[cfg(target_os = "windows")]
        SettingsTab::System => draw_system_tab(app, ui),
        #[cfg(not(target_os = "windows"))]
        SettingsTab::System => {}
        SettingsTab::About => draw_about_tab(app, ui),
    }
}

fn draw_library_tab(app: &mut ImageViewerApp, ui: &mut egui::Ui, open_dir: &mut bool) {
    ui.vertical(|ui| {
        draw_library_controls(app, ui, open_dir);
    });
}

fn draw_library_controls(app: &mut ImageViewerApp, ui: &mut egui::Ui, open_dir: &mut bool) {
    // ── Directory ──────────────────────────────────────────────
    ui.label(
        RichText::new(t!("section.directory"))
            .color(app.cached_palette.accent2)
            .strong(),
    );
    ui.add_space(2.0);

    let dir_full = app
        .settings
        .last_image_dir
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned());
    let dir_short = app
        .settings
        .last_image_dir
        .as_ref()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir_full.clone().unwrap_or_default());
    let dir_empty = app.settings.last_image_dir.is_none();
    let dir_label = if dir_empty {
        t!("label.no_dir").to_string()
    } else {
        dir_short
    };
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if styled_button(ui, t!("btn.pick"), &app.cached_palette).clicked() {
                *open_dir = true;
            }
            ui.add_space(4.0);
            if styled_button(ui, t!("btn.refresh"), &app.cached_palette).clicked() {
                if let Some(dir) = app.settings.last_image_dir.clone() {
                    app.load_directory(dir);
                }
            }

            let box_w = (ui.available_width() - 16.0).max(20.0);
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                let resp = path_display_box(ui, &dir_label, dir_empty, box_w, &app.cached_palette);
                if let Some(full) = &dir_full {
                    resp.on_hover_text(full.as_str());
                }
            });
        });
    });

    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let old_recursive = app.settings.recursive;
        ui.checkbox(
            &mut app.settings.recursive,
            t!("label.recursive_scan").to_string(),
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

        ui.add_space(12.0);

        if ui
            .checkbox(
                &mut app.settings.preload,
                t!("label.enable_preload").to_string(),
            )
            .changed()
        {
            app.queue_save();
        }
    });

    if ui
        .checkbox(
            &mut app.settings.resume_last_image,
            t!("label.resume_last").to_string(),
        )
        .changed()
    {
        app.queue_save();
    }

    if app.scanning {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(RichText::new(&app.status_message).color(app.cached_palette.text_muted));
        });
    }
}

fn draw_viewing_tab(app: &mut ImageViewerApp, ui: &mut egui::Ui, fullscreen_changed: &mut bool) {
    ui.columns(2, |cols| {
        draw_viewing_main_column(app, &mut cols[0], fullscreen_changed);
        draw_viewing_hdr_column(app, &mut cols[1]);
    });
}

fn draw_viewing_main_column(
    app: &mut ImageViewerApp,
    ui: &mut egui::Ui,
    fullscreen_changed: &mut bool,
) {
    ui.vertical(|ui| {
        // ── Display ────────────────────────────────────────────────
        ui.label(
            RichText::new(t!("section.display"))
                .color(app.cached_palette.accent2)
                .strong(),
        );
        ui.add_space(2.0);

        let old_fullscreen = app.settings.fullscreen;
        ui.checkbox(
            &mut app.settings.fullscreen,
            t!("label.fullscreen").to_string(),
        );
        if old_fullscreen != app.settings.fullscreen {
            *fullscreen_changed = true;
        }

        ui.add_space(6.0);

        ui.label(RichText::new(t!("label.scale_mode")).color(app.cached_palette.text_muted));
        ui.add_space(2.0);
        let old_scale = app.settings.scale_mode;
        ui.horizontal(|ui| {
            let fit_active = app.settings.scale_mode == ScaleMode::FitToWindow;
            if ui
                .add(egui::Button::selectable(
                    fit_active,
                    t!("scale.fit_btn").to_string(),
                ))
                .clicked()
                && !fit_active
            {
                app.settings.scale_mode = ScaleMode::FitToWindow;
            }
            let orig_active = app.settings.scale_mode == ScaleMode::OriginalSize;
            if ui
                .add(egui::Button::selectable(
                    orig_active,
                    t!("scale.original_btn").to_string(),
                ))
                .clicked()
                && !orig_active
            {
                app.settings.scale_mode = ScaleMode::OriginalSize;
            }
        });
        if old_scale != app.settings.scale_mode {
            app.zoom_factor = 1.0;
            app.pan_offset = Vec2::ZERO;
            app.queue_save();
        }
        ui.add_space(4.0);
        ui.label(RichText::new(t!("label.z_toggle_hint")).color(app.cached_palette.text_muted));

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui
                .checkbox(&mut app.settings.show_osd, t!("label.show_osd"))
                .changed()
            {
                app.queue_save();
            }

            ui.add_space(12.0);

            if ui
                .checkbox(
                    &mut app.settings.raw_high_quality,
                    t!("label.raw_high_quality"),
                )
                .on_hover_text(t!("hint.raw_high_quality"))
                .changed()
            {
                app.reload_current();
                app.queue_save();
            }
        });

        // ── Transitions ──────────────────────────────────────────
        ui.add_space(8.0);
        ui.label(
            RichText::new(t!("section.transitions"))
                .color(app.cached_palette.accent2)
                .strong(),
        );
        ui.add_space(2.0);

        ui.horizontal(|ui| {
            ui.label(t!("label.style"));
            let old_style = app.settings.transition_style;
            egui::ComboBox::from_id_salt("transition_style")
                .selected_text(app.settings.transition_style.label())
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
            if old_style != app.settings.transition_style {
                app.queue_save();
            }
        });

        if app.settings.transition_style != TransitionStyle::None {
            ui.horizontal(|ui| {
                ui.label(t!("label.duration"));
                let old_ms = app.settings.transition_ms;
                ui.add(egui::Slider::new(&mut app.settings.transition_ms, 50..=2000).suffix("ms"));
                if old_ms != app.settings.transition_ms {
                    app.queue_save();
                }
            });
        }
    });
}

fn draw_viewing_hdr_column(app: &mut ImageViewerApp, ui: &mut egui::Ui) {
    ui.vertical(|ui| {
        draw_slideshow_section(app, ui);
        ui.add_space(8.0);
        draw_hdr_settings_if_available(app, ui);
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
        // ── Music ──────────────────────────────────────────────────
        ui.label(
            RichText::new(t!("section.music"))
                .color(app.cached_palette.accent2)
                .strong(),
        );
        ui.add_space(2.0);

        let old_play_music = app.settings.play_music;
        let old_show_music_osd = app.settings.show_music_osd;

        ui.horizontal(|ui| {
            ui.checkbox(&mut app.settings.play_music, t!("label.play_music"));
            if app.settings.play_music {
                ui.checkbox(&mut app.settings.show_music_osd, t!("label.show_music_osd"));
            }
        });

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
            let music_short = app
                .settings
                .music_path
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| music_full.clone().unwrap_or_default());
            let music_empty = app.settings.music_path.is_none();
            let music_label = if music_empty {
                t!("label.no_music").to_string()
            } else {
                music_short
            };
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if styled_button(ui, t!("btn.pick_dir"), &app.cached_palette).clicked() {
                        *open_music_dir = true;
                    }
                    ui.add_space(4.0);
                    if styled_button(ui, t!("btn.pick_file"), &app.cached_palette).clicked() {
                        *open_music_file = true;
                    }
                    let box_w = (ui.available_width() - 16.0).max(20.0);
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        let resp = path_display_box(
                            ui,
                            &music_label,
                            music_empty,
                            box_w,
                            &app.cached_palette,
                        );
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
                        ui.label(
                            RichText::new(t!("music.scanning"))
                                .color(app.cached_palette.text_muted),
                        );
                    } else if let Some(count) = app.cached_music_count {
                        if count > 0 {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(t!(
                                        "music.files_ready",
                                        count = count.to_string()
                                    ))
                                    .color(app.cached_palette.accent2),
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
            let filename = app.audio.get_current_track();
            let metadata = app.audio.get_metadata();

            if let Some(f) = filename {
                ui.add_space(4.0);
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

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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
                                        let current_cue_idx = app.audio.get_current_cue_track();
                                        let painter = ui.painter();
                                        let slider_rect = resp.rect;

                                        for (idx, &marker_ms) in markers.iter().enumerate() {
                                            if marker_ms > tot_ms {
                                                continue;
                                            }
                                            let ratio =
                                                (marker_ms as f32 / tot_ms as f32).clamp(0.0, 1.0);
                                            let x =
                                                slider_rect.left() + ratio * slider_rect.width();
                                            let center = egui::pos2(x, slider_rect.center().y);

                                            let is_current = current_cue_idx == Some(idx);
                                            let color = if is_current {
                                                app.cached_palette.accent2
                                            } else {
                                                app.cached_palette.text_muted.gamma_multiply(0.6)
                                            };
                                            let radius = if is_current { 2.5 } else { 1.5 };
                                            painter.circle_filled(center, radius, color);
                                        }
                                    }

                                    if resp.drag_stopped() || (resp.clicked() && !resp.dragged()) {
                                        app.audio.seek(std::time::Duration::from_secs_f32(pos_s));
                                        app.music_seeking_target_ms = Some((pos_s * 1000.0) as u64);
                                        app.music_seek_timeout = Some(Instant::now());
                                        app.music_hud_last_activity = Instant::now();
                                    }
                                },
                            );
                        });
                    });
                }
            }

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(t!("music.output_device")).color(app.cached_palette.text_muted),
                );
                if ui
                    .button("⟲")
                    .on_hover_text(t!("music.refresh_devices"))
                    .clicked()
                {
                    app.refresh_audio_devices();
                }

                let current_dev = app
                    .settings
                    .audio_device
                    .clone()
                    .unwrap_or_else(|| t!("music.default_device").to_string());

                // .width(remaining) + .truncate() lets egui clip the selected text with
                // a trailing "…" when the device name is too long for the available space,
                // without ever expanding the settings window horizontally.
                egui::ComboBox::from_id_salt("audio_device_select")
                    .selected_text(&current_dev)
                    .width(ui.available_width())
                    .truncate()
                    .show_ui(ui, |ui| {
                        let default_label = t!("music.default_device").to_string();
                        if ui
                            .selectable_label(app.settings.audio_device.is_none(), &default_label)
                            .clicked()
                        {
                            app.settings.audio_device = None;
                            app.audio.set_device(None);
                            app.queue_save();
                            app.music_hud_last_activity = Instant::now();
                        }
                        for dev in &app.cached_audio_devices {
                            let is_selected = app.settings.audio_device.as_ref() == Some(dev);
                            let short_name = middle_truncate(dev, 40);
                            if ui.selectable_label(is_selected, short_name).clicked() {
                                app.settings.audio_device = Some(dev.clone());
                                app.audio.set_device(Some(dev.clone()));
                                app.queue_save();
                                app.music_hud_last_activity = Instant::now();
                            }
                        }
                    });
            });

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(t!("label.volume")).color(app.cached_palette.text_muted));
                let old_vol = app.settings.volume;

                let resp = ui.add(
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
            });
            if let Some(err) = app.audio.take_error() {
                ui.label(
                    RichText::new(t!("music.audio_error", err = err))
                        .color(Color32::from_rgb(255, 100, 100)),
                );
            }
        }
    });
}

fn draw_appearance_tab(app: &mut ImageViewerApp, ui: &mut egui::Ui, ctx: &Context) {
    ui.vertical(|ui| {
        ui.add_space(8.0);
        ui.label(
            RichText::new(t!("section.font"))
                .color(app.cached_palette.accent2)
                .strong(),
        );
        ui.add_space(2.0);

        ui.horizontal(|ui| {
            ui.label(t!("label.interface_size"));
            let mut current_size = app.temp_font_size.unwrap_or(app.settings.font_size);
            let resp = ui.add(egui::Slider::new(&mut current_size, 12.0..=32.0).step_by(1.0));

            if resp.dragged() {
                app.temp_font_size = Some(current_size);
            } else if resp.drag_stopped() || (resp.changed() && !resp.dragged()) {
                app.settings.font_size = current_size;
                app.temp_font_size = None;
                setup_visuals(ctx, &app.settings, &app.cached_palette);
                app.queue_save();
            }
        });

        ui.push_id("font_selection_area", |ui| {
            ui.horizontal(|ui| {
                ui.label(t!("label.interface_font"));
                let old_family = app.settings.font_family.clone();
                egui::ComboBox::from_id_salt("font_family")
                    .selected_text(if app.settings.font_family == "System Default" {
                        t!("label.system_default").to_string()
                    } else {
                        app.settings.font_family.clone()
                    })
                    .show_ui(ui, |ui| {
                        for family in &app.font_families {
                            let label = if family == "System Default" {
                                t!("label.system_default").to_string()
                            } else {
                                family.clone()
                            };
                            ui.selectable_value(
                                &mut app.settings.font_family,
                                family.clone(),
                                label,
                            );
                        }
                    });
                if old_family != app.settings.font_family {
                    app.is_font_error = false;
                    if !setup_fonts(ctx, &app.settings) {
                        app.settings.font_family = old_family;
                        app.is_font_error = true;
                    } else {
                        app.queue_save();
                    }
                }
            });
            if app.is_font_error {
                ui.label(
                    RichText::new(t!("label.font_load_error"))
                        .color(Color32::from_rgb(255, 100, 100)),
                );
            }
        });

        ui.horizontal(|ui| {
            ui.label(t!("section.language"));
            let old_lang = app.settings.language.clone();
            egui::ComboBox::from_id_salt("language_select")
                .selected_text(match app.settings.language.as_str() {
                    "en" => t!("lang.en").to_string(),
                    "zh-CN" => t!("lang.zh_cn").to_string(),
                    "zh-TW" => t!("lang.zh_tw").to_string(),
                    "zh-HK" => t!("lang.zh_hk").to_string(),
                    _ => app.settings.language.clone(),
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut app.settings.language,
                        "en".to_string(),
                        t!("lang.en"),
                    );
                    ui.selectable_value(
                        &mut app.settings.language,
                        "zh-CN".to_string(),
                        t!("lang.zh_cn"),
                    );
                    ui.selectable_value(
                        &mut app.settings.language,
                        "zh-TW".to_string(),
                        t!("lang.zh_tw"),
                    );
                    ui.selectable_value(
                        &mut app.settings.language,
                        "zh-HK".to_string(),
                        t!("lang.zh_hk"),
                    );
                });
            if old_lang != app.settings.language {
                rust_i18n::set_locale(&app.settings.language);
                app.queue_save();
            }
        });

        ui.horizontal(|ui| {
            ui.label(t!("section.theme"));
            let old_theme = app.settings.theme;
            egui::ComboBox::from_id_salt("app_theme_select")
                .selected_text(match app.settings.theme {
                    crate::app::AppTheme::Dark => t!("theme.dark").to_string(),
                    crate::app::AppTheme::Light => t!("theme.light").to_string(),
                    crate::app::AppTheme::System => t!("theme.system").to_string(),
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut app.settings.theme,
                        crate::app::AppTheme::Dark,
                        t!("theme.dark"),
                    );
                    ui.selectable_value(
                        &mut app.settings.theme,
                        crate::app::AppTheme::Light,
                        t!("theme.light"),
                    );
                    ui.selectable_value(
                        &mut app.settings.theme,
                        crate::app::AppTheme::System,
                        t!("theme.system"),
                    );
                });
            if old_theme != app.settings.theme {
                app.queue_save();
            }
        });
    });
}

fn draw_updates_tab(app: &mut ImageViewerApp, ui: &mut egui::Ui) {
    ui.vertical(|ui| {
        draw_updates_section(app, ui);
    });
}

fn draw_about_tab(app: &mut ImageViewerApp, ui: &mut egui::Ui) {
    ui.vertical_centered(|ui| {
        ui.add_space(12.0);
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
        ui.add_space(8.0);
        ui.label(RichText::new(t!("about.copyright")).color(app.cached_palette.text_muted));
        ui.add_space(4.0);
        ui.hyperlink_to(t!("about.website").to_string(), GITHUB_RELEASES_PAGE);
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
    ui.vertical(|ui| {
        draw_windows_section(app, ui);
    });
}

#[cfg(target_os = "windows")]
fn draw_windows_section(app: &mut ImageViewerApp, ui: &mut egui::Ui) {
    ui.label(
        RichText::new(t!("section.system_windows"))
            .color(app.cached_palette.accent2)
            .strong(),
    );
    ui.add_space(2.0);
    ui.label(RichText::new(t!("win.register_hint")).color(app.cached_palette.text_muted));
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if styled_button(ui, t!("win.assoc_formats"), &app.cached_palette).clicked() {
            if let Ok(reg) = crate::formats::get_registry().read() {
                let formats = reg.formats.clone();
                app.active_modal = Some(crate::ui::dialogs::modal_state::ActiveModal::FileAssoc(
                    crate::ui::dialogs::file_assoc::State::new(formats),
                ));
            }
        }
        ui.add_space(8.0);
        if styled_button(ui, t!("win.remove_assoc"), &app.cached_palette).clicked() {
            app.active_modal = Some(crate::ui::dialogs::modal_state::ActiveModal::Confirm(
                crate::ui::dialogs::confirm::State::remove_file_assoc(
                    t!("win.confirm_remove_title"),
                    t!("win.confirm_remove_msg"),
                ),
            ));
        }
    });
}
