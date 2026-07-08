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

#[cfg(feature = "tile-debug")]
use super::helpers::draw_tile_debug_border;
use super::helpers::{
    HdrPlaneTileVisit, TileRequestBudget, TiledPlaneKind, draw_hdr_plane_tile_visit,
    effective_hdr_tiled_alphas, has_pending_visible_tiles_for_backend,
    hdr_tile_cache_key_for_coord, is_tiled_plane_active, prev_transition_params_for_tiled_draw,
    should_draw_tiled_preview_for_backend, should_draw_tiled_preview_transition_for_backend,
    should_invalidate_tile_requests_on_pan_drag, should_repaint_for_ready_tiles_for_backend,
    tile_decode_source_for_backend, tile_pending_key_for_backend, tile_plane_kind_for_backend,
    tile_request_priority, tile_visits_for_backend_into, tiled_lookahead_padding,
    tiled_plane_threshold_for_backend,
};
use super::{BURST_UPLOAD_MAX_512, BURST_UPLOAD_MULT, FALLBACK_PREVIEW_SCALE};
use crate::app::ImageViewerApp;
use crate::app::rendering::plan::RenderShape;
use crate::app::rendering::plane::{
    PlaneBackendKind, PlaneDrawSource, draw_plane, draw_sdr_texture_plane, hdr_image_plane_rect,
};
use crate::tile_cache::{TileCoord, TileStatus};
use eframe::egui::{self, Color32, Pos2, Rect, Vec2};
use std::sync::Arc;

