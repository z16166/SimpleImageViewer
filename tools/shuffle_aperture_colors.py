"""Randomly permute the six solid fill colors on the aperture app icon.

Each pixel is assigned to one of six blade colors (HSV distance + spatial cleanup).
Anti-aliased rim pixels and thin mis-label spikes are corrected so fills do not
bleed across black divider lines or leave old palette specks after a shuffle.

Use a fixed ``--seed`` to reproduce a layout you like.
"""

from __future__ import annotations

import argparse
import random
from collections import Counter, defaultdict
from pathlib import Path

import numpy as np
from PIL import Image

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_ICON = ROOT / "assets" / "icon.png"
BLADES = 6
BLACK_MAX = 40
ALPHA_MIN = 128
COLOR_BIN = 16
HUE_WEIGHT = 2.0
SAT_WEIGHT = 1.0
VAL_WEIGHT = 0.5
# Rim pixels (blended toward black / another blade) may use a different label than core fill.
RIM_VALUE_MAX = 235
# Post-recolor cleanup also treats pixels on a blade boundary (neighbor color differs).
OUTPUT_RIM_PASSES = 12
REFINE_PASSES = 24


def colored_mask(arr: np.ndarray) -> np.ndarray:
    alpha = arr[..., 3]
    return (alpha > ALPHA_MIN) & (np.max(arr[..., :3], axis=2) > BLACK_MAX)


def rgb_to_hsv(rgb: np.ndarray) -> tuple[np.ndarray, np.ndarray, np.ndarray]:
    """uint8 HxWx3 -> hue, saturation, value in [0, 1]."""
    x = rgb.astype(np.float64) / 255.0
    r, g, b = x[..., 0], x[..., 1], x[..., 2]
    mx = np.maximum(np.maximum(r, g), b)
    mn = np.minimum(np.minimum(r, g), b)
    d = mx - mn
    h = np.zeros_like(mx)
    with np.errstate(invalid="ignore", divide="ignore"):
        mask = d > 1e-8
        mr = (mx == r) & mask
        mg = (mx == g) & mask
        mb = (mx == b) & mask
        h[mr] = ((g - b) / d)[mr] % 6.0
        h[mg] = ((b - r) / d + 2.0)[mg]
        h[mb] = ((r - g) / d + 4.0)[mb]
        s = np.where(mx > 1e-8, d / mx, 0.0)
    h = h / 6.0
    return h, s, mx


def palette_hsv(palette: list[tuple[int, int, int]]) -> tuple[np.ndarray, np.ndarray, np.ndarray]:
    stack = np.array(palette, dtype=np.float64) / 255.0
    return rgb_to_hsv(stack.reshape(1, 1, BLADES, 3))


