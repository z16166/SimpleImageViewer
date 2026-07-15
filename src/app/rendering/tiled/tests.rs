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

use super::*;
use crate::app::TransitionStyle;
use crate::app::rendering::plane::{PlaneBackendKind, clipped_plane_rect_and_uv};
use crate::loader::{TileDecodeSource, TilePixelKind, TiledImageSource};
use crate::tile_cache::TileCoord;
use eframe::egui::{Pos2, Rect};
use std::collections::HashSet;
use std::sync::Arc;

#[test]
fn tiled_preview_supports_complex_transitions() {
    assert!(super::should_draw_tiled_preview_transition(
        TransitionStyle::Curtain,
        true,
        true
    ));
    assert!(super::should_draw_tiled_preview_transition(
        TransitionStyle::PageFlip,
        true,
        true
    ));
    assert!(super::should_draw_tiled_preview_transition(
        TransitionStyle::Ripple,
        true,
        true
    ));
    assert!(!super::should_draw_tiled_preview_transition(
        TransitionStyle::Fade,
        true,
        true
    ));
    assert!(!super::should_draw_tiled_preview_transition(
        TransitionStyle::Curtain,
        false,
        true
    ));
    assert!(!super::should_draw_tiled_preview_transition(
        TransitionStyle::Curtain,
        true,
        false
    ));
}

#[test]
fn test_effective_hdr_tiled_alphas() {
    use crate::app::rendering::transitions::TransitionParams;

    // Case 1: not animating -> default full opacity for new, invisible for prev
    let tp = TransitionParams {
        is_animating: false,
        alpha: 0.5,
        prev_alpha: 0.8,
        t: 0.3,
        ..Default::default()
    };
    let (tile_alpha, prev_alpha) = effective_hdr_tiled_alphas(&tp, TransitionStyle::Fade);
    assert_eq!(tile_alpha, 1.0);
    assert_eq!(prev_alpha, 0.0);

    // Case 2: animating, Fade -> uses standard alpha params
    let tp = TransitionParams {
        is_animating: true,
        alpha: 0.5,
        prev_alpha: 0.8,
        t: 0.3,
        ..Default::default()
    };
    let (tile_alpha, prev_alpha) = effective_hdr_tiled_alphas(&tp, TransitionStyle::Fade);
    assert_eq!(tile_alpha, 0.5);
    assert_eq!(prev_alpha, 0.8);

    // Case 3: animating, Curtain (geometric / position transition style) -> degraded to crossfade driven by t
    let tp = TransitionParams {
        is_animating: true,
        alpha: 1.0,      // default for curtain
        prev_alpha: 0.0, // default for curtain
        t: 0.4,
        ..Default::default()
    };
    let (tile_alpha, prev_alpha) = effective_hdr_tiled_alphas(&tp, TransitionStyle::Curtain);
    // ease_out = 1.0 - (1.0 - 0.4)^3 = 1.0 - 0.216 = 0.784
    assert!((tile_alpha - 0.784).abs() < 1e-5);
    assert!((prev_alpha - 0.6).abs() < 1e-5);

    // Case 4: animating, Push (position transition style) -> degraded to crossfade driven by t
    let tp = TransitionParams {
        is_animating: true,
        alpha: 1.0,
        prev_alpha: 0.0,
        t: 0.25,
        ..Default::default()
    };
    let (tile_alpha, prev_alpha) = effective_hdr_tiled_alphas(&tp, TransitionStyle::Push);
    // ease_out = 1.0 - (1.0 - 0.25)^3 = 1.0 - 0.421875 = 0.578125
    assert!((tile_alpha - 0.578125).abs() < 1e-5);
    assert!((prev_alpha - 0.75).abs() < 1e-5);
}

