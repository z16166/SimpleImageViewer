#!/usr/bin/env python3
"""Split large Rust modules — line ranges verified against source structure."""

from __future__ import annotations

from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SRC = ROOT / "src"

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


def slice_lines(lines: list[str], start: int, end_inclusive: int) -> str:
    """Extract lines [start, end_inclusive] using 1-based line numbers."""
    return "".join(lines[start - 1 : end_inclusive])


def write_part(path: Path, body: str, copyright: bool = True) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    text = (COPYRIGHT if copyright else "") + body
    path.write_text(text, encoding="utf-8")


def extract_tests(lines: list[str]) -> tuple[str, int]:
    for i, line in enumerate(lines):
        if line.startswith("#[cfg(test)]") and i + 1 < len(lines) and lines[i + 1].strip().startswith("mod tests"):
            return "".join(lines[i + 1 :]), i + 1
    # No tests: use len+1 so `test_start - 1` keeps the final source line in slices.
    return "", len(lines) + 1


def slice_ranges(lines: list[str], ranges: list[tuple[int, int]]) -> str:
    return "".join(slice_lines(lines, s, e) for s, e in ranges)


def extract_imports(lines: list[str]) -> str:
    out: list[str] = []
    i = 16
    while i < len(lines):
        line = lines[i]
        stripped = line.strip()
        if not stripped:
            i += 1
            continue
        if stripped.startswith("use ") or stripped.startswith("#[cfg"):
            out.append(line)
            i += 1
            continue
        if stripped.startswith("//") or stripped.startswith("//!"):
            i += 1
            continue
        if stripped.startswith("mod "):
            while out and out[-1].strip().startswith("#[cfg"):
                out.pop()
            i += 1
            continue
        if out and (
            stripped.startswith("}")
            or stripped.endswith(",")
            or stripped.endswith("};")
            or (out[-1].rstrip().endswith("{") and not stripped.startswith("#"))
        ):
            out.append(line)
            if stripped.endswith("};") or stripped == "};":
                i += 1
                continue
            i += 1
            continue
        if not out:
            i += 1
            continue
        break
    return "".join(out)


def split_file(
    src: Path,
    dst_dir: Path,
    parts: list[tuple[str, int | list[tuple[int, int]], int | None]],
    mod_body: str,
    *,
    delete_src: bool = True,
    import_parts: set[str] | None = None,
) -> None:
    if not src.exists():
        print(f"skip missing {src}")
        return
    lines = read_lines(src)
    imports = extract_imports(lines)
    tests, test_start = extract_tests(lines)
    inject_all = import_parts is None
    for item in parts:
        name = item[0]
        if len(item) == 2:
            start, end = item[1], test_start - 1
        else:
            start, end = item[1], item[2]
        if isinstance(start, list):
            body = slice_ranges(lines, start)
        else:
            end = min(end, test_start - 1 if test_start <= len(lines) else end)
            body = slice_lines(lines, start, end)
        if name == "imports" or name == "mod":
            pass
        elif import_parts is not None:
            if "use super::imports::*" not in body:
                body = "use super::imports::*;\n\n" + body
        elif not body.lstrip().startswith("use ") and not body.lstrip().startswith("#[cfg"):
            body = imports + body
        write_part(dst_dir / f"{name}.rs", body)
    if tests.strip():
        write_part(dst_dir / "tests.rs", tests.strip() + "\n", copyright=False)
    write_part(dst_dir / "mod.rs", mod_body)
    if delete_src:
        src.unlink()
    print(f"split {src.relative_to(ROOT)} -> {dst_dir.relative_to(ROOT)}/")


