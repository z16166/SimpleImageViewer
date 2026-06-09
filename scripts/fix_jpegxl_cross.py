#!/usr/bin/env python3
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MARKER = "// along with this program.  If not, see <https://www.gnu.org/licenses/>.\n"
base = ROOT / "src/hdr/jpegxl"


def pub_fn(path: Path, name: str) -> None:
    t = path.read_text(encoding="utf-8")
    if f"pub(crate) fn {name}" not in t:
        t = t.replace(f"\nfn {name}", f"\npub(crate) fn {name}", 1)
    path.write_text(t, encoding="utf-8")


def main() -> None:
    meta = base / "metadata.rs"
    for fn in [
        "ensure_jxl_success",
        "capture_jxl_box",
        "linear_to_srgb_u8",
        "jxl_decoder_copy_target_data_icc",
        "jxl_decoder_copy_target_original_icc",
        "jxl_apply_preferred_profile_from_target_data_icc",
        "read_jxl_metadata",
    ]:
        pub_fn(meta, fn)

    runner = base / "runner.rs"
    rt = runner.read_text(encoding="utf-8")
    rt = rt.replace("struct JxlResizableRunnerPtr", "pub(crate) struct JxlResizableRunnerPtr", 1)
    runner.write_text(rt, encoding="utf-8")

    decode = base / "decode.rs"
    dt = decode.read_text(encoding="utf-8")
    block = (
        "use super::metadata::{\n"
        "    capture_jxl_box, ensure_jxl_success, jxl_apply_preferred_profile_from_target_data_icc,\n"
        "    jxl_decoder_copy_target_data_icc, jxl_decoder_copy_target_original_icc, linear_to_srgb_u8,\n"
        "    read_jxl_metadata,\n"
        "};\n"
        "use super::runner::JxlResizableRunnerPtr;\n\n"
    )
    if "use super::metadata::{" not in dt:
        dt = dt.replace("use super::probe::is_jxl_header;\n\n", f"use super::probe::is_jxl_header;\n\n{block}", 1)
    decode.write_text(dt, encoding="utf-8")

    mt = meta.read_text(encoding="utf-8")
    probe_import = (
        "use super::probe::{\n"
        "    JXL_TRANSFER_FUNCTION_709, JXL_TRANSFER_FUNCTION_GAMMA, JXL_TRANSFER_FUNCTION_HLG,\n"
        "    JXL_TRANSFER_FUNCTION_LINEAR, JXL_TRANSFER_FUNCTION_PQ, JXL_TRANSFER_FUNCTION_SRGB,\n"
        "};\n"
        "use super::decode::{\n"
        "    decode_jxl_hdr_bytes_with_target_capacity, jxl_tag_display_referred_when_sdr_grade,\n"
        "};\n\n"
    )
    if "JXL_TRANSFER_FUNCTION_LINEAR" not in mt.split("fn ensure_jxl_success")[0]:
        mt = mt.replace(MARKER, MARKER + probe_import, 1)
    meta.write_text(mt, encoding="utf-8")

    pub_fn(decode, "jxl_tag_display_referred_when_sdr_grade")
    print("fix_jpegxl_cross ok")


if __name__ == "__main__":
    main()
