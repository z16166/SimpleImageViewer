#!/usr/bin/env python3
"""Rebuild split submodules from git monoliths with correct boundaries and imports."""
from __future__ import annotations

import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

TYPES_RS = """use eframe::egui;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HdrMonitorSignature {
    outer_rect: Option<[i32; 4]>,
    monitor_size: Option<[i32; 2]>,
    native_pixels_per_point_milli: Option<i32>,
}

impl HdrMonitorSignature {
    pub fn from_viewport(viewport: &egui::ViewportInfo) -> Self {
        Self {
            outer_rect: viewport.outer_rect.map(|rect| {
                [
                    rect.min.x.round() as i32,
                    rect.min.y.round() as i32,
                    rect.max.x.round() as i32,
                    rect.max.y.round() as i32,
                ]
            }),
            monitor_size: viewport
                .monitor_size
                .map(|size| [size.x.round() as i32, size.y.round() as i32]),
            native_pixels_per_point_milli: viewport
                .native_pixels_per_point
                .map(|value| (value * 1000.0).round() as i32),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HdrNativeSurfaceEncoding {
    LinearScRgb,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    PqHdr10,
    #[allow(dead_code)]
    Gamma22Electrical,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HdrMonitorSelection {
    pub hdr_supported: bool,
    pub label: String,
    pub max_luminance_nits: Option<f32>,
    pub max_full_frame_luminance_nits: Option<f32>,
    pub max_hdr_capacity: Option<f32>,
    pub hdr_capacity_source: Option<&'static str>,
    pub native_surface_encoding: Option<HdrNativeSurfaceEncoding>,
}

pub(crate) const HDR_MONITOR_PROBE_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(750);
"""


def git_lines(rel: str) -> list[str]:
    text = subprocess.check_output(
        ["git", "show", f"HEAD:{rel}"], cwd=ROOT, text=True, encoding="utf-8"
    )
    return text.splitlines(keepends=True)


def copyright(lines: list[str]) -> str:
    return "".join(lines[:16])


def test_start(lines: list[str]) -> int:
    for i, line in enumerate(lines):
        if line.startswith("#[cfg(test)]") and i + 1 < len(lines) and "mod tests" in lines[i + 1]:
            return i
    return len(lines)


def write(base: Path, name: str, header: str, body: str) -> None:
    (base / name).write_text(header + body, encoding="utf-8")


def rebuild_decode() -> None:
    lines = git_lines("src/hdr/decode.rs")
    base = ROOT / "src/hdr/decode"
    imports = "".join(lines[16:30]).replace("use super::types::", "use crate::hdr::types::")
    for name, start, end in [
        ("constants.rs", 17, 41),
        ("decode_image.rs", 43, 82),
        ("radiance.rs", 84, 186),
        ("exr.rs", 188, 202),
        ("tone_map.rs", 223, 557),
    ]:
        extra = "" if name == "constants.rs" else imports.replace(
            "use super::types::", "use crate::hdr::types::"
        )
        write(base, name, copyright(lines), extra + "".join(lines[start - 1 : end]))
    write(
        base,
        "paths.rs",
        copyright(lines),
        """use std::path::Path;

pub(crate) fn is_exr_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("exr"))
}

pub(crate) fn is_radiance_hdr_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| matches!(ext.to_ascii_lowercase().as_str(), "hdr" | "pic"))
}
""",
    )
    radiance = base / "radiance.rs"
    radiance.write_text(
        radiance.read_text(encoding="utf-8").replace(
            "fn decode_radiance_hdr_image", "pub(crate) fn decode_radiance_hdr_image", 1
        ),
        encoding="utf-8",
    )
    decode_image = base / "decode_image.rs"
    t = decode_image.read_text(encoding="utf-8")
    t = t.replace("is_exr_path(path)", "super::paths::is_exr_path(path)")
    t = t.replace("is_radiance_hdr_path(path)", "super::paths::is_radiance_hdr_path(path)")
    t = t.replace(
        "return decode_exr_display_image(path);",
        "return super::exr::decode_exr_display_image(path);",
    )
    t = t.replace(
        "return decode_radiance_hdr_image(path);",
        "return super::radiance::decode_radiance_hdr_image(path);",
    )
    t = t.replace(
        "validate_hdr_fallback_budget",
        "super::tone_map::validate_hdr_fallback_budget",
    )
    t = t.replace(
        "limits.max_alloc = Some(MAX_HDR_FALLBACK_DECODE_BYTES);",
        "limits.max_alloc = Some(super::constants::MAX_HDR_FALLBACK_DECODE_BYTES);",
    )
    decode_image.write_text(t, encoding="utf-8")
    print("decode/")


