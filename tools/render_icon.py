"""Render assets/icon.png — 256×256 transparent app icon."""

from __future__ import annotations

from pathlib import Path

from PIL import Image, ImageChops, ImageDraw

RENDER_SIZE = 512
OUT_SIZE = 256
OUT = Path(__file__).resolve().parents[1] / "assets" / "icon.png"


def lerp(a: float, b: float, t: float) -> float:
    return a + (b - a) * t


def lerp_color(
    c0: tuple[int, int, int],
    c1: tuple[int, int, int],
    t: float,
) -> tuple[int, int, int]:
    return (
        int(lerp(c0[0], c1[0], t)),
        int(lerp(c0[1], c1[1], t)),
        int(lerp(c0[2], c1[2], t)),
    )


def diagonal_gradient(
    size: int,
    top_left: tuple[int, int, int],
    mid: tuple[int, int, int],
    bottom_right: tuple[int, int, int],
) -> Image.Image:
    img = Image.new("RGBA", (size, size))
    px = img.load()
    denom = max((size - 1) * 2, 1)
    for y in range(size):
        for x in range(size):
            t = (x + y) / denom
            if t < 0.55:
                u = t / 0.55
                rgb = lerp_color(top_left, mid, u)
            else:
                u = (t - 0.55) / 0.45
                rgb = lerp_color(mid, bottom_right, u)
            px[x, y] = (*rgb, 255)
    return img


def rounded_mask(size: int, radius: int) -> Image.Image:
    mask = Image.new("L", (size, size), 0)
    ImageDraw.Draw(mask).rounded_rectangle([0, 0, size - 1, size - 1], radius=radius, fill=255)
    return mask


def apply_mask_alpha(img: Image.Image, mask: Image.Image) -> Image.Image:
    out = img.copy()
    out.putalpha(ImageChops.multiply(img.getchannel("A"), mask))
    return out


def draw_glow(
    d: ImageDraw.ImageDraw,
    cx: int,
    cy: int,
    core_r: int,
    core: tuple[int, int, int, int],
    glow: tuple[int, int, int, int],
    rings: int = 14,
) -> None:
    for i in range(rings, 0, -1):
        r = core_r + i * 5
        t = i / rings
        alpha = int(glow[3] * (t * t) * 0.55)
        d.ellipse([cx - r, cy - r, cx + r, cy + r], fill=(glow[0], glow[1], glow[2], alpha))
    d.ellipse([cx - core_r, cy - core_r, cx + core_r, cy + core_r], fill=core)


def render_at(size: int) -> Image.Image:
    indigo = (79, 70, 229)
    sky = (56, 189, 248)
    peach = (251, 146, 60)
    hill_far = (16, 120, 110)
    hill_near = (5, 150, 105)
    sun_core = (255, 251, 235, 255)
    sun_glow = (253, 186, 116, 200)

    radius = size * 58 // OUT_SIZE
    tile_mask = rounded_mask(size, radius)
    art = diagonal_gradient(size, indigo, sky, peach)
    d = ImageDraw.Draw(art)

    s = size / OUT_SIZE

    def p(x: float, y: float) -> tuple[float, float]:
        return (x * s, y * s)

    d.polygon(
        [
            p(-8, 198),
            p(28, 168),
            p(62, 182),
            p(98, 148),
            p(138, 162),
            p(176, 138),
            p(214, 152),
            p(268, 128),
            p(268, 268),
            p(-8, 268),
        ],
        fill=(*hill_far, 215),
    )
    d.polygon(
        [
            p(-8, 222),
            p(44, 198),
            p(88, 210),
            p(132, 186),
            p(178, 200),
            p(228, 178),
            p(268, 190),
            p(268, 268),
            p(-8, 268),
        ],
        fill=(*hill_near, 255),
    )

    draw_glow(
        d,
        int(188 * s),
        int(76 * s),
        int(24 * s),
        sun_core,
        sun_glow,
        rings=10,
    )
    d.rectangle(
        [int(150 * s), int(226 * s), int(246 * s), int(238 * s)],
        fill=(255, 255, 255, 45),
    )

    inset = int(12 * s)
    r = int(46 * s)
    d.rounded_rectangle(
        [inset, inset, size - inset - 1, size - inset - 1],
        radius=r,
        outline=(255, 255, 255, 90),
        width=max(2, int(3 * s)),
    )
    d.rounded_rectangle(
        [int(10 * s), int(10 * s), size - int(11 * s), int(54 * s)],
        radius=int(42 * s),
        fill=(255, 255, 255, 48),
    )
    d.line(
        [(int(22 * s), int(14 * s)), (size - int(22 * s), int(14 * s))],
        fill=(255, 255, 255, 190),
        width=max(2, int(2 * s)),
    )

    for y in range(size - int(44 * s), size):
        t = (y - (size - int(44 * s))) / max(int(44 * s), 1)
        d.line([(int(14 * s), y), (size - int(14 * s), y)], fill=(15, 23, 42, int(80 * t)))

    icon = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    icon.alpha_composite(apply_mask_alpha(art, tile_mask))
    return icon


def main() -> None:
    icon = render_at(RENDER_SIZE).resize((OUT_SIZE, OUT_SIZE), Image.Resampling.LANCZOS)

    OUT.parent.mkdir(parents=True, exist_ok=True)
    icon.save(OUT, format="PNG", optimize=True)

    im = Image.open(OUT).convert("RGBA")
    opaque = sum(1 for p in im.getdata() if p[3] > 0)
    print(f"Wrote {OUT}")
    print(
        f"opaque={opaque}/{OUT_SIZE * OUT_SIZE} ({100 * opaque / (OUT_SIZE * OUT_SIZE):.1f}%)"
    )


if __name__ == "__main__":
    main()
