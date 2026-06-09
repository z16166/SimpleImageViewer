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

use super::helpers::{
    pending_navigation_hold_params, should_clear_transition_state_after_static_hdr_draw,
};
use super::{should_draw_static_hdr_immediately, should_route_through_hdr_plane};
use crate::app::rendering::geometry::PlaneLayout;
use crate::app::rendering::plan::{RenderPlan, RenderShape};
use crate::app::rendering::plane::{PlaneBackendKind, draw_sdr_texture_plane, hdr_image_plane_rect};
use crate::app::{ImageViewerApp, TransitionStyle};
use crate::hdr::renderer::HdrRenderOutputMode;
use eframe::egui::{self, Color32, Pos2, Rect, Vec2};
use std::sync::Arc;
use std::time::Instant;

impl ImageViewerApp {
    /// Pin HDR GPU bindings while a transition is active so prev/current planes are not evicted
    /// mid-animation (re-upload + ISO compose would flash a full frame).
    pub(crate) fn hdr_plane_keep_resident(&self) -> bool {
        if self.transition_start.is_some() {
            return true;
        }
        // Keep prev/current HDR image-plane bindings pinned for one extra frame after the
        // transition is declared settled. 50ms covers a normal 60Hz/120Hz frame handoff without
        // keeping abandoned bindings resident long enough to fight the LRU cache.
        const POST_TRANSITION_BINDING_HOLD: std::time::Duration =
            std::time::Duration::from_millis(50);
        self.transition_settled_at
            .is_some_and(|t| t.elapsed() < POST_TRANSITION_BINDING_HOLD)
    }

    /// While the navigation target is not render-ready and transitions are disabled, keep drawing
    /// the outgoing image. HDR sources must use the float plane — drawing only the SDR fallback
    /// texture looks noticeably darker on HDR displays.
    pub(crate) fn draw_pending_navigation_hold_frame(&self, ui: &mut egui::Ui, screen_rect: Rect) {
        let tp = pending_navigation_hold_params();
        self.draw_prev_image_underneath(
            ui,
            screen_rect,
            &tp,
            self.current_rotation,
            None,
            None,
            None,
        );
    }

    /// HDR float-plane draw params for the outgoing frame during cross-format transitions.
    pub(crate) fn effective_hdr_display_output(
        &self,
    ) -> Option<(wgpu::TextureFormat, HdrRenderOutputMode)> {
        let format = self.hdr_target_format?;
        let mode = crate::hdr::monitor::effective_render_output_mode(
            Some(format),
            self.effective_hdr_monitor_selection().as_ref(),
        );
        Some((format, mode))
    }

