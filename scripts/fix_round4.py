#!/usr/bin/env python3
"""Fourth-round fixes."""
from __future__ import annotations

import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MARKER = "// along with this program.  If not, see <https://www.gnu.org/licenses/>.\n"


def pubify_struct_fields(text: str, struct_name: str) -> str:
    pattern = rf"(pub(?:\(crate\)|\(\w+\))? struct {re.escape(struct_name)} \{{)(.*?)(\n\}})"
    m = re.search(pattern, text, re.DOTALL)
    if not m:
        return text
    head, body, tail = m.group(1), m.group(2), m.group(3)
    lines = body.splitlines(keepends=True)
    out = []
    for line in lines:
        if re.match(r"^\s+pub(?:\(crate\)|\(\w+\))?\s+\w", line) or line.strip().startswith("//"):
            out.append(line)
            continue
        if re.match(r"^\s+\w", line):
            out.append(re.sub(r"^(\s+)(\w)", r"\1pub(crate) \2", line, count=1))
        else:
            out.append(line)
    return text[: m.start()] + head + "".join(out) + tail + text[m.end() :]


def pub_fn(path: Path, name: str) -> None:
    t = path.read_text(encoding="utf-8")
    for prefix in ("fn ", "unsafe fn "):
        old = f"\n{prefix}{name}"
        new = f"\npub(crate) {prefix}{name}"
        if old in t and new not in t:
            t = t.replace(old, new, 1)
    path.write_text(t, encoding="utf-8")


