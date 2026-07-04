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

use super::should_draw_tiled_preview_transition;
use crate::app::TransitionStyle;
use crate::app::rendering::plan::RenderPlan;
use crate::app::rendering::plane::PlaneBackendKind;
use crate::app::rendering::transitions::TransitionParams;
use crate::hdr::tiled::HdrTiledSource;
use crate::loader::{TileDecodeSource, TilePixelKind};
use crate::tile_cache::{PendingTileKey, TileCoord, TileManager};
use eframe::egui::{self, Pos2, Rect, Vec2};
use std::collections::HashSet;
use std::sync::Arc;

use super::{FIT_SCALE_BUFFER, HDR_TILE_MIN_SCREEN_PX, PREVIEW_QUALITY_THRESHOLD};
use crate::app::rendering::geometry::PlaneLayout;
use crate::app::rendering::plane::{PlaneDrawSource, draw_plane};

pub(crate) fn should_draw_tiled_preview_transition_for_backend(
    plane_backend: PlaneBackendKind,
    transition: TransitionStyle,
    is_animating: bool,
    has_preview_texture: bool,
) -> bool {
    plane_backend == PlaneBackendKind::Sdr
        && should_draw_tiled_preview_transition(transition, is_animating, has_preview_texture)
}

/// Returns `(tile_alpha, prev_alpha)` for the HDR tiled rendering path.
///
/// For transition styles whose geometric effect is incompatible with per-tile wgpu
/// callbacks (Slide, Push, PageFlip, Ripple, Curtain), degrades to a crossfade so
/// the old image fades out and the new one fades in instead of hard-cutting.
///
/// `TransitionParams::alpha` / `prev_alpha` for these styles are their defaults
/// (1.0 / 0.0) because their custom rendering path never sets them — they cannot
/// be used directly in the tiled path.
pub(crate) fn effective_hdr_tiled_alphas(
    tp: &TransitionParams,
    style: TransitionStyle,
) -> (f32, f32) {
    if !tp.is_animating {
        return (1.0, 0.0);
    }
    match style {
        TransitionStyle::Fade | TransitionStyle::ZoomFade => {
            // Standard alpha params already encode a correct crossfade.
            (tp.alpha, tp.prev_alpha)
        }
        _ => {
            // Slide / Push (position-based), PageFlip / Ripple / Curtain (geometry-based):
            // tp.alpha / prev_alpha are at their defaults (1.0 / 0.0) for these styles.
            // Degrade to a crossfade driven by the raw normalised time `tp.t`.
            let ease_out = 1.0 - (1.0 - tp.t).powi(3);
            (ease_out, 1.0 - tp.t)
        }
    }
}

pub(crate) fn prev_transition_params_for_tiled_draw(
    tp: TransitionParams,
    prev_alpha_eff: f32,
) -> TransitionParams {
    TransitionParams {
        prev_alpha: prev_alpha_eff,
        prev_offset: Vec2::ZERO,
        prev_scale: 1.0,
        ..tp
    }
}

pub(crate) fn rotated_axis_aligned_rect(rect: Rect, pivot: Pos2, angle: f32) -> Rect {
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

pub(crate) fn tile_plane_rect_for_tile(
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

#[cfg(feature = "tile-debug")]
pub(crate) fn draw_tile_debug_border(
    ui: &egui::Ui,
    rect: Rect,
    pivot: Pos2,
    rot: Option<egui::emath::Rot2>,
) {
    if let Some(r) = rot {
        let p1 = pivot + r * (rect.left_top() - pivot);
        let p2 = pivot + r * (rect.right_top() - pivot);
        let p3 = pivot + r * (rect.right_bottom() - pivot);
        let p4 = pivot + r * (rect.left_bottom() - pivot);
        ui.painter().line_segment(
            [p1, p2],
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(0, 255, 0)),
        );
        ui.painter().line_segment(
            [p2, p3],
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(0, 255, 0)),
        );
        ui.painter().line_segment(
            [p3, p4],
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(0, 255, 0)),
        );
        ui.painter().line_segment(
            [p4, p1],
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(0, 255, 0)),
        );
    } else {
        ui.painter().rect(
            rect,
            0.0,
            egui::Color32::TRANSPARENT,
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(0, 255, 0)),
            egui::StrokeKind::Inside,
        );
    }
}

