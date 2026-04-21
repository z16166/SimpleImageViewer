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

use eframe::egui::{self, Color32, Pos2, Rect, Vec2};
use crate::app::{ImageViewerApp, TransitionStyle};

/// Parameters computed per-frame for the current transition animation.
pub(crate) struct TransitionParams {
    pub alpha: f32,
    pub scale: f32,
    pub offset: Vec2,
    pub prev_alpha: f32,
    pub prev_scale: f32,
    pub prev_offset: Vec2,
    pub is_animating: bool,
}

impl Default for TransitionParams {
    fn default() -> Self {
        Self {
            alpha: 1.0,
            scale: 1.0,
            offset: Vec2::ZERO,
            prev_alpha: 0.0,
            prev_scale: 1.0,
            prev_offset: Vec2::ZERO,
            is_animating: false,
        }
    }
}

const RIPPLE_SEGMENTS: u32 = 128;

impl ImageViewerApp {
    /// Compute per-frame transition animation parameters.
    /// Returns `TransitionParams` with alpha/scale/offset for both current and previous images.
    pub(crate) fn compute_transition_params(&mut self) -> TransitionParams {
        let mut p = TransitionParams::default();

        if let Some(start) = self.transition_start {
            let elapsed = start.elapsed().as_secs_f32();
            let duration = self.settings.transition_ms as f32 / 1000.0;
            if elapsed < duration {
                p.is_animating = true;
                let t = (elapsed / duration).clamp(0.0, 1.0);
                // Easing: Cubic Out
                let ease_out = 1.0 - (1.0 - t).powi(3);
                
                match self.active_transition {
                    TransitionStyle::Fade => {
                        p.alpha = ease_out;
                        p.prev_alpha = 1.0 - t;
                    }
                    TransitionStyle::ZoomFade => {
                        p.alpha = ease_out;
                        p.scale = 0.95 + 0.05 * ease_out;
                        p.prev_alpha = 1.0 - t;
                        p.prev_scale = 1.0 + 0.05 * t;
                    }
                    TransitionStyle::Slide => {
                        let dir = if self.is_next { 1.0 } else { -1.0 };
                        // screen_rect not available here; caller must pass it via offset calculation
                        // We store a normalised offset in [0,1] and the caller multiplies by width.
                        // NOTE: offset.x is stored as (-1..1) normalised; multiply by screen width in caller.
                        p.offset = Vec2::new(dir * (1.0 - ease_out), 0.0); // normalised
                        p.prev_alpha = 1.0 - t;
                    }
                    TransitionStyle::Push => {
                        let dir = if self.is_next { 1.0 } else { -1.0 };
                        p.offset = Vec2::new(dir * (1.0 - ease_out), 0.0); // normalised
                        p.prev_offset = Vec2::new(-dir * ease_out, 0.0);   // normalised
                        p.prev_alpha = 1.0;
                    }
                    TransitionStyle::PageFlip | TransitionStyle::Ripple | TransitionStyle::Curtain => {
                        // Custom rendering; keep is_animating true, no standard params needed.
                    }
                    _ => { p.is_animating = false; }
                }
            } else {
                self.transition_start = None;
                self.prev_texture = None;
            }
        }
        p
    }

