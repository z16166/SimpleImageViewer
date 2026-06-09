#!/usr/bin/env python3
import re
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
lines = subprocess.check_output(
    ["git", "show", "HEAD:src/hdr/decode.rs"], cwd=ROOT, text=True, encoding="utf-8"
).splitlines(keepends=True)
cp = "".join(lines[:15])
body = "".join(lines[16:82])
body = body.replace("use super::types::", "use crate::hdr::types::")
body = body.replace("is_exr_path(path)", "super::paths::is_exr_path(path)")
body = body.replace("is_radiance_hdr_path(path)", "super::paths::is_radiance_hdr_path(path)")
body = body.replace("return decode_exr_display_image", "return super::exr::decode_exr_display_image")
body = body.replace("return decode_radiance_hdr_image", "return super::radiance::decode_radiance_hdr_image")
body = body.replace("validate_hdr_fallback_budget(", "super::tone_map::validate_hdr_fallback_budget(")
body = body.replace(
    "};\n\nconst HDR_RGBA32F_BYTES_PER_PIXEL",
    "};\n\nuse super::constants::MAX_HDR_FALLBACK_DECODE_BYTES;\n\nconst HDR_RGBA32F_BYTES_PER_PIXEL",
)
(ROOT / "src/hdr/decode/decode_image.rs").write_text(cp + body, encoding="utf-8")

exr = ROOT / "src/hdr/decode/exr.rs"
et = exr.read_text(encoding="utf-8")
et = re.sub(r"super::tone_map::(?:super::tone_map::)+", "super::tone_map::", et)
if "super::tone_map::validate_hdr_fallback_budget" not in et:
    et = et.replace("validate_hdr_fallback_budget(", "super::tone_map::validate_hdr_fallback_budget(")
exr.write_text(et, encoding="utf-8")
print("fix_decode_image ok")
