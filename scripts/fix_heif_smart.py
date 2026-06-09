#!/usr/bin/env python3
from pathlib import Path
import re

MARKER = "// along with this program.  If not, see <https://www.gnu.org/licenses/>.\n"
base = Path(__file__).resolve().parents[1] / "src/hdr/heif"

cross = {
    "orientation.rs": ["use super::session::{HeifCtxGuard, HeifPrimaryGuard, open_heif_primary_from_bytes};\n"],
    "load.rs": [
        "use super::decode::decode_primary_heif_to_hdr;\n",
        "use super::session::{HeifCtxGuard, HeifPrimaryGuard, open_heif_primary_from_bytes};\n",
    ],
    "decode.rs": [
        "use super::gain_map::decode_heif_gain_map;\n",
        "use super::metadata::read_heif_metadata;\n",
        "use super::orientation::HeifDecodeOptionsIgnoredGeometryOwned;\n",
        "use super::session::{HeifCtxGuard, HeifPrimaryGuard, ensure_heif_ok_lib, heif_error_to_string_lib, open_heif_primary_from_bytes};\n",
        "use super::ycbcr::{HeifYcbcrMatrix, hdr_buffer_from_ycbcr, heif_ycbcr_matrix_from_nclx};\n",
    ],
    "gain_map.rs": [
        "use super::orientation::libheif_exif_orientation_tag;\n",
        "use super::session::HeifPrimaryGuard;\n",
    ],
    "metadata.rs": ["use super::brand::heif_nclx_to_metadata;\n"],
}
pub_syms = {
    "session.rs": [
        "HeifCtxGuard",
        "HeifPrimaryGuard",
        "ensure_heif_ok_lib",
        "heif_error_to_string_lib",
        "open_heif_primary_from_bytes",
        "append_heif_unci_build_hint",
        "append_mini_format_read_hint",
    ],
    "orientation.rs": [
        "HeifDecodeOptionsIgnoredGeometryOwned",
        "allocate_decode_options_for_heif_manual_geometry_fixup",
        "heif_exif_orientation_from_handle",
        "libheif_transformation_props_to_manual_exif",
    ],
    "decode.rs": ["decode_primary_heif_to_hdr", "RawHeifImage", "heif_try_decode_into"],
    "ycbcr.rs": ["HeifYcbcrMatrix", "hdr_buffer_from_ycbcr", "heif_ycbcr_matrix_from_nclx"],
    "gain_map.rs": ["decode_heif_gain_map", "HeifAuxiliaryImageHandle"],
    "metadata.rs": [
        "read_heif_metadata",
        "heif_metadata_without_embedded_colour_info",
        "apply_heif_transfer_depth_heuristics",
        "refine_heif_transfer_for_primary_bit_depth",
        "inspect_heif_gain_map_auxiliaries",
        "list_heif_auxiliary_evidence",
        "heif_sample_bit_depth",
    ],
}


def strip_cross_imports(text: str) -> str:
    if MARKER not in text:
        return text
    head, rest = text.split(MARKER, 1)
    while rest.lstrip("\n").startswith("use super::"):
        rest = rest.lstrip("\n")
        if "\n\n" in rest:
            _, rest = rest.split("\n\n", 1)
        else:
            break
    if rest and not rest.startswith("\n"):
        rest = "\n" + rest
    return head + MARKER + rest


def make_pub(t: str, sym: str) -> str:
    if re.search(rf"(?m)^pub\(crate\) (?:struct|enum|fn) {re.escape(sym)}\b", t):
        return t
    t = re.sub(rf"(?m)^struct {re.escape(sym)}\b", f"pub(crate) struct {sym}", t, count=1)
    t = re.sub(rf"(?m)^enum {re.escape(sym)}\b", f"pub(crate) enum {sym}", t, count=1)
    t = re.sub(rf"(?m)^(?!\s*pub(?:\(crate\)|\s))fn {re.escape(sym)}\b", f"pub(crate) fn {sym}", t, count=1)
    return t


for name, extras in cross.items():
    p = base / name
    t = strip_cross_imports(p.read_text(encoding="utf-8"))
    extra = "".join(extras)
    if extra.strip() not in t:
        t = t.replace(MARKER, MARKER + extra, 1)
    for sym in pub_syms.get(name, []):
        t = make_pub(t, sym)
    p.write_text(t, encoding="utf-8")

for name, syms in pub_syms.items():
    if name in cross:
        continue
    p = base / name
    t = p.read_text(encoding="utf-8")
    for sym in syms:
        t = make_pub(t, sym)
    p.write_text(t, encoding="utf-8")

print("heif smart ok")
