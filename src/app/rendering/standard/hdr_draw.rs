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

use super::helpers::curtain_hdr_transition_rotation;
use crate::app::rendering::geometry::PlaneLayout;
use crate::app::rendering::plan::RenderPlan;
use crate::app::rendering::plane::{
    PlaneBackendKind, PlaneDrawSource, draw_plane, hdr_image_plane_rect,
};
use crate::app::{ImageViewerApp, TransitionStyle};
use crate::hdr::renderer::HdrRenderOutputMode;
use crate::hdr::types::{HdrImageBuffer, HdrToneMapSettings};
use eframe::egui::{self, Color32, Pos2, Rect, Vec2};
use std::sync::Arc;

pub(crate) struct PrevImageUnderneathParams<'a> {
    pub(crate) screen_rect: Rect,
    pub(crate) transition: &'a crate::app::rendering::transitions::TransitionParams,
    pub(crate) rotation: i32,
    pub(crate) target_format: Option<wgpu::TextureFormat>,
    pub(crate) hdr_output_mode: Option<HdrRenderOutputMode>,
    pub(crate) override_dest: Option<Rect>,
}

pub(crate) struct HdrImagePlaneClippedDraw {
    pub(crate) clip: Rect,
    pub(crate) rect: Rect,
    pub(crate) hdr_image: Arc<HdrImageBuffer>,
    pub(crate) tone_map: HdrToneMapSettings,
    pub(crate) target_format: wgpu::TextureFormat,
    pub(crate) hdr_output_mode: HdrRenderOutputMode,
    pub(crate) rotation: i32,
    pub(crate) alpha: f32,
    pub(crate) ripple: Option<(egui::Pos2, f32, f32, u32)>,
}

pub(crate) struct HdrRectangularTransitionDraw {
    pub(crate) screen_rect: Rect,
    pub(crate) final_dest: Rect,
    pub(crate) unrotated_final_dest: Rect,
    pub(crate) rotation: i32,
    pub(crate) angle: f32,
    pub(crate) hdr_image: Arc<HdrImageBuffer>,
    pub(crate) tone_map: HdrToneMapSettings,
    pub(crate) target_format: wgpu::TextureFormat,
    pub(crate) hdr_output_mode: HdrRenderOutputMode,
    pub(crate) alpha: f32,
}

pub(crate) struct PageFlipHdrTransitionDraw {
    pub(crate) screen_rect: Rect,
    pub(crate) final_dest: Rect,
    pub(crate) unrotated_final_dest: Rect,
    pub(crate) rotation: i32,
    pub(crate) angle: f32,
    pub(crate) hdr_image: Arc<HdrImageBuffer>,
    pub(crate) tone_map: HdrToneMapSettings,
    pub(crate) target_format: wgpu::TextureFormat,
    pub(crate) hdr_output_mode: HdrRenderOutputMode,
    pub(crate) alpha: f32,
}

pub(crate) struct CurtainHdrTransitionDraw {
    pub(crate) screen_rect: Rect,
    pub(crate) final_dest: Rect,
    pub(crate) rotation: i32,
    pub(crate) hdr_image: Arc<HdrImageBuffer>,
    pub(crate) tone_map: HdrToneMapSettings,
    pub(crate) target_format: wgpu::TextureFormat,
    pub(crate) hdr_output_mode: HdrRenderOutputMode,
    pub(crate) alpha: f32,
}

impl ImageViewerApp {
    /// GPU RAW demosaic runs inside [`HdrImagePlaneCallback::prepare`]. While bootstrap
    /// preview is shown the render plan keeps the visible backend on SDR, which would
    /// otherwise never invoke that callback. Schedule an alpha-zero HDR plane so the bake
    /// completes and pending can clear without flashing the float plane over the preview.
    pub(crate) fn ensure_gpu_raw_demosaic_bake_callback(
        &self,
        ui: &mut egui::Ui,
        clip: Rect,
        hdr_image: &Arc<HdrImageBuffer>,
        render_plan: &RenderPlan,
        layout: &PlaneLayout,
        rotation: i32,
    ) {
        if !self
            .hdr_raw_gpu_demosaic_pending_indices
            .contains(&self.current_index)
        {
            return;
        }
        if hdr_image.metadata.raw_gpu_source.is_none() {
            return;
        }
        // Visible draw uses the SDR bootstrap preview while pending; schedule an alpha-zero
        // HDR plane so prepare() runs demosaic. When the plan already routes through Hdr,
        // the main plane draw below triggers the same callback -- avoid a duplicate submit.
        if render_plan.backend != PlaneBackendKind::Sdr {
            return;
        }
        let Some(target_format) = render_plan.target_format else {
            return;
        };
        self.draw_hdr_image_plane_clipped(
            ui,
            HdrImagePlaneClippedDraw {
                clip,
                rect: hdr_image_plane_rect(layout),
                hdr_image: Arc::clone(hdr_image),
                tone_map: self.hdr_renderer.tone_map,
                target_format,
                hdr_output_mode: render_plan.output_mode,
                rotation,
                alpha: 0.0,
                ripple: None,
            },
        );
        ui.ctx().request_repaint();
    }

