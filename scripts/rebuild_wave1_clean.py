#!/usr/bin/env python3
"""Idempotent wave-1 rebuild from git HEAD + import fixes (no body duplication)."""
from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SRC = ROOT / "src"
MARKER_LF = "// along with this program.  If not, see <https://www.gnu.org/licenses/>.\n\n"
MARKER_CRLF = "// along with this program.  If not, see <https://www.gnu.org/licenses/>.\r\n\r\n"
COPYRIGHT_LEN = 15


def prepend_after_copyright(text: str, imports: str) -> str:
    lines = text.splitlines(keepends=True)
    if len(lines) < COPYRIGHT_LEN:
        return text
    head = "".join(lines[:COPYRIGHT_LEN])
    rest = "".join(lines[COPYRIGHT_LEN:])
    while rest.startswith("use "):
        rest = rest.split("\n\n", 1)[-1]
        if rest.startswith("use "):
            rest = rest.split("\r\n\r\n", 1)[-1]
    return head + imports + rest


def git_lines(rel: str) -> list[str]:
    return subprocess.check_output(
        ["git", "show", f"HEAD:{rel}"], cwd=ROOT, text=True, encoding="utf-8"
    ).splitlines(keepends=True)


def copyright(lines: list[str]) -> str:
    return "".join(lines[:COPYRIGHT_LEN])


