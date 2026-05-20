"""Clean aperture icon: defringe white halos, refine alpha, output 256×256 RGBA."""

from __future__ import annotations

from pathlib import Path

import numpy as np
from PIL import Image, ImageFilter

TARGET = 256
MARGIN = 2
WORK_SIZE = 1024

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "assets" / "icon.png"


def unblend_white(rgb: np.ndarray, alpha: np.ndarray) -> np.ndarray:
    out = rgb.astype(np.float32)
    f = alpha.astype(np.float32) / 255.0
    mask = f > 0.02
    for c in range(3):
        ch = out[..., c]
        ch[mask] = (ch[mask] - (1.0 - f[mask]) * 255.0) / f[mask]
    return np.clip(out, 0, 255).astype(np.uint8)


def refine_alpha(alpha: Image.Image, erode: int = 2, dilate: int = 1) -> Image.Image:
    a = alpha.filter(ImageFilter.MedianFilter(3))
    for _ in range(erode):
        a = a.filter(ImageFilter.MinFilter(3))
    for _ in range(dilate):
        a = a.filter(ImageFilter.MaxFilter(3))
    return a


def cleanup_rgba(arr: np.ndarray) -> np.ndarray:
    rgb = arr[..., :3].astype(np.float32)
    max_c = np.max(rgb, axis=2)
    min_c = np.min(rgb, axis=2)
    sat = max_c - min_c
    a = arr[..., 3].astype(np.float32)

    fringe = (a > 0) & (a < 230) & (sat < 60)
    arr[fringe, 3] = 0
    arr[fringe, :3] = 0

    speck = (a >= 200) & (max_c > 200) & (sat < 40)
    arr[speck, 3] = 0
    arr[speck, :3] = 0

    hard = arr[..., 3] > 0
    arr[hard & (arr[..., 3] >= 128), 3] = 255
    arr[arr[..., 3] < 32, 3] = 0
    arr[arr[..., 3] == 0, :3] = 0
    return arr


def crop_square_centered(rgba: Image.Image) -> Image.Image:
    bbox = rgba.getbbox()
    if not bbox:
        raise ValueError("empty image")
    cropped = rgba.crop(bbox)
    cw, ch = cropped.size
    side = max(cw, ch)
    square = Image.new("RGBA", (side, side), (0, 0, 0, 0))
    square.paste(cropped, ((side - cw) // 2, (side - ch) // 2))
    return square


def fit_256(square: Image.Image) -> Image.Image:
    content = TARGET - 2 * MARGIN
    scaled = square.resize((content, content), Image.Resampling.LANCZOS)
    canvas = Image.new("RGBA", (TARGET, TARGET), (0, 0, 0, 0))
    off = (TARGET - content) // 2
    canvas.paste(scaled, (off, off), scaled)
    return canvas


def process_icon(src: Path, dst: Path) -> None:
    im = Image.open(src).convert("RGBA")
    big = im.resize((WORK_SIZE, WORK_SIZE), Image.Resampling.LANCZOS)
    arr = np.array(big)

    rgb = unblend_white(arr[..., :3], arr[..., 3])
    arr = np.dstack([rgb, arr[..., 3]])

    pil = Image.fromarray(arr, "RGBA")
    pil.putalpha(refine_alpha(pil.getchannel("A"), erode=2, dilate=1))
    arr = cleanup_rgba(np.array(pil))

    pil = Image.fromarray(arr, "RGBA")
    square = crop_square_centered(pil)
    out = fit_256(square)
    out_arr = cleanup_rgba(np.array(out))
    Image.fromarray(out_arr, "RGBA").save(dst, format="PNG", optimize=True)


def main() -> None:
    src = OUT
    if not src.is_file():
        raise SystemExit(f"icon not found: {src}")
    process_icon(src, OUT)
    im = Image.open(OUT).convert("RGBA")
    semi = sum(1 for p in im.getdata() if 0 < p[3] < 255)
    print(f"Wrote {OUT} bbox={im.getbbox()} semi_transparent_px={semi}")


if __name__ == "__main__":
    main()
