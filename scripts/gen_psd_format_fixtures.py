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
"""Generate PSD/PSB fixtures covering blend modes, colour-mode variants,
bit-depth variants, compression methods, and layer-group section types.

Output sub-directories under tests/data/psd_format/ (gitignored via tests/data/*).
Does not require Photoshop.

Usage:
  python scripts/gen_psd_format_fixtures.py
"""

from __future__ import annotations

import struct
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
OUT_DIR = ROOT / "tests" / "data" / "psd_format"

CANVAS_W = 64
CANVAS_H = 64

# ── Pack helpers ───────────────────────────────────────────────────────────

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

def u64(n: int) -> bytes:
    return struct.pack(">Q", n)


def pascal_name(name: str) -> bytes:
    raw = name.encode("ascii")
    body = bytes([len(raw)]) + raw
    pad = (4 - (len(body) % 4)) % 4
    return body + (b"\x00" * pad)


def pad_even(data: bytes) -> bytes:
    if len(data) % 2 == 1:
        data += b"\x00"
    return data


# ── Compression ────────────────────────────────────────────────────────────

def packbits_row(row: bytes) -> bytes:
    """Simple PackBits (RLE) encoding for one row of bytes."""
    out = bytearray()
    i = 0
    n = len(row)
    while i < n:
        # Look for a run of identical bytes (≥3)
        run_end = i + 1
        while run_end < n and run_end - i < 128 and row[run_end] == row[i]:
            run_end += 1
        run_len = run_end - i
        if run_len >= 3:
            out += u8(257 - run_len)  # negative run-length
            out += u8(row[i])
            i = run_end
            continue
        # Literal run
        lit_end = i + 1
        while lit_end < n:
            # Check if next 3 bytes form a repeat run
            if lit_end + 2 <= n and len(set(row[lit_end:lit_end + 3])) == 1:
                break
            lit_end += 1
            if lit_end - i >= 128:
                break
        lit_len = lit_end - i
        out += u8(lit_len - 1)  # n-1 literal bytes follow
        out += row[i:lit_end]
        i = lit_end
    return bytes(out)


def raw_channel(plane: bytes) -> bytes:
    """Raw (compression 0): u16(0) + pixel bytes."""
    return u16(0) + plane


def rle_channel(plane: bytes, w: int, h: int) -> bytes:
    """RLE (compression 1): u16(1) + row-byte-counts + packed rows."""
    out = bytearray()
    out += u16(1)
    rows = [plane[i * w:(i + 1) * w] for i in range(h)]
    packed = [packbits_row(r) for r in rows]
    for p in packed:
        out += u16(len(p))
    for p in packed:
        out += p
    return bytes(out)


def zip_channel(plane: bytes) -> bytes:
    """ZIP (compression 2): u16(2) + zlib-deflated data."""
    return u16(2) + zlib.compress(plane)


def zip_pred_channel(plane: bytes, w: int, h: int) -> bytes:
    """ZIP+Prediction (compression 3): delta-predict then deflate."""
    pred = bytearray()
    for row_idx in range(h):
        row = plane[row_idx * w:(row_idx + 1) * w]
        pred.append(row[0])
        for x in range(1, w):
            pred.append((row[x] - row[x - 1]) & 0xFF)
    return u16(3) + zlib.compress(bytes(pred))


# ── Channel payloads for different colour modes ───────────────────────────

def solid_plane(w: int, h: int, value: int) -> bytes:
    return bytes([value]) * (w * h)


def rgb_layer_channels(w: int, h: int, r: int, g: int, b: int, a: int,
                       comp: int = 0) -> bytes:
    """4 channels: alpha(-1), R(0), G(1), B(2)."""
    a_plane = solid_plane(w, h, a)
    r_plane = solid_plane(w, h, r)
    g_plane = solid_plane(w, h, g)
    b_plane = solid_plane(w, h, b)
    return _compress_planes(comp, w, h, [a_plane, r_plane, g_plane, b_plane])