    /// Draw the standard (non-tiled) image rendering path, including transition animations.
    ///
    /// Called from `draw_image_canvas_ui` when there is an active texture in `texture_cache`.
    pub(crate) fn draw_standard_image(
        &mut self,
        ui: &mut egui::Ui,
        screen_rect: Rect,
        canvas_resp: &egui::Response,
        texture: Option<egui::TextureHandle>,
    ) {
        // --- Animated image frame advancement ---
        let texture = if let Some(ref mut anim) = self.animation {
            if anim.image_index == self.current_index && !anim.textures.is_empty() {
                let elapsed = anim.frame_start.elapsed();
                if elapsed >= anim.delays[anim.current_frame] {
                    // Infinite loop for all animated formats here (GIF/WebP/APNG/AVIF sequence, etc.);
                    // container metadata such as AVIF `repetitionCount` is intentionally ignored.
                    anim.current_frame = (anim.current_frame + 1) % anim.textures.len();
                    anim.frame_start = Instant::now();
                }
                if let Some(hdr_frames) = &anim.hdr_frames {
                    if let Some(hdr) = hdr_frames.get(anim.current_frame) {
                        self.current_hdr_image = Some(crate::app::CurrentHdrImage::new(
                            anim.image_index,
                            Arc::clone(hdr),
                        ));
                    }
                }
                let remaining =
                    anim.delays[anim.current_frame].saturating_sub(anim.frame_start.elapsed());
                ui.ctx().request_repaint_after(remaining);
                Some(anim.textures[anim.current_frame].clone())
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
        } else if let Some((w, h)) = self.current_image_res {
            Vec2::new(w as f32, h as f32)
        } else if let Some(texture) = texture.as_ref() {
            texture.size_vec2()
        } else {
            Vec2::splat(1.0)
        };

        if canvas_resp.dragged() {
            self.pan_offset += canvas_resp.drag_delta();
            self.invalidate_tile_requests_for_view_change();
        }

        // --- Transition parameter computation ---
        // Slide and Push use normalised offsets; multiply by screen width here.
        let mut tp = self.compute_transition_params();
        let hdr_image = self
            .current_hdr_image
            .as_ref()
            .and_then(|current| current.image_for_index(self.current_index))
            .cloned();
        let hdr_image =
            hdr_image.or_else(|| self.hdr_image_cache.get(&self.current_index).cloned());
        // `draw_standard_image` is only reached when `texture_cache.get(current_index)` returned
        // `Some`. Static HDR installs always register [`ImageViewerApp::hdr_sdr_fallback_indices`];
        // keep [`build_render_plan`]/[`crate::hdr::monitor::effective_render_output_mode`] inputs
        // aligned with OSD (`hdr_status`) so FloatImagePlane is not advertised while drawing the
        // SDR cache path due to bookkeeping drift (`install_static_hdr_image` missed the set).
        let has_sdr_fallback = self.hdr_sdr_fallback_indices.contains(&self.current_index);
        let render_plan =
            self.build_render_plan(RenderShape::Static, hdr_image.is_some(), has_sdr_fallback);
        let static_hdr_draw = should_draw_static_hdr_immediately(
            &render_plan,
            self.active_transition,
            tp.is_animating,
        );
        if static_hdr_draw {
            if should_clear_transition_state_after_static_hdr_draw(
                static_hdr_draw,
                self.pending_transition_target,
                self.current_index,
            ) {
                self.transition_start = None;
                self.prev_texture = None;
                self.prev_hdr_image = None;
                self.prev_transition_rect = None;
            }
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
                TransitionStyle::PageFlip | TransitionStyle::Curtain | TransitionStyle::Ripple
            )
            && render_plan.backend == PlaneBackendKind::Hdr
        {
            if let (Some(hdr_image), Some(target_format)) =
                (hdr_image.clone(), render_plan.target_format)
            {
                if self.active_transition == TransitionStyle::Ripple {
                    // 1. Compute ripple state
                    let elapsed = self
                        .transition_start
                        .map(|s| s.elapsed().as_secs_f32())
                        .unwrap_or(0.0);
                    let duration = self.settings.transition_ms as f32 / 1000.0;
                    let t = (elapsed / duration).clamp(0.0, 1.0);
                    let ease = 3.0 * t * t - 2.0 * t * t * t; // smoothstep

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

                    // 2. Draw OLD image (clipped with a circular hole)
                    let (p_dest, _, has_prev) =
                        self.transition_prev_layout(screen_rect, final_dest);
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

                    // 3. Draw NEW image in HDR with circular clip in shader
                    let ppp = ui.ctx().pixels_per_point();
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
                        Some((
                            center,
                            current_radius,
                            ppp,
                            crate::hdr::renderer::RIPPLE_CLIP_INSIDE,
                        )),
                    );

                    // 4. Water ripple rings at the expanding edge
                    crate::app::rendering::transitions::draw_ripple_rings(
                        ui,
                        center,
                        current_radius,
                    );
                    ui.ctx().request_repaint();
                    return;
                } else {
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
            if tp.is_animating {
                // HDR plane expected but not drawable this frame — hold the outgoing frame instead
                // of falling through to the SDR page-flip path (full-frame brightness flash).
                self.draw_pending_navigation_hold_frame(ui, screen_rect);
                ui.ctx().request_repaint();
                return;
            }
        }

        // Static HDR images draw through egui-wgpu so the float buffer reaches the shader.
        // All HDR transitions (including Curtain, PageFlip, and Ripple) draw both the new image
        // and the previous background image through the HDR plane path if `prev_hdr_image` is available.
        // The plan's backend must be `Hdr`; otherwise (e.g. monitor probed as SDR-only, or
        // probe failed and the conservative gate kicked in) the cached SDR fallback texture
        // is the correct visual source — see `should_route_through_hdr_plane`.
        if should_route_through_hdr_plane(&render_plan) {
            if let (Some(hdr_image), Some(target_format)) = (hdr_image, render_plan.target_format) {
                let geometric_transition = matches!(
                    self.active_transition,
                    TransitionStyle::PageFlip | TransitionStyle::Curtain | TransitionStyle::Ripple
                );
                if tp.is_animating && geometric_transition {
                    // Dedicated clipped HDR paths above handle these styles.
                } else if tp.is_animating {
                    self.draw_prev_image_underneath(
                        ui,
                        screen_rect,
                        &tp,
                        rotation,
                        Some(target_format),
                        Some(render_plan.output_mode),
                        None,
                    );
                    ui.ctx().request_repaint();
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
                        None,
                    );
                } else {
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
                        None,
                    );
                }
                return;
            }
        }

        // --- Draw sequence ---
        if tp.is_animating
            && matches!(
                self.active_transition,
                TransitionStyle::PageFlip | TransitionStyle::Ripple | TransitionStyle::Curtain
            )
            && render_plan.backend == PlaneBackendKind::Sdr
            && texture.is_some()
        {
            let texture = texture.as_ref().expect("checked above");
            match self.active_transition {
                TransitionStyle::PageFlip => self.draw_page_flip_transition(
                    ui,
                    screen_rect,
                    texture,
                    final_dest,
                    unrotated_final_dest,
                    rotation,
                    angle,
                    tp.alpha,
                ),
                TransitionStyle::Curtain => {
                    self.draw_curtain_transition(ui, screen_rect, texture, final_dest, tp.alpha)
                }
                TransitionStyle::Ripple => self.draw_ripple_transition(
                    ui,
                    screen_rect,
                    texture,
                    final_dest,
                    rotation,
                    angle,
                ),
                _ => unreachable!(),
            }
            ui.ctx().request_repaint();
            return;
        } else {
            // Standard Fade / ZoomFade / Slide / Push (and no-transition static draw):

            // 1. Draw OLD image (underneath or fading out)
            if tp.is_animating {
                self.draw_prev_image_underneath(ui, screen_rect, &tp, rotation, None, None, None);
                ui.ctx().request_repaint();
            }

            // 2. Draw NEW image (on top, with alpha/motion)
            if let Some(texture) = texture.as_ref() {
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
    }
}
