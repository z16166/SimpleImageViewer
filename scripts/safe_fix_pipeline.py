#!/usr/bin/env python3
"""Run fix scripts in safe order (no apply_split_fixes)."""
from __future__ import annotations

import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

SCRIPTS = [
    "rebuild_libtiff.py",
    "fix_openexr_resplit.py",
    "fix_heif_pub_all.py",
    "fix_audio_wiring.py",
    "fix_orchestrator.py",
    "minimal_split_fixes.py",  # jpegxl decode/metadata imports
    "fix_openexr_resplit.py",
    "fix_remaining.py",
    "fix_pub_cleanup.py",
    "fix_tiled_imports.py",
    "fix_radiance_imports.py",
    "fix_decode_image.py",
    "fix_libtiff_cross.py",
    "fix_jpegxl_cross.py",
    "fix_audio_loop_state.py",
    "fix_libtiff_load.py",
    "fix_openexr_exports.py",
    "fix_round2.py",
    "fix_round3.py",
    "fix_round4.py",
]

def main() -> None:
    for name in SCRIPTS:
        path = ROOT / "scripts" / name
        if path.exists():
            subprocess.run(["python", str(path)], cwd=ROOT, check=True)
    print("safe_fix_pipeline ok")

if __name__ == "__main__":
    main()
