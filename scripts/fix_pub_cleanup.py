#!/usr/bin/env python3
"""Clean duplicated visibility qualifiers and fix common split syntax issues."""
from __future__ import annotations

import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MARKER = "// along with this program.  If not, see <https://www.gnu.org/licenses/>.\n"


def clean_text(text: str) -> str:
    for _ in range(10):
        text = re.sub(r"pub\(crate\)\s+pub\(crate\)\s+", "pub(crate) ", text)
        text = re.sub(r"pub\(crate\)\s+pub\s+", "pub ", text)
        text = re.sub(r"pub\s+pub\(crate\)\s+", "pub(crate) ", text)
        text = re.sub(r"pub\s+pub\s+", "pub ", text)
    return text


def prepend_once(path: Path, block: str) -> None:
    t = path.read_text(encoding="utf-8")
    if block.strip() not in t:
        t = t.replace(MARKER, MARKER + block, 1)
    path.write_text(t, encoding="utf-8")


def main() -> None:
    for path in (ROOT / "src").rglob("*.rs"):
        new = clean_text(path.read_text(encoding="utf-8"))
        path.write_text(new, encoding="utf-8")

    for name in ["load.rs", "poll.rs", "tiles.rs"]:
        p = ROOT / "src/loader/orchestrator" / name
        p.write_text(
            p.read_text(encoding="utf-8").replace(
                "\n\n//! Worker pool, deferred loads, refinement channels, tile queue orchestration ([`ImageLoader`]).\n\n",
                "\n",
            ),
            encoding="utf-8",
        )

    cue = ROOT / "src/audio/cue.rs"
    ct = cue.read_text(encoding="utf-8")
    if "AtomicBool" not in ct.split("struct CueTrack")[0]:
        ct = ct.replace(
            MARKER,
            MARKER + "use std::sync::atomic::{AtomicBool, Ordering};\nuse std::sync::Arc;\n\n",
            1,
        )
    ct = ct.replace("\nfn read_text_file_with_fallback", "\npub(crate) fn read_text_file_with_fallback", 1)
    cue.write_text(ct, encoding="utf-8")
    prepend_once(ROOT / "src/audio/playlist.rs", "use super::cue::read_text_file_with_fallback;\n\n")

    print("fix_pub_cleanup ok")


if __name__ == "__main__":
    main()
