#!/usr/bin/env python3
"""Fix imports for wave-1 splits and repair tiled/wic/radiance modules."""
from __future__ import annotations

import re
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SRC = ROOT / "src"
MARKER = "// along with this program.  If not, see <https://www.gnu.org/licenses/>.\n\n"
CROSS_IMPORT = (
    "use super::buffer::HdrTileBuffer;\n"
    "use super::cache::{HdrTileCache, configured_hdr_tile_cache_max_bytes};\n"
    "use super::globals::{\n"
    "    DEFAULT_HDR_TILE_CACHE_MAX_BYTES, HDR_TILE_CACHE_MAX_BYTES, HdrTileCacheKey,\n"
    "    MAX_HDR_TILE_CACHE_MAX_BYTES,\n"
    "};\n"
    "use super::kind::{HdrTiledSource, HdrTiledSourceKind};\n"
    "use super::validate::{validate_rgba32f_len, validate_tile_bounds};\n\n"
)


def git_lines(rel: str) -> list[str]:
    return subprocess.check_output(
        ["git", "show", f"HEAD:{rel}"], cwd=ROOT, text=True, encoding="utf-8"
    ).splitlines(keepends=True)


def strip_after_marker(text: str) -> str:
    return text.split(MARKER, 1)[-1]


def remove_self_imports(path: Path) -> None:
    t = path.read_text(encoding="utf-8")
    for line in [
        "use super::buffer::HdrTileBuffer;\n",
        "use super::cache::{HdrTileCache, configured_hdr_tile_cache_max_bytes};\n",
        "use super::cache::HdrTileCache;\n",
    ]:
        t = t.replace(line, "")
    path.write_text(t, encoding="utf-8")


def fix_tiled() -> None:
    mod = SRC / "hdr/tiled/mod.rs"
    t = mod.read_text(encoding="utf-8")
    if "mod globals;" not in t:
        t = t.replace("mod buffer;\n", "mod buffer;\nmod globals;\n", 1)
    mod.write_text(t, encoding="utf-8")

    g = SRC / "hdr/tiled/globals.rs"
    gt = g.read_text(encoding="utf-8")
    for name in [
        "DEFAULT_HDR_TILE_CACHE_MAX_BYTES",
        "MAX_HDR_TILE_CACHE_MAX_BYTES",
        "HDR_TILE_CACHE_MAX_BYTES",
        "NEXT_HDR_TILE_CACHE_ID",
    ]:
        gt = gt.replace(f"const {name}", f"pub(crate) const {name}", 1)
        gt = gt.replace(f"static {name}", f"pub(crate) static {name}", 1)
    gt = re.sub(r"\nstruct HdrTileCacheKey", "\npub(crate) struct HdrTileCacheKey", gt, count=1)
    g.write_text(gt, encoding="utf-8")

    remove_self_imports(SRC / "hdr/tiled/buffer.rs")
    remove_self_imports(SRC / "hdr/tiled/cache.rs")

    for fname in ["source.rs", "preview.rs"]:
        p = SRC / "hdr/tiled" / fname
        rest = strip_after_marker(p.read_text(encoding="utf-8"))
        while rest.startswith("use "):
            rest = rest.split("\n\n", 1)[-1]
        p.write_text(
            p.read_text(encoding="utf-8").split(MARKER)[0] + MARKER + CROSS_IMPORT + rest,
            encoding="utf-8",
        )

    buf = SRC / "hdr/tiled/buffer.rs"
    bt = buf.read_text(encoding="utf-8")
    if "use super::globals::NEXT_HDR_TILE_CACHE_ID" not in bt:
        buf.write_text(bt.replace(MARKER, MARKER + "use super::globals::NEXT_HDR_TILE_CACHE_ID;\n\n", 1), encoding="utf-8")

    for p in (SRC / "hdr/tiled").rglob("*.rs"):
        t = p.read_text(encoding="utf-8").replace("use super::types::", "use crate::hdr::types::")
        p.write_text(t, encoding="utf-8")


