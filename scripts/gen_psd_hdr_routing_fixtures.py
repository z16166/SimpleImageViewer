#!/usr/bin/env python3
"""Generate minimal PSD/PSB fixtures for HDR routing branch tests.

Outputs under tests/data/psd_hdr_routing/ (gitignored via tests/data/*).
Does not require Photoshop -- writes minimal valid PSD/PSB files plus manifest.json.

Usage:
  python scripts/gen_psd_hdr_routing_fixtures.py
"""

from __future__ import annotations

import json
import struct
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
OUT_DIR = ROOT / "tests" / "data" / "psd_hdr_routing"


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


def f32_be(v: float) -> bytes:
    return struct.pack(">f", v)


def pad_even(data: bytes) -> bytes:
    if len(data) % 2 == 1:
        return data + b"\x00"
    return data


def pascal_string_even(name: str) -> bytes:
    """Pascal string padded to even length (Photoshop image resource name)."""
    raw = name.encode("ascii")
    body = bytes([len(raw)]) + raw
    if len(body) % 2 == 1:
        body += b"\x00"
    return body


def pascal_name_layer(name: str) -> bytes:
    """Pascal string padded to multiple of 4 (layer name in extra data)."""
    raw = name.encode("ascii")
    body = bytes([len(raw)]) + raw
    pad = (4 - (len(body) % 4)) % 4
    return body + (b"\x00" * pad)


def packbits_row(row: bytes) -> bytes:
    """Naive PackBits: store the whole row as one literal run (row len <= 128)."""
    assert 1 <= len(row) <= 128
    return bytes([len(row) - 1]) + row


# ---------------------------------------------------------------------------
# Synthetic ICC profiles
# ---------------------------------------------------------------------------


def s15fixed16(v: float) -> bytes:
    """Encode a signed 15.16 fixed-point value."""
    return i32(int(round(v * 65536.0)))


def icc_tag_cicp(*, transfer: int, primaries: int = 1, matrix: int = 0, full_range: int = 1) -> bytes:
    """cicp tag: type 'cicp' + reserved + 4 u8 fields."""
    return b"cicp" + u32(0) + u8(primaries) + u8(transfer) + u8(matrix) + u8(full_range)


def icc_tag_lumi(*, y_nits: float) -> bytes:
    """lumi tag as XYZType with Y = nits (s15Fixed16), X=Z=0."""
    return b"XYZ " + u32(0) + s15fixed16(0.0) + s15fixed16(y_nits) + s15fixed16(0.0)


def icc_tag_desc(text: str) -> bytes:
    """desc tag as textDescriptionType (ICC v2)."""
    ascii_bytes = text.encode("ascii") + b"\x00"
    body = bytearray()
    body += b"desc"
    body += u32(0)
    body += u32(len(ascii_bytes))
    body += ascii_bytes
    body += u32(0)  # unicode language code
    body += u32(0)  # unicode count
    body += u16(0)  # scriptcode code
    body += u8(0)  # scriptcode count
    body += b"\x00" * 67  # scriptcode string (fixed 67 bytes)
    return bytes(body)


def build_icc_profile(*, tags: dict[bytes, bytes]) -> bytes:
    """Minimal ICC profile: 128-byte header + tag table + tag data."""
    n = len(tags)
    tag_table_size = 4 + n * 12
    data_start = 128 + tag_table_size

    ordered = list(tags.items())
    offsets: list[int] = []
    sizes: list[int] = []
    payloads: list[bytes] = []
    cursor = data_start
    for _sig, payload in ordered:
        pad = (4 - (cursor % 4)) % 4
        cursor += pad
        offsets.append(cursor)
        sizes.append(len(payload))
        payloads.append((b"\x00" * pad) + payload)
        cursor += len(payload)

    profile_size = cursor
    pad_end = (4 - (profile_size % 4)) % 4
    profile_size += pad_end

    header = bytearray(128)
    struct.pack_into(">I", header, 0, profile_size)
    header[4:8] = b"\x00" * 4
    header[8:12] = bytes([0x02, 0x40, 0x00, 0x00])
    header[12:16] = b"mntr"
    header[16:20] = b"RGB "
    header[20:24] = b"XYZ "
    header[36:40] = b"acsp"
    header[40:44] = b"APPL"
    struct.pack_into(">I", header, 64, 0)
    header[68:80] = s15fixed16(0.9642) + s15fixed16(1.0) + s15fixed16(0.8249)
    header[80:84] = b"sig "

    out = bytearray()
    out += header
    out += u32(n)
    for (sig, _), off, sz in zip(ordered, offsets, sizes):
        out += sig + u32(off) + u32(sz)
    for p in payloads:
        out += p
    out += b"\x00" * pad_end
    assert len(out) == profile_size
    return bytes(out)