def split_test_file(src: Path, dst_dir: Path, chunk_lines: int = 450) -> None:
    if not src.exists():
        return
    lines = read_lines(src)
    body_start = 0
    for i, line in enumerate(lines):
        if line.strip().startswith("mod tests"):
            body_start = i + 1
            break
    content_lines = lines[body_start:]
    if content_lines and content_lines[0].strip() == "{":
        content_lines = content_lines[1:]
    if content_lines and content_lines[-1].strip() == "}":
        content_lines = content_lines[:-1]

    chunks: list[tuple[str, list[str]]] = []
    current: list[str] = ["use super::*;\n\n"]
    part = 1
    for line in content_lines:
        current.append(line)
        if line.startswith("#[test]") and len(current) > chunk_lines:
            chunks.append((f"part{part}", current))
            part += 1
            current = ["use super::*;\n\n"]
    if len(current) > 2:
        chunks.append((f"part{part}", current))

    if len(chunks) <= 1 and len(content_lines) > chunk_lines:
        chunks = []
        part = 1
        for i in range(0, len(content_lines), chunk_lines):
            chunk = ["use super::*;\n\n"] + content_lines[i : i + chunk_lines]
            chunks.append((f"part{part}", chunk))
            part += 1

    if not chunks:
        return
    dst_dir.mkdir(parents=True, exist_ok=True)
    mod_names = []
    for name, chunk in chunks:
        write_part(dst_dir / f"{name}.rs", "".join(chunk), copyright=False)
        mod_names.append(name)
    write_part(dst_dir / "mod.rs", "\n".join(f"mod {n};" for n in mod_names) + "\n")
    src.unlink()
    print(f"split tests {src.relative_to(ROOT)} -> {dst_dir.relative_to(ROOT)}/")


