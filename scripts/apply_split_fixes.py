#!/usr/bin/env python3
"""Post-split fixes for SimpleImageViewer module splits."""
from __future__ import annotations

import re
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SRC = ROOT / "src"

PATHS_RS = """use std::path::Path;

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
"""

TYPES_RS = """use eframe::egui;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HdrMonitorSignature {
    pub(crate) outer_rect: Option<[i32; 4]>,
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

MARKER = "// along with this program.  If not, see <https://www.gnu.org/licenses/>.\n\n"

WIC_IMPORTS_RS = """pub use crate::formats::{FormatGroup, ImageFormat, get_registry};
pub use crate::loader::TiledImageSource;
pub use std::cell::RefCell;
pub use std::sync::atomic::Ordering;
pub use std::thread;
pub use windows::Win32::Foundation::GENERIC_READ;
pub use windows::Win32::Graphics::Imaging::*;
pub use windows::Win32::System::Com::*;
pub use windows::core::*;

thread_local! {
    static WIC_FACTORY: RefCell<Option<IWICImagingFactory>> = RefCell::new(None);
}

pub(crate) fn get_wic_factory() -> windows::core::Result<IWICImagingFactory> {
    WIC_FACTORY.with(|f| {
        let mut factory = f.borrow_mut();
        if factory.is_none() {
            let instance =
                unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)? };
            *factory = Some(instance);
        }
        factory
            .as_ref()
            .cloned()
            .ok_or_else(|| windows::core::Error::from_win32())
    })
}
"""


def git_lines(rel: str) -> list[str]:
    return subprocess.check_output(
        ["git", "show", f"HEAD:{rel}"], cwd=ROOT, text=True, encoding="utf-8"
    ).splitlines(keepends=True)


def replace_once(text: str, old: str, new: str) -> str:
    return text.replace(old, new, 1) if old in text and new not in text else text


def insert_after_marker(path: Path, extra: str) -> None:
    if not path.exists() or extra.strip() in path.read_text(encoding="utf-8"):
        return
    path.write_text(path.read_text(encoding="utf-8").replace(MARKER, MARKER + extra + "\n", 1), encoding="utf-8")


def main() -> None:
    for rel in [
        "src/hdr/decode.rs",
        "src/hdr/monitor.rs",
        "src/hdr/radiance_tiled.rs",
        "src/hdr/tiled.rs",
        "src/wic.rs",
        "src/loader/decode/raw.rs",
        "src/hdr/heif_apple_gain_map_compose_simd.rs",
        "src/hdr/jpegxl.rs",
    ]:
        p = ROOT / rel
        if p.exists():
            p.unlink()

    (SRC / "hdr/decode/paths.rs").write_text(
        "// Simple Image Viewer - A high-performance, cross-platform image viewer\n" + PATHS_RS,
        encoding="utf-8",
    )

    types = SRC / "hdr/monitor/types.rs"
    if types.exists():
        types.write_text(types.read_text(encoding="utf-8").split("use eframe::egui;")[0] + TYPES_RS, encoding="utf-8")

    mod = SRC / "hdr/monitor/mod.rs"
    if mod.exists():
        t = mod.read_text(encoding="utf-8")
        t = re.sub(
            r"pub use windows::any_active_output_supports_hdr;\n?",
            "",
            t,
        )
        if "pub use windows::any_active_output_supports_hdr" not in t:
            t = t.replace(
                "pub use effective::{\n    active_monitor_hdr_status, effective_capability_output_mode,\n"
                "    effective_monitor_selection, effective_render_output_mode,\n};\n",
                "pub use effective::{\n    active_monitor_hdr_status, effective_capability_output_mode,\n"
                "    effective_monitor_selection, effective_render_output_mode,\n};\n"
                "pub use windows::any_active_output_supports_hdr;\n",
            )
        mod.write_text(t, encoding="utf-8")

    insert_after_marker(
        SRC / "hdr/monitor/effective.rs",
        "use super::types::HdrMonitorSelection;\n"
        "#[cfg(target_os = \"macos\")]\n"
        "use super::macos::macos_active_monitor_hdr_status;\n"
        "use super::windows::windows_active_monitor_hdr_status;",
    )

    decode_mod = SRC / "hdr/decode/mod.rs"
    if decode_mod.exists():
        t = decode_mod.read_text(encoding="utf-8")
        t = t.replace("pub use paths::is_hdr_candidate_ext", "pub use decode_image::is_hdr_candidate_ext")
        t = t.replace("pub use radiance::RadianceHeaderParams", "pub(crate) use radiance::RadianceHeaderParams")
        t = t.replace("pub use exr::", "pub(crate) use exr::")
        t = t.replace("pub use tone_map::{", "pub(crate) use tone_map::{")
        decode_mod.write_text(t, encoding="utf-8")

    tiled_mod = SRC / "hdr/tiled/mod.rs"
    if tiled_mod.exists():
        t = tiled_mod.read_text(encoding="utf-8").replace("pub use ", "pub(crate) use ")
        tiled_mod.write_text(t, encoding="utf-8")

    for d in [SRC / "hdr/decode", SRC / "hdr/tiled", SRC / "hdr/radiance_tiled"]:
        if not d.exists():
            continue
        for p in d.rglob("*.rs"):
            t = p.read_text(encoding="utf-8").replace("use super::types::", "use crate::hdr::types::")
            p.write_text(t, encoding="utf-8")

    for d in [SRC / "hdr/monitor"]:
        if not d.exists():
            continue
        for p in d.rglob("*.rs"):
            if p.name in ("mod.rs", "types.rs", "wayland.rs"):
                continue
            t = p.read_text(encoding="utf-8")
            t = t.replace("use super::renderer::", "use crate::hdr::renderer::")
            t = t.replace("use super::types::HdrOutputMode", "use crate::hdr::types::HdrOutputMode")
            if p.stem in ("state", "probe"):
                t = re.sub(r"#\[cfg\(target_os = \"linux\"\)\]\n(?=#\[derive)", "", t, count=1)
                t = re.sub(
                    r"#\[cfg\(target_os = \"linux\"\)\]\n(?=pub struct SpawnMonitorHdrProbe)",
                    "",
                    t,
                    count=1,
                )
            p.write_text(t, encoding="utf-8")

    insert_after_marker(
        SRC / "hdr/monitor/state.rs",
        "use super::effective::active_monitor_hdr_status;\n"
        "use super::types::{HdrMonitorSelection, HdrMonitorSignature, HDR_MONITOR_PROBE_INTERVAL};",
    )
    insert_after_marker(
        SRC / "hdr/monitor/windows.rs",
        "use super::types::{HdrMonitorSelection, HdrNativeSurfaceEncoding};",
    )
    insert_after_marker(
        SRC / "hdr/monitor/macos.rs",
        "use super::types::{HdrMonitorSelection, HdrNativeSurfaceEncoding};",
    )
    insert_after_marker(
        SRC / "hdr/monitor/probe.rs",
        "use super::windows::{dxgi_output_hdr_active, finite_positive_luminance, monitor_device_name};",
    )
    probe = SRC / "hdr/monitor/probe.rs"
    if probe.exists() and "#[derive(Debug, Clone)]" not in probe.read_text(encoding="utf-8"):
        probe.write_text(
            probe.read_text(encoding="utf-8").replace(
                "pub struct SpawnMonitorHdrProbe {",
                "#[derive(Debug, Clone)]\npub struct SpawnMonitorHdrProbe {",
                1,
            ),
            encoding="utf-8",
        )

    # tiled cross-module
    g = SRC / "hdr/tiled/globals.rs"
    if g.exists():
        t = g.read_text(encoding="utf-8")
        t = t.replace("const DEFAULT_HDR_TILE_CACHE_MAX_BYTES", "pub(crate) const DEFAULT_HDR_TILE_CACHE_MAX_BYTES", 1)
        t = t.replace("const MAX_HDR_TILE_CACHE_MAX_BYTES", "pub(crate) const MAX_HDR_TILE_CACHE_MAX_BYTES", 1)
        t = t.replace("static NEXT_HDR_TILE_CACHE_ID", "pub(crate) static NEXT_HDR_TILE_CACHE_ID", 1)
        t = t.replace(
            "AtomicUsize::new(initial_hdr_tile_cache_max_bytes())",
            "AtomicUsize::new(DEFAULT_HDR_TILE_CACHE_MAX_BYTES)",
        )
        g.write_text(t, encoding="utf-8")
    insert_after_marker(
        SRC / "hdr/tiled/buffer.rs",
        "use super::globals::NEXT_HDR_TILE_CACHE_ID;",
    )
    insert_after_marker(
        SRC / "hdr/tiled/cache.rs",
        "use super::buffer::HdrTileBuffer;\n"
        "use super::globals::{\n"
        "    DEFAULT_HDR_TILE_CACHE_MAX_BYTES, HDR_TILE_CACHE_MAX_BYTES, HdrTileCacheKey,\n"
        "    MAX_HDR_TILE_CACHE_MAX_BYTES,\n"
        "};",
    )
    insert_after_marker(
        SRC / "hdr/tiled/kind.rs",
        "use super::buffer::HdrTileBuffer;",
    )
    insert_after_marker(
        SRC / "hdr/tiled/preview.rs",
        "use super::buffer::HdrTileBuffer;\n"
        "use super::globals::{DEFAULT_HDR_TILE_CACHE_MAX_BYTES, HdrTileCacheKey};\n"
        "use super::kind::HdrTiledSource;\n"
        "use super::validate::validate_rgba32f_len;",
    )
    insert_after_marker(
        SRC / "hdr/tiled/source.rs",
        "use super::cache::configured_hdr_tile_cache_max_bytes;\n"
        "use super::preview::downsample_hdr_image_nearest;\n"
        "use super::validate::{validate_rgba32f_len, validate_tile_bounds};",
    )
    insert_after_marker(
        SRC / "hdr/radiance_tiled/tile_decode.rs",
        "use super::layout::{Rgbe8Pixel, inner_range_covering_coord_inclusive, outer_range_covering_coord_inclusive};",
    )
    layout = SRC / "hdr/radiance_tiled/layout.rs"
    if layout.exists():
        t = layout.read_text(encoding="utf-8")
        for fn in ["inner_range_covering_coord_inclusive", "outer_range_covering_coord_inclusive"]:
            t = replace_once(t, f"\nfn {fn}", f"\npub(crate) fn {fn}")
        t = replace_once(t, "\n    fn to_rgb_f32", "\n    pub(crate) fn to_rgb_f32")
        t = t.replace("    rgb:", "    pub(crate) rgb:")
        t = t.replace("    exponent:", "    pub(crate) exponent:")
        t = t.replace(
            "pub(crate) struct RadianceRasterLayout {\n    pub(crate) width: u32,\n    pub(crate) height: u32,\n    outer_axis: RadianceScanAxis,\n    outer_sign: RadianceScanSign,\n    outer_len: u32,\n    inner_axis: RadianceScanAxis,\n    inner_sign: RadianceScanSign,\n    inner_len: u32,\n}",
            "pub(crate) struct RadianceRasterLayout {\n    pub(crate) width: u32,\n    pub(crate) height: u32,\n    pub(crate) outer_axis: RadianceScanAxis,\n    pub(crate) outer_sign: RadianceScanSign,\n    pub(crate) outer_len: u32,\n    pub(crate) inner_axis: RadianceScanAxis,\n    pub(crate) inner_sign: RadianceScanSign,\n    pub(crate) inner_len: u32,\n}",
        )
        t = t.replace(
            "pub(crate) struct RadianceStridePlan {\n    pub(crate) outer_major_is_y: bool,\n    outer_len: u32,\n    inner_len: u32,\n    pub(crate) x_start: i32,\n    x_step: i32,\n    pub(crate) y_start: i32,\n    y_step: i32,\n}",
            "pub(crate) struct RadianceStridePlan {\n    pub(crate) outer_major_is_y: bool,\n    pub(crate) outer_len: u32,\n    pub(crate) inner_len: u32,\n    pub(crate) x_start: i32,\n    pub(crate) x_step: i32,\n    pub(crate) y_start: i32,\n    pub(crate) y_step: i32,\n}",
        )
        layout.write_text(t, encoding="utf-8")

    rle_vis = SRC / "hdr/radiance_tiled/rle.rs"
    if rle_vis.exists():
        rle_vis.write_text(
            rle_vis.read_text(encoding="utf-8").replace(
                "    fn to_rgb_f32(self)", "    pub(crate) fn to_rgb_f32(self)", 1
            ),
            encoding="utf-8",
        )

    for raw_part, fns in [
        ("preview.rs", ["extract_embedded_preview", "raw_embedded_preview_meets_hq_requirement"]),
        ("develop.rs", ["develop_full_resolution", "develop_hq_preview"]),
    ]:
        p = SRC / "loader/decode/raw" / raw_part
        if p.exists():
            t = p.read_text(encoding="utf-8")
            for fn in fns:
                t = replace_once(t, f"\nfn {fn}", f"\npub(crate) fn {fn}")
            p.write_text(t, encoding="utf-8")
    load = SRC / "loader/decode/raw/load.rs"
    if load.exists():
        load.write_text(
            load.read_text(encoding="utf-8").replace(
                MARKER,
                MARKER
                + "use super::develop::{develop_full_resolution, develop_hq_preview};\n"
                "use super::preview::{extract_embedded_preview, raw_embedded_preview_meets_hq_requirement};\n\n",
                1,
            ),
            encoding="utf-8",
        )

    insert_after_marker(
        SRC / "wic/load.rs",
        "use super::tiled_source::WicTiledSource;",
    )
    insert_after_marker(
        SRC / "wic/discovery.rs",
        "use super::com::ComGuard;",
    )
    insert_after_marker(
        SRC / "wic/tiled_source.rs",
        "use super::com::ComGuard;",
    )

    insert_after_marker(
        SRC / "hdr/tiled/source.rs",
        "use super::buffer::HdrTileBuffer;\n"
        "use super::cache::HdrTileCache;\n"
        "use super::globals::HdrTileCacheKey;\n"
        "use super::kind::{HdrTiledSource, HdrTiledSourceKind};",
    )

    # radiance cross-module imports
    insert_after_marker(
        SRC / "hdr/radiance_tiled/header.rs",
        "use super::layout::{RadianceRasterLayout, RadianceScanAxis, RadianceScanSign, Rgbe8Pixel};\n"
        "use super::rle::{read_scanline, skip_scanline};",
    )
    insert_after_marker(
        SRC / "hdr/radiance_tiled/tile_decode.rs",
        "use super::header::{build_radiance_scanline_offsets, read_radiance_header, validate_scanline_offsets};\n"
        "use super::layout::RadianceRasterLayout;\n"
        "use super::rle::read_scanline;",
    )
    layout = SRC / "hdr/radiance_tiled/layout.rs"
    if layout.exists():
        t = layout.read_text(encoding="utf-8")
        for name in ["RadianceRasterLayout", "RadianceScanAxis", "RadianceScanSign"]:
            t = replace_once(t, f"struct {name}", f"pub(crate) struct {name}")
            t = replace_once(t, f"enum {name}", f"pub(crate) enum {name}")
        t = replace_once(t, "struct Rgbe8Pixel", "pub(crate) struct Rgbe8Pixel")
        layout.write_text(t, encoding="utf-8")
    rle = SRC / "hdr/radiance_tiled/rle.rs"
    if rle.exists():
        t = rle.read_text(encoding="utf-8")
        t = replace_once(t, "\nfn read_scanline", "\npub(crate) fn read_scanline")
        t = replace_once(t, "\nfn skip_scanline", "\npub(crate) fn skip_scanline")
        rle.write_text(t, encoding="utf-8")


    # decode cross-module refs
    di = SRC / "hdr/decode/decode_image.rs"
    if di.exists():
        t = di.read_text(encoding="utf-8")
        for a, b in [
            ("is_exr_path(path)", "super::paths::is_exr_path(path)"),
            ("is_radiance_hdr_path(path)", "super::paths::is_radiance_hdr_path(path)"),
            ("return decode_exr_display_image", "return super::exr::decode_exr_display_image"),
            ("return decode_radiance_hdr_image", "return super::radiance::decode_radiance_hdr_image"),
            ("validate_hdr_fallback_budget(", "super::tone_map::validate_hdr_fallback_budget("),
            ("MAX_HDR_FALLBACK_DECODE_BYTES", "super::constants::MAX_HDR_FALLBACK_DECODE_BYTES"),
        ]:
            t = t.replace(a, b)
        di.write_text(t, encoding="utf-8")

    for name in [
        "HDR_RGBA32F_BYTES_PER_PIXEL", "SDR_RGBA8_BYTES_PER_PIXEL", "HDR_FALLBACK_BYTES_PER_PIXEL_WITH_SDR",
        "MAX_HDR_FALLBACK_PIXELS", "MAX_HDR_FALLBACK_DECODE_BYTES", "MAX_HDR_FALLBACK_TOTAL_BYTES",
        "MAX_HDR_TONE_MAP_INPUT", "INVERSE_DISPLAY_GAMMA",
    ]:
        c = SRC / "hdr/decode/constants.rs"
        c.write_text(c.read_text(encoding="utf-8").replace(f"const {name}", f"pub(crate) const {name}"), encoding="utf-8")

    c = SRC / "hdr/decode/constants.rs"
    c.write_text(c.read_text(encoding="utf-8").replace("use crate::hdr::tiled::HdrTiledSource;\n\n", ""), encoding="utf-8")

    exr = SRC / "hdr/decode/exr.rs"
    exr.write_text(
        exr.read_text(encoding="utf-8").replace(
            "validate_hdr_fallback_budget(", "super::tone_map::validate_hdr_fallback_budget("
        ),
        encoding="utf-8",
    )
    rad = SRC / "hdr/decode/radiance.rs"
    rad.write_text(
        rad.read_text(encoding="utf-8")
        .replace("fn decode_radiance_hdr_image", "pub(crate) fn decode_radiance_hdr_image", 1)
        .replace("validate_hdr_fallback_budget(", "super::tone_map::validate_hdr_fallback_budget("),
        encoding="utf-8",
    )
    tone = SRC / "hdr/decode/tone_map.rs"
    if "use super::constants::" not in tone.read_text(encoding="utf-8"):
        tone.write_text(
            tone.read_text(encoding="utf-8").replace(
                MARKER,
                MARKER
                + "use super::constants::{\n"
                "    HDR_FALLBACK_BYTES_PER_PIXEL_WITH_SDR, INVERSE_DISPLAY_GAMMA, MAX_HDR_FALLBACK_PIXELS,\n"
                "    MAX_HDR_FALLBACK_TOTAL_BYTES, MAX_HDR_TONE_MAP_INPUT,\n"
                "};\n\n",
                1,
            ),
            encoding="utf-8",
        )

    # tiled validate visibility
    v = SRC / "hdr/tiled/validate.rs"
    v.write_text(
        v.read_text(encoding="utf-8").replace("fn validate_rgba32f_len", "pub(crate) fn validate_rgba32f_len", 1),
        encoding="utf-8",
    )

    # monitor visibility
    mac = SRC / "hdr/monitor/macos.rs"
    if mac.exists():
        t = mac.read_text(encoding="utf-8")
        if t.count("use super::types::HdrMonitorSelection") > 1:
            t = t.replace("use super::types::HdrMonitorSelection;\n\n", "", 1)
        mac.write_text(
            t.replace(
                "fn macos_active_monitor_hdr_status", "pub(crate) fn macos_active_monitor_hdr_status", 1
            ),
            encoding="utf-8",
        )
    win = SRC / "hdr/monitor/windows.rs"
    t = win.read_text(encoding="utf-8")
    t = replace_once(t, "\nfn dxgi_output_hdr_active", "\npub(crate) fn dxgi_output_hdr_active")
    t = replace_once(t, "\nfn finite_positive_luminance", "\npub(crate) fn finite_positive_luminance")
    t = replace_once(t, "\nfn windows_active_monitor_hdr_status", "\npub(crate) fn windows_active_monitor_hdr_status")
    t = replace_once(t, "\nfn monitor_device_name", "\npub(crate) fn monitor_device_name")
    win.write_text(t, encoding="utf-8")

    # radiance cross-module visibility
    hdr = SRC / "hdr/radiance_tiled/header.rs"
    t = hdr.read_text(encoding="utf-8")
    for fn in ["build_radiance_scanline_offsets", "read_radiance_header", "decode_radiance_rgba32f_from_mmap", "validate_scanline_offsets"]:
        t = replace_once(t, f"\nfn {fn}", f"\npub(crate) fn {fn}")
    hdr.write_text(t, encoding="utf-8")
    layout = SRC / "hdr/radiance_tiled/layout.rs"
    if layout.exists() and "pub(crate) struct Rgbe8Pixel" not in layout.read_text(encoding="utf-8"):
        t = layout.read_text(encoding="utf-8")
        for name in ["RadianceRasterLayout", "RadianceScanAxis", "RadianceScanSign"]:
            t = replace_once(t, f"struct {name}", f"pub(crate) struct {name}")
            t = replace_once(t, f"enum {name}", f"pub(crate) enum {name}")
        t = replace_once(t, "struct Rgbe8Pixel", "pub(crate) struct Rgbe8Pixel")
        layout.write_text(t, encoding="utf-8")
    src = SRC / "hdr/radiance_tiled/source.rs"
    if "use super::tile_decode::" not in src.read_text(encoding="utf-8"):
        src.write_text(
            src.read_text(encoding="utf-8").replace(
                MARKER,
                MARKER
                + "use super::header::{build_radiance_scanline_offsets, read_radiance_header};\n"
                "use super::layout::RadianceRasterLayout;\n"
                "use super::tile_decode::{decode_radiance_hdr_preview, decode_radiance_sdr_preview, decode_radiance_tile_window};\n\n",
                1,
            ),
            encoding="utf-8",
        )
    td = SRC / "hdr/radiance_tiled/tile_decode.rs"
    t = td.read_text(encoding="utf-8")
    for fn in ["decode_radiance_tile_window", "decode_radiance_sdr_preview", "decode_radiance_hdr_preview"]:
        t = replace_once(t, f"\nfn {fn}", f"\npub(crate) fn {fn}")
    td.write_text(t, encoding="utf-8")
    rle = SRC / "hdr/radiance_tiled/rle.rs"
    if "use super::layout::Rgbe8Pixel" not in rle.read_text(encoding="utf-8"):
        rle.write_text(rle.read_text(encoding="utf-8").replace(MARKER, MARKER + "use super::layout::Rgbe8Pixel;\n\n", 1), encoding="utf-8")

    # jpegxl runner visibility (decode module is monolithic again)
    runner = SRC / "hdr/jpegxl/runner.rs"
    if runner.exists():
        runner.write_text(
            runner.read_text(encoding="utf-8")
            .replace("struct JxlResizableRunnerPtr", "pub(crate) struct JxlResizableRunnerPtr", 1)
            .replace("    fn try_new()", "    pub(crate) fn try_new()", 1)
            .replace("    fn as_ptr(&self)", "    pub(crate) fn as_ptr(&self)", 1),
            encoding="utf-8",
        )
    decode = SRC / "hdr/jpegxl/decode.rs"
    if decode.exists() and "use super::runner::JxlResizableRunnerPtr" not in decode.read_text(encoding="utf-8"):
        decode.write_text(
            decode.read_text(encoding="utf-8").replace(
                MARKER,
                MARKER + "use super::probe::is_jxl_header;\nuse super::runner::JxlResizableRunnerPtr;\n\n",
                1,
            ),
            encoding="utf-8",
        )
    # wic load + raw doc fix
    lines = git_lines("src/wic.rs")
    (SRC / "wic/load.rs").write_text(
        "".join(lines[:16]) + "use super::com::ComGuard;\nuse super::imports::*;\nuse super::tiled_source::WicTiledSource;\nuse windows::core::*;\n\n" + "".join(lines[593:]),
        encoding="utf-8",
    )
    raw_mod = SRC / "loader/decode/raw/mod.rs"
    doc = "".join(git_lines("src/loader/decode/raw.rs")[16:26])
    if "LibRAW" not in raw_mod.read_text(encoding="utf-8"):
        raw_mod.write_text(raw_mod.read_text(encoding="utf-8").replace("mod develop;", doc + "\nmod develop;", 1), encoding="utf-8")
    prev = SRC / "loader/decode/raw/preview.rs"
    if prev.exists():
        t = prev.read_text(encoding="utf-8")
        # Remove duplicate module doc block left in preview body (doc lives in mod.rs).
        doc_block = "".join(git_lines("src/loader/decode/raw.rs")[16:26])
        t = t.replace(doc_block, "", 1)
        prev.write_text(t, encoding="utf-8")
    for p in [SRC / "loader/decode/raw/preview.rs", SRC / "loader/decode/raw/develop.rs", SRC / "loader/decode/raw/load.rs"]:
        p.write_text(
            p.read_text(encoding="utf-8").replace("use super::assemble::", "use crate::loader::decode::assemble::"),
            encoding="utf-8",
        )

    # wic imports + load rebuild
    (SRC / "wic/imports.rs").write_text(
        "// Simple Image Viewer - A high-performance, cross-platform image viewer\n" + WIC_IMPORTS_RS,
        encoding="utf-8",
    )

    tiled_src = SRC / "wic/tiled_source.rs"
    if tiled_src.exists():
        t = tiled_src.read_text(encoding="utf-8")
        t = t.replace("    fn wrap_with_cache(", "    pub(crate) fn wrap_with_cache(", 1)
        if "pub(crate) path:" not in t:
            for field in [
                "path", "width", "height", "factory", "decoder", "frame", "source",
                "physical_width", "physical_height", "transform_options", "stream", "_mmap",
            ]:
                t = t.replace(f"    {field}:", f"    pub(crate) {field}:", 1)
        tiled_src.write_text(t, encoding="utf-8")

    install = SRC / "app/image_management/loader_results/install.rs"
    if install.exists():
        install.write_text(
            install.read_text(encoding="utf-8").replace(
                "    fn handle_image_load_result(",
                "    pub(crate) fn handle_image_load_result(",
                1,
            ),
            encoding="utf-8",
        )

    print("fixes applied")


if __name__ == "__main__":
    main()
