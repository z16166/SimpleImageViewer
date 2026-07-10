#!/usr/bin/env python3
"""Generate local PSD fixtures that demonstrate clipping groups on the main canvas.

Outputs under tests/data/psd_clipping/ (gitignored via tests/data/*).
Does not require Photoshop -- writes a minimal layered PSD with blank Image Data
so the viewer degrades P1 -> P2 layer composite.

Usage:
  python scripts/gen_psd_clipping_fixture.py

Then open:
  tests/data/psd_clipping/clipping_on.psd   -- blue only inside red base
  tests/data/psd_clipping/clipping_off.psd  -- blue extends outside red
"""

from __future__ import annotations

import struct
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
OUT_DIR = ROOT / "tests" / "data" / "psd_clipping"

# Large enough to see clearly in the main window.
CANVAS_W = 256
CANVAS_H = 256

# Opaque red base rectangle (left side).
BASE_L, BASE_T, BASE_R, BASE_B = 40, 60, 140, 200

# Semi-transparent blue square that deliberately extends past the red base.
CLIP_L, CLIP_T, CLIP_R, CLIP_B = 90, 40, 220, 180


def u8(n: int) -> bytes:
    return struct.pack(">B", n)


def u16(n: int) -> bytes:
    return struct.pack(">H", n)


def i16(n: int) -> bytes:
    return struct.pack(">h", n)


def u32(n: int) -> bytes:
    return struct.pack(">I", n)


def i32(n: int) -> bytes:
    return struct.pack(">i", n)


def pascal_name(name: str) -> bytes:
    raw = name.encode("ascii")
    # Pascal string padded to multiple of 4 (including length byte).
    body = bytes([len(raw)]) + raw
    pad = (4 - (len(body) % 4)) % 4
    return body + (b"\x00" * pad)


def solid_channel(w: int, h: int, value: int) -> bytes:
    """Raw compression (0) + planar bytes."""
    return u16(0) + bytes([value]) * (w * h)


def layer_channel_payload(w: int, h: int, r: int, g: int, b: int, a: int) -> bytes:
    # Channel order in image data follows the record channel list order.
    # We list: -1 alpha, 0 R, 1 G, 2 B.
    return (
        solid_channel(w, h, a)
        + solid_channel(w, h, r)
        + solid_channel(w, h, g)
        + solid_channel(w, h, b)
    )


def layer_record(
    top: int,
    left: int,
    bottom: int,
    right: int,
    *,
    opacity: int,
    clipping: int,
    name: str,
    channel_data_lens: list[int],
) -> bytes:
    """One layer record (no channel pixels -- those follow all records)."""
    out = bytearray()
    out += i32(top) + i32(left) + i32(bottom) + i32(right)
    out += u16(4)  # channel count
    # Channel ids: alpha, R, G, B -- each length includes 2-byte compression.
    for ch_id, data_len in zip((-1, 0, 1, 2), channel_data_lens):
        out += i16(ch_id)
        out += u32(data_len)
    out += b"8BIM"
    out += b"norm"
    out += u8(opacity)
    out += u8(clipping)
    out += u8(0)  # flags (visible)
    out += u8(0)  # filler

    # Extra data: mask (0) + blending ranges (0) + name
    name_bytes = pascal_name(name)
    extra = u32(0) + u32(0) + name_bytes
    out += u32(len(extra))
    out += extra
    return bytes(out)


def build_psd(*, clip_clipping: int) -> bytes:
    base_w = BASE_R - BASE_L
    base_h = BASE_B - BASE_T
    clip_w = CLIP_R - CLIP_L
    clip_h = CLIP_B - CLIP_T

    base_ch = layer_channel_payload(base_w, base_h, 220, 40, 40, 255)
    # Blue at ~70% opacity so the red base still reads underneath in the overlap.
    clip_ch = layer_channel_payload(clip_w, clip_h, 40, 80, 255, 180)

    # Per-channel lengths must match what we emit in channel image data.
    def lens_for(payload: bytes) -> list[int]:
        # 4 channels, each: 2 byte compression + w*h bytes
        # payload is concatenation of 4 equal-sized channel blobs
        n = len(payload) // 4
        return [n, n, n, n]

    base_lens = lens_for(base_ch)
    clip_lens = lens_for(clip_ch)

    # Bottom-to-top: base first, then clip.
    rec_base = layer_record(
        BASE_T,
        BASE_L,
        BASE_B,
        BASE_R,
        opacity=255,
        clipping=0,
        name="BaseRed",
        channel_data_lens=base_lens,
    )
    rec_clip = layer_record(
        CLIP_T,
        CLIP_L,
        CLIP_B,
        CLIP_R,
        opacity=255,
        clipping=clip_clipping,
        name="ClipBlue",
        channel_data_lens=clip_lens,
    )

    layer_records = rec_base + rec_clip
    channel_image_data = base_ch + clip_ch

    # Layer info section: length + count + records + channel data
    layer_info_body = i16(2) + layer_records + channel_image_data
    # Layer info length must be even
    if len(layer_info_body) % 2 == 1:
        layer_info_body += b"\x00"
    layer_info = u32(len(layer_info_body)) + layer_info_body

    # Global layer mask info = 0
    layer_and_mask = layer_info + u32(0)
    # Section length must be even
    if len(layer_and_mask) % 2 == 1:
        layer_and_mask += b"\x00"

    out = bytearray()
    out += b"8BPS"
    out += u16(1)  # PSD
    out += b"\x00" * 6
    out += u16(3)  # RGB channels in Image Data
    out += u32(CANVAS_H)
    out += u32(CANVAS_W)
    out += u16(8)
    out += u16(3)  # RGB
    out += u32(0)  # color mode data
    out += u32(0)  # image resources
    out += u32(len(layer_and_mask))
    out += layer_and_mask

    # Image Data: raw black RGB so P1 is absolutely blank -> P2 layer composite.
    out += u16(0)  # raw
    out += bytes(CANVAS_W * CANVAS_H * 3)
    return bytes(out)


def main() -> None:
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    on_path = OUT_DIR / "clipping_on.psd"
    off_path = OUT_DIR / "clipping_off.psd"
    on_path.write_bytes(build_psd(clip_clipping=1))
    off_path.write_bytes(build_psd(clip_clipping=0))
    print(f"wrote {on_path} ({on_path.stat().st_size} bytes)")
    print(f"wrote {off_path} ({off_path.stat().st_size} bytes)")
    print()
    print("Open clipping_on.psd: blue should appear ONLY inside the red rectangle.")
    print("Open clipping_off.psd: blue should extend outside the red rectangle.")


if __name__ == "__main__":
    main()
