#!/usr/bin/env python3
"""Final wave-1 rebuild without stripping module preludes."""
from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))

from split_wave1_monoliths import split_audio, split_heif, split_orchestrator  # noqa: E402

MARKER = "// along with this program.  If not, see <https://www.gnu.org/licenses/>.\n"
CUE_IMPORTS = (
    "use std::fs;\n"
    "use std::path::Path;\n"
    "use std::time::Duration;\n\n"
)
LOOP_CROSS = (
    "use super::cue::{load_cue, CueSheet};\n"
    "use super::player::AudioError;\n"
    "use super::playlist::{build_base_non_m3u_set, expand_m3u_excluding_base, is_m3u_path};\n"
    "use super::slots::{\n"
    "    set_cue_markers, set_cue_track, set_current_path, set_current_track, set_error, set_metadata,\n"
    "};\n"
    "use super::sources::symphonia::{get_file_metadata, open_source};\n\n"
)
LOAD_CROSS = (
    "use std::ffi::CString;\n"
    "use std::path::PathBuf;\n\n"
    "use crate::hdr::types::{\n"
    "    HdrColorProfile, HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrLuminanceMetadata,\n"
    "    HdrPixelFormat, HdrReference, HdrToneMapSettings, HdrTransferFunction,\n"
    "};\n"
    "use super::decode::{\n"
    "    decode_ieee_scene_linear_rgba32f, decode_logl_logluv_scene_linear_rgba32f,\n"
    "    decode_uint16_rgb_scene_linear_rgba32f,\n"
    "    tiff_ieee_scene_linear_eligible, tiff_logl_logluv_hdr_eligible,\n"
    "    tiff_uint16_rgb_scene_linear_eligible,\n"
    "};\n"
    "use super::handle::create_tiff_handle;\n"
    "use super::scanline::manual_decode_scanline;\n\n"
)
TILED_CROSS = (
    "use super::buffer::HdrTileBuffer;\n"
    "use super::cache::{HdrTileCache, configured_hdr_tile_cache_max_bytes};\n"
    "use super::globals::{\n"
    "    DEFAULT_HDR_TILE_CACHE_MAX_BYTES, HDR_TILE_CACHE_MAX_BYTES, HdrTileCacheKey,\n"
    "    MAX_HDR_TILE_CACHE_MAX_BYTES,\n"
    "};\n"
    "use super::kind::{HdrTiledSource, HdrTiledSourceKind};\n"
    "use super::validate::{validate_rgba32f_len, validate_tile_bounds};\n\n"
)


def dedupe_uses(text: str) -> str:
    lines = text.splitlines(keepends=True)
    out: list[str] = []
    seen: set[str] = set()
    i = 0
    while i < len(lines):
        line = lines[i]
        if line.startswith("use ") or line.startswith("pub use "):
            block = line
            while not block.rstrip().endswith(";") and i + 1 < len(lines):
                i += 1
                block += lines[i]
            key = re.sub(r"\s+", " ", block.strip())
            if key not in seen:
                seen.add(key)
                out.append(block)
        else:
            out.append(line)
        i += 1
    return "".join(out)


def prepend_once(path: Path, block: str) -> None:
    t = path.read_text(encoding="utf-8")
    if block.strip() not in t:
        t = t.replace(MARKER, MARKER + block, 1)
    path.write_text(dedupe_uses(t), encoding="utf-8")


def fix_cue() -> None:
    p = ROOT / "src/audio/cue.rs"
    t = p.read_text(encoding="utf-8")
    if CUE_IMPORTS.strip() not in t:
        t = t.replace(MARKER, MARKER + CUE_IMPORTS, 1)
    t = t.replace("\nstruct CueTrack", "\npub(crate) struct CueTrack", 1)
    t = t.replace("\nstruct CueSheet", "\npub(crate) struct CueSheet", 1)
    t = t.replace("\nfn load_cue", "\npub(crate) fn load_cue", 1)
    p.write_text(dedupe_uses(t), encoding="utf-8")


