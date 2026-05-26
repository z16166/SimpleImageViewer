#!/usr/bin/env python3
"""Build Ultra HDR JPEG test corpora for ISO backward and BaseRenditionIsHDR paths.

Patches the trailing gain-map JPEG inside a GContainer Ultra HDR file (forward sample)
and re-appends it with an updated Item:Length.

Usage (from repo root):
  python scripts/generate_gain_map_test_samples.py F:/HDR/Ultra_HDR_Samples/Originals/Ultra_HDR_Samples_Originals_01.jpg
  python scripts/generate_gain_map_test_samples.py input.jpg -o tests/data/gain_map_samples

Requires a **GContainer** Ultra HDR JPEG (trailing gain map + Item:Length). MPF-only files
(e.g. libavif ``seine_sdr_gainmap_srgb.jpg``) are not supported by this rebuild path.
"""
from __future__ import annotations

import argparse
import importlib.util
import re
import struct
import sys
from pathlib import Path

ISO_GAIN_MAP_NAMESPACE = b"urn:iso:std:iso:ts:21496:-1\0"
ISO_BACKWARD_DIRECTION_FLAG = 0x04
XAP_PREFIX = b"http://ns.adobe.com/xap/1.0/\0"
JPEG_SOI = b"\xff\xd8"
JPEG_EOI = b"\xff\xd9"


