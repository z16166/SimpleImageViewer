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
use eframe::egui::{self, Color32, Pos2, Rect, Vec2};

pub(crate) fn draw_ripple_rings(ui: &egui::Ui, center: Pos2, current_radius: f32) {
    for ring in 0..4u32 {
        let ring_radius = current_radius - ring as f32 * 14.0;
        if ring_radius <= 2.0 {
            continue;
        }
        let ring_alpha = (0.35 - ring as f32 * 0.09).max(0.0);
        let ring_color = Color32::from_rgba_unmultiplied(180, 215, 255, (ring_alpha * 255.0) as u8);
        let ring_width = 2.5 - ring as f32 * 0.5;
        ui.painter().circle(
            center,
            ring_radius,
            Color32::TRANSPARENT,
            egui::Stroke::new(ring_width, ring_color),
        );
    }
}

pub(crate) fn draw_ripple_old_image(
    ui: &egui::Ui,
    prev: &egui::TextureHandle,
    p_dest: Rect,
    center: Pos2,
    current_radius: f32,
    rotation: i32,
    angle: f32,
) {
    let mut mesh = build_ripple_old_image_mesh(p_dest, center, current_radius, rotation, angle);
    mesh.texture_id = prev.id();
    ui.painter()
        .with_clip_rect(p_dest)
        .add(egui::Shape::mesh(mesh));
}

pub(crate) fn build_ripple_old_image_mesh(
    p_dest: Rect,
    center: Pos2,
    current_radius: f32,
    rotation: i32,
    angle: f32,
) -> egui::Mesh {
    let mut mesh = egui::Mesh::default();
    let angles = ripple_old_image_boundary_angles(p_dest, center);

    // Calculate distance from center to all four boundaries of the unrotated p_dest.
    // This removes the assumption that center is exactly at p_dest.center().
    let dist_to_right = (p_dest.max.x - center.x).max(0.0);
    let dist_to_left = (center.x - p_dest.min.x).max(0.0);
    let dist_to_bottom = (p_dest.max.y - center.y).max(0.0);
    let dist_to_top = (center.y - p_dest.min.y).max(0.0);

    for &a in &angles {
        let dx = a.cos();
        let dy = a.sin();

        // Calculate intersection distance t to the boundary of the unrotated p_dest.
        // For a ray from center, the distance depends on the direction of (dx, dy).
        let tx = if dx > 1e-6 {
            dist_to_right / dx
        } else if dx < -1e-6 {
            dist_to_left / -dx
        } else {
            f32::INFINITY
        };
        let ty = if dy > 1e-6 {
            dist_to_bottom / dy
        } else if dy < -1e-6 {
            dist_to_top / -dy
        } else {
            f32::INFINITY
        };
        let t = tx.min(ty);

        // Clip the inner circle radius to the outer boundary
        let inner_radius = current_radius.min(t);

        // Unrotated positions relative to the center
        let inner_pos = snap_pos_to_rect_edges(
            Pos2::new(center.x + inner_radius * dx, center.y + inner_radius * dy),
            p_dest,
        );
        let outer_pos =
            snap_pos_to_rect_edges(Pos2::new(center.x + t * dx, center.y + t * dy), p_dest);

        // UV coordinates mapped linearly onto p_dest [0, 1]
        let inner_uv = Pos2::new(
            ((inner_pos.x - p_dest.min.x) / p_dest.width()).clamp(0.0, 1.0),
            ((inner_pos.y - p_dest.min.y) / p_dest.height()).clamp(0.0, 1.0),
        );
        let outer_uv = Pos2::new(
            ((outer_pos.x - p_dest.min.x) / p_dest.width()).clamp(0.0, 1.0),
            ((outer_pos.y - p_dest.min.y) / p_dest.height()).clamp(0.0, 1.0),
        );

        mesh.vertices.push(egui::epaint::Vertex {
            pos: inner_pos,
            uv: inner_uv,
            color: Color32::WHITE,
        });
        mesh.vertices.push(egui::epaint::Vertex {
            pos: outer_pos,
            uv: outer_uv,
            color: Color32::WHITE,
        });
    }

    for i in 0..angles.len() - 1 {
        let i0 = (2 * i) as u32;
        let i1 = (2 * i + 1) as u32;
        let i2 = (2 * i + 2) as u32;
        let i3 = (2 * i + 3) as u32;

        // Triangle 1: inner current, outer current, outer next
        mesh.indices.push(i0);
        mesh.indices.push(i1);
        mesh.indices.push(i3);

        // Triangle 2: inner current, outer next, inner next
        mesh.indices.push(i0);
        mesh.indices.push(i3);
        mesh.indices.push(i2);
    }

    // Apply rotation if needed
    if rotation != 0 {
        let rot = egui::emath::Rot2::from_angle(angle);
        let pivot = center;
        for v in &mut mesh.vertices {
            v.pos = pivot + rot * (v.pos - pivot);
        }
    }

    mesh
}

