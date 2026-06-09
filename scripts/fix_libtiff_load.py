#!/usr/bin/env python3
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MARKER = "// along with this program.  If not, see <https://www.gnu.org/licenses/>.\n"
base = ROOT / "src/libtiff_loader"

MMAP_IMPORTS = (
    "use libtiff_viewer as lib;\n"
    "use memmap2::Mmap;\n"
    "use std::ffi::c_void;\n"
    "use std::sync::Arc;\n\n"
)

SCANLINE_EXTRA = (
    "use memmap2::Mmap;\n"
    "use parking_lot::Mutex;\n"
    "use std::path::PathBuf;\n\n"
    "use super::handle::create_tiff_handle;\n"
    "use super::thumbnail::extract_embedded_thumbnail;\n"
    "use super::decode::{get_raw_value, process_scanline_contig, process_scanline_separate};\n\n"
)


def strip_use_block(text: str) -> str:
    if MARKER not in text:
        return text
    head, rest = text.split(MARKER, 1)
    while rest.lstrip("\n").startswith("use "):
        rest = rest.lstrip("\n")
        if "\n\n" in rest:
            _, rest = rest.split("\n\n", 1)
        else:
            break
    if not rest.startswith("\n"):
        rest = "\n" + rest
    return head + MARKER + rest


def main() -> None:
    mmap = base / "mmap.rs"
    body = strip_use_block(mmap.read_text(encoding="utf-8")).split(MARKER, 1)[1]
    mmap.write_text(MARKER + MMAP_IMPORTS + body.lstrip("\n"), encoding="utf-8")

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
        + SCANLINE_EXTRA
    )
    for name in ("scanline.rs", "tiled.rs"):
        p = base / name
        body = strip_use_block(p.read_text(encoding="utf-8")).split(MARKER, 1)[1]
        p.write_text(MARKER + shared + body.lstrip("\n"), encoding="utf-8")

    decode = base / "decode.rs"
    dt = decode.read_text(encoding="utf-8")
    for fn in ["get_raw_value", "process_scanline_contig", "process_scanline_separate"]:
        if f"pub(crate) fn {fn}" not in dt:
            dt = dt.replace(f"\nfn {fn}", f"\npub(crate) fn {fn}", 1)
    decode.write_text(dt, encoding="utf-8")

    load = base / "load.rs"
    extra = (
        "use parking_lot::Mutex;\n\n"
        "use crate::hdr::decode::hdr_to_sdr_rgba8_with_tone_settings;\n"
        "use super::decode::try_camera_tiff_rgb8_hdr_upgrade;\n"
        "use super::scanline::LibTiffScanlineSource;\n"
        "use super::tiled::LibTiffTiledSource;\n\n"
    )
    lt = load.read_text(encoding="utf-8")
    if "LibTiffTiledSource" not in lt.split("fn load_tiff")[0]:
        lt = lt.replace(MARKER, MARKER + extra, 1)
    load.write_text(lt, encoding="utf-8")

    print("fix_libtiff_load ok")


if __name__ == "__main__":
    main()