def _load_gen8k():
    script_dir = Path(__file__).resolve().parent
    spec = importlib.util.spec_from_file_location(
        "generate_8k_ultra_hdr_sample",
        script_dir / "generate_8k_ultra_hdr_sample.py",
    )
    if spec is None or spec.loader is None:
        raise RuntimeError("Could not load generate_8k_ultra_hdr_sample.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def rebuild_jpeg_metadata(jpeg: bytes, patch_marker_payload) -> bytes:
    """Rewrite APP1/APP2 payloads via ``patch_marker_payload(marker, payload) -> payload``."""
    if jpeg[:2] != JPEG_SOI:
        raise ValueError("Not a JPEG stream")
    out = bytearray(JPEG_SOI)
    i = 2
    while i + 1 < len(jpeg):
        if jpeg[i] != 0xFF:
            raise ValueError(f"Invalid JPEG marker at byte {i}")
        while i < len(jpeg) and jpeg[i] == 0xFF:
            i += 1
        marker = jpeg[i]
        i += 1
        if marker in (0xDA, 0xD9):
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
        if marker in (0xE1, 0xE2):
            payload = patch_marker_payload(marker, payload)
        out.append(0xFF)
        out.append(marker)
        out.extend(struct.pack(">H", len(payload) + 2))
        out.extend(payload)
        i = payload_end
    raise ValueError("JPEG missing SOS/EOI")


def minimal_iso_metadata_bytes(flags: int = 0x08) -> bytes:
    """Match ``gain_map.rs`` ``minimal_iso_metadata()`` (single-channel, common denominator)."""
    out = bytearray()
    out.extend(struct.pack(">H", 0))  # minimum_version
    out.extend(struct.pack(">H", 0))  # writer_version
    out.append(flags)
    out.extend(struct.pack(">I", 10))  # denominator
    out.extend(struct.pack(">I", 0))  # base_hdr_headroom -> 2^0
    out.extend(struct.pack(">I", 20))  # alternate_hdr_headroom -> 2^2
    out.extend(struct.pack(">i", 0))  # gain_map_min
    out.extend(struct.pack(">i", 20))  # gain_map_max
    out.extend(struct.pack(">I", 10))  # gamma = 1
    out.extend(struct.pack(">i", 0))  # base_offset
    out.extend(struct.pack(">i", 0))  # alternate_offset
    return bytes(out)


def gain_jpeg_has_iso_app2(gain_jpeg: bytes) -> bool:
    i = 2
    while i + 1 < len(gain_jpeg):
        if gain_jpeg[i] != 0xFF:
            i += 1
            continue
        marker = gain_jpeg[i + 1]
        i += 2
        if marker in (0xDA, 0xD9):
            break
        seg_len = struct.unpack(">H", gain_jpeg[i : i + 2])[0]
        payload = gain_jpeg[i + 2 : i + seg_len]
        if marker == 0xE2 and payload.startswith(ISO_GAIN_MAP_NAMESPACE):
            return True
        i += seg_len
    return False


def inject_iso_app2_segment(gain_jpeg: bytes, flags: int = 0x08) -> bytes:
    """Insert ISO 21496 APP2 after the first APP1 segment when the gain map is XMP-only."""
    if gain_jpeg_has_iso_app2(gain_jpeg):
        return gain_jpeg
    iso_payload = ISO_GAIN_MAP_NAMESPACE + minimal_iso_metadata_bytes(flags)
    segment = b"\xff\xe2" + struct.pack(">H", len(iso_payload) + 2) + iso_payload

    i = 2
    while i + 1 < len(gain_jpeg):
        if gain_jpeg[i] != 0xFF:
            raise ValueError(f"Invalid JPEG marker at byte {i}")
        while i < len(gain_jpeg) and gain_jpeg[i] == 0xFF:
            i += 1
        marker = gain_jpeg[i]
        i += 1
        if marker in (0xDA, 0xD9):
            raise ValueError("Gain-map JPEG has no APP1 segment to anchor ISO APP2 insert")
        if i + 2 > len(gain_jpeg):
            raise ValueError("Truncated gain-map JPEG segment length")
        seg_len = struct.unpack(">H", gain_jpeg[i : i + 2])[0]
        payload_end = i + seg_len
        if payload_end > len(gain_jpeg):
            raise ValueError("Truncated gain-map JPEG segment payload")
        if marker == 0xE1:
            return gain_jpeg[:payload_end] + segment + gain_jpeg[payload_end:]
        i = payload_end
    raise ValueError("Gain-map JPEG missing APP1/XMP segment")


def patch_iso_backward_flag(gain_jpeg: bytes) -> bytes:
    gain_jpeg = inject_iso_app2_segment(gain_jpeg, flags=0x08)

    def patch(marker: int, payload: bytes) -> bytes:
        if marker != 0xE2 or not payload.startswith(ISO_GAIN_MAP_NAMESPACE):
            return payload
        iso = bytearray(payload[len(ISO_GAIN_MAP_NAMESPACE) :])
        if len(iso) < 5:
            raise ValueError("ISO gain-map metadata too short to patch flags byte")
        iso[4] |= ISO_BACKWARD_DIRECTION_FLAG
        return ISO_GAIN_MAP_NAMESPACE + bytes(iso)

    return rebuild_jpeg_metadata(gain_jpeg, patch)


def patch_base_rendition_is_hdr(gain_jpeg: bytes) -> bytes:
    def patch(marker: int, payload: bytes) -> bytes:
        if marker != 0xE1 or not payload.startswith(XAP_PREFIX):
            return payload
        text = payload[len(XAP_PREFIX) :].decode("latin-1")
        if "BaseRenditionIsHDR" in text:
            text = re.sub(
                r'(hdrgm:BaseRenditionIsHDR=")([^"]*)(")',
                r'\1True\3',
                text,
                count=1,
            )
        elif "hdrgm:Version" in text:
            text = text.replace(
                'hdrgm:Version="1.0"',
                'hdrgm:Version="1.0" hdrgm:BaseRenditionIsHDR="True"',
                1,
            )
        else:
            raise ValueError("Gain-map XMP missing hdrgm:Version; cannot set BaseRenditionIsHDR")
        return XAP_PREFIX + text.encode("latin-1")

    return rebuild_jpeg_metadata(gain_jpeg, patch)


def rebuild_ultra_hdr(primary: bytes, gain: bytes, patch_xmp_item_length) -> bytes:
    gain_len = len(gain)
    primary = patch_xmp_item_length(primary, gain_len)
    composite = primary + gain
    if not composite.endswith(JPEG_EOI):
        raise ValueError("Composite output does not end with JPEG EOI")
    return composite


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "input",
        type=Path,
        help="Forward GContainer Ultra HDR JPEG (e.g. Ultra_HDR_Samples_Originals_01.jpg)",
    )
    parser.add_argument(
        "-o",
        "--output-dir",
        type=Path,
        default=Path("tests/data/gain_map_samples"),
        help="Directory for patched output JPEGs",
    )
    args = parser.parse_args()

    if not args.input.is_file():
        print(f"Input not found: {args.input}", file=sys.stderr)
        return 1

    gen8k = _load_gen8k()
    data = args.input.read_bytes()
    primary, gain = gen8k.split_gcontainer_ultra_hdr(data)

    args.output_dir.mkdir(parents=True, exist_ok=True)

    backward_gain = patch_iso_backward_flag(gain)
    backward_path = args.output_dir / "sample_iso_backward.jpg"
    backward_bytes = rebuild_ultra_hdr(
        primary, backward_gain, gen8k.patch_xmp_item_length
    )
    backward_path.write_bytes(backward_bytes)
    print(f"Wrote {backward_path} ({len(backward_bytes)} bytes, ISO backward flag)")

    base_hdr_gain = patch_base_rendition_is_hdr(gain)
    base_hdr_path = args.output_dir / "sample_base_rendition_is_hdr.jpg"
    base_hdr_bytes = rebuild_ultra_hdr(
        primary, base_hdr_gain, gen8k.patch_xmp_item_length
    )
    base_hdr_path.write_bytes(base_hdr_bytes)
    print(
        f"Wrote {base_hdr_path} ({len(base_hdr_bytes)} bytes, BaseRenditionIsHDR=True)"
    )

    gen8k.verify_ultra_hdr_output(backward_path)
    gen8k.verify_ultra_hdr_output(base_hdr_path)
    print("Verified both outputs parse as Ultra HDR GContainer JPEGs.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
