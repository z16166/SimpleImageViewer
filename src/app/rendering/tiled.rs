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

use crate::app::rendering::plane::{
    clipped_plane_rect_and_uv, draw_sdr_texture_plane, hdr_image_plane_rect,
    select_tiled_plane_backend,
};
use crate::app::{ImageViewerApp, TransitionStyle};
use crate::loader::TilePixelKind;
use crate::tile_cache::{PendingTileKey, TileCoord, TileStatus};
use eframe::egui::{self, Color32, Pos2, Rect, Vec2};
use std::collections::HashSet;
use std::sync::Arc;

const FALLBACK_PREVIEW_SCALE: f32 = 0.1;
const PREVIEW_QUALITY_THRESHOLD: f32 = 1.2;
const FIT_SCALE_BUFFER: f32 = 1.05;
const BURST_UPLOAD_MULT: usize = 4;
/// Hard per-frame upload cap for 512px tiles (each tile = 1MB RGBA).
/// 16 × 1MB = 16MB per frame — safe for all GPU tiers.
const BURST_UPLOAD_MAX_512: usize = 16;

pub(crate) fn should_draw_tiled_preview_transition(
    transition: TransitionStyle,
    is_animating: bool,
    has_preview_texture: bool,
) -> bool {
    is_animating
        && has_preview_texture
        && matches!(
            transition,
            TransitionStyle::PageFlip | TransitionStyle::Ripple | TransitionStyle::Curtain
        )
}

fn rotated_axis_aligned_rect(rect: Rect, pivot: Pos2, angle: f32) -> Rect {
    let rot = egui::emath::Rot2::from_angle(angle);
    let corners = [
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
    ]
    .map(|p| pivot + rot * (p - pivot));
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
}

fn hdr_tile_plane_rect_for_sdr_tile(
    tile_screen_rect: Rect,
    pivot: Pos2,
    rotation_steps: i32,
) -> Rect {
    let rotation_steps = rotation_steps.rem_euclid(4);
    if rotation_steps == 0 {
        tile_screen_rect
    } else {
        rotated_axis_aligned_rect(
            tile_screen_rect,
            pivot,
            rotation_steps as f32 * (std::f32::consts::PI / 2.0),
        )
    }
}

fn clipped_hdr_tile_plane(tile_screen_rect: Rect, clip_rect: Rect) -> Option<(Rect, Rect)> {
    let rect = tile_screen_rect.intersect(clip_rect);
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return None;
    }

    let uv_min_x =
        ((rect.min.x - tile_screen_rect.min.x) / tile_screen_rect.width()).clamp(0.0, 1.0);
    let uv_max_x =
        ((rect.max.x - tile_screen_rect.min.x) / tile_screen_rect.width()).clamp(0.0, 1.0);
    let uv_min_y =
        ((rect.min.y - tile_screen_rect.min.y) / tile_screen_rect.height()).clamp(0.0, 1.0);
    let uv_max_y =
        ((rect.max.y - tile_screen_rect.min.y) / tile_screen_rect.height()).clamp(0.0, 1.0);
    let uv = Rect::from_min_max(Pos2::new(uv_min_x, uv_min_y), Pos2::new(uv_max_x, uv_max_y));
    Some((rect, uv))
}

#[cfg(feature = "tile-debug")]
fn draw_tile_debug_border(ui: &egui::Ui, rect: Rect, pivot: Pos2, rot: Option<egui::emath::Rot2>) {
    if let Some(r) = rot {
        let p1 = pivot + r * (rect.left_top() - pivot);
        let p2 = pivot + r * (rect.right_top() - pivot);
        let p3 = pivot + r * (rect.right_bottom() - pivot);
        let p4 = pivot + r * (rect.left_bottom() - pivot);
        ui.painter().line_segment(
            [p1, p2],
            egui::Stroke::new(1.0, Color32::from_rgb(0, 255, 0)),
        );
        ui.painter().line_segment(
            [p2, p3],
            egui::Stroke::new(1.0, Color32::from_rgb(0, 255, 0)),
        );
        ui.painter().line_segment(
            [p3, p4],
            egui::Stroke::new(1.0, Color32::from_rgb(0, 255, 0)),
        );
        ui.painter().line_segment(
            [p4, p1],
            egui::Stroke::new(1.0, Color32::from_rgb(0, 255, 0)),
        );
    } else {
        ui.painter().rect(
            rect,
            0.0,
            Color32::TRANSPARENT,
            egui::Stroke::new(1.0, Color32::from_rgb(0, 255, 0)),
            egui::StrokeKind::Inside,
        );
    }
}

