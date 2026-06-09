#!/usr/bin/env python3
"""Split large Rust modules into subdirectories."""

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


def slice_lines(lines: list[str], start: int, end: int) -> str:
    return "".join(lines[start - 1 : end])


def write_module(path: Path, body: str, include_copyright: bool = True) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    prefix = COPYRIGHT if include_copyright else ""
    path.write_text(prefix + body, encoding="utf-8")


def split_radiance_tiled() -> None:
    src = SRC / "hdr" / "radiance_tiled.rs"
    if not src.exists():
        return
    lines = read_lines(src)

    dst = SRC / "hdr" / "radiance_tiled"
    write_module(dst / "layout.rs", slice_lines(lines, 17, 303))
    write_module(dst / "source.rs", slice_lines(lines, 305, 447))
    write_module(dst / "tile_decode.rs", slice_lines(lines, 449, 664))
    write_module(dst / "header.rs", slice_lines(lines, 666, 860))
    write_module(dst / "rle.rs", slice_lines(lines, 862, 1079))
    write_module(dst / "tests.rs", slice_lines(lines, 1081, len(lines)), include_copyright=False)

    mod_rs = """mod header;
mod layout;
mod rle;
mod source;
mod tile_decode;

#[cfg(test)]
mod tests;

pub use header::decode_radiance_rgba32f_from_mmap;
pub use source::RadianceHdrTiledImageSource;
"""
    write_module(dst / "mod.rs", mod_rs)
    src.unlink()


def split_monitor() -> None:
    src = SRC / "hdr" / "monitor.rs"
    if not src.exists():
        return
    dst = SRC / "hdr" / "monitor"
    lines = read_lines(src)

    write_module(dst / "types.rs", slice_lines(lines, 17, 132))
    write_module(dst / "state.rs", slice_lines(lines, 134, 288))
    write_module(dst / "effective.rs", slice_lines(lines, 289, 380))
    write_module(
        dst / "macos.rs",
        slice_lines(lines, 381, 416) + slice_lines(lines, 903, 1034),
    )
    write_module(
        dst / "windows.rs",
        slice_lines(lines, 27, 52) + slice_lines(lines, 417, 730),
    )
    write_module(dst / "probe.rs", slice_lines(lines, 731, 902))
    write_module(dst / "tests.rs", slice_lines(lines, 1036, len(lines)), include_copyright=False)

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

pub use effective::{
    active_monitor_hdr_status, any_active_output_supports_hdr, effective_capability_output_mode,
    effective_monitor_selection, effective_render_output_mode,
};
pub use probe::{spawn_monitor_hdr_status, SpawnMonitorHdrProbe};
pub use state::HdrMonitorState;
pub use types::{HdrMonitorSelection, HdrMonitorSignature, HdrNativeSurfaceEncoding};
"""
    write_module(dst / "mod.rs", mod_rs)
    src.unlink()


def split_hdr_decode() -> None:
    src = SRC / "hdr" / "decode.rs"
    if not src.exists():
        return
    dst = SRC / "hdr" / "decode"
    lines = read_lines(src)

    write_module(dst / "constants.rs", slice_lines(lines, 17, 41))
    write_module(dst / "decode_image.rs", slice_lines(lines, 43, 82))
    write_module(dst / "radiance.rs", slice_lines(lines, 84, 186))
    write_module(dst / "exr.rs", slice_lines(lines, 188, 202))
    write_module(dst / "paths.rs", slice_lines(lines, 204, 221))
    write_module(dst / "tone_map.rs", slice_lines(lines, 223, 557))
    write_module(dst / "tests.rs", slice_lines(lines, 559, len(lines)), include_copyright=False)

    mod_rs = """mod constants;
mod decode_image;
mod exr;
mod paths;
mod radiance;
mod tone_map;

#[cfg(test)]
mod tests;

pub use decode_image::decode_hdr_image;
pub use paths::is_hdr_candidate_ext;
pub use radiance::RadianceHeaderParams;
pub use exr::decode_exr_display_image;
pub use tone_map::{
    bt709_nonlinear_channel_to_linear, decode_transfer_to_display_linear,
    hdr_to_sdr_rgba8, hdr_to_sdr_rgba8_with_tone_settings, hlg_nonlinear_to_scene_linear,
    linear_primary_to_linear_srgb, linear_srgb_linear_to_srgb_u8, pq_nonlinear_to_absolute_nits,
    pq_nonlinear_to_display_linear, srgb_nonlinear_channel_to_linear, validate_hdr_fallback_budget,
};