def rebuild_tiled() -> None:
    lines = git_lines("src/hdr/tiled.rs")
    base = ROOT / "src/hdr/tiled"
    imports = "".join(lines[16:27])
    for name, start, end in [
        ("globals.rs", 17, 34),
        ("buffer.rs", 36, 101),
        ("kind.rs", 103, 149),
        ("source.rs", 151, 301),
        ("preview.rs", 302, 436),
        ("cache.rs", 437, 548),
        ("validate.rs", 550, 596),
    ]:
        extra = "" if name == "globals.rs" else imports
        body = extra + "".join(lines[start - 1 : end])
        write(base, name, copyright(lines), body)
    validate = base / "validate.rs"
    validate.write_text(
        validate.read_text(encoding="utf-8").replace(
            "fn validate_rgba32f_len", "pub(crate) fn validate_rgba32f_len", 1
        ),
        encoding="utf-8",
    )
    for fname in ["buffer.rs", "source.rs", "preview.rs", "cache.rs"]:
        t = (base / fname).read_text(encoding="utf-8")
        if "use super::buffer" not in t:
            ins = (
                "use super::buffer::HdrTileBuffer;\n"
                "use super::cache::{HdrTileCache, configured_hdr_tile_cache_max_bytes};\n"
                "use super::globals::{\n"
                "    DEFAULT_HDR_TILE_CACHE_MAX_BYTES, HDR_TILE_CACHE_MAX_BYTES, HdrTileCacheKey,\n"
                "    MAX_HDR_TILE_CACHE_MAX_BYTES,\n"
                "};\n"
                "use super::kind::{HdrTiledSource, HdrTiledSourceKind};\n"
                "use super::validate::{validate_rgba32f_len, validate_tile_bounds};\n\n"
            )
            marker = "// along with this program.  If not, see <https://www.gnu.org/licenses/>.\n\n"
            # skip duplicate import block from monolith (lines 17-27)
            rest = t.split(marker, 1)[-1]
            while rest.startswith("use parking_lot") or rest.startswith("use std::"):
                rest = rest.split("\n\n", 1)[-1]
            (base / fname).write_text(copyright(lines) + ins + rest, encoding="utf-8")
    print("tiled/")


def rebuild_radiance_tiled() -> None:
    lines = git_lines("src/hdr/radiance_tiled.rs")
    base = ROOT / "src/hdr/radiance_tiled"
    imports = "".join(lines[16:27])
    for name, start, end in [
        ("layout.rs", 17, 303),
        ("source.rs", 305, 447),
        ("tile_decode.rs", 449, 664),
        ("header.rs", 666, 860),
        ("rle.rs", 862, test_start(lines)),
    ]:
        extra = "" if name == "layout.rs" else imports
        write(base, name, copyright(lines), extra + "".join(lines[start - 1 : end]))
    for fname in ["source.rs", "tile_decode.rs", "header.rs", "rle.rs"]:
        t = (base / fname).read_text(encoding="utf-8")
        ins = (
            "use super::header::{decode_radiance_rgba32f_from_mmap, read_radiance_header};\n"
            "use super::layout::{RadianceRasterLayout, RadianceScanAxis, RadianceScanSign, build_radiance_scanline_offsets};\n"
            "use super::rle::decode_radiance_rle_scanline;\n\n"
        )
        if "use super::layout" not in t:
            marker = "// along with this program.  If not, see <https://www.gnu.org/licenses/>.\n\n"
            # only add relevant imports per file - use minimal set
            if fname == "source.rs":
                ins = (
                    "use super::header::read_radiance_header;\n"
                    "use super::layout::{RadianceRasterLayout, build_radiance_scanline_offsets};\n\n"
                )
            elif fname == "tile_decode.rs":
                ins = "use super::layout::RadianceRasterLayout;\nuse super::rle::decode_radiance_rle_scanline;\n\n"
            elif fname == "header.rs":
                ins = "use super::layout::{RadianceRasterLayout, RadianceScanAxis, RadianceScanSign};\n\n"
            t = t.replace(marker, marker + ins, 1)
            (base / fname).write_text(t, encoding="utf-8")
    print("radiance_tiled/")


