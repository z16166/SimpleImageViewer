// Simple Image Viewer - A high-performance, cross-platform image viewer
// Copyright (C) 2024-2026 Simple Image Viewer Contributors
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

use crate::theme::ThemePalette;
use crate::ui::dialogs::MovableModal;
use crate::ui::dialogs::modal_state::ModalResult;
use eframe::egui::{self, Color32, Context, RichText};
use rust_i18n::t;

// ── Private state ────────────────────────────────────────────────────────────

pub struct State {
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
    pub pixels: Option<Vec<Vec<[u8; 4]>>>,
    pub load_rx: Option<crossbeam_channel::Receiver<Vec<Vec<[u8; 4]>>>>,
}

fn text_color_for_bg(r: u8, g: u8, b: u8) -> Color32 {
    let lum = 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32;
    if lum > 128.0 {
        Color32::BLACK
    } else {
        Color32::WHITE
    }
}

// ── Rendering ────────────────────────────────────────────────────────────────

pub fn show(state: &mut State, ctx: &Context, palette: &ThemePalette) -> ModalResult {
    // Poll the background channel receiver for loaded pixels
    if let Some(ref rx) = state.load_rx {
        if let Ok(pixels) = rx.try_recv() {
            state.pixels = Some(pixels);
            state.load_rx = None;
        }
    }

    let mut result = ModalResult::Pending;

    let title = t!(
        "pixel_inspector.title",
        x0 = state.x0.to_string(),
        x1 = state.x1.saturating_sub(1).to_string(),
        y0 = state.y0.to_string(),
        y1 = state.y1.saturating_sub(1).to_string()
    );

    // Escape exits the modal
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        result = ModalResult::Dismissed;
    }

    MovableModal::new("pixel_region_dialog", title)
        .resizable(true)
        .default_size([640.0, 420.0])
        .min_size([320.0, 240.0])
        .show(ctx, palette, |ui| {
            // ── Fixed bottom bar: OK ──────────────────────────────────────────
            egui::Panel::bottom("pixel_region_footer")
                .resizable(false)
                .show_inside(ui, |ui| {
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if crate::ui::utils::styled_button(ui, t!("btn.ok"), palette).clicked() {
                            result = ModalResult::Dismissed;
                        }
                    });
                    ui.add_space(6.0);
                });

            // ── Header bar: Format note and warnings ──────────────────────────
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(t!("pixel_inspector.format_note"))
                        .color(palette.text_muted)
                        .small(),
                );

                let w = state.x1.saturating_sub(state.x0);
                let h = state.y1.saturating_sub(state.y0);
                if w >= crate::constants::PIXEL_REGION_WARN_DIM
                    || h >= crate::constants::PIXEL_REGION_WARN_DIM
                {
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(t!(
                            "pixel_inspector.region_size_warning",
                            w = w.to_string(),
                            h = h.to_string()
                        ))
                        .color(Color32::from_rgb(255, 100, 100))
                        .small(),
                    );
                }
            });

            ui.add_space(6.0);

            // ── Scrollable Grid or Spinner ────────────────────────────────────
            if let Some(ref pixels) = state.pixels {
                if pixels.is_empty() || pixels[0].is_empty() {
                    ui.centered_and_justified(|ui| {
                        ui.label(
                            RichText::new(t!("pixel_inspector.region_empty"))
                                .color(palette.text_muted),
                        );
                    });
                } else {
                    egui::ScrollArea::both()
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            egui::Grid::new("pixel_grid")
                                .spacing([4.0, 4.0])
                                .show(ui, |ui| {
                                    // Row 0: Headers (X coordinates)
                                    ui.label(RichText::new("Y\\X").monospace().weak());
                                    for x in state.x0..state.x1 {
                                        ui.label(
                                            RichText::new(format!("{}", x)).monospace().weak(),
                                        );
                                    }
                                    ui.end_row();

                                    // Rows 1..N: Data rows
                                    for (r_idx, row_pixels) in pixels.iter().enumerate() {
                                        let y = state.y0 + r_idx as u32;
                                        ui.label(
                                            RichText::new(format!("{}", y)).monospace().weak(),
                                        );

                                        for px in row_pixels {
                                            let [r, g, b, a] = *px;
                                            let rgba_str =
                                                format!("{:02X}{:02X}{:02X}{:02X}", r, g, b, a);

                                            // Opaque RGB color for cell background to keep contrast legible
                                            let bg_color = Color32::from_rgb(r, g, b);
                                            let text_color = text_color_for_bg(r, g, b);

                                            // Use Frame to fill the background color and center the text
                                            egui::Frame::new()
                                                .fill(bg_color)
                                                .inner_margin(egui::Margin::symmetric(6, 2))
                                                .corner_radius(egui::CornerRadius::same(2))
                                                .show(ui, |ui| {
                                                    ui.label(
                                                        RichText::new(&rgba_str)
                                                            .monospace()
                                                            .color(text_color)
                                                            .size(9.0),
                                                    );
                                                });
                                        }
                                        ui.end_row();
                                    }
                                });
                        });
                }
            } else {
                ui.centered_and_justified(|ui| {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(
                            RichText::new(t!("pixel_inspector.loading")).color(palette.text_muted),
                        );
                    });
                });
            }
        });

    result
}
