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

use crate::settings::TransitionStyle;

mod draw;
mod helpers;

pub(crate) const FALLBACK_PREVIEW_SCALE: f32 = 0.1;
pub(super) const PREVIEW_QUALITY_THRESHOLD: f32 = 1.2;
pub(super) const FIT_SCALE_BUFFER: f32 = 1.05;
pub(super) const HDR_TILE_MIN_SCREEN_PX: f32 = 192.0;
pub(crate) const BURST_UPLOAD_MULT: usize = 4;
/// Hard per-frame upload cap for 512px tiles (each tile = 1MB RGBA).
/// 16 × 1MB = 16MB per frame — safe for all GPU tiers.
pub(crate) const BURST_UPLOAD_MAX_512: usize = 16;

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

