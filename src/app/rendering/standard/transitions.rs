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

use std::sync::Arc;

use crate::hdr::renderer::HdrRenderOutputMode;

use super::helpers::resolve_transition_prev_layout;
use crate::app::ImageViewerApp;
use eframe::egui::{self, Color32, Pos2, Rect, Vec2};

impl ImageViewerApp {
    pub(crate) fn transition_normalized_t(&self) -> f32 {
        let elapsed = self
            .transition_start
            .map(|s| s.elapsed().as_secs_f32())
            .unwrap_or(0.0);
        let duration = self.settings.transition_ms as f32 / 1000.0;
        (elapsed / duration).clamp(0.0, 1.0)
    }

    /// Outgoing-frame layout for complex transitions whose destination is an SDR texture.
    pub(crate) fn transition_prev_layout(
        &self,
        screen_rect: Rect,
        final_dest: Rect,
    ) -> (Rect, Rect, bool) {
        let prev_size = self
            .prev_hdr_image
            .as_ref()
            .map(|h| Vec2::new(h.width as f32, h.height as f32))
            .or_else(|| self.prev_texture.as_ref().map(|t| t.size_vec2()));
        let has_prev = self.prev_texture.is_some() || self.prev_hdr_image.is_some();
        resolve_transition_prev_layout(
            screen_rect,
            final_dest,
            prev_size,
            self.prev_transition_rect,
            has_prev,
            |size, rect| self.compute_display_rect(size, rect),
        )
    }

    /// Draw the outgoing frame clipped to `clip`, preferring the HDR float plane when available.
    pub(crate) fn draw_outgoing_transition_frame_clipped(
        &self,
        ui: &mut egui::Ui,
        _screen_rect: Rect,
        clip: Rect,
        p_dest: Rect,
        rotation: i32,
        alpha: f32,
        hdr_output: Option<(wgpu::TextureFormat, HdrRenderOutputMode)>,
    ) {
        if let Some(prev_hdr) = self.prev_hdr_image.as_ref() {
            let hdr_draw = hdr_output.or_else(|| self.effective_hdr_display_output());
            if let Some((target_format, hdr_output_mode)) = hdr_draw {
                self.draw_hdr_image_plane_clipped(
                    ui,
                    clip,
                    p_dest,
                    Arc::clone(prev_hdr),
                    self.hdr_renderer.tone_map,
                    target_format,
                    hdr_output_mode,
                    rotation,
                    alpha,
                    None,
                );
                return;
            }
        }
        if let Some(prev) = self.prev_texture.as_ref() {
            ui.painter().with_clip_rect(clip).image(
                prev.id(),
                p_dest,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE.linear_multiply(alpha),
            );
        }
    }

    pub(crate) fn draw_outgoing_transition_frame_ripple(
        &self,
        ui: &mut egui::Ui,
        screen_rect: Rect,
        p_dest: Rect,
        center: Pos2,
        current_radius: f32,
        rotation: i32,
        angle: f32,
    ) {
        if let Some(prev_hdr) = self.prev_hdr_image.as_ref()
            && let Some((target_format, hdr_output_mode)) = self.effective_hdr_display_output()
        {
            let ppp = ui.ctx().pixels_per_point();
            self.draw_hdr_image_plane_clipped(
                ui,
                screen_rect,
                p_dest,
                Arc::clone(prev_hdr),
                self.hdr_renderer.tone_map,
                target_format,
                hdr_output_mode,
                rotation,
                1.0,
                Some((
                    center,
                    current_radius,
                    ppp,
                    crate::hdr::renderer::RIPPLE_CLIP_OUTSIDE,
                )),
            );
            return;
        }
        if let Some(prev) = self.prev_texture.as_ref() {
            crate::app::rendering::transitions::draw_ripple_old_image(
                ui,
                prev,
                p_dest,
                center,
                current_radius,
                rotation,
                angle,
            );
        }
    }

