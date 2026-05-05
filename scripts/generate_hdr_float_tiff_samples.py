#!/usr/bin/env python3
"""Generate small IEEE float RGB TIFFs (f16/f32/f64) for HDR decoder tests.

Requires: pip install numpy tifffile

Writes to tests/data/ next to this repo root (run from repo root recommended).
"""
from __future__ import annotations

import sys
from pathlib import Path

import numpy as np


def main() -> None:
    try:
        import tifffile
    except ImportError:
        print("Install: pip install numpy tifffile", file=sys.stderr)
        sys.exit(1)

    root = Path(__file__).resolve().parents[1]
    out_dir = root / "tests" / "data"
    out_dir.mkdir(parents=True, exist_ok=True)

    h, w = 64, 64
    # Scene-linear-style ramp (0..1.2 in R&G to exercise HDR >1.0)
    r = np.linspace(0.0, 1.2, w, dtype=np.float64)
    g = np.linspace(0.0, 1.0, h, dtype=np.float64)
    R = np.outer(g, np.ones(w))
    G = np.outer(np.ones(h), r)
    B = np.full((h, w), 0.25, dtype=np.float64)
    rgb64 = np.stack([R, G, B], axis=-1)

    name_fmt = "hdr_ieee_rgb_{}bit.tif"
    specs = [
        ("f16", np.float16, name_fmt.format(16)),
        ("f32", np.float32, name_fmt.format(32)),
        ("f64", np.float64, name_fmt.format(64)),
    ]

    for _, dtype, fname in specs:
        arr = rgb64.astype(dtype, copy=False)
        path = out_dir / fname
        tifffile.imwrite(
            path,
            arr,
            photometric="rgb",
            compression="none",
        )
        print(f"Wrote {path} shape={arr.shape} dtype={arr.dtype}")

    print("Done.")


if __name__ == "__main__":
    main()
