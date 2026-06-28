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

use crate::app::TransitionStyle;
use crate::app::rendering::plan::RenderPlan;
use crate::app::rendering::plane::PlaneBackendKind;

mod draw;
mod hdr_draw;
mod helpers;
mod transitions;

pub(crate) use self::hdr_draw::{HdrImagePlaneClippedDraw, HdrRectangularTransitionDraw, PrevImageUnderneathParams};
pub(crate) use self::transitions::{
    OutgoingFrameClippedParams, OutgoingFrameRippleParams, PageFlipTransitionDraw,
};

#[cfg(test)]
mod tests;

pub(crate) fn should_route_through_hdr_plane(plan: &RenderPlan) -> bool {
    plan.backend == PlaneBackendKind::Hdr
}

pub(crate) fn should_draw_static_hdr_immediately(
    plan: &RenderPlan,
    _transition: TransitionStyle,
    is_animating: bool,
) -> bool {
    if plan.backend != PlaneBackendKind::Hdr {
        return false;
    }

    if !is_animating {
        return true;
    }

    // During animation, keep standard and complex transitions on their transition paths.
    false
}

pub(crate) fn should_dispatch_standard_draw(
    has_sdr_texture: bool,
    has_current_hdr_image: bool,
    sdr_fallback_is_placeholder: bool,
) -> bool {
    has_current_hdr_image || (has_sdr_texture && !sdr_fallback_is_placeholder)
}

/// Hold the outgoing frame while the navigation target is still decoding (transition style
/// `None`). Requires a saved previous texture and/or HDR buffer.
pub(crate) fn should_draw_pending_navigation_hold_frame(
    transition_start: Option<std::time::Instant>,
    pending_transition_target: Option<usize>,
    current_index: usize,
    has_prev_frame: bool,
) -> bool {
    transition_start.is_none() && pending_transition_target == Some(current_index) && has_prev_frame
}