    fn draw_curtain_split_shadows(
        ui: &egui::Ui,
        union_rect: Rect,
        center_x: f32,
        shift: f32,
        ease: f32,
    ) {
        let shadow_w = 30.0;
        let shadow_alpha = (1.0 - ease) * 0.45;
        let shadow_color = Color32::from_black_alpha((shadow_alpha * 255.0) as u8);
        let transparent = Color32::TRANSPARENT;

        let mut lm = egui::Mesh::default();
        let ls_rect = Rect::from_min_max(
            Pos2::new(center_x - shift - shadow_w, union_rect.min.y),
            Pos2::new(center_x - shift, union_rect.max.y),
        );
        lm.colored_vertex(ls_rect.left_top(), transparent);
        lm.colored_vertex(ls_rect.right_top(), shadow_color);
        lm.colored_vertex(ls_rect.right_bottom(), shadow_color);
        lm.colored_vertex(ls_rect.left_bottom(), transparent);
        lm.add_triangle(0, 1, 2);
        lm.add_triangle(0, 2, 3);
        ui.painter().add(egui::Shape::mesh(lm));

        let mut rm = egui::Mesh::default();
        let rs_rect = Rect::from_min_max(
            Pos2::new(center_x + shift, union_rect.min.y),
            Pos2::new(center_x + shift + shadow_w, union_rect.max.y),
        );
        rm.colored_vertex(rs_rect.left_top(), shadow_color);
        rm.colored_vertex(rs_rect.right_top(), transparent);
        rm.colored_vertex(rs_rect.right_bottom(), transparent);
        rm.colored_vertex(rs_rect.left_bottom(), shadow_color);
        rm.add_triangle(0, 1, 2);
        rm.add_triangle(0, 2, 3);
        ui.painter().add(egui::Shape::mesh(rm));
    }

