#!/usr/bin/env python3
"""One-shot: split src/loader/decode/mod.rs into submodules."""
from pathlib import Path


def lines_of(p: Path) -> list[str]:
    return p.read_text(encoding="utf-8").splitlines(keepends=True)


def slice_lines(lines: list[str], lo: int, hi: int) -> str:
    """1-based inclusive line numbers."""
    return "".join(lines[lo - 1 : hi])


def main():
    decode_dir = Path("src/loader/decode")
    mod_path = decode_dir / "mod.rs"
    lines = lines_of(mod_path)

    assemble_body_raw = slice_lines(lines, 1113, 1197) + slice_lines(lines, 2779, 2924)
    assemble_pub = assemble_body_raw
    assemble_pub = assemble_pub.replace("fn make_image_data", "pub(crate) fn make_image_data", 1)
    assemble_pub = assemble_pub.replace("fn make_hdr_image_data(", "pub(crate) fn make_hdr_image_data(", 1)
    assemble_pub = assemble_pub.replace(
        "fn make_hdr_image_data_for_limit(", "pub(crate) fn make_hdr_image_data_for_limit(", 1
    )
    assemble_pub = assemble_pub.replace("pub struct MemoryImageSource", "pub(crate) struct MemoryImageSource", 1)
    assemble_pub = assemble_pub.replace(
        "struct HdrSdrTiledFallbackSource", "pub(crate) struct HdrSdrTiledFallbackSource", 1
    )

    hdr_raw = slice_lines(lines, 690, 891)
    hdr_pub = hdr_raw
    for old, rep in (
        ("fn is_exr_path", "pub(crate) fn is_exr_path"),
        ("fn load_hdr(", "pub(crate) fn load_hdr("),
        ("fn load_detected_exr", "pub(crate) fn load_detected_exr"),
        ("fn try_load_disk_backed_exr_hdr", "pub(crate) fn try_load_disk_backed_exr_hdr"),
        ("fn exr_tiled_source_to_static_hdr", "pub(crate) fn exr_tiled_source_to_static_hdr"),
        ("fn try_load_disk_backed_radiance_hdr", "pub(crate) fn try_load_disk_backed_radiance_hdr"),
        ("fn is_exr_disk_backed_probe_fallback_error", "pub(crate) fn is_exr_disk_backed_probe_fallback_error"),
        ("fn is_exr_deep_data_unsupported_error", "pub(crate) fn is_exr_deep_data_unsupported_error"),
        ("fn load_deep_exr(", "pub(crate) fn load_deep_exr("),
        ("fn make_deep_exr_placeholder", "pub(crate) fn make_deep_exr_placeholder"),
    ):
        hdr_pub = hdr_pub.replace(old, rep, 1)

    jpeg_body = slice_lines(lines, 408, 490)

    modern_raw = slice_lines(lines, 521, 688)
    modern_pub = modern_raw
    for old, rep in (
        ("#[allow(dead_code)]\nfn is_avif_path", "#[allow(dead_code)]\npub(crate) fn is_avif_path"),
        ("#[allow(dead_code)]\nfn is_heif_path", "#[allow(dead_code)]\npub(crate) fn is_heif_path"),
        ("#[allow(dead_code)]\nfn is_jxl_path", "#[allow(dead_code)]\npub(crate) fn is_jxl_path"),
        ("fn is_hdr_capable_modern_format_path", "pub(crate) fn is_hdr_capable_modern_format_path"),
        ("fn load_avif_with_target_capacity", "pub(crate) fn load_avif_with_target_capacity"),
        ("fn load_jxl_with_target_capacity", "pub(crate) fn load_jxl_with_target_capacity"),
        ("fn load_heif_hdr_aware", "pub(crate) fn load_heif_hdr_aware"),
    ):
        modern_pub = modern_pub.replace(old, rep, 1)

    raster_raw = slice_lines(lines, 494, 519) + slice_lines(lines, 893, 1108)
    raster_pub = raster_raw.replace("fn load_static", "pub(crate) fn load_static", 1)
    raster_pub = raster_pub.replace("fn process_animation_frames", "pub(crate) fn process_animation_frames", 1)
    raster_pub = raster_pub.replace("fn load_gif", "pub(crate) fn load_gif", 1)
    raster_pub = raster_pub.replace("fn load_png", "pub(crate) fn load_png", 1)
    raster_pub = raster_pub.replace("fn load_webp", "pub(crate) fn load_webp", 1)
    raster_pub = raster_pub.replace("fn load_psd", "pub(crate) fn load_psd", 1)
    raster_pub = raster_pub.replace("fn is_maybe_animated", "pub(crate) fn is_maybe_animated", 1)

    detect_raw = slice_lines(lines, 2297, 2403)
    detect_pub = detect_raw.replace("fn load_by_image_format", "pub(crate) fn load_by_image_format", 1).replace(
        "fn load_via_content_detection", "pub(crate) fn load_via_content_detection", 1
    )

    raw_raw = slice_lines(lines, 2404, 2777).replace(
        "pub struct RawImageSource", "pub(crate) struct RawImageSource", 1
    ).replace("pub fn new", "pub(crate) fn new", 1)
    raw_pub = raw_raw.replace("fn load_raw(", "pub(crate) fn load_raw(", 1)

    gpl_hdr = "// Simple Image Viewer - A high-performance, cross-platform image viewer\n"
    gpl = (
        gpl_hdr
        + "// Copyright (C) 2024-2026 Simple Image Viewer Contributors\n"
        "//\n"
        "// This program is free software: you can redistribute it and/or modify\n"
        "// it under the terms of the GNU General Public License as published by\n"
        "// the Free Software Foundation, either version 3 of the License, or\n"
        "// (at your option) any later version.\n"
        "//\n"
        "// This program is distributed in the hope that it will be useful,\n"
        "// but WITHOUT ANY WARRANTY; without even the implied warranty of\n"
        "// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the\n"
        "// GNU General Public License for more details.\n"
        "//\n"
        "// You should have received a copy of the GNU General Public License\n"
        "// along with this program.  If not, see <https://www.gnu.org/licenses/>.\n\n"
    )

    (decode_dir / "assemble.rs").write_text(
        gpl
        + "//! HDR/SDR assembly and in-memory tiled sources.\n\n"
        + "use crate::hdr::tiled::{HdrTiledImageSource, HdrTiledSource};\n"
        + "use crate::hdr::types::HdrToneMapSettings;\n"
        + "use crate::loader::{hdr_to_sdr_with_user_tone, AnimationFrame, DecodedImage, ImageData, TiledImageSource};\n"
        + "use std::sync::Arc;\n\n"
        + assemble_pub,
        encoding="utf-8",
    )

    (decode_dir / "jpeg.rs").write_text(
        gpl
        + "//! Baseline JPEG and Ultra HDR (JPEG_R).\n\n"
        + "use crate::hdr::types::HdrToneMapSettings;\n"
        + "use crate::loader::{hdr_gain_map_decode_capacity, hdr_sdr_fallback_rgba8_eager_or_placeholder};\n"
        + "use crate::loader::{DecodedImage, ImageData};\n"
        + "use std::path::PathBuf;\n"
        + "use std::sync::Arc;\n\n"
        + "use super::assemble::{make_hdr_image_data, make_image_data, MemoryImageSource};\n\n"
        + jpeg_body,
        encoding="utf-8",
    )

    (decode_dir / "modern.rs").write_text(
        gpl + "//! AVIF, JPEG XL, HEIF/HIF loaders.\n\n"
        + "use crate::hdr::types::HdrToneMapSettings;\n"
        + "use crate::loader::{\n"
        + "    apply_exif_orientation_to_hdr_pair, apply_exif_orientation_to_image_data,\n"
        + "    hdr_gain_map_decode_capacity, hdr_sdr_fallback_rgba8_eager_or_placeholder,\n"
        + "    DecodedImage, ImageData,\n"
        + "};\n"
        + "use std::path::{Path, PathBuf};\n\n"
        + "use super::assemble::make_hdr_image_data;\n\n"
        + modern_pub,
        encoding="utf-8",
    )

    (decode_dir / "hdr_formats.rs").write_text(
        gpl + "//! Radiance `.hdr`, OpenEXR routing, disk-backed probing.\n\n"
        + "use crate::hdr::types::HdrToneMapSettings;\n"
        + "use crate::loader::{\n"
        + "    apply_exif_orientation_to_hdr_pair, hdr_sdr_fallback_rgba8_eager_or_placeholder,\n"
        + "    DecodedImage, ImageData, TiledImageSource,\n"
        + "};\n"
        + "use std::path::Path;\n"
        + "use std::sync::Arc;\n\n"
        + "use super::assemble::{make_hdr_image_data, HdrSdrTiledFallbackSource, MemoryImageSource};\n\n"
        + hdr_pub,
        encoding="utf-8",
    )

    (decode_dir / "raster.rs").write_text(
        gpl
        + "//! GIF / PNG / WebP / PSD and static raster via `image`.\n\n"
        + "use crate::constants::{DEFAULT_ANIMATION_DELAY_MS, MIN_ANIMATION_DELAY_THRESHOLD_MS};\n"
        + "use crate::hdr::types::HdrToneMapSettings;\n"
        + "use crate::loader::{apply_exif_orientation_to_image_data, AnimationFrame, DecodedImage, ImageData};\n"
        + "use std::path::PathBuf;\n"
        + "use std::time::Duration;\n\n"
        + "use super::assemble::make_image_data;\n"
        + "use super::hdr_formats::{is_exr_path, load_hdr};\n\n"
        + raster_pub,
        encoding="utf-8",
    )

    (decode_dir / "detect.rs").write_text(
        gpl + "//! Content sniffing.\n\n"
        + "use crate::hdr::types::HdrToneMapSettings;\n"
        + "use crate::loader::ImageData;\n"
        + "use std::path::PathBuf;\n\n"
        + "use super::hdr_formats::{load_detected_exr, load_hdr};\n"
        + "use super::jpeg::load_jpeg_with_target_capacity;\n"
        + "use super::modern::{load_avif_with_target_capacity, load_heif_hdr_aware, load_jxl_with_target_capacity};\n"
        + "use super::raster::{load_gif, load_png, load_static, load_webp};\n\n"
        + detect_pub,
        encoding="utf-8",
    )

    (decode_dir / "raw.rs").write_text(
        gpl + "//! LibRAW and raw tiled refinement.\n\n"
        + "use crate::constants::RGBA_CHANNELS;\n"
        + "use crate::hdr::types::HdrToneMapSettings;\n"
        + "use crate::loader::{hq_preview_max_side, hdr_sdr_fallback_rgba8_eager_or_placeholder};\n"
        + "use crate::loader::{DecodedImage, ImageData, RefinementRequest, TiledImageSource};\n"
        + "use crate::raw_processor::RawProcessor;\n"
        + "use crossbeam_channel::Sender;\n"
        + "use image::{DynamicImage, GenericImageView};\n"
        + "use parking_lot::RwLock as PLRwLock;\n"
        + "use std::path::PathBuf;\n"
        + "use std::sync::Arc;\n\n"
        + "use super::assemble::{make_hdr_image_data, make_image_data, MemoryImageSource};\n\n"
        + raw_pub,
        encoding="utf-8",
    )

    imports_block = "".join(lines[18:45])
    tests_body = "".join(lines[1198:])

    new_mod = (
        gpl
        + "//! Decode pipeline (`load_image_file`) and submodule graph.\n\n"
        + "mod assemble;\nmod detect;\nmod hdr_formats;\nmod jpeg;\nmod modern;\nmod raster;\nmod raw;\n\n"
        + imports_block
        + "\npub(crate) use assemble::{MemoryImageSource, make_hdr_image_data, make_hdr_image_data_for_limit, "
        + "make_image_data};\n"
        + "#[cfg(test)] pub(crate) use jpeg::load_jpeg;\n"
        + "pub(crate) use modern::{is_avif_path, is_heif_path, is_hdr_capable_modern_format_path, is_jxl_path};\n"
        + "pub(crate) use raster::is_maybe_animated;\n"
        + "\n"
        + "use detect::load_via_content_detection;\n"
        + "use hdr_formats::load_hdr;\n"
        + "use jpeg::load_jpeg_with_target_capacity;\n"
        + "use modern::{load_avif_with_target_capacity, load_heif_hdr_aware, load_jxl_with_target_capacity};\n"
        + "use raster::{load_gif, load_png, load_psd, load_static, load_webp};\n"
        + "use raw::load_raw;\n"
        + "\n"
        + slice_lines(lines, 46, 388)
        + slice_lines(lines, 390, 406)
        + "\n"
        + tests_body
    )

    mod_path.write_text(new_mod, encoding="utf-8")
    print("split decode modules written")


if __name__ == "__main__":
    main()