pub(crate) use paths::{is_exr_path, is_radiance_hdr_path};
pub(crate) use radiance::decode_radiance_hdr_image;
"""
    write_module(dst / "mod.rs", mod_rs)
    src.unlink()


def split_tiled() -> None:
    src = SRC / "hdr" / "tiled.rs"
    if not src.exists():
        return
    dst = SRC / "hdr" / "tiled"
    lines = read_lines(src)

    write_module(dst / "buffer.rs", slice_lines(lines, 17, 33) + slice_lines(lines, 36, 103))
    write_module(dst / "kind.rs", slice_lines(lines, 104, 151))
    write_module(dst / "source.rs", slice_lines(lines, 152, 301))
    write_module(dst / "preview.rs", slice_lines(lines, 302, 436))
    write_module(dst / "cache.rs", slice_lines(lines, 437, 566))
    write_module(dst / "validate.rs", slice_lines(lines, 567, 596))
    write_module(dst / "tests.rs", slice_lines(lines, 598, len(lines)), include_copyright=False)

    mod_rs = """mod buffer;
mod cache;
mod kind;
mod preview;
mod source;
mod validate;

#[cfg(test)]
mod tests;

pub use buffer::HdrTileBuffer;
pub use cache::{
    configured_hdr_tile_cache_max_bytes, configure_hdr_tile_cache_budget_from_system_memory,
    HdrTileCache, HdrTileCacheKey, HDR_TILE_CACHE_MAX_BYTES,
};
pub use kind::HdrTiledSourceKind;
pub use preview::{
    downsample_hdr_image_nearest, hdr_preview_from_tiled_source_nearest,
    preview_dimensions, preview_sample_coord, sdr_preview_from_hdr_preview,
};
pub use source::{HdrTiledImageSource, HdrTiledSource};
pub use validate::validate_tile_bounds;
"""
    write_module(dst / "mod.rs", mod_rs)
    src.unlink()


def split_wic() -> None:
    src = SRC / "wic.rs"
    if not src.exists():
        return
    dst = SRC / "wic"
    lines = read_lines(src)

    write_module(dst / "factory.rs", slice_lines(lines, 17, 45))
    write_module(dst / "com.rs", slice_lines(lines, 42, 86))
    write_module(dst / "discovery.rs", slice_lines(lines, 87, 200))
    write_module(dst / "tiled_source.rs", slice_lines(lines, 42, 45) + slice_lines(lines, 201, 592))
    write_module(dst / "load.rs", slice_lines(lines, 42, 45) + slice_lines(lines, 594, len(lines)))

    mod_rs = """mod com;
mod discovery;
mod factory;
mod load;
mod tiled_source;

pub use crate::formats::{FormatGroup, ImageFormat, get_registry};
pub use com::{ComGuard, init_rayon_with_com};
pub use discovery::spawn_wic_discovery;
pub use load::{load_via_wic, load_via_wic_stream_sniff};
pub use tiled_source::WicTiledSource;
"""
    write_module(dst / "mod.rs", mod_rs)
    src.unlink()


def split_raw() -> None:
    src = SRC / "loader" / "decode" / "raw.rs"
    if not src.exists():
        return
    dst = SRC / "loader" / "decode" / "raw"
    lines = read_lines(src)

    write_module(dst / "preview.rs", slice_lines(lines, 17, 126))
    write_module(dst / "develop.rs", slice_lines(lines, 127, 284))
    write_module(dst / "load.rs", slice_lines(lines, 285, len(lines)))
    # tests embedded in load.rs at end - extract if present
    load_path = dst / "load.rs"
    load_text = load_path.read_text(encoding="utf-8")
    if "#[cfg(test)]" in load_text:
        idx = load_text.index("#[cfg(test)]")
        tests_body = load_text[idx:]
        load_path.write_text(load_text[:idx], encoding="utf-8")
        write_module(dst / "tests.rs", tests_body, include_copyright=False)

    mod_rs = """mod develop;
mod load;
mod preview;

#[cfg(test)]
mod tests;

pub(crate) use load::load_raw;
"""
    write_module(dst / "mod.rs", mod_rs)
    src.unlink()


def split_jpegxl_decode() -> None:
    src = SRC / "hdr" / "jpegxl" / "decode.rs"
    if not src.exists():
        return
    dst = SRC / "hdr" / "jpegxl" / "decode"
    lines = read_lines(src)

    write_module(dst / "runner.rs", slice_lines(lines, 17, 63))
    write_module(dst / "entry.rs", slice_lines(lines, 64, 268))
    write_module(dst / "color.rs", slice_lines(lines, 269, 338))
    write_module(dst / "frame.rs", slice_lines(lines, 339, 502))
    write_module(dst / "pipeline.rs", slice_lines(lines, 503, 1085))
    write_module(dst / "helpers.rs", slice_lines(lines, 1086, len(lines)))

    mod_rs = """mod color;
mod entry;
mod frame;
mod helpers;
mod pipeline;
mod runner;