def fix_radiance() -> None:
    rle = SRC / "hdr/radiance_tiled/rle.rs"
    t = rle.read_text(encoding="utf-8")
    t = t.replace("use super::rle::decode_radiance_rle_scanline;\n", "")
    t = t.replace("use super::layout::build_radiance_scanline_offsets;\n", "")
    if "use super::layout::Rgbe8Pixel" not in t:
        t = t.replace(MARKER, MARKER + "use super::layout::Rgbe8Pixel;\n\n", 1)
    rle.write_text(t, encoding="utf-8")

    src = SRC / "hdr/radiance_tiled/source.rs"
    st = src.read_text(encoding="utf-8")
    st = st.replace("use super::layout::build_radiance_scanline_offsets;\n", "")
    src.write_text(st, encoding="utf-8")

    td = SRC / "hdr/radiance_tiled/tile_decode.rs"
    tt = td.read_text(encoding="utf-8")
    tt = tt.replace("use super::rle::decode_radiance_rle_scanline;\n", "use super::rle::read_scanline;\n")
    td.write_text(tt, encoding="utf-8")

    hdr = SRC / "hdr/radiance_tiled/header.rs"
    ht = hdr.read_text(encoding="utf-8")
    for fn in [
        "build_radiance_scanline_offsets",
        "read_radiance_header",
        "decode_radiance_rgba32f_from_mmap",
        "validate_scanline_offsets",
    ]:
        ht = ht.replace(f"\nfn {fn}", f"\npub(crate) fn {fn}")
    hdr.write_text(ht, encoding="utf-8")

    mod = SRC / "hdr/radiance_tiled/mod.rs"
    mod.write_text(
        mod.read_text(encoding="utf-8").replace(
            "pub use header::decode_radiance_rgba32f_from_mmap;",
            "pub(crate) use header::decode_radiance_rgba32f_from_mmap;",
        ),
        encoding="utf-8",
    )


def fix_openexr_visibility() -> None:
    chrom = SRC / "hdr/openexr_core/chromaticities.rs"
    ct = chrom.read_text(encoding="utf-8")
    for fn in [
        "chromaticities_looks_like_aces_ap0",
        "hdr_color_space_from_chromaticities_xy",
        "imf_exr_chromaticities_from_path",
        "openexr_luminance_weights_from_chromaticities_xy",
    ]:
        ct = ct.replace(f"\nfn {fn}", f"\npub(crate) fn {fn}", 1)
    chrom.write_text(ct, encoding="utf-8")

    types = SRC / "hdr/openexr_core/types.rs"
    tt = types.read_text(encoding="utf-8")
    for name in [
        "OpenExrCoreDecodedChunkKey",
        "OpenExrCoreDecodedChunk",
        "OpenExrCoreDecodedChunkCache",
    ]:
        tt = tt.replace(f"struct {name}", f"pub(crate) struct {name}", 1)
    types.write_text(tt, encoding="utf-8")


