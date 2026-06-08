pub(crate) mod file_ops;
pub(crate) mod geometry;
pub(crate) mod plan;
pub(crate) mod plane;
pub(crate) mod standard;
pub(crate) mod tiled;
pub(crate) mod transitions;

use crate::app::ImageViewerApp;
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

impl ImageViewerApp {
    pub(crate) fn draw_image_canvas_ui(&mut self, ui: &mut egui::Ui) {
        // Block canvas mouse interaction when a modal dialog is open.
        // egui::Modal renders its own dimming overlay, so we do not need to
        // draw one manually here any more.
        let any_modal_open = self.active_modal.is_some();

        // Fill the area with dark background
        egui::Frame::NONE
            .fill(self.cached_palette.canvas_bg)
            .show(ui, |ui| {
                let screen_rect = ui.max_rect();

                // Allocate the whole viewport for drag interaction and clicks early.
                // If a modal is open, we sense nothing to block background clicks/drags.
                let sense = if any_modal_open {
                    Sense::hover()
                } else {
                    Sense::click_and_drag()
                };
                let canvas_resp = ui.allocate_rect(screen_rect, sense);
                self.flush_deferred_sdr_upload_for_index(self.current_index, ui.ctx());
                let pointer_hotkey_action = if !any_modal_open && canvas_resp.hovered() {
                    self.map_pointer_button_to_action(ui.ctx())
                } else {
                    None
                };
                if let Some(action) = pointer_hotkey_action {
                    self.dispatch_action(action, ui.ctx());
                }

                // ── Custom right-click context menu ──────────────────────────
                // We bypass `response.context_menu()` entirely because egui's
                // popup layer consumes the secondary-click event when it closes
                // an existing menu, making it impossible to re-open the menu
                // with a single right-click.  Instead we detect raw right-clicks
                // via `ctx.input()` and render the menu through `egui::Area`.
                if !any_modal_open
                    && pointer_hotkey_action.is_none()
                    && !self.image_files.is_empty()
                {
                    let ctx = ui.ctx().clone();
                    let raw_secondary = ctx.input(|i| i.pointer.secondary_clicked());
                    let interact_pos = ctx.input(|i| i.pointer.interact_pos());

                    if raw_secondary && canvas_resp.hovered() {
                        if let Some(pos) = interact_pos {
                            self.context_menu_pos = Some(pos);
                        }
                    }

                    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                        self.context_menu_pos = None;
                    }

                    if let Some(pos) = self.context_menu_pos {
                        let menu_id = egui::Id::new(format!(
                            "__custom_canvas_ctx_menu_{}",
                            self.settings.language
                        ));
                        let area_resp = egui::Area::new(menu_id)
                            .kind(egui::UiKind::Menu)
                            .order(egui::Order::Foreground)
                            .fixed_pos(pos)
                            .sense(Sense::hover())
                            .show(&ctx, |ui| {
                                egui::Frame::menu(ui.style()).show(ui, |ui| {
                                    ui.with_layout(
                                        egui::Layout::top_down_justified(egui::Align::LEFT),
                                        |ui| self.draw_context_menu_items(ui),
                                    );
                                });
                            });

                        let menu_rect = area_resp.response.rect;
                        let primary_clicked = ctx.input(|i| i.pointer.primary_clicked());
                        if primary_clicked {
                            if let Some(pp) = interact_pos {
                                if !menu_rect.contains(pp) {
                                    self.context_menu_pos = None;
                                }
                            }
                        }
                        if area_resp.response.should_close() {
                            self.context_menu_pos = None;
                        }
                    }
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

                self.prepare_display_frame(ui.ctx());

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
                        egui::Area::new("error_display".into())
                            .anchor(Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                            .show(ui.ctx(), |ui| {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(text)
                                            .font(FontId::proportional(16.0))
                                            .color(Color32::from_rgb(255, 100, 100)),
                                    )
                                    .selectable(true)
                                    .halign(egui::Align::Center),
                                );
                            });
                        return;
                    }
                }

                // ── Rendering dispatch ────────────────────────────────────────
                if self.tiled_canvas_matches_current_index() {
                    // Large-image tiled path → tiled.rs
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
                        // Standard / animated path → standard.rs
                        self.draw_standard_image(ui, screen_rect, &canvas_resp, texture);
                    }
                }

                // ── Global HUD / OSD overlay ──────────────────────────────────
                // Drawn outside the texture-success branch to ensure persistent display
                // during refinement, transitions, or slow tile loading.
                if self.settings.show_osd {
                    let res = if let Some(r) = self.current_image_res {
                        r
                    } else {
                        (0, 0)
                    };
                    let img_size = Vec2::new(res.0 as f32, res.1 as f32);
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

                    let mut res_w = 0u32;
                    let mut res_h = 0u32;
                    let mut mode_tag = t!("osd.mode.static").to_string();

                    if self.tiled_canvas_matches_current_index() {
                        if let Some(tm) = &self.tile_manager {
                            res_w = tm.full_width;
                            res_h = tm.full_height;
                            mode_tag = t!("osd.mode.tiled").to_string();
                        }
                    } else if let Some((w, h)) = self.current_image_res {
                        res_w = w;
                        res_h = h;
                        let threshold = crate::tile_cache::TILED_THRESHOLD
                            .load(std::sync::atomic::Ordering::Relaxed);
                        if w as u64 * h as u64 > threshold {
                            mode_tag = t!("osd.mode.tiled").to_string();
                        }
                    }

                    if res_w > 0 {
                        let file_size_bytes = self
                            .file_byte_len_by_index
                            .get(self.current_index)
                            .copied()
                            .unwrap_or(0);
                        let current_state = crate::ui::osd::OsdState {
                            index: self.current_index,
                            total: self.image_files.len(),
                            zoom_pct,
                            res: (res_w, res_h),
                            file_size_bytes,
                            mode: mode_tag.to_string(),
                            current_track: self.audio.get_current_track(),
                            metadata: self.audio.get_metadata(),
                            current_cue_track: self.audio.get_current_cue_track(),
                            current_pos_ms: self.audio.get_pos_ms(),
                            total_duration_ms: self.audio.get_duration_ms(),
                            cue_markers: self.audio.get_cue_markers(),
                            hdr_status: self.current_hdr_osd_tag(),
                        };
                        let fname = self.image_files[self.current_index]
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy();
                        self.osd.render(
                            ui,
                            screen_rect,
                            &current_state,
                            &fname,
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
                    if should_show_loading_hint(res_w, has_current_drawable, has_pending_hold_frame)
                    {
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
                            t!("hint.keyboard").to_string(),
                            FontId::proportional(crate::constants::OSD_ERROR_TEXT_SIZE),
                            self.cached_palette.osd_hint,
                        );
                    }
                }
                self.draw_hotkeys_issue_overlay(ui, screen_rect);

                // Wheel navigation/zoom: run after the canvas is allocated so egui hover
                // heuristics in `logic()` cannot swallow scroll (see `handle_main_window_wheel_input`).
                self.handle_main_window_wheel_input(ui.ctx());
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

    #[test]
    fn loading_hint_only_shows_when_nothing_is_drawable() {
        assert!(should_show_loading_hint(0, false, false));
        assert!(!should_show_loading_hint(1, false, false));
        assert!(!should_show_loading_hint(0, true, false));
        assert!(!should_show_loading_hint(0, false, true));
    }
}