impl ImageViewerApp {
    /// Draw the tiled (large-image) rendering path.
    ///
    /// Called from `draw_image_canvas_ui` when `self.tile_manager.is_some()`.
    pub(crate) fn draw_tiled_image(
        &mut self,
        ui: &mut egui::Ui,
        screen_rect: Rect,
        canvas_resp: &egui::Response,
    ) {
        if canvas_resp.dragged() && !self.is_pixel_region_selection_active(ui.ctx()) {
            self.pan_offset += canvas_resp.drag_delta();
            if should_invalidate_tile_requests_on_pan_drag() {
                self.invalidate_tile_requests_for_view_change();
            }
        }

        // Rotation logic
        let rotation = self.current_rotation;
        let angle = rotation as f32 * (std::f32::consts::PI / 2.0);

        // Extract dimensions first; transition handling below needs mutable access to self.
        let (full_width, full_height) = {
            let tm = self.tile_manager();
            (tm.full_width, tm.full_height)
        };
        let img_size = Vec2::new(full_width as f32, full_height as f32);
        let layout = self.compute_plane_layout(img_size, screen_rect);
        let rotated_img_size = layout.rotated_image_size;
        let dest = layout.dest;

        // The painter transform will handle the actual rotation.
        // We need to draw the UNROTATED image into a rect that, when rotated, matches 'dest'.
        let unrotated_dest = layout.unrotated_dest;
        let hdr_source_for_frame = self
            .current_hdr_tiled_image
            .as_ref()
            .and_then(|current| current.source_for_index(self.current_index))
            .cloned();
        // `has_sdr_fallback` tracks whether the tile manager already carries an SDR preview
        // texture that the `PlaneBackendKind::Sdr` fast path can blit. When absent — e.g. an
        // HDR-only tiled source (subsampled / luminance-chroma EXR such as `Flowers.exr`) on
        // an SDR panel — `select_render_backend` upgrades the plan to `Hdr` so the HDR
        // image-plane shader tone-maps the frame through `SdrToneMapped` instead of leaving
        // the canvas blank. We deliberately do **not** pre-synthesize an SDR preview in the
        // loader for HDR content: that memory is pure waste on systems (or usage modes) that
        // never reach the SDR pipeline.
        let has_sdr_fallback = self
            .tile_manager
            .as_ref()
            .is_some_and(|tm| tm.preview_texture.is_some());
        let render_plan = self.build_render_plan(
            RenderShape::Tiled,
            hdr_source_for_frame.is_some(),
            has_sdr_fallback,
        );
        // Align supplemental HDR OSD with [`ImageViewerApp::compute_hdr_render_path`]:
        // ordinary SDR tiled images also carry a preview texture, but that is not HDR content.
        let has_hdr_content = hdr_source_for_frame.is_some()
            || self
                .hdr_tiled_source_cache
                .contains_key(&self.current_index)
            || self.hdr_image_cache.contains_key(&self.current_index)
            || self.hdr_sdr_fallback_indices.contains(&self.current_index);
        self.record_frame_render_plan(render_plan, RenderShape::Tiled, false, has_hdr_content);
        let plane_backend = render_plan.backend;

        let tp = self.compute_transition_params();
        let preview_for_transition = self
            .tile_manager
            .as_ref()
            .and_then(|tm| tm.preview_texture.clone());
        if should_draw_tiled_preview_transition_for_backend(
            plane_backend,
            self.active_transition,
            tp.is_animating,
            preview_for_transition.is_some(),
        ) && let Some(preview) = preview_for_transition
        {
            self.draw_complex_transition(
                ui,
                crate::app::rendering::transitions::ComplexTransitionDraw {
                    screen_rect,
                    texture: &preview,
                    final_dest: dest,
                    unrotated_final_dest: unrotated_dest,
                    rotation,
                    angle,
                    alpha: tp.alpha,
                },
            );
            return;
        }

        let (tile_alpha, prev_alpha_eff) = effective_hdr_tiled_alphas(&tp, self.active_transition);

        if tp.is_animating {
            crate::app::rendering::transitions::request_navigation_transition_repaint(
                ui.ctx(),
                true,
            );
        }

        // Draw the previous image underneath for crossfade effect if we are animating
        // and have a valid previous alpha. When the outgoing frame is HDR but the new tiled
        // target is SDR-only, `draw_prev_image_underneath` resolves HDR plane params itself.
        if tp.is_animating
            && prev_alpha_eff > 0.0
            && (self.prev_hdr_image.is_some() || self.prev_texture.is_some())
        {
            let prev_tp = prev_transition_params_for_tiled_draw(tp, prev_alpha_eff);
            let (target_format, hdr_output_mode) = if plane_backend == PlaneBackendKind::Hdr {
                (render_plan.target_format, Some(render_plan.output_mode))
            } else {
                (None, None)
            };
            self.draw_prev_image_underneath(
                ui,
                crate::app::rendering::standard::PrevImageUnderneathParams {
                    screen_rect,
                    transition: &prev_tp,
                    rotation,
                    target_format,
                    hdr_output_mode,
                    override_dest: Some(hdr_image_plane_rect(&layout)),
                },
            );
        }

        // Render high-res tiles.
        // We use a dynamic threshold: Never trigger tiling in "Fit to Window" mode (regardless of image size).
        // For giant images, we also only trigger tiling when the effective scale exceeds
        // the preview scale, ensuring we don't thrash VRAM for no visual gain.
        let fit_scale = (screen_rect.width() / rotated_img_size.x)
            .min(screen_rect.height() / rotated_img_size.y)
            .min(1.0);

        // preview_scale: ratio of preview texture resolution to the ORIGINAL image resolution.
        // This tells us at what display scale the preview's native pixels would be 1:1.
        // Above this scale, tiles provide higher quality than the preview.
        let preview_scale = if let Some(ref p) = self.tile_manager().preview_texture {
            p.size()[0] as f32 / rotated_img_size.x.max(1.0)
        } else {
            FALLBACK_PREVIEW_SCALE // Fallback
        };

        let threshold = tiled_plane_threshold_for_backend(
            plane_backend,
            preview_scale,
            fit_scale,
            crate::tile_cache::get_tile_size(),
        );

        let effective_scale = dest.width() / rotated_img_size.x;

        let mut hdr_preview_drawn = false;
        if should_draw_tiled_preview_for_backend(plane_backend, TiledPlaneKind::Hdr)
            && let Some(hdr_preview) = self
                .current_hdr_tiled_preview
                .as_ref()
                .and_then(|current| current.image_for_index(self.current_index))
        {
            draw_plane(
                ui,
                screen_rect,
                hdr_image_plane_rect(&layout),
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                &layout,
                PlaneDrawSource::HdrImage {
                    image: Arc::clone(hdr_preview),
                    tone_map: self.hdr_renderer.tone_map,
                    target_format: render_plan
                        .target_format
                        .unwrap_or(wgpu::TextureFormat::Bgra8Unorm),
                    output_mode: render_plan.output_mode,
                    rotation_steps: rotation as u32,
                    alpha: tile_alpha,
                    ripple: None,
                    keep_resident: self.hdr_plane_keep_resident(),
                    raw_demosaic_baked_notify: None,
                    hdr_pending_work: Some(Arc::clone(&self.hdr_pending_work)),
                    sync_plane_upload_on_cache_miss: self.hdr_plane_sync_upload_on_cache_miss(),
                },
            );
            hdr_preview_drawn = true;
        }

        // Draw the preview that matches the active tiled plane backend.
        // Fallback to SDR preview texture if the HDR preview is not yet ready.
        if (should_draw_tiled_preview_for_backend(plane_backend, TiledPlaneKind::Sdr)
            || (plane_backend == PlaneBackendKind::Hdr && !hdr_preview_drawn))
            && let Some(ref preview) = self.tile_manager().preview_texture
        {
            let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
            draw_plane(
                ui,
                screen_rect,
                unrotated_dest,
                uv,
                &layout,
                PlaneDrawSource::SdrTexture {
                    texture_id: preview.id(),
                    color: Color32::WHITE,
                },
            );
        }

        // Log threshold diagnostics once per image load
        #[cfg(feature = "tile-debug")]
        {
            use std::sync::atomic::{AtomicU64, Ordering};
            static LAST_LOGGED_SCALE: AtomicU64 = AtomicU64::new(0);
            let scale_bits = (effective_scale * 1000.0) as u64;
            let prev = LAST_LOGGED_SCALE.load(Ordering::Relaxed);
            if scale_bits != prev {
                LAST_LOGGED_SCALE.store(scale_bits, Ordering::Relaxed);
                if effective_scale >= threshold * 0.9 && effective_scale <= threshold * 1.1 {
                    let fname = self.image_files[self.current_index]
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("?");
                    log::info!(
                        "[Tiling] [{}] preview_scale={:.4}, fit_scale={:.4}, threshold={:.4}, effective={:.4}, img_w={}, tiled={}",
                        fname,
                        preview_scale,
                        fit_scale,
                        threshold,
                        effective_scale,
                        rotated_img_size.x as u32,
                        effective_scale >= threshold
                    );
                }
            }
        }

        if is_tiled_plane_active(effective_scale, threshold) {
            // Compute visible tiles using the UNROTATED destination rect.
            // When rotation is active, we must inverse-rotate the screen clip
            // region into unrotated coordinate space. Otherwise, for extremely
            // tall/narrow images rotated 90°/270°, the unrotated rect is narrow
            // and its intersection with screen_rect only covers the center tiles.
            let padding = tiled_lookahead_padding(
                self.hardware_tier.look_ahead_padding(),
                crate::tile_cache::get_tile_size(),
            );
            let tile_clip = if rotation != 0 {
                let inv_rot = egui::emath::Rot2::from_angle(-angle);
                let pivot = layout.pivot;
                let corners = [
                    screen_rect.left_top(),
                    screen_rect.right_top(),
                    screen_rect.right_bottom(),
                    screen_rect.left_bottom(),
                ]
                .map(|p| pivot + inv_rot * (p - pivot));
                // Compute the axis-aligned bounding box of the rotated corners
                let min_x = corners.iter().map(|p| p.x).fold(f32::INFINITY, f32::min);
                let max_x = corners
                    .iter()
                    .map(|p| p.x)
                    .fold(f32::NEG_INFINITY, f32::max);
                let min_y = corners.iter().map(|p| p.y).fold(f32::INFINITY, f32::min);
                let max_y = corners
                    .iter()
                    .map(|p| p.y)
                    .fold(f32::NEG_INFINITY, f32::max);
                Rect::from_min_max(Pos2::new(min_x, min_y), Pos2::new(max_x, max_y))
            } else {
                screen_rect
            };
            if let Some(tm) = self.tile_manager.as_ref() {
                tm.visible_tiles_into(
                    unrotated_dest,
                    tile_clip,
                    padding,
                    &mut self.tiled_visible_tiles_scratch,
                    Some(&mut self.tiled_primary_visible_tiles_scratch),
                );
            }
            tile_visits_for_backend_into(
                plane_backend,
                &self.tiled_primary_visible_tiles_scratch,
                &self.tiled_visible_tiles_scratch,
                &mut self.tiled_tile_visits_scratch,
            );
            let tile_visits = &self.tiled_tile_visits_scratch;
            self.tiled_primary_visible_scratch.clear();
            self.tiled_primary_visible_scratch.extend(
                self.tiled_primary_visible_tiles_scratch
                    .iter()
                    .map(|(coord, _, _)| *coord),
            );
            self.tiled_visible_coords_scratch.clear();
            self.tiled_visible_coords_scratch
                .extend(self.tiled_visible_tiles_scratch.iter().map(|(c, _, _)| *c));
            let primary_visible_coords = &self.tiled_primary_visible_scratch;
            let visible_coords = &self.tiled_visible_coords_scratch;
            if let Some(hdr_source) = hdr_source_for_frame.as_ref() {
                self.tiled_protected_keys_scratch.clear();
                self.tiled_protected_keys_scratch.extend(
                    self.tiled_primary_visible_tiles_scratch
                        .iter()
                        .map(|(coord, _, _)| {
                            hdr_tile_cache_key_for_coord(hdr_source.as_ref(), *coord)
                        }),
                );
                hdr_source.protect_cached_tiles(&self.tiled_protected_keys_scratch);
            }
            if let Some(tm) = &mut self.tile_manager {
                tm.retain_pending_tiles(visible_coords);
            }

            // ANTI-THRASHING: We no longer truncate 'visible' here.
            // Eviction logic is now handled in get_or_create_tile to prevent circular holes.
            // visible.truncate(self.hardware_tier.gpu_cache_tiles());

            // Upload and draw tiles (mutable borrow, scoped)
            let ctx_ref = ui.ctx().clone();

            // BURST POLICY:
            // If we are NOT dragging and NOT scrolling (stable view), boost upload quota
            // to fill the screen quickly. Otherwise, keep it low to maintain 60FPS.
            //
            // VRAM safety: burst_upload_max keeps per-frame GPU upload bounded in BYTES,
            // not just tile count. tile_size_scale = tile_px / 512.
            //   512px tile  = 512×512×4 =  1 MB → burst_upload_max = 16/1 = 16 tiles = 16 MB/frame
            //   1024px tile = 1024×1024×4 = 4 MB → burst_upload_max = 16/2 =  8 tiles = 32 MB/frame
            //
            // 32 MB/frame at 60 FPS ≈ 2 GB/s, well within PCIe 4.0 x16 bandwidth (32 GB/s).
            // This prevents Windows TDR (GPU timeout reset → black screen) on any hardware.
            let tile_size_scale = (crate::tile_cache::get_tile_size() / 512) as usize;
            let burst_upload_max = (BURST_UPLOAD_MAX_512 / tile_size_scale).max(1);
            let is_interacting = canvas_resp.dragged() || self.last_mouse_wheel_nav.abs() > 0.01;
            let tile_upload_quota = if !is_interacting {
                (self.tile_upload_quota * BURST_UPLOAD_MULT).min(burst_upload_max)
            // Burst mode
            } else {
                self.tile_upload_quota.min(burst_upload_max) // Stable mode also capped
            };

            let mut newly_uploaded = 0;
            let mut uploaded_coords_scratch: Vec<TileCoord> = Vec::new();
            let mut tile_request_budget = TileRequestBudget::new(
                tile_visits.len(),
                crate::tile_cache::get_tile_size(),
                rayon::current_num_threads(),
            );

            {
                let current_index = self.current_index;
                let loader = &mut self.loader;
                let tone_map = self.hdr_renderer.tone_map;
                #[cfg(feature = "tile-debug")]
                let rot = if rotation != 0 {
                    Some(egui::emath::Rot2::from_angle(angle))
                } else {
                    None
                };

                let tm = self
                    .tile_manager
                    .as_mut()
                    .expect("tile_manager accessed without active tiled source");

                for (idx, (coord, tile_screen_rect, uv)) in tile_visits.iter().enumerate() {
                    if tile_plane_kind_for_backend(plane_backend) == TiledPlaneKind::Hdr {
                        draw_hdr_plane_tile_visit(
                            ui,
                            HdrPlaneTileVisit {
                                screen_rect,
                                layout: &layout,
                                render_plan: &render_plan,
                                plane_backend,
                                hdr_source_for_frame: hdr_source_for_frame.as_ref(),
                                tm,
                                budget: &mut tile_request_budget,
                                primary_visible_coords,
                                tile_visits_len: tile_visits.len(),
                                visit_idx: idx,
                                coord: *coord,
                                tile_screen_rect: *tile_screen_rect,
                                rotation_steps: rotation,
                                loader,
                                current_index,
                                tone_map,
                                alpha: tile_alpha,
                                show_tile_debug_osd: self.settings.show_osd,
                                hdr_pending_work: Arc::clone(&self.hdr_pending_work),
                            },
                        );
                        continue;
                    }

                    let allow_upload = newly_uploaded < tile_upload_quota;
                    let (status, just_uploaded) =
                        tm.get_or_create_tile(*coord, &ctx_ref, allow_upload, visible_coords);

                    if just_uploaded {
                        newly_uploaded += 1;
                        uploaded_coords_scratch.push(*coord);
                    }

                    match status {
                        TileStatus::Ready(handle, _ready_at) => {
                            // Draw tile at full opacity immediately.
                            // No fade-in: the preview texture is always rendered underneath,
                            // so tile pop-in is not jarring. Fade caused continuous repaints
                            // that wasted CPU/GPU cycles even when the user was not interacting.
                            draw_sdr_texture_plane(
                                ui,
                                screen_rect,
                                handle.id(),
                                *tile_screen_rect,
                                *uv,
                                Color32::WHITE,
                                &layout,
                            );

                            // DEBUG: Visual confirmation of high-res tile placement
                            #[cfg(feature = "tile-debug")]
                            if self.settings.show_osd {
                                draw_tile_debug_border(ui, *tile_screen_rect, layout.pivot, rot);
                            }
                        }
                        TileStatus::Pending(needs_request) => {
                            if needs_request {
                                let is_primary_visible = primary_visible_coords.contains(coord);
                                let pending_key =
                                    tile_pending_key_for_backend(*coord, plane_backend);
                                if !tile_request_budget.try_mark_pending(
                                    &mut tm.pending_tiles,
                                    pending_key,
                                    is_primary_visible,
                                ) {
                                    continue; // Don't break — still need to draw already-Ready tiles below
                                }
                                let source = tm.get_source();
                                let priority = tile_request_priority(tile_visits.len(), idx);
                                if let Some(source) = tile_decode_source_for_backend(
                                    plane_backend,
                                    Some(source),
                                    hdr_source_for_frame.as_ref(),
                                ) {
                                    if !loader.request_tile(
                                        current_index,
                                        tm.decode_profile.clone(),
                                        priority,
                                        source,
                                        coord.col,
                                        coord.row,
                                    ) {
                                        tm.pending_tiles.remove(&pending_key);
                                    }
                                } else {
                                    tm.pending_tiles.remove(&pending_key);
                                }
                            }
                        }
                    }
                }

                // GPU textures are authoritative after upload; drop redundant CPU copies
                // in one write-lock batch rather than per-tile inside get_or_create_tile.
                if !uploaded_coords_scratch.is_empty() {
                    tm.release_cpu_pixels_for_coords(&uploaded_coords_scratch);
                }
            }

            // DEBUG HUD: real-time tiled rendering diagnostics
            #[cfg(feature = "tile-debug")]
            if self.settings.show_osd {
                let (vis_gpu, vis_ready, vis_pending) =
                    self.tile_manager().stats_for_visible(visible_coords);
                let (total_gpu, total_mem, _total_pnd) = self.tile_manager().tiles_and_pending();

                let debug_text = format!(
                    "VIS: {} (GPU:{} RDY:{} PND:{}) | ALL: (GPU:{} MEM:{}) | SCALE: {:.3}",
                    visible_coords.len(),
                    vis_gpu,
                    vis_ready,
                    vis_pending,
                    total_gpu,
                    total_mem,
                    effective_scale
                );
                ui.painter().text(
                    screen_rect.right_bottom() - egui::vec2(10.0, 10.0),
                    egui::Align2::RIGHT_BOTTOM,
                    debug_text,
                    egui::FontId::monospace(14.0),
                    Color32::from_rgb(0, 255, 0),
                );
            }

            // ANTI-STALL LOGIC:
            // If we uploaded tiles this frame, OR if there are more ready to upload in CPU cache,
            // request another repaint immediately to keep the pipeline moving.
            let has_more_ready = should_repaint_for_ready_tiles_for_backend(
                plane_backend,
                self.tile_manager().has_ready_to_upload(visible_coords)
                    || has_pending_visible_tiles_for_backend(
                        plane_backend,
                        &self.tile_manager().pending_tiles,
                        visible_coords,
                    ),
            );
            if newly_uploaded > 0 || has_more_ready {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(4));
            }
        }
    }
}