def ir_block(resource_id: int, data: bytes, name: str = "") -> bytes:
    """Photoshop image resource: 8BIM + id + pascal name + len + data (pad even)."""
    out = bytearray()
    out += b"8BIM"
    out += u16(resource_id)
    out += pascal_string_even(name)
    out += u32(len(data))
    out += pad_even(data)
    return bytes(out)


def ir_icc(profile: bytes) -> bytes:
    return ir_block(1039, profile)


def patterned_jpeg_rgb(width: int = 8, height: int = 8) -> bytes:
    """RGB JPEG with spatial variance so P3 zero-information barrier accepts it.

    Prefers Pillow when available; otherwise falls back to a tiny hand-rolled
    grayscale JPEG expanded is not enough -- require Pillow for this fixture.
    """
    try:
        from PIL import Image  # type: ignore
        import io

        img = Image.new("RGB", (width, height))
        px = img.load()
        for y in range(height):
            for x in range(width):
                px[x, y] = ((x * 37) & 255, (y * 53) & 255, ((x + y) * 19) & 255)
        buf = io.BytesIO()
        img.save(buf, format="JPEG", quality=95)
        return buf.getvalue()
    except Exception as exc:  # noqa: BLE001 -- fixture helper
        raise RuntimeError(
            "Pillow is required to generate rgb8_p3_only.psd (pip install pillow)"
        ) from exc


def ir_thumbnail_jpeg(jpeg: bytes, width: int, height: int) -> bytes:
    """IR 1036 Photoshop thumbnail resource (JPEG format)."""
    widthbytes = width * 3
    header = (
        u32(1)
        + u32(width)
        + u32(height)
        + u32(widthbytes)
        + u32(len(jpeg))
        + u32(len(jpeg))
        + u16(24)
        + u16(1)
    )
    return ir_block(1036, header + jpeg)


# ---------------------------------------------------------------------------
# Planar image data builders
# ---------------------------------------------------------------------------


def planar_rgb8(width: int, height: int, pattern: bool = True) -> bytes:
    n = width * height
    planar = bytearray(3 * n)
    for y in range(height):
        for x in range(width):
            i = y * width + x
            if pattern:
                planar[i] = (x * 17 + y * 3) & 0xFF
                planar[n + i] = (x * 9 + y * 11) & 0xFF
                planar[2 * n + i] = (255 - x * 13 - y * 5) & 0xFF
    return bytes(planar)


def planar_cmyk8(width: int, height: int) -> bytes:
    n = width * height
    planar = bytearray(4 * n)
    for y in range(height):
        for x in range(width):
            i = y * width + x
            planar[i] = (x * 11) & 0xFF
            planar[n + i] = (y * 13) & 0xFF
            planar[2 * n + i] = (x * 7 + y * 5) & 0xFF
            planar[3 * n + i] = 32
    return bytes(planar)


def planar_gray16(width: int, height: int) -> bytes:
    """Big-endian u16 grayscale samples."""
    out = bytearray()
    for y in range(height):
        for x in range(width):
            v = (x * 4000 + y * 1000) & 0xFFFF
            out += u16(v)
    return bytes(out)


def planar_rgb16(width: int, height: int, pattern: bool = True) -> bytes:
    """Big-endian u16 RGB planar."""
    n = width * height
    planes = [bytearray(n * 2) for _ in range(3)]
    for y in range(height):
        for x in range(width):
            i = y * width + x
            if pattern:
                r = (x * 4000 + y * 100) & 0xFFFF
                g = (x * 2000 + y * 300) & 0xFFFF
                b = (0xFFFF - x * 1500 - y * 200) & 0xFFFF
            else:
                r = g = b = 0
            planes[0][i * 2 : i * 2 + 2] = u16(r)
            planes[1][i * 2 : i * 2 + 2] = u16(g)
            planes[2][i * 2 : i * 2 + 2] = u16(b)
    return bytes(planes[0] + planes[1] + planes[2])


