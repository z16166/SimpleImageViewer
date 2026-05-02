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

use crate::app::rendering::geometry::PlaneLayout;
use crate::app::rendering::plan::{RenderPlan, RenderShape};
use crate::app::rendering::plane::{
    PlaneBackendKind, PlaneDrawSource, draw_plane, draw_sdr_texture_plane, hdr_image_plane_rect,
};
use crate::app::{ImageViewerApp, TransitionStyle};
use crate::hdr::renderer::HdrRenderOutputMode;
use crate::hdr::types::{HdrImageBuffer, HdrToneMapSettings};
use eframe::egui::{self, Color32, Pos2, Rect, Vec2};
use std::sync::Arc;
use std::time::Instant;

pub(crate) fn should_use_hdr_callback(transition: TransitionStyle, is_animating: bool) -> bool {
    !is_animating || !matches!(transition, TransitionStyle::Ripple)
}

pub(crate) fn should_draw_static_hdr_immediately(
    plan: &RenderPlan,
    transition: TransitionStyle,
    is_animating: bool,
) -> bool {
    plan.backend == PlaneBackendKind::Hdr
        && !(is_animating
            && matches!(
                transition,
                TransitionStyle::PageFlip | TransitionStyle::Curtain
            ))
}

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
                let remaining =
                    anim.delays[anim.current_frame].saturating_sub(anim.frame_start.elapsed());
                ui.ctx().request_repaint_after(remaining);
                anim.textures[anim.current_frame].clone()
            } else {
                texture
            }
        } else {
            texture
        };

        // Use original image dimensions if known (Tiled previews are smaller than the real image)
        let img_size = if let Some((w, h)) = self.texture_cache.get_original_res(self.current_index)
        {
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
        let hdr_image = self
            .current_hdr_image
            .as_ref()
            .and_then(|current| current.image_for_index(self.current_index))
            .cloned();
        let render_plan = self.build_render_plan(RenderShape::Static, hdr_image.is_some());
        if should_draw_static_hdr_immediately(&render_plan, self.active_transition, tp.is_animating)
        {
            self.transition_start = None;
            self.prev_texture = None;
            tp = crate::app::rendering::transitions::TransitionParams::default();
        }
        if matches!(
            self.active_transition,
            TransitionStyle::Slide | TransitionStyle::Push
        ) {
            tp.offset.x *= screen_rect.width();
            tp.prev_offset.x *= screen_rect.width();
        }

        let layout = self.compute_plane_layout(img_size, screen_rect);
        let rotation = layout.rotation_steps;
        let angle = layout.angle;
        let dest = layout.dest;

        let final_dest = Rect::from_center_size(dest.center() + tp.offset, dest.size() * tp.scale);

        // The painter transform handles visual rotation; draw un-rotated texture into un-rotated rect.
        let unrotated_final_dest =
            crate::app::rendering::geometry::unrotated_draw_rect_for_display(final_dest, rotation);
        let final_layout = PlaneLayout::from_dest(img_size, rotation, final_dest);

        if tp.is_animating
            && matches!(
                self.active_transition,
                TransitionStyle::PageFlip | TransitionStyle::Curtain
            )
        {
            if let (Some(hdr_image), Some(target_format)) =
                (hdr_image.clone(), self.hdr_target_format)
            {
                self.draw_rectangular_hdr_transition(
                    ui,
                    screen_rect,
                    hdr_image_plane_rect(&final_layout),
                    unrotated_final_dest,
                    rotation,
                    angle,
                    hdr_image,
                    self.hdr_renderer.tone_map,
                    target_format,
                    render_plan.output_mode,
                    tp.alpha,
                );
                ui.ctx().request_repaint();
                return;
            }
        }

        // Static HDR images draw through egui-wgpu so the float buffer reaches the shader.
        // Ripple remains on the SDR texture path because its circular mesh cannot be expressed
        // by the current rectangular HDR image-plane callback.
        if should_use_hdr_callback(self.active_transition, tp.is_animating) {
            if let (Some(hdr_image), Some(target_format)) = (hdr_image, render_plan.target_format) {
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

                // HDR images draw through egui-wgpu so the float buffer reaches the shader.
                // The SDR fallback texture stays cached for non-wgpu paths and transitions.
                self.draw_hdr_image_plane_clipped(
                    ui,
                    screen_rect,
                    hdr_image_plane_rect(&final_layout),
                    hdr_image,
                    self.hdr_renderer.tone_map,
                    target_format,
                    render_plan.output_mode,
                    rotation,
                    tp.alpha,
                );
                return;
            }
        }

        // --- Draw sequence ---
        if tp.is_animating
            && matches!(
                self.active_transition,
                TransitionStyle::PageFlip | TransitionStyle::Ripple | TransitionStyle::Curtain
            )
        {
            // Complex per-pixel transitions handled in transitions.rs
            self.draw_complex_transition(
                ui,
                screen_rect,
                &texture,
                final_dest,
                unrotated_final_dest,
                rotation,
                angle,
                tp.alpha,
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
            draw_sdr_texture_plane(
                ui,
                screen_rect,
                texture.id(),
                unrotated_final_dest,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE.linear_multiply(tp.alpha),
                &final_layout,
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_rectangular_hdr_transition(
        &self,
        ui: &mut egui::Ui,
        screen_rect: Rect,
        final_dest: Rect,
        unrotated_final_dest: Rect,
        rotation: i32,
        angle: f32,
        hdr_image: Arc<HdrImageBuffer>,
        tone_map: HdrToneMapSettings,
        target_format: wgpu::TextureFormat,
        hdr_output_mode: HdrRenderOutputMode,
        alpha: f32,
    ) {
        match self.active_transition {
            TransitionStyle::PageFlip => self.draw_page_flip_hdr_new_image(
                ui,
                screen_rect,
                final_dest,
                unrotated_final_dest,
                rotation,
                angle,
                hdr_image,
                tone_map,
                target_format,
                hdr_output_mode,
                alpha,
            ),
            TransitionStyle::Curtain => self.draw_curtain_hdr_new_image(
                ui,
                screen_rect,
                final_dest,
                hdr_image,
                tone_map,
                target_format,
                hdr_output_mode,
                alpha,
            ),
            _ => {}
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_hdr_image_plane_clipped(
        &self,
        ui: &mut egui::Ui,
        clip: Rect,
        rect: Rect,
        hdr_image: Arc<HdrImageBuffer>,
        tone_map: HdrToneMapSettings,
        target_format: wgpu::TextureFormat,
        hdr_output_mode: HdrRenderOutputMode,
        rotation: i32,
        alpha: f32,
    ) {
        let layout = PlaneLayout::from_dest(Vec2::new(rect.width(), rect.height()), rotation, rect);
        draw_plane(
            ui,
            clip,
            rect,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            &layout,
            PlaneDrawSource::HdrImage {
                image: hdr_image,
                tone_map,
                target_format,
                output_mode: hdr_output_mode,
                rotation_steps: rotation as u32,
                alpha,
            },
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_page_flip_hdr_new_image(
        &self,
        ui: &mut egui::Ui,
        screen_rect: Rect,
        final_dest: Rect,
        _unrotated_final_dest: Rect,
        rotation: i32,
        _angle: f32,
        hdr_image: Arc<HdrImageBuffer>,
        tone_map: HdrToneMapSettings,
        target_format: wgpu::TextureFormat,
        hdr_output_mode: HdrRenderOutputMode,
        alpha: f32,
    ) {
        if let Some(prev) = self.prev_texture.as_ref() {
            let p_size = prev.size_vec2();
            let p_dest = self.compute_display_rect(p_size, screen_rect);
            let union_rect = p_dest.union(final_dest);

            let elapsed = self
                .transition_start
                .map(|s| s.elapsed().as_secs_f32())
                .unwrap_or(0.0);
            let duration = self.settings.transition_ms as f32 / 1000.0;
            let t = (elapsed / duration).clamp(0.0, 1.0);
            let ease_in_out = 3.0 * t * t - 2.0 * t * t * t;

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
            self.draw_hdr_image_plane_clipped(
                ui,
                new_clip,
                final_dest,
                hdr_image,
                tone_map,
                target_format,
                hdr_output_mode,
                rotation,
                alpha,
            );

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

    #[allow(clippy::too_many_arguments)]
    fn draw_curtain_hdr_new_image(
        &self,
        ui: &mut egui::Ui,
        screen_rect: Rect,
        final_dest: Rect,
        hdr_image: Arc<HdrImageBuffer>,
        tone_map: HdrToneMapSettings,
        target_format: wgpu::TextureFormat,
        hdr_output_mode: HdrRenderOutputMode,
        alpha: f32,
    ) {
        if let Some(prev) = self.prev_texture.as_ref() {
            let p_size = prev.size_vec2();
            let p_dest = self.compute_display_rect(p_size, screen_rect);
            let union_rect = p_dest.union(final_dest);

            let elapsed = self
                .transition_start
                .map(|s| s.elapsed().as_secs_f32())
                .unwrap_or(0.0);
            let duration = self.settings.transition_ms as f32 / 1000.0;
            let t = (elapsed / duration).clamp(0.0, 1.0);
            let ease = 1.0 - (1.0 - t).powi(3);

            let center_x = union_rect.center().x;
            let half_w = union_rect.width() / 2.0;
            let shift = ease * half_w;

            let new_clip = Rect::from_min_max(
                Pos2::new(center_x - shift, union_rect.min.y),
                Pos2::new(center_x + shift, union_rect.max.y),
            );
            self.draw_hdr_image_plane_clipped(
                ui,
                new_clip,
                final_dest,
                hdr_image,
                tone_map,
                target_format,
                hdr_output_mode,
                0,
                alpha,
            );

            let left_clip = Rect::from_min_max(
                union_rect.left_top(),
                Pos2::new(center_x - shift, union_rect.max.y),
            );
            ui.painter().with_clip_rect(left_clip).image(
                prev.id(),
                p_dest.translate(Vec2::new(-shift, 0.0)),
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );

            let right_clip = Rect::from_min_max(
                Pos2::new(center_x + shift, union_rect.min.y),
                union_rect.right_bottom(),
            );
            ui.painter().with_clip_rect(right_clip).image(
                prev.id(),
                p_dest.translate(Vec2::new(shift, 0.0)),
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );

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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::rendering::plan::{RenderPlan, RenderShape};

    fn static_plan(
        has_hdr_plane: bool,
        target: Option<wgpu::TextureFormat>,
        output_mode: HdrRenderOutputMode,
    ) -> RenderPlan {
        RenderPlan::new(RenderShape::Static, has_hdr_plane, target, output_mode)
    }

    #[test]
    fn hdr_callback_is_disabled_during_complex_transitions() {
        for style in [TransitionStyle::Ripple] {
            assert!(!should_use_hdr_callback(style, true));
            assert!(should_use_hdr_callback(style, false));
        }
    }

    #[test]
    fn rectangular_complex_transitions_can_reveal_static_hdr_directly() {
        for style in [TransitionStyle::PageFlip, TransitionStyle::Curtain] {
            assert!(should_use_hdr_callback(style, true));
        }
    }

    #[test]
    fn hdr_callback_can_render_standard_transitions() {
        for style in [
            TransitionStyle::Fade,
            TransitionStyle::ZoomFade,
            TransitionStyle::Slide,
            TransitionStyle::Push,
            TransitionStyle::None,
        ] {
            assert!(should_use_hdr_callback(style, true));
        }
    }

    #[test]
    fn native_static_hdr_draws_immediately_without_sdr_transition_phase() {
        assert!(should_draw_static_hdr_immediately(
            &static_plan(
                true,
                Some(wgpu::TextureFormat::Rgba16Float),
                HdrRenderOutputMode::NativeHdr
            ),
            TransitionStyle::None,
            false
        ));
        assert!(!should_draw_static_hdr_immediately(
            &static_plan(
                true,
                Some(wgpu::TextureFormat::Rgba16Float),
                HdrRenderOutputMode::SdrToneMapped
            ),
            TransitionStyle::None,
            false
        ));
        assert!(!should_draw_static_hdr_immediately(
            &static_plan(
                false,
                Some(wgpu::TextureFormat::Rgba16Float),
                HdrRenderOutputMode::NativeHdr
            ),
            TransitionStyle::None,
            false
        ));
        assert!(!should_draw_static_hdr_immediately(
            &static_plan(
                true,
                Some(wgpu::TextureFormat::Rgba16Float),
                HdrRenderOutputMode::NativeHdr
            ),
            TransitionStyle::Curtain,
            true
        ));
        assert!(should_draw_static_hdr_immediately(
            &static_plan(
                true,
                Some(wgpu::TextureFormat::Rgba16Float),
                HdrRenderOutputMode::NativeHdr
            ),
            TransitionStyle::Ripple,
            true
        ));
    }
}
