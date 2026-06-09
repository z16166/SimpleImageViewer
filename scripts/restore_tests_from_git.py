#!/usr/bin/env python3
"""Restore tests.rs bodies from pre-split monolith #[cfg(test)] mod tests blocks."""

from __future__ import annotations

import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
COMMIT = "916e8fa"

COPYRIGHT = """// Simple Image Viewer - A high-performance, cross-platform image viewer
// Copyright (C) 2024-2026 Simple Image Viewer Contributors
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

"""

# tests.rs path -> monolith path at COMMIT
MAPPINGS: dict[str, str] = {
    "src/hdr/heif/tests.rs": "src/hdr/heif.rs",
    "src/hdr/avif/tests.rs": "src/hdr/avif.rs",
    "src/libtiff_loader/tests.rs": "src/libtiff_loader.rs",
    "src/hdr/openexr_core/tests.rs": "src/hdr/openexr_core_backend.rs",
    "src/loader/orchestrator/tests.rs": "src/loader/orchestrator.rs",
    "src/hdr/decode/tests.rs": "src/hdr/decode.rs",
    "src/hdr/tiled/tests.rs": "src/hdr/tiled.rs",
    "src/hdr/monitor/tests.rs": "src/hdr/monitor.rs",
    "src/hdr/radiance_tiled/tests.rs": "src/hdr/radiance_tiled.rs",
    "src/hdr/heif_apple_gain_map_compose_simd/tests.rs": "src/hdr/heif_apple_gain_map_compose_simd.rs",
    "src/app/input/tests.rs": "src/app/input.rs",
    "src/app/rendering/standard/tests.rs": "src/app/rendering/standard.rs",
    "src/loader/decode/raw/tests.rs": "src/loader/decode/raw.rs",
}


def git_show(path: str) -> str:
    return subprocess.check_output(
        ["git", "show", f"{COMMIT}:{path}"],
        cwd=ROOT,
        encoding="utf-8",
        errors="replace",
    )


def extract_tests_body(source: str) -> str | None:
    needle = "#[cfg(test)]"
    pos = 0
    while True:
        idx = source.find(needle, pos)
        if idx == -1:
            return None
        window = source[idx : idx + 80]
        if "mod tests {" in window:
            brace = source.find("mod tests {", idx) + len("mod tests {")
            depth = 1
            i = brace
            while i < len(source) and depth > 0:
                ch = source[i]
                if ch == "{":
                    depth += 1
                elif ch == "}":
                    depth -= 1
                i += 1
            inner = source[brace : i - 1]
            return dedent_block(inner)
        pos = idx + len(needle)


def dedent_block(text: str) -> str:
    lines = text.splitlines()
    if not lines:
        return ""
    # Drop leading/trailing blank lines from extraction.
    while lines and not lines[0].strip():
        lines.pop(0)
    while lines and not lines[-1].strip():
        lines.pop()
    if not lines:
        return ""
    indent = len(lines[0]) - len(lines[0].lstrip(" "))
    if indent == 0:
        return "\n".join(lines) + "\n"
    out = []
    for line in lines:
        if line.strip():
            if line.startswith(" " * indent):
                out.append(line[indent:])
            else:
                out.append(line)
        else:
            out.append("")
    return "\n".join(out) + "\n"


def main() -> None:
    restored = []
    skipped = []
    for dest_rel, src_rel in MAPPINGS.items():
        dest = ROOT / dest_rel
        try:
            source = git_show(src_rel)
        except subprocess.CalledProcessError:
            skipped.append((dest_rel, f"missing source {src_rel}"))
            continue
        body = extract_tests_body(source)
        if body is None:
            skipped.append((dest_rel, "no #[cfg(test)] mod tests block"))
            continue
        dest.write_text(COPYRIGHT + body, encoding="utf-8", newline="\n")
        restored.append(dest_rel)

    print(f"Restored {len(restored)} test modules from {COMMIT}")
    for p in restored:
        print(f"  {p}")
    if skipped:
        print(f"Skipped {len(skipped)}:")
        for p, why in skipped:
            print(f"  {p}: {why}")


if __name__ == "__main__":
    main()