pub(crate) fn should_schedule_tile_request(
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

pub(crate) fn tile_pixel_kind_for_backend(plane_backend: PlaneBackendKind) -> TilePixelKind {
    match plane_backend {
        PlaneBackendKind::Sdr => TilePixelKind::Sdr,
        PlaneBackendKind::Hdr => TilePixelKind::Hdr,
    }
}

pub(crate) fn tile_pending_key_for_backend(
    coord: TileCoord,
    plane_backend: PlaneBackendKind,
) -> PendingTileKey {
    PendingTileKey::new(coord, tile_pixel_kind_for_backend(plane_backend))
}

pub(crate) fn tile_decode_source_for_backend(
    plane_backend: PlaneBackendKind,
    sdr_source: Option<Arc<dyn crate::loader::TiledImageSource>>,
    hdr_source: Option<&Arc<dyn crate::hdr::tiled::HdrTiledSource>>,
) -> Option<TileDecodeSource> {
    match plane_backend {
        PlaneBackendKind::Sdr => sdr_source.map(TileDecodeSource::Sdr),
        PlaneBackendKind::Hdr => hdr_source.map(|source| TileDecodeSource::Hdr(Arc::clone(source))),
    }
}

pub(crate) fn tile_request_pending_cap(visible_count: usize, tile_size: u32) -> usize {
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

pub(crate) fn tile_request_hard_pending_cap(tile_size: u32) -> usize {
    if tile_size >= 1024 { 96 } else { 192 }
}

pub(crate) fn tile_request_frame_schedule_cap(worker_threads: usize, tile_size: u32) -> usize {
    let scale = if tile_size >= 1024 { 1 } else { 2 };
    worker_threads.max(1) * scale
}

pub(crate) struct TileRequestBudget {
    pending_cap: usize,
    hard_pending_cap: usize,
    frame_schedule_cap: usize,
    scheduled_this_frame: usize,
}

impl TileRequestBudget {
    pub(crate) fn new(visible_count: usize, tile_size: u32, worker_threads: usize) -> Self {
        Self {
            pending_cap: tile_request_pending_cap(visible_count, tile_size),
            hard_pending_cap: tile_request_hard_pending_cap(tile_size),
            frame_schedule_cap: tile_request_frame_schedule_cap(worker_threads, tile_size),
            scheduled_this_frame: 0,
        }
    }

    pub(crate) fn should_schedule(
        &self,
        is_cached: bool,
        pending_count: usize,
        is_primary_visible: bool,
    ) -> bool {
        should_schedule_tile_request(
            is_cached,
            pending_count,
            self.pending_cap,
            self.hard_pending_cap,
            self.scheduled_this_frame,
            self.frame_schedule_cap,
            is_primary_visible,
        )
    }

    pub(crate) fn record_scheduled(&mut self) {
        self.scheduled_this_frame += 1;
    }

    pub(crate) fn try_mark_pending(
        &mut self,
        pending_tiles: &mut HashSet<PendingTileKey>,
        pending_key: PendingTileKey,
        is_primary_visible: bool,
    ) -> bool {
        if !self.should_schedule(false, pending_tiles.len(), is_primary_visible) {
            return false;
        }
        if !pending_tiles.insert(pending_key) {
            return false;
        }
        self.record_scheduled();
        true
    }

    #[cfg(test)]
    pub(crate) fn pending_cap(&self) -> usize {
        self.pending_cap
    }

    #[cfg(test)]
    pub(crate) fn hard_pending_cap(&self) -> usize {
        self.hard_pending_cap
    }

    #[cfg(test)]
    pub(crate) fn frame_schedule_cap(&self) -> usize {
        self.frame_schedule_cap
    }
}

pub(crate) fn hdr_tile_cache_key_for_coord(
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

pub(crate) fn prioritize_tile_visits(
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

pub(crate) fn tile_visits_for_backend(
    plane_backend: PlaneBackendKind,
    primary_visible: &[(TileCoord, Rect, Rect)],
    padded_visible: &[(TileCoord, Rect, Rect)],
) -> Vec<(TileCoord, Rect, Rect)> {
    match plane_backend {
        PlaneBackendKind::Sdr => padded_visible.to_vec(),
        PlaneBackendKind::Hdr => prioritize_tile_visits(primary_visible, padded_visible),
    }
}

pub(crate) fn tile_request_priority(tile_visit_count: usize, visit_idx: usize) -> f32 {
    tile_visit_count.saturating_sub(visit_idx) as f32
}

pub(crate) fn tiled_lookahead_padding(hardware_padding: f32, tile_size: u32) -> f32 {
    hardware_padding.min(tile_size as f32 * 2.0)
}

pub(crate) fn should_invalidate_tile_requests_on_pan_drag() -> bool {
    false
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TiledPlaneKind {
    Sdr,
    Hdr,
}

pub(crate) fn tile_plane_kind_for_backend(plane_backend: PlaneBackendKind) -> TiledPlaneKind {
    match plane_backend {
        PlaneBackendKind::Sdr => TiledPlaneKind::Sdr,
        PlaneBackendKind::Hdr => TiledPlaneKind::Hdr,
    }
}

pub(crate) fn should_draw_tiled_preview_for_backend(
    plane_backend: PlaneBackendKind,
    preview_kind: TiledPlaneKind,
) -> bool {
    tile_plane_kind_for_backend(plane_backend) == preview_kind
}

pub(crate) fn should_draw_tiled_tile_plane_for_backend(
    plane_backend: PlaneBackendKind,
    tile_plane_kind: TiledPlaneKind,
    has_cached_tile: bool,
) -> bool {
    tile_plane_kind_for_backend(plane_backend) == tile_plane_kind && has_cached_tile
}

pub(crate) fn should_repaint_for_ready_tiles_for_backend(
    plane_backend: PlaneBackendKind,
    has_ready_to_upload: bool,
) -> bool {
    match plane_backend {
        PlaneBackendKind::Sdr | PlaneBackendKind::Hdr => has_ready_to_upload,
    }
}

pub(crate) fn has_pending_visible_tiles_for_backend(
    plane_backend: PlaneBackendKind,
    pending_tiles: &HashSet<PendingTileKey>,
    visible_coords: &[TileCoord],
) -> bool {
    if plane_backend != PlaneBackendKind::Hdr {
        return false;
    }

    pending_tiles
        .iter()
        .any(|key| key.pixel_kind == TilePixelKind::Hdr && visible_coords.contains(&key.coord))
}

pub(crate) fn tiled_plane_threshold(preview_scale: f32, fit_scale: f32, tile_size: u32) -> f32 {
    if preview_scale >= fit_scale {
        (preview_scale * PREVIEW_QUALITY_THRESHOLD).max(fit_scale * FIT_SCALE_BUFFER)
    } else {
        let min_tile_screen_px = 64.0;
        let tile_scale_min = min_tile_screen_px / tile_size as f32;
        tile_scale_min.max(fit_scale * FIT_SCALE_BUFFER)
    }
}

pub(crate) fn tiled_plane_threshold_for_backend(
    plane_backend: PlaneBackendKind,
    preview_scale: f32,
    fit_scale: f32,
    tile_size: u32,
) -> f32 {
    let base = tiled_plane_threshold(preview_scale, fit_scale, tile_size);
    match plane_backend {
        PlaneBackendKind::Sdr => base,
        PlaneBackendKind::Hdr => base.max(HDR_TILE_MIN_SCREEN_PX / tile_size.max(1) as f32),
    }
}

pub(crate) fn is_tiled_plane_active(effective_scale: f32, threshold: f32) -> bool {
    effective_scale >= threshold
}

pub(crate) struct HdrTileDecodeRequest<'a> {
    pub(crate) plane_backend: PlaneBackendKind,
    pub(crate) coord: TileCoord,
    pub(crate) visit_idx: usize,
    pub(crate) tile_visits_len: usize,
    pub(crate) is_primary_visible: bool,
    pub(crate) hdr_source: &'a Arc<dyn HdrTiledSource>,
}

pub(crate) fn enqueue_hdr_plane_tile_decode(
    loader: &mut crate::loader::ImageLoader,
    current_index: usize,
    tm: &mut TileManager,
    budget: &mut TileRequestBudget,
    request: HdrTileDecodeRequest<'_>,
) {
    let HdrTileDecodeRequest {
        plane_backend,
        coord,
        visit_idx,
        tile_visits_len,
        is_primary_visible,
        hdr_source,
    } = request;
    if !budget.try_mark_pending(
        &mut tm.pending_tiles,
        tile_pending_key_for_backend(coord, plane_backend),
        is_primary_visible,
    ) {
        return;
    }
    let Some(source) = tile_decode_source_for_backend(plane_backend, None, Some(hdr_source)) else {
        return;
    };
    loader.request_tile(
        current_index,
        tm.decode_profile.clone(),
        tile_request_priority(tile_visits_len, visit_idx),
        source,
        coord.col,
        coord.row,
    );
}

/// HDR tiled plane: enqueue decode on cache miss, otherwise draw cached RGBA32F.
pub(crate) struct HdrPlaneTileVisit<'a> {
    pub(crate) screen_rect: Rect,
    pub(crate) layout: &'a PlaneLayout,
    pub(crate) render_plan: &'a RenderPlan,
    pub(crate) plane_backend: PlaneBackendKind,
    pub(crate) hdr_source_for_frame: Option<&'a Arc<dyn HdrTiledSource>>,
    pub(crate) tm: &'a mut TileManager,
    pub(crate) budget: &'a mut TileRequestBudget,
    pub(crate) primary_visible_coords: &'a HashSet<TileCoord>,
    pub(crate) tile_visits_len: usize,
    pub(crate) visit_idx: usize,
    pub(crate) coord: TileCoord,
    pub(crate) tile_screen_rect: Rect,
    pub(crate) rotation_steps: i32,
    pub(crate) loader: &'a mut crate::loader::ImageLoader,
    pub(crate) current_index: usize,
    pub(crate) tone_map: crate::hdr::types::HdrToneMapSettings,
    pub(crate) alpha: f32,
    pub(crate) show_tile_debug_osd: bool,
    pub(crate) hdr_pending_work: Arc<crate::hdr::renderer::HdrPendingWorkQueues>,
}

pub(crate) fn draw_hdr_plane_tile_visit(ui: &mut egui::Ui, visit: HdrPlaneTileVisit<'_>) {
    let HdrPlaneTileVisit {
        screen_rect,
        layout,
        render_plan,
        plane_backend,
        hdr_source_for_frame,
        tm,
        budget,
        primary_visible_coords,
        tile_visits_len,
        visit_idx,
        coord,
        tile_screen_rect,
        rotation_steps,
        loader,
        current_index,
        tone_map,
        alpha,
        #[cfg_attr(not(feature = "tile-debug"), allow(unused_variables))]
        show_tile_debug_osd,
        hdr_pending_work,
    } = visit;
    let Some(hdr_source) = hdr_source_for_frame else {
        return;
    };
    let is_primary_visible = primary_visible_coords.contains(&coord);
    let (tile_x, tile_y, tile_w, tile_h) = hdr_tile_cache_key_for_coord(hdr_source.as_ref(), coord);

    let Some(hdr_tile) = hdr_source.cached_tile_rgba32f_arc(tile_x, tile_y, tile_w, tile_h) else {
        enqueue_hdr_plane_tile_decode(
            loader,
            current_index,
            tm,
            budget,
            HdrTileDecodeRequest {
                plane_backend,
                coord,
                visit_idx,
                tile_visits_len,
                is_primary_visible,
                hdr_source,
            },
        );
        return;
    };

    if !should_draw_tiled_tile_plane_for_backend(plane_backend, TiledPlaneKind::Hdr, true) {
        return;
    }

    let pivot = layout.pivot;
    let unclipped_hdr_rect = tile_plane_rect_for_tile(tile_screen_rect, pivot, rotation_steps);

    draw_plane(
        ui,
        screen_rect,
        unclipped_hdr_rect,
        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
        layout,
        PlaneDrawSource::HdrTile {
            tile: hdr_tile,
            tone_map,
            target_format: render_plan
                .target_format
                .unwrap_or(wgpu::TextureFormat::Bgra8Unorm),
            output_mode: render_plan.output_mode,
            rotation_steps: rotation_steps as u32,
            alpha,
            hdr_pending_work: Some(Arc::clone(&hdr_pending_work)),
        },
    );

    #[cfg(feature = "tile-debug")]
    if show_tile_debug_osd {
        use crate::app::rendering::plane::clipped_plane_rect_and_uv;

        if let Some((hdr_rect, _)) = clipped_plane_rect_and_uv(unclipped_hdr_rect, screen_rect) {
            draw_tile_debug_border(ui, hdr_rect, pivot, None);
        }
    }
}

#[cfg(test)]
pub(crate) fn tile_kind_uses_shared_schedule_policy(
    _pixel_kind: TilePixelKind,
    is_cached: bool,
    pending_count: usize,
    pending_cap: usize,
    hard_pending_cap: usize,
    scheduled_this_frame: usize,
    frame_schedule_cap: usize,
    is_primary_visible: bool,
) -> bool {
    should_schedule_tile_request(
        is_cached,
        pending_count,
        pending_cap,
        hard_pending_cap,
        scheduled_this_frame,
        frame_schedule_cap,
        is_primary_visible,
    )
}