def main() -> None:
    # monitor: keep existing wayland.rs
    split_file(
        SRC / "hdr" / "monitor.rs",
        SRC / "hdr" / "monitor",
        [
            ("types", 17, 110),
            ("state", 112, 287),
            ("effective", 289, 378),
            ("macos", [(380, 415), (902, 1033)], None),
            ("windows", [(27, 52), (416, 727), (1026, 1033)], None),
            ("probe", 731, 901),
        ],
        """mod effective;
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
""",
        delete_src=True,
    )

    split_file(
        SRC / "hdr" / "radiance_tiled.rs",
        SRC / "hdr" / "radiance_tiled",
        [
            ("layout", 17, 303),
            ("source", 305, 447),
            ("tile_decode", 449, 664),
            ("header", 666, 860),
            ("rle", 862, 1079),
        ],
        """mod header;
mod layout;
mod rle;
mod source;
mod tile_decode;

#[cfg(test)]
mod tests;

pub(crate) use layout::{RadianceRasterLayout, RadianceScanAxis, RadianceScanSign};
pub(crate) use header::decode_radiance_rgba32f_from_mmap;
pub use source::RadianceHdrTiledImageSource;
""",
    )

    split_file(
        SRC / "hdr" / "decode.rs",
        SRC / "hdr" / "decode",
        [
            ("constants", 17, 41),
            ("decode_image", 43, 82),
            ("radiance", 84, 186),
            ("exr", 188, 202),
            ("paths", 204, 214),
            ("tone_map", 223, 557),
        ],
        """mod constants;
mod decode_image;
mod exr;
mod paths;
mod radiance;
mod tone_map;

#[cfg(test)]
mod tests;

pub use decode_image::{decode_hdr_image, is_hdr_candidate_ext};
pub(crate) use radiance::RadianceHeaderParams;
pub(crate) use exr::decode_exr_display_image;
pub(crate) use tone_map::{
    bt709_nonlinear_channel_to_linear, decode_transfer_to_display_linear,
    hdr_to_sdr_rgba8, hdr_to_sdr_rgba8_with_tone_settings, hlg_nonlinear_to_scene_linear,
    linear_primary_to_linear_srgb, linear_srgb_linear_to_srgb_u8, pq_nonlinear_to_absolute_nits,
    pq_nonlinear_to_display_linear, srgb_nonlinear_channel_to_linear, validate_hdr_fallback_budget,
};
pub(crate) use paths::{is_exr_path, is_radiance_hdr_path};
pub(crate) use radiance::decode_radiance_hdr_image;
""",
    )

    split_file(
        SRC / "hdr" / "tiled.rs",
        SRC / "hdr" / "tiled",
        [
            ("globals", 17, 34),
            ("buffer", 36, 101),
            ("kind", 103, 149),
            ("source", 151, 301),
            ("preview", 302, 436),
            ("cache", 437, 548),
            ("validate", 550, 596),
        ],
        """mod buffer;
mod cache;
mod globals;
mod kind;
mod preview;
mod source;
mod validate;

#[cfg(test)]
mod tests;

pub(crate) use buffer::HdrTileBuffer;
pub(crate) use cache::{HdrTileCache, configured_hdr_tile_cache_max_bytes, configure_hdr_tile_cache_budget_from_system_memory};
pub(crate) use globals::{HDR_TILE_CACHE_MAX_BYTES, HdrTileCacheKey};
pub(crate) use kind::{HdrTiledSource, HdrTiledSourceKind};
pub(crate) use preview::{
    downsample_hdr_image_nearest, hdr_preview_from_tiled_source_nearest, preview_dimensions,
    preview_sample_coord, sdr_preview_from_hdr_preview,
};
pub(crate) use source::HdrTiledImageSource;
pub(crate) use validate::{validate_rgba32f_len, validate_tile_bounds};
""",
    )

    split_file(
        SRC / "wic.rs",
        SRC / "wic",
        [
            ("imports", 17, 45),
            ("com", 47, 86),
            ("discovery", 87, 197),
            ("tiled_source", 199, 592),
            ("load", 594, 961),
        ],
        """mod com;
mod discovery;
mod imports;
mod load;
mod tiled_source;

pub use crate::formats::{FormatGroup, ImageFormat, get_registry};
pub use com::{ComGuard, init_rayon_with_com};
pub use discovery::spawn_wic_discovery;
pub use load::{load_via_wic, load_via_wic_stream_sniff};
pub use tiled_source::WicTiledSource;
""",
        delete_src=True,
        import_parts={"imports"},
    )

    split_file(
        SRC / "loader" / "decode" / "raw.rs",
        SRC / "loader" / "decode" / "raw",
        [
            ("preview", 43, 124),
            ("develop", 126, 284),
            ("load", 285, 532),
        ],
        """mod develop;
mod load;
mod preview;

#[cfg(test)]
mod tests;

pub(crate) use load::load_raw;
""",
    )

    split_file(
        SRC / "hdr" / "heif_apple_gain_map_compose_simd.rs",
        SRC / "hdr" / "heif_apple_gain_map_compose_simd",
        [
            ("core", 17, 946),
            ("compose", 947, 1086),
        ],
        """mod compose;
mod core;

#[cfg(test)]
mod tests;

pub(crate) use compose::compose_apple_gain_map_pixels;
pub(crate) use core::GainRowLinear;
""",
    )

    split_file(
        SRC / "hdr" / "jpegxl.rs",
        SRC / "hdr" / "jpegxl",
        [
            ("runner", 37, 61),
            ("probe", 62, 140),
            ("decode", [(142, 200), (201, 1916)], None),
        ],
        """mod decode;
mod probe;
mod runner;

#[cfg(test)]
mod tests;

pub(crate) use probe::{is_jxl_header, libjxl_probe_orientation_from_bytes, libjxl_probe_orientation_from_path};
pub(crate) use decode::jxl_color_encoding_to_metadata;
#[cfg(feature = "jpegxl")]
pub(crate) use decode::{
    decode_jxl_bytes_to_image_data, decode_jxl_hdr, decode_jxl_hdr_bytes,
    decode_jxl_hdr_bytes_with_target_capacity, decode_jxl_hdr_with_target_capacity, load_jxl_hdr,
    decode_jxl_gain_map_from_bundle, read_jxl_gain_map_bundle, JxlGainMapBundleRef,
    load_jxl_hdr_with_target_capacity, srgb_unit_to_u8,
};
""",
    )

    split_test_file(SRC / "hdr" / "jpegxl" / "tests.rs", SRC / "hdr" / "jpegxl" / "tests", 450)
    # image_management/tests.rs: keep monolith (chunked split breaks use super::* context)
    split_test_file(SRC / "hdr" / "renderer" / "tests.rs", SRC / "hdr" / "renderer" / "tests", 400)
    split_test_file(SRC / "hdr" / "ultra_hdr" / "tests.rs", SRC / "hdr" / "ultra_hdr" / "tests", 400)


if __name__ == "__main__":
    main()
