#!/usr/bin/env python3
"""Upscale a GContainer Ultra HDR JPEG while preserving gain-map HDR data.

ImageMagick cannot resize a composite Ultra HDR file in one pass — it drops the
trailing gain-map JPEG and GContainer XMP. This script instead:

  1. Splits primary + trailing gain-map using ``Item:Length`` (Adobe GContainer)
  2. Upscales both JPEG streams with the same scale factor (``magick``)
  3. Updates ``Item:Length`` in the primary XMP and re-appends the gain map

MPF-only Camera Raw exports are not rebuilt here; re-export at target resolution
or start from a GContainer sample (e.g. Ultra_HDR_Samples Originals).

Usage (from repo root):
  python scripts/generate_8k_ultra_hdr_sample.py F:/HDR/Ultra_HDR_Samples/Originals/Ultra_HDR_Samples_Originals_01.jpg
  python scripts/generate_8k_ultra_hdr_sample.py input.jpg -o tests/data/ultra_hdr_8192.jpg --long-edge 8192
"""
from __future__ import annotations

import argparse
import re
import shutil
import struct
import subprocess
import sys
import tempfile
from pathlib import Path

JPEG_SOI = b"\xff\xd8"
JPEG_EOI = b"\xff\xd9"
HDR_GAIN_MAP_NAMESPACE = "http://ns.adobe.com/hdr-gain-map/1.0/"
# Keep in sync with `constants::ABSOLUTE_MAX_TEXTURE_SIDE` and `tile_cache::TILED_THRESHOLD`.
SIV_ABSOLUTE_MAX_TEXTURE_SIDE = 8192
SIV_TILED_THRESHOLD_PIXELS = 64_000_000


def has_magick() -> bool:
    return shutil.which("magick") is not None


def jpeg_dimensions_from_bytes(data: bytes) -> tuple[int, int]:
    """Read SOF dimensions without decoding pixels (safe for huge Ultra HDR files)."""
    i = 0
    while i + 1 < len(data):
        if data[i] != 0xFF:
            i += 1
            continue
        marker = data[i + 1]
        if marker in (0xC0, 0xC2, 0xC1, 0xC3) and i + 9 <= len(data):
            height = (data[i + 5] << 8) | data[i + 6]
            width = (data[i + 7] << 8) | data[i + 8]
            return width, height
        if marker in (0xD8, 0xD9):
            i += 2
            continue
        if marker == 0xDA:
            break
        if i + 4 > len(data):
            break
        seg_len = (data[i + 2] << 8) | data[i + 3]
        i += 2 + seg_len
    raise RuntimeError("Could not determine JPEG dimensions from SOF")


def jpeg_dimensions(path: Path) -> tuple[int, int]:
    return jpeg_dimensions_from_bytes(path.read_bytes())


def primary_jpeg_bytes(path: Path) -> bytes:
    data = path.read_bytes()
    length = gain_map_trailer_length(data)
    if length is not None:
        return data[:-length]
    return data


def primary_dimensions(path: Path) -> tuple[int, int]:
    return jpeg_dimensions_from_bytes(primary_jpeg_bytes(path))


def long_edge(path: Path) -> int:
    return max(primary_dimensions(path))


def siv_routes_hdr_tiled(width: int, height: int) -> bool:
    pixel_count = width * height
    max_side = max(width, height)
    return (
        pixel_count >= SIV_TILED_THRESHOLD_PIXELS
        or max_side >= SIV_ABSOLUTE_MAX_TEXTURE_SIDE
    )


def gain_map_trailer_length(data: bytes) -> int | None:
    text = data.decode("latin-1", errors="replace")
    for pattern in (
        r'Item:Semantic="GainMap"[^>]*Item:Length="(\d+)"',
        r"Item:Semantic='GainMap'[^>]*Item:Length='(\d+)'",
        r'Item:Length="(\d+)"[^>]*Item:Semantic="GainMap"',
        r"Item:Length='(\d+)'[^>]*Item:Semantic='GainMap'",
    ):
        match = re.search(pattern, text, flags=re.DOTALL)
        if match:
            return int(match.group(1))
    semantic_index = text.find('Item:Semantic="GainMap"')
    if semantic_index < 0:
        semantic_index = text.find("Item:Semantic='GainMap'")
    if semantic_index < 0:
        return None
    item_start = text.rfind("<Container:Item", 0, semantic_index)
    if item_start < 0:
        return None
    item_end = text.find(">", semantic_index)
    if item_end < 0:
        return None
    item = text[item_start:item_end]
    for name in ("Item:Length", "Length"):
        match = re.search(rf'{name}="(\d+)"', item)
        if match:
            return int(match.group(1))
        match = re.search(rf"{name}='(\d+)'", item)
        if match:
            return int(match.group(1))
    return None