def fix_libtiff() -> None:
    lines = git_lines("src/libtiff_loader.rs")
    cp = "".join(lines[:15])
    base = SRC / "libtiff_loader"

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
    (base / "constants.rs").write_text(constants, encoding="utf-8")

    thumb = cp + "use libtiff_viewer as lib;\n\n" + "".join(lines[209:299]).replace(
        "fn extract_embedded_thumbnail", "pub(crate) fn extract_embedded_thumbnail", 1
    )
    (base / "thumbnail.rs").write_text(thumb, encoding="utf-8")

    mmap = base / "mmap.rs"
    mt = mmap.read_text(encoding="utf-8")
    mt = mt.replace("struct TiffMmapContext", "pub(crate) struct TiffMmapContext", 1)
    for fn in [
        "tiff_read_proc", "tiff_write_proc", "tiff_seek_proc", "tiff_close_proc",
        "tiff_size_proc", "tiff_map_proc", "tiff_unmap_proc",
    ]:
        mt = mt.replace(f"unsafe extern \"C\" fn {fn}", f"pub(crate) unsafe extern \"C\" fn {fn}", 1)
    mmap.write_text(mt, encoding="utf-8")

    for path, reps in [
        ("handle.rs", [("struct TiffHandle", "pub struct TiffHandle")]),
        ("scanline.rs", [("struct LibTiffScanlineSource", "pub struct LibTiffScanlineSource")]),
        ("tiled.rs", [("struct LibTiffTiledSource", "pub struct LibTiffTiledSource")]),
    ]:
        p = base / path
        t = p.read_text(encoding="utf-8")
        for a, b in reps:
            t = t.replace(a, b, 1)
        p.write_text(t, encoding="utf-8")

    shared = (
        "use super::constants::*;\n"
        "use super::handle::TiffHandle;\n"
        "use super::mmap::{TiffMmapContext, tiff_close_proc, tiff_map_proc, tiff_read_proc, tiff_seek_proc, tiff_size_proc, tiff_unmap_proc, tiff_write_proc};\n"
        "use super::orientation::{apply_orientation_buffer, apply_orientation_buffer_f32};\n"
        "use super::thumbnail::extract_embedded_thumbnail;\n"
    )
    for name in ["decode.rs", "load.rs", "scanline.rs", "tiled.rs", "orientation.rs"]:
        p = base / name
        rest = strip_after_marker(p.read_text(encoding="utf-8"))
        while rest.startswith("use libtiff") or rest.startswith("use crate::") or rest.startswith("use std::"):
            rest = rest.split("\n\n", 1)[-1]
        p.write_text(p.read_text(encoding="utf-8").split(MARKER)[0] + MARKER + shared + rest, encoding="utf-8")

    orient = base / "orientation.rs"
    ot = orient.read_text(encoding="utf-8")
    ot = ot.replace("\nfn apply_orientation_buffer", "\npub(crate) fn apply_orientation_buffer", 1)
    ot = ot.replace("\nfn apply_orientation_buffer_f32", "\npub(crate) fn apply_orientation_buffer_f32", 1)
    orient.write_text(ot, encoding="utf-8")

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
    (base / "mod.rs").write_text(mod_rs, encoding="utf-8")

    test_lines = lines[2287:]
    inner = test_lines[2:-1]
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
    (base / "tests.rs").write_text(tests_header + "".join(inner), encoding="utf-8")