fn ripple_old_image_boundary_angles(p_dest: Rect, center: Pos2) -> Vec<f32> {
    let mut angles = Vec::with_capacity(RIPPLE_SEGMENTS as usize + 5);
    for i in 0..=RIPPLE_SEGMENTS {
        angles.push((i as f32 / RIPPLE_SEGMENTS as f32) * std::f32::consts::TAU);
    }

    for corner in [
        p_dest.left_top(),
        p_dest.right_top(),
        p_dest.right_bottom(),
        p_dest.left_bottom(),
    ] {
        let delta = corner - center;
        if delta.length_sq() > 1e-6 {
            angles.push(delta.y.atan2(delta.x).rem_euclid(std::f32::consts::TAU));
        }
    }

    angles.sort_by(|a, b| a.total_cmp(b));
    angles.dedup_by(|a, b| (*a - *b).abs() < 1e-5);
    if angles
        .last()
        .is_none_or(|last| (*last - std::f32::consts::TAU).abs() > 1e-5)
    {
        angles.push(std::f32::consts::TAU);
    }
    angles
}

fn snap_pos_to_rect_edges(pos: Pos2, rect: Rect) -> Pos2 {
    const EDGE_EPSILON: f32 = 1e-3;

    let x = if (pos.x - rect.min.x).abs() < EDGE_EPSILON {
        rect.min.x
    } else if (pos.x - rect.max.x).abs() < EDGE_EPSILON {
        rect.max.x
    } else {
        pos.x
    };

    let y = if (pos.y - rect.min.y).abs() < EDGE_EPSILON {
        rect.min.y
    } else if (pos.y - rect.max.y).abs() < EDGE_EPSILON {
        rect.max.y
    } else {
        pos.y
    };

    Pos2::new(x, y)
}

/// Parameters computed per-frame for the current transition animation.
#[derive(Clone, Copy)]
pub(crate) struct TransitionParams {
    pub alpha: f32,
    pub scale: f32,
    pub offset: Vec2,
    pub prev_alpha: f32,
    pub prev_scale: f32,
    pub prev_offset: Vec2,
    pub is_animating: bool,
    /// Normalized animation progress in `[0.0, 1.0]`, valid only when `is_animating`.
    ///
    /// Exposed so callers that cannot use the pre-computed geometric `alpha`/`offset`
    /// (e.g. the tiled HDR path) can derive their own easing from raw `t`.
    pub t: f32,
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
            t: 0.0,
        }
    }
}

fn push_terminal_prev_offset(is_next: bool) -> Vec2 {
    let dir = if is_next { 1.0 } else { -1.0 };
    Vec2::new(-dir, 0.0)
}

pub(crate) struct ComplexTransitionDraw<'a> {
    pub(crate) screen_rect: Rect,
    pub(crate) texture: &'a egui::TextureHandle,
    pub(crate) final_dest: Rect,
    pub(crate) unrotated_final_dest: Rect,
    pub(crate) rotation: i32,
    pub(crate) angle: f32,
    pub(crate) alpha: f32,
}

