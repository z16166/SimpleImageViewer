#!/usr/bin/env python3
"""Correct openexr_core split boundaries and module preludes."""
from __future__ import annotations

import re
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
BASE = ROOT / "src/hdr/openexr_core"
CP_END = 8  # openexr monolith: copyright through blank line (before #![allow])


def git_lines(rel: str) -> list[str]:
    return subprocess.check_output(
        ["git", "show", f"HEAD:{rel}"], cwd=ROOT, text=True, encoding="utf-8"
    ).splitlines(keepends=True)


def cp(lines: list[str]) -> str:
    return "".join(lines[:CP_END])


def write(path: Path, header: str, body: str) -> None:
    path.write_text(header + body, encoding="utf-8")


def main() -> None:
    lines = git_lines("src/hdr/openexr_core_backend.rs")
    header = cp(lines)

    chrom_prelude = (
        "#![allow(dead_code)]\n\n"
        "use std::ffi::{CStr, c_void};\n"
        "use std::path::Path;\n"
        "use std::ptr;\n\n"
        "use memmap2::Mmap;\n"
        "use openexr_core_sys as sys;\n\n"
        "use super::types::{OpenExrCorePartInfo, OpenExrCoreRgbaTile};\n\n"
    )
    write(BASE / "chromaticities.rs", header, chrom_prelude + "".join(lines[56 - 1 : 267]))

    types_prelude = (
        "#![allow(dead_code)]\n\n"
        "use parking_lot::Mutex;\n"
        "use std::collections::{HashMap, HashSet, VecDeque};\n"
        "use std::sync::Arc;\n\n"
    )
    write(BASE / "types.rs", header, types_prelude + "".join(lines[269 - 1 : 414]))

    mmap_prelude = (
        "#![allow(dead_code)]\n\n"
        "use std::ffi::{c_int, c_void};\n"
        "use std::sync::{\n"
        "    Arc,\n"
        "    atomic::{AtomicBool, Ordering},\n"
        "};\n\n"
        "use memmap2::Mmap;\n"
        "use openexr_core_sys as sys;\n\n"
    )
    mmap_body = "".join(lines[415 - 1 : 549])
    mmap_body = mmap_body.replace("fn openexr_memory_map_initializer", "pub(crate) fn openexr_memory_map_initializer", 1)
    mmap_body = mmap_body.replace("struct ExrMmapReadCookie", "pub(crate) struct ExrMmapReadCookie", 1)
    mmap_body = mmap_body.replace("struct ExrMmapCookieGuard", "pub(crate) struct ExrMmapCookieGuard", 1)
    mmap_body = re.sub(r"(?m)^    fn new\(", "    pub(crate) fn new(", mmap_body, count=1)
    mmap_body = re.sub(r"(?m)^    fn mark_context_alive\(", "    pub(crate) fn mark_context_alive(", mmap_body, count=1)
    write(BASE / "mmap.rs", header, mmap_prelude + mmap_body)

    rc_prelude = (
        "#![allow(dead_code)]\n\n"
        "use parking_lot::{Condvar, Mutex};\n"
        "use std::collections::{HashMap, HashSet, VecDeque};\n"
        "use std::ffi::{CStr, CString, c_int, c_void};\n"
        "use std::path::{Path, PathBuf};\n"
        "use std::ptr;\n"
        "use std::sync::{\n"
        "    Arc,\n"
        "    atomic::{AtomicBool, Ordering},\n"
        "};\n"
        "use std::time::Instant;\n\n"
        "use openexr_core_sys as sys;\n\n"
        "use super::channels::{\n"
        "    OpenExrCoreChannelChunkLayout, OpenExrCoreChunkDecodeTiming, OpenExrCoreDecodedChunkFetch,\n"
        "    OpenExrCoreTileGrid, DecodePipelineGuard, assign_channel_roles, budgeted_scanline_preview_source_y,\n"
        "    channel_sample_f32, channel_sample_f32_filtered, compression_name, configured_decoded_chunk_cache_max_bytes,\n"
        "    copy_channels, copy_decoded_chunk_to_tile, decode_pipeline_channels, decoded_chunk_key, exr_result,\n"
        "    extent_from_window_axis, sample_decoded_scanline_chunk_into_preview, sampled_channel_flat_index,\n"
        "    scanline_preview_decode_parallelism, scanline_preview_dimensions,\n"
        "    scanline_preview_source_row_budget, storage_name, validate_tile_bounds,\n"
        "};\n"
        "use super::chromaticities::{\n"
        "    deep_scanline_flatten_rgba_via_imf, extract_rgba32f_tile_from_flat_buffer,\n"
        "    hdr_color_space_from_chromaticities_xy, imf_exr_chromaticities_from_path,\n"
        "    is_luminance_chroma_scanline_part, openexr_luminance_weights_from_chromaticities_xy,\n"
        "    rgba_input_scanline_flatten_rgba_via_imf,\n"
        "};\n"
        "use super::mmap::{ExrMmapCookieGuard, openexr_memory_map_initializer};\n"
        "use super::types::{\n"
        "    ChannelRole, OpenExrCoreChannelInfo, OpenExrCoreDecodedChunk, OpenExrCoreDecodedChunkCache,\n"
        "    OpenExrCoreDecodedChunkKey, OpenExrCorePartInfo, OpenExrCoreRgbaTile,\n"
        "};\n\n"
    )
    write(
        BASE / "read_context.rs",
        header,
        rc_prelude + "".join(lines[39 - 1 : 54]) + "".join(lines[551 - 1 : 1486]),
    )

    ch_prelude = (
        "#![allow(dead_code)]\n\n"
        "use parking_lot::Mutex;\n"
        "use std::collections::{HashMap, HashSet, VecDeque};\n"
        "use std::ffi::{CStr, c_void};\n"
        "use std::sync::Arc;\n\n"
        "use openexr_core_sys as sys;\n\n"
        "use super::types::{\n"
        "    ChannelRole, OpenExrCoreChannelInfo, OpenExrCoreDecodedChunk, OpenExrCoreDecodedChunkCache,\n"
        "    OpenExrCoreDecodedChunkKey,\n"
        "};\n"
        "use super::{DEFAULT_DECODED_CHUNK_CACHE_BYTES, MAX_DECODED_CHUNK_CACHE_BYTES};\n\n"
    )
    ch_body = "".join(lines[1488 - 1 : 1980])
    for sym in [
        "OpenExrCoreChunkDecodeTiming",
        "OpenExrCoreDecodedChunkFetch",
        "OpenExrCoreTileGrid",
        "DecodePipelineGuard",
        "OpenExrCoreChannelChunkLayout",
    ]:
        ch_body = ch_body.replace(f"struct {sym}", f"pub(crate) struct {sym}", 1)
    for fn in [
        "extent_from_window_axis",
        "validate_tile_bounds",
        "decoded_chunk_key",
        "copy_decoded_chunk_to_tile",
        "sample_decoded_scanline_chunk_into_preview",
        "scanline_preview_dimensions",
        "scanline_preview_source_row_budget",
        "scanline_preview_decode_parallelism",
        "budgeted_scanline_preview_source_y",
        "sampled_channel_flat_index",
        "channel_sample_f32",
        "channel_sample_f32_filtered",
        "decode_pipeline_channels",
        "storage_name",
        "compression_name",
        "assign_channel_roles",
        "copy_channels",
        "exr_attr_string_to_string",
        "exr_result",
    ]:
        ch_body = re.sub(rf"(?m)^fn {re.escape(fn)}\b", f"pub(crate) fn {fn}", ch_body, count=1)
    ch_body = re.sub(
        r"(?m)^fn configured_decoded_chunk_cache_max_bytes\b",
        "pub(crate) fn configured_decoded_chunk_cache_max_bytes",
        ch_body,
        count=1,
    )
    write(BASE / "channels.rs", header, ch_prelude + ch_body)

    types_path = BASE / "types.rs"
    tt = types_path.read_text(encoding="utf-8")
    for name in ["OpenExrCoreDecodedChunk"]:
        tt = tt.replace(f"struct {name}", f"pub(crate) struct {name}", 1)
    types_path.write_text(tt, encoding="utf-8")

    chrom_path = BASE / "chromaticities.rs"
    ct = chrom_path.read_text(encoding="utf-8")
    for fn in [
        "imf_exr_chromaticities_from_path",
        "hdr_color_space_from_chromaticities_xy",
        "openexr_luminance_weights_from_chromaticities_xy",
        "chromaticities_looks_like_aces_ap0",
    ]:
        ct = re.sub(rf"(?m)^fn {re.escape(fn)}\b", f"pub(crate) fn {fn}", ct, count=1)
    chrom_path.write_text(ct, encoding="utf-8")

    mod_rs = header + """#![allow(dead_code)]

mod channels;
mod chromaticities;
mod mmap;
mod read_context;
mod types;

#[cfg(test)]
mod tests;

pub(crate) const DEFAULT_DECODED_CHUNK_CACHE_BYTES: usize = 512 * 1024 * 1024;
pub(crate) const MAX_DECODED_CHUNK_CACHE_BYTES: usize = 4 * 1024 * 1024 * 1024;
pub(crate) const SCANLINE_BOOTSTRAP_PREVIEW_MAX_SIDE: u32 = 1024;
pub(crate) const SCANLINE_BOOTSTRAP_PREVIEW_SOURCE_ROW_BUDGET: u32 = 192;
pub(crate) const SCANLINE_REFINED_PREVIEW_SOURCE_ROW_BUDGET: u32 = 0;

pub(crate) use chromaticities::{
    chromaticities_looks_like_aces_ap0, deep_scanline_flatten_rgba_via_imf,
    extract_rgba32f_tile_from_flat_buffer, hdr_color_space_from_chromaticities_xy,
    imf_exr_chromaticities_from_path, is_luminance_chroma_scanline_part,
    openexr_luminance_weights_from_chromaticities_xy, rgba_input_scanline_flatten_rgba_via_imf,
};
pub(crate) use read_context::OpenExrCoreReadContext;
pub(crate) use types::{
    OpenExrCoreChannelInfo, OpenExrCorePartInfo, OpenExrCoreRgbaTile,
    OpenExrCoreDecodedChunk, OpenExrCoreDecodedChunkCache, OpenExrCoreDecodedChunkKey,
};
"""
    (BASE / "mod.rs").write_text(mod_rs, encoding="utf-8")
    print("fix_openexr_resplit ok")


if __name__ == "__main__":
    main()