def extract_blade_palette(arr: np.ndarray) -> list[tuple[int, int, int]]:
    """Six median RGB fills from the largest coarse color buckets."""
    mask = colored_mask(arr)
    pts = arr[mask][:, :3]
    bins: dict[tuple[int, int, int], list[np.ndarray]] = defaultdict(list)
    for p in pts:
        key = tuple((int(p[c]) // COLOR_BIN * COLOR_BIN) for c in range(3))
        bins[key].append(p)
    if len(bins) < BLADES:
        raise ValueError(f"expected at least {BLADES} color buckets, got {len(bins)}")
    top = sorted(bins.keys(), key=lambda k: -len(bins[k]))[:BLADES]
    palette: list[tuple[int, int, int]] = []
    for key in top:
        stack = np.stack(bins[key], axis=0)
        med = np.median(stack, axis=0).astype(np.uint8)
        palette.append((int(med[0]), int(med[1]), int(med[2])))
    return palette


def label_blades(arr: np.ndarray, palette: list[tuple[int, int, int]]) -> np.ndarray:
    """Per-pixel blade index 0..5 (HSV distance; low-sat pixels use RGB fallback)."""
    ph, ps, pv = palette_hsv(palette)
    ph = ph.reshape(BLADES)
    ps = ps.reshape(BLADES)
    pv = pv.reshape(BLADES)

    h, s, v = rgb_to_hsv(arr[..., :3])
    hd = np.abs(h[..., None] - ph[None, None, :])
    hd = np.minimum(hd, 1.0 - hd)
    dist_hsv = (
        HUE_WEIGHT * hd
        + SAT_WEIGHT * np.abs(s[..., None] - ps[None, None, :])
        + VAL_WEIGHT * np.abs(v[..., None] - pv[None, None, :])
    )
    labels = np.argmin(dist_hsv, axis=2).astype(np.int32)

    low_sat = s < 0.12
    if np.any(low_sat):
        centers = np.array(palette, dtype=np.float64)
        rgb = arr[..., :3].astype(np.float64)
        dist_rgb = np.sum((rgb[:, :, None, :] - centers[None, None, :, :]) ** 2, axis=3)
        labels[low_sat] = np.argmin(dist_rgb, axis=2)[low_sat].astype(np.int32)

    return labels


def walkable_mask(arr: np.ndarray) -> np.ndarray:
    """Blade + anti-alias rim; excludes separators and transparent background."""
    alpha = arr[..., 3]
    rgb = arr[..., :3]
    mx = np.max(rgb, axis=2)
    separator = (alpha > 0) & (mx <= BLACK_MAX)
    return (alpha > 0) & ~separator


def refine_labels_majority(labels: np.ndarray, walkable: np.ndarray) -> np.ndarray:
    """Relabel rim spikes when most 4-neighbors share another blade id."""
    h, w = labels.shape
    out = labels.copy()
    for _ in range(REFINE_PASSES):
        changed = False
        new = out.copy()
        for y in range(1, h - 1):
            for x in range(1, w - 1):
                if not walkable[y, x]:
                    continue
                own = int(out[y, x])
                nbrs = [
                    int(out[y + dy, x + dx])
                    for dy, dx in ((-1, 0), (1, 0), (0, -1), (0, 1))
                    if walkable[y + dy, x + dx]
                ]
                if len(nbrs) < 2:
                    continue
                counts = Counter(nbrs)
                best, cnt = counts.most_common(1)[0]
                own_cnt = counts[own]
                if best != own and cnt >= 3:
                    new[y, x] = best
                    changed = True
                elif best != own and cnt >= 2 and cnt > own_cnt:
                    new[y, x] = best
                    changed = True
        out = new
        if not changed:
            break
    return out


def fix_corner_spike_labels(
    labels: np.ndarray,
    orig: np.ndarray,
    walkable: np.ndarray,
) -> np.ndarray:
    """Fix 2x2 corner mis-labels on anti-aliased rims (e.g. yellow speck in purple)."""
    h, w = labels.shape
    out = labels.copy()
    dirs = {"N": (-1, 0), "S": (1, 0), "W": (0, -1), "E": (0, 1)}
    # Center sits on the (a,b) side; (c,d) is the other blade across the corner.
    # (a,b) = spike side; (c,d) = surrounding blade. No pattern where spike is on (c,d).
    corner_pairs = (("N", "W", "S", "E"), ("W", "S", "N", "E"), ("N", "E", "W", "S"))
    for y in range(1, h - 1):
        for x in range(1, w - 1):
            if not walkable[y, x]:
                continue
            if int(np.max(orig[y, x, :3])) >= RIM_VALUE_MAX:
                continue
            center = int(out[y, x])
            nbr: dict[str, int] = {}
            ok = True
            for name, (dy, dx) in dirs.items():
                ny, nx = y + dy, x + dx
                if not walkable[ny, nx]:
                    ok = False
                    break
                nbr[name] = int(out[ny, nx])
            if not ok:
                continue
            nbr4 = [nbr[k] for k in ("N", "S", "W", "E")]
            nbr4_counts = Counter(nbr4)
            for a, b, c, d in corner_pairs:
                other = nbr[c]
                if (
                    center == nbr[a] == nbr[b]
                    and nbr[c] == nbr[d] == other
                    and other != center
                    and nbr4_counts[other] >= 2
                    and nbr4_counts[center] <= 2
                    and nbr4_counts[other] >= nbr4_counts[center]
                ):
                    out[y, x] = other
                    break
    return out


def _nearest_palette_label(rgb: np.ndarray, palette: list[tuple[int, int, int]], candidates: set[int]) -> int:
    best_id = min(
        candidates,
        key=lambda i: int(np.sum((rgb.astype(np.float64) - np.array(palette[i], dtype=np.float64)) ** 2)),
    )
    return best_id


def relabel_rim_by_neighbors(
    labels: np.ndarray,
    orig: np.ndarray,
    walkable: np.ndarray,
    palette: list[tuple[int, int, int]],
) -> np.ndarray:
    """Snap anti-aliased rim pixels to the dominant neighboring blade label."""
    h, w = labels.shape
    out = labels.copy()
    for y in range(1, h - 1):
        for x in range(1, w - 1):
            if not walkable[y, x] or int(np.max(orig[y, x, :3])) >= RIM_VALUE_MAX:
                continue
            center = int(out[y, x])
            nbrs = [
                int(out[y + dy, x + dx])
                for dy in (-1, 0, 1)
                for dx in (-1, 0, 1)
                if (dy, dx) != (0, 0) and walkable[y + dy, x + dx]
            ]
            if len(nbrs) < 4:
                continue
            counts = Counter(nbrs)
            best, best_cnt = counts.most_common(1)[0]
            if best == center or best_cnt < 2:
                continue
            center_cnt = counts[center]
            if best_cnt < center_cnt:
                continue
            if best_cnt == center_cnt:
                tied = {lid for lid, cnt in counts.items() if cnt == best_cnt}
                if center in tied:
                    tied.discard(center)
                if not tied:
                    continue
                best = _nearest_palette_label(orig[y, x, :3], palette, tied)
            if best != center:
                out[y, x] = best
    return out


def relabel_rim_cardinal_agreement(
    labels: np.ndarray,
    orig: np.ndarray,
    walkable: np.ndarray,
) -> np.ndarray:
    """Outer corners: all walkable 4-neighbors share one label different from center."""
    h, w = labels.shape
    out = labels.copy()
    for y in range(1, h - 1):
        for x in range(1, w - 1):
            if not walkable[y, x] or int(np.max(orig[y, x, :3])) >= RIM_VALUE_MAX:
                continue
            center = int(out[y, x])
            cardinals = [
                int(out[y + dy, x + dx])
                for dy, dx in ((-1, 0), (1, 0), (0, -1), (0, 1))
                if walkable[y + dy, x + dx]
            ]
            if len(cardinals) < 2:
                continue
            agree = cardinals[0]
            if all(c == agree for c in cardinals) and agree != center:
                out[y, x] = agree
    return out


def relabel_rim_palette_fit(
    labels: np.ndarray,
    orig: np.ndarray,
    walkable: np.ndarray,
    palette: list[tuple[int, int, int]],
) -> np.ndarray:
    """Pick the neighbor blade label whose palette best matches the rim pixel RGB."""
    h, w = labels.shape
    out = labels.copy()
    for y in range(1, h - 1):
        for x in range(1, w - 1):
            if not walkable[y, x] or int(np.max(orig[y, x, :3])) >= RIM_VALUE_MAX:
                continue
            center = int(out[y, x])
            candidates = {center}
            for dy in (-1, 0, 1):
                for dx in (-1, 0, 1):
                    if dy == 0 and dx == 0:
                        continue
                    ny, nx = y + dy, x + dx
                    if walkable[ny, nx]:
                        candidates.add(int(out[ny, nx]))
            if len(candidates) < 2:
                continue
            cardinals = [
                int(out[y + dy, x + dx])
                for dy, dx in ((-1, 0), (1, 0), (0, -1), (0, 1))
                if walkable[y + dy, x + dx]
            ]
            if len(cardinals) >= 2 and all(c == cardinals[0] for c in cardinals):
                agree = cardinals[0]
                if agree != center:
                    out[y, x] = agree
                    continue
            best = _nearest_palette_label(orig[y, x, :3], palette, candidates)
            if best != center:
                out[y, x] = best
    return out


def prepare_labels(arr: np.ndarray) -> tuple[np.ndarray, list[tuple[int, int, int]], np.ndarray]:
    palette = extract_blade_palette(arr)
    walkable = walkable_mask(arr)
    labels = label_blades(arr, palette)
    labels = refine_labels_majority(labels, walkable)
    for _ in range(8):
        next_labels = fix_corner_spike_labels(labels, arr, walkable)
        if np.array_equal(next_labels, labels):
            break
        labels = next_labels
    labels = relabel_rim_by_neighbors(labels, arr, walkable, palette)
    for _ in range(4):
        next_labels = relabel_rim_palette_fit(labels, arr, walkable, palette)
        if np.array_equal(next_labels, labels):
            break
        labels = next_labels
    labels = relabel_rim_cardinal_agreement(labels, arr, walkable)
    return labels, palette, walkable


def _output_blade_ids(rgb: np.ndarray, new_palette: list[tuple[int, int, int]]) -> np.ndarray:
    pal = np.array(new_palette, dtype=np.float64)
    flat = rgb.reshape(-1, 3).astype(np.float64)
    ids = np.argmin(np.sum((flat[:, None, :] - pal[None, :, :]) ** 2, axis=2), axis=1)
    return ids.reshape(rgb.shape[0], rgb.shape[1]).astype(np.int32)


def _is_output_rim(
    y: int,
    x: int,
    ids: np.ndarray,
    walkable: np.ndarray,
    orig: np.ndarray,
) -> bool:
    if int(np.max(orig[y, x, :3])) < 250:
        return True
    center = int(ids[y, x])
    for dy in (-1, 0, 1):
        for dx in (-1, 0, 1):
            if dy == 0 and dx == 0:
                continue
            ny, nx = y + dy, x + dx
            if not walkable[ny, nx]:
                return True
            if int(ids[ny, nx]) != center:
                return True
    return False


def cleanup_output_rim_specks(
    out: np.ndarray,
    orig: np.ndarray,
    walkable: np.ndarray,
    new_palette: list[tuple[int, int, int]],
) -> np.ndarray:
    """After recolor: fix wrong palette specks using 8-neighbor output color majority."""
    h, w = out.shape[:2]
    arr = out.copy()
    rgb = arr[..., :3]
    for _ in range(OUTPUT_RIM_PASSES):
        ids = _output_blade_ids(rgb, new_palette)
        changed = False
        for y in range(1, h - 1):
            for x in range(1, w - 1):
                if not walkable[y, x] or not _is_output_rim(y, x, ids, walkable, orig):
                    continue
                center = int(ids[y, x])
                nbrs = [
                    int(ids[y + dy, x + dx])
                    for dy in (-1, 0, 1)
                    for dx in (-1, 0, 1)
                    if (dy, dx) != (0, 0) and walkable[y + dy, x + dx]
                ]
                if len(nbrs) < 2:
                    continue
                counts = Counter(nbrs)
                best, best_cnt = counts.most_common(1)[0]
                if best == center:
                    continue
                center_cnt = counts[center]
                if best_cnt >= 2 and best_cnt > center_cnt:
                    arr[y, x, :3] = new_palette[best]
                    rgb = arr[..., :3]
                    changed = True
                elif best_cnt >= 2 and best_cnt == center_cnt:
                    tied = {lid for lid, cnt in counts.items() if cnt == best_cnt and lid != center}
                    if tied:
                        pick = _nearest_palette_label(
                            orig[y, x, :3],
                            new_palette,
                            tied,
                        )
                        if pick != center:
                            arr[y, x, :3] = new_palette[pick]
                            rgb = arr[..., :3]
                            changed = True
        if not changed:
            break
    return arr


def shuffle_blade_colors(
    src: Path,
    dst: Path,
    *,
    seed: int | None = None,
) -> list[int]:
    """Permute blade fill colors; return perm where new[blade] = old[perm[blade]]."""
    rng = random.Random(seed)
    arr = np.array(Image.open(src).convert("RGBA"))
    labels, palette, walkable = prepare_labels(arr)

    perm = list(range(BLADES))
    rng.shuffle(perm)
    new_palette = [palette[perm[b]] for b in range(BLADES)]

    out = arr.copy()
    for blade in range(BLADES):
        px = walkable & (labels == blade)
        out[px, :3] = new_palette[blade]

    out = cleanup_output_rim_specks(out, arr, walkable, new_palette)
    Image.fromarray(out, "RGBA").save(dst, format="PNG", optimize=True)
    return perm


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--in", dest="inp", type=Path, default=DEFAULT_ICON)
    parser.add_argument("--out", type=Path, default=None)
    parser.add_argument("--seed", type=int, default=None)
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    inp: Path = args.inp
    out: Path = args.out or inp
    if not inp.is_file():
        raise SystemExit(f"input not found: {inp}")

    arr = np.array(Image.open(inp).convert("RGBA"))
    labels, palette, walkable = prepare_labels(arr)
    counts = [int(np.sum(walkable & (labels == b))) for b in range(BLADES)]

    print("blade palette (median RGB):", palette)
    print("pixels per blade (walkable):", counts)

    if args.dry_run:
        rng = random.Random(args.seed)
        perm = list(range(BLADES))
        rng.shuffle(perm)
        print("example permutation (new[b] = old[perm[b]]):", perm)
        return

    perm = shuffle_blade_colors(inp, out, seed=args.seed)
    print(f"Wrote {out}")
    print(f"permutation (new[blade] = old[perm[blade]]): {perm}")
    if args.seed is not None:
        print(f"seed={args.seed}")


if __name__ == "__main__":
    main()