def gray_layer_channels(w: int, h: int, gray: int, a: int,
                        comp: int = 0) -> bytes:
    """2 channels: alpha(-1), gray(0)."""
    a_plane = solid_plane(w, h, a)
    g_plane = solid_plane(w, h, gray)
    return _compress_planes(comp, w, h, [a_plane, g_plane])


def cmyk_layer_channels(w: int, h: int, c: int, m: int, y: int, k: int,
                         a: int, comp: int = 0) -> bytes:
    """5 channels: alpha(-1), C(0), M(1), Y(2), K(3)."""
    a_plane = solid_plane(w, h, a)
    c_plane = solid_plane(w, h, c)
    m_plane = solid_plane(w, h, m)
    y_plane = solid_plane(w, h, y)
    k_plane = solid_plane(w, h, k)
    return _compress_planes(comp, w, h, [a_plane, c_plane, m_plane, y_plane, k_plane])


def _compress_planes(comp: int, w: int, h: int, planes: list[bytes]) -> bytes:
    """Concatenate compressed planes in channel order."""
    out = bytearray()
    for plane in planes:
        if comp == 0:
            out += raw_channel(plane)
        elif comp == 1:
            out += rle_channel(plane, w, h)
        elif comp == 2:
            out += zip_channel(plane)
        elif comp == 3:
            out += zip_pred_channel(plane, w, h)
        else:
            raise ValueError(f"unknown compression {comp}")
    return bytes(out)


def layer_channel_ids(mode_code: int) -> list[int]:
    """Channel ids in the order they appear in image data, per colour mode."""
    if mode_code == 1:   # Grayscale
        return [-1, 0]
    elif mode_code == 3:  # RGB
        return [-1, 0, 1, 2]
    elif mode_code == 4:  # CMYK
        return [-1, 0, 1, 2, 3]
    else:
        return []


def channel_count_for_mode(mode_code: int) -> int:
    return len(layer_channel_ids(mode_code))


def layer_channels_for_data(mode_code: int, w: int, h: int,
                             colors: dict, comp: int = 0) -> bytes:
    """Dispatch to the correct layer channel builder by colour mode."""
    if mode_code == 1:   # Grayscale
        return gray_layer_channels(w, h, colors["g"], colors.get("a", 255), comp)
    elif mode_code == 3:  # RGB
        return rgb_layer_channels(w, h, colors["r"], colors["g"], colors["b"],
                                  colors.get("a", 255), comp)
    elif mode_code == 4:  # CMYK
        return cmyk_layer_channels(w, h, colors["c"], colors["m"], colors["y"],
                                   colors["k"], colors.get("a", 255), comp)
    raise ValueError(f"unsupported mode {mode_code}")


# ── Layer record with extra tagged blocks ─────────────────────────────────

def layer_record(
    top: int, left: int, bottom: int, right: int, *,
    opacity: int, clipping: int, blend_key: bytes, name: str,
    channel_data_lens: list[int],
    extra_tags: bytes = b"",
) -> bytes:
    """One layer record (channel image data follows all records)."""
    out = bytearray()
    out += i32(top) + i32(left) + i32(bottom) + i32(right)
    out += u16(len(channel_data_lens))
    for ch_id, data_len in zip(layer_channel_ids_for_len(len(channel_data_lens)),
                                channel_data_lens):
        out += i16(ch_id)
        out += u32(data_len)
    out += b"8BIM"
    out += blend_key
    out += u8(opacity)
    out += u8(clipping)
    out += u8(0)  # flags (visible)
    out += u8(0)  # filler
    name_bytes = pascal_name(name)
    extra = u32(0) + u32(0) + name_bytes + extra_tags
    out += u32(len(extra))
    out += extra
    return bytes(out)


