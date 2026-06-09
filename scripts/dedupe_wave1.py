#!/usr/bin/env python3
"""Remove duplicate use/import lines introduced by repeated wiring scripts."""
from __future__ import annotations

import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MARKER = "// along with this program.  If not, see <https://www.gnu.org/licenses/>.\n"


def dedupe_use_block(text: str) -> str:
    """Drop duplicate `use` lines while preserving order."""
    lines = text.splitlines(keepends=True)
    out: list[str] = []
    seen_uses: set[str] = set()
    i = 0
    while i < len(lines):
        line = lines[i]
        if line.startswith("use ") or line.startswith("pub use "):
            block = line
            while not block.endswith(";\n") and i + 1 < len(lines):
                i += 1
                block += lines[i]
            key = re.sub(r"\s+", " ", block.strip())
            if key in seen_uses:
                i += 1
                continue
            seen_uses.add(key)
            out.append(block)
        else:
            out.append(line)
        i += 1
    return "".join(out)


def strip_duplicate_blocks_after_marker(text: str) -> str:
    if MARKER not in text:
        return dedupe_use_block(text)
    head, rest = text.split(MARKER, 1)
    # Remove blank-line-separated duplicate prelude blocks (same lines repeated)
    parts = rest.split("\n\n")
    unique_parts: list[str] = []
    seen: set[str] = set()
    for part in parts:
        key = part.strip()
        if not key:
            if unique_parts and unique_parts[-1].strip():
                unique_parts.append("")
            continue
        if key.startswith("use ") or key.startswith("pub use ") or key.startswith("#["):
            if key in seen:
                continue
            seen.add(key)
        unique_parts.append(part)
    rest = "\n\n".join(unique_parts)
    if not rest.startswith("\n") and rest:
        rest = "\n" + rest
    return dedupe_use_block(head + MARKER + rest)


def main() -> None:
    for path in (ROOT / "src").rglob("*.rs"):
        text = path.read_text(encoding="utf-8")
        new = strip_duplicate_blocks_after_marker(text)
        if new != text:
            path.write_text(new, encoding="utf-8")
    print("dedupe_wave1 ok")


if __name__ == "__main__":
    main()
