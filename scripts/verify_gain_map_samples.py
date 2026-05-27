#!/usr/bin/env python3
"""Quick sanity check for tests/data/gain_map_samples/*.jpg."""
from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

ISO_NS = b"urn:iso:std:iso:ts:21496:-1\0"
ROOT = Path(__file__).resolve().parents[1]
SAMPLES = ROOT / "tests" / "data" / "gain_map_samples"


def _load_gen8k():
    spec = importlib.util.spec_from_file_location(
        "generate_8k_ultra_hdr_sample",
        Path(__file__).resolve().parent / "generate_8k_ultra_hdr_sample.py",
    )
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


def gain_map_trailer(data: bytes, gen8k) -> bytes:
    length = gen8k.gain_map_trailer_length(data)
    if length is None:
        raise ValueError("missing GainMap Item:Length")
    return data[-length:]


def iso_backward_flag(gain_jpeg: bytes) -> bool:
    i = 2
    while i + 1 < len(gain_jpeg):
        if gain_jpeg[i] != 0xFF:
            i += 1
            continue
        marker = gain_jpeg[i + 1]
        i += 2
        if marker in (0xDA, 0xD9):
            break
        seg_len = int.from_bytes(gain_jpeg[i : i + 2], "big")
        payload = gain_jpeg[i + 2 : i + seg_len]
        if marker == 0xE2 and payload.startswith(ISO_NS):
            return bool(payload[len(ISO_NS) + 4] & 0x04)
        i += seg_len
    return False


def main() -> int:
    gen8k = _load_gen8k()
    backward_path = SAMPLES / "sample_iso_backward.jpg"
    base_hdr_path = SAMPLES / "sample_base_rendition_is_hdr.jpg"
    for path in (backward_path, base_hdr_path):
        if not path.is_file():
            print(f"missing: {path}", file=sys.stderr)
            return 1

    backward_gain = gain_map_trailer(backward_path.read_bytes(), gen8k)
    if not iso_backward_flag(backward_gain):
        print("sample_iso_backward.jpg: ISO backward flag not set", file=sys.stderr)
        return 1

    base_text = base_hdr_path.read_bytes().decode("latin-1", errors="replace")
    if 'BaseRenditionIsHDR="True"' not in base_text and "BaseRenditionIsHDR='True'" not in base_text:
        print("sample_base_rendition_is_hdr.jpg: BaseRenditionIsHDR=True not found", file=sys.stderr)
        return 1

    gen8k.verify_ultra_hdr_output(backward_path)
    gen8k.verify_ultra_hdr_output(base_hdr_path)

    print(f"OK: {backward_path} ({backward_path.stat().st_size} bytes)")
    print(f"OK: {base_hdr_path} ({base_hdr_path.stat().st_size} bytes)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