def split_gcontainer_ultra_hdr(data: bytes) -> tuple[bytes, bytes]:
    length = gain_map_trailer_length(data)
    if length is None:
        raise ValueError(
            "Not a GContainer Ultra HDR JPEG (missing Container Item:Length for GainMap). "
            "MPF-only files must be re-exported at target resolution."
        )
    if length > len(data):
        raise ValueError("Gain-map Item:Length exceeds file size")
    primary = data[:-length]
    gain = data[-length:]
    if not primary.endswith(JPEG_EOI):
        raise ValueError("Primary JPEG does not end with EOI before gain-map trailer")
    if not gain.startswith(JPEG_SOI) or not gain.endswith(JPEG_EOI):
        raise ValueError("Gain-map trailer is not a complete JPEG stream")
    return primary, gain


def patch_xmp_item_length(payload: bytes, new_len: int) -> bytes:
    text = payload.decode("latin-1")
    if "GainMap" not in text or "Item:Length" not in text:
        return payload
    new_text, count = re.subn(
        r'(Item:Length=")(\d+)(")', rf"\g<1>{new_len}\g<3>", text, count=1
    )
    if count == 0:
        new_text, count = re.subn(
            r"(Item:Length=')(\d+)(')", rf"\g<1>{new_len}\g<3>", text, count=1
        )
    if count == 0:
        raise ValueError("Could not locate Item:Length in GContainer XMP")
    return new_text.encode("latin-1")


def rebuild_primary_metadata(jpeg: bytes, patch_app1) -> bytes:
    if jpeg[:2] != JPEG_SOI:
        raise ValueError("Primary stream is not a JPEG")
    out = bytearray(JPEG_SOI)
    i = 2
    while i + 1 < len(jpeg):
        if jpeg[i] != 0xFF:
            raise ValueError(f"Invalid JPEG marker at byte {i}")
        while i < len(jpeg) and jpeg[i] == 0xFF:
            i += 1
        marker = jpeg[i]
        i += 1
        if marker in (0xDA, 0xD9):  # SOS or EOI — copy remainder unchanged
            out.append(0xFF)
            out.append(marker)
            out.extend(jpeg[i:])
            return bytes(out)
        if marker == 0x01 or (0xD0 <= marker <= 0xD7):
            out.append(0xFF)
            out.append(marker)
            continue
        if i + 2 > len(jpeg):
            raise ValueError("Truncated JPEG segment length")
        seg_len = struct.unpack(">H", jpeg[i : i + 2])[0]
        if seg_len < 2:
            raise ValueError(f"Invalid JPEG segment length {seg_len}")
        payload_end = i + seg_len
        if payload_end > len(jpeg):
            raise ValueError("Truncated JPEG segment payload")
        payload = jpeg[i + 2 : payload_end]
        if marker == 0xE1:
            payload = patch_app1(payload)
        out.append(0xFF)
        out.append(marker)
        out.extend(struct.pack(">H", len(payload) + 2))
        out.extend(payload)
        i = payload_end
    raise ValueError("Primary JPEG missing SOS/EOI")


def upscale_jpeg_magick(src: Path, dst: Path, width: int, height: int, quality: int) -> None:
    dst.parent.mkdir(parents=True, exist_ok=True)
    cmd = [
        "magick",
        str(src),
        "-filter",
        "Lanczos",
        "-resize",
        f"{width}x{height}!",
        "-quality",
        str(quality),
        str(dst),
    ]
    print("Running:", " ".join(cmd))
    subprocess.run(cmd, check=True)


def verify_primary_metadata_parseable(primary: bytes) -> None:
    """Match Rust ``primary_metadata_segments`` — must reach SOS without error."""
    i = 2
    while i + 1 < len(primary):
        if primary[i] != 0xFF:
            raise ValueError(f"Primary JPEG metadata corrupt at byte {i}")
        while i < len(primary) and primary[i] == 0xFF:
            i += 1
        marker = primary[i]
        i += 1
        if marker in (0xDA, 0xD9):
            return
        if marker == 0x01 or (0xD0 <= marker <= 0xD7):
            continue
        if i + 2 > len(primary):
            raise ValueError("Primary JPEG metadata truncated")
        seg_len = struct.unpack(">H", primary[i : i + 2])[0]
        if seg_len < 2 or i + seg_len > len(primary):
            raise ValueError(f"Primary JPEG segment length {seg_len} invalid at {i}")
        i += seg_len
    raise ValueError("Primary JPEG missing SOS")