#[test]
fn test_prev_transition_params_for_tiled_draw() {
    use crate::app::rendering::transitions::TransitionParams;
    use eframe::egui::Vec2;

    let tp = TransitionParams {
        alpha: 0.5,
        scale: 2.0,
        offset: Vec2::new(10.0, 20.0),
        prev_alpha: 0.1,
        prev_scale: 0.8,
        prev_offset: Vec2::new(5.0, 5.0),
        is_animating: true,
        t: 0.3,
    };

    let prev_tp = prev_transition_params_for_tiled_draw(tp, 0.7);

    // Verification:
    // 1. prev_alpha MUST capture the provided value (0.7)
    assert_eq!(prev_tp.prev_alpha, 0.7);
    // 2. prev_offset MUST be reset to Vec2::ZERO (essential to prevent moving background)
    assert_eq!(prev_tp.prev_offset, Vec2::ZERO);
    // 3. prev_scale MUST be reset to 1.0
    assert_eq!(prev_tp.prev_scale, 1.0);
    // 4. Other fields MUST be preserved from original tp
    assert_eq!(prev_tp.alpha, 0.5);
    assert_eq!(prev_tp.scale, 2.0);
    assert_eq!(prev_tp.offset, Vec2::new(10.0, 20.0));
    assert!(prev_tp.is_animating);
    assert_eq!(prev_tp.t, 0.3);
}

#[test]
fn tiled_preview_transition_is_selected_by_backend() {
    assert!(should_draw_tiled_preview_transition_for_backend(
        PlaneBackendKind::Sdr,
        TransitionStyle::Curtain,
        true,
        true
    ));
    assert!(!should_draw_tiled_preview_transition_for_backend(
        PlaneBackendKind::Hdr,
        TransitionStyle::Curtain,
        true,
        true
    ));
}

#[test]
fn ready_tile_repaint_is_selected_by_backend() {
    assert!(should_repaint_for_ready_tiles_for_backend(
        PlaneBackendKind::Sdr,
        true
    ));
    assert!(should_repaint_for_ready_tiles_for_backend(
        PlaneBackendKind::Hdr,
        true
    ));
    assert!(!should_repaint_for_ready_tiles_for_backend(
        PlaneBackendKind::Sdr,
        false
    ));
}

#[test]
fn visible_pending_hdr_tiles_continue_repaint_until_ready() {
    let visible = HashSet::from([TileCoord { col: 3, row: 5 }]);
    let pending = HashSet::from([crate::tile_cache::PendingTileKey::new(
        TileCoord { col: 3, row: 5 },
        TilePixelKind::Hdr,
    )]);

    assert!(has_pending_visible_tiles_for_backend(
        PlaneBackendKind::Hdr,
        &pending,
        &visible
    ));
    assert!(!has_pending_visible_tiles_for_backend(
        PlaneBackendKind::Sdr,
        &pending,
        &visible
    ));
}

#[test]
fn tiled_tile_plane_is_selected_by_backend() {
    assert_eq!(
        tile_plane_kind_for_backend(PlaneBackendKind::Sdr),
        TiledPlaneKind::Sdr
    );
    assert_eq!(
        tile_plane_kind_for_backend(PlaneBackendKind::Hdr),
        TiledPlaneKind::Hdr
    );
}

#[test]
fn tiled_backend_selects_matching_pixel_kind_and_pending_key() {
    let coord = TileCoord { col: 2, row: 3 };

    assert_eq!(
        tile_pixel_kind_for_backend(PlaneBackendKind::Sdr),
        TilePixelKind::Sdr
    );
    assert_eq!(
        tile_pixel_kind_for_backend(PlaneBackendKind::Hdr),
        TilePixelKind::Hdr
    );
    assert_eq!(
        tile_pending_key_for_backend(coord, PlaneBackendKind::Sdr),
        crate::tile_cache::PendingTileKey::new(coord, TilePixelKind::Sdr)
    );
    assert_eq!(
        tile_pending_key_for_backend(coord, PlaneBackendKind::Hdr),
        crate::tile_cache::PendingTileKey::new(coord, TilePixelKind::Hdr)
    );
}

