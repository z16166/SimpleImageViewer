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

use crate::app::{ImageViewerApp, TransitionStyle};
use crate::ui::utils::{settings_card, themed_labeled_toggle};
use eframe::egui;
use rust_i18n::t;

const HDR_SLIDER_VALUE_WIDTH: f32 = 90.0;
const TRANSITIONS_SLIDER_VALUE_WIDTH: f32 = 72.0;

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
                ui.label(t!("hdr.exposure_ev"));
                super::add_slider(
                    ui,
                    HDR_SLIDER_VALUE_WIDTH,
                    egui::Slider::new(exposure_slot, -8.0..=8.0)
                        .step_by(0.1)
                        .suffix(" EV"),
                    super::SliderTrackMode::Elastic,
                )
                .on_hover_text(hint);
                ui.end_row();

                ui.label(t!("hdr.sdr_white_nits"));
                super::add_slider(
                    ui,
                    HDR_SLIDER_VALUE_WIDTH,
                    egui::Slider::new(&mut app.settings.hdr_sdr_white_nits, 80.0..=400.0)
                        .step_by(1.0)
                        .suffix(" nits"),
                    super::SliderTrackMode::Elastic,
                )
                .on_hover_text(t!("hdr.sdr_white_hint"));
                ui.end_row();

                ui.label(t!("hdr.max_display_nits"));
                super::add_slider(
                    ui,
                    HDR_SLIDER_VALUE_WIDTH,
                    egui::Slider::new(&mut app.settings.hdr_max_display_nits, 100.0..=10_000.0)
                        .logarithmic(true)
                        .suffix(" nits"),
                    super::SliderTrackMode::Elastic,
                )
                .on_hover_text(t!("hdr.max_display_hint"));
                ui.end_row();
            });

        let new = (
            (
                app.settings.hdr_exposure_ev_native,
                app.settings.hdr_exposure_ev_sdr,
            ),
            app.settings.hdr_sdr_white_nits,
            app.settings.hdr_max_display_nits,
        );
        if old != new {
            let capacity_inputs_changed = old.1 != new.1 || old.2 != new.2;
            app.settings.hdr_max_display_nits = app
                .settings
                .hdr_max_display_nits
                .max(app.settings.hdr_sdr_white_nits);
            app.sync_hdr_tone_map_settings();
            if capacity_inputs_changed {
                app.refresh_ultra_hdr_decode_capacity(ui.ctx());
            }
            app.queue_save();
            ui.ctx().request_repaint();
        }
    });
}
pub(super) fn draw_slideshow_tab(app: &mut ImageViewerApp, ui: &mut egui::Ui) {
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
                    super::add_slider(
                        ui,
                        TRANSITIONS_SLIDER_VALUE_WIDTH,
                        egui::Slider::new(&mut app.settings.transition_ms, 50..=2000).suffix("ms"),
                        super::SliderTrackMode::Elastic,
                    );
                    ui.end_row();
                }
            });
        if old_style != app.settings.transition_style || old_ms != app.settings.transition_ms {
            app.queue_save();
        }
    });
}
pub(super) fn draw_hdr_settings_if_available(app: &mut ImageViewerApp, ui: &mut egui::Ui) {
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