def fix_heif_cross_module() -> None:
    base = SRC / "hdr/heif"
    for name in ["session.rs", "orientation.rs", "decode.rs", "ycbcr.rs", "gain_map.rs", "metadata.rs", "load.rs"]:
        p = base / name
        t = p.read_text(encoding="utf-8")
        if "use super::session::" not in t and name != "session.rs":
            extra = (
                "use super::session::{HeifCtxGuard, HeifPrimaryGuard, ensure_heif_ok_lib, heif_error_to_string_lib, open_heif_primary_from_bytes};\n"
                "use super::orientation::{\n"
                "    HeifDecodeOptionsIgnoredGeometryOwned, allocate_decode_options_for_heif_manual_geometry_fixup,\n"
                "    heif_exif_orientation_from_handle, libheif_primary_decode_should_ignore_embedded_geometry,\n"
                "    libheif_transformation_props_to_manual_exif,\n"
                "};\n"
                "use super::metadata::{\n"
                "    apply_heif_transfer_depth_heuristics, heif_metadata_without_embedded_colour_info,\n"
                "    read_heif_metadata, refine_heif_transfer_for_primary_bit_depth,\n"
                "};\n"
                "use super::ycbcr::{HeifYcbcrMatrix, hdr_buffer_from_ycbcr, heif_ycbcr_matrix_from_nclx};\n"
                "use super::gain_map::{align_apple_gain_map_to_primary_display_orientation, decode_heif_gain_map};\n\n"
            )
            if name == "load.rs":
                extra = (
                    "use super::decode::decode_primary_heif_to_hdr;\n"
                    "use super::session::{HeifCtxGuard, HeifPrimaryGuard, open_heif_primary_from_bytes};\n\n"
                )
            elif name == "ycbcr.rs":
                extra = ""
            elif name == "gain_map.rs":
                extra = "use super::orientation::libheif_exif_orientation_tag;\nuse super::session::HeifPrimaryGuard;\n\n"
            elif name == "metadata.rs":
                extra = "use super::brand::heif_nclx_to_metadata;\n\n"
            if extra and extra.strip() not in t:
                t = t.replace(MARKER, MARKER + extra, 1)
        p.write_text(t, encoding="utf-8")

    for fname, symbols in [
        ("session.rs", ["HeifCtxGuard", "HeifPrimaryGuard", "ensure_heif_ok_lib", "heif_error_to_string_lib", "open_heif_primary_from_bytes"]),
        ("orientation.rs", ["HeifDecodeOptionsIgnoredGeometryOwned", "allocate_decode_options_for_heif_manual_geometry_fixup", "heif_exif_orientation_from_handle", "libheif_transformation_props_to_manual_exif"]),
        ("decode.rs", ["decode_primary_heif_to_hdr", "RawHeifImage", "heif_try_decode_into"]),
        ("ycbcr.rs", ["HeifYcbcrMatrix", "hdr_buffer_from_ycbcr", "heif_ycbcr_matrix_from_nclx"]),
        ("gain_map.rs", ["decode_heif_gain_map", "HeifAuxiliaryImageHandle"]),
        ("metadata.rs", ["read_heif_metadata", "heif_metadata_without_embedded_colour_info", "apply_heif_transfer_depth_heuristics", "refine_heif_transfer_for_primary_bit_depth", "inspect_heif_gain_map_auxiliaries", "list_heif_auxiliary_evidence", "heif_sample_bit_depth"]),
    ]:
        p = base / fname
        t = p.read_text(encoding="utf-8")
        for sym in symbols:
            t = t.replace(f"struct {sym}", f"pub(crate) struct {sym}", 1)
            t = t.replace(f"enum {sym}", f"pub(crate) enum {sym}", 1)
            t = t.replace(f"fn {sym}", f"pub(crate) fn {sym}", 1)
        p.write_text(t, encoding="utf-8")


def fix_avif_metadata_ext() -> None:
    p = SRC / "hdr/avif/metadata.rs"
    if p.exists():
        p.write_text(p.read_text(encoding="utf-8").replace("trait AvifMetadataExt", "pub(crate) trait AvifMetadataExt", 1), encoding="utf-8")


def fix_tiled_draw() -> None:
    p = SRC / "app/rendering/tiled/draw.rs"
    t = p.read_text(encoding="utf-8")
    t = t.replace(
        "    TileRequestBudget, TiledPlaneKind, draw_hdr_plane_tile_visit, draw_tile_debug_border,",
        "    TileRequestBudget, TiledPlaneKind, draw_hdr_plane_tile_visit,",
    )
    if '#[cfg(feature = "tile-debug")]' not in t:
        t = t.replace(
            "use std::sync::Arc;\n\nimpl ImageViewerApp",
            'use std::sync::Arc;\n\n#[cfg(feature = "tile-debug")]\nuse super::helpers::draw_tile_debug_border;\n\nimpl ImageViewerApp',
        )
    p.write_text(t, encoding="utf-8")


def dedupe_pub_crate() -> None:
    for p in SRC.rglob("*.rs"):
        t = p.read_text(encoding="utf-8")
        n = t
        while "pub(crate) pub(crate)" in n:
            n = n.replace("pub(crate) pub(crate)", "pub(crate)")
        if n != t:
            p.write_text(n, encoding="utf-8")


def main() -> None:
    dedupe_pub_crate()
    fix_tiled()
    fix_radiance()
    fix_openexr_visibility()
    fix_libtiff()
    fix_heif_cross_module()
    fix_avif_metadata_ext()
    fix_tiled_draw()
    print("wave1 import fixes applied")


if __name__ == "__main__":
    main()