struct TestTiledSource;

impl TiledImageSource for TestTiledSource {
    fn width(&self) -> u32 {
        1
    }

    fn height(&self) -> u32 {
        1
    }

    fn extract_tile(&self, _x: u32, _y: u32, _w: u32, _h: u32) -> Arc<Vec<u8>> {
        Arc::new(vec![0, 0, 0, 255])
    }

    fn generate_preview(&self, _max_w: u32, _max_h: u32) -> (u32, u32, Vec<u8>) {
        (1, 1, vec![0, 0, 0, 255])
    }

    fn full_pixels(&self) -> Option<Arc<Vec<u8>>> {
        None
    }
}

fn test_hdr_source() -> Arc<dyn crate::hdr::tiled::HdrTiledSource> {
    Arc::new(
        crate::hdr::tiled::HdrTiledImageSource::new(crate::hdr::types::HdrImageBuffer {
            width: 1,
            height: 1,
            format: crate::hdr::types::HdrPixelFormat::Rgba32Float,
            color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
            metadata: crate::hdr::types::HdrImageMetadata::from_color_space(
                crate::hdr::types::HdrColorSpace::LinearSrgb,
            ),
            rgba_f32: Arc::new(vec![0.0, 0.0, 0.0, 1.0]),
        })
        .expect("build HDR tiled source"),
    )
}