def rebuild_monitor() -> None:
    lines = git_lines("src/hdr/monitor.rs")
    base = ROOT / "src/hdr/monitor"
    write(base, "types.rs", copyright(lines), TYPES_RS)
    win_prefix = "".join(lines[25:52])
    for name, start, end, prefix in [
        ("state.rs", 112, 287, ""),
        ("effective.rs", 289, 378, ""),
        ("macos.rs", 380, 415, ""),
        ("windows.rs", 416, 727, win_prefix),
        ("probe.rs", 731, 901, ""),
    ]:
        write(base, name, copyright(lines), prefix + "".join(lines[start - 1 : end]))
    macos = base / "macos.rs"
    macos.write_text(macos.read_text(encoding="utf-8") + "".join(lines[901:1033]), encoding="utf-8")
    win = base / "windows.rs"
    wmarker = "// along with this program.  If not, see <https://www.gnu.org/licenses/>.\n\n"
    win.write_text(
        win.read_text(encoding="utf-8").replace(
            wmarker,
            wmarker + "use super::types::HdrMonitorSelection;\n\n",
            1,
        )
        .replace("fn finite_positive_luminance", "pub(crate) fn finite_positive_luminance", 1)
        .replace("fn dxgi_output_hdr_active", "pub(crate) fn dxgi_output_hdr_active", 1),
        encoding="utf-8",
    )
    effective = base / "effective.rs"
    ins = (
        "use crate::hdr::renderer::HdrRenderOutputMode;\n"
        "use crate::hdr::types::HdrOutputMode;\n\n"
        "use super::macos::macos_active_monitor_hdr_status;\n"
        "use super::types::HdrMonitorSelection;\n"
        "use super::windows::windows_active_monitor_hdr_status;\n\n"
    )
    t = effective.read_text(encoding="utf-8")
    marker = "// along with this program.  If not, see <https://www.gnu.org/licenses/>.\n\n"
    effective.write_text(t.replace(marker, marker + ins, 1), encoding="utf-8")
    probe = base / "probe.rs"
    pins = (
        "use super::types::HdrMonitorSelection;\n"
        "use super::windows::{dxgi_output_hdr_active, finite_positive_luminance};\n\n"
    )
    pt = probe.read_text(encoding="utf-8")
    if "dxgi_output_hdr_active" not in pt:
        probe.write_text(pt.replace(marker, marker + pins, 1), encoding="utf-8")
    mod_rs = """mod effective;
mod macos;
mod probe;
mod state;
mod types;
mod windows;

#[cfg(target_os = "linux")]
mod wayland;

#[cfg(test)]
mod tests;

pub use types::{HdrMonitorSelection, HdrMonitorSignature, HdrNativeSurfaceEncoding};
pub use state::HdrMonitorState;
pub use probe::{spawn_monitor_hdr_status, SpawnMonitorHdrProbe};
pub use effective::{
    active_monitor_hdr_status, effective_capability_output_mode,
    effective_monitor_selection, effective_render_output_mode,
};
pub use windows::any_active_output_supports_hdr;

#[cfg(target_os = "windows")]
pub(crate) use windows::dxgi_output_hdr_active;
"""
    (base / "mod.rs").write_text(copyright(lines) + mod_rs, encoding="utf-8")
    print("monitor/")