    /// Page-flip transition for SDR destination textures.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn draw_page_flip_transition(
        &self,
        ui: &mut egui::Ui,
        screen_rect: Rect,
        texture: &egui::TextureHandle,
        final_dest: Rect,
        unrotated_final_dest: Rect,
        rotation: i32,
        angle: f32,
        alpha: f32,
    ) {
        let (p_dest, union_rect, has_prev) = self.transition_prev_layout(screen_rect, final_dest);
        let ease_in_out = {
            let t = self.transition_normalized_t();
            3.0 * t * t - 2.0 * t * t * t
        };

        let clip_x = if self.is_next {
            union_rect.max.x - (union_rect.width() * ease_in_out)
        } else {
            union_rect.min.x + (union_rect.width() * ease_in_out)
        };

        let mut new_clip = union_rect;
        if self.is_next {
            new_clip.min.x = clip_x;
        } else {
            new_clip.max.x = clip_x;
        }

        if has_prev {
            let mut old_clip = union_rect;
            if self.is_next {
                old_clip.max.x = clip_x;
            } else {
                old_clip.min.x = clip_x;
            }
            self.draw_outgoing_transition_frame_clipped(
                ui,
                screen_rect,
                old_clip,
                p_dest,
                rotation,
                1.0,
                None,
            );
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
        ui.painter()
            .with_clip_rect(new_clip)
            .add(egui::Shape::mesh(mesh));

        if has_prev {
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

            let color_shadow = Color32::from_black_alpha((shadow_alpha * 255.0) as u8);
            let color_transparent = Color32::TRANSPARENT;
            let mut shadow_mesh = egui::Mesh::default();
            let (c_left, c_right) = if self.is_next {
                (color_transparent, color_shadow)
            } else {
                (color_shadow, color_transparent)
            };
            shadow_mesh.colored_vertex(shadow_rect.left_top(), c_left);
            shadow_mesh.colored_vertex(shadow_rect.right_top(), c_right);
            shadow_mesh.colored_vertex(shadow_rect.right_bottom(), c_right);
            shadow_mesh.colored_vertex(shadow_rect.left_bottom(), c_left);
            shadow_mesh.add_triangle(0, 1, 2);
            shadow_mesh.add_triangle(0, 2, 3);
            ui.painter().add(egui::Shape::mesh(shadow_mesh));
        }
    }

    /// Curtain transition for SDR destination textures.
    pub(crate) fn draw_curtain_transition(
        &self,
        ui: &mut egui::Ui,
        screen_rect: Rect,
        texture: &egui::TextureHandle,
        final_dest: Rect,
        _alpha: f32,
    ) {
        let (p_dest, union_rect, has_prev) = self.transition_prev_layout(screen_rect, final_dest);
        let t = self.transition_normalized_t();
        let ease = 1.0 - (1.0 - t).powi(3);

        let center_x = union_rect.center().x;
        let half_w = union_rect.width() / 2.0;
        let shift = ease * half_w;

        let new_clip = Rect::from_min_max(
            Pos2::new(center_x - shift, union_rect.min.y),
            Pos2::new(center_x + shift, union_rect.max.y),
        );
        ui.painter().with_clip_rect(new_clip).image(
            texture.id(),
            final_dest,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );

        if has_prev {
            let left_clip = Rect::from_min_max(
                union_rect.left_top(),
                Pos2::new(center_x - shift, union_rect.max.y),
            );
            let right_clip = Rect::from_min_max(
                Pos2::new(center_x + shift, union_rect.min.y),
                union_rect.right_bottom(),
            );
            self.draw_outgoing_transition_frame_clipped(
                ui,
                screen_rect,
                left_clip,
                p_dest.translate(Vec2::new(-shift, 0.0)),
                0,
                1.0,
                None,
            );
            self.draw_outgoing_transition_frame_clipped(
                ui,
                screen_rect,
                right_clip,
                p_dest.translate(Vec2::new(shift, 0.0)),
                0,
                1.0,
                None,
            );
            Self::draw_curtain_split_shadows(ui, union_rect, center_x, shift, ease);
        }
    }

    /// Ripple transition for SDR destination textures.
    pub(crate) fn draw_ripple_transition(
        &self,
        ui: &mut egui::Ui,
        screen_rect: Rect,
        texture: &egui::TextureHandle,
        final_dest: Rect,
        rotation: i32,
        angle: f32,
    ) {
        let t = self.transition_normalized_t();
        let ease = 3.0 * t * t - 2.0 * t * t * t;

        let center = final_dest.center();
        let corners = [
            screen_rect.left_top(),
            screen_rect.right_top(),
            screen_rect.left_bottom(),
            screen_rect.right_bottom(),
        ];
        let max_radius = corners
            .iter()
            .map(|c| center.distance(*c))
            .fold(0.0f32, f32::max);
        let current_radius = max_radius * ease;

        let (p_dest, _, has_prev) = self.transition_prev_layout(screen_rect, final_dest);
        if has_prev {
            self.draw_outgoing_transition_frame_ripple(
                ui,
                screen_rect,
                p_dest,
                center,
                current_radius,
                rotation,
                angle,
            );
        }

        let segments = crate::app::rendering::transitions::RIPPLE_SEGMENTS;
        let mut mesh = egui::Mesh::default();
        mesh.texture_id = texture.id();

        let center_uv = Pos2::new(
            (center.x - final_dest.min.x) / final_dest.width(),
            (center.y - final_dest.min.y) / final_dest.height(),
        );
        mesh.vertices.push(egui::epaint::Vertex {
            pos: center,
            uv: center_uv,
            color: Color32::WHITE,
        });

        for i in 0..=segments {
            let a = (i as f32 / segments as f32) * std::f32::consts::TAU;
            let pos = Pos2::new(
                center.x + current_radius * a.cos(),
                center.y + current_radius * a.sin(),
            );
            let uv = Pos2::new(
                (pos.x - final_dest.min.x) / final_dest.width(),
                (pos.y - final_dest.min.y) / final_dest.height(),
            );
            mesh.vertices.push(egui::epaint::Vertex {
                pos,
                uv,
                color: Color32::WHITE,
            });
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
        ui.painter()
            .with_clip_rect(final_dest)
            .add(egui::Shape::mesh(mesh));

        crate::app::rendering::transitions::draw_ripple_rings(ui, center, current_radius);
    }
}
