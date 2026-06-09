#!/usr/bin/env python3
"""Split wave-1 monoliths: libtiff_loader, heif, openexr_core, audio, orchestrator."""

from __future__ import annotations

import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SRC = ROOT / "src"
COPYRIGHT_LEN = 15


def git_lines(rel: str) -> list[str]:
    p = SRC.parent / rel
    if p.exists():
        return p.read_text(encoding="utf-8").splitlines(keepends=True)
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


def split_libtiff_loader() -> None:
    lines = git_lines("src/libtiff_loader.rs")
    base = SRC / "libtiff_loader"
    cp = copyright(lines)

    chunks = [
        ("mmap.rs", 30, 123),
        ("handle.rs", 125, 182),
        ("tiled.rs", 184, 460),
        ("scanline.rs", 462, 906),
        ("decode.rs", 908, 1794),
        ("load.rs", 1835, 2246),
    ]
    for name, start, end in chunks:
        write(base / name, cp, "".join(lines[start - 1 : end]))

    orient = cp + "".join(lines[1796 - 1 : 1833]) + "\n" + "".join(lines[2248 - 1 : 2285])
    (base / "orientation.rs").write_text(orient, encoding="utf-8")

    test_lines = lines[2287:]
    inner = test_lines[1:-1] if test_lines and test_lines[0].strip().startswith("mod tests") else test_lines
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

    # thumbnail.rs from tiled extract_embedded_thumbnail - use lines 210-299 from original tiled section
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
    (SRC / "libtiff_loader.rs").unlink(missing_ok=True)
    print("libtiff_loader/")


def split_orchestrator() -> None:
    lines = git_lines("src/loader/orchestrator.rs")
    base = SRC / "loader" / "orchestrator"
    cp = copyright(lines)
    ts = test_start(lines)

    imports = "".join(lines[15 - 1 : 44]) + "\n"
    write(base / "types.rs", cp, imports + "".join(lines[46 - 1 : 156]))
    imports = "".join(lines[15 - 1 : 44]) + "\nuse super::types::{ImageLoader, TileRequest, should_spawn_load_task};\n\n"
    write(
        base / "load.rs",
        cp,
        imports + "".join(lines[157 - 1 : 1300]) + "}\n",
    )
    write(
        base / "tiles.rs",
        cp,
        imports + "impl ImageLoader {\n" + "".join(lines[1302 - 1 : 1323]) + "}\n",
    )
    write(
        base / "poll.rs",
        cp,
        imports + "impl ImageLoader {\n" + "".join(lines[1324 - 1 : 1440]) + "}\n",
    )

    test_inner = lines[ts + 1 : -1] if lines[ts].strip().startswith("#[cfg(test)]") else lines[ts:]
    write(
        base / "tests.rs",
        cp,
        "use super::*;\n\n" + "".join(test_inner).replace("mod tests {\n", "").rstrip("}\n"),
    )

    imports = """use crate::hdr::types::HdrToneMapSettings;
use crate::loader::LoaderOutput;
use crossbeam_channel::{Receiver, Sender};
use parking_lot::{Condvar, Mutex};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU32;

"""
    new_body = imports + """impl ImageLoader {
    pub fn new() -> Self {
        types::ImageLoader {
            loading: Mutex::new(HashMap::new()),
            current_gen: Mutex::new(HashMap::new()),
            global_generation: AtomicU32::new(0),
            tile_queue: Mutex::new(BinaryHeap::new()),
            refinement_tx: REFINEMENT_POOL.0.clone(),
            deferred_results: Vec::new(),
            delayed_fallback: (Mutex::new(None), Condvar::new()),
            result_rx: LoaderOutput::spawn_receiver(),
            hdr_target_capacity: Mutex::new(1.0),
            hdr_tone_map: Mutex::new(HdrToneMapSettings::default()),
            tiles_in_flight: Mutex::new(HashSet::new()),
        }
    }
"""
    # Fallback: keep struct + new from original slice
    write(base / "mod.rs", cp, "".join(lines[133 - 1 : 157 - 1]) + "\n" + "".join(lines[157 - 1 : ts - 1]))
    # Simpler mod.rs:
    mod_rs = cp + """mod load;
mod poll;
mod tiles;
mod types;

#[cfg(test)]
mod tests;

pub use types::{ImageLoader, TileInFlightKey};

pub(crate) use types::TileInFlightKey as OrchestratorTileKey;
"""
    # Actually use full original mod content approach - single mod re-export
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
    (SRC / "loader" / "orchestrator.rs").unlink(missing_ok=True)
    print("orchestrator/")


