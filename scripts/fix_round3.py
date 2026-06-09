#!/usr/bin/env python3
"""Third-round visibility and import fixes."""
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


def pub_method_in_impl(text: str, name: str) -> str:
    return text.replace(f"\n    fn {name}", f"\n    pub(crate) fn {name}", 1)


def pub_fn(path: Path, name: str) -> None:
    t = path.read_text(encoding="utf-8")
    if f"pub(crate) fn {name}" not in t and f"pub(crate) unsafe fn {name}" not in t:
        t = t.replace(f"\nfn {name}", f"\npub(crate) fn {name}", 1)
        t = t.replace(f"\nunsafe fn {name}", f"\npub(crate) unsafe fn {name}", 1)
    path.write_text(t, encoding="utf-8")


def main() -> None:
    # audio CueTrack fields
    cue = ROOT / "src/audio/cue.rs"
    cue.write_text(pubify_struct_fields(cue.read_text(encoding="utf-8"), "CueTrack"), encoding="utf-8")

    # app HDR cache structs
    types = ROOT / "src/app/types.rs"
    tt = types.read_text(encoding="utf-8")
    tt = pubify_struct_fields(tt, "CurrentHdrImage")
    tt = pubify_struct_fields(tt, "CurrentHdrTiledImage")
    types.write_text(tt, encoding="utf-8")

    # standard rendering cross-impl methods
    for name in ("transitions.rs", "hdr_draw.rs"):
        p = ROOT / "src/app/rendering/standard" / name
        t = p.read_text(encoding="utf-8")
        for fn in [
            "transition_normalized_t",
            "transition_prev_layout",
            "draw_outgoing_transition_frame_clipped",
            "draw_outgoing_transition_frame_ripple",
            "draw_rectangular_hdr_transition",
            "draw_hdr_image_plane_clipped",
            "draw_page_flip_hdr_new_image",
            "draw_curtain_hdr_new_image",
        ]:
            t = pub_method_in_impl(t, fn)
        p.write_text(t, encoding="utf-8")

    # openexr visibility
    ch = ROOT / "src/hdr/openexr_core/channels.rs"
    ct = ch.read_text(encoding="utf-8")
    for s in [
        "OpenExrCoreChunkDecodeTiming",
        "OpenExrCoreDecodedChunkFetch",
        "OpenExrCoreTileGrid",
    ]:
        ct = pubify_struct_fields(ct, s)
    if "SCANLINE_REFINED_PREVIEW_SOURCE_ROW_BUDGET" not in ct:
        ct = ct.replace(
            "SCANLINE_BOOTSTRAP_PREVIEW_SOURCE_ROW_BUDGET,\n};",
            "SCANLINE_BOOTSTRAP_PREVIEW_SOURCE_ROW_BUDGET,\n"
            "    SCANLINE_REFINED_PREVIEW_SOURCE_ROW_BUDGET,\n};",
        )
    ch.write_text(ct, encoding="utf-8")

    otypes = ROOT / "src/hdr/openexr_core/types.rs"
    ot = otypes.read_text(encoding="utf-8")
    ot = ot.replace("\n    fn new(max_bytes: usize)", "\n    pub(crate) fn new(max_bytes: usize)", 1)
    otypes.write_text(ot, encoding="utf-8")

    mmap = ROOT / "src/hdr/openexr_core/mmap.rs"
    mt = mmap.read_text(encoding="utf-8")
    mt = mt.replace("\n    fn as_mut_ptr(", "\n    pub(crate) fn as_mut_ptr(", 1)
    mmap.write_text(mt, encoding="utf-8")

    layout = ROOT / "src/hdr/radiance_tiled/layout.rs"
    layout.write_text(
        pubify_struct_fields(layout.read_text(encoding="utf-8"), "RadianceStridePlan"),
        encoding="utf-8",
    )

    src = ROOT / "src/hdr/radiance_tiled/source.rs"
    st = src.read_text(encoding="utf-8")
    if "build_radiance_scanline_offsets" not in st.split("use super::")[1].split("\n")[0]:
        st = st.replace(
            "use super::header::read_radiance_header;\n",
            "use super::header::{build_radiance_scanline_offsets, read_radiance_header};\n",
        )
    src.write_text(st, encoding="utf-8")

    # libtiff
    handle = ROOT / "src/libtiff_loader/handle.rs"
    ht = handle.read_text(encoding="utf-8")
    if "pub(crate) ptr:" not in ht:
        ht = ht.replace("    ptr: *mut c_void,", "    pub(crate) ptr: *mut c_void,", 1)
    handle.write_text(ht, encoding="utf-8")

    scanline = ROOT / "src/libtiff_loader/scanline.rs"
    st = scanline.read_text(encoding="utf-8")
    st = st.replace("\nunsafe fn manual_decode_scanline", "\npub(crate) unsafe fn manual_decode_scanline", 1)
    scanline.write_text(st, encoding="utf-8")

    load = ROOT / "src/libtiff_loader/load.rs"
    lt = load.read_text(encoding="utf-8")
    if "manual_decode_scanline" not in lt.split("fn load_via")[0]:
        lt = lt.replace(
            "use super::scanline::LibTiffScanlineSource;\n",
            "use super::scanline::{LibTiffScanlineSource, manual_decode_scanline};\n",
        )
    load.write_text(lt, encoding="utf-8")

    # raw loader cross-imports
    raw_load = ROOT / "src/loader/decode/raw/load.rs"
    rt = raw_load.read_text(encoding="utf-8")
    block = (
        "use super::develop::{develop_full_resolution, develop_hq_preview};\n"
        "use super::preview::{extract_embedded_preview, raw_embedded_preview_meets_hq_requirement};\n\n"
    )
    if "extract_embedded_preview" not in rt.split("fn load_raw")[0]:
        rt = rt.replace(MARKER, MARKER + block, 1)
    raw_load.write_text(rt, encoding="utf-8")

    # wic tiled source
    wic = ROOT / "src/wic/load.rs"
    wt = wic.read_text(encoding="utf-8")
    if "WicTiledSource" not in wt.split("fn load_via_wic")[0]:
        wt = wt.replace(MARKER, MARKER + "use super::tiled_source::WicTiledSource;\n\n", 1)
    wic.write_text(wt, encoding="utf-8")

    # heif ycbcr planar_read_sample import fix
    ycbcr = ROOT / "src/hdr/heif/ycbcr.rs"
    yt = ycbcr.read_text(encoding="utf-8")
    yt = yt.replace(
        "use super::decode::{planar_scale_from_depth, planar_semantic_depth_bits, planar_storage_span_bytes};\n\n",
        "use super::decode::{\n"
        "    planar_read_sample, planar_scale_from_depth, planar_semantic_depth_bits,\n"
        "    planar_storage_span_bytes,\n"
        "};\n\n",
    )
    ycbcr.write_text(yt, encoding="utf-8")

    orient = ROOT / "src/hdr/heif/orientation.rs"
    ot = orient.read_text(encoding="utf-8")
    ot = ot.replace(
        "use super::session::{HeifPrimaryGuard, open_heif_primary_from_bytes};\n\n",
        "use super::session::{\n"
        "    HeifPrimaryGuard, open_heif_primary_from_bytes, orientation_from_heif_exif_item_blob,\n"
        "};\n\n",
    )
    orient.write_text(ot, encoding="utf-8")

    print("fix_round3 ok")


if __name__ == "__main__":
    main()
