#!/usr/bin/env python3
"""Second-round fixes after safe pipeline."""
from __future__ import annotations

import re
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MARKER = "// along with this program.  If not, see <https://www.gnu.org/licenses/>.\n"


def git_lines(rel: str) -> list[str]:
    return subprocess.check_output(
        ["git", "show", f"HEAD:{rel}"], cwd=ROOT, text=True, encoding="utf-8"
    ).splitlines(keepends=True)


def pub_fn(path: Path, name: str) -> None:
    t = path.read_text(encoding="utf-8")
    if f"pub(crate) fn {name}" not in t:
        t = t.replace(f"\nfn {name}", f"\npub(crate) fn {name}", 1)
    path.write_text(t, encoding="utf-8")


def dedupe_pub(text: str) -> str:
    while "pub(crate) pub(crate)" in text:
        text = text.replace("pub(crate) pub(crate)", "pub(crate)")
    return text


def main() -> None:
    # decode_image: drop duplicate import when local const exists
    di = ROOT / "src/hdr/decode/decode_image.rs"
    dt = di.read_text(encoding="utf-8")
    if "const MAX_HDR_FALLBACK_DECODE_BYTES" in dt and "use super::constants::MAX_HDR_FALLBACK_DECODE_BYTES" in dt:
        dt = dt.replace("use super::constants::MAX_HDR_FALLBACK_DECODE_BYTES;\n\n", "")
        dt = dt.replace(
            "limits.max_alloc = Some(super::constants::MAX_HDR_FALLBACK_DECODE_BYTES);",
            "limits.max_alloc = Some(MAX_HDR_FALLBACK_DECODE_BYTES);",
        )
    di.write_text(dt, encoding="utf-8")

    # openexr types corruption
    types = ROOT / "src/hdr/openexr_core/types.rs"
    tt = dedupe_pub(types.read_text(encoding="utf-8"))
    tt = tt.replace(
        "struct OpenExrCoreDecodedChunk {",
        "pub(crate) struct OpenExrCoreDecodedChunk {",
        1,
    )
    types.write_text(tt, encoding="utf-8")

    # channels missing imports / cfg-gated names
    ch = ROOT / "src/hdr/openexr_core/channels.rs"
    ct = ch.read_text(encoding="utf-8")
    ct = ct.replace("#[cfg(feature = \"tile-debug\")]\npub(crate) fn storage_name", "pub(crate) fn storage_name")
    ct = ct.replace("#[cfg(feature = \"tile-debug\")]\npub(crate) fn compression_name", "pub(crate) fn compression_name")
    if "use super::read_context::OpenExrCoreReadContext" not in ct:
        ct = ct.replace(
            "use super::{DEFAULT_DECODED_CHUNK_CACHE_BYTES, MAX_DECODED_CHUNK_CACHE_BYTES};",
            "use super::read_context::OpenExrCoreReadContext;\n"
            "use super::{\n"
            "    DEFAULT_DECODED_CHUNK_CACHE_BYTES, MAX_DECODED_CHUNK_CACHE_BYTES,\n"
            "    SCANLINE_BOOTSTRAP_PREVIEW_MAX_SIDE, SCANLINE_BOOTSTRAP_PREVIEW_SOURCE_ROW_BUDGET,\n"
            "};\n"
            "use std::time::Instant;",
        )
    ch.write_text(ct, encoding="utf-8")

    # libtiff load imports
    load = ROOT / "src/libtiff_loader/load.rs"
    extra = (
        "use crate::hdr::decode::hdr_to_sdr_rgba8_with_tone_settings;\n"
        "use crate::hdr::types::{\n"
        "    HdrColorProfile, HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat,\n"
        "    HdrReference, HdrToneMapSettings, HdrTransferFunction,\n"
        "};\n"
        "use super::decode::{\n"
        "    decode_ieee_scene_linear_rgba32f, decode_logl_logluv_scene_linear_rgba32f,\n"
        "    decode_uint16_rgb_scene_linear_rgba32f, tiff_ieee_scene_linear_eligible,\n"
        "    tiff_logl_logluv_hdr_eligible, tiff_uint16_rgb_scene_linear_eligible,\n"
        "    try_camera_tiff_rgb8_hdr_upgrade,\n"
        "};\n"
        "use super::scanline::LibTiffScanlineSource;\n"
        "use super::tiled::LibTiffTiledSource;\n"
        "use parking_lot::Mutex;\n"
        "use std::ffi::CString;\n\n"
    )
    lt = load.read_text(encoding="utf-8")
    if "HdrToneMapSettings" not in lt.split("pub fn load_via_libtiff")[0]:
        lt = lt.replace(MARKER, MARKER + extra, 1)
    load.write_text(lt, encoding="utf-8")

    decode = ROOT / "src/libtiff_loader/decode.rs"
    for fn in [
        "tiff_ieee_scene_linear_eligible",
        "tiff_uint16_rgb_scene_linear_eligible",
        "decode_uint16_rgb_scene_linear_rgba32f",
        "decode_ieee_scene_linear_rgba32f",
        "tiff_logl_logluv_hdr_eligible",
        "decode_logl_logluv_scene_linear_rgba32f",
    ]:
        pub_fn(decode, fn)

    # radiance rle imports
    rle = ROOT / "src/hdr/radiance_tiled/rle.rs"
    rt = rle.read_text(encoding="utf-8")
    if "use std::io::" not in rt:
        rt = rt.replace(
            "use super::layout::Rgbe8Pixel;\n\n",
            "use super::layout::Rgbe8Pixel;\n\nuse std::io::{Cursor, Read};\n\n",
        )
    rle.write_text(rt, encoding="utf-8")

    src = ROOT / "src/hdr/radiance_tiled/source.rs"
    st = src.read_text(encoding="utf-8")
    if "build_radiance_scanline_offsets" not in st:
        st = st.replace(
            "use super::header::read_radiance_header;\n",
            "use super::header::{build_radiance_scanline_offsets, read_radiance_header};\n",
        )
    src.write_text(st, encoding="utf-8")

    # tiled std + hdr type imports
    tiled_std = (
        "use crate::hdr::types::{HdrImageBuffer, HdrPixelFormat};\n"
        "use parking_lot::Mutex;\n"
        "use std::collections::{HashMap, HashSet, VecDeque};\n"
        "use std::sync::atomic::Ordering;\n"
        "use std::sync::Arc;\n\n"
    )
    for name in ("cache.rs", "source.rs", "preview.rs"):
        p = ROOT / "src/hdr/tiled" / name
        t = p.read_text(encoding="utf-8")
        if "use std::sync::atomic::Ordering" not in t:
            t = t.replace(MARKER, MARKER + tiled_std, 1)
        p.write_text(t, encoding="utf-8")

    # monitor effective rebuild
    lines = git_lines("src/hdr/monitor.rs")
    eff_body = "".join(lines[288:378])
    eff = ROOT / "src/hdr/monitor/effective.rs"
    eff.write_text(
        MARKER
        + "use super::types::{HdrMonitorSelection, HdrNativeSurfaceEncoding};\n"
        "use super::windows::windows_active_monitor_hdr_status;\n"
        "#[cfg(target_os = \"macos\")]\n"
        "use super::macos::macos_active_monitor_hdr_status;\n"
        "#[cfg(target_os = \"linux\")]\n"
        "use super::wayland;\n"
        "use crate::hdr::renderer::HdrRenderOutputMode;\n"
        "use crate::hdr::types::HdrOutputMode;\n\n"
        + eff_body,
        encoding="utf-8",
    )

    state = ROOT / "src/hdr/monitor/state.rs"
    st = state.read_text(encoding="utf-8")
    if "use std::time::" not in st:
        st = st.replace(
            MARKER,
            MARKER + "use std::time::{Duration, Instant};\n\nuse eframe::egui;\n\n",
            1,
        )
    state.write_text(st, encoding="utf-8")

    mac = ROOT / "src/hdr/monitor/macos.rs"
    mt = mac.read_text(encoding="utf-8")
    mt = re.sub(
        r"use super::types::HdrMonitorSelection;\n\nuse super::types::\{HdrMonitorSelection, HdrNativeSurfaceEncoding\};",
        "use super::types::{HdrMonitorSelection, HdrNativeSurfaceEncoding};",
        mt,
    )
    mac.write_text(mt, encoding="utf-8")

    orient = ROOT / "src/hdr/heif/orientation.rs"
    ot = orient.read_text(encoding="utf-8")
    if "orientation_from_heif_exif_item_blob" not in ot.split("fn heif_exif")[0]:
        ot = ot.replace(
            "use super::session::{HeifPrimaryGuard, open_heif_primary_from_bytes};\n\n",
            "use super::session::{\n"
            "    HeifPrimaryGuard, open_heif_primary_from_bytes, orientation_from_heif_exif_item_blob,\n"
            "};\n\n",
        )
    orient.write_text(ot, encoding="utf-8")

    ycbcr = ROOT / "src/hdr/heif/ycbcr.rs"
    yt = ycbcr.read_text(encoding="utf-8")
    if "planar_read_sample" not in yt.split("fn ")[0]:
        yt = yt.replace(
            MARKER,
            MARKER + "use super::decode::planar_read_sample;\n\n",
            1,
        )
    ycbcr.write_text(yt, encoding="utf-8")

    print("fix_round2 ok")


if __name__ == "__main__":
    main()