def planar_rgb32(
    width: int,
    height: int,
    *,
    hdr: bool = True,
    blank: bool = False,
) -> bytes:
    """Big-endian IEEE754 float RGB planar. hdr=True puts some values > 1.0."""
    n = width * height
    planes = [bytearray(n * 4) for _ in range(3)]
    for y in range(height):
        for x in range(width):
            i = y * width + x
            if blank:
                r = g = b = 0.0
            elif hdr:
                r = 0.5 + (x / max(width - 1, 1)) * 2.0
                g = 0.25 + (y / max(height - 1, 1)) * 1.5
                b = 1.2 if (x + y) % 2 == 0 else 0.8
            else:
                r = x / max(width - 1, 1)
                g = y / max(height - 1, 1)
                b = 0.5
            planes[0][i * 4 : i * 4 + 4] = f32_be(r)
            planes[1][i * 4 : i * 4 + 4] = f32_be(g)
            planes[2][i * 4 : i * 4 + 4] = f32_be(b)
    return bytes(planes[0] + planes[1] + planes[2])


def image_data_raw(planar: bytes) -> bytes:
    return u16(0) + planar


def image_data_rle(
    planar: bytes, width: int, height: int, channels: int, bytes_per_sample: int = 1
) -> bytes:
    """PackBits RLE for 8-bit planar data."""
    assert bytes_per_sample == 1
    row_counts: list[int] = []
    rows: list[bytes] = []
    row_bytes = width * bytes_per_sample
    for ch in range(channels):
        base = ch * height * row_bytes
        for y in range(height):
            row = planar[base + y * row_bytes : base + (y + 1) * row_bytes]
            packed = packbits_row(row)
            row_counts.append(len(packed))
            rows.append(packed)
    out = bytearray()
    out += u16(1)
    for c in row_counts:
        out += u16(c)
    for r in rows:
        out += r
    return bytes(out)


# ---------------------------------------------------------------------------
# Layer helpers
# ---------------------------------------------------------------------------


def solid_channel_raw(w: int, h: int, value: int, depth: int = 8) -> bytes:
    """Compression 0 + solid channel samples."""
    if depth == 8:
        return u16(0) + bytes([value]) * (w * h)
    if depth == 16:
        return u16(0) + (u16(value) * (w * h))
    if depth == 32:
        fv = float(value)
        return u16(0) + (f32_be(fv) * (w * h))
    raise ValueError(depth)


def layer_channel_payload_rgb(
    w: int, h: int, r: int, g: int, b: int, a: int, depth: int = 8
) -> bytes:
    return (
        solid_channel_raw(w, h, a, depth)
        + solid_channel_raw(w, h, r, depth)
        + solid_channel_raw(w, h, g, depth)
        + solid_channel_raw(w, h, b, depth)
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
    hidden: bool = False,
) -> bytes:
    out = bytearray()
    out += i32(top) + i32(left) + i32(bottom) + i32(right)
    out += u16(4)
    for ch_id, data_len in zip((-1, 0, 1, 2), channel_data_lens):
        out += i16(ch_id)
        out += u32(data_len)
    out += b"8BIM"
    out += b"norm"
    out += u8(opacity)
    out += u8(clipping)
    flags = 0x02 if hidden else 0x00
    out += u8(flags)
    out += u8(0)

    name_bytes = pascal_name_layer(name)
    extra = u32(0) + u32(0) + name_bytes
    out += u32(len(extra))
    out += extra
    return bytes(out)


def build_layer_and_mask(
    records: bytes, channel_image_data: bytes, layer_count: int
) -> bytes:
    layer_info_body = i16(layer_count) + records + channel_image_data
    if len(layer_info_body) % 2 == 1:
        layer_info_body += b"\x00"
    layer_info = u32(len(layer_info_body)) + layer_info_body
    layer_and_mask = layer_info + u32(0)
    if len(layer_and_mask) % 2 == 1:
        layer_and_mask += b"\x00"
    return layer_and_mask


