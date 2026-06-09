#!/usr/bin/env python3
"""Fix test modules broken by the monolith split."""

from __future__ import annotations

from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SRC = ROOT / "src"


def find_block_end(lines: list[str], start: int) -> int:
    """Return index of line with closing brace matching opening brace on start line."""
    depth = 0
    for i in range(start, len(lines)):
        for ch in lines[i]:
            if ch == "{":
                depth += 1
            elif ch == "}":
                depth -= 1
                if depth == 0:
                    return i
    raise ValueError(f"unmatched brace starting at line {start + 1}")


def dedent(text: str, spaces: int = 4) -> str:
    prefix = " " * spaces
    out = []
    for line in text.splitlines(keepends=True):
        if line.startswith(prefix) and line.strip():
            out.append(line[spaces:])
        else:
            out.append(line)
    return "".join(out)


def unwrap_mod_tests(text: str) -> str:
    lines = text.splitlines(keepends=True)
    mod_idx = None
    for i, line in enumerate(lines):
        if line.strip() == "mod tests {":
            mod_idx = i
            break
        if line.strip() == "#[cfg(test)]" and i + 1 < len(lines) and lines[i + 1].strip() == "mod tests {":
            mod_idx = i + 1
            break

    if mod_idx is None:
        # File may be uniformly over-indented without a wrapper (avif/tests.rs).
        body = [ln for ln in lines if ln.strip() and not ln.strip().startswith("//")]
        if body and all(ln.startswith("    ") for ln in body[: min(10, len(body))]):
            return dedent("".join(lines))
        return text

    header = lines[:mod_idx]
    if header and header[-1].strip() == "#[cfg(test)]":
        header = header[:-1]

    end_idx = find_block_end(lines, mod_idx)
    inner = lines[mod_idx + 1 : end_idx]
    tail = lines[end_idx + 1 :]

    inner_text = dedent("".join(inner))
    result = "".join(header) + inner_text + "".join(tail)
    return result.rstrip() + "\n"


def drop_duplicate_imports(text: str) -> str:
    lines = text.splitlines(keepends=True)
    seen: set[str] = set()
    out: list[str] = []
    i = 0
    while i < len(lines):
        line = lines[i]
        if line.lstrip().startswith("use "):
            block = [line]
            i += 1
            while i < len(lines) and lines[i].strip() and (
                lines[i].lstrip().startswith("use ")
                or (not lines[i].strip().startswith("#") and block[-1].rstrip().endswith(","))
            ):
                block.append(lines[i])
                i += 1
            key = "".join(block).strip()
            if key in seen:
                continue
            seen.add(key)
            out.extend(block)
            continue
        out.append(line)
        i += 1
    return "".join(out)


def fix_tests_rs(path: Path) -> bool:
    original = path.read_text(encoding="utf-8")
    text = unwrap_mod_tests(original)
    text = drop_duplicate_imports(text)
    if text != original:
        path.write_text(text, encoding="utf-8", newline="\n")
        return True
    return False


def fix_test_part_super_refs(path: Path) -> bool:
    original = path.read_text(encoding="utf-8")
    text = original
    replacements = [
        ("use super::tile_cache::", "use super::super::tile_cache::"),
        ("use super::tone_map_uniform::", "use super::super::tone_map_uniform::"),
        ("use super::upload::", "use super::super::upload::"),
        ("use super::decode_jxl_bytes_to_image_data", "use crate::loader::decode::decode_jxl_bytes_to_image_data"),
        ("super::decode_jxl_bytes_to_image_data", "crate::loader::decode::decode_jxl_bytes_to_image_data"),
        ("super::srgb_unit_to_u8", "super::super::srgb_unit_to_u8"),
        ("super::linear_to_srgb_u8", "super::super::linear_to_srgb_u8"),
    ]
    for old, new in replacements:
        text = text.replace(old, new)
    if "use super::*;" in text and path.parent.name == "tests":
        text = text.replace("use super::*;", "use super::super::*;")
    if text != original:
        path.write_text(text, encoding="utf-8", newline="\n")
        return True
    return False


def main() -> None:
    changed: list[str] = []
    for path in sorted(SRC.rglob("tests.rs")):
        if fix_tests_rs(path):
            changed.append(str(path.relative_to(ROOT)))
    for path in sorted(SRC.rglob("tests/part*.rs")):
        if fix_test_part_super_refs(path):
            changed.append(str(path.relative_to(ROOT)))
    print(f"Updated {len(changed)} files")
    for p in changed:
        print(f"  {p}")


if __name__ == "__main__":
    main()