    pub(crate) fn draw_prev_image_underneath(
        &self,
        ui: &mut egui::Ui,
        params: PrevImageUnderneathParams<'_>,
    ) {
        let PrevImageUnderneathParams {
            screen_rect,
            transition: tp,
            rotation,
            target_format,
            hdr_output_mode,
            override_dest,
        } = params;
        let hdr_draw = match (target_format, hdr_output_mode) {
            (Some(format), Some(mode)) => Some((format, mode)),
            (None, None) => self.effective_hdr_display_output(),
            _ => None,
        };
        if let Some(prev_hdr) = self.prev_hdr_image.as_ref()
            && let Some((target_format, hdr_output_mode)) = hdr_draw
        {
            let p_dest = override_dest
                .or(self.prev_transition_rect)
                .unwrap_or_else(|| {
                    let p_size = Vec2::new(prev_hdr.width as f32, prev_hdr.height as f32);
                    self.compute_display_rect(p_size, screen_rect)
                });
            let p_final_dest = Rect::from_center_size(
                p_dest.center() + tp.prev_offset,
                p_dest.size() * tp.prev_scale,
            );
            self.draw_hdr_image_plane_clipped(
                ui,
                HdrImagePlaneClippedDraw {
                    clip: screen_rect,
                    rect: p_final_dest,
                    hdr_image: Arc::clone(prev_hdr),
                    tone_map: self.hdr_renderer.tone_map,
                    target_format,
                    hdr_output_mode,
                    rotation,
                    alpha: tp.prev_alpha,
                    ripple: None,
                },
            );
            return;
        }

        if let Some(ref prev) = self.prev_texture {
            let p_dest = override_dest
                .or(self.prev_transition_rect)
                .unwrap_or_else(|| {
                    let p_size = prev.size_vec2();
                    self.compute_display_rect(p_size, screen_rect)
                });
            let p_final_dest = Rect::from_center_size(
                p_dest.center() + tp.prev_offset,
                p_dest.size() * tp.prev_scale,
            );
            ui.painter().with_clip_rect(screen_rect).image(
                prev.id(),
                p_final_dest,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE.linear_multiply(tp.prev_alpha),
            );
        }
    }

    pub(crate) fn draw_rectangular_hdr_transition(
        &self,
        ui: &mut egui::Ui,
        draw: HdrRectangularTransitionDraw,
    ) {
        let HdrRectangularTransitionDraw {
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
        } = draw;
        match self.active_transition {
            TransitionStyle::PageFlip => self.draw_page_flip_hdr_new_image(
                ui,
                PageFlipHdrTransitionDraw {
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
                },
            ),
            TransitionStyle::Curtain => self.draw_curtain_hdr_new_image(
                ui,
                CurtainHdrTransitionDraw {
                    screen_rect,
                    final_dest,
                    rotation,
                    hdr_image,
                    tone_map,
                    target_format,
                    hdr_output_mode,
                    alpha,
                },
            ),
            _ => {}
        }
    }

