"""Replace dark purple blade fill with a brighter purple on the aperture icon."""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

import numpy as np
from PIL import Image

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_IN = ROOT / "assets" / "icon.png"
DEFAULT_OUT = DEFAULT_IN

# Original aperture purple (median of the purple blade).
DARK_PURPLE = np.array([109, 57, 140], dtype=np.float64)
# Target from user reference (~#9365FF).
BRIGHT_PURPLE = np.array([147, 101, 255], dtype=np.float64)

sys.path.insert(0, str(ROOT / "tools"))
from shuffle_aperture_colors import (  # noqa: E402
    BLADES,
    colored_mask,
    extract_blade_palette,
    label_blades,
    walkable_mask,
)


def purple_blade_index(palette: list[tuple[int, int, int]]) -> int:
    dists = [np.sum((np.array(p, dtype=np.float64) - DARK_PURPLE) ** 2) for p in palette]
    return int(np.argmin(dists))


def recolor_purple(
    arr: np.ndarray,
    *,
    purple_idx: int | None = None,
) -> np.ndarray:
    """Map dark-purple blade pixels to BRIGHT_PURPLE; preserve black-edge anti-alias."""
    palette = extract_blade_palette(arr)
    pidx = purple_blade_index(palette) if purple_idx is None else purple_idx
    labels = label_blades(arr, palette)
    walk = walkable_mask(arr)

    out = arr.copy()
    mask = walk & (labels == pidx)
    if not np.any(mask):
        raise ValueError(f"no pixels for purple blade index {pidx}")

    rgb = out[..., :3].astype(np.float64)
    # Scale each channel from dark fill toward black so rims stay smooth.
    ratio = np.zeros(rgb.shape[:2], dtype=np.float64)
    for c in range(3):
        ratio = np.maximum(ratio, rgb[..., c] / max(DARK_PURPLE[c], 1.0))
    ratio = np.clip(ratio, 0.0, 1.0)
    new_rgb = (BRIGHT_PURPLE[None, None, :] * ratio[..., None]).astype(np.uint8)
    out[mask, :3] = new_rgb[mask]
    return out


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--in", dest="inp", type=Path, default=DEFAULT_IN)
    parser.add_argument("--out", type=Path, default=None)
    args = parser.parse_args()
    inp: Path = args.inp
    out: Path = args.out or inp
    if not inp.is_file():
        raise SystemExit(f"input not found: {inp}")

    arr = np.array(Image.open(inp).convert("RGBA"))
    palette = extract_blade_palette(arr)
    pidx = purple_blade_index(palette)
    print(f"purple blade index {pidx}, palette[{pidx}]={palette[pidx]}")
    print(f"dark {tuple(DARK_PURPLE.astype(int))} -> bright {tuple(BRIGHT_PURPLE.astype(int))}")

    result = recolor_purple(arr)
    Image.fromarray(result, "RGBA").save(out, format="PNG", optimize=True)
    print(f"Wrote {out}")


if __name__ == "__main__":
    main()
