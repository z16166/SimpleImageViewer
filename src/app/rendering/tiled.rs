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

use crate::app::ImageViewerApp;
use crate::app::rendering::geometry::{
    rotated_image_size_for_display, unrotated_draw_rect_for_display,
};
use crate::tile_cache::{TileCoord, TileStatus};
use eframe::egui::{self, Color32, Pos2, Rect, Vec2};

const FALLBACK_PREVIEW_SCALE: f32 = 0.1;
const PREVIEW_QUALITY_THRESHOLD: f32 = 1.2;
const FIT_SCALE_BUFFER: f32 = 1.05;
const BURST_UPLOAD_MULT: usize = 4;
/// Hard per-frame upload cap for 512px tiles (each tile = 1MB RGBA).
/// 16 × 1MB = 16MB per frame — safe for all GPU tiers.
const BURST_UPLOAD_MAX_512: usize = 16;

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
            self.generation = self.generation.wrapping_add(1);
            self.loader.set_generation(self.generation);
            if let Some(tm) = &mut self.tile_manager {
                tm.generation = self.generation;
                tm.pending_tiles.clear();
            }
            self.loader.flush_tile_queue();
        }

        // Rotation logic
        let rotation = self.current_rotation;
        let angle = rotation as f32 * (std::f32::consts::PI / 2.0);

        // Extract immutable data first (avoids borrow conflict with compute_display_rect)
        let tm_ref = self.tile_manager.as_ref().unwrap();
        let img_size = Vec2::new(tm_ref.full_width as f32, tm_ref.full_height as f32);

        let rotated_img_size = rotated_image_size_for_display(img_size, rotation);
        let dest = self.compute_display_rect(rotated_img_size, screen_rect);

        // The painter transform will handle the actual rotation.
        // We need to draw the UNROTATED image into a rect that, when rotated, matches 'dest'.
        let unrotated_dest = unrotated_draw_rect_for_display(dest, rotation);

        // 1. Draw preview texture as blurry background
        if let Some(ref preview) = self.tile_manager.as_ref().unwrap().preview_texture {
            let mut mesh = egui::Mesh::with_texture(preview.id());
            let color = Color32::WHITE;
            let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
            mesh.add_rect_with_uv(unrotated_dest, uv, color);

            if rotation != 0 {
                let pivot = dest.center();
                let rot = egui::emath::Rot2::from_angle(angle);
                for v in &mut mesh.vertices {
                    v.pos = pivot + rot * (v.pos - pivot);
                }
            }
            ui.painter()
                .with_clip_rect(screen_rect)
                .add(egui::Shape::mesh(mesh));
        }

        // 2. Render high-res tiles.
        // We use a dynamic threshold: Never trigger tiling in "Fit to Window" mode (regardless of image size).
        // For giant images, we also only trigger tiling when the effective scale exceeds
        // the preview scale, ensuring we don't thrash VRAM for no visual gain.
        let fit_scale = (screen_rect.width() / rotated_img_size.x)
            .min(screen_rect.height() / rotated_img_size.y)
            .min(1.0);

        // preview_scale: ratio of preview texture resolution to the ORIGINAL image resolution.
        // This tells us at what display scale the preview's native pixels would be 1:1.
        // Above this scale, tiles provide higher quality than the preview.
        let preview_scale = if let Some(ref p) = tm_ref.preview_texture {
            p.size()[0] as f32 / rotated_img_size.x.max(1.0)
        } else {
            FALLBACK_PREVIEW_SCALE // Fallback
        };

        // Trigger tiling when the display resolution exceeds the preview's native resolution.
        // Two scenarios:
        // 1. HQ preview available (preview_scale >= fit_scale): tile when zoomed past preview quality
        // 2. LQ bootstrap preview (preview_scale < fit_scale): use conservative threshold to avoid
        //    flooding the queue with thousands of tiles before HQ preview arrives
        let threshold = if preview_scale >= fit_scale {
            // Tile when zoomed sufficiently past preview's native resolution.
            // At preview_scale * 1.0, tiles offer no visible improvement over the preview.
            // At 1.2x, tiles are noticeably sharper while keeping tile count manageable.
            (preview_scale * PREVIEW_QUALITY_THRESHOLD).max(fit_scale * FIT_SCALE_BUFFER)
        } else {
            // LQ bootstrap: require tiles to render at >= 64 screen pixels before loading
            let min_tile_screen_px = 64.0;
            let tile_scale_min = min_tile_screen_px / crate::tile_cache::get_tile_size() as f32;
            tile_scale_min.max(fit_scale * FIT_SCALE_BUFFER)
        };

        let effective_scale = dest.width() / rotated_img_size.x;

        // Log threshold diagnostics once per image load
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

        if effective_scale >= threshold {
            // Compute visible tiles using the UNROTATED destination rect.
            // When rotation is active, we must inverse-rotate the screen clip
            // region into unrotated coordinate space. Otherwise, for extremely
            // tall/narrow images rotated 90°/270°, the unrotated rect is narrow
            // and its intersection with screen_rect only covers the center tiles.
            let padding = self.hardware_tier.look_ahead_padding();
            let tile_clip = if rotation != 0 {
                let inv_rot = egui::emath::Rot2::from_angle(-angle);
                let pivot = dest.center();
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
            let visible_coords: Vec<TileCoord> = visible.iter().map(|(c, _, _)| *c).collect();

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

            {
                let tm = self.tile_manager.as_mut().unwrap();
                let pivot = dest.center();
                let rot = if rotation != 0 {
                    Some(egui::emath::Rot2::from_angle(angle))
                } else {
                    None
                };

                for (idx, (coord, tile_screen_rect, uv)) in visible.iter().enumerate() {
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
                            let mut mesh = egui::Mesh::with_texture(handle.id());
                            mesh.add_rect_with_uv(*tile_screen_rect, *uv, Color32::WHITE);
                            if let Some(r) = rot {
                                for v in &mut mesh.vertices {
                                    v.pos = pivot + r * (v.pos - pivot);
                                }
                            }
                            ui.painter()
                                .with_clip_rect(screen_rect)
                                .add(egui::Shape::mesh(mesh));

                            if let Some(hdr_source) = self
                                .current_hdr_tiled_image
                                .as_ref()
                                .and_then(|current| current.source_for_index(self.current_index))
                            {
                                let ts = crate::tile_cache::get_tile_size();
                                let tile_x = coord.col * ts;
                                let tile_y = coord.row * ts;
                                let tile_w = ts.min(hdr_source.width() - tile_x);
                                let tile_h = ts.min(hdr_source.height() - tile_y);
                                match hdr_source
                                    .extract_tile_rgba32f_arc(tile_x, tile_y, tile_w, tile_h)
                                {
                                    Ok(hdr_tile) => {
                                        let hdr_rect = hdr_tile_plane_rect_for_sdr_tile(
                                            *tile_screen_rect,
                                            pivot,
                                            rotation,
                                        );
                                        ui.painter().add(
                                            crate::hdr::renderer::hdr_tile_plane_callback(
                                                hdr_rect,
                                                hdr_tile,
                                                self.hdr_renderer.tone_map,
                                                self.hdr_target_format
                                                    .unwrap_or(wgpu::TextureFormat::Bgra8Unorm),
                                                crate::hdr::monitor::effective_render_output_mode(
                                                    self.hdr_target_format,
                                                    self.hdr_monitor_state.selection(),
                                                ),
                                                rotation as u32,
                                                1.0,
                                            ),
                                        );
                                    }
                                    Err(err) => {
                                        log::warn!(
                                            "[HDR] Failed to extract HDR tile ({},{}): {}",
                                            coord.col,
                                            coord.row,
                                            err
                                        );
                                    }
                                }
                            }

                            // DEBUG: Visual confirmation of high-res tile placement
                            #[cfg(feature = "tile-debug")]
                            if self.settings.show_osd {
                                let debug_rect = *tile_screen_rect;
                                if let Some(r) = rot {
                                    // Approximate rotation of rect for border
                                    let p1 = pivot + r * (debug_rect.left_top() - pivot);
                                    let p2 = pivot + r * (debug_rect.right_top() - pivot);
                                    let p3 = pivot + r * (debug_rect.right_bottom() - pivot);
                                    let p4 = pivot + r * (debug_rect.left_bottom() - pivot);
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
                                        debug_rect,
                                        0.0,
                                        Color32::TRANSPARENT,
                                        egui::Stroke::new(1.0, Color32::from_rgb(0, 255, 0)),
                                        egui::StrokeKind::Inside,
                                    );
                                }
                            }
                        }
                        TileStatus::Pending(needs_request) => {
                            if needs_request {
                                // Dynamic pending cap: scale inversely with visible tile count.
                                // At high zoom (few tiles visible), load fast.
                                // At low zoom (many visible), allow enough to keep worker threads busy.
                                // Scale down for larger tiles to keep memory bounded.
                                let visible_count = visible.len();
                                let ts = crate::tile_cache::get_tile_size();
                                let scale = if ts >= 1024 { 2 } else { 1 }; // halve caps for 1024 tiles
                                let max_pending = if visible_count > 1000 {
                                    24 / scale
                                } else if visible_count > 200 {
                                    48 / scale
                                } else if visible_count > 50 {
                                    64 / scale
                                } else {
                                    96 / scale
                                };
                                if tm.pending_tiles.len() >= max_pending {
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
                                tm.pending_tiles.insert(*coord);
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
                .has_ready_to_upload(&visible_coords);
            if newly_uploaded > 0 || has_more_ready {
                ui.ctx().request_repaint();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{hdr_tile_plane_rect_for_sdr_tile, rotated_axis_aligned_rect};
    use eframe::egui::{Pos2, Rect};

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
}