def one_visible_rgb_layer(
    canvas_w: int,
    canvas_h: int,
    *,
    depth: int,
    color: tuple[int, int, int, int],
) -> bytes:
    """Single visible RGB layer covering a small rect with solid color."""
    left, top = 1, 1
    right, bottom = min(canvas_w, 5), min(canvas_h, 3)
    w = right - left
    h = bottom - top
    r, g, b, a = color
    if depth == 16:

        def s16(v: int) -> int:
            return v if v > 255 else (v * 257)

        payload = layer_channel_payload_rgb(
            w, h, s16(r), s16(g), s16(b), s16(a), depth=16
        )
    elif depth == 32:

        def solid_f(val: float) -> bytes:
            return u16(0) + (f32_be(val) * (w * h))

        payload = (
            solid_f(1.0)
            + solid_f(float(r) if r <= 1 else r / 255.0)
            + solid_f(float(g) if g <= 1 else g / 255.0)
            + solid_f(float(b) if b <= 1 else b / 255.0)
        )
    else:
        payload = layer_channel_payload_rgb(w, h, r, g, b, a, depth=8)

    n = len(payload) // 4
    lenses = [n, n, n, n]
    rec = layer_record(
        top,
        left,
        bottom,
        right,
        opacity=255,
        clipping=0,
        name="Layer0",
        channel_data_lens=lenses,
        hidden=False,
    )
    return build_layer_and_mask(rec, payload, 1)


def hidden_rgb_layer(canvas_w: int, canvas_h: int) -> bytes:
    left, top = 0, 0
    right, bottom = min(canvas_w, 4), min(canvas_h, 2)
    w = right - left
    h = bottom - top
    payload = layer_channel_payload_rgb(w, h, 200, 100, 50, 255, depth=8)
    n = len(payload) // 4
    lenses = [n, n, n, n]
    rec = layer_record(
        top,
        left,
        bottom,
        right,
        opacity=255,
        clipping=0,
        name="Hidden",
        channel_data_lens=lenses,
        hidden=True,
    )
    return build_layer_and_mask(rec, payload, 1)


def layer_channel_payload_cmyk(
    w: int, h: int, c: int, m: int, y: int, k: int, depth: int = 16
) -> bytes:
    """CMYK channel payload without transparency (ids 0..3)."""
    return (
        solid_channel_raw(w, h, c, depth)
        + solid_channel_raw(w, h, m, depth)
        + solid_channel_raw(w, h, y, depth)
        + solid_channel_raw(w, h, k, depth)
    )


def layer_record_cmyk(
    top: int,
    left: int,
    bottom: int,
    right: int,
    *,
    opacity: int,
    name: str,
    channel_data_lens: list[int],
    hidden: bool = False,
) -> bytes:
    out = bytearray()
    out += i32(top) + i32(left) + i32(bottom) + i32(right)
    out += u16(4)
    for ch_id, data_len in zip((0, 1, 2, 3), channel_data_lens):
        out += i16(ch_id)
        out += u32(data_len)
    out += b"8BIM"
    out += b"norm"
    out += u8(opacity)
    out += u8(0)
    flags = 0x02 if hidden else 0x00
    out += u8(flags)
    out += u8(0)

    name_bytes = pascal_name_layer(name)
    extra = u32(0) + u32(0) + name_bytes
    out += u32(len(extra))
    out += extra
    return bytes(out)


def one_visible_cmyk_layer(
    canvas_w: int,
    canvas_h: int,
    *,
    depth: int,
    color: tuple[int, int, int, int],
) -> bytes:
    """Single visible CMYK layer covering a small rect."""
    left, top = 1, 1
    right, bottom = min(canvas_w, 5), min(canvas_h, 3)
    w = right - left
    h = bottom - top
    c, m, y, k = color
    if depth == 16:

        def s16(v: int) -> int:
            return v if v > 255 else (v * 257)

        payload = layer_channel_payload_cmyk(
            w, h, s16(c), s16(m), s16(y), s16(k), depth=16
        )
    else:
        payload = layer_channel_payload_cmyk(w, h, c, m, y, k, depth=depth)

    n = len(payload) // 4
    lenses = [n, n, n, n]
    rec = layer_record_cmyk(
        top,
        left,
        bottom,
        right,
        opacity=255,
        name="CyanLayer",
        channel_data_lens=lenses,
        hidden=False,
    )
    return build_layer_and_mask(rec, payload, 1)


