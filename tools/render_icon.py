"""Render assets/icon.png — 256×256 transparent app icon."""

from __future__ import annotations

from pathlib import Path

from PIL import Image, ImageDraw

W = H = 256
OUT = Path(__file__).resolve().parents[1] / "assets" / "icon.png"

SKY = (2, 132, 199, 255)        # vivid blue
WHITE = (255, 255, 255, 255)
GOLD = (255, 209, 0, 255)       # warm accent


def main() -> None:
    img = Image.new("RGBA", (W, H), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)

    # Full-bleed rounded tile
    d.rounded_rectangle([0, 0, W - 1, H - 1], radius=58, fill=SKY)

    # Bold “picture card” (fills ~82% of canvas)
    pad = 22
    card = [pad, pad, W - pad, H - pad]
    d.rounded_rectangle(card, radius=36, fill=WHITE)

    # Minimal inner glyph: viewfinder ring + golden focal dot (not a cliché landscape)
    cx, cy = W // 2, H // 2 + 4
    outer_r = 72
    inner_r = 52
    d.ellipse(
        [cx - outer_r, cy - outer_r, cx + outer_r, cy + outer_r],
        outline=SKY,
        width=18,
    )
    d.ellipse([cx - inner_r, cy - inner_r, cx + inner_r, cy + inner_r], fill=SKY)
    d.ellipse([cx - 16, cy - 56, cx + 16, cy - 24], fill=GOLD)

    OUT.parent.mkdir(parents=True, exist_ok=True)
    img.save(OUT, format="PNG", optimize=True)

    im = Image.open(OUT).convert("RGBA")
    print(f"Wrote {OUT} bbox={im.getbbox()}")


if __name__ == "__main__":
    main()