    pub(crate) fn draw_hdr_image_plane_clipped(
        &self,
        ui: &mut egui::Ui,
        draw: HdrImagePlaneClippedDraw,
    ) {
        let HdrImagePlaneClippedDraw {
            clip,
            rect,
            hdr_image,
            tone_map,
            target_format,
            hdr_output_mode,
            rotation,
            alpha,
            ripple,
        } = draw;
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
                ripple,
                keep_resident: self.hdr_plane_keep_resident(),
                raw_demosaic_baked_notify: Some(Arc::clone(&self.raw_demosaic_baked_notify)),
                hdr_pending_work: Some(Arc::clone(&self.hdr_pending_work)),
            },
        );
    }

    pub(crate) fn draw_page_flip_hdr_new_image(
        &self,
        ui: &mut egui::Ui,
        draw: PageFlipHdrTransitionDraw,
    ) {
        let PageFlipHdrTransitionDraw {
            screen_rect,
            final_dest,
            unrotated_final_dest: _unrotated_final_dest,
            rotation,
            angle: _angle,
            hdr_image,
            tone_map,
            target_format,
            hdr_output_mode,
            alpha,
        } = draw;
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

        // Draw outgoing first so its HDR GPU binding is prepared (and LRU-protected) before
        // the incoming image upload can evict it from the small plane cache.
        if has_prev {
            let mut old_clip = union_rect;
            if self.is_next {
                old_clip.max.x = clip_x;
            } else {
                old_clip.min.x = clip_x;
            }
            self.draw_outgoing_transition_frame_clipped(
                ui,
                crate::app::rendering::standard::OutgoingFrameClippedParams {
                    clip: old_clip,
                    dest: p_dest,
                    rotation,
                    alpha: 1.0,
                    hdr_output: Some((target_format, hdr_output_mode)),
                },
            );
        }

        self.draw_hdr_image_plane_clipped(
            ui,
            HdrImagePlaneClippedDraw {
                clip: new_clip,
                rect: final_dest,
                hdr_image,
                tone_map,
                target_format,
                hdr_output_mode,
                rotation,
                alpha,
                ripple: None,
            },
        );

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

    pub(crate) fn draw_curtain_hdr_new_image(
        &self,
        ui: &mut egui::Ui,
        draw: CurtainHdrTransitionDraw,
    ) {
        let CurtainHdrTransitionDraw {
            screen_rect,
            final_dest,
            rotation,
            hdr_image,
            tone_map,
            target_format,
            hdr_output_mode,
            alpha,
        } = draw;
        if self.prev_texture.is_some() || self.prev_hdr_image.is_some() {
            let p_size = self
                .prev_hdr_image
                .as_ref()
                .map(|h| Vec2::new(h.width as f32, h.height as f32))
                .or_else(|| self.prev_texture.as_ref().map(|t| t.size_vec2()))
                .expect("either prev_hdr_image or prev_texture must be Some");
            let p_dest = self
                .prev_transition_rect
                .unwrap_or_else(|| self.compute_display_rect(p_size, screen_rect));
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
                HdrImagePlaneClippedDraw {
                    clip: new_clip,
                    rect: final_dest,
                    hdr_image,
                    tone_map,
                    target_format,
                    hdr_output_mode,
                    rotation: curtain_hdr_transition_rotation(rotation),
                    alpha,
                    ripple: None,
                },
            );

            let left_clip = Rect::from_min_max(
                union_rect.left_top(),
                Pos2::new(center_x - shift, union_rect.max.y),
            );
            let right_clip = Rect::from_min_max(
                Pos2::new(center_x + shift, union_rect.min.y),
                union_rect.right_bottom(),
            );

            if let Some(prev_hdr) = self.prev_hdr_image.as_ref() {
                self.draw_hdr_image_plane_clipped(
                    ui,
                    HdrImagePlaneClippedDraw {
                        clip: left_clip,
                        rect: p_dest.translate(Vec2::new(-shift, 0.0)),
                        hdr_image: Arc::clone(prev_hdr),
                        tone_map: self.hdr_renderer.tone_map,
                        target_format,
                        hdr_output_mode,
                        rotation: curtain_hdr_transition_rotation(rotation),
                        alpha,
                        ripple: None,
                    },
                );
                self.draw_hdr_image_plane_clipped(
                    ui,
                    HdrImagePlaneClippedDraw {
                        clip: right_clip,
                        rect: p_dest.translate(Vec2::new(shift, 0.0)),
                        hdr_image: Arc::clone(prev_hdr),
                        tone_map: self.hdr_renderer.tone_map,
                        target_format,
                        hdr_output_mode,
                        rotation: curtain_hdr_transition_rotation(rotation),
                        alpha,
                        ripple: None,
                    },
                );
            } else if let Some(prev) = self.prev_texture.as_ref() {
                ui.painter().with_clip_rect(left_clip).image(
                    prev.id(),
                    p_dest.translate(Vec2::new(-shift, 0.0)),
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
                ui.painter().with_clip_rect(right_clip).image(
                    prev.id(),
                    p_dest.translate(Vec2::new(shift, 0.0)),
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
            }

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
