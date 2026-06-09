#!/usr/bin/env python3
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
lines = subprocess.check_output(
    ["git", "show", "HEAD:src/libtiff_loader.rs"], cwd=ROOT, text=True, encoding="utf-8"
).splitlines(keepends=True)
cp = "".join(lines[:15])
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
    "use crate::loader::{DecodedImage, ImageData, TiledImageSource};\n\n"
)

for name, start, end in [
    ("mmap.rs", 30, 123),
    ("handle.rs", 125, 182),
    ("tiled.rs", 184, 460),
    ("scanline.rs", 462, 906),
    ("decode.rs", 908, 1794),
    ("load.rs", 1835, 2246),
]:
    body = "".join(lines[start - 1 : end])
    if name == "handle.rs":
        ins = (
            "use libtiff_viewer as lib;\n"
            "use memmap2::Mmap;\n"
            "use std::ffi::CString;\n"
            "use std::os::raw::c_void;\n"
            "use std::path::Path;\n"
            "use std::sync::Arc;\n\n"
            "use super::mmap::{\n"
            "    TiffMmapContext, tiff_close_proc, tiff_map_proc, tiff_read_proc, tiff_seek_proc,\n"
            "    tiff_size_proc, tiff_unmap_proc, tiff_write_proc,\n"
            "};\n\n"
        )
        body = body.replace("fn create_tiff_handle", "pub(crate) fn create_tiff_handle", 1)
        (base / name).write_text(cp + ins + body, encoding="utf-8")
    elif name == "mmap.rs":
        body = body.replace("struct TiffMmapContext", "pub(crate) struct TiffMmapContext", 1)
        for fn in [
            "tiff_read_proc",
            "tiff_write_proc",
            "tiff_seek_proc",
            "tiff_close_proc",
            "tiff_size_proc",
            "tiff_map_proc",
            "tiff_unmap_proc",
        ]:
            body = body.replace(
                f'unsafe extern "C" fn {fn}', f'pub(crate) unsafe extern "C" fn {fn}', 1
            )
        (base / name).write_text(cp + body, encoding="utf-8")
    else:
        text = cp + shared + body
        if name == "tiled.rs":
            text = text.replace("struct LibTiffTiledSource", "pub struct LibTiffTiledSource", 1)
        if name == "scanline.rs":
            text = text.replace("struct LibTiffScanlineSource", "pub struct LibTiffScanlineSource", 1)
        (base / name).write_text(text, encoding="utf-8")

orient = cp + "".join(lines[1795:1833]) + "".join(lines[2247:2285])
orient = orient.replace("fn apply_orientation_buffer_f32", "pub(crate) fn apply_orientation_buffer_f32", 1)
orient = orient.replace("fn apply_orientation_buffer(", "pub(crate) fn apply_orientation_buffer(", 1)
(base / "orientation.rs").write_text(orient, encoding="utf-8")

thumb = cp + "use libtiff_viewer as lib;\n\n" + "".join(lines[209:299])
thumb = thumb.replace("fn extract_embedded_thumbnail", "pub(crate) fn extract_embedded_thumbnail", 1)
(base / "thumbnail.rs").write_text(thumb, encoding="utf-8")
print("rebuild_libtiff ok")