fn should_schedule_tile_request(
    is_cached: bool,
    pending_count: usize,
    pending_cap: usize,
    hard_pending_cap: usize,
    scheduled_this_frame: usize,
    frame_schedule_cap: usize,
    is_primary_visible: bool,
) -> bool {
    !is_cached
        && pending_count < hard_pending_cap
        && scheduled_this_frame < frame_schedule_cap
        && (is_primary_visible || pending_count < pending_cap)
}

fn tile_request_pending_cap(visible_count: usize, tile_size: u32) -> usize {
    let scale = if tile_size >= 1024 { 2 } else { 1 };
    if visible_count > 1000 {
        24 / scale
    } else if visible_count > 200 {
        48 / scale
    } else if visible_count > 50 {
        64 / scale
    } else {
        96 / scale
    }
}

fn tile_request_hard_pending_cap(tile_size: u32) -> usize {
    if tile_size >= 1024 { 96 } else { 192 }
}

fn tile_request_frame_schedule_cap(worker_threads: usize, tile_size: u32) -> usize {
    let scale = if tile_size >= 1024 { 1 } else { 2 };
    worker_threads.max(1) * scale
}

fn hdr_tile_cache_key_for_coord(
    source: &dyn crate::hdr::tiled::HdrTiledSource,
    coord: TileCoord,
) -> (u32, u32, u32, u32) {
    let ts = crate::tile_cache::get_tile_size();
    let tile_x = coord.col * ts;
    let tile_y = coord.row * ts;
    let tile_w = ts.min(source.width() - tile_x);
    let tile_h = ts.min(source.height() - tile_y);
    (tile_x, tile_y, tile_w, tile_h)
}

fn prioritize_hdr_tile_visits(
    primary_visible: &[(TileCoord, Rect, Rect)],
    padded_visible: &[(TileCoord, Rect, Rect)],
) -> Vec<(TileCoord, Rect, Rect)> {
    let mut ordered = primary_visible.to_vec();
    let primary_coords = primary_visible
        .iter()
        .map(|(coord, _, _)| *coord)
        .collect::<HashSet<_>>();
    ordered.extend(
        padded_visible
            .iter()
            .filter(|(coord, _, _)| !primary_coords.contains(coord))
            .copied(),
    );
    ordered
}

fn tiled_lookahead_padding(hardware_padding: f32, tile_size: u32) -> f32 {
    hardware_padding.min(tile_size as f32 * 2.0)
}

fn should_invalidate_tile_requests_on_pan_drag() -> bool {
    false
}

fn should_draw_sdr_preview_for_tiled_mode(
    draw_sdr_tiles: bool,
    effective_scale: f32,
    tile_threshold: f32,
) -> bool {
    let _ = (effective_scale, tile_threshold);
    draw_sdr_tiles
}

fn should_draw_hdr_preview_for_tiled_mode(
    draw_sdr_tiles: bool,
    effective_scale: f32,
    tile_threshold: f32,
) -> bool {
    let _ = (effective_scale, tile_threshold);
    !draw_sdr_tiles
}

fn should_draw_hdr_tiles_for_tiled_mode(draw_sdr_tiles: bool, has_cached_tile: bool) -> bool {
    !draw_sdr_tiles && has_cached_tile
}