    /// Draw complex per-pixel transitions (PageFlip, Ripple, Curtain).
    ///
    /// Called from `draw_standard_image` when `is_animating` and style matches one of these.
    pub(crate) fn draw_complex_transition(
        &mut self,
        ui: &mut egui::Ui,
        screen_rect: Rect,
        texture: &egui::TextureHandle,
        final_dest: Rect,
        unrotated_final_dest: Rect,
        rotation: i32,
        angle: f32,
        alpha: f32,
    ) {
        match self.active_transition {
            TransitionStyle::PageFlip => {
                if let Some(prev) = self.prev_texture.as_ref() {
                    let p_size = prev.size_vec2();
                    let p_dest = self.compute_display_rect(p_size, screen_rect);
                    let union_rect = p_dest.union(final_dest);

                    let elapsed = self.transition_start.map(|s| s.elapsed().as_secs_f32()).unwrap_or(0.0);
                    let duration = self.settings.transition_ms as f32 / 1000.0;
                    let t = (elapsed / duration).clamp(0.0, 1.0);
                    let ease_in_out = 3.0 * t * t - 2.0 * t * t * t;

                    let clip_x = if self.is_next {
                        union_rect.max.x - (union_rect.width() * ease_in_out)
                    } else {
                        union_rect.min.x + (union_rect.width() * ease_in_out)
                    };

                    // 1. Draw NEW image (revealed part, clipped)
                    let mut new_clip = union_rect;
                    if self.is_next {
                        new_clip.min.x = clip_x;
                    } else {
                        new_clip.max.x = clip_x;
                    }

                    let mut mesh = egui::Mesh::with_texture(texture.id());
                    mesh.add_rect_with_uv(
                        unrotated_final_dest,
                        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                        Color32::WHITE.linear_multiply(alpha),
                    );
                    if rotation != 0 {
                        let rot = egui::emath::Rot2::from_angle(angle);
                        let pivot = final_dest.center();
                        for v in &mut mesh.vertices {
                            v.pos = pivot + rot * (v.pos - pivot);
                        }
                    }
                    ui.painter().with_clip_rect(new_clip).add(egui::Shape::mesh(mesh));

                    // 2. Draw OLD image (unrevealed part, clipped)
                    let mut old_clip = union_rect;
                    if self.is_next {
                        old_clip.max.x = clip_x;
                    } else {
                        old_clip.min.x = clip_x;
                    }
                    ui.painter().with_clip_rect(old_clip).image(
                        prev.id(),
                        p_dest,
                        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                        Color32::WHITE,
                    );

                    // Page fold shadow
                    let shadow_width = 40.0;
                    let shadow_alpha = (1.0 - ease_in_out) * 0.4;
                    let shadow_rect = if self.is_next {
                        Rect::from_min_max(
                            Pos2::new(clip_x - shadow_width, union_rect.min.y),
                            Pos2::new(clip_x, union_rect.max.y),
                        )
                    } else {
                        Rect::from_min_max(
                            Pos2::new(clip_x, union_rect.min.y),
                            Pos2::new(clip_x + shadow_width, union_rect.max.y),
                        )
                    };

                    let color_shadow   = Color32::from_black_alpha((shadow_alpha * 255.0) as u8);
                    let color_transparent = Color32::TRANSPARENT;
                    let mut shadow_mesh = egui::Mesh::default();
                    let (c_left, c_right) = if self.is_next {
                        (color_transparent, color_shadow)
                    } else {
                        (color_shadow, color_transparent)
                    };
                    shadow_mesh.colored_vertex(shadow_rect.left_top(),     c_left);
                    shadow_mesh.colored_vertex(shadow_rect.right_top(),    c_right);
                    shadow_mesh.colored_vertex(shadow_rect.right_bottom(), c_right);
                    shadow_mesh.colored_vertex(shadow_rect.left_bottom(),  c_left);
                    shadow_mesh.add_triangle(0, 1, 2);
                    shadow_mesh.add_triangle(0, 2, 3);
                    ui.painter().add(egui::Shape::mesh(shadow_mesh));
                }
            }

            TransitionStyle::Ripple => {
                // 1. Draw OLD image as full background
                if let Some(prev) = self.prev_texture.as_ref() {
                    let p_size = prev.size_vec2();
                    let p_dest = self.compute_display_rect(p_size, screen_rect);
                    ui.painter().image(
                        prev.id(),
                        p_dest,
                        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                        Color32::WHITE,
                    );
                }

                // 2. Compute ripple state
                let elapsed = self.transition_start.map(|s| s.elapsed().as_secs_f32()).unwrap_or(0.0);
                let duration = self.settings.transition_ms as f32 / 1000.0;
                let t = (elapsed / duration).clamp(0.0, 1.0);
                let ease = 3.0 * t * t - 2.0 * t * t * t; // smoothstep

                let center = final_dest.center();
                let corners = [
                    screen_rect.left_top(), screen_rect.right_top(),
                    screen_rect.left_bottom(), screen_rect.right_bottom(),
                ];
                let max_radius = corners.iter()
                    .map(|c| center.distance(*c))
                    .fold(0.0f32, f32::max);
                let current_radius = max_radius * ease;

                // 3. Create circular textured mesh for new image (triangle fan)
                let segments = RIPPLE_SEGMENTS;
                let mut mesh = egui::Mesh::default();
                mesh.texture_id = texture.id();

                let center_uv = Pos2::new(
                    (center.x - final_dest.min.x) / final_dest.width(),
                    (center.y - final_dest.min.y) / final_dest.height(),
                );
                mesh.vertices.push(egui::epaint::Vertex { pos: center, uv: center_uv, color: Color32::WHITE });

                for i in 0..=segments {
                    let a = (i as f32 / segments as f32) * std::f32::consts::TAU;
                    let pos = Pos2::new(center.x + current_radius * a.cos(), center.y + current_radius * a.sin());
                    let uv  = Pos2::new(
                        (pos.x - final_dest.min.x) / final_dest.width(),
                        (pos.y - final_dest.min.y) / final_dest.height(),
                    );
                    mesh.vertices.push(egui::epaint::Vertex { pos, uv, color: Color32::WHITE });
                }
                for i in 0..segments {
                    mesh.indices.push(0);
                    mesh.indices.push(i + 1);
                    mesh.indices.push(i + 2);
                }

                if rotation != 0 {
                    let rot = egui::emath::Rot2::from_angle(angle);
                    let pivot = final_dest.center();
                    for v in &mut mesh.vertices {
                        v.pos = pivot + rot * (v.pos - pivot);
                    }
                }
                ui.painter().with_clip_rect(final_dest).add(egui::Shape::mesh(mesh));

                // 4. Water ripple rings at the expanding edge
                for ring in 0..4u32 {
                    let ring_radius = current_radius - ring as f32 * 14.0;
                    if ring_radius <= 2.0 { continue; }
                    let ring_alpha = (0.35 - ring as f32 * 0.09).max(0.0);
                    let ring_color = Color32::from_rgba_unmultiplied(
                        180, 215, 255,
                        (ring_alpha * 255.0) as u8,
                    );
                    let ring_width = 2.5 - ring as f32 * 0.5;
                    let points: Vec<Pos2> = (0..=segments)
                        .map(|i| {
                            let a = (i as f32 / segments as f32) * std::f32::consts::TAU;
                            Pos2::new(center.x + ring_radius * a.cos(), center.y + ring_radius * a.sin())
                        })
                        .collect();
                    ui.painter().add(egui::Shape::line(points, egui::Stroke::new(ring_width, ring_color)));
                }
            }

            TransitionStyle::Curtain => {
                if let Some(prev) = self.prev_texture.as_ref() {
                    let p_size = prev.size_vec2();
                    let p_dest = self.compute_display_rect(p_size, screen_rect);
                    let union_rect = p_dest.union(final_dest);

                    let elapsed = self.transition_start.map(|s| s.elapsed().as_secs_f32()).unwrap_or(0.0);
                    let duration = self.settings.transition_ms as f32 / 1000.0;
                    let t = (elapsed / duration).clamp(0.0, 1.0);
                    let ease = 1.0 - (1.0 - t).powi(3); // Cubic Out

                    let center_x = union_rect.center().x;
                    let half_w = union_rect.width() / 2.0;
                    let shift = ease * half_w;

                    // 1. Draw NEW image (revealed in the gap)
                    let new_clip = Rect::from_min_max(
                        Pos2::new(center_x - shift, union_rect.min.y),
                        Pos2::new(center_x + shift, union_rect.max.y),
                    );
                    ui.painter().with_clip_rect(new_clip).image(
                        texture.id(), final_dest,
                        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                        Color32::WHITE,
                    );

                    // 2. Left curtain slides left
                    let left_clip = Rect::from_min_max(
                        union_rect.left_top(),
                        Pos2::new(center_x - shift, union_rect.max.y),
                    );
                    ui.painter().with_clip_rect(left_clip).image(
                        prev.id(), p_dest.translate(Vec2::new(-shift, 0.0)),
                        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                        Color32::WHITE,
                    );

                    // 3. Right curtain slides right
                    let right_clip = Rect::from_min_max(
                        Pos2::new(center_x + shift, union_rect.min.y),
                        union_rect.right_bottom(),
                    );
                    ui.painter().with_clip_rect(right_clip).image(
                        prev.id(), p_dest.translate(Vec2::new(shift, 0.0)),
                        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                        Color32::WHITE,
                    );

                    // Gradient shadows at split edges
                    let shadow_w     = 30.0;
                    let shadow_alpha = (1.0 - ease) * 0.45;
                    let shadow_color = Color32::from_black_alpha((shadow_alpha * 255.0) as u8);
                    let transparent  = Color32::TRANSPARENT;

                    let mut lm = egui::Mesh::default();
                    let ls_rect = Rect::from_min_max(
                        Pos2::new(center_x - shift - shadow_w, union_rect.min.y),
                        Pos2::new(center_x - shift, union_rect.max.y),
                    );
                    lm.colored_vertex(ls_rect.left_top(),     transparent);
                    lm.colored_vertex(ls_rect.right_top(),    shadow_color);
                    lm.colored_vertex(ls_rect.right_bottom(), shadow_color);
                    lm.colored_vertex(ls_rect.left_bottom(),  transparent);
                    lm.add_triangle(0, 1, 2);
                    lm.add_triangle(0, 2, 3);
                    ui.painter().add(egui::Shape::mesh(lm));

                    let mut rm = egui::Mesh::default();
                    let rs_rect = Rect::from_min_max(
                        Pos2::new(center_x + shift, union_rect.min.y),
                        Pos2::new(center_x + shift + shadow_w, union_rect.max.y),
                    );
                    rm.colored_vertex(rs_rect.left_top(),     shadow_color);
                    rm.colored_vertex(rs_rect.right_top(),    transparent);
                    rm.colored_vertex(rs_rect.right_bottom(), transparent);
                    rm.colored_vertex(rs_rect.left_bottom(),  shadow_color);
                    rm.add_triangle(0, 1, 2);
                    rm.add_triangle(0, 2, 3);
                    ui.painter().add(egui::Shape::mesh(rm));
                }
            }

            _ => unreachable!("draw_complex_transition called with non-complex style"),
        }

        ui.ctx().request_repaint();
    }
}
