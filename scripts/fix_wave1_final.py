#!/usr/bin/env python3
"""Rebuild wave-1 splits cleanly and apply wiring fixes (no cleanup_splits)."""
from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))

from split_wave1_monoliths import (  # noqa: E402
    split_audio,
    split_heif,
    split_libtiff_loader,
    split_openexr_core,
    split_orchestrator,
)

MARKER = "// along with this program.  If not, see <https://www.gnu.org/licenses/>.\n\n"


def dedupe_pub() -> None:
    for path in (ROOT / "src").rglob("*.rs"):
        text = path.read_text(encoding="utf-8")
        new = text
        for _ in range(8):
            new = re.sub(r"(pub(?:\(crate\)|\(super\))?)\s+\1\s+", r"\1 ", new)
            new = new.replace("pub pub ", "pub ")
        if new != text:
            path.write_text(new, encoding="utf-8")


def wire_libtiff() -> None:
    import fix_wave1_imports as fwi

    fwi.fix_libtiff()
    handle = ROOT / "src/libtiff_loader/handle.rs"
    ht = handle.read_text(encoding="utf-8")
    if "use super::mmap::TiffMmapContext" not in ht:
        ins = (
            "use libtiff_viewer as lib;\n"
            "use memmap2::Mmap;\n"
            "use std::ffi::CString;\n"
            "use std::os::raw::c_void;\n"
            "use std::path::Path;\n"
            "use std::sync::Arc;\n\n"
            "use super::mmap::{\n"
            "    TiffMmapContext, tiff_close_proc, tiff_map_proc, tiff_read_proc, tiff_seek_proc,\n"
            "    tiff_size_proc, tiff_unmap_proc, tiff_write_proc,\n"
            "};\n\n"
        )
        body = ht.split(MARKER, 1)[-1]
        while body.startswith("use "):
            body = body.split("\n\n", 1)[-1]
        handle.write_text(
            ht.split(MARKER)[0]
            + MARKER
            + ins
            + body.replace("fn create_tiff_handle", "pub(crate) fn create_tiff_handle", 1),
            encoding="utf-8",
        )
    tiled = ROOT / "src/libtiff_loader/tiled.rs"
    tiled.write_text(
        tiled.read_text(encoding="utf-8").replace(
            "use super::thumbnail::extract_embedded_thumbnail;\n", ""
        ),
        encoding="utf-8",
    )


def wire_audio() -> None:
    slots = ROOT / "src/audio/slots.rs"
    text = slots.read_text(encoding="utf-8")
    for fn in [
        "set_error",
        "set_current_track",
        "set_current_path",
        "set_metadata",
        "set_cue_track",
        "set_cue_markers",
    ]:
        text = text.replace(f"\nfn {fn}", f"\npub(crate) fn {fn}", 1)
    slots.write_text(text, encoding="utf-8")

    playlist = ROOT / "src/audio/playlist.rs"
    text = playlist.read_text(encoding="utf-8")
    for fn in ["build_base_non_m3u_set", "is_m3u_path", "expand_m3u_excluding_base"]:
        text = text.replace(f"\nfn {fn}", f"\npub(crate) fn {fn}", 1)
    playlist.write_text(text, encoding="utf-8")

    cue = ROOT / "src/audio/cue.rs"
    cue.write_text(
        cue.read_text(encoding="utf-8").replace("\nstruct CueSheet", "\npub(crate) struct CueSheet", 1),
        encoding="utf-8",
    )


def wire_app() -> None:
    inserts = [
        ("src/app/rendering/tiled/mod.rs", "use crate::settings::TransitionStyle;\n\n"),
        (
            "src/app/rendering/standard/transitions.rs",
            "use std::sync::Arc;\n\nuse crate::hdr::renderer::HdrRenderOutputMode;\n\n",
        ),
        ("src/app/types.rs", "use crate::scanner;\n\n"),
        ("src/app/hotkeys_ui.rs", "use rust_i18n::t;\n\n"),
    ]
    for rel, ins in inserts:
        path = ROOT / rel
        text = path.read_text(encoding="utf-8")
        if ins.strip() not in text:
            path.write_text(text.replace(MARKER, MARKER + ins, 1), encoding="utf-8")


def main() -> None:
    split_libtiff_loader()
    split_heif()
    split_openexr_core()
    split_audio()
    split_orchestrator()

    wire_libtiff()

    import fix_wave1_imports as fwi

    fwi.fix_heif_cross_module()
    fwi.fix_openexr_visibility()

    for script in ["fix_audio_wiring.py", "fix_heif_smart.py", "fix_openexr_exports.py", "finish_wave1.py"]:
        subprocess.run(["python", str(ROOT / "scripts" / script)], cwd=ROOT, check=True)

    wire_audio()
    wire_app()
    dedupe_pub()
    print("fix_wave1_final done")


if __name__ == "__main__":
    main()
