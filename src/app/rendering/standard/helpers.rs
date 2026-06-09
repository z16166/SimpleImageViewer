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

use crate::app::rendering::transitions::TransitionParams;
use eframe::egui::{Pos2, Rect, Vec2};

pub(super) fn should_clear_transition_state_after_static_hdr_draw(
    static_hdr_draw: bool,
    pending_transition_target: Option<usize>,
    current_index: usize,
) -> bool {
    static_hdr_draw && pending_transition_target != Some(current_index)
}

pub(super) fn pending_navigation_hold_params() -> TransitionParams {
    TransitionParams {
        prev_alpha: 1.0,
        ..TransitionParams::default()
    }
}

pub(super) fn resolve_transition_prev_layout(
    screen_rect: Rect,
    final_dest: Rect,
    prev_size: Option<Vec2>,
    captured_prev_dest: Option<Rect>,
    has_prev: bool,
    compute_display_rect: impl FnOnce(Vec2, Rect) -> Rect,
) -> (Rect, Rect, bool) {
    let p_dest = captured_prev_dest
        .or_else(|| prev_size.map(|size| compute_display_rect(size, screen_rect)))
        .unwrap_or(final_dest);
    let union_rect = if has_prev {
        p_dest.union(final_dest)
    } else {
        final_dest
    };
    (p_dest, union_rect, has_prev)
}

pub(super) fn curtain_hdr_transition_rotation(rotation: i32) -> i32 {
    rotation
}