pub use entry::{
    decode_jxl_bytes_to_image_data, decode_jxl_hdr, decode_jxl_hdr_bytes,
    decode_jxl_hdr_bytes_with_target_capacity, decode_jxl_hdr_with_target_capacity, load_jxl_hdr,
    load_jxl_hdr_with_target_capacity, srgb_unit_to_u8,
};
pub use helpers::{
    jxl_find_black_extra_channel_index, jxl_sdr_grade_fallback_rgba8,
    jxl_tag_display_referred_when_sdr_grade, linear_to_srgb_u8,
};
"""
    write_module(dst / "mod.rs", mod_rs)
    src.unlink()


def split_read_context() -> None:
    src = SRC / "hdr" / "openexr_core" / "read_context.rs"
    if not src.exists():
        return
    dst = SRC / "hdr" / "openexr_core" / "read_context"
    lines = read_lines(src)

    write_module(
        dst / "context.rs",
        slice_lines(lines, 17, 60)
        + "impl OpenExrCoreReadContext {\n"
        + slice_lines(lines, 62, 210)
        + "}\n",
    )
    write_module(
        dst / "scanline.rs",
        slice_lines(lines, 17, 44)
        + "impl OpenExrCoreReadContext {\n"
        + slice_lines(lines, 211, 593)
        + "}\n",
    )
    write_module(
        dst / "tiles.rs",
        slice_lines(lines, 17, 44)
        + slice_lines(lines, 999, 1015)
        + "impl OpenExrCoreReadContext {\n"
        + slice_lines(lines, 594, 998)
        + "}\n"
        + slice_lines(lines, 1016, 1023),
    )

    mod_rs = """mod context;
mod scanline;
mod tiles;

pub use context::OpenExrCoreReadContext;
"""
    write_module(dst / "mod.rs", mod_rs)
    src.unlink()


def split_heif_compose_simd() -> None:
    src = SRC / "hdr" / "heif_apple_gain_map_compose_simd.rs"
    if not src.exists():
        return
    dst = SRC / "hdr" / "heif_apple_gain_map_compose_simd"
    lines = read_lines(src)

    write_module(dst / "core.rs", slice_lines(lines, 17, 946))
    write_module(dst / "compose.rs", slice_lines(lines, 947, 1086))
    write_module(dst / "tests.rs", slice_lines(lines, 1087, len(lines)), include_copyright=False)

    mod_rs = """mod compose;
mod core;

#[cfg(test)]
mod tests;

pub(crate) use compose::compose_apple_gain_map_pixels;
pub(crate) use core::GainRowLinear;
"""
    write_module(dst / "mod.rs", mod_rs)
    src.unlink()


def split_test_file(src: Path, dst_dir: Path, chunk_lines: int = 450) -> None:
    if not src.exists():
        return
    lines = read_lines(src)
    body_start = 1
    for i, line in enumerate(lines):
        stripped = line.strip()
        if stripped == "#[cfg(test)]":
            body_start = i + 1
            break
        if stripped.startswith("mod tests"):
            body_start = i + 1
            break

    content_lines = lines[body_start:]
    if content_lines and content_lines[0].strip() == "{":
        content_lines = content_lines[1:]
    if content_lines and content_lines[-1].strip() == "}":
        content_lines = content_lines[:-1]

    chunks: list[tuple[str, list[str]]] = []
    current: list[str] = []
    part = 1
    for line in content_lines:
        current.append(line)
        if line.startswith("#[test]") and len(current) > chunk_lines:
            chunks.append((f"part{part}", current))
            part += 1
            current = ["use super::*;\n", "\n"]
    if current:
        if not current[0].startswith("use "):
            current.insert(0, "use super::*;\n\n")
        chunks.append((f"part{part}", current))

    if len(chunks) <= 1 and len(content_lines) > chunk_lines:
        chunks = []
        part = 1
        for i in range(0, len(content_lines), chunk_lines):
            chunk = content_lines[i : i + chunk_lines]
            if chunk and not chunk[0].startswith("use "):
                chunk.insert(0, "use super::*;\n\n")
            chunks.append((f"part{part}", chunk))
            part += 1

    dst_dir.mkdir(parents=True, exist_ok=True)
    mod_names = []
    for name, chunk in chunks:
        write_module(dst_dir / f"{name}.rs", "".join(chunk), include_copyright=False)
        mod_names.append(name)

    mod_decl = "\n".join(f"mod {n};" for n in mod_names)
    write_module(dst_dir / "mod.rs", f"{mod_decl}\n")
    src.unlink()


def main() -> None:
    split_radiance_tiled()
    split_monitor()
    split_hdr_decode()
    split_tiled()
    split_wic()
    split_raw()
    split_jpegxl_decode()
    split_read_context()
    split_heif_compose_simd()

    split_test_file(SRC / "hdr" / "jpegxl" / "tests.rs", SRC / "hdr" / "jpegxl" / "tests", 450)
    split_test_file(
        SRC / "app" / "image_management" / "tests.rs",
        SRC / "app" / "image_management" / "tests",
        450,
    )
    split_test_file(SRC / "hdr" / "renderer" / "tests.rs", SRC / "hdr" / "renderer" / "tests", 400)
    split_test_file(SRC / "hdr" / "ultra_hdr" / "tests.rs", SRC / "hdr" / "ultra_hdr" / "tests", 400)

    print("Split complete.")


if __name__ == "__main__":
    main()
