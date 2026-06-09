#!/usr/bin/env python3
"""Cleanup broken split artifacts."""
from __future__ import annotations

import re
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MARKER = "// along with this program.  If not, see <https://www.gnu.org/licenses/>.\n\n"


def git_lines(rel: str) -> list[str]:
    return subprocess.check_output(
        ["git", "show", f"HEAD:{rel}"], cwd=ROOT, text=True, encoding="utf-8"
    ).splitlines(keepends=True)


def fix_pub_dupes() -> None:
    for p in (ROOT / "src").rglob("*.rs"):
        t = p.read_text(encoding="utf-8")
        n = re.sub(r"\bpub\s+pub\s+", "pub ", t)
        n = re.sub(r"\bpub\s+pub\(crate\)\s+", "pub(crate) ", n)
        n = re.sub(r"\bpub\(crate\)\s+pub\(crate\)\s+", "pub(crate) ", n)
        if n != t:
            p.write_text(n, encoding="utf-8")


def rebuild_libtiff() -> None:
    lines = git_lines("src/libtiff_loader.rs")
    header = "".join(lines[:15])
    base = ROOT / "src/libtiff_loader"
    shared = (
        "use super::constants::*;\n"
        "use libtiff_viewer as lib;\n"
        "use std::ffi::CStr;\n"
        "use std::os::raw::c_void;\n"
        "use std::path::Path;\n"
        "use std::sync::Arc;\n\n"
        "use super::handle::TiffHandle;\n"
        "use super::mmap::{\n"
        "    TiffMmapContext, tiff_close_proc, tiff_map_proc, tiff_read_proc, tiff_seek_proc,\n"
        "    tiff_size_proc, tiff_unmap_proc, tiff_write_proc,\n"
        "};\n"
        "use super::orientation::{apply_orientation_buffer, apply_orientation_buffer_f32};\n"
        "use super::thumbnail::extract_embedded_thumbnail;\n"
        "use crate::loader::{DecodedImage, ImageData, TiledImageSource};\n\n"
    )
    (base / "handle.rs").write_text(header + "".join(lines[124:182]), encoding="utf-8")
    mmap = header + "".join(lines[29:123])
    mmap = mmap.replace("struct TiffMmapContext", "pub(crate) struct TiffMmapContext", 1)
    for fn in [
        "tiff_read_proc", "tiff_write_proc", "tiff_seek_proc", "tiff_close_proc",
        "tiff_size_proc", "tiff_map_proc", "tiff_unmap_proc",
    ]:
        mmap = mmap.replace(f"unsafe extern \"C\" fn {fn}", f"pub(crate) unsafe extern \"C\" fn {fn}", 1)
    (base / "mmap.rs").write_text(mmap, encoding="utf-8")
    for name, start, end in [
        ("scanline.rs", 462, 906),
        ("tiled.rs", 184, 460),
        ("decode.rs", 908, 1794),
        ("load.rs", 1835, 2246),
    ]:
        (base / name).write_text(header + shared + "".join(lines[start - 1 : end]), encoding="utf-8")
    orient = "".join(lines[1795:1833]) + "".join(lines[2247:2285])
    if "pub(crate) fn apply_orientation_buffer_f32" not in orient:
        orient = orient.replace(
            "fn apply_orientation_buffer_f32", "pub(crate) fn apply_orientation_buffer_f32", 1
        )
    if "pub(crate) fn apply_orientation_buffer(" not in orient:
        orient = orient.replace(
            "fn apply_orientation_buffer(", "pub(crate) fn apply_orientation_buffer(", 1
        )
    (base / "orientation.rs").write_text(header + orient, encoding="utf-8")


def fix_radiance() -> None:
    src = ROOT / "src/hdr/radiance_tiled/source.rs"
    rest = src.read_text(encoding="utf-8").split(MARKER, 1)[-1]
    while rest.startswith("use super::") or rest.startswith("use parking_lot") or rest.startswith("use std::"):
        rest = rest.split("\n\n", 1)[-1]
    ins = (
        "use super::header::{build_radiance_scanline_offsets, read_radiance_header};\n"
        "use super::layout::RadianceRasterLayout;\n"
        "use super::tile_decode::{decode_radiance_hdr_preview, decode_radiance_sdr_preview, decode_radiance_tile_window};\n\n"
    )
    src.write_text(src.read_text(encoding="utf-8").split(MARKER)[0] + MARKER + ins + rest, encoding="utf-8")
    hdr = ROOT / "src/hdr/radiance_tiled/header.rs"
    ht = hdr.read_text(encoding="utf-8")
    for fn in [
        "build_radiance_scanline_offsets", "read_radiance_header",
        "decode_radiance_rgba32f_from_mmap", "validate_scanline_offsets",
    ]:
        ht = ht.replace(f"\nfn {fn}", f"\npub(crate) fn {fn}")
    hdr.write_text(ht, encoding="utf-8")
    rle = ROOT / "src/hdr/radiance_tiled/rle.rs"
    rt = rle.read_text(encoding="utf-8")
    rt = re.sub(r"use super::layout::build_radiance_scanline_offsets;\n", "", rt)
    rt = re.sub(r"use super::rle::decode_radiance_rle_scanline;\n", "", rt)
    if "use super::layout::Rgbe8Pixel" not in rt:
        rt = rt.replace(MARKER, MARKER + "use super::layout::Rgbe8Pixel;\n\n", 1)
    for fn in ["read_scanline", "skip_scanline"]:
        rt = rt.replace(f"\nfn {fn}", f"\npub(crate) fn {fn}")
    rle.write_text(rt, encoding="utf-8")


def add_audio_prelude() -> None:
    prelude = (
        "use crate::constants::{\n"
        "    AUDIO_BUFFER_CAPACITY, AUDIO_BUFFER_QUEUE_DEPTH, AUDIO_CHUNK_SIZE, AUDIO_RECOVERY_COOLDOWN,\n"
        "    DEFAULT_CHANNELS, DEFAULT_SAMPLE_RATE, is_supported_music_extension,\n"
        "};\n"
        "use crate::scanner::is_offline;\n"
        "use crossbeam_channel::Sender;\n"
        "use parking_lot::Mutex;\n"
        "use std::collections::{HashSet, VecDeque};\n"
        "use std::fs;\n"
        "use std::path::{Path, PathBuf};\n"
        "use std::sync::Arc;\n"
        "use std::sync::atomic::{AtomicBool, Ordering};\n"
        "use std::time::{Duration, Instant};\n\n"
    )
    base = ROOT / "src/audio"
    for name in ["player.rs", "playlist.rs", "slots.rs", "cue.rs", "loop_state.rs", "run_loop.rs"]:
        p = base / name
        rest = p.read_text(encoding="utf-8").split(MARKER, 1)[-1]
        while rest.startswith("use "):
            rest = rest.split("\n\n", 1)[-1]
        p.write_text(p.read_text(encoding="utf-8").split(MARKER)[0] + MARKER + prelude + rest, encoding="utf-8")


def main() -> None:
    fix_pub_dupes()
    rebuild_libtiff()
    fix_radiance()
    add_audio_prelude()
    print("cleanup_splits done")


if __name__ == "__main__":
    main()