def planar_cmyk16(width: int, height: int, *, blank: bool = False) -> bytes:
    """Big-endian u16 CMYK planar. blank=True writes full-ink zeros."""
    n = width * height
    planes = [bytearray(n * 2) for _ in range(4)]
    for y in range(height):
        for x in range(width):
            i = y * width + x
            if blank:
                c = m = yv = k = 0
            else:
                c = (x * 4000) & 0xFFFF
                m = (y * 3000) & 0xFFFF
                yv = (x * 2000 + y * 1000) & 0xFFFF
                k = 0x2000
            planes[0][i * 2 : i * 2 + 2] = u16(c)
            planes[1][i * 2 : i * 2 + 2] = u16(m)
            planes[2][i * 2 : i * 2 + 2] = u16(yv)
            planes[3][i * 2 : i * 2 + 2] = u16(k)
    return bytes(planes[0] + planes[1] + planes[2] + planes[3])


# ---------------------------------------------------------------------------
# PSD/PSB file assembly
# ---------------------------------------------------------------------------


def write_file_header(
    *,
    version: int,
    channels: int,
    height: int,
    width: int,
    depth: int,
    color_mode: int,
) -> bytes:
    out = bytearray()
    out += b"8BPS"
    out += u16(version)
    out += b"\x00" * 6
    out += u16(channels)
    out += u32(height)
    out += u32(width)
    out += u16(depth)
    out += u16(color_mode)
    return bytes(out)


def assemble_psd(
    *,
    version: int = 1,
    channels: int,
    height: int,
    width: int,
    depth: int,
    color_mode: int,
    image_resources: bytes = b"",
    layer_and_mask: bytes = b"",
    image_data: bytes,
) -> bytes:
    out = bytearray()
    out += write_file_header(
        version=version,
        channels=channels,
        height=height,
        width=width,
        depth=depth,
        color_mode=color_mode,
    )
    out += u32(0)
    out += u32(len(image_resources))
    out += image_resources
    if version == 2:
        out += struct.pack(">Q", len(layer_and_mask))
    else:
        out += u32(len(layer_and_mask))
    out += layer_and_mask
    out += image_data
    return bytes(out)


# ---------------------------------------------------------------------------
# Fixture builders
# ---------------------------------------------------------------------------


