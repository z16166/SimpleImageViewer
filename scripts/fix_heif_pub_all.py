#!/usr/bin/env python3
"""Pubify all top-level heif items and wire load.rs imports."""
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
BASE = ROOT / "src/hdr/heif"
MARKER = "// along with this program.  If not, see <https://www.gnu.org/licenses/>.\n"

LOAD_IMPORTS = (
    "use super::decode::decode_primary_heif_to_hdr;\n"
    "use super::gain_map::{decode_heif_gain_map, heif_has_apple_hdr_gain_map_auxiliary};\n"
    "use super::metadata::{inspect_heif_gain_map_auxiliaries, read_heif_metadata, refine_heif_transfer_for_primary_bit_depth};\n"
    "use super::orientation::allocate_decode_options_for_heif_manual_geometry_fixup;\n"
    "use super::session::{HeifCtxGuard, HeifPrimaryGuard, open_heif_primary_from_bytes};\n\n"
)

GAIN_MAP_IMPORTS = (
    "use super::decode::RawHeifImage;\n"
    "use super::metadata::{HeifAuxiliaryClassification, list_heif_auxiliary_evidence};\n"
    "use super::orientation::{heif_exif_orientation_from_raw_handle, libheif_exif_orientation_tag};\n"
    "use super::session::HeifPrimaryGuard;\n\n"
)

ORIENTATION_IMPORTS = (
    "use super::session::{HeifPrimaryGuard, open_heif_primary_from_bytes};\n\n"
)

DECODE_IMPORTS = (
    "use super::session::{append_heif_unci_build_hint, append_mini_format_read_hint};\n"
    "use super::metadata::heif_sample_bit_depth;\n"
    "use super::ycbcr::hdr_buffer_from_ycbcr;\n\n"
)

METADATA_IMPORTS = (
    "use super::brand::heif_nclx_to_metadata;\n"
    "use super::gain_map::HeifAuxiliaryImageHandle;\n\n"
)

YCBCR_IMPORTS = (
    "use super::decode::{planar_scale_from_depth, planar_semantic_depth_bits, planar_storage_span_bytes};\n\n"
)


def pubify_file(path: Path) -> None:
    t = path.read_text(encoding="utf-8")
    t = re.sub(r"pub\(crate\)\s+pub\(crate\)\s+", "pub(crate) ", t)
    lines = t.splitlines(keepends=True)
    out = []
    for line in lines:
        if re.match(r"^impl ", line):
            out.append(line)
        elif re.match(r"^(struct|enum|fn|const|type|trait) ", line):
            out.append("pub(crate) " + line)
        elif re.match(r"^pub\(crate\) (struct|enum|fn|const|type|trait) ", line):
            out.append(line)
        elif re.match(r"^pub fn ", line):
            out.append(line.replace("pub fn ", "pub(crate) fn ", 1))
        else:
            out.append(line)
    path.write_text("".join(out), encoding="utf-8")


def prepend_once(path: Path, block: str) -> None:
    t = path.read_text(encoding="utf-8")
    if block.strip() not in t:
        t = t.replace(MARKER, MARKER + block, 1)
    path.write_text(t, encoding="utf-8")


def main() -> None:
    for name, block in [
        ("load.rs", LOAD_IMPORTS),
        ("gain_map.rs", GAIN_MAP_IMPORTS),
        ("decode.rs", DECODE_IMPORTS),
        ("metadata.rs", METADATA_IMPORTS),
        ("orientation.rs", ORIENTATION_IMPORTS),
        ("ycbcr.rs", YCBCR_IMPORTS),
    ]:
        prepend_once(BASE / name, block)

    for path in BASE.rglob("*.rs"):
        if path.name in ("mod.rs", "tests.rs"):
            continue
        pubify_file(path)

    print("fix_heif_pub_all ok")


if __name__ == "__main__":
    main()
