#!/usr/bin/env python3
"""Generate PSD/PSB fixtures that exercise every layer blend mode.

For each of the 28 PSD blend-mode keys, produces one two-layer PSD (base +
blending layer) so the viewer's blend-mode implementation can be visually and
programmatically verified.

Outputs under tests/data/psd_blend_modes/ (gitignored via tests/data/*).
Does not require Photoshop -- writes a minimal layered PSD with blank Image
Data so the renderer falls back to P2 layer composite.

Usage:
  python scripts/gen_psd_blend_modes_fixtures.py

Then open any file under tests/data/psd_blend_modes/ to verify the blend.
"""

from __future__ import annotations

import struct
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
OUT_DIR = ROOT / "tests" / "data" / "psd_blend_modes"

CANVAS_W = 128
CANVAS_H = 128

# Base layer: full-canvas solid red
BASE_W = CANVAS_W
BASE_H = CANVAS_H
BASE_RGB = (200, 40, 40)

# Blending layer: smaller blue-green square in the centre
BLEND_W = 80
BLEND_H = 80
BLEND_LEFT = (CANVAS_W - BLEND_W) // 2
BLEND_TOP = (CANVAS_H - BLEND_H) // 2
BLEND_RGB = (50, 180, 200)


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
    body = bytes([len(raw)]) + raw
    pad = (4 - (len(body) % 4)) % 4
    return body + (b"\x00" * pad)


def solid_channel(w: int, h: int, value: int) -> bytes:
    """Raw compression (0) + planar bytes."""
    return u16(0) + bytes([value]) * (w * h)


def layer_channel_payload(w: int, h: int, r: int, g: int, b: int, a: int) -> bytes:
    """Channel order: -1 alpha, 0 R, 1 G, 2 B."""
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
    blend_key: bytes,
    name: str,
    channel_data_lens: list[int],
) -> bytes:
    """One layer record (no channel image data)."""
    out = bytearray()
    out += i32(top) + i32(left) + i32(bottom) + i32(right)
    out += u16(4)  # channel count
    for ch_id, data_len in zip((-1, 0, 1, 2), channel_data_lens):
        out += i16(ch_id)
        out += u32(data_len)
    out += b"8BIM"
    out += blend_key  # 4-byte blend mode key
    out += u8(opacity)
    out += u8(clipping)
    out += u8(0)  # flags (visible)
    out += u8(0)  # filler
    name_bytes = pascal_name(name)
    extra = u32(0) + u32(0) + name_bytes
    out += u32(len(extra))
    out += extra
    return bytes(out)


def build_psd(blend_key: bytes, psb: bool = False) -> bytes:
    """Build a minimal PSD/PSB with two layers: red base + blend top."""
    base_ch = layer_channel_payload(BASE_W, BASE_H, *BASE_RGB, 255)
    blend_ch = layer_channel_payload(BLEND_W, BLEND_H, *BLEND_RGB, 200)

    def lens_for(payload: bytes) -> list[int]:
        n = len(payload) // 4
        return [n, n, n, n]

    base_lens = lens_for(base_ch)
    blend_lens = lens_for(blend_ch)

    # Bottom-to-top: base first, then blend layer.
    rec_base = layer_record(
        0, 0, BASE_H, BASE_W,
        opacity=255, clipping=0,
        blend_key=b"norm",
        name="Base",
        channel_data_lens=base_lens,
    )
    rec_blend = layer_record(
        BLEND_TOP, BLEND_LEFT, BLEND_TOP + BLEND_H, BLEND_LEFT + BLEND_W,
        opacity=255, clipping=0,
        blend_key=blend_key,
        name=f"Blend_{blend_key.decode('ascii', errors='replace').strip()}",
        channel_data_lens=blend_lens,
    )

    layer_records = rec_base + rec_blend
    channel_image_data = base_ch + blend_ch

    layer_info_body = i16(2) + layer_records + channel_image_data
    if len(layer_info_body) % 2 == 1:
        layer_info_body += b"\x00"
    layer_info = u32(len(layer_info_body)) + layer_info_body
    layer_and_mask = layer_info + u32(0)  # global mask = 0
    if len(layer_and_mask) % 2 == 1:
        layer_and_mask += b"\x00"

    version = 2 if psb else 1
    out = bytearray()
    out += b"8BPS"
    out += u16(version)
    out += b"\x00" * 6
    out += u16(3)  # RGB channels
    out += u32(CANVAS_H)
    out += u32(CANVAS_W)
    out += u16(8)  # 8-bit
    out += u16(3)  # RGB colour mode
    out += u32(0)  # colour mode data
    out += u32(0)  # image resources
    out += u32(len(layer_and_mask))
    out += layer_and_mask
    # Image Data: raw black so P1 is blank -> P2 composite.
    out += u16(0)  # raw
    out += bytes(CANVAS_W * CANVAS_H * 3)
    return bytes(out)


# ── All 28 PSD blend-mode keys ────────────────────────────────────────────
# (key, description, is_psb)
BLEND_FIXTURES: list[tuple[bytes, str, bool]] = [
    # Normal & special
    (b"norm", "normal", False),
    (b"diss", "dissolve", False),
    (b"pass", "pass_through", False),
    # Darken group
    (b"dark", "darken", False),
    (b"mul ", "multiply", False),
    (b"idiv", "color_burn", False),
    (b"lbrn", "linear_burn", False),
    (b"dkCl", "darker_color", False),
    # Lighten group
    (b"lite", "lighten", False),
    (b"scrn", "screen", False),
    (b"div ", "color_dodge", False),
    (b"lddg", "linear_dodge", False),
    (b"lgCl", "lighter_color", False),
    # Overlay group
    (b"over", "overlay", False),
    (b"sLit", "soft_light", False),
    (b"hLit", "hard_light", False),
    # Contrast group
    (b"vLit", "vivid_light", False),
    (b"lLit", "linear_light", False),
    (b"pLit", "pin_light", False),
    (b"hMix", "hard_mix", False),
    # Comparative group
    (b"diff", "difference", False),
    (b"excl", "exclusion", False),
    (b"subt", "subtract", False),
    (b"fdiv", "divide", False),
    # Non-separable
    (b"hue ", "hue", False),
    (b"sat ", "saturation", False),
    (b"colr", "color", False),
    (b"lum ", "luminosity", False),
    # PSB variant (version 2) for a few representative keys
    (b"norm", "psb_normal", True),
    (b"scrn", "psb_screen", True),
    (b"diff", "psb_difference", True),
    (b"hMix", "psb_hard_mix", True),
]


def main() -> None:
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    count = 0
    for key, desc, is_psb in BLEND_FIXTURES:
        suffix = ".psb" if is_psb else ".psd"
        path = OUT_DIR / f"blend_{desc}{suffix}"
        data = build_psd(key, psb=is_psb)
        path.write_bytes(data)
        count += 1
        print(f"  wrote {path.name} ({path.stat().st_size} bytes)  [{key!r}]")
    print()
    print(f"Generated {count} fixture files under {OUT_DIR}")
    print("Open any file to verify the blend mode renders correctly.")


if __name__ == "__main__":
    main()