def build_fixtures() -> list[dict]:
    """Build all fixtures; return manifest entries."""
    manifest: list[dict] = []
    W, H = 8, 4

    def write(name: str, data: bytes, branch: str, depth: int, notes: str) -> None:
        path = OUT_DIR / name
        path.write_bytes(data)
        print(f"wrote {path} ({len(data)} bytes)")
        manifest.append(
            {
                "file": name,
                "expected_branch": branch,
                "depth": depth,
                "notes": notes,
            }
        )

    # 1. rgb8_flat.psd -- PackBits RLE
    planar8 = planar_rgb8(W, H, pattern=True)
    data = assemble_psd(
        channels=3,
        height=H,
        width=W,
        depth=8,
        color_mode=3,
        image_data=image_data_rle(planar8, W, H, 3),
    )
    write(
        "rgb8_flat.psd",
        data,
        "sdr_p1",
        8,
        "8-bit RGB flat with PackBits RLE Image Data",
    )

    # 2. rgb16_flat_no_icc.psd
    data = assemble_psd(
        channels=3,
        height=H,
        width=W,
        depth=16,
        color_mode=3,
        image_data=image_data_raw(planar_rgb16(W, H)),
    )
    write(
        "rgb16_flat_no_icc.psd",
        data,
        "sdr_p1",
        16,
        "16-bit RGB flat, no ICC (high-precision SDR)",
    )

    # 3. rgb32_flat.psd
    data = assemble_psd(
        channels=3,
        height=H,
        width=W,
        depth=32,
        color_mode=3,
        image_data=image_data_raw(planar_rgb32(W, H, hdr=True)),
    )
    write(
        "rgb32_flat.psd",
        data,
        "hdr_p1",
        32,
        "32-bit RGB flat with BE floats >1.0 (HDR when HDR env)",
    )

    # 4. rgb16_flat_cicp_pq.psd
    icc_pq = build_icc_profile(tags={b"cicp": icc_tag_cicp(transfer=16)})
    data = assemble_psd(
        channels=3,
        height=H,
        width=W,
        depth=16,
        color_mode=3,
        image_resources=ir_icc(icc_pq),
        image_data=image_data_raw(planar_rgb16(W, H)),
    )
    write(
        "rgb16_flat_cicp_pq.psd",
        data,
        "hdr_p1",
        16,
        "16-bit RGB + synthetic ICC cicp transfer=16 (PQ) in IR 1039",
    )

    # 5. rgb16_flat_cicp_hlg.psd
    icc_hlg = build_icc_profile(tags={b"cicp": icc_tag_cicp(transfer=18)})
    data = assemble_psd(
        channels=3,
        height=H,
        width=W,
        depth=16,
        color_mode=3,
        image_resources=ir_icc(icc_hlg),
        image_data=image_data_raw(planar_rgb16(W, H)),
    )
    write(
        "rgb16_flat_cicp_hlg.psd",
        data,
        "hdr_p1",
        16,
        "16-bit RGB + synthetic ICC cicp transfer=18 (HLG)",
    )

    # 6. rgb16_flat_lumi_high.psd
    icc_lumi = build_icc_profile(tags={b"lumi": icc_tag_lumi(y_nits=1000.0)})
    data = assemble_psd(
        channels=3,
        height=H,
        width=W,
        depth=16,
        color_mode=3,
        image_resources=ir_icc(icc_lumi),
        image_data=image_data_raw(planar_rgb16(W, H)),
    )
    write(
        "rgb16_flat_lumi_high.psd",
        data,
        "hdr_p1",
        16,
        "16-bit RGB + ICC lumi Y=1000 nits",
    )

    # 7. rgb16_flat_desc_hdr10.psd
    icc_desc = build_icc_profile(tags={b"desc": icc_tag_desc("HDR10 Display Profile")})
    data = assemble_psd(
        channels=3,
        height=H,
        width=W,
        depth=16,
        color_mode=3,
        image_resources=ir_icc(icc_desc),
        image_data=image_data_raw(planar_rgb16(W, H)),
    )
    write(
        "rgb16_flat_desc_hdr10.psd",
        data,
        "hdr_p1",
        16,
        "16-bit RGB + ICC desc containing HDR10",
    )

    # 8. rgb32_blank_p1_layers.psd
    layers32 = one_visible_rgb_layer(W, H, depth=32, color=(2, 1, 1, 1))
    data = assemble_psd(
        channels=3,
        height=H,
        width=W,
        depth=32,
        color_mode=3,
        layer_and_mask=layers32,
        image_data=image_data_raw(planar_rgb32(W, H, blank=True)),
    )
    write(
        "rgb32_blank_p1_layers.psd",
        data,
        "hdr_p2",
        32,
        "32-bit blank/solid flat Image Data + one visible RGB layer",
    )

    # 9. rgb8_blank_p1_layers.psd
    layers8 = one_visible_rgb_layer(W, H, depth=8, color=(220, 40, 40, 255))
    data = assemble_psd(
        channels=3,
        height=H,
        width=W,
        depth=8,
        color_mode=3,
        layer_and_mask=layers8,
        image_data=image_data_raw(planar_rgb8(W, H, pattern=False)),
    )
    write(
        "rgb8_blank_p1_layers.psd",
        data,
        "sdr_p2",
        8,
        "8-bit blank flat Image Data + visible RGB layer",
    )

    # 10. rgb8_p3_only.psd
    thumb_w, thumb_h = 8, 8
    jpeg = patterned_jpeg_rgb(thumb_w, thumb_h)
    resources = ir_thumbnail_jpeg(jpeg, thumb_w, thumb_h)
    hidden = hidden_rgb_layer(W, H)
    data = assemble_psd(
        channels=3,
        height=H,
        width=W,
        depth=8,
        color_mode=3,
        image_resources=resources,
        layer_and_mask=hidden,
        image_data=image_data_raw(planar_rgb8(W, H, pattern=False)),
    )
    write(
        "rgb8_p3_only.psd",
        data,
        "sdr_p3",
        8,
        "blank flat, hidden layer, IR 1036 JPEG thumbnail (P3 fallback)",
    )

    # 11. gray16_flat.psd
    data = assemble_psd(
        channels=1,
        height=H,
        width=W,
        depth=16,
        color_mode=1,
        image_data=image_data_raw(planar_gray16(W, H)),
    )
    write(
        "gray16_flat.psd",
        data,
        "sdr_p1",
        16,
        "16-bit grayscale flat, no ICC",
    )

    # 12. rgb32_psb.psb
    data = assemble_psd(
        version=2,
        channels=3,
        height=H,
        width=W,
        depth=32,
        color_mode=3,
        image_data=image_data_raw(planar_rgb32(W, H, hdr=True)),
    )
    write(
        "rgb32_psb.psb",
        data,
        "hdr_p1",
        32,
        "PSB v2 32-bit small flat with floats >1.0",
    )

    # 13. cmyk8_flat.psd
    data = assemble_psd(
        channels=4,
        height=H,
        width=W,
        depth=8,
        color_mode=4,
        image_data=image_data_raw(planar_cmyk8(W, H)),
    )
    write(
        "cmyk8_flat.psd",
        data,
        "sdr_p1",
        8,
        "8-bit CMYK flat control",
    )

    # 14. rgb16_blank_p1_layers.psd -- SDR env layers-only (no HDR ICC)
    layers16 = one_visible_rgb_layer(W, H, depth=16, color=(220, 40, 40, 255))
    data = assemble_psd(
        channels=3,
        height=H,
        width=W,
        depth=16,
        color_mode=3,
        layer_and_mask=layers16,
        image_data=image_data_raw(planar_rgb16(W, H, pattern=False)),
    )
    write(
        "rgb16_blank_p1_layers.psd",
        data,
        "sdr_p2",
        16,
        "16-bit RGB blank flat + visible layer (SDR P2 via f32 composite)",
    )

    # 15. cmyk16_blank_p1_layers.psd -- print layers-only on SDR
    layers_cmyk16 = one_visible_cmyk_layer(W, H, depth=16, color=(0, 255, 255, 64))
    data = assemble_psd(
        channels=4,
        height=H,
        width=W,
        depth=16,
        color_mode=4,
        layer_and_mask=layers_cmyk16,
        image_data=image_data_raw(planar_cmyk16(W, H, blank=True)),
    )
    write(
        "cmyk16_blank_p1_layers.psd",
        data,
        "sdr_env_p2",
        16,
        "16-bit CMYK blank flat + visible layer (force SDR main P2)",
    )

    # 16. rgb32_blank_p1_layers_sdr.psd -- 32-bit layers-only when env is SDR
    layers32_sdr = one_visible_rgb_layer(W, H, depth=32, color=(2, 1, 1, 1))
    data = assemble_psd(
        channels=3,
        height=H,
        width=W,
        depth=32,
        color_mode=3,
        layer_and_mask=layers32_sdr,
        image_data=image_data_raw(planar_rgb32(W, H, blank=True)),
    )
    write(
        "rgb32_blank_p1_layers_sdr.psd",
        data,
        "sdr_env_p2",
        32,
        "32-bit blank flat + visible layer decoded via SDR main (env capacity 1.0)",
    )

    return manifest


def main() -> None:
    OUT_DIR.mkdir(parents=True, exist_ok=True)
    manifest = build_fixtures()
    man_path = OUT_DIR / "manifest.json"
    text = json.dumps(manifest, indent=2, ensure_ascii=True) + "\n"
    man_path.write_bytes(text.encode("ascii"))
    print(f"wrote {man_path} ({len(manifest)} entries)")


if __name__ == "__main__":
    main()