pub(crate) const RIPPLE_SEGMENTS: u32 = 128;

/// ~60 Hz pacing for navigation transition frames. Prefer over immediate
/// [`egui::Context::request_repaint`] so transition steps do not stack RepaintNow wakeups.
const NAVIGATION_TRANSITION_FRAME_INTERVAL: std::time::Duration =
    std::time::Duration::from_micros(16_667);

pub(crate) fn request_navigation_transition_repaint(ctx: &egui::Context, is_animating: bool) {
    if is_animating {
        ctx.request_repaint_after(NAVIGATION_TRANSITION_FRAME_INTERVAL);
    }
}

impl ImageViewerApp {
    /// Compute per-frame transition animation parameters.
    /// Returns `TransitionParams` with alpha/scale/offset for both current and previous images.
    pub(crate) fn compute_transition_params(&mut self) -> TransitionParams {
        // Fast path: idle frames skip Option unwrap + elapsed/ease work (#39).
        let Some(start) = self.transition_start else {
            return TransitionParams::default();
        };

        let mut p = TransitionParams::default();
        let elapsed = start.elapsed().as_secs_f32();
        let duration = self.settings.transition_ms as f32 / 1000.0;
        if elapsed < duration {
            p.is_animating = true;
            let t = (elapsed / duration).clamp(0.0, 1.0);
            p.t = t;
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
                    // Slide-over semantics: keep the old image stable underneath
                    // while the new image moves in above it.
                    p.prev_alpha = 1.0;
                }
                TransitionStyle::Push => {
                    let dir = if self.is_next { 1.0 } else { -1.0 };
                    p.offset = Vec2::new(dir * (1.0 - ease_out), 0.0); // normalised
                    p.prev_offset = Vec2::new(-dir * ease_out, 0.0); // normalised
                    p.prev_alpha = 1.0;
                }
                TransitionStyle::PageFlip | TransitionStyle::Ripple | TransitionStyle::Curtain => {
                    // Custom rendering; keep is_animating true, no standard params needed.
                }
                _ => {
                    p.is_animating = false;
                }
            }
        } else if self.transition_end_hold {
            // The t=1.0 hold frame has been drawn. Only now do we mark the transition as
            // settled and release the outgoing frame. Starting the post-transition timers here
            // intentionally shifts them by one frame (~8-16ms) so background uploads cannot
            // begin on the same frame that bridges animated and static rendering.
            self.transition_end_hold = false;
            self.transition_start = None;
            self.transition_settled_at = Some(std::time::Instant::now());
            self.prev_texture = None;
            self.prev_hdr_image = None;
            self.prev_transition_rect = None;
        } else {
            // Hold one frame at t=1.0 on the geometric path so the last animated
            // frame matches the first static HDR draw (avoids end-of-flip flash). Alpha
            // transitions end with the new image fully opaque; Slide/Push end with both
            // offsets reset; PageFlip/Ripple/Curtain already render their final geometry via
            // custom paths, so they only need `is_animating` and `t=1.0` preserved.
            self.transition_end_hold = true;
            p.is_animating = true;
            p.t = 1.0;
            match self.active_transition {
                TransitionStyle::Fade => {
                    p.alpha = 1.0;
                    p.prev_alpha = 0.0;
                }
                TransitionStyle::ZoomFade => {
                    p.alpha = 1.0;
                    p.scale = 1.0;
                    p.prev_alpha = 0.0;
                    p.prev_scale = 1.0;
                }
                TransitionStyle::Slide => {
                    p.offset = Vec2::ZERO;
                    p.prev_offset = Vec2::ZERO;
                    p.prev_alpha = 1.0;
                }
                TransitionStyle::Push => {
                    p.offset = Vec2::ZERO;
                    p.prev_offset = push_terminal_prev_offset(self.is_next);
                    p.prev_alpha = 1.0;
                }
                TransitionStyle::PageFlip | TransitionStyle::Ripple | TransitionStyle::Curtain => {}
                _ => {
                    p.is_animating = false;
                }
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
        params: ComplexTransitionDraw<'_>,
    ) {
        let ComplexTransitionDraw {
            screen_rect,
            texture,
            final_dest,
            unrotated_final_dest,
            rotation,
            angle,
            alpha,
        } = params;
        match self.active_transition {
            TransitionStyle::PageFlip => {
                self.draw_page_flip_transition(
                    ui,
                    crate::app::rendering::standard::PageFlipTransitionDraw {
                        screen_rect,
                        texture,
                        final_dest,
                        unrotated_final_dest,
                        rotation,
                        angle,
                        alpha,
                    },
                );
            }
            TransitionStyle::Ripple => {
                self.draw_ripple_transition(ui, screen_rect, texture, final_dest, rotation, angle);
            }
            TransitionStyle::Curtain => {
                self.draw_curtain_transition(ui, screen_rect, texture, final_dest, alpha);
            }

            TransitionStyle::None
            | TransitionStyle::Fade
            | TransitionStyle::ZoomFade
            | TransitionStyle::Slide
            | TransitionStyle::Push
            | TransitionStyle::Random => {
                unreachable!(
                    "draw_complex_transition called with non-complex style: {:?}",
                    self.active_transition
                );
            }
        }

        request_navigation_transition_repaint(ui.ctx(), true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_transition_params_idle_returns_default() {
        let mut app = crate::app::image_management::tests::make_test_app();
        assert!(app.transition_start.is_none());
        let p = app.compute_transition_params();
        assert!(!p.is_animating);
        assert_eq!(p.alpha, 1.0);
        assert_eq!(p.t, 0.0);
        assert!(app.transition_start.is_none());
    }

    #[test]
    fn push_terminal_prev_offset_keeps_outgoing_frame_off_canvas() {
        assert_eq!(push_terminal_prev_offset(true), Vec2::new(-1.0, 0.0));
        assert_eq!(push_terminal_prev_offset(false), Vec2::new(1.0, 0.0));
    }

    #[test]
    fn ripple_old_image_mesh_outer_boundary_includes_corners() {
        let p_dest = Rect::from_min_max(Pos2::new(640.0, 0.0), Pos2::new(1280.0, 1080.0));
        let center = p_dest.center();
        let mesh = build_ripple_old_image_mesh(p_dest, center, 320.0, 0, 0.0);
        let corners = [
            p_dest.left_top(),
            p_dest.right_top(),
            p_dest.left_bottom(),
            p_dest.right_bottom(),
        ];

        for corner in corners {
            assert!(
                mesh.vertices.iter().any(|v| v.pos.distance(corner) < 1e-4),
                "old image mesh outer boundary must include corner {corner:?}"
            );
        }
    }

    #[test]
    fn test_build_ripple_old_image_mesh_zero_radius() {
        let p_dest = Rect::from_min_size(Pos2::new(100.0, 100.0), Vec2::new(200.0, 100.0));
        let center = p_dest.center();
        let mesh = build_ripple_old_image_mesh(p_dest, center, 0.0, 0, 0.0);
        let angle_count = ripple_old_image_boundary_angles(p_dest, center).len();

        assert_eq!(mesh.vertices.len(), 2 * angle_count);
        assert_eq!(mesh.indices.len(), 6 * (angle_count - 1));

        // Since current_radius is 0.0, all inner vertices should be exactly at the center
        // and their UVs should be exactly at [0.5, 0.5] (the center of p_dest)
        for i in 0..angle_count {
            let inner_v = &mesh.vertices[2 * i];
            assert!((inner_v.pos.x - center.x).abs() < 1e-4);
            assert!((inner_v.pos.y - center.y).abs() < 1e-4);
            assert!((inner_v.uv.x - 0.5).abs() < 1e-4);
            assert!((inner_v.uv.y - 0.5).abs() < 1e-4);
        }
    }

    #[test]
    fn test_build_ripple_old_image_mesh_large_radius_clamping() {
        let p_dest = Rect::from_min_size(Pos2::new(100.0, 100.0), Vec2::new(200.0, 100.0));
        let center = p_dest.center();
        // A radius of 1000.0 is much larger than the distance to any boundary of p_dest.
        // Thus, all inner vertices must be clamped to the outer vertices.
        let mesh = build_ripple_old_image_mesh(p_dest, center, 1000.0, 0, 0.0);

        for i in 0..=RIPPLE_SEGMENTS as usize {
            let inner_v = &mesh.vertices[2 * i];
            let outer_v = &mesh.vertices[2 * i + 1];

            // Verify inner vertices are clamped to outer vertices
            assert!((inner_v.pos.x - outer_v.pos.x).abs() < 1e-4);
            assert!((inner_v.pos.y - outer_v.pos.y).abs() < 1e-4);
            assert!((inner_v.uv.x - outer_v.uv.x).abs() < 1e-4);
            assert!((inner_v.uv.y - outer_v.uv.y).abs() < 1e-4);
        }
    }

    #[test]
    fn test_build_ripple_old_image_mesh_rotation() {
        let p_dest = Rect::from_min_size(Pos2::new(100.0, 100.0), Vec2::new(200.0, 100.0));
        let center = p_dest.center();
        let angle = std::f32::consts::FRAC_PI_2; // 90 degrees

        let mesh_unrotated = build_ripple_old_image_mesh(p_dest, center, 10.0, 0, 0.0);
        let mesh_rotated = build_ripple_old_image_mesh(p_dest, center, 10.0, 1, angle);

        assert_eq!(mesh_unrotated.vertices.len(), mesh_rotated.vertices.len());

        for (v_unrot, v_rot) in mesh_unrotated
            .vertices
            .iter()
            .zip(mesh_rotated.vertices.iter())
        {
            // Verify UVs are identical (rotation is post-process, UV mapping should not change)
            assert!((v_unrot.uv.x - v_rot.uv.x).abs() < 1e-4);
            assert!((v_unrot.uv.y - v_rot.uv.y).abs() < 1e-4);

            // Verify positions are rotated by 90 degrees around center
            // (x - cx, y - cy) rotated by 90 degrees -> (-(y - cy), x - cx)
            let dx = v_unrot.pos.x - center.x;
            let dy = v_unrot.pos.y - center.y;
            let expected_rot_x = center.x - dy;
            let expected_rot_y = center.y + dx;

            assert!(
                (v_rot.pos.x - expected_rot_x).abs() < 1e-4,
                "Expected {}, got {}",
                expected_rot_x,
                v_rot.pos.x
            );
            assert!(
                (v_rot.pos.y - expected_rot_y).abs() < 1e-4,
                "Expected {}, got {}",
                expected_rot_y,
                v_rot.pos.y
            );
        }
    }

    #[test]
    fn test_build_ripple_old_image_mesh_non_concentric_center() {
        let p_dest = Rect::from_min_size(Pos2::new(100.0, 100.0), Vec2::new(200.0, 100.0));
        // Offset center to the right: Pos2(250.0, 150.0) -> right boundary is only 50px away
        let center = Pos2::new(250.0, 150.0);
        let mesh = build_ripple_old_image_mesh(p_dest, center, 100.0, 0, 0.0);

        // Ray to the right (a = 0): dx = 1.0, dy = 0.0
        // Expected boundary distance: dist_to_right = max.x - center.x = 300.0 - 250.0 = 50.0
        // The vertex for i = 0 (ray right) should be at x = 250 + 50 = 300
        let outer_v_right = &mesh.vertices[1]; // outer vertex for i = 0
        assert!((outer_v_right.pos.x - 300.0).abs() < 1e-4);
    }
}