def split_openexr_core() -> None:
    lines = git_lines("src/hdr/openexr_core_backend.rs")
    base = SRC / "hdr" / "openexr_core"
    cp = copyright(lines)
    ts = test_start(lines)

    write(base / "chromaticities.rs", cp, "".join(lines[56 - 1 : 267]))
    write(base / "types.rs", cp, "".join(lines[268 - 1 : 414]))
    write(base / "mmap.rs", cp, "".join(lines[415 - 1 : 550]))
    imports = "".join(lines[10 - 1 : 20]) + (
        "use memmap2::Mmap;\n"
        "use openexr_core_sys as sys;\n\n"
        "use super::channels::{\n"
        "    OpenExrCoreChannelInfo, OpenExrCoreChannelChunkLayout, assign_channel_roles, channel_sample_f32,\n"
        "    channel_sample_f32_filtered, copy_channels, decode_pipeline_channels, sampled_channel_flat_index,\n"
        "};\n"
        "use super::chromaticities::{\n"
        "    OpenExrCorePartInfo, OpenExrCoreRgbaTile, chromaticities_looks_like_aces_ap0,\n"
        "    deep_scanline_flatten_rgba_via_imf, extract_rgba32f_tile_from_flat_buffer,\n"
        "    hdr_color_space_from_chromaticities_xy, imf_exr_chromaticities_from_path,\n"
        "    is_luminance_chroma_scanline_part, openexr_luminance_weights_from_chromaticities_xy,\n"
        "    rgba_input_scanline_flatten_rgba_via_imf,\n"
        "};\n"
        "use super::mmap::{OpenExrCoreMmapContext, openexr_memory_map_initializer};\n"
        "use super::types::{\n"
        "    ChannelRole, OpenExrCoreDecodedChunk, OpenExrCoreDecodedChunkCache, OpenExrCoreDecodedChunkKey,\n"
        "};\n\n"
    )
    write(
        base / "read_context.rs",
        cp,
        imports + "".join(lines[39 - 1 : 54]) + "".join(lines[550 - 1 : 1862]),
    )
    write(base / "channels.rs", cp, "".join(lines[1863 - 1 : 1980]))

    test_body = "".join(lines[ts + 2 : -1])
    write(base / "tests.rs", cp, test_body)

    mod_rs = cp + """#![allow(dead_code)]

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
pub(crate) use types::{OpenExrCoreChannelInfo, OpenExrCorePartInfo, OpenExrCoreRgbaTile};
"""
    write(base / "mod.rs", "", mod_rs)

    hdr_mod = (SRC / "hdr" / "mod.rs").read_text(encoding="utf-8")
    hdr_mod = hdr_mod.replace(
        "pub(crate) mod openexr_core_backend;",
        "pub(crate) mod openexr_core;\npub(crate) mod openexr_core_backend {\n    pub(crate) use super::openexr_core::*;\n}",
    )
    (SRC / "hdr" / "mod.rs").write_text(hdr_mod, encoding="utf-8")
    (SRC / "hdr" / "openexr_core_backend.rs").unlink(missing_ok=True)
    print("openexr_core/")


