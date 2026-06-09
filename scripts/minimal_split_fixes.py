#!/usr/bin/env python3
"""Minimal fixes after split + apply_split_fixes."""
from __future__ import annotations

import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def git_lines(rel: str) -> list[str]:
    return subprocess.check_output(
        ["git", "show", f"HEAD:{rel}"], cwd=ROOT, text=True, encoding="utf-8"
    ).splitlines(keepends=True)


def main() -> None:
    # wic load
    lines = git_lines("src/wic.rs")
    (ROOT / "src/wic/load.rs").write_text(
        "".join(lines[:16])
        + "use super::com::ComGuard;\nuse super::imports::*;\nuse windows::core::*;\n\n"
        + "".join(lines[593:]),
        encoding="utf-8",
    )

    # raw modules (avoid broken import injection)
    lines = git_lines("src/loader/decode/raw.rs")
    imports = "".join(lines[16:42]).replace(
        "use super::assemble", "use crate::loader::decode::assemble"
    )
    test_i = next(
        i
        for i, l in enumerate(lines)
        if l.startswith("#[cfg(test)]") and i + 1 < len(lines) and "mod tests" in lines[i + 1]
    )
    for name, start, end in [("preview.rs", 43, 124), ("develop.rs", 126, 284), ("load.rs", 285, test_i)]:
        (ROOT / "src/loader/decode/raw" / name).write_text(
            "".join(lines[:16]) + imports + "".join(lines[start - 1 : end]),
            encoding="utf-8",
        )

    # jpegxl decode/metadata imports
    lines = git_lines("src/hdr/jpegxl.rs")
    jxl_imports = "".join(lines[16:35])
    test_i = next(
        i
        for i, l in enumerate(lines)
        if l.startswith("#[cfg(test)]") and i + 1 < len(lines) and "mod tests" in lines[i + 1]
    )
    (ROOT / "src/hdr/jpegxl/decode.rs").write_text(
        "".join(lines[:16])
        + "use super::probe::is_jxl_header;\n\n"
        + jxl_imports
        + "".join(lines[198:1218]),
        encoding="utf-8",
    )
    (ROOT / "src/hdr/jpegxl/metadata.rs").write_text(
        "".join(lines[:16]) + jxl_imports + "".join(lines[1219:test_i]),
        encoding="utf-8",
    )
    (ROOT / "src/hdr/jpegxl/mod.rs").write_text(
        (ROOT / "src/hdr/jpegxl/mod.rs")
        .read_text(encoding="utf-8")
        .replace(
            "pub(crate) use metadata::jxl_color_encoding_to_metadata;\n",
            "",
        )
        .replace(
            "pub(crate) use probe::{is_jxl_header, libjxl_probe_orientation_from_bytes, libjxl_probe_orientation_from_path};",
            "pub(crate) use probe::{is_jxl_header, jxl_color_encoding_to_metadata, libjxl_probe_orientation_from_bytes, libjxl_probe_orientation_from_path};",
        ),
        encoding="utf-8",
    )

    # types imports in split hdr modules
    for d in ["src/hdr/decode", "src/hdr/tiled", "src/hdr/radiance_tiled"]:
        for p in (ROOT / d).rglob("*.rs"):
            t = p.read_text(encoding="utf-8").replace("use super::types::", "use crate::hdr::types::")
            p.write_text(t, encoding="utf-8")

    # tiled mod + validate + globals
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
    v = ROOT / "src/hdr/tiled/validate.rs"
    v.write_text(
        v.read_text(encoding="utf-8").replace("fn validate_rgba32f_len", "pub(crate) fn validate_rgba32f_len", 1),
        encoding="utf-8",
    )
    g = ROOT / "src/hdr/tiled/globals.rs"
    g.write_text(
        g.read_text(encoding="utf-8").replace(
            "AtomicUsize::new(initial_hdr_tile_cache_max_bytes())",
            "AtomicUsize::new(DEFAULT_HDR_TILE_CACHE_MAX_BYTES)",
        ),
        encoding="utf-8",
    )

    # decode cross refs + pub constants
    c = ROOT / "src/hdr/decode/constants.rs"
    for n in [
        "HDR_RGBA32F_BYTES_PER_PIXEL", "SDR_RGBA8_BYTES_PER_PIXEL", "HDR_FALLBACK_BYTES_PER_PIXEL_WITH_SDR",
        "MAX_HDR_FALLBACK_PIXELS", "MAX_HDR_FALLBACK_DECODE_BYTES", "MAX_HDR_FALLBACK_TOTAL_BYTES",
        "MAX_HDR_TONE_MAP_INPUT", "INVERSE_DISPLAY_GAMMA",
    ]:
        c.write_text(c.read_text(encoding="utf-8").replace(f"const {n}", f"pub(crate) const {n}"), encoding="utf-8")
    c.write_text(c.read_text(encoding="utf-8").replace("use crate::hdr::tiled::HdrTiledSource;\n\n", ""), encoding="utf-8")

    di = ROOT / "src/hdr/decode/decode_image.rs"
    for a, b in [
        ("is_exr_path(path)", "super::paths::is_exr_path(path)"),
        ("is_radiance_hdr_path(path)", "super::paths::is_radiance_hdr_path(path)"),
        ("return decode_exr_display_image", "return super::exr::decode_exr_display_image"),
        ("return decode_radiance_hdr_image", "return super::radiance::decode_radiance_hdr_image"),
        ("validate_hdr_fallback_budget(", "super::tone_map::validate_hdr_fallback_budget("),
        ("MAX_HDR_FALLBACK_DECODE_BYTES", "super::constants::MAX_HDR_FALLBACK_DECODE_BYTES"),
    ]:
        di.write_text(di.read_text(encoding="utf-8").replace(a, b), encoding="utf-8")

    exr = ROOT / "src/hdr/decode/exr.rs"
    exr.write_text(
        exr.read_text(encoding="utf-8").replace(
            "validate_hdr_fallback_budget(", "super::tone_map::validate_hdr_fallback_budget("
        ),
        encoding="utf-8",
    )
    rad = ROOT / "src/hdr/decode/radiance.rs"
    rad.write_text(
        rad.read_text(encoding="utf-8")
        .replace("fn decode_radiance_hdr_image", "pub(crate) fn decode_radiance_hdr_image", 1)
        .replace("validate_hdr_fallback_budget(", "super::tone_map::validate_hdr_fallback_budget("),
        encoding="utf-8",
    )
    tone = ROOT / "src/hdr/decode/tone_map.rs"
    m = "// along with this program.  If not, see <https://www.gnu.org/licenses/>.\n\n"
    if "use super::constants::" not in tone.read_text(encoding="utf-8"):
        tone.write_text(
            tone.read_text(encoding="utf-8").replace(
                m,
                m
                + "use super::constants::{\n"
                "    HDR_FALLBACK_BYTES_PER_PIXEL_WITH_SDR, INVERSE_DISPLAY_GAMMA, MAX_HDR_FALLBACK_PIXELS,\n"
                "    MAX_HDR_FALLBACK_TOTAL_BYTES, MAX_HDR_TONE_MAP_INPUT,\n"
                "};\n\n",
                1,
            ),
            encoding="utf-8",
        )

    # monitor visibility
    eff = ROOT / "src/hdr/monitor/effective.rs"
    eff.write_text(
        eff.read_text(encoding="utf-8").replace(
            "use super::macos::macos_active_monitor_hdr_status;",
            "#[cfg(target_os = \"macos\")]\nuse super::macos::macos_active_monitor_hdr_status;",
        ),
        encoding="utf-8",
    )
    mac = ROOT / "src/hdr/monitor/macos.rs"
    if "use super::types::HdrMonitorSelection" not in mac.read_text(encoding="utf-8"):
        mac.write_text(
            mac.read_text(encoding="utf-8").replace(
                m,
                m + "use super::types::HdrMonitorSelection;\n\n",
                1,
            ),
            encoding="utf-8",
        )
    mac.write_text(
        mac.read_text(encoding="utf-8").replace(
            "fn macos_active_monitor_hdr_status", "pub(crate) fn macos_active_monitor_hdr_status", 1
        ),
        encoding="utf-8",
    )
    win = ROOT / "src/hdr/monitor/windows.rs"
    if "dxgi_output_hdr_active" in win.read_text(encoding="utf-8") and "pub(crate) fn dxgi_output_hdr_active" not in win.read_text(encoding="utf-8"):
        # only replace fn at start of line after #[cfg]
        import subprocess as sp
        text = win.read_text(encoding="utf-8")
        text = text.replace("\nfn dxgi_output_hdr_active", "\npub(crate) fn dxgi_output_hdr_active", 1)
        text = text.replace("\nfn finite_positive_luminance", "\npub(crate) fn finite_positive_luminance", 1)
        text = text.replace("\nfn windows_active_monitor_hdr_status", "\npub(crate) fn windows_active_monitor_hdr_status", 1)
        win.write_text(text, encoding="utf-8")
    probe = ROOT / "src/hdr/monitor/probe.rs"
    if "dxgi_output_hdr_active" not in probe.read_text(encoding="utf-8"):
        probe.write_text(
            probe.read_text(encoding="utf-8").replace(
                m,
                m
                + "use super::types::HdrMonitorSelection;\n"
                "use super::windows::{dxgi_output_hdr_active, finite_positive_luminance};\n\n",
                1,
            ),
            encoding="utf-8",
        )

    # radiance cross-module
    hdr = ROOT / "src/hdr/radiance_tiled/header.rs"
    for fn in ["build_radiance_scanline_offsets", "read_radiance_header", "decode_radiance_rgba32f_from_mmap", "validate_scanline_offsets"]:
        t = hdr.read_text(encoding="utf-8")
        if f"pub(crate) fn {fn}" not in t:
            t = t.replace(f"\nfn {fn}", f"\npub(crate) fn {fn}", 1)
        hdr.write_text(t, encoding="utf-8")
    layout = ROOT / "src/hdr/radiance_tiled/layout.rs"
    layout.write_text(layout.read_text(encoding="utf-8").replace("struct Rgbe8Pixel", "pub(crate) struct Rgbe8Pixel", 1), encoding="utf-8")
    src = ROOT / "src/hdr/radiance_tiled/source.rs"
    if "use super::tile_decode::" not in src.read_text(encoding="utf-8"):
        src.write_text(
            src.read_text(encoding="utf-8").replace(
                m,
                m
                + "use super::header::{build_radiance_scanline_offsets, read_radiance_header};\n"
                "use super::layout::RadianceRasterLayout;\n"
                "use super::tile_decode::{decode_radiance_hdr_preview, decode_radiance_sdr_preview, decode_radiance_tile_window};\n\n",
                1,
            ),
            encoding="utf-8",
        )
    td = ROOT / "src/hdr/radiance_tiled/tile_decode.rs"
    for fn in ["decode_radiance_tile_window", "decode_radiance_sdr_preview", "decode_radiance_hdr_preview"]:
        t = td.read_text(encoding="utf-8")
        if f"pub(crate) fn {fn}" not in t:
            t = t.replace(f"\nfn {fn}", f"\npub(crate) fn {fn}", 1)
        td.write_text(t, encoding="utf-8")
    rle = ROOT / "src/hdr/radiance_tiled/rle.rs"
    if "use super::layout::Rgbe8Pixel" not in rle.read_text(encoding="utf-8"):
        rle.write_text(rle.read_text(encoding="utf-8").replace(m, m + "use super::layout::Rgbe8Pixel;\n\n", 1), encoding="utf-8")

    # restore image_management tests monolith
    tests = ROOT / "src/app/image_management/tests"
    if tests.is_dir():
        import shutil
        shutil.rmtree(tests)
    subprocess.run(["git", "restore", "src/app/image_management/tests.rs"], cwd=ROOT, check=False)

    print("minimal fixes done")


if __name__ == "__main__":
    main()
