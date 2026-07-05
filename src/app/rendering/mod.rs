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

pub(crate) mod file_ops;
pub(crate) mod geometry;
pub(crate) mod plan;
pub(crate) mod plane;
pub(crate) mod standard;
pub(crate) mod tiled;
pub(crate) mod transitions;

use crate::app::ImageViewerApp;
use crate::app::rendering::geometry::main_window_canvas_rects;
use crate::app::rendering::standard::{
    should_dispatch_standard_draw, should_draw_pending_navigation_hold_frame,
};
use crate::ui::utils::draw_empty_hint;
use eframe::egui::{self, Align2, Color32, FontId, Rect, RichText, Sense, Vec2};
use rust_i18n::t;

fn should_show_loading_hint(
    res_w: u32,
    has_current_drawable: bool,
    has_pending_hold_frame: bool,
) -> bool {
    res_w == 0 && !has_current_drawable && !has_pending_hold_frame
}

#[cfg(feature = "preload-debug")]
fn canvas_drawable_kind(app: &ImageViewerApp) -> &'static str {
    if app.tiled_canvas_matches_current_index() {
        "tiled"
    } else if app.texture_cache.contains(app.current_index) {
        "sdr_texture"
    } else if app
        .current_hdr_image
        .as_ref()
        .is_some_and(|current| current.image_for_index(app.current_index).is_some())
    {
        "hdr_current"
    } else if app.hdr_image_cache.contains_key(&app.current_index) {
        "hdr_cache"
    } else {
        "unknown"
    }
}

fn draw_canvas_centered_error(ui: &mut egui::Ui, canvas_rect: Rect, text: String) {
    ui.scope_builder(egui::UiBuilder::new().max_rect(canvas_rect), |ui| {
        ui.with_layout(
            egui::Layout::centered_and_justified(egui::Direction::TopDown),
            |ui| {
                ui.set_max_width((canvas_rect.width() * 0.9).max(64.0));
                ui.add(
                    egui::Label::new(
                        RichText::new(text)
                            .font(FontId::proportional(16.0))
                            .color(Color32::from_rgb(255, 100, 100)),
                    )
                    .selectable(true)
                    .wrap()
                    .halign(egui::Align::Center),
                );
            },
        );
    });
}

impl ImageViewerApp {
    pub(crate) fn draw_image_canvas_ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        // Block canvas mouse interaction when a modal dialog is open.
        // egui::Modal renders its own dimming overlay, so we do not need to
        // draw one manually here any more.
        let any_modal_open = self.active_modal.is_some();