def layer_channel_ids_for_len(n_channels: int) -> list[int]:
    """Guess channel ids from count: alpha + planar channels."""
    if n_channels == 2:
        return [-1, 0]          # Grayscale
    elif n_channels == 4:
        return [-1, 0, 1, 2]    # RGB
    elif n_channels == 5:
        return [-1, 0, 1, 2, 3]  # CMYK
    return list(range(n_channels))


def tagged_block(key: bytes, payload: bytes) -> bytes:
    """8BIM-tagged block used in layer extra data."""
    return b"8BIM" + key + u32(len(payload)) + pad_even(payload)


# ── Build full PSD/PSB ────────────────────────────────────────────────────

def build_psd(
    *,
    blend_key: bytes = b"norm",
    psb: bool = False,
    color_mode: int = 3,       # 1=Gray, 3=RGB, 4=CMYK
    depth: int = 8,
    comp: int = 0,
    n_channels: int | None = None,
    layer_section_divider: bytes | None = None,
    base_colors: dict | None = None,
    blend_colors: dict | None = None,
) -> bytes:
    """Build a PSD/PSB with optional two layers or a single divider layer.

    `layer_section_divider`: if set, appends an `lsct` tagged block to the
    second layer's extra data with this 4-byte payload (section type).
    """
    mode_code = color_mode
    if n_channels is None:
        n_channels = channel_count_for_mode(mode_code)

    has_blend = base_colors is not None
    n_layers = 2 if has_blend else 1

    # ── Layer records ─────────────────────────────────────────────────
    records = bytearray()
    ch_data = bytearray()

    if has_blend:
        # Base layer (full canvas)
        base_ch = layer_channels_for_data(mode_code, CANVAS_W, CANVAS_H,
                                          base_colors, comp)
        ch_lens_base = [len(base_ch) // n_channels] * n_channels
        records += layer_record(
            0, 0, CANVAS_H, CANVAS_W,
            opacity=255, clipping=0, blend_key=b"norm",
            name="Base", channel_data_lens=ch_lens_base,
        )
        ch_data += base_ch

        # Blending layer
        bw = 48
        bh = 48
        left = (CANVAS_W - bw) // 2
        top = (CANVAS_H - bh) // 2
        extra_tags = b""
        if layer_section_divider is not None:
            extra_tags += tagged_block(b"lsct", layer_section_divider)
        blend_ch = layer_channels_for_data(mode_code, bw, bh,
                                           blend_colors, comp)
        ch_lens_blend = [len(blend_ch) // n_channels] * n_channels
        records += layer_record(
            top, left, top + bh, left + bw,
            opacity=255, clipping=0, blend_key=blend_key,
            name="Blend", channel_data_lens=ch_lens_blend,
            extra_tags=extra_tags,
        )
        ch_data += blend_ch
    else:
        # Single divider record
        extra_tags = b""
        if layer_section_divider is not None:
            extra_tags += tagged_block(b"lsct", layer_section_divider)
        records += layer_record(
            0, 0, 1, 1,
            opacity=255, clipping=0, blend_key=b"norm",
            name="Divider", channel_data_lens=[],
            extra_tags=extra_tags,
        )

    # ── Layer & mask info ─────────────────────────────────────────────
    layer_info_body = i16(n_layers) + bytes(records) + bytes(ch_data)
    layer_info_body = pad_even(layer_info_body)
    layer_info = u32(len(layer_info_body)) + layer_info_body
    layer_and_mask = layer_info + u32(0)
    layer_and_mask = pad_even(layer_and_mask)

    # ── Header ────────────────────────────────────────────────────────
    version = 2 if psb else 1
    out = bytearray()
    out += b"8BPS"
    out += u16(version)
    out += b"\x00" * 6
    out += u16(n_channels)
    out += u32(CANVAS_H)
    out += u32(CANVAS_W)
    out += u16(depth)
    out += u16(mode_code)
    out += u32(0)  # colour mode data
    out += u32(0)  # image resources
    out += u32(len(layer_and_mask))
    out += layer_and_mask

    # Image Data (blank → forces P2 composite)
    out += u16(0)  # raw
    # 3 channels × w × h bytes for P1 fallback
    bytes_per_ch = CANVAS_W * CANVAS_H
    out += bytes(bytes_per_ch * 3)
    return bytes(out)


# ── Fixture definitions ───────────────────────────────────────────────────

def gen_blend_modes(out_dir: Path) -> int:
    """Generate 2-layer PSD files for all 28 blend-mode keys + PSB variants."""
    count = 0
    base_rgb = {"r": 200, "g": 40, "b": 40}
    blend_rgb = {"r": 50, "g": 180, "b": 200, "a": 200}

    blend_keys: list[tuple[bytes, str, bool]] = [
        (b"norm", "normal", False), (b"diss", "dissolve", False),
        (b"pass", "pass_through", False),
        (b"dark", "darken", False), (b"mul ", "multiply", False),
        (b"idiv", "color_burn", False), (b"lbrn", "linear_burn", False),
        (b"dkCl", "darker_color", False),
        (b"lite", "lighten", False), (b"scrn", "screen", False),
        (b"div ", "color_dodge", False), (b"lddg", "linear_dodge", False),
        (b"lgCl", "lighter_color", False),
        (b"over", "overlay", False), (b"sLit", "soft_light", False),
        (b"hLit", "hard_light", False),
        (b"vLit", "vivid_light", False), (b"lLit", "linear_light", False),
        (b"pLit", "pin_light", False), (b"hMix", "hard_mix", False),
        (b"diff", "difference", False), (b"excl", "exclusion", False),
        (b"subt", "subtract", False), (b"fdiv", "divide", False),
        (b"hue ", "hue", False), (b"sat ", "saturation", False),
        (b"colr", "color", False), (b"lum ", "luminosity", False),
        (b"norm", "psb_normal", True), (b"scrn", "psb_screen", True),
        (b"diff", "psb_difference", True), (b"hMix", "psb_hard_mix", True),
    ]
    sub = out_dir / "blend_modes"
    sub.mkdir(parents=True, exist_ok=True)
    for key, desc, is_psb in blend_keys:
        suffix = ".psb" if is_psb else ".psd"
        path = sub / f"blend_{desc}{suffix}"
        data = build_psd(blend_key=key, psb=is_psb,
                         base_colors=base_rgb, blend_colors=blend_rgb)
        path.write_bytes(data)
        count += 1
        print(f"  [blend] {path.name}  [{key!r}]")
    return count


def gen_color_mode_variants(out_dir: Path) -> int:
    """PSD files with various colour modes, bit depths, and PSB."""
    count = 0
    sub = out_dir / "color_mode"
    sub.mkdir(parents=True, exist_ok=True)

    variants: list[tuple[str, int, int, int, dict, dict, bool]] = [
        # (desc, mode_code, depth, n_channels, base_colors, blend_colors, psb)
        ("gray8",       1, 8,  2, {"g": 180},           {"g": 60, "a": 200}, False),
        ("gray16",      1, 16, 2, {"g": 180},           {"g": 60, "a": 200}, False),
        ("rgb8",        3, 8,  4, {"r": 200, "g": 40, "b": 40},
                                                     {"r": 50, "g": 180, "b": 200, "a": 200}, False),
        ("rgb16",       3, 16, 4, {"r": 200, "g": 40, "b": 40},
                                                     {"r": 50, "g": 180, "b": 200, "a": 200}, False),
        ("rgb32",       3, 32, 4, {"r": 200, "g": 40, "b": 40},
                                                     {"r": 50, "g": 180, "b": 200, "a": 200}, False),
        ("cmyk8",       4, 8,  5, {"c": 30, "m": 200, "y": 200, "k": 10},
                                                     {"c": 200, "m": 30, "y": 30, "k": 10, "a": 200}, False),
        ("cmyk16",      4, 16, 5, {"c": 30, "m": 200, "y": 200, "k": 10},
                                                     {"c": 200, "m": 30, "y": 30, "k": 10, "a": 200}, False),
        # PSB variants
        ("psb_gray8",   1, 8,  2, {"g": 180},           {"g": 60, "a": 200}, True),
        ("psb_rgb16",   3, 16, 4, {"r": 200, "g": 40, "b": 40},
                                                     {"r": 50, "g": 180, "b": 200, "a": 200}, True),
        ("psb_cmyk8",   4, 8,  5, {"c": 30, "m": 200, "y": 200, "k": 10},
                                                     {"c": 200, "m": 30, "y": 30, "k": 10, "a": 200}, True),
    ]
    for desc, mode_code, depth, n_ch, base_c, blend_c, psb in variants:
        suffix = ".psb" if psb else ".psd"
        path = sub / f"{desc}{suffix}"
        data = build_psd(blend_key=b"norm", psb=psb,
                         color_mode=mode_code, depth=depth,
                         n_channels=n_ch,
                         base_colors=base_c, blend_colors=blend_c)
        path.write_bytes(data)
        count += 1
        print(f"  [color]  {path.name}")
    return count


def gen_compression_variants(out_dir: Path) -> int:
    """Layered PSD files with each compression method."""
    count = 0
    sub = out_dir / "compression"
    sub.mkdir(parents=True, exist_ok=True)
    base_rgb = {"r": 200, "g": 40, "b": 40}
    blend_rgb = {"r": 50, "g": 180, "b": 200, "a": 200}

    for comp, label in [(0, "raw"), (1, "rle"), (2, "zip"), (3, "zip_pred")]:
        path = sub / f"layer_{label}.psd"
        data = build_psd(blend_key=b"norm", comp=comp,
                         base_colors=base_rgb, blend_colors=blend_rgb)
        path.write_bytes(data)
        count += 1
        print(f"  [comp]   {path.name}")
    return count


def gen_section_type_variants(out_dir: Path) -> int:
    """PSD files with layer-group section-type (lsct) tags."""
    count = 0
    sub = out_dir / "section_type"
    sub.mkdir(parents=True, exist_ok=True)
    base_rgb = {"r": 200, "g": 40, "b": 40}
    blend_rgb = {"r": 50, "g": 180, "b": 200, "a": 200}

    section_types: list[tuple[str, int]] = [
        ("open_folder",      1),
        ("closed_folder",    2),
        ("bounding_divider", 3),
        ("layer_group",      4),
    ]
    for label, stype in section_types:
        path = sub / f"section_{label}.psd"
        data = build_psd(blend_key=b"norm",
                         base_colors=base_rgb, blend_colors=blend_rgb,
                         layer_section_divider=u32(stype))
        path.write_bytes(data)
        count += 1
        print(f"  [section] {path.name}  (type={stype})")
    return count


def gen_divider_only_variants(out_dir: Path) -> int:
    """PSD with only a single divider layer (no pixel data) for each
    section type.  Exercises the `lsct` parsing path."""
    count = 0
    sub = out_dir / "divider_only"
    sub.mkdir(parents=True, exist_ok=True)

    section_types: list[tuple[str, int]] = [
        ("open_folder",      1),
        ("closed_folder",    2),
        ("bounding_divider", 3),
        ("layer_group",      4),
    ]
    for label, stype in section_types:
        path = sub / f"divider_{label}.psd"
        data = build_psd(layer_section_divider=u32(stype))
        path.write_bytes(data)
        count += 1
        print(f"  [divider] {path.name}  (type={stype})")
    return count


# ── Main ──────────────────────────────────────────────────────────────────

def main() -> None:
    total = 0
    total += gen_blend_modes(OUT_DIR)
    total += gen_color_mode_variants(OUT_DIR)
    total += gen_compression_variants(OUT_DIR)
    total += gen_section_type_variants(OUT_DIR)
    total += gen_divider_only_variants(OUT_DIR)
    print(f"\nGenerated {total} fixture files under {OUT_DIR}")


if __name__ == "__main__":
    main()