def pubify_libtiff() -> None:
    scan = ROOT / "src/libtiff_loader/scanline.rs"
    st = scan.read_text(encoding="utf-8")
    st = st.replace("unsafe fn manual_decode_scanline", "pub(crate) unsafe fn manual_decode_scanline", 1)
    scan.write_text(st, encoding="utf-8")
    decode = ROOT / "src/libtiff_loader/decode.rs"
    dt = decode.read_text(encoding="utf-8")
    for fn in [
        "tiff_ieee_scene_linear_eligible",
        "tiff_uint16_rgb_scene_linear_eligible",
        "decode_uint16_rgb_scene_linear_rgba32f",
        "decode_ieee_scene_linear_rgba32f",
        "tiff_logl_logluv_hdr_eligible",
        "decode_logl_logluv_scene_linear_rgba32f",
    ]:
        dt = re.sub(rf"(?m)^(?!\s*pub(?:\(crate\)|\s))fn {re.escape(fn)}\b", f"pub(crate) fn {fn}", dt, count=1)
    decode.write_text(dt, encoding="utf-8")
    prepend_once(ROOT / "src/libtiff_loader/load.rs", LOAD_CROSS)


def fix_tiled() -> None:
    for fname in ["source.rs", "preview.rs"]:
        prepend_once(ROOT / "src/hdr/tiled" / fname, TILED_CROSS)
    import fix_wave1_imports as fwi

    fwi.fix_tiled()
    fwi.fix_radiance()
    for p in (ROOT / "src/hdr/tiled").rglob("*.rs"):
        p.write_text(dedupe_uses(p.read_text(encoding="utf-8")), encoding="utf-8")


def main() -> None:
    split_heif()
    split_audio()
    split_orchestrator()
    subprocess.run(
        ["python", "-c", "import sys; sys.path.insert(0,'scripts'); import rebuild_splits as rs; rs.rebuild_tiled(); rs.rebuild_radiance_tiled()"],
        cwd=ROOT,
        check=True,
    )
    subprocess.run(["python", str(ROOT / "scripts/fix_radiance_imports.py")], cwd=ROOT, check=True)
    subprocess.run(["python", str(ROOT / "scripts/rebuild_libtiff.py")], cwd=ROOT, check=True)
    subprocess.run(["python", str(ROOT / "scripts/fix_openexr_resplit.py")], cwd=ROOT, check=True)
    subprocess.run(["python", str(ROOT / "scripts/fix_heif_pub_all.py")], cwd=ROOT, check=True)
    import fix_wave1_imports as fwi

    fwi.fix_heif_cross_module()
    fix_cue()
    subprocess.run(["python", str(ROOT / "scripts/fix_audio_wiring.py")], cwd=ROOT, check=True)
    prepend_once(ROOT / "src/audio/loop_state.rs", LOOP_CROSS)
    pubify_libtiff()
    subprocess.run(["python", str(ROOT / "scripts/fix_orchestrator.py")], cwd=ROOT, check=True)
    subprocess.run(["python", str(ROOT / "scripts/apply_split_fixes.py")], cwd=ROOT, check=True)
    subprocess.run(
        ["python", "-c", "import sys; sys.path.insert(0,'scripts'); import rebuild_splits as rs; rs.rebuild_jpegxl()"],
        cwd=ROOT,
        check=True,
    )
    subprocess.run(["python", str(ROOT / "scripts/minimal_split_fixes.py")], cwd=ROOT, check=True)
    subprocess.run(["python", str(ROOT / "scripts/fix_openexr_resplit.py")], cwd=ROOT, check=True)
    subprocess.run(["python", str(ROOT / "scripts/fix_remaining.py")], cwd=ROOT, check=True)
    subprocess.run(["python", str(ROOT / "scripts/fix_pub_cleanup.py")], cwd=ROOT, check=True)
    subprocess.run(["python", str(ROOT / "scripts/fix_tiled_imports.py")], cwd=ROOT, check=True)
    subprocess.run(["python", str(ROOT / "scripts/fix_decode_image.py")], cwd=ROOT, check=True)
    subprocess.run(["python", str(ROOT / "scripts/fix_libtiff_cross.py")], cwd=ROOT, check=True)
        new = dedupe_uses(path.read_text(encoding="utf-8"))
        path.write_text(new, encoding="utf-8")
    print("finish_compile prep ok")


if __name__ == "__main__":
    main()