#[test]
fn tile_decode_source_is_selected_by_backend() {
    let sdr_source: Arc<dyn TiledImageSource> = Arc::new(TestTiledSource);
    let hdr_source = test_hdr_source();

    assert!(matches!(
        tile_decode_source_for_backend(
            PlaneBackendKind::Sdr,
            Some(Arc::clone(&sdr_source)),
            Some(&hdr_source)
        ),
        Some(TileDecodeSource::Sdr(_))
    ));
    assert!(matches!(
        tile_decode_source_for_backend(
            PlaneBackendKind::Hdr,
            Some(Arc::clone(&sdr_source)),
            Some(&hdr_source)
        ),
        Some(TileDecodeSource::Hdr(_))
    ));
    assert!(
        tile_decode_source_for_backend(PlaneBackendKind::Sdr, None, Some(&hdr_source)).is_none()
    );
    assert!(
        tile_decode_source_for_backend(PlaneBackendKind::Hdr, Some(sdr_source), None).is_none()
    );
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
fn tile_plane_rect_handles_rotation_like_sdr_tiles() {
    let rect = Rect::from_min_max(Pos2::new(10.0, 20.0), Pos2::new(30.0, 60.0));
    let pivot = Pos2::new(20.0, 40.0);

    assert_eq!(tile_plane_rect_for_tile(rect, pivot, 0), rect);

    let rotated = tile_plane_rect_for_tile(rect, pivot, 1);
    assert_eq!(
        rotated,
        rotated_axis_aligned_rect(rect, pivot, std::f32::consts::FRAC_PI_2)
    );
}

#[test]
fn clipped_plane_rect_matches_tile_clipping_semantics() {
    let tile_rect = Rect::from_min_max(Pos2::new(-50.0, 10.0), Pos2::new(50.0, 110.0));
    let clip = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(100.0, 100.0));

    let (rect, uv) = clipped_plane_rect_and_uv(tile_rect, clip).expect("visible clipped tile");

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
fn sdr_tile_request_uses_shared_primary_visible_overcommit_policy() {
    let input = TileSchedulePolicyTestInput {
        is_cached: false,
        pending_count: 96,
        pending_cap: 96,
        hard_pending_cap: 192,
        scheduled_this_frame: 0,
        frame_schedule_cap: 32,
        is_primary_visible: true,
    };
    assert!(tile_kind_uses_shared_schedule_policy(
        TilePixelKind::Sdr,
        input
    ));
    let input = TileSchedulePolicyTestInput {
        is_cached: false,
        pending_count: 96,
        pending_cap: 96,
        hard_pending_cap: 192,
        scheduled_this_frame: 0,
        frame_schedule_cap: 32,
        is_primary_visible: true,
    };
    assert!(tile_kind_uses_shared_schedule_policy(
        TilePixelKind::Hdr,
        input
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
fn tile_request_budget_centralizes_caps_and_frame_counter() {
    let mut budget = TileRequestBudget::new(60, 512, 8);

    assert_eq!(budget.pending_cap(), 64);
    assert_eq!(budget.hard_pending_cap(), 192);
    assert_eq!(budget.frame_schedule_cap(), 16);
    assert!(budget.should_schedule(false, 64, true));
    assert!(!budget.should_schedule(false, 64, false));

    for _ in 0..16 {
        assert!(budget.should_schedule(false, 0, true));
        budget.record_scheduled();
    }
    assert!(!budget.should_schedule(false, 0, true));
}

#[test]
fn tile_request_budget_marks_pending_once_and_records_schedule() {
    let mut budget = TileRequestBudget::new(10, 512, 8);
    let coord = TileCoord { col: 2, row: 3 };
    let key = tile_pending_key_for_backend(coord, PlaneBackendKind::Sdr);
    let mut pending = std::collections::HashSet::new();

    assert!(budget.try_mark_pending(&mut pending, key, true));
    assert!(!budget.try_mark_pending(&mut pending, key, true));
    assert_eq!(pending.len(), 1);

    for row in 0..15 {
        let key = tile_pending_key_for_backend(TileCoord { col: 9, row }, PlaneBackendKind::Sdr);
        assert!(budget.try_mark_pending(&mut pending, key, true));
    }
    let key = tile_pending_key_for_backend(TileCoord { col: 10, row: 0 }, PlaneBackendKind::Sdr);
    assert!(!budget.try_mark_pending(&mut pending, key, true));
}

#[test]
fn tile_request_priority_is_derived_from_shared_visit_order() {
    assert_eq!(tile_request_priority(4, 0), 4);
    assert_eq!(tile_request_priority(4, 3), 1);
    assert_eq!(tile_request_priority(0, 0), 0);
    assert_eq!(tile_request_priority(2, 7), 0);
}

#[test]
fn tile_visit_order_prioritizes_primary_visible_before_lookahead() {
    let primary = vec![tile_visit(3, 3), tile_visit(4, 3)];
    let padded = vec![
        tile_visit(2, 3),
        tile_visit(3, 3),
        tile_visit(4, 3),
        tile_visit(5, 3),
    ];

    let mut ordered = Vec::new();
    let mut coords_scratch = HashSet::new();
    super::prioritize_tile_visits_into(&mut ordered, &mut coords_scratch, &primary, &padded);
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
fn tile_visit_order_prioritizes_primary_for_all_backends() {
    let primary = vec![tile_visit(3, 3), tile_visit(4, 3)];
    let padded = vec![
        tile_visit(2, 3),
        tile_visit(3, 3),
        tile_visit(4, 3),
        tile_visit(5, 3),
    ];
    let expected = vec![
        TileCoord { col: 3, row: 3 },
        TileCoord { col: 4, row: 3 },
        TileCoord { col: 2, row: 3 },
        TileCoord { col: 5, row: 3 },
    ];

    for backend in [PlaneBackendKind::Sdr, PlaneBackendKind::Hdr] {
        let ordered = tile_visits_for_backend(backend, &primary, &padded);
        assert_eq!(
            ordered
                .iter()
                .map(|(coord, _, _)| *coord)
                .collect::<Vec<_>>(),
            expected
        );
    }
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
fn tiled_preview_base_plane_is_selected_by_backend() {
    assert!(should_draw_tiled_preview_for_backend(
        PlaneBackendKind::Sdr,
        TiledPlaneKind::Sdr
    ));
    assert!(!should_draw_tiled_preview_for_backend(
        PlaneBackendKind::Hdr,
        TiledPlaneKind::Sdr
    ));
    assert!(should_draw_tiled_preview_for_backend(
        PlaneBackendKind::Hdr,
        TiledPlaneKind::Hdr
    ));
    assert!(!should_draw_tiled_preview_for_backend(
        PlaneBackendKind::Sdr,
        TiledPlaneKind::Hdr
    ));
}

#[test]
fn tiled_tile_plane_drawing_requires_matching_backend_and_ready_tile() {
    assert!(!should_draw_tiled_tile_plane_for_backend(
        PlaneBackendKind::Hdr,
        TiledPlaneKind::Hdr,
        false
    ));
    assert!(should_draw_tiled_tile_plane_for_backend(
        PlaneBackendKind::Hdr,
        TiledPlaneKind::Hdr,
        true
    ));
    assert!(!should_draw_tiled_tile_plane_for_backend(
        PlaneBackendKind::Sdr,
        TiledPlaneKind::Hdr,
        true
    ));
    assert!(should_draw_tiled_tile_plane_for_backend(
        PlaneBackendKind::Sdr,
        TiledPlaneKind::Sdr,
        true
    ));
}

#[test]
fn tiled_plane_threshold_matches_preview_quality_policy() {
    assert_eq!(tiled_plane_threshold(0.5, 0.25, 512), 0.6);
    assert_eq!(tiled_plane_threshold(0.05, 0.25, 512), 0.2625);
    assert!(!is_tiled_plane_active(0.59, 0.6));
    assert!(is_tiled_plane_active(0.6, 0.6));
}

#[test]
fn hdr_tile_plane_threshold_waits_until_tiles_are_visually_meaningful() {
    let sdr_threshold =
        tiled_plane_threshold_for_backend(PlaneBackendKind::Sdr, 4096.0 / 24576.0, 0.05, 512);
    let hdr_threshold =
        tiled_plane_threshold_for_backend(PlaneBackendKind::Hdr, 4096.0 / 24576.0, 0.05, 512);

    assert!(sdr_threshold < 0.25);
    assert_eq!(hdr_threshold, 0.375);
    assert!(!is_tiled_plane_active(0.25, hdr_threshold));
    assert!(is_tiled_plane_active(0.375, hdr_threshold));
}

fn tile_visit(col: u32, row: u32) -> (TileCoord, Rect, Rect) {
    (
        TileCoord { col, row },
        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
    )
}

#[test]
fn tiled_preview_fallback_is_active_when_hdr_preview_not_ready() {
    let plane_backend = PlaneBackendKind::Hdr;

    // Under Hdr backend:
    // If hdr_preview_drawn is true, we should NOT draw the SDR preview fallback
    let should_draw_sdr_fallback_when_hdr_drawn =
        should_draw_tiled_preview_for_backend(plane_backend, TiledPlaneKind::Sdr)
            || (plane_backend == PlaneBackendKind::Hdr && !true);

    // If hdr_preview_drawn is false, we DO want to draw the SDR preview fallback
    let should_draw_sdr_fallback_when_hdr_not_drawn =
        should_draw_tiled_preview_for_backend(plane_backend, TiledPlaneKind::Sdr)
            || (plane_backend == PlaneBackendKind::Hdr && !false);

    assert!(!should_draw_sdr_fallback_when_hdr_drawn);
    assert!(should_draw_sdr_fallback_when_hdr_not_drawn);

    // Under Sdr backend:
    // We should always draw the SDR preview regardless
    let plane_backend_sdr = PlaneBackendKind::Sdr;
    let should_draw_sdr_under_sdr =
        should_draw_tiled_preview_for_backend(plane_backend_sdr, TiledPlaneKind::Sdr)
            || (plane_backend_sdr == PlaneBackendKind::Hdr && !true);

    assert!(should_draw_sdr_under_sdr);
}
