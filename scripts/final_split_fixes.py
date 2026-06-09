#!/usr/bin/env python3
"""Targeted fixes after split_modules_v2 + apply_split_fixes."""
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def prepend(path: Path, block: str) -> None:
    m = "// along with this program.  If not, see <https://www.gnu.org/licenses/>.\n\n"
    t = path.read_text(encoding="utf-8")
    if block.strip() not in t:
        path.write_text(t.replace(m, m + block, 1), encoding="utf-8")


def main() -> None:
    # tiled mod re-exports
    mod = ROOT / "src/hdr/tiled/mod.rs"
    mod.write_text(
        mod.read_text(encoding="utf-8")
        .replace("pub use ", "pub(crate) use ")
        .replace(
            "pub(crate) use kind::HdrTiledSourceKind;",
            "pub(crate) use kind::{HdrTiledSource, HdrTiledSourceKind};",
        )
        .replace(
            "pub(crate) use source::{HdrTiledImageSource, HdrTiledSource};",
            "pub(crate) use source::HdrTiledImageSource;",
        )
        .replace(
            "pub(crate) use validate::validate_tile_bounds;",
            "pub(crate) use validate::{validate_rgba32f_len, validate_tile_bounds};",
        ),
        encoding="utf-8",
    )

    # globals static init
    g = ROOT / "src/hdr/tiled/globals.rs"
    g.write_text(
        g.read_text(encoding="utf-8").replace(
            "AtomicUsize::new(initial_hdr_tile_cache_max_bytes())",
            "AtomicUsize::new(DEFAULT_HDR_TILE_CACHE_MAX_BYTES)",
        ),
        encoding="utf-8",
    )

    # validate rgba visibility
    v = ROOT / "src/hdr/tiled/validate.rs"
    v.write_text(
        v.read_text(encoding="utf-8").replace(
            "fn validate_rgba32f_len", "pub(crate) fn validate_rgba32f_len", 1
        ),
        encoding="utf-8",
    )

    # decode cross-module
    for name, reps in [
        ("decode_image.rs", [
            ("is_exr_path(path)", "super::paths::is_exr_path(path)"),
            ("is_radiance_hdr_path(path)", "super::paths::is_radiance_hdr_path(path)"),
            ("return decode_exr_display_image", "return super::exr::decode_exr_display_image"),
            ("return decode_radiance_hdr_image", "return super::radiance::decode_radiance_hdr_image"),
            ("validate_hdr_fallback_budget(", "super::tone_map::validate_hdr_fallback_budget("),
            ("MAX_HDR_FALLBACK_DECODE_BYTES", "super::constants::MAX_HDR_FALLBACK_DECODE_BYTES"),
        ]),
        ("exr.rs", [("validate_hdr_fallback_budget(", "super::tone_map::validate_hdr_fallback_budget(")]),
        ("radiance.rs", [
            ("fn decode_radiance_hdr_image", "pub(crate) fn decode_radiance_hdr_image"),
            ("validate_hdr_fallback_budget(", "super::tone_map::validate_hdr_fallback_budget("),
        ]),
    ]:
        p = ROOT / "src/hdr/decode" / name
        t = p.read_text(encoding="utf-8")
        for a, b in reps:
            t = t.replace(a, b)
        p.write_text(t, encoding="utf-8")

    c = ROOT / "src/hdr/decode/constants.rs"
    for name in [
        "HDR_RGBA32F_BYTES_PER_PIXEL", "SDR_RGBA8_BYTES_PER_PIXEL",
        "HDR_FALLBACK_BYTES_PER_PIXEL_WITH_SDR", "MAX_HDR_FALLBACK_PIXELS",
        "MAX_HDR_FALLBACK_DECODE_BYTES", "MAX_HDR_FALLBACK_TOTAL_BYTES",
        "MAX_HDR_TONE_MAP_INPUT", "INVERSE_DISPLAY_GAMMA",
    ]:
        c.write_text(c.read_text(encoding="utf-8").replace(f"const {name}", f"pub(crate) const {name}"), encoding="utf-8")

    tone = ROOT / "src/hdr/decode/tone_map.rs"
    if "use super::constants::" not in tone.read_text(encoding="utf-8"):
        prepend(
            tone,
            "use super::constants::{\n"
            "    HDR_FALLBACK_BYTES_PER_PIXEL_WITH_SDR, INVERSE_DISPLAY_GAMMA, MAX_HDR_FALLBACK_PIXELS,\n"
            "    MAX_HDR_FALLBACK_TOTAL_BYTES, MAX_HDR_TONE_MAP_INPUT,\n"
            "};\n\n",
        )

    # monitor
    eff = ROOT / "src/hdr/monitor/effective.rs"
    t = eff.read_text(encoding="utf-8")
    t = t.replace(
        "use super::macos::macos_active_monitor_hdr_status;",
        "#[cfg(target_os = \"macos\")]\nuse super::macos::macos_active_monitor_hdr_status;",
    )
    eff.write_text(t, encoding="utf-8")
    mac = ROOT / "src/hdr/monitor/macos.rs"
    prepend(mac, "use super::types::HdrMonitorSelection;\n\n")
    mac.write_text(
        mac.read_text(encoding="utf-8").replace(
            "fn macos_active_monitor_hdr_status", "pub(crate) fn macos_active_monitor_hdr_status", 1
        ),
        encoding="utf-8",
    )
    win = ROOT / "src/hdr/monitor/windows.rs"
    win.write_text(
        win.read_text(encoding="utf-8")
        .replace("fn windows_active_monitor_hdr_status", "pub(crate) fn windows_active_monitor_hdr_status", 1)
        .replace("fn finite_positive_luminance", "pub(crate) fn finite_positive_luminance", 1)
        .replace("fn dxgi_output_hdr_active", "pub(crate) fn dxgi_output_hdr_active", 1),
        encoding="utf-8",
    )
    prepend(
        ROOT / "src/hdr/monitor/probe.rs",
        "use super::types::HdrMonitorSelection;\n"
        "use super::windows::{dxgi_output_hdr_active, finite_positive_luminance};\n\n",
    )

    # wic load
    load = ROOT / "src/wic/load.rs"
    if "use windows::core::*" not in load.read_text(encoding="utf-8"):
        prepend(load, "use super::com::ComGuard;\nuse super::imports::*;\nuse windows::core::*;\n\n")

    # raw assemble path
    for name in ["preview.rs", "develop.rs", "load.rs"]:
        p = ROOT / "src/loader/decode/raw" / name
        p.write_text(
            p.read_text(encoding="utf-8").replace(
                "use super::assemble::", "use crate::loader::decode::assemble::"
            ),
            encoding="utf-8",
        )

    # radiance cross-module
    for fn in ["build_radiance_scanline_offsets", "read_radiance_header", "decode_radiance_rgba32f_from_mmap", "validate_scanline_offsets"]:
        h = ROOT / "src/hdr/radiance_tiled/header.rs"
        h.write_text(h.read_text(encoding="utf-8").replace(f"fn {fn}", f"pub(crate) fn {fn}", 1), encoding="utf-8")
    layout = ROOT / "src/hdr/radiance_tiled/layout.rs"
    layout.write_text(layout.read_text(encoding="utf-8").replace("struct Rgbe8Pixel", "pub(crate) struct Rgbe8Pixel", 1), encoding="utf-8")
    prepend(
        ROOT / "src/hdr/radiance_tiled/source.rs",
        "use super::header::{build_radiance_scanline_offsets, read_radiance_header};\n"
        "use super::layout::RadianceRasterLayout;\n"
        "use super::tile_decode::{decode_radiance_hdr_preview, decode_radiance_sdr_preview, decode_radiance_tile_window};\n\n",
    )
    for fn in ["decode_radiance_tile_window", "decode_radiance_sdr_preview", "decode_radiance_hdr_preview"]:
        td = ROOT / "src/hdr/radiance_tiled/tile_decode.rs"
        td.write_text(td.read_text(encoding="utf-8").replace(f"fn {fn}", f"pub(crate) fn {fn}", 1), encoding="utf-8")
    prepend(ROOT / "src/hdr/radiance_tiled/rle.rs", "use super::layout::Rgbe8Pixel;\n\n")

    # types import fix in split modules
    for d in ["src/hdr/decode", "src/hdr/tiled", "src/hdr/radiance_tiled"]:
        for p in (ROOT / d).rglob("*.rs"):
            t = p.read_text(encoding="utf-8").replace("use super::types::", "use crate::hdr::types::")
            p.write_text(t, encoding="utf-8")

    print("final fixes applied")


if __name__ == "__main__":
    main()
