#!/usr/bin/env python3
"""One-shot rebuild of wave-1 splits and wiring."""
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))

from split_wave1_monoliths import (  # noqa: E402
    split_audio,
    split_heif,
    split_openexr_core,
    split_orchestrator,
)


def dedupe_pub() -> None:
    for path in (ROOT / "src").rglob("*.rs"):
        text = path.read_text(encoding="utf-8")
        new = text
        for _ in range(8):
            new = re.sub(r"(pub(?:\(crate\)|\(super\))?)\s+\1\s+", r"\1 ", new)
            new = new.replace("pub pub ", "pub ")
        if new != text:
            path.write_text(new, encoding="utf-8")


def main() -> None:
    subprocess.run(["python", str(ROOT / "scripts" / "rebuild_libtiff.py")], cwd=ROOT, check=True)
    split_heif()
    split_openexr_core()
    split_audio()
    split_orchestrator()

    import fix_wave1_imports as fwi

    fwi.fix_heif_cross_module()
    fwi.fix_openexr_visibility()

    for script in [
        "fix_audio_wiring.py",
        "fix_heif_smart.py",
        "fix_openexr_exports.py",
        "wire_wave1_imports.py",
        "finish_wave1.py",
    ]:
        subprocess.run(["python", str(ROOT / "scripts" / script)], cwd=ROOT, check=True)

    decode = ROOT / "src/libtiff_loader/decode.rs"
    dt = decode.read_text(encoding="utf-8")
    for fn in [
        "tiff_ieee_scene_linear_eligible",
        "tiff_uint16_rgb_scene_linear_eligible",
        "decode_uint16_rgb_scene_linear_rgba32f",
        "decode_ieee_scene_linear_rgba32f",
        "tiff_logl_logluv_hdr_eligible",
        "decode_logl_logluv_scene_linear_rgba32f",
        "manual_decode_scanline",
    ]:
        dt = dt.replace(f"\nfn {fn}", f"\npub(crate) fn {fn}", 1)
    decode.write_text(dt, encoding="utf-8")

    dedupe_pub()
    print("rebuild_wave1 ok")


if __name__ == "__main__":
    main()