def write(path: Path, header: str, body: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(header + body, encoding="utf-8")


def test_start(lines: list[str]) -> int:
    for i, line in enumerate(lines):
        if line.strip() == "#[cfg(test)]" and i + 1 < len(lines) and "mod tests" in lines[i + 1]:
            return i
    return len(lines)


def split_libtiff() -> None:
    lines = git_lines("src/libtiff_loader.rs")
    cp = copyright(lines)
    base = SRC / "libtiff_loader"
    # scanline ends before inline TIFF constants (moved to constants.rs); decode includes manual_decode_scanline.
    for name, start, end in [
        ("mmap.rs", 30, 123),
        ("handle.rs", 125, 182),
        ("tiled.rs", 184, 460),
        ("scanline.rs", 464, 710),
        ("decode.rs", 733, 1794),
        ("load.rs", 1835, 2246),
    ]:
        write(base / name, cp, "".join(lines[start - 1 : end]))
    orient = "".join(lines[1796 - 1 : 1833]) + "\n" + "".join(lines[2248 - 1 : 2285])
    write(base / "orientation.rs", cp, orient)
    test_lines = lines[2287:]
    inner = test_lines[2:-1] if test_lines else []
    tests_header = cp + """use super::constants::*;
use super::decode::{
    tiff_ieee_scene_linear_eligible, tiff_logl_logluv_hdr_eligible,
    tiff_uint16_rgb_scene_linear_eligible,
};
use super::mmap::{
    TiffMmapContext, tiff_close_proc, tiff_map_proc, tiff_read_proc, tiff_seek_proc,
    tiff_size_proc, tiff_unmap_proc, tiff_write_proc,
};
use super::{load_via_libtiff, peek_tiff_tags};
use crate::loader::ImageData;
use libtiff_viewer as lib;
use std::ffi::CStr;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

"""
    write(base / "tests.rs", "", tests_header + "".join(inner))
    constants = cp + """// TIFF Photometric Interpretations
pub(crate) const PHOTO_MINISWHITE: u16 = 0;
pub(crate) const PHOTO_MINISBLACK: u16 = 1;
pub(crate) const PHOTO_RGB: u16 = 2;
pub(crate) const PHOTO_PALETTE: u16 = 3;
pub(crate) const PHOTO_SEPARATED: u16 = 5;
pub(crate) const PHOTO_LOGL: u16 = 32844;
pub(crate) const PHOTO_LOGLUV: u16 = 32845;
pub(crate) const FORMAT_UINT: u16 = 1;
pub(crate) const FORMAT_INT: u16 = 2;
pub(crate) const FORMAT_IEEEFP: u16 = 3;
pub(crate) const CONFIG_CONTIG: u16 = 1;
pub(crate) const CONFIG_SEPARATE: u16 = 2;
#[allow(dead_code)]
pub(crate) const COMPRESSION_THUNDERSCAN: u16 = 32809;
"""
    write(base / "constants.rs", "", constants)
    thumb = cp + "use libtiff_viewer as lib;\n\n" + "".join(lines[210 - 1 : 299]).replace(
        "fn extract_embedded_thumbnail", "pub(crate) fn extract_embedded_thumbnail", 1
    )
    write(base / "thumbnail.rs", "", thumb)
    mod_rs = cp + """mod constants;
mod decode;
mod handle;
mod load;
mod mmap;
mod orientation;
mod scanline;
mod thumbnail;
mod tiled;

#[cfg(test)]
mod tests;

pub use handle::TiffHandle;
pub use load::load_via_libtiff;
pub use scanline::LibTiffScanlineSource;
pub use tiled::LibTiffTiledSource;
pub(crate) use orientation::{apply_orientation_buffer, apply_orientation_buffer_f32};
#[cfg(test)]
pub use load::peek_tiff_tags;
"""
    write(base / "mod.rs", "", mod_rs)


def split_orchestrator() -> None:
    lines = git_lines("src/loader/orchestrator.rs")
    cp = copyright(lines)
    base = SRC / "loader" / "orchestrator"
    ts = test_start(lines)
    prelude = """use crate::hdr::types::HdrToneMapSettings;
use crate::loader::decode::load_image_file;
use crate::loader::preview_caps::{
    REFINEMENT_POOL, finalize_raw_hq_developed_image, finalize_raw_hq_hdr_buffer,
};
use crate::loader::{
    DecodedImage, HdrSdrFallbackResult, LoadResult, LoaderOutput, PreviewBundle, PreviewResult,
    RefinementRequest, TileDecodeSource, TilePixelKind, TileResult,
    hdr_display_requests_sdr_preview, hdr_sdr_fallback_rgba8_eager_or_placeholder,
    hq_preview_max_side, source_key_for_path,
};
use crate::raw_processor::RawProcessor;
use crossbeam_channel::{Receiver, Sender, TryRecvError};
use image::DynamicImage;
use parking_lot::{Condvar, Mutex};
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU32;
use std::time::Duration;

use super::types::{ImageLoader, TileRequest, should_spawn_load_task};

enum EitherDevelop {
    Sdr(DynamicImage),
    Hdr(crate::hdr::types::HdrImageBuffer),
}

"""
    types_prelude = """use crate::hdr::types::HdrToneMapSettings;
use crate::loader::{
    LoaderOutput, RefinementRequest, TileDecodeSource, TilePixelKind,
};
use crossbeam_channel::{Receiver, Sender};
use parking_lot::{Condvar, Mutex};
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU32;
use std::time::Duration;

"""
    write(base / "types.rs", cp, types_prelude + "".join(lines[30 - 1 : 156]))
    write(
        base / "load.rs",
        cp,
        prelude + "impl ImageLoader {\n" + "".join(lines[158 - 1 : 1300]) + "}\n",
    )
    write(
        base / "tiles.rs",
        cp,
        prelude + "impl ImageLoader {\n" + "".join(lines[1302 - 1 : 1322]) + "}\n",
    )
    write(
        base / "poll.rs",
        cp,
        prelude + "impl ImageLoader {\n" + "".join(lines[1324 - 1 : 1440]) + "}\n",
    )
    test_inner = lines[ts + 1 : -1]
    write(base / "tests.rs", cp, "use super::*;\n\n" + "".join(test_inner))
    mod_rs = cp + """mod load;
mod poll;
mod tiles;
mod types;

#[cfg(test)]
mod tests;

pub use types::ImageLoader;
pub(crate) use types::TileInFlightKey;
"""
    write(base / "mod.rs", "", mod_rs)
    types_path = base / "types.rs"
    tt = types_path.read_text(encoding="utf-8")
    tt = tt.replace("struct TileRequest {", "pub(crate) struct TileRequest {", 1)
    tt = tt.replace("\nfn should_spawn_load_task(", "\npub(crate) fn should_spawn_load_task(", 1)
    tt = tt.replace("struct DelayedFallbackJob {", "pub(crate) struct DelayedFallbackJob {", 1)
    types_path.write_text(tt, encoding="utf-8")


def split_openexr() -> None:
    lines = git_lines("src/hdr/openexr_core_backend.rs")
    cp = "".join(lines[:8]) + "\r\n"  # exclude crate-level attrs/imports from submodules
    base = SRC / "hdr" / "openexr_core"
    ts = test_start(lines)
    shared = (
        "use parking_lot::{Condvar, Mutex};\r\n"
        "use std::collections::{HashMap, HashSet, VecDeque};\r\n"
        "use std::ffi::{CStr, CString, c_int, c_void};\r\n"
        "use std::path::{Path, PathBuf};\r\n"
        "use std::ptr;\r\n"
        "use std::sync::{\r\n"
        "    Arc,\r\n"
        "    atomic::{AtomicBool, Ordering},\r\n"
        "};\r\n"
        "use std::time::Instant;\r\n"
        "\r\n"
        "use memmap2::Mmap;\r\n"
        "use openexr_core_sys as sys;\r\n"
        "\r\n"
    )
    write(base / "chromaticities.rs", cp, shared + "".join(lines[56 - 1 : 292]))
    write(base / "types.rs", cp, shared + "".join(lines[295 - 1 : 414]))
    write(base / "mmap.rs", cp, shared + "".join(lines[415 - 1 : 550]))
    read_ctx_imports = (
        shared
        + "use super::channels::{\r\n"
        "    assign_channel_roles, copy_channels, exr_attr_string_to_string, exr_result,\r\n"
        "    OpenExrCoreChannelChunkLayout, OpenExrCoreChunkDecodeTiming, OpenExrCoreDecodedChunkFetch,\r\n"
        "    OpenExrCoreTileGrid,\r\n"
        "};\r\n"
        "use super::chromaticities::{\r\n"
        "    chromaticities_looks_like_aces_ap0, deep_scanline_flatten_rgba_via_imf,\r\n"
        "    extract_rgba32f_tile_from_flat_buffer, hdr_color_space_from_chromaticities_xy,\r\n"
        "    imf_exr_chromaticities_from_path, is_luminance_chroma_scanline_part,\r\n"
        "    openexr_luminance_weights_from_chromaticities_xy, rgba_input_scanline_flatten_rgba_via_imf,\r\n"
        "    OpenExrCoreChannelInfo, OpenExrCorePartInfo, OpenExrCoreRgbaTile,\r\n"
        "};\r\n"
        "use super::mmap::{ExrMmapCookieGuard, openexr_memory_map_initializer};\r\n"
        "use super::types::{\r\n"
        "    OpenExrCoreDecodedChunk, OpenExrCoreDecodedChunkCache, OpenExrCoreDecodedChunkKey,\r\n"
        "};\r\n"
        "\r\n"
    )
    write(base / "read_context.rs", cp, read_ctx_imports + "".join(lines[41 - 1 : 1486]))
    write(base / "channels.rs", cp, shared + "".join(lines[1488 - 1 : ts]))
    write(base / "tests.rs", cp, shared + "".join(lines[ts + 2 : -1]))
    mod_rs = copyright(lines) + """#[allow(dead_code)]

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
    OpenExrCoreChannelInfo, OpenExrCorePartInfo, OpenExrCoreRgbaTile,
};
pub(crate) use read_context::OpenExrCoreReadContext;
pub(crate) use types::{OpenExrCoreDecodedChunk, OpenExrCoreDecodedChunkCache, OpenExrCoreDecodedChunkKey};
"""
    write(base / "mod.rs", "", mod_rs)
    hdr_mod = (SRC / "hdr" / "mod.rs").read_text(encoding="utf-8")
    if "pub(crate) mod openexr_core;" not in hdr_mod:
        hdr_mod = hdr_mod.replace(
            "pub(crate) mod openexr_core_backend;",
            "pub(crate) mod openexr_core;\npub(crate) mod openexr_core_backend {\n    pub(crate) use super::openexr_core::*;\n}",
        )
        (SRC / "hdr" / "mod.rs").write_text(hdr_mod, encoding="utf-8")


def split_heif() -> None:
    lines = git_lines("src/hdr/heif.rs")
    cp = copyright(lines)
    base = SRC / "hdr" / "heif"
    ts = test_start(lines)
    write(base / "brand.rs", cp, "".join(lines[32 - 1 : 68]))
    write(base / "session.rs", cp, "".join(lines[117 - 1 : 237]))
    write(base / "orientation.rs", cp, "".join(lines[238 - 1 : 565]))
    write(base / "load.rs", cp, "".join(lines[567 - 1 : 698]))
    write(base / "decode.rs", cp, "".join(lines[700 - 1 : 1296]))
    write(base / "ycbcr.rs", cp, "".join(lines[1298 - 1 : 1602]))
    write(base / "gain_map.rs", cp, "".join(lines[1603 - 1 : 1892]))
    write(base / "metadata.rs", cp, "".join(lines[1893 - 1 : 2154]))
    write(base / "tests.rs", cp, "".join(lines[ts + 1 : -1]))
    mod_rs = cp + """mod brand;

#[cfg(feature = "heif-native")]
mod decode;
#[cfg(feature = "heif-native")]
mod gain_map;
#[cfg(feature = "heif-native")]
mod load;
#[cfg(feature = "heif-native")]
mod metadata;
#[cfg(feature = "heif-native")]
mod orientation;
#[cfg(feature = "heif-native")]
mod session;
#[cfg(feature = "heif-native")]
mod ycbcr;

#[cfg(test)]
mod tests;

pub(crate) use brand::{heif_nclx_to_metadata, is_heif_brand};

#[cfg(feature = "heif-native")]
pub(crate) use gain_map::align_apple_gain_map_to_primary_display_orientation;
#[cfg(feature = "heif-native")]
pub(crate) use load::{decode_heif_hdr, decode_heif_hdr_bytes, load_heif_hdr};
#[cfg(feature = "heif-native")]
pub(crate) use metadata::{
    classify_heif_auxiliary_type, HeifAuxiliaryClassification, HeifAuxiliaryEvidence,
    apply_heif_unknown_transfer_bt709_primaries_fallback,
};
#[cfg(feature = "heif-native")]
pub(crate) use orientation::{
    HeifDecodeOptionsIgnoredGeometryOwned, decoded_pixels_match_swapped_ispe,
    heif_exif_orientation_from_raw_handle, libheif_exif_orientation_tag,
    libheif_manual_geometry_exif_orientation_from_bytes,
    libheif_manual_geometry_exif_orientation_from_path,
    libheif_primary_decode_should_ignore_embedded_geometry,
    libheif_primary_geometric_mirror_rotation_only,
};
"""
    write(base / "mod.rs", "", mod_rs)


def split_audio() -> None:
    lines = git_lines("src/audio.rs")
    cp = copyright(lines)
    base = SRC / "audio"
    write(base / "player.rs", cp, "".join(lines[85 - 1 : 340]))
    write(base / "playlist.rs", cp, "".join(lines[342 - 1 : 448]))
    write(base / "slots.rs", cp, "".join(lines[453 - 1 : 479]))
    write(base / "cue.rs", cp, "".join(lines[481 - 1 : 689]))
    write(base / "sources" / "ape.rs", cp, "".join(lines[690 - 1 : 918]))
    write(
        base / "sources" / "symphonia.rs",
        cp,
        "".join(lines[17 - 1 : 83]) + "".join(lines[920 - 1 : 1306]),
    )
    write(base / "sources" / "mod.rs", cp, "mod ape;\nmod symphonia;\n")
    write(base / "loop_state.rs", cp, "".join(lines[1308 - 1 : 2178]))
    write(base / "run_loop.rs", cp, "".join(lines[2180 - 1 : len(lines)]))
    mod_rs = cp + """mod cue;
mod loop_state;
mod player;
mod playlist;
mod run_loop;
mod slots;
mod sources;

#[cfg(test)]
mod tests;

pub use player::{AudioCommand, AudioError, AudioPlayer};
pub use playlist::collect_music_files;
"""
    write(base / "mod.rs", "", mod_rs)


def fix_libtiff_imports() -> None:
    base = SRC / "libtiff_loader"
    for name in ["handle.rs", "mmap.rs"]:
        p = base / name
        t = p.read_text(encoding="utf-8")
        if "use libtiff_viewer as lib" not in t:
            p.write_text(prepend_after_copyright(t, "use libtiff_viewer as lib;\r\n\r\n"), encoding="utf-8")
    mmap = base / "mmap.rs"
    mt = mmap.read_text(encoding="utf-8")
    mt = mt.replace("struct TiffMmapContext", "pub(crate) struct TiffMmapContext", 1)
    for fn in [
        "tiff_read_proc", "tiff_write_proc", "tiff_seek_proc", "tiff_close_proc",
        "tiff_size_proc", "tiff_map_proc", "tiff_unmap_proc",
    ]:
        mt = mt.replace(f"unsafe extern \"C\" fn {fn}", f"pub(crate) unsafe extern \"C\" fn {fn}", 1)
    mmap.write_text(mt, encoding="utf-8")
    for path, rep in [
        ("handle.rs", ("struct TiffHandle", "pub struct TiffHandle")),
        ("scanline.rs", ("struct LibTiffScanlineSource", "pub struct LibTiffScanlineSource")),
        ("tiled.rs", ("struct LibTiffTiledSource", "pub struct LibTiffTiledSource")),
    ]:
        p = base / path
        t = p.read_text(encoding="utf-8")
        if rep[1] not in t:
            t = t.replace(rep[0], rep[1], 1)
        p.write_text(t, encoding="utf-8")
    shared = (
        "use super::constants::*;\r\n"
        "use libtiff_viewer as lib;\r\n"
        "use std::ffi::CStr;\r\n"
        "use std::os::raw::c_void;\r\n"
        "use std::path::Path;\r\n"
        "use std::sync::Arc;\r\n"
        "\r\n"
        "use super::handle::TiffHandle;\r\n"
        "use super::mmap::{\r\n"
        "    TiffMmapContext, tiff_close_proc, tiff_map_proc, tiff_read_proc, tiff_seek_proc,\r\n"
        "    tiff_size_proc, tiff_unmap_proc, tiff_write_proc,\r\n"
        "};\r\n"
        "use super::orientation::{apply_orientation_buffer, apply_orientation_buffer_f32};\r\n"
        "use crate::loader::{DecodedImage, ImageData, TiledImageSource};\r\n"
        "\r\n"
    )
    scanline_shared = shared + "use super::thumbnail::extract_embedded_thumbnail;\r\n\r\n"
    decode_shared = shared
    load_shared = shared + "use super::decode::manual_decode_scanline;\r\nuse super::scanline::LibTiffScanlineSource;\r\nuse super::tiled::LibTiffTiledSource;\r\n\r\n"

    def prepend_imports(name: str, imports: str) -> None:
        p = base / name
        p.write_text(prepend_after_copyright(p.read_text(encoding="utf-8"), imports), encoding="utf-8")

    prepend_imports("scanline.rs", scanline_shared)
    prepend_imports("decode.rs", decode_shared)
    prepend_imports("load.rs", load_shared)
    prepend_imports("tiled.rs", shared)

    orient = base / "orientation.rs"
    ot = orient.read_text(encoding="utf-8")
    ot = ot.replace("\nfn apply_orientation_buffer(", "\npub(crate) fn apply_orientation_buffer(", 1)
    ot = ot.replace("\nfn apply_orientation_buffer_f32", "\npub(crate) fn apply_orientation_buffer_f32", 1)
    ot = ot.replace("\r\nfn apply_orientation_buffer(", "\r\npub(crate) fn apply_orientation_buffer(", 1)
    ot = ot.replace("\r\nfn apply_orientation_buffer_f32", "\r\npub(crate) fn apply_orientation_buffer_f32", 1)
    orient.write_text(ot, encoding="utf-8")

    dec = base / "decode.rs"
    dt = dec.read_text(encoding="utf-8")
    dt = dt.replace("unsafe fn manual_decode_scanline", "pub(crate) unsafe fn manual_decode_scanline", 1)
    dec.write_text(dt, encoding="utf-8")


def fix_openexr_visibility() -> None:
    base = SRC / "hdr/openexr_core"
    chrom = base / "chromaticities.rs"
    ct = chrom.read_text(encoding="utf-8")
    for fn in [
        "chromaticities_looks_like_aces_ap0",
        "hdr_color_space_from_chromaticities_xy",
        "imf_exr_chromaticities_from_path",
        "openexr_luminance_weights_from_chromaticities_xy",
    ]:
        ct = re.sub(rf"(?m)^(?!\s*pub(?:\(crate\)|\s))fn {re.escape(fn)}\b", f"pub(crate) fn {fn}", ct, count=1)
    chrom.write_text(ct, encoding="utf-8")
    types = base / "types.rs"
    tt = types.read_text(encoding="utf-8")
    for name in ["OpenExrCoreDecodedChunkKey", "OpenExrCoreDecodedChunk", "OpenExrCoreDecodedChunkCache"]:
        tt = re.sub(rf"(?m)^(?!\s*pub(?:\(crate\)|\s))struct {re.escape(name)}\b", f"pub(crate) struct {name}", tt, count=1)
    types.write_text(tt, encoding="utf-8")
    rc = base / "read_context.rs"
    rt = rc.read_text(encoding="utf-8")
    rt = re.sub(r"(?m)^struct OpenExrCoreReadContext\b", "pub(crate) struct OpenExrCoreReadContext", rt, count=1)
    rc.write_text(rt, encoding="utf-8")


def fix_audio_modules() -> None:
    base = SRC / "audio"
    prelude = (
        "use crate::constants::{\r\n"
        "    AUDIO_BUFFER_CAPACITY, AUDIO_BUFFER_QUEUE_DEPTH, AUDIO_CHUNK_SIZE, AUDIO_RECOVERY_COOLDOWN,\r\n"
        "    DEFAULT_CHANNELS, DEFAULT_SAMPLE_RATE, is_supported_music_extension,\r\n"
        "};\r\n"
        "use crate::scanner::is_offline;\r\n"
        "use crossbeam_channel::Sender;\r\n"
        "use parking_lot::Mutex;\r\n"
        "use std::collections::{HashSet, VecDeque};\r\n"
        "use std::fs;\r\n"
        "use std::path::{Path, PathBuf};\r\n"
        "use std::sync::Arc;\r\n"
        "use std::sync::atomic::{AtomicBool, Ordering};\r\n"
        "use std::time::{Duration, Instant};\r\n"
        "\r\n"
    )
    extras = {
        "loop_state.rs": prelude
        + "use super::cue::{load_cue, CueSheet};\r\n"
        + "use super::player::AudioError;\r\n"
        + "use super::sources::symphonia::{get_file_metadata, open_source};\r\n"
        + "\r\n",
        "run_loop.rs": prelude
        + "use super::loop_state::{AudioLoopState, AudioSlots};\r\n"
        + "use super::player::{AudioCommand, AudioError};\r\n"
        + "\r\n",
        "cue.rs": prelude,
        "player.rs": prelude + "use super::run_loop::run_audio_loop;\r\n\r\n",
        "playlist.rs": prelude,
        "slots.rs": prelude,
    }
    for name, imports in extras.items():
        p = base / name
        p.write_text(prepend_after_copyright(p.read_text(encoding="utf-8"), imports), encoding="utf-8")
    for name, sym in [("cue.rs", "load_cue"), ("loop_state.rs", "AudioSlots"), ("loop_state.rs", "AudioLoopState")]:
        p = base / name
        t = p.read_text(encoding="utf-8")
        t = t.replace(f"\r\nfn {sym}", f"\r\npub(crate) fn {sym}", 1)
        t = t.replace(f"\r\nstruct {sym}", f"\r\npub(crate) struct {sym}", 1)
        p.write_text(t, encoding="utf-8")
    rt = (base / "run_loop.rs").read_text(encoding="utf-8")
    if "pub(crate) fn run_audio_loop" not in rt:
        (base / "run_loop.rs").write_text(rt.replace("fn run_audio_loop", "pub(crate) fn run_audio_loop", 1), encoding="utf-8")
    sym_prelude = (
        "use crate::constants::{\r\n"
        "    AUDIO_BUFFER_CAPACITY, AUDIO_BUFFER_QUEUE_DEPTH, AUDIO_CHUNK_SIZE, AUDIO_RECOVERY_COOLDOWN,\r\n"
        "    DEFAULT_CHANNELS, DEFAULT_SAMPLE_RATE, is_supported_music_extension,\r\n"
        "};\r\n"
        "use std::path::{Path, PathBuf};\r\n"
        "use std::sync::Arc;\r\n"
        "use std::sync::atomic::{AtomicBool, Ordering};\r\n"
        "use std::time::{Duration, Instant};\r\n\r\n"
    )
    sym = base / "sources/symphonia.rs"
    st = prepend_after_copyright(sym.read_text(encoding="utf-8"), sym_prelude)
    for fn in ["get_file_metadata", "create_source", "open_source"]:
        st = st.replace(f"\r\nfn {fn}", f"\r\npub(crate) fn {fn}", 1)
        st = st.replace(f"\nfn {fn}", f"\npub(crate) fn {fn}", 1)
    for fn in [
        "wasapi_monitor_init",
        "wasapi_monitor_uninit",
        "wasapi_is_device_available",
        "wasapi_poll_device_lost",
    ]:
        st = st.replace(f"\nunsafe fn {fn}", f"\npub(crate) unsafe fn {fn}", 1)
        st = st.replace(f"\r\nunsafe fn {fn}", f"\r\npub(crate) unsafe fn {fn}", 1)
    if "use super::ape::ApeSource" not in st:
        st = prepend_after_copyright(st, "use super::ape::ApeSource;\r\n\r\n")
    sym.write_text(st, encoding="utf-8")
    ape = base / "sources/ape.rs"
    ape.write_text(
        prepend_after_copyright(
            ape.read_text(encoding="utf-8").replace("struct ApeSource", "pub(crate) struct ApeSource", 1),
            sym_prelude,
        ),
        encoding="utf-8",
    )


def fix_heif_modules() -> None:
    import re

    base = SRC / "hdr/heif"
    heif_prelude = (
        "use crate::hdr::cicp::{self, H273_TRANSFER_ITU_BT709, H273_TRANSFER_SMPTE170M};\r\n"
        "use crate::hdr::types::{\r\n"
        "    HdrColorProfile, HdrImageMetadata, HdrLuminanceMetadata, HdrReference, HdrTransferFunction,\r\n"
        "};\r\n"
        "#[cfg(feature = \"heif-native\")]\r\n"
        "use crate::hdr::types::{HdrGainMapMetadata, HdrImageBuffer, HdrPixelFormat, HdrToneMapSettings};\r\n"
        "#[cfg(feature = \"heif-native\")]\r\n"
        "use std::ffi::CStr;\r\n"
        "#[cfg(feature = \"heif-native\")]\r\n"
        "use std::path::Path;\r\n"
        "#[cfg(feature = \"heif-native\")]\r\n"
        "use std::sync::Arc;\r\n"
        "#[cfg(feature = \"heif-native\")]\r\n"
        "use std::sync::OnceLock;\r\n"
        "\r\n"
    )
    cross = {
        "orientation.rs": ["use super::session::{HeifCtxGuard, HeifPrimaryGuard, open_heif_primary_from_bytes};\r\n"],
        "load.rs": [
            "use super::decode::decode_primary_heif_to_hdr;\r\n",
            "use super::session::{HeifCtxGuard, HeifPrimaryGuard, open_heif_primary_from_bytes};\r\n",
        ],
        "decode.rs": [
            "use super::gain_map::decode_heif_gain_map;\r\n",
            "use super::metadata::read_heif_metadata;\r\n",
            "use super::orientation::HeifDecodeOptionsIgnoredGeometryOwned;\r\n",
            "use super::session::{HeifCtxGuard, HeifPrimaryGuard, ensure_heif_ok_lib, heif_error_to_string_lib, open_heif_primary_from_bytes};\r\n",
            "use super::ycbcr::{HeifYcbcrMatrix, hdr_buffer_from_ycbcr, heif_ycbcr_matrix_from_nclx};\r\n",
        ],
        "gain_map.rs": [
            "use super::orientation::libheif_exif_orientation_tag;\r\n",
            "use super::session::HeifPrimaryGuard;\r\n",
        ],
        "metadata.rs": ["use super::brand::heif_nclx_to_metadata;\r\n"],
    }
    pub_syms = {
        "session.rs": ["HeifCtxGuard", "HeifPrimaryGuard", "ensure_heif_ok_lib", "heif_error_to_string_lib", "open_heif_primary_from_bytes"],
        "orientation.rs": ["HeifDecodeOptionsIgnoredGeometryOwned", "allocate_decode_options_for_heif_manual_geometry_fixup", "heif_exif_orientation_from_handle", "libheif_transformation_props_to_manual_exif"],
        "decode.rs": ["decode_primary_heif_to_hdr", "RawHeifImage", "heif_try_decode_into"],
        "ycbcr.rs": ["HeifYcbcrMatrix", "hdr_buffer_from_ycbcr", "heif_ycbcr_matrix_from_nclx"],
        "gain_map.rs": ["decode_heif_gain_map", "HeifAuxiliaryImageHandle"],
        "metadata.rs": ["read_heif_metadata", "heif_metadata_without_embedded_colour_info", "apply_heif_transfer_depth_heuristics", "refine_heif_transfer_for_primary_bit_depth", "inspect_heif_gain_map_auxiliaries", "list_heif_auxiliary_evidence", "heif_sample_bit_depth"],
    }

    def make_pub(t: str, sym: str) -> str:
        t = re.sub(rf"(?m)^struct {re.escape(sym)}\b", f"pub(crate) struct {sym}", t, count=1)
        t = re.sub(rf"(?m)^enum {re.escape(sym)}\b", f"pub(crate) enum {sym}", t, count=1)
        t = re.sub(rf"(?m)^(?!\s*pub(?:\(crate\)|\s))fn {re.escape(sym)}\b", f"pub(crate) fn {sym}", t, count=1)
        return t

    brand_prelude = (
        "use crate::hdr::cicp::{self, H273_TRANSFER_ITU_BT709, H273_TRANSFER_SMPTE170M};\r\n"
        "use crate::hdr::types::{\r\n"
        "    HdrColorProfile, HdrImageMetadata, HdrLuminanceMetadata, HdrReference, HdrTransferFunction,\r\n"
        "};\r\n\r\n"
    )
    all_heif = [
        "brand.rs",
        "session.rs",
        "orientation.rs",
        "load.rs",
        "decode.rs",
        "ycbcr.rs",
        "gain_map.rs",
        "metadata.rs",
    ]
    for fname in all_heif:
        p = base / fname
        imports = brand_prelude if fname == "brand.rs" else heif_prelude + "".join(cross.get(fname, []))
        t = prepend_after_copyright(p.read_text(encoding="utf-8"), imports)
        for sym in pub_syms.get(fname, []):
            t = make_pub(t, sym)
        p.write_text(t, encoding="utf-8")


def dedupe_import_blocks() -> None:
    """Remove repeated contiguous `use ...` blocks introduced by CRLF marker mismatch."""
    for p in SRC.rglob("*.rs"):
        lines = p.read_text(encoding="utf-8").splitlines(keepends=True)
        out: list[str] = []
        seen_blocks: set[str] = set()
        i = 0
        while i < len(lines):
            if lines[i].startswith("use "):
                block: list[str] = []
                j = i
                while j < len(lines) and (lines[j].startswith("use ") or lines[j].strip() == ""):
                    block.append(lines[j])
                    j += 1
                key = "".join(block)
                if key not in seen_blocks:
                    seen_blocks.add(key)
                    out.extend(block)
                i = j
            else:
                out.append(lines[i])
                i += 1
        text = "".join(out)
        if text != "".join(lines):
            p.write_text(text, encoding="utf-8")
    for p in SRC.rglob("*.rs"):
        t = p.read_text(encoding="utf-8")
        n = t
        n = re.sub(r"\bpub\s+pub\s+", "pub ", n)
        n = re.sub(r"\bpub\(crate\)\s+pub\(crate\)\s+", "pub(crate) ", n)
        n = re.sub(r"\bpub\(super\)\s+pub\(super\)\s+", "pub(super) ", n)
        if n != t:
            p.write_text(n, encoding="utf-8")


def main() -> None:
    print("rebuilding wave1 from git HEAD...")
    split_libtiff()
    split_heif()
    split_openexr()
    split_audio()
    split_orchestrator()
    fix_libtiff_imports()
    fix_openexr_visibility()
    fix_audio_modules()
    fix_heif_modules()
    sp = ROOT / "scripts" / "fix_openexr_exports.py"
    if sp.exists():
        subprocess.run([sys.executable, str(sp)], cwd=ROOT, check=False)
    print("wave1 clean rebuild done")


if __name__ == "__main__":
    main()
