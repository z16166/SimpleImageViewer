#!/usr/bin/env python3
# Simple Image Viewer - A high-performance, cross-platform image viewer
# Copyright (C) 2024-2026 Simple Image Viewer Contributors
#
# This program is free software: you can redistribute it and/or modify
# it under the terms of the GNU General Public License as published by
# the Free Software Foundation, either version 3 of the License, or
# (at your option) any later version.
#
# This program is distributed in the hope that it will be useful,
# but WITHOUT ANY WARRANTY; without even the implied warranty of
# MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
# GNU General Public License for more details.
#
# You should have received a copy of the GNU General Public License
# along with this program.  If not, see <https://www.gnu.org/licenses/>.
"""Generate local PSD fixtures for Image Data compression codes 0-3.

Outputs under tests/data/psd_compress/ (gitignored via tests/data/*).
Does not require ImageMagick or Photoshop -- writes a minimal valid PSD.

Usage:
  python scripts/gen_psd_compress_fixtures.py
"""

from __future__ import annotations

import struct
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
OUT_DIR = ROOT / "tests" / "data" / "psd_compress"

WIDTH = 8
HEIGHT = 4
CHANNELS = 3  # RGB


def packbits_row(row: bytes) -> bytes:
    """Naive PackBits: store the whole row as one literal run (row len <= 128)."""
    assert 1 <= len(row) <= 128
    return bytes([len(row) - 1]) + row


def apply_prediction_8(planar: bytearray, width: int, height: int, channels: int) -> None:
    for ch in range(channels):
        base = ch * width * height
        for y in range(height):
            row = base + y * width
            for x in range(width - 1, 0, -1):
                planar[row + x] = (planar[row + x] - planar[row + x - 1]) & 0xFF


def make_planar_rgb() -> bytearray:
    """Distinct non-solid pattern so blank barriers do not reject the decode."""
    planar = bytearray(CHANNELS * WIDTH * HEIGHT)
    for y in range(HEIGHT):
        for x in range(WIDTH):
            i = y * WIDTH + x
            planar[i] = (x * 17 + y * 3) & 0xFF  # R
            planar[WIDTH * HEIGHT + i] = (x * 9 + y * 11) & 0xFF  # G
            planar[2 * WIDTH * HEIGHT + i] = (255 - x * 13 - y * 5) & 0xFF  # B
    return planar


def write_psd_header(buf: bytearray) -> None:
    buf += b"8BPS"
    buf += struct.pack(">H", 1)  # version
    buf += b"\x00" * 6
    buf += struct.pack(">H", CHANNELS)
    buf += struct.pack(">I", HEIGHT)
    buf += struct.pack(">I", WIDTH)
    buf += struct.pack(">H", 8)  # depth
    buf += struct.pack(">H", 3)  # RGB
    buf += struct.pack(">I", 0)  # color mode data
    buf += struct.pack(">I", 0)  # image resources
    buf += struct.pack(">I", 0)  # layer and mask info


def build_psd(compression: int) -> bytes:
    planar = make_planar_rgb()
    out = bytearray()
    write_psd_header(out)
    out += struct.pack(">H", compression)

    if compression == 0:
        out += planar
    elif compression == 1:
        row_counts = []
        rows = []
        for ch in range(CHANNELS):
            base = ch * WIDTH * HEIGHT
            for y in range(HEIGHT):
                row = bytes(planar[base + y * WIDTH : base + (y + 1) * WIDTH])
                packed = packbits_row(row)
                row_counts.append(len(packed))
                rows.append(packed)
        for count in row_counts:
            out += struct.pack(">H", count)
        for row in rows:
            out += row
    elif compression in (2, 3):
        payload = bytearray(planar)
        if compression == 3:
            apply_prediction_8(payload, WIDTH, HEIGHT, CHANNELS)
        out += zlib.compress(bytes(payload), level=6)
    else:
        raise ValueError(compression)
    return bytes(out)


def main() -> None:
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    names = {
        0: "rgb8_raw.psd",
        1: "rgb8_rle.psd",
        2: "rgb8_zip.psd",
        3: "rgb8_zip_prediction.psd",
    }
    for code, name in names.items():
        path = OUT_DIR / name
        data = build_psd(code)
        path.write_bytes(data)
        print(f"wrote {path} ({len(data)} bytes, compression={code})")


if __name__ == "__main__":
    main()
