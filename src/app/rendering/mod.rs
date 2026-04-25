pub(crate) mod file_ops;
pub(crate) mod geometry;
pub(crate) mod standard;
pub(crate) mod tiled;
pub(crate) mod transitions;

use crate::app::ImageViewerApp;
use crate::ui::utils::draw_empty_hint;
use eframe::egui::{self, Align2, Color32, FontId, RichText, Sense, Vec2};
use rust_i18n::t;

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

                // ── Custom right-click context menu ──────────────────────────
                // We bypass `response.context_menu()` entirely because egui's
                // popup layer consumes the secondary-click event when it closes
                // an existing menu, making it impossible to re-open the menu
                // with a single right-click.  Instead we detect raw right-clicks
                // via `ctx.input()` and render the menu through `egui::Area`.
                if !any_modal_open && !self.image_files.is_empty() {
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
                        let area_resp = egui::Area::new(egui::Id::new("__custom_canvas_ctx_menu"))
                            .kind(egui::UiKind::Menu)
                            .order(egui::Order::Foreground)
                            .fixed_pos(pos)
                            .default_width(ctx.global_style().spacing.menu_width)
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

                if self.show_settings && canvas_resp.clicked_by(egui::PointerButton::Primary) {
                    self.show_settings = false;
                }

                if self.image_files.is_empty() {
                    draw_empty_hint(ui, screen_rect, &self.cached_palette);
                    return;
                }

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
                if self.tile_manager.is_some() {
                    // Large-image tiled path → tiled.rs
                    self.draw_tiled_image(ui, screen_rect, &canvas_resp);
                } else if let Some(texture) = self.texture_cache.get(self.current_index).cloned() {
                    // Standard / animated path → standard.rs
                    self.draw_standard_image(ui, screen_rect, &canvas_resp, texture);
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
                    let mut mode_tag = "STATIC";

                    if let Some(tm) = &self.tile_manager {
                        res_w = tm.full_width;
                        res_h = tm.full_height;
                        mode_tag = "TILED";
                    } else if let Some((w, h)) = self.current_image_res {
                        res_w = w;
                        res_h = h;
                        let threshold = crate::tile_cache::TILED_THRESHOLD
                            .load(std::sync::atomic::Ordering::Relaxed);
                        if w as u64 * h as u64 > threshold {
                            mode_tag = "TILED";
                        }
                    }

                    if res_w > 0 {
                        let current_state = crate::ui::osd::OsdState {
                            index: self.current_index,
                            total: self.image_files.len(),
                            zoom_pct,
                            res: (res_w, res_h),
                            mode: mode_tag.to_string(),
                            current_track: self.audio.get_current_track(),
                            metadata: self.audio.get_metadata(),
                            current_cue_track: self.audio.get_current_cue_track(),
                            current_pos_ms: self.audio.get_pos_ms(),
                            total_duration_ms: self.audio.get_duration_ms(),
                            cue_markers: self.audio.get_cue_markers(),
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

                    if res_w == 0 {
                        self.osd
                            .render_loading_hint(ui, screen_rect, &self.cached_palette);
                    }

                    if !self.show_settings {
                        ui.painter().text(
                            screen_rect.right_bottom() + Vec2::new(-12.0, -12.0),
                            Align2::RIGHT_BOTTOM,
                            t!("hint.keyboard").to_string(),
                            FontId::proportional(13.0),
                            self.cached_palette.osd_hint,
                        );
                    }
                }
            });
    }
}
