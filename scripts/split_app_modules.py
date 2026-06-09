#!/usr/bin/env python3
"""Split large app modules into subdirectories."""
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SRC = ROOT / "src" / "app"

COPYRIGHT = """// Simple Image Viewer - A high-performance, cross-platform image viewer
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

"""


def read_lines(path: Path) -> list[str]:
    return path.read_text(encoding="utf-8").splitlines(keepends=True)


def write(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")


def slice_lines(lines: list[str], start: int, end: int) -> str:
    return "".join(lines[start - 1 : end])


def split_standard() -> None:
    src = SRC / "rendering" / "standard.rs"
    lines = read_lines(src)
    out = SRC / "rendering" / "standard"

    mod_rs = COPYRIGHT + """use crate::app::rendering::plan::RenderPlan;
use crate::app::rendering::plane::PlaneBackendKind;
use crate::app::TransitionStyle;

mod draw;
mod hdr_draw;
mod helpers;
mod transitions;

#[cfg(test)]
mod tests;

"""
    mod_rs += slice_lines(lines, 41, 87)
    write(out / "mod.rs", mod_rs)

    draw_uses = """use super::helpers::{
    pending_navigation_hold_params, should_clear_transition_state_after_static_hdr_draw,
};
use super::{should_draw_static_hdr_immediately, should_route_through_hdr_plane};
use crate::app::rendering::geometry::PlaneLayout;
use crate::app::rendering::plan::{RenderPlan, RenderShape};
use crate::app::rendering::plane::{PlaneBackendKind, draw_sdr_texture_plane, hdr_image_plane_rect};
use crate::app::{ImageViewerApp, TransitionStyle};
use eframe::egui::{self, Color32, Pos2, Rect, Vec2};
use std::sync::Arc;
use std::time::Instant;

"""
    write(out / "draw.rs", COPYRIGHT + draw_uses + slice_lines(lines, 89, 461) + "}\n")

    hdr_uses = """use super::helpers::curtain_hdr_transition_rotation;
use crate::app::rendering::geometry::PlaneLayout;
use crate::app::rendering::plane::{PlaneDrawSource, draw_plane};
use crate::app::rendering::transitions::TransitionParams;
use crate::app::{ImageViewerApp, TransitionStyle};
use crate::hdr::renderer::HdrRenderOutputMode;
use crate::hdr::types::{HdrImageBuffer, HdrToneMapSettings};
use eframe::egui::{self, Color32, Pos2, Rect, Vec2};
use std::sync::Arc;

impl ImageViewerApp {
"""
    hdr_body = slice_lines(lines, 470, 845)
    write(out / "hdr_draw.rs", COPYRIGHT + hdr_uses + hdr_body + "}\n")

    trans_uses = """use super::helpers::resolve_transition_prev_layout;
use crate::app::rendering::transitions;
use crate::app::{ImageViewerApp, TransitionStyle};
use eframe::egui::{self, Color32, Pos2, Rect, Vec2};

impl ImageViewerApp {
"""
    trans_body = slice_lines(lines, 847, 1244)
    write(out / "transitions.rs", COPYRIGHT + trans_uses + trans_body + "}\n")

    helpers_body = """use crate::app::rendering::transitions::TransitionParams;
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
"""
    write(out / "helpers.rs", COPYRIGHT + helpers_body)

    tests_body = slice_lines(lines, 1278, len(lines))
    tests_body = tests_body.replace("#[cfg(test)]\nmod tests {\n", "", 1).rstrip()
    if tests_body.endswith("}"):
        tests_body = tests_body[:-1].rstrip()
    tests_body = tests_body.replace("    use super::*;\n", "", 1)
    write(
        out / "tests.rs",
        COPYRIGHT
        + "#[cfg(test)]\nmod tests {\n"
        + "    use super::helpers::{\n"
        + "        curtain_hdr_transition_rotation, pending_navigation_hold_params,\n"
        + "        resolve_transition_prev_layout, should_clear_transition_state_after_static_hdr_draw,\n"
        + "    };\n"
        + "    use super::{\n"
        + "        should_dispatch_standard_draw, should_draw_pending_navigation_hold_frame,\n"
        + "        should_draw_static_hdr_immediately, should_route_through_hdr_plane,\n"
        + "    };\n"
        + tests_body,
    )

    src.unlink()


def split_input() -> None:
    src = SRC / "input.rs"
    lines = read_lines(src)
    out = SRC / "input"

    mod_rs = """use crate::hotkeys::model::{HotkeyActionId, HotkeyLogicalKey};

mod actions;
mod keyboard;
mod pointer;
mod ui;
mod wheel;

#[cfg(test)]
mod tests;

pub(crate) use actions::AppAction;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AutoSwitchStep {
    Stop,
    NavigateTo(usize),
    ShuffleToFirst,
}

pub(crate) fn auto_switch_step(
    image_count: usize,
    current_index: usize,
    random_order: bool,
    random_order_ready: bool,
) -> AutoSwitchStep {
    if image_count <= 1 {
        return AutoSwitchStep::Stop;
    }
    if random_order && !random_order_ready {
        return AutoSwitchStep::ShuffleToFirst;
    }

    let last = image_count - 1;
    if current_index >= last {
        if random_order {
            return AutoSwitchStep::ShuffleToFirst;
        }
    }

    AutoSwitchStep::NavigateTo((current_index + 1) % image_count)
}

#[cfg(test)]
struct HotkeyBinding {
    modifiers: u8,
    key: egui::Key,
}

#[cfg(test)]
const M_NONE: u8 = 0;
const M_CTRL: u8 = 1;
const M_SHIFT: u8 = 2;
const M_ALT: u8 = 4;

pub(super) fn get_modifiers_mask(m: egui::Modifiers) -> u8 {
    let mut mask = 0;
    if m.ctrl || m.command {
        mask |= M_CTRL;
    }
    if m.shift {
        mask |= M_SHIFT;
    }
    if m.alt {
        mask |= M_ALT;
    }
    mask
}

fn app_action_from_hotkey_action_id(action: HotkeyActionId) -> AppAction {
"""
    mod_rs += slice_lines(lines, 924, 950)
    mod_rs += "}\n\npub(super) fn text_event_to_hotkey_logical_key(text: &str) -> Option<HotkeyLogicalKey> {\n"
    mod_rs += slice_lines(lines, 953, 955)
    mod_rs += "}\n"
    write(out / "mod.rs", mod_rs)

    write(
        out / "keyboard.rs",
        "use super::{AppAction, app_action_from_hotkey_action_id, get_modifiers_mask};\n"
        + "use crate::app::ImageViewerApp;\n"
        + "use crate::hotkeys::model::{HotkeyLogicalKey, KeyChord};\n"
        + "use eframe::egui::{self, Context, Key};\n\n"
        + slice_lines(lines, 47, 159)
        + "}\n",
    )

    write(
        out / "wheel.rs",
        "use super::{AppAction, app_action_from_hotkey_action_id};\n"
        + "use crate::app::ImageViewerApp;\n"
        + "use crate::hotkeys::model::{HotkeyActionId, KeyChord};\n"
        + "use eframe::egui::{self, Context, MouseWheelUnit, Vec2};\n\n"
        + "struct WheelHotkeyMatch {\n"
        + "    action: AppAction,\n"
        + "    normalized_delta_y: f32,\n"
        + "}\n\n"
        + "impl ImageViewerApp {\n"
        + slice_lines(lines, 113, 290)
        + "}\n",
    )

    write(
        out / "pointer.rs",
        "use super::{AppAction, app_action_from_hotkey_action_id};\n"
        + "use crate::app::ImageViewerApp;\n"
        + "use crate::hotkeys::model::KeyChord;\n"
        + "use eframe::egui::{self, Context, Event};\n\n"
        + "impl ImageViewerApp {\n"
        + slice_lines(lines, 161, 182)
        + "}\n",
    )

    write(
        out / "actions.rs",
        "use super::{AutoSwitchStep, auto_switch_step};\n"
        + "use crate::app::ImageViewerApp;\n"
        + "use crate::constants::KEYBOARD_NAV_MIN_INTERVAL_SECS;\n"
        + "use crate::ui::dialogs::modal_state::ActiveModal;\n"
        + "use eframe::egui::{self, Context, Vec2};\n"
        + "use std::time::{Duration, Instant};\n\n"
        + "#[derive(Debug, Clone, Copy, PartialEq)]\n"
        + "pub(crate) enum AppAction {\n"
        + slice_lines(lines, 752, 776)
        + "\nimpl ImageViewerApp {\n"
        + slice_lines(lines, 292, 441)
        + "}\n",
    )

    write(
        out / "ui.rs",
        "use super::AppAction;\n"
        + "use crate::app::ImageViewerApp;\n"
        + "use crate::ui::dialogs::modal_state::{ActiveModal, ModalResult};\n"
        + "use crate::ui::utils::copy_file_to_clipboard;\n"
        + "use crate::ui::{hud as ui_hud, settings as ui_settings};\n"
        + "use eframe::egui::{self, Context};\n"
        + "use rust_i18n::t;\n\n"
        + "impl ImageViewerApp {\n"
        + slice_lines(lines, 447, 747)
        + "}\n",
    )

    write(out / "tests.rs", slice_lines(lines, 807, 922) + slice_lines(lines, 957, len(lines)))

    src.unlink()


TILED_PUB_ITEMS = [
    "should_draw_tiled_preview_transition_for_backend",
    "effective_hdr_tiled_alphas",
    "prev_transition_params_for_tiled_draw",
    "draw_tile_debug_border",
    "TileRequestBudget",
    "TiledPlaneKind",
    "hdr_tile_cache_key_for_coord",
    "tile_visits_for_backend",
    "tile_request_priority",
    "tiled_lookahead_padding",
    "should_invalidate_tile_requests_on_pan_drag",
    "tile_plane_kind_for_backend",
    "should_draw_tiled_preview_for_backend",
    "should_repaint_for_ready_tiles_for_backend",
    "has_pending_visible_tiles_for_backend",
    "tiled_plane_threshold_for_backend",
    "is_tiled_plane_active",
    "enqueue_hdr_plane_tile_decode",
    "draw_hdr_plane_tile_visit",
    "tile_pending_key_for_backend",
    "tile_decode_source_for_backend",
]


def split_tiled() -> None:
    src = SRC / "rendering" / "tiled.rs"
    lines = read_lines(src)
    out = SRC / "rendering" / "tiled"

    mod_rs = (
        COPYRIGHT
        + "mod draw;\nmod helpers;\n\n#[cfg(test)]\nmod tests;\n\n"
        + slice_lines(lines, 31, 51)
        + "\n"
    )
    write(out / "mod.rs", mod_rs.replace("const FALLBACK", "pub(crate) const FALLBACK").replace(
        "const BURST_UPLOAD_MULT", "pub(crate) const BURST_UPLOAD_MULT"
    ).replace("const BURST_UPLOAD_MAX_512", "pub(crate) const BURST_UPLOAD_MAX_512"))

    helpers = (
        "use super::should_draw_tiled_preview_transition;\n"
        + "use crate::app::rendering::plan::RenderPlan;\n"
        + "use crate::app::rendering::plane::PlaneBackendKind;\n"
        + "use crate::app::rendering::transitions::TransitionParams;\n"
        + "use crate::app::TransitionStyle;\n"
        + "use crate::hdr::tiled::HdrTiledSource;\n"
        + "use crate::loader::{TileDecodeSource, TilePixelKind};\n"
        + "use crate::tile_cache::{PendingTileKey, TileCoord, TileManager, TileStatus};\n"
        + "use eframe::egui::{self, Color32, Pos2, Rect, Vec2};\n"
        + "use std::collections::HashSet;\n"
        + "use std::sync::Arc;\n\n"
        + slice_lines(lines, 53, 566)
    )
    for name in TILED_PUB_ITEMS:
        helpers = helpers.replace(f"fn {name}", f"pub(crate) fn {name}")
        helpers = helpers.replace(f"struct {name}", f"pub(crate) struct {name}")
        helpers = helpers.replace(f"enum {name}", f"pub(crate) enum {name}")
    write(out / "helpers.rs", helpers)

    draw_uses = """use super::helpers::{
    TileRequestBudget, TiledPlaneKind, draw_hdr_plane_tile_visit, draw_tile_debug_border,
    effective_hdr_tiled_alphas, has_pending_visible_tiles_for_backend,
    hdr_tile_cache_key_for_coord, is_tiled_plane_active, prev_transition_params_for_tiled_draw,
    should_draw_tiled_preview_for_backend, should_draw_tiled_preview_transition_for_backend,
    should_invalidate_tile_requests_on_pan_drag, should_repaint_for_ready_tiles_for_backend,
    tile_decode_source_for_backend, tile_pending_key_for_backend, tile_plane_kind_for_backend,
    tile_request_priority, tile_visits_for_backend, tiled_lookahead_padding,
    tiled_plane_threshold_for_backend,
};
use super::{BURST_UPLOAD_MAX_512, BURST_UPLOAD_MULT, FALLBACK_PREVIEW_SCALE};
use crate::app::rendering::geometry::PlaneLayout;
use crate::app::rendering::plan::{RenderPlan, RenderShape};
use crate::app::rendering::plane::{
    PlaneBackendKind, PlaneDrawSource, draw_plane, draw_sdr_texture_plane, hdr_image_plane_rect,
};
use crate::app::{ImageViewerApp, TransitionStyle};
use crate::tile_cache::{TileCoord, TileStatus};
use eframe::egui::{self, Color32, Rect, Vec2};
use std::collections::HashSet;
use std::sync::Arc;

"""
    write(out / "draw.rs", COPYRIGHT + draw_uses + slice_lines(lines, 568, 1032))

    src.unlink()


def split_loader_results() -> None:
    src = SRC / "image_management" / "loader_results.rs"
    lines = read_lines(src)
    out = SRC / "image_management" / "loader_results"

    write(
        out / "mod.rs",
        COPYRIGHT + "use super::*;\n\nmod display;\nmod file_ops;\nmod install;\nmod process;\n",
    )
    write(out / "file_ops.rs", slice_lines(lines, 1, 95) + "}\n")
    write(
        out / "process.rs",
        slice_lines(lines, 1, 18) + "impl ImageViewerApp {\n" + slice_lines(lines, 97, 675) + "}\n",
    )
    write(
        out / "display.rs",
        slice_lines(lines, 1, 18)
        + "impl ImageViewerApp {\n"
        + slice_lines(lines, 677, 725)
        + "}\n",
    )
    body = slice_lines(lines, 727, len(lines)).rstrip()
    if body.endswith("}"):
        body = body[:-1].rstrip()
    write(
        out / "install.rs",
        slice_lines(lines, 1, 18)
        + "impl ImageViewerApp {\n"
        + body
        + "\n}\n",
    )
    src.unlink()


def main() -> None:
    split_standard()
    split_input()
    split_tiled()
    split_loader_results()
    print("Split complete.")


if __name__ == "__main__":
    main()
