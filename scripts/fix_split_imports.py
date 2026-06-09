from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def fix_standard_draw() -> None:
    p = ROOT / "src/app/rendering/standard/draw.rs"
    t = p.read_text(encoding="utf-8")
    if "use crate::hdr::renderer::HdrRenderOutputMode;" not in t:
        t = t.replace(
            "use crate::app::{ImageViewerApp, TransitionStyle};",
            "use crate::app::{ImageViewerApp, TransitionStyle};\n"
            "use crate::hdr::renderer::HdrRenderOutputMode;",
        )
        p.write_text(t, encoding="utf-8")


def fix_standard_transitions() -> None:
    p = ROOT / "src/app/rendering/standard/transitions.rs"
    t = p.read_text(encoding="utf-8")
    needle = "use eframe::egui::{self, Color32, Pos2, Rect, Vec2};"
    repl = (
        needle
        + "\nuse crate::hdr::renderer::HdrRenderOutputMode;\nuse std::sync::Arc;"
    )
    if "HdrRenderOutputMode" not in t:
        t = t.replace(needle, repl)
        p.write_text(t, encoding="utf-8")


def fix_tiled_mod() -> None:
    p = ROOT / "src/app/rendering/tiled/mod.rs"
    t = p.read_text(encoding="utf-8")
    for name in ("PREVIEW_QUALITY_THRESHOLD", "FIT_SCALE_BUFFER", "HDR_TILE_MIN_SCREEN_PX"):
        t = t.replace(f"const {name}", f"pub(super) const {name}")
    p.write_text(t, encoding="utf-8")


def fix_tiled_helpers() -> None:
    p = ROOT / "src/app/rendering/tiled/helpers.rs"
    t = p.read_text(encoding="utf-8")
    extra = (
        "use super::{FIT_SCALE_BUFFER, HDR_TILE_MIN_SCREEN_PX, PREVIEW_QUALITY_THRESHOLD};\n"
        "use crate::app::rendering::geometry::PlaneLayout;\n"
        "use crate::app::rendering::plane::{PlaneDrawSource, draw_plane};\n\n"
    )
    if "PlaneLayout" not in t.split("\n", 20)[0:20]:
        t = t.replace("use std::sync::Arc;\n\n", f"use std::sync::Arc;\n\n{extra}")
    t = t.replace(
        '#[cfg(feature = "tile-debug")]\nfn draw_tile_debug_border',
        '#[cfg(feature = "tile-debug")]\npub(crate) fn draw_tile_debug_border',
    )
    p.write_text(t, encoding="utf-8")


def fix_tiled_draw() -> None:
    p = ROOT / "src/app/rendering/tiled/draw.rs"
    t = p.read_text(encoding="utf-8")
    t = t.replace(
        "use eframe::egui::{self, Color32, Rect, Vec2};",
        "use eframe::egui::{self, Color32, Pos2, Rect, Vec2};",
    )
    t = t.replace(
        "draw_hdr_plane_tile_visit, draw_tile_debug_border,",
        "draw_hdr_plane_tile_visit,",
    )
    marker = "use std::sync::Arc;\n\nimpl ImageViewerApp"
    if 'feature = "tile-debug"' not in t:
        t = t.replace(
            marker,
            'use std::sync::Arc;\n\n#[cfg(feature = "tile-debug")]\n'
            "use super::helpers::draw_tile_debug_border;\n\nimpl ImageViewerApp",
        )
    p.write_text(t, encoding="utf-8")


def main() -> None:
    fix_standard_draw()
    fix_standard_transitions()
    fix_tiled_mod()
    fix_tiled_helpers()
    fix_tiled_draw()
    print("fixed split imports")


if __name__ == "__main__":
    main()