def rebuild_wic() -> None:
    lines = git_lines("src/wic.rs")
    base = ROOT / "src/wic"
    for name, start, end in [
        ("imports.rs", 17, 45),
        ("com.rs", 47, 86),
        ("discovery.rs", 87, 197),
        ("tiled_source.rs", 199, 592),
        ("load.rs", 594, 961),
    ]:
        write(base, name, copyright(lines), "".join(lines[start - 1 : end]))
    imports = base / "imports.rs"
    imports.write_text(
        imports.read_text(encoding="utf-8").replace(
            "fn get_wic_factory", "pub(crate) fn get_wic_factory", 1
        ),
        encoding="utf-8",
    )
    load = base / "load.rs"
    body = "".join(lines[593:961])
    load.write_text(
        copyright(lines)
        + "use super::com::ComGuard;\nuse super::imports::*;\nuse windows::core::*;\n\n"
        + body,
        encoding="utf-8",
    )
    print("wic/")


def rebuild_raw() -> None:
    lines = git_lines("src/loader/decode/raw.rs")
    base = ROOT / "src/loader/decode/raw"
    imports = "".join(lines[16:42]).replace(
        "use super::assemble", "use crate::loader::decode::assemble"
    )
    for name, start, end in [
        ("preview.rs", 43, 124),
        ("develop.rs", 126, 284),
        ("load.rs", 285, test_start(lines)),
    ]:
        write(base, name, copyright(lines), imports + "".join(lines[start - 1 : end]))
    print("raw/")


def rebuild_jpegxl() -> None:
    lines = git_lines("src/hdr/jpegxl.rs")
    base = ROOT / "src/hdr/jpegxl"
    for name, start, end in [
        ("runner.rs", 37, 61),
        ("probe.rs", 62, 197),
        ("decode.rs", 199, 1218),
        ("metadata.rs", 1220, test_start(lines)),
    ]:
        extra = "use super::probe::is_jxl_header;\n\n" if name == "decode.rs" else ""
        write(base, name, copyright(lines), extra + "".join(lines[start - 1 : end]))
    mod_rs = """mod decode;
mod metadata;
mod probe;
mod runner;

#[cfg(test)]
mod tests;

pub(crate) use probe::{
    is_jxl_header, jxl_color_encoding_to_metadata, libjxl_probe_orientation_from_bytes,
    libjxl_probe_orientation_from_path,
};
#[cfg(feature = "jpegxl")]
pub(crate) use decode::{
    decode_jxl_bytes_to_image_data, decode_jxl_hdr, decode_jxl_hdr_bytes,
    decode_jxl_hdr_bytes_with_target_capacity, decode_jxl_hdr_with_target_capacity, load_jxl_hdr,
    load_jxl_hdr_with_target_capacity, srgb_unit_to_u8,
};
#[cfg(feature = "jpegxl")]
pub(crate) use metadata::{
    decode_jxl_gain_map_from_bundle, read_jxl_gain_map_bundle, JxlGainMapBundleRef,
};
"""
    (base / "mod.rs").write_text(copyright(lines) + mod_rs, encoding="utf-8")
    print("jpegxl/")


def rebuild_heif_apple() -> None:
    lines = git_lines("src/hdr/heif_apple_gain_map_compose_simd.rs")
    base = ROOT / "src/hdr/heif_apple_gain_map_compose_simd"
    for name, start, end in [("core.rs", 17, 946), ("compose.rs", 947, test_start(lines))]:
        write(base, name, copyright(lines), "".join(lines[start - 1 : end]))
    print("heif_apple/")


def main() -> None:
    rebuild_decode()
    rebuild_tiled()
    rebuild_radiance_tiled()
    rebuild_monitor()
    rebuild_wic()
    rebuild_raw()
    rebuild_jpegxl()
    rebuild_heif_apple()
    print("all rebuilds done")


if __name__ == "__main__":
    main()
