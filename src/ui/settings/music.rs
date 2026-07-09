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
use crate::ui::utils::{
    middle_truncate, path_display_box, settings_card, stable_selectable_label, styled_button,
    styled_button_widget, themed_labeled_toggle,
};
use eframe::egui::{self, Color32, RichText};
use rust_i18n::t;
use std::time::Instant;

const MUSIC_SLIDER_VALUE_WIDTH: f32 = 60.0;

pub(super) fn draw_music_tab(
    app: &mut ImageViewerApp,
    ui: &mut egui::Ui,
    open_music_file: &mut bool,
    open_music_dir: &mut bool,
    music_enabled_changed: &mut bool,
) {
    ui.vertical(|ui| {
        let palette = app.cached_palette;
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
                            .is_some_and(|t| t.elapsed().as_secs() >= 30);
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
                            super::grid_label(
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
                                        if stable_selectable_label(
                                            ui,
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
                                            if stable_selectable_label(ui, is_selected, short_name)
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

                            super::grid_label(
                                ui,
                                RichText::new(t!("label.volume"))
                                    .color(app.cached_palette.text_muted),
                            );
                            let old_vol = app.settings.volume;
                            let resp = super::add_slider(
                                ui,
                                MUSIC_SLIDER_VALUE_WIDTH,
                                egui::Slider::new(&mut app.settings.volume, 0.0..=1.0)
                                    .show_value(true)
                                    .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
                                super::SliderTrackMode::Elastic,
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
