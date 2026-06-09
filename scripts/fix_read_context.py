#!/usr/bin/env python3
"""Rebuild openexr read_context with correct imports."""
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
lines = subprocess.check_output(
    ["git", "show", "HEAD:src/hdr/openexr_core_backend.rs"], cwd=ROOT, text=True, encoding="utf-8"
).splitlines(keepends=True)

header = "".join(lines[:15])
imports = """use parking_lot::{Condvar, Mutex};
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::{CStr, CString, c_int, c_void};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Instant;

use memmap2::Mmap;
use openexr_core_sys as sys;

use super::channels::{
    assign_channel_roles, copy_channels, decode_chunk_rgba, exr_attr_string_to_string, exr_result,
    extract_rgba_from_pipeline, storage_label, ChannelRole, OpenExrCoreChannelChunkLayout,
    OpenExrCoreChunkDecodeTiming, OpenExrCoreDecodedChunkFetch, OpenExrCoreTileGrid,
};
use super::chromaticities::{
    chromaticities_looks_like_aces_ap0, deep_scanline_flatten_rgba_via_imf,
    extract_rgba32f_tile_from_flat_buffer, hdr_color_space_from_chromaticities_xy,
    imf_exr_chromaticities_from_path, is_luminance_chroma_scanline_part,
    openexr_luminance_weights_from_chromaticities_xy, rgba_input_scanline_flatten_rgba_via_imf,
    OpenExrCoreChannelInfo, OpenExrCorePartInfo, OpenExrCoreRgbaTile,
};
use super::mmap::{OpenExrCoreMmapContext, openexr_memory_map_initializer};
use super::types::{
    OpenExrCoreDecodedChunk, OpenExrCoreDecodedChunkCache, OpenExrCoreDecodedChunkKey,
};

"""
body = "".join(lines[40:1486])  # struct OpenExrCoreReadContext through impl closing brace
out = header + imports + body
path = ROOT / "src/hdr/openexr_core/read_context.rs"
path.write_text(out, encoding="utf-8")
print("delta", out.count("{") - out.count("}"))