fn tiled_plane_threshold(preview_scale: f32, fit_scale: f32, tile_size: u32) -> f32 {
    if preview_scale >= fit_scale {
        (preview_scale * PREVIEW_QUALITY_THRESHOLD).max(fit_scale * FIT_SCALE_BUFFER)
    } else {
        let min_tile_screen_px = 64.0;
        let tile_scale_min = min_tile_screen_px / tile_size as f32;
        tile_scale_min.max(fit_scale * FIT_SCALE_BUFFER)
    }
}

fn is_tiled_plane_active(effective_scale: f32, threshold: f32) -> bool {
    effective_scale >= threshold
}

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
        if canvas_resp.dragged() {
            self.pan_offset += canvas_resp.drag_delta();
            if should_invalidate_tile_requests_on_pan_drag() {
                self.generation = self.generation.wrapping_add(1);
                self.loader.set_generation(self.generation);
                if let Some(tm) = &mut self.tile_manager {
                    tm.generation = self.generation;
                    tm.pending_tiles.clear();
                }
                self.loader.flush_tile_queue();
            }
        }

        // Rotation logic
        let rotation = self.current_rotation;
        let angle = rotation as f32 * (std::f32::consts::PI / 2.0);

        // Extract dimensions first; transition handling below needs mutable access to self.
        let (full_width, full_height) = {
            let tm = self.tile_manager.as_ref().unwrap();
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
        let hdr_output_mode = hdr_source_for_frame.as_ref().map(|_| {
            crate::hdr::monitor::effective_render_output_mode(
                self.hdr_target_format,
                self.hdr_monitor_state.selection(),
            )
        });
        let plane_backend =
            select_tiled_plane_backend(hdr_output_mode, hdr_source_for_frame.is_some());
        let draw_sdr_tiles = plane_backend.draws_sdr();

        let tp = self.compute_transition_params();
        let preview_for_transition = self
            .tile_manager
            .as_ref()
            .and_then(|tm| tm.preview_texture.clone());
        if draw_sdr_tiles
            && should_draw_tiled_preview_transition(
                self.active_transition,
                tp.is_animating,
                preview_for_transition.is_some(),
            )
        {
            if let Some(preview) = preview_for_transition {
                self.draw_complex_transition(
                    ui,
                    screen_rect,
                    &preview,
                    dest,
                    unrotated_dest,
                    rotation,
                    angle,
                    tp.alpha,
                );
                return;
            }
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
        let preview_scale = if let Some(ref p) = self.tile_manager.as_ref().unwrap().preview_texture
        {
            p.size()[0] as f32 / rotated_img_size.x.max(1.0)
        } else {
            FALLBACK_PREVIEW_SCALE // Fallback
        };

        let threshold =
            tiled_plane_threshold(preview_scale, fit_scale, crate::tile_cache::get_tile_size());

        let effective_scale = dest.width() / rotated_img_size.x;

        if should_draw_hdr_preview_for_tiled_mode(draw_sdr_tiles, effective_scale, threshold) {
            if let Some(hdr_preview) = self
                .current_hdr_tiled_preview
                .as_ref()
                .and_then(|current| current.image_for_index(self.current_index))
            {
                let hdr_rect = hdr_image_plane_rect(&layout);
                if let Some((clipped_rect, uv_rect)) =
                    clipped_plane_rect_and_uv(hdr_rect, screen_rect)
                {
                    ui.painter()
                        .add(crate::hdr::renderer::hdr_image_plane_callback_with_uv(
                            clipped_rect,
                            Arc::clone(hdr_preview),
                            self.hdr_renderer.tone_map,
                            self.hdr_target_format
                                .unwrap_or(wgpu::TextureFormat::Bgra8Unorm),
                            hdr_output_mode.unwrap_or(
                                crate::hdr::renderer::HdrRenderOutputMode::SdrToneMapped,
                            ),
                            rotation as u32,
                            1.0,
                            uv_rect,
                        ));
                }
            }
        }

        // Draw SDR preview only when SDR tiled rendering is the active mode.
        if should_draw_sdr_preview_for_tiled_mode(draw_sdr_tiles, effective_scale, threshold) {
            if let Some(ref preview) = self.tile_manager.as_ref().unwrap().preview_texture {
                let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
                draw_sdr_texture_plane(
                    ui,
                    screen_rect,
                    preview.id(),
                    unrotated_dest,
                    uv,
                    Color32::WHITE,
                    &layout,
                );
            }
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
            let visible = self.tile_manager.as_ref().unwrap().visible_tiles(
                unrotated_dest,
                tile_clip,
                padding,
            );
            let primary_visible =
                self.tile_manager
                    .as_ref()
                    .unwrap()
                    .visible_tiles(unrotated_dest, tile_clip, 0.0);
            let tile_visits = if draw_sdr_tiles {
                visible.clone()
            } else {
                prioritize_hdr_tile_visits(&primary_visible, &visible)
            };
            let primary_visible_coords = primary_visible
                .iter()
                .map(|(coord, _, _)| *coord)
                .collect::<HashSet<_>>();
            let visible_coords: Vec<TileCoord> = visible.iter().map(|(c, _, _)| *c).collect();
            if let Some(hdr_source) = hdr_source_for_frame.as_ref() {
                let protected_keys: Vec<_> = primary_visible
                    .iter()
                    .map(|(coord, _, _)| hdr_tile_cache_key_for_coord(hdr_source.as_ref(), *coord))
                    .collect();
                hdr_source.protect_cached_tiles(&protected_keys);
            }
            if let Some(tm) = &mut self.tile_manager {
                tm.retain_pending_tiles(&visible_coords);
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
                (self.tile_upload_quota * BURST_UPLOAD_MULT).min(burst_upload_max) // Burst mode
            } else {
                self.tile_upload_quota.min(burst_upload_max) // Stable mode also capped
            };

            let mut newly_uploaded = 0;
            let tile_pending_cap =
                tile_request_pending_cap(tile_visits.len(), crate::tile_cache::get_tile_size());
            let tile_hard_pending_cap =
                tile_request_hard_pending_cap(crate::tile_cache::get_tile_size());
            let tile_frame_schedule_cap = tile_request_frame_schedule_cap(
                rayon::current_num_threads(),
                crate::tile_cache::get_tile_size(),
            );
            let mut hdr_scheduled_this_frame = 0;

            {
                let tm = self.tile_manager.as_mut().unwrap();
                let pivot = dest.center();
                #[cfg(feature = "tile-debug")]
                let rot = if rotation != 0 {
                    Some(egui::emath::Rot2::from_angle(angle))
                } else {
                    None
                };

                for (idx, (coord, tile_screen_rect, uv)) in tile_visits.iter().enumerate() {
                    if !draw_sdr_tiles {
                        if let Some(hdr_source) = hdr_source_for_frame.as_ref() {
                            let is_primary_visible = primary_visible_coords.contains(coord);
                            let (tile_x, tile_y, tile_w, tile_h) =
                                hdr_tile_cache_key_for_coord(hdr_source.as_ref(), *coord);
                            let Some(hdr_tile) =
                                hdr_source.cached_tile_rgba32f_arc(tile_x, tile_y, tile_w, tile_h)
                            else {
                                let hdr_pending_count = tm.pending_tiles.len();
                                if should_schedule_tile_request(
                                    false,
                                    hdr_pending_count,
                                    tile_pending_cap,
                                    tile_hard_pending_cap,
                                    hdr_scheduled_this_frame,
                                    tile_frame_schedule_cap,
                                    is_primary_visible,
                                ) && tm
                                    .pending_tiles
                                    .insert(PendingTileKey::new(*coord, TilePixelKind::Hdr))
                                {
                                    self.loader.request_hdr_tile(
                                        self.current_index,
                                        tm.generation,
                                        (tile_visits.len() - idx) as f32,
                                        Arc::clone(hdr_source),
                                        coord.col,
                                        coord.row,
                                    );
                                    hdr_scheduled_this_frame += 1;
                                }
                                continue;
                            };
                            if !should_draw_hdr_tiles_for_tiled_mode(draw_sdr_tiles, true) {
                                continue;
                            }

                            let unclipped_hdr_rect = hdr_tile_plane_rect_for_sdr_tile(
                                *tile_screen_rect,
                                pivot,
                                rotation,
                            );
                            if let Some((hdr_rect, uv_rect)) =
                                clipped_hdr_tile_plane(unclipped_hdr_rect, screen_rect)
                            {
                                ui.painter()
                                    .add(crate::hdr::renderer::hdr_tile_plane_callback_with_uv(
                                    hdr_rect,
                                    hdr_tile,
                                    self.hdr_renderer.tone_map,
                                    self.hdr_target_format
                                        .unwrap_or(wgpu::TextureFormat::Bgra8Unorm),
                                    hdr_output_mode.unwrap_or(
                                        crate::hdr::renderer::HdrRenderOutputMode::SdrToneMapped,
                                    ),
                                    rotation as u32,
                                    1.0,
                                    uv_rect,
                                ));

                                #[cfg(feature = "tile-debug")]
                                if self.settings.show_osd {
                                    draw_tile_debug_border(ui, hdr_rect, pivot, None);
                                }
                            }
                        }
                        continue;
                    }

                    let allow_upload = newly_uploaded < tile_upload_quota;
                    let (status, just_uploaded) =
                        tm.get_or_create_tile(*coord, &ctx_ref, allow_upload, &visible_coords);

                    if just_uploaded {
                        newly_uploaded += 1;
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
                                draw_tile_debug_border(ui, *tile_screen_rect, pivot, rot);
                            }
                        }
                        TileStatus::Pending(needs_request) => {
                            if needs_request {
                                if tm.pending_tiles.len() >= tile_pending_cap {
                                    continue; // Don't break — still need to draw already-Ready tiles below
                                }
                                let source = tm.get_source();
                                let generation = tm.generation;
                                // visible list is already sorted by distance to center
                                let priority = (visible.len() - idx) as f32;
                                self.loader.request_tile(
                                    self.current_index,
                                    generation,
                                    priority,
                                    source,
                                    coord.col,
                                    coord.row,
                                );
                                tm.pending_tiles
                                    .insert(PendingTileKey::new(*coord, TilePixelKind::Sdr));
                            }
                        }
                    }
                }
            }

            // DEBUG HUD: real-time tiled rendering diagnostics
            #[cfg(feature = "tile-debug")]
            if self.settings.show_osd {
                let (vis_gpu, vis_ready, vis_pending) = self
                    .tile_manager
                    .as_ref()
                    .unwrap()
                    .stats_for_visible(&visible_coords);
                let (total_gpu, total_mem, _total_pnd) =
                    self.tile_manager.as_ref().unwrap().tiles_and_pending();

                let debug_text = format!(
                    "VIS: {} (GPU:{} RDY:{} PND:{}) | ALL: (GPU:{} MEM:{}) | SCALE: {:.3}",
                    visible.len(),
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
            let has_more_ready = self
                .tile_manager
                .as_ref()
                .unwrap()
                .has_ready_to_upload(&visible_coords)
                && draw_sdr_tiles;
            if newly_uploaded > 0 || has_more_ready {
                ui.ctx().request_repaint();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        clipped_hdr_tile_plane, hdr_tile_plane_rect_for_sdr_tile, is_tiled_plane_active,
        rotated_axis_aligned_rect, should_draw_hdr_preview_for_tiled_mode,
        should_draw_hdr_tiles_for_tiled_mode, should_draw_sdr_preview_for_tiled_mode,
        should_draw_tiled_preview_transition, should_invalidate_tile_requests_on_pan_drag,
        should_schedule_tile_request, tile_request_frame_schedule_cap,
        tile_request_hard_pending_cap, tile_request_pending_cap, tiled_plane_threshold,
    };
    use crate::app::TransitionStyle;
    use crate::tile_cache::TileCoord;
    use eframe::egui::{Pos2, Rect};

    #[test]
    fn tiled_preview_supports_complex_transitions() {
        assert!(should_draw_tiled_preview_transition(
            TransitionStyle::Curtain,
            true,
            true
        ));
        assert!(should_draw_tiled_preview_transition(
            TransitionStyle::PageFlip,
            true,
            true
        ));
        assert!(should_draw_tiled_preview_transition(
            TransitionStyle::Ripple,
            true,
            true
        ));
        assert!(!should_draw_tiled_preview_transition(
            TransitionStyle::Fade,
            true,
            true
        ));
        assert!(!should_draw_tiled_preview_transition(
            TransitionStyle::Curtain,
            false,
            true
        ));
        assert!(!should_draw_tiled_preview_transition(
            TransitionStyle::Curtain,
            true,
            false
        ));
    }

    #[test]
    fn rotated_axis_aligned_rect_swaps_size_for_quarter_turns() {
        let rect = Rect::from_min_max(Pos2::new(10.0, 20.0), Pos2::new(30.0, 60.0));
        let pivot = Pos2::new(20.0, 40.0);

        let rotated = rotated_axis_aligned_rect(rect, pivot, std::f32::consts::FRAC_PI_2);

        assert_eq!(rotated.width(), rect.height());
        assert_eq!(rotated.height(), rect.width());
        assert_eq!(rotated.center(), rect.center());
    }

    #[test]
    fn hdr_tile_plane_rect_matches_sdr_tile_geometry() {
        let rect = Rect::from_min_max(Pos2::new(10.0, 20.0), Pos2::new(30.0, 60.0));
        let pivot = Pos2::new(20.0, 40.0);

        assert_eq!(hdr_tile_plane_rect_for_sdr_tile(rect, pivot, 0), rect);

        let rotated = hdr_tile_plane_rect_for_sdr_tile(rect, pivot, 1);
        assert_eq!(
            rotated,
            rotated_axis_aligned_rect(rect, pivot, std::f32::consts::FRAC_PI_2)
        );
    }

    #[test]
    fn clipped_hdr_tile_plane_preserves_visible_uv_subrect() {
        let tile_rect = Rect::from_min_max(Pos2::new(-50.0, 10.0), Pos2::new(50.0, 110.0));
        let clip = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(100.0, 100.0));

        let (rect, uv) = clipped_hdr_tile_plane(tile_rect, clip).expect("visible clipped tile");

        assert_eq!(
            rect,
            Rect::from_min_max(Pos2::new(0.0, 10.0), Pos2::new(50.0, 100.0))
        );
        assert_eq!(
            uv,
            Rect::from_min_max(Pos2::new(0.5, 0.0), Pos2::new(1.0, 0.9))
        );
    }

    #[test]
    fn tile_request_scheduling_is_budgeted() {
        assert!(!should_schedule_tile_request(true, 0, 96, 192, 0, 32, true));
        assert!(should_schedule_tile_request(
            false, 2, 96, 192, 0, 32, false
        ));
        assert!(!should_schedule_tile_request(
            false, 96, 96, 192, 0, 32, false
        ));
        assert!(should_schedule_tile_request(
            false, 96, 96, 192, 0, 32, true
        ));
        assert!(!should_schedule_tile_request(
            false, 192, 96, 192, 0, 32, true
        ));
        assert!(!should_schedule_tile_request(
            false, 2, 96, 192, 32, 32, true
        ));
    }

    #[test]
    fn tile_request_pending_cap_scales_like_sdr_tile_queue() {
        assert_eq!(tile_request_pending_cap(10, 512), 96);
        assert_eq!(tile_request_pending_cap(60, 512), 64);
        assert_eq!(tile_request_pending_cap(201, 512), 48);
        assert_eq!(tile_request_pending_cap(1001, 512), 24);
        assert_eq!(tile_request_pending_cap(60, 1024), 32);
    }

    #[test]
    fn tile_request_hard_pending_cap_bounds_primary_overcommit() {
        assert_eq!(tile_request_hard_pending_cap(512), 192);
        assert_eq!(tile_request_hard_pending_cap(1024), 96);
    }

    #[test]
    fn tile_request_frame_schedule_cap_limits_queue_bursts() {
        assert_eq!(tile_request_frame_schedule_cap(8, 512), 16);
        assert_eq!(tile_request_frame_schedule_cap(8, 1024), 8);
        assert_eq!(tile_request_frame_schedule_cap(0, 512), 2);
    }

    #[test]
    fn hdr_tile_visit_order_prioritizes_primary_visible_before_lookahead() {
        let primary = vec![tile_visit(3, 3), tile_visit(4, 3)];
        let padded = vec![
            tile_visit(2, 3),
            tile_visit(3, 3),
            tile_visit(4, 3),
            tile_visit(5, 3),
        ];

        let ordered = super::prioritize_hdr_tile_visits(&primary, &padded);
        let ordered_coords = ordered
            .iter()
            .map(|(coord, _, _)| *coord)
            .collect::<Vec<_>>();

        assert_eq!(
            ordered_coords,
            vec![
                TileCoord { col: 3, row: 3 },
                TileCoord { col: 4, row: 3 },
                TileCoord { col: 2, row: 3 },
                TileCoord { col: 5, row: 3 },
            ]
        );
    }

    #[test]
    fn tiled_lookahead_padding_is_capped_to_two_tile_widths() {
        assert_eq!(super::tiled_lookahead_padding(2048.0, 512), 1024.0);
        assert_eq!(super::tiled_lookahead_padding(1024.0, 1024), 1024.0);
    }

    #[test]
    fn pan_drag_keeps_tile_generation_and_worker_queue_alive() {
        assert!(!should_invalidate_tile_requests_on_pan_drag());
    }

    #[test]
    fn native_hdr_tiled_mode_hides_sdr_preview_once_tiles_are_active() {
        assert!(!should_draw_sdr_preview_for_tiled_mode(false, 2.0, 1.0));
        assert!(!should_draw_sdr_preview_for_tiled_mode(false, 0.5, 1.0));
        assert!(should_draw_sdr_preview_for_tiled_mode(true, 2.0, 1.0));
    }

    #[test]
    fn native_hdr_tiled_mode_keeps_hdr_preview_as_base_plane() {
        assert!(should_draw_hdr_preview_for_tiled_mode(false, 0.5, 1.0));
        assert!(should_draw_hdr_preview_for_tiled_mode(false, 2.0, 1.0));
        assert!(!should_draw_hdr_preview_for_tiled_mode(true, 0.5, 1.0));
    }

    #[test]
    fn native_hdr_tiled_mode_draws_cached_tiles_over_hdr_preview() {
        assert!(!should_draw_hdr_tiles_for_tiled_mode(false, false));
        assert!(should_draw_hdr_tiles_for_tiled_mode(false, true));
        assert!(!should_draw_hdr_tiles_for_tiled_mode(true, true));
    }

    #[test]
    fn tiled_plane_threshold_matches_preview_quality_policy() {
        assert_eq!(tiled_plane_threshold(0.5, 0.25, 512), 0.6);
        assert_eq!(tiled_plane_threshold(0.05, 0.25, 512), 0.2625);
        assert!(!is_tiled_plane_active(0.59, 0.6));
        assert!(is_tiled_plane_active(0.6, 0.6));
    }

    fn tile_visit(col: u32, row: u32) -> (TileCoord, Rect, Rect) {
        (
            TileCoord { col, row },
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
        )
    }
}