def split_heif() -> None:
    lines = git_lines("src/hdr/heif.rs")
    base = SRC / "hdr" / "heif"
    cp = copyright(lines)
    ts = test_start(lines)
    imp = "".join(lines[16 - 1 : 31])

    write(base / "brand.rs", cp, imp + "".join(lines[32 - 1 : 68]))
    write(base / "session.rs", cp, imp + "".join(lines[70 - 1 : 236]))
    write(base / "orientation.rs", cp, imp + "".join(lines[238 - 1 : 566]))
    write(base / "load.rs", cp, imp + "".join(lines[567 - 1 : 699]))
    write(base / "decode.rs", cp, imp + "".join(lines[700 - 1 : 1297]))
    write(base / "ycbcr.rs", cp, imp + "".join(lines[1298 - 1 : 1602]))
    write(base / "gain_map.rs", cp, imp + "".join(lines[1603 - 1 : 1892]))
    write(base / "metadata.rs", cp, imp + "".join(lines[1895 - 1 : 2154]))

    test_inner = lines[ts + 1 : -1]
    write(base / "tests.rs", cp, imp + "".join(test_inner))

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
    (SRC / "hdr" / "heif.rs").unlink(missing_ok=True)
    print("heif/")


def split_audio() -> None:
    lines = git_lines("src/audio.rs")
    base = SRC / "audio"
    cp = copyright(lines)
    imp = "".join(lines[17 - 1 : 83])

    write(base / "player.rs", cp, imp + "".join(lines[85 - 1 : 340]))
    write(base / "playlist.rs", cp, imp + "".join(lines[342 - 1 : 448]))
    slots_imports = (
        "use parking_lot::Mutex;\n"
        "use std::path::PathBuf;\n"
        "use std::sync::Arc;\n\n"
        "use super::player::AudioError;\n\n"
    )
    slots_body = "".join(lines[457 - 1 : 479])
    for fn in [
        "set_error",
        "set_current_track",
        "set_current_path",
        "set_metadata",
        "set_cue_track",
        "set_cue_markers",
    ]:
        slots_body = slots_body.replace(f"\nfn {fn}", f"\npub(crate) fn {fn}", 1)
    write(base / "slots.rs", cp, slots_imports + slots_body)
    write(base / "cue.rs", cp, "".join(lines[481 - 1 : 688]))
    write(base / "sources" / "ape.rs", cp, imp + "".join(lines[690 - 1 : 918]))
    write(
        base / "sources" / "symphonia.rs",
        cp,
        imp + "".join(lines[920 - 1 : 1306]),
    )
    write(base / "sources" / "mod.rs", cp, "mod ape;\nmod symphonia;\n")
    write(base / "loop_state.rs", cp, imp + "".join(lines[1308 - 1 : 2178]))
    run_imports = (
        "use crossbeam_channel;\n"
        "use parking_lot::Mutex;\n"
        "use std::path::PathBuf;\n"
        "use std::sync::Arc;\n"
        "use std::sync::atomic::AtomicBool;\n"
        "use std::time::Duration;\n\n"
        "use super::loop_state::{AudioLoopState, AudioSlots};\n"
        "use super::player::AudioCommand;\n"
        "use super::slots::{set_current_track, set_metadata};\n\n"
        "#[cfg(windows)]\n"
        "unsafe extern \"C\" {\n"
        "    fn wasapi_monitor_init();\n"
        "    fn wasapi_monitor_uninit();\n"
        "    fn wasapi_poll_device_lost() -> bool;\n"
        "}\n\n"
    )
    run_body = "".join(lines[2180 - 1 : len(lines)]).replace(
        "fn run_audio_loop", "pub(crate) fn run_audio_loop", 1
    )
    write(base / "run_loop.rs", cp, run_imports + run_body)

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
    (SRC / "audio.rs").unlink(missing_ok=True)
    print("audio/")


def main() -> None:
    split_libtiff_loader()
    split_heif()
    split_openexr_core()
    split_audio()
    split_orchestrator()
    print("wave1 splits done — run apply_libtiff_imports.py next")


if __name__ == "__main__":
    main()
