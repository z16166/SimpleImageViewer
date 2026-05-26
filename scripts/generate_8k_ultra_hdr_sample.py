#!/usr/bin/env python3
"""Upscale an Ultra HDR JPEG to 8K+ for GPU compose / tiled stress tests.

Requires ImageMagick 7+ (`magick` on PATH). The resize re-encodes the file;
GContainer-style Ultra HDR samples usually survive better than MPF-only exports.
For MPF (Camera Raw) files, prefer re-exporting at target resolution when possible.

Usage (from repo root):
  python scripts/generate_8k_ultra_hdr_sample.py F:/HDR/GainMap/Triad-gain-map.jpg
  python scripts/generate_8k_ultra_hdr_sample.py input.jpg -o tests/data/ultra_hdr_8192.jpg --long-edge 8192
"""
from __future__ import annotations

import argparse
import shutil
import subprocess
import sys
from pathlib import Path


def has_magick() -> bool:
    return shutil.which("magick") is not None


def long_edge(path: Path) -> int:
    try:
        from PIL import Image

        with Image.open(path) as img:
            return max(img.size)
    except ImportError:
        pass
    # Fallback: parse SOF0 from baseline JPEG (works for most samples).
    data = path.read_bytes()
    i = 0
    while i + 1 < len(data):
        if data[i] != 0xFF:
            i += 1
            continue
        marker = data[i + 1]
        if marker in (0xC0, 0xC2) and i + 9 <= len(data):
            height = (data[i + 5] << 8) | data[i + 6]
            width = (data[i + 7] << 8) | data[i + 8]
            return max(width, height)
        if marker in (0xD8, 0xD9):
            i += 2
            continue
        if i + 4 > len(data):
            break
        seg_len = (data[i + 2] << 8) | data[i + 3]
        i += 2 + seg_len
    raise RuntimeError(f"Could not determine JPEG dimensions for {path}")


def upscale_with_magick(src: Path, dst: Path, long_edge_px: int) -> None:
    dst.parent.mkdir(parents=True, exist_ok=True)
    # Keep APP segments when possible; quality 95 limits recompression artifacts.
    cmd = [
        "magick",
        str(src),
        "-filter",
        "Lanczos",
        "-resize",
        f"{long_edge_px}x{long_edge_px}>",
        "-quality",
        "95",
        str(dst),
    ]
    print("Running:", " ".join(cmd))
    subprocess.run(cmd, check=True)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path, help="Ultra HDR JPEG (or any JPEG to upscale)")
    parser.add_argument(
        "-o",
        "--output",
        type=Path,
        default=None,
        help="Output path (default: tests/data/ultra_hdr_<edge>.jpg next to repo root)",
    )
    parser.add_argument(
        "--long-edge",
        type=int,
        default=8192,
        help="Target long edge in pixels (default: 8192)",
    )
    args = parser.parse_args()

    if not has_magick():
        print("ImageMagick 7+ required: install and ensure `magick` is on PATH.", file=sys.stderr)
        sys.exit(1)

    src = args.source.resolve()
    if not src.is_file():
        print(f"Source not found: {src}", file=sys.stderr)
        sys.exit(1)

    root = Path(__file__).resolve().parents[1]
    out = args.output
    if out is None:
        out = root / "tests" / "data" / f"ultra_hdr_{args.long_edge}.jpg"
    out = out.resolve()

    before = long_edge(src)
    print(f"Source long edge: {before}px -> target: {args.long_edge}px")
    if before >= args.long_edge:
        print(
            f"Source already >= {args.long_edge}px; copying to {out} for test harness convenience."
        )
        out.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(src, out)
    else:
        upscale_with_magick(src, out, args.long_edge)

    after = long_edge(out)
    print(f"Wrote {out} ({after}px long edge)")
    print("Set SIV_GAIN_MAP_SAMPLES_DIR or pass this path to manual GPU stress tests.")


if __name__ == "__main__":
    main()