def verify_ultra_hdr_output(path: Path) -> None:
    data = path.read_bytes()
    text = data.decode("latin-1", errors="replace")
    if HDR_GAIN_MAP_NAMESPACE not in text or "GainMap" not in text:
        raise ValueError("Output is missing Ultra HDR / GContainer gain-map XMP")
    length = gain_map_trailer_length(data)
    if length is None:
        raise ValueError("Output is missing Item:Length for GainMap")
    gain = data[-length:]
    if not gain.startswith(JPEG_SOI) or not gain.endswith(JPEG_EOI):
        raise ValueError("Output gain-map trailer is not a valid JPEG")
    primary = data[:-length]
    if not primary.endswith(JPEG_EOI):
        raise ValueError("Output primary JPEG is malformed")
    verify_primary_metadata_parseable(primary)
    # Rust ``inspect_ultra_hdr_jpeg_bytes`` requirements:
    has_ns_ver = HDR_GAIN_MAP_NAMESPACE in text and "hdrgm:Version" in text
    has_gain_item = 'Item:Semantic="GainMap"' in text or "Item:Semantic='GainMap'" in text
    if not (has_ns_ver and has_gain_item):
        raise ValueError(
            "Primary XMP missing hdrgm:Version or Container GainMap item (viewer will not route HDR)"
        )


def upscale_gcontainer_ultra_hdr(
    src: Path,
    dst: Path,
    long_edge_px: int,
    *,
    quality: int = 95,
) -> None:
    raw = src.read_bytes()
    primary_bytes, gain_bytes = split_gcontainer_ultra_hdr(raw)

    with tempfile.TemporaryDirectory(prefix="siv-ultra-hdr-upscale-") as tmp:
        tmp_dir = Path(tmp)
        primary_path = tmp_dir / "primary.jpg"
        gain_path = tmp_dir / "gain.jpg"
        primary_path.write_bytes(primary_bytes)
        gain_path.write_bytes(gain_bytes)

        primary_w, primary_h = jpeg_dimensions(primary_path)
        gain_w, gain_h = jpeg_dimensions(gain_path)
        src_long = max(primary_w, primary_h)
        if src_long <= 0:
            raise ValueError("Source primary JPEG has zero dimensions")
        scale = long_edge_px / src_long
        out_primary_w = max(1, round(primary_w * scale))
        out_primary_h = max(1, round(primary_h * scale))
        out_gain_w = max(1, round(gain_w * scale))
        out_gain_h = max(1, round(gain_h * scale))

        up_primary_path = tmp_dir / "primary_up.jpg"
        up_gain_path = tmp_dir / "gain_up.jpg"
        upscale_jpeg_magick(primary_path, up_primary_path, out_primary_w, out_primary_h, quality)
        upscale_jpeg_magick(gain_path, up_gain_path, out_gain_w, out_gain_h, quality)

        up_primary = up_primary_path.read_bytes()
        up_gain = up_gain_path.read_bytes()
        patched_primary = rebuild_primary_metadata(
            up_primary,
            lambda payload: patch_xmp_item_length(payload, len(up_gain)),
        )
        dst.parent.mkdir(parents=True, exist_ok=True)
        dst.write_bytes(patched_primary + up_gain)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path, help="GContainer Ultra HDR JPEG")
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
        help="Target long edge in pixels for the primary/base image (default: 8192)",
    )
    parser.add_argument(
        "--quality",
        type=int,
        default=95,
        help="JPEG quality for magick re-encode (default: 95)",
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
        try:
            upscale_gcontainer_ultra_hdr(
                src, out, args.long_edge, quality=args.quality
            )
        except ValueError as err:
            print(f"Error: {err}", file=sys.stderr)
            sys.exit(1)

    after = long_edge(out)
    print(f"Wrote {out} ({after}px primary long edge)")
    if before < args.long_edge and after < args.long_edge:
        print(
            f"Error: output long edge {after}px is still below target {args.long_edge}px.",
            file=sys.stderr,
        )
        sys.exit(1)

    try:
        verify_ultra_hdr_output(out)
    except ValueError as err:
        print(f"Error: Ultra HDR verification failed: {err}", file=sys.stderr)
        sys.exit(1)

    print("Verified GContainer gain-map trailer and XMP.")
    pw, ph = primary_dimensions(out)
    pixel_count = pw * ph
    if siv_routes_hdr_tiled(pw, ph):
        print(
            f"SIV routing: ImageData::HdrTiled (primary {pw}x{ph}, "
            f"{pixel_count / 1_000_000:.1f} MP, max_side={max(pw, ph)})."
        )
    else:
        print(
            f"SIV routing: static ImageData::Hdr (primary {pw}x{ph}, "
            f"{pixel_count / 1_000_000:.1f} MP) — below tiled thresholds "
            f"(<{SIV_ABSOLUTE_MAX_TEXTURE_SIDE}px side and "
            f"<{SIV_TILED_THRESHOLD_PIXELS / 1_000_000:.0f} MP)."
        )
    print("Set SIV_ULTRA_HDR_SAMPLES_DIR or pass this path to manual GPU stress tests.")


if __name__ == "__main__":
    main()
