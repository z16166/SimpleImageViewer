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

use std::time::Instant;
use eframe::egui::{self, Color32, Pos2, Rect, Vec2};
use crate::app::{ImageViewerApp, TransitionStyle};

impl ImageViewerApp {
    /// Draw the standard (non-tiled) image rendering path, including transition animations.
    ///
    /// Called from `draw_image_canvas_ui` when there is an active texture in `texture_cache`.
    pub(crate) fn draw_standard_image(
        &mut self,
        ui: &mut egui::Ui,
        screen_rect: Rect,
        canvas_resp: &egui::Response,
        texture: egui::TextureHandle,
    ) {
        // --- Animated image frame advancement ---
        let texture = if let Some(ref mut anim) = self.animation {
            if anim.image_index == self.current_index && !anim.textures.is_empty() {
                let elapsed = anim.frame_start.elapsed();
                if elapsed >= anim.delays[anim.current_frame] {
                    anim.current_frame = (anim.current_frame + 1) % anim.textures.len();
                    anim.frame_start = Instant::now();
                }
                let remaining = anim.delays[anim.current_frame]
                    .saturating_sub(anim.frame_start.elapsed());
                ui.ctx().request_repaint_after(remaining);
                anim.textures[anim.current_frame].clone()
            } else {
                texture
            }
        } else {
            texture
        };

        // Use original image dimensions if known (Tiled previews are smaller than the real image)
        let img_size = if let Some((w, h)) = self.texture_cache.get_original_res(self.current_index) {
            Vec2::new(w as f32, h as f32)
        } else {
            texture.size_vec2()
        };

        if canvas_resp.dragged() {
            self.pan_offset += canvas_resp.drag_delta();
            // Bumping generation here ensures that if we zoom into tiled mode later,
            // or if multiple levels of tiled loaders exist, the priority is reset.
            self.generation = self.generation.wrapping_add(1);
            self.loader.set_generation(self.generation);
        }

        // --- Transition parameter computation ---
        // Slide and Push use normalised offsets; multiply by screen width here.
        let mut tp = self.compute_transition_params();
        if matches!(self.active_transition, TransitionStyle::Slide | TransitionStyle::Push) {
            tp.offset.x     *= screen_rect.width();
            tp.prev_offset.x *= screen_rect.width();
        }

        // --- Rotation setup ---
        let rotation   = self.current_rotation;
        let needs_swap = rotation % 2 != 0;
        let angle      = rotation as f32 * (std::f32::consts::PI / 2.0);

        // Compute current display rect, swapping dimensions for 90°/270° rotations
        let rotated_img_size = if needs_swap { Vec2::new(img_size.y, img_size.x) } else { img_size };
        let dest = self.compute_display_rect(rotated_img_size, screen_rect);

        let final_dest = Rect::from_center_size(
            dest.center() + tp.offset,
            dest.size() * tp.scale,
        );

        // The painter transform handles visual rotation; draw un-rotated texture into un-rotated rect.
        let unrotated_final_size = if needs_swap {
            Vec2::new(final_dest.height(), final_dest.width())
        } else {
            final_dest.size()
        };
        let unrotated_final_dest = Rect::from_center_size(final_dest.center(), unrotated_final_size);

        // --- Draw sequence ---
        if tp.is_animating && matches!(
            self.active_transition,
            TransitionStyle::PageFlip | TransitionStyle::Ripple | TransitionStyle::Curtain
        ) {
            // Complex per-pixel transitions handled in transitions.rs
            self.draw_complex_transition(
                ui, screen_rect, &texture,
                final_dest, unrotated_final_dest,
                rotation, angle, tp.alpha,
            );
        } else {
            // Standard Fade / ZoomFade / Slide / Push (and no-transition static draw):

            // 1. Draw OLD image (underneath or fading out)
            if tp.is_animating {
                if let Some(prev) = &self.prev_texture.clone() {
                    let p_size = prev.size_vec2();
                    let p_dest = self.compute_display_rect(p_size, screen_rect);
                    let p_final_dest = Rect::from_center_size(
                        p_dest.center() + tp.prev_offset,
                        p_dest.size() * tp.prev_scale,
                    );
                    ui.painter().image(
                        prev.id(),
                        p_final_dest,
                        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                        Color32::WHITE.linear_multiply(tp.prev_alpha),
                    );
                }
                ui.ctx().request_repaint();
            }

            // 2. Draw NEW image (on top, with alpha/motion)
            let mut mesh = egui::Mesh::with_texture(texture.id());
            mesh.add_rect_with_uv(
                unrotated_final_dest,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE.linear_multiply(tp.alpha),
            );
            if rotation != 0 {
                let rot   = egui::emath::Rot2::from_angle(angle);
                let pivot = final_dest.center();
                for v in &mut mesh.vertices {
                    v.pos = pivot + rot * (v.pos - pivot);
                }
            }
            ui.painter().add(egui::Shape::mesh(mesh));
        }
    }
}