def main() -> None:
    # raw load: doc comment must precede imports
    raw = ROOT / "src/loader/decode/raw/load.rs"
    rt = raw.read_text(encoding="utf-8")
    rt = rt.replace(
        MARKER
        + "use super::develop::{develop_full_resolution, develop_hq_preview};\n"
        + "use super::preview::{extract_embedded_preview, raw_embedded_preview_meets_hq_requirement};\n\n\n"
        + "//! LibRAW and raw tiled refinement.\n"
        + "//!\n"
        + "//! `raw_high_quality` controls whether LibRaw's expensive demosaic runs:\n"
        + "//! - **Off:** use embedded previews whenever present (SDR pipeline on all displays).\n"
        + "//!   Full develop only when the file has no embedded preview; on HDR displays that\n"
        + "//!   develop result uses the HDR pipeline.\n"
        + "//! - **On:** use embedded previews when they meet HQ size requirements; otherwise demosaic at\n"
        + "//!   full sensor resolution. Developed pixels use the HDR pipeline on HDR displays.\n\n",
        MARKER
        + "//! LibRAW and raw tiled refinement.\n"
        + "//!\n"
        + "//! `raw_high_quality` controls whether LibRaw's expensive demosaic runs:\n"
        + "//! - **Off:** use embedded previews whenever present (SDR pipeline on all displays).\n"
        + "//!   Full develop only when the file has no embedded preview; on HDR displays that\n"
        + "//!   develop result uses the HDR pipeline.\n"
        + "//! - **On:** use embedded previews when they meet HQ size requirements; otherwise demosaic at\n"
        + "//!   full sensor resolution. Developed pixels use the HDR pipeline on HDR displays.\n\n"
        + "use super::develop::{develop_full_resolution, develop_hq_preview};\n"
        + "use super::preview::{extract_embedded_preview, raw_embedded_preview_meets_hq_requirement};\n\n",
    )
    raw.write_text(rt, encoding="utf-8")
    for name in [
        "develop_full_resolution",
        "develop_hq_preview",
        "extract_embedded_preview",
        "raw_embedded_preview_meets_hq_requirement",
    ]:
        pub_fn(ROOT / "src/loader/decode/raw/develop.rs", name)
        pub_fn(ROOT / "src/loader/decode/raw/preview.rs", name)

    # libtiff handle + decode hdr types
    handle = ROOT / "src/libtiff_loader/handle.rs"
    ht = handle.read_text(encoding="utf-8")
    ht = ht.replace("    ptr: *mut lib::TIFF,", "    pub(crate) ptr: *mut lib::TIFF,", 1)
    handle.write_text(ht, encoding="utf-8")

    decode = ROOT / "src/libtiff_loader/decode.rs"
    dt = decode.read_text(encoding="utf-8")
    if "HdrImageBuffer" not in dt.split("fn get_raw_value")[0]:
        dt = dt.replace(
            MARKER,
            MARKER
            + "use crate::hdr::types::{\n"
            "    HdrColorProfile, HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat,\n"
            "    HdrReference, HdrToneMapSettings, HdrTransferFunction,\n"
            "};\n\n",
            1,
        )
    decode.write_text(dt, encoding="utf-8")

    mmap = ROOT / "src/libtiff_loader/mmap.rs"
    mt = mmap.read_text(encoding="utf-8")
    if "c_int" not in mt.split("fn tiff")[0]:
        mt = mt.replace("use std::ffi::c_void;\n", "use std::ffi::{c_int, c_void};\n", 1)
    mmap.write_text(mt, encoding="utf-8")

    # openexr
    ch = ROOT / "src/hdr/openexr_core/channels.rs"
    ct = ch.read_text(encoding="utf-8")
    if "SCANLINE_REFINED_PREVIEW_SOURCE_ROW_BUDGET" not in ct.split("use super::")[2]:
        ct = ct.replace(
            "SCANLINE_BOOTSTRAP_PREVIEW_SOURCE_ROW_BUDGET,\n};",
            "SCANLINE_BOOTSTRAP_PREVIEW_SOURCE_ROW_BUDGET, SCANLINE_REFINED_PREVIEW_SOURCE_ROW_BUDGET,\n};",
        )
    ch.write_text(ct, encoding="utf-8")

    rc = ROOT / "src/hdr/openexr_core/read_context.rs"
    rc.write_text(pubify_struct_fields(rc.read_text(encoding="utf-8"), "OpenExrCoreReadContext"), encoding="utf-8")

    types = ROOT / "src/hdr/openexr_core/types.rs"
    types.write_text(
        pubify_struct_fields(types.read_text(encoding="utf-8"), "OpenExrCoreDecodedChunk"),
        encoding="utf-8",
    )

    ommap = ROOT / "src/hdr/openexr_core/mmap.rs"
    om = ommap.read_text(encoding="utf-8")
    if "use std::ptr;" not in om:
        om = om.replace("use std::sync::Arc;\n", "use std::ptr;\nuse std::sync::Arc;\n", 1)
    ommap.write_text(om, encoding="utf-8")

    # tiled globals exports
    gl = ROOT / "src/hdr/tiled/globals.rs"
    gl.write_text(
        gl.read_text(encoding="utf-8").replace(
            "pub static HDR_TILE_CACHE_MAX_BYTES",
            "pub(crate) static HDR_TILE_CACHE_MAX_BYTES",
            1,
        ),
        encoding="utf-8",
    )

    src = ROOT / "src/hdr/tiled/source.rs"
    st = src.read_text(encoding="utf-8")
    if "HdrColorSpace" not in st.split("impl HdrTiledImageSource")[0]:
        st = st.replace(
            "use crate::hdr::types::{HdrImageBuffer, HdrPixelFormat};\n",
            "use crate::hdr::types::{HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat};\n",
        )
    src.write_text(st, encoding="utf-8")

    prev = ROOT / "src/hdr/tiled/preview.rs"
    pt = prev.read_text(encoding="utf-8")
    if "HdrColorSpace" not in pt.split("pub(crate) fn")[0]:
        pt = pt.replace(
            "use crate::hdr::types::{HdrImageBuffer, HdrPixelFormat};\n",
            "use crate::hdr::types::{HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat};\n",
        )
    prev.write_text(pt, encoding="utf-8")

    # jpegxl probe
    probe = ROOT / "src/hdr/jpegxl/probe.rs"
    pt = probe.read_text(encoding="utf-8")
    if "JXL_PROBE_ITERATION_CAP" not in pt.split("fn is_jxl_header")[0]:
        pt = pt.replace(
            MARKER,
            MARKER
            + "use crate::constants::JXL_PROBE_ITERATION_CAP;\n"
            + "use crate::hdr::types::HdrImageMetadata;\n\n",
            1,
        )
    probe.write_text(pt, encoding="utf-8")

    # app fields + tiled helpers
    app = ROOT / "src/app/types.rs"
    at = app.read_text(encoding="utf-8")
    at = at.replace(
        "    rgb10a2_pq_encode_requested:",
        "    pub(crate) rgb10a2_pq_encode_requested:",
        1,
    )
    at = at.replace(
        "    last_logged_swap_chain_format_request:",
        "    pub(crate) last_logged_swap_chain_format_request:",
        1,
    )
    app.write_text(at, encoding="utf-8")

    helpers = ROOT / "src/app/rendering/tiled/helpers.rs"
    ht = helpers.read_text(encoding="utf-8")
    ht = ht.replace("\n    fn new(", "\n    pub(crate) fn new(", 1)
    ht = ht.replace("\n    fn try_mark_pending(", "\n    pub(crate) fn try_mark_pending(", 1)
    helpers.write_text(ht, encoding="utf-8")

    print("fix_round4 ok")


if __name__ == "__main__":
    main()