        // Fill the area with dark background
        egui::Frame::NONE
            .fill(self.cached_palette.canvas_bg)
            .show(ui, |ui| {
                let available = ui.available_rect_before_wrap();
                let grab = ui.style().interaction.resize_grab_radius_side;
                let embedded_panel = self.embedded_nav_panel_rect_for_area(ui.ctx(), available);
                let (screen_rect, interact_rect) =
                    main_window_canvas_rects(available, grab, embedded_panel);
                self.last_canvas_rect = Some(screen_rect);
                ui.set_clip_rect(ui.clip_rect().intersect(screen_rect));

                // Allocate the canvas for drag interaction and clicks early.
                // If a modal is open, we sense nothing to block background clicks/drags.
                let sense = if any_modal_open {
                    Sense::hover()
                } else {
                    Sense::click_and_drag()
                };
                let canvas_resp = ui.allocate_rect(interact_rect, sense);
                if canvas_resp.clicked() {
                    self.deactivate_directory_tree_list_keyboard(ui.ctx());
                }
                if !self.scanning {
                    self.flush_deferred_sdr_upload_for_index(self.current_index, ui.ctx());
                }
                let pointer_hotkey_action = if !any_modal_open && canvas_resp.hovered() {
                    self.map_pointer_button_to_action(ui.ctx())
                } else {
                    None
                };
                if let Some(action) = pointer_hotkey_action {
                    if action == crate::app::input::AppAction::SelectPixelRegion {
                        if let Some(pos) = ui.input(|i| i.pointer.interact_pos())
                            && let Some(res) = self.current_image_res
                        {
                            let img_size = Vec2::new(res.0 as f32, res.1 as f32);
                            let display_rect =
                                self.compute_plane_layout(img_size, screen_rect).dest;
                            self.handle_pixel_region_click(pos, display_rect);
                        }
                    } else {
                        self.dispatch_action(action, ui.ctx());
                    }
                }

                // ── Custom right-click context menu ──────────────────────────
                // We bypass `response.context_menu()` entirely because egui's
                // popup layer consumes the secondary-click event when it closes
                // an existing menu, making it impossible to re-open the menu
                // with a single right-click.  Instead we detect raw right-clicks
                // via `ctx.input()` and render the menu through `egui::Area`.
                if !any_modal_open && pointer_hotkey_action.is_none() {
                    let open_zone = canvas_resp.hovered().then_some(canvas_resp.rect);
                    self.try_open_image_context_menu(
                        ui.ctx(),
                        open_zone,
                        !self.image_files.is_empty(),
                    );
                    self.paint_image_context_menu_if_open(ui.ctx());
                }

                if pointer_hotkey_action.is_none()
                    && self.show_settings
                    && canvas_resp.clicked_by(egui::PointerButton::Primary)
                {
                    self.show_settings = false;
                }

                if self.image_files.is_empty() {
                    draw_empty_hint(ui, screen_rect, &self.cached_palette);
                    return;
                }

                self.prepare_display_frame(ui.ctx(), Some(frame));

                // ── Error message ─────────────────────────────────────────────
                if let Some(ref err) = self.error_message {
                    if self.show_settings && self.is_font_error {
                        // Rendered inline in draw_settings_panel — skip global overlay.
                    } else {
                        let text = if self.is_font_error {
                            format!("⚠ {}", t!("status.invalid_font"))
                        } else {
                            format!("⚠ {err}")
                        };
                        draw_canvas_centered_error(ui, screen_rect, text);
                        return;
                    }
                }

                // ── Rendering dispatch ────────────────────────────────────────
                if self.should_draw_tiled_canvas() {
                    // Large-image tiled path -> tiled.rs
                    self.draw_tiled_image(ui, screen_rect, &canvas_resp);
                } else {
                    let texture = self.texture_cache.get(self.current_index).cloned();
                    let has_hdr_plane =
                        self.current_hdr_image.as_ref().is_some_and(|current| {
                            current.image_for_index(self.current_index).is_some()
                        }) || self.hdr_image_cache.contains_key(&self.current_index);
                    let sdr_fallback_is_placeholder = self
                        .hdr_placeholder_fallback_indices
                        .contains(&self.current_index);
                    if should_draw_pending_navigation_hold_frame(
                        self.transition_start,
                        self.pending_transition_target,
                        self.current_index,
                        self.prev_texture.is_some() || self.prev_hdr_image.is_some(),
                    ) {
                        // Target index is current but transition has not started yet: keep drawing
                        // the outgoing frame instead of flashing one static frame of the new image.
                        self.draw_pending_navigation_hold_frame(ui, screen_rect);
                        ui.ctx().request_repaint();
                    } else if should_dispatch_standard_draw(
                        texture.is_some(),
                        has_hdr_plane,
                        sdr_fallback_is_placeholder,
                    ) {
                        // Standard / animated path -> standard.rs
                        self.draw_standard_image(ui, screen_rect, &canvas_resp, texture);
                    }
                }

                let current_img_size = self
                    .current_image_res
                    .map(|res| Vec2::new(res.0 as f32, res.1 as f32));

                // ── Pixel Inspector hover tooltip & canvas feedback ──────────
                if let Some(img_size) = current_img_size {
                    let display_rect = self.compute_plane_layout(img_size, screen_rect).dest;
                    self.draw_pixel_inspector_canvas_feedback(ui, display_rect);
                    self.draw_pixel_hover_tooltip(ui, screen_rect, display_rect);
                }

                // ── Global HUD / OSD overlay ──────────────────────────────────
                // Drawn outside the texture-success branch to ensure persistent display
                // during refinement, transitions, or slow tile loading.
                if self.settings.show_osd {
                    let img_size = current_img_size.unwrap_or(Vec2::ZERO);
                    let rotation = self.current_rotation;
                    let needs_swap = rotation % 2 != 0;
                    let rotated_img_size = if needs_swap {
                        Vec2::new(img_size.y, img_size.x)
                    } else {
                        img_size
                    };

                    let effective_scale =
                        self.calculate_effective_scale(rotated_img_size, screen_rect);
                    let zoom_pct =
                        (effective_scale * self.cached_pixels_per_point * 100.0).round() as u32;

                    let image_frame = self.current_image_frame_status(zoom_pct);
                    let res_w = image_frame.as_ref().map_or(0, |frame| frame.res.0);
                    if let Some(frame) = image_frame.as_ref() {
                        self.update_view_status_for_paint(frame);
                        self.osd.render_image(
                            ui,
                            screen_rect,
                            &self.cached_palette,
                            &self.last_save_error,
                        );
                    }

                    let has_current_drawable = self.tiled_canvas_matches_current_index()
                        || self.texture_cache.contains(self.current_index)
                        || self.current_hdr_image.as_ref().is_some_and(|current| {
                            current.image_for_index(self.current_index).is_some()
                        })
                        || self.hdr_image_cache.contains_key(&self.current_index);
                    let has_pending_hold_frame = should_draw_pending_navigation_hold_frame(
                        self.transition_start,
                        self.pending_transition_target,
                        self.current_index,
                        self.prev_texture.is_some() || self.prev_hdr_image.is_some(),
                    );
                    let show_loading_hint = should_show_loading_hint(
                        res_w,
                        has_current_drawable,
                        has_pending_hold_frame,
                    );
                    self.canvas_display_timing.tick_paint(
                        self.current_index,
                        show_loading_hint,
                        has_current_drawable,
                        {
                            #[cfg(feature = "preload-debug")]
                            {
                                canvas_drawable_kind(self)
                            }
                            #[cfg(not(feature = "preload-debug"))]
                            {
                                ""
                            }
                        },
                    );
                    if show_loading_hint {
                        self.osd
                            .render_loading_hint(ui, screen_rect, &self.cached_palette);
                    }

                    if !self.show_settings {
                        ui.painter().text(
                            screen_rect.right_bottom()
                                + Vec2::new(
                                    -crate::constants::OSD_MARGIN,
                                    -crate::constants::OSD_MARGIN,
                                ),
                            Align2::RIGHT_BOTTOM,
                            self.cached_keyboard_hint.as_str(),
                            FontId::proportional(crate::constants::OSD_ERROR_TEXT_SIZE),
                            self.cached_palette.osd_hint,
                        );
                    }
                }
                self.draw_hotkeys_issue_overlay(ui, screen_rect);

                // Wheel navigation/zoom: run after the canvas is allocated so egui hover
                // heuristics in `logic()` cannot swallow scroll (see `handle_main_window_wheel_input`).
                self.handle_main_window_wheel_input(ui.ctx(), screen_rect);
            });
    }

    fn draw_hotkeys_issue_overlay(&self, ui: &mut egui::Ui, screen_rect: Rect) {
        let Some(message) = self.hotkeys_status_message() else {
            return;
        };
        let bottom_inset = self.hotkeys_issue_bottom_inset();
        egui::Area::new("hotkeys_issue_overlay".into())
            .anchor(
                Align2::LEFT_BOTTOM,
                Vec2::new(crate::constants::OSD_MARGIN, -bottom_inset),
            )
            .show(ui.ctx(), |ui| {
                ui.set_max_width(
                    (screen_rect.width() - crate::constants::OSD_MARGIN * 2.0).max(64.0),
                );
                ui.add(
                    egui::Label::new(
                        RichText::new(message)
                            .font(FontId::proportional(crate::constants::OSD_ERROR_TEXT_SIZE))
                            .color(Color32::from_rgb(255, 100, 100)),
                    )
                    .wrap(),
                );
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::rendering::geometry::main_window_canvas_rects;
    use eframe::egui::Pos2;

    #[test]
    fn canvas_error_overlay_center_follows_inset_canvas_not_full_window() {
        let available = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1000.0, 800.0));
        let panel = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(200.0, 800.0));
        let (paint, _) = main_window_canvas_rects(available, 0.0, Some(panel));
        assert_ne!(paint.center().x, available.center().x);
        assert_eq!(paint.center().x, 600.0);
    }

    #[test]
    fn loading_hint_only_shows_when_nothing_is_drawable() {
        assert!(should_show_loading_hint(0, false, false));
        assert!(!should_show_loading_hint(1, false, false));
        assert!(!should_show_loading_hint(0, true, false));
        assert!(!should_show_loading_hint(0, false, true));
    }

    #[test]
    fn test_validation_error_enum() {
        use crate::ui::dialogs::file_copy_cut::ValidationError;
        let err1 = ValidationError::EmptyPath;
        let err2 = ValidationError::NotADirectory;
        assert_ne!(err1, err2);
    }

    #[test]
    fn test_file_op_error_localization() {
        use crate::app::types::FileOpError;
        let msg = FileOpError::InvalidSource.localized_message();
        assert!(!msg.is_empty());
    }
}
