#!/usr/bin/env python3
"""Single clean pipeline to apply all splits and fixes."""
from __future__ import annotations

import re
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SRC = ROOT / "src"
MARKER = "// along with this program.  If not, see <https://www.gnu.org/licenses/>.\n\n"


def git_lines(rel: str) -> list[str]:
    return subprocess.check_output(
        ["git", "show", f"HEAD:{rel}"], cwd=ROOT, text=True, encoding="utf-8"
    ).splitlines(keepends=True)


def dedupe_pub(text: str) -> str:
    n = text
    for _ in range(5):
        n = re.sub(r"(pub(?:\(crate\)|\(super\))?)\s+\1\s+", r"\1 ", n)
        n = n.replace("pub pub ", "pub ")
    return n


def run_script(name: str) -> None:
    subprocess.run(["python", str(ROOT / "scripts" / name)], cwd=ROOT, check=True)


def wave1_bodies() -> None:
    # audio
    lines = git_lines("src/audio.rs")
    h, imp = "".join(lines[:15]), "".join(lines[16:83])
    base = SRC / "audio"
    for n, s, e in [
        ("player.rs", 85, 340),
        ("playlist.rs", 342, 448),
        ("slots.rs", 453, 479),
        ("loop_state.rs", 1308, 2178),
        ("run_loop.rs", 2180, len(lines)),
    ]:
        (base / n).write_text(h + imp + "".join(lines[s - 1 : e]), encoding="utf-8")
    (base / "cue.rs").write_text(
        h
        + "use std::fs;\nuse std::path::Path;\nuse std::sync::atomic::{AtomicBool, Ordering};\nuse std::time::Duration;\n\n"
        + "".join(lines[480:688]),
        encoding="utf-8",
    )
    (base / "sources/ape.rs").write_text(h + imp + "".join(lines[689:918]), encoding="utf-8")
    (base / "sources/symphonia.rs").write_text(h + imp + "".join(lines[918:1306]), encoding="utf-8")
    (base / "sources/mod.rs").write_text(h + "mod ape;\nmod symphonia;\n", encoding="utf-8")

    # heif
    lines = git_lines("src/hdr/heif.rs")
    h, imp = "".join(lines[:15]), "".join(lines[16:31])
    base = SRC / "hdr/heif"
    for n, s, e in [
        ("brand.rs", 32, 68),
        ("session.rs", 70, 237),
        ("orientation.rs", 238, 566),
        ("load.rs", 567, 698),
        ("decode.rs", 703, 1311),
        ("ycbcr.rs", 1313, 1602),
        ("gain_map.rs", 1604, 1892),
        ("metadata.rs", 1894, 2154),
    ]:
        (base / n).write_text(h + imp + "".join(lines[s - 1 : e]), encoding="utf-8")
    ts = next(
        i
        for i, l in enumerate(lines)
        if l.strip() == "#[cfg(test)]" and i + 1 < len(lines) and "mod tests" in lines[i + 1]
    )
    (base / "tests.rs").write_text(h + imp + "".join(lines[ts + 1 : -1]), encoding="utf-8")
    mod_rs = h + Path("scripts/split_wave1_monoliths.py").read_text(encoding="utf-8")
    # write heif mod from split_wave1 template
    (base / "mod.rs").write_text(
        h
        + """mod brand;

#[cfg(feature = "heif-native")]
mod decode;
#[cfg(feature = "heif-native")]
mod gain_map;
#[cfg(feature = "heif-native")]
mod load;
#[cfg(feature = "heif-native")]
mod metadata;
#[cfg(feature = "heif-native")]
mod orientation;
#[cfg(feature = "heif-native")]
mod session;
#[cfg(feature = "heif-native")]
mod ycbcr;

#[cfg(test)]
mod tests;

pub(crate) use brand::{heif_nclx_to_metadata, is_heif_brand};

#[cfg(feature = "heif-native")]
pub(crate) use gain_map::align_apple_gain_map_to_primary_display_orientation;
#[cfg(feature = "heif-native")]
pub(crate) use load::{decode_heif_hdr, decode_heif_hdr_bytes, load_heif_hdr};
#[cfg(feature = "heif-native")]
pub(crate) use metadata::{
    apply_heif_unknown_transfer_bt709_primaries_fallback, classify_heif_auxiliary_type,
    HeifAuxiliaryClassification, HeifAuxiliaryEvidence,
};
#[cfg(feature = "heif-native")]
pub(crate) use orientation::{
    decoded_pixels_match_swapped_ispe, heif_exif_orientation_from_raw_handle,
    libheif_exif_orientation_tag, libheif_manual_geometry_exif_orientation_from_bytes,
    libheif_manual_geometry_exif_orientation_from_path,
    libheif_primary_decode_should_ignore_embedded_geometry,
    libheif_primary_geometric_mirror_rotation_only, HeifDecodeOptionsIgnoredGeometryOwned,
};
""",
        encoding="utf-8",
    )

    # openexr
    lines = git_lines("src/hdr/openexr_core_backend.rs")
    h = "".join(lines[:15])
    ts = next(
        i
        for i, l in enumerate(lines)
        if l.strip() == "#[cfg(test)]" and i + 1 < len(lines) and "mod tests" in lines[i + 1]
    )
    base = SRC / "hdr/openexr_core"
    (base / "read_context.rs").write_text(h + "".join(lines[40:55]) + "".join(lines[550:1486]), encoding="utf-8")
    for n, s, e in [
        ("chromaticities.rs", 56, 292),
        ("types.rs", 295, 414),
        ("mmap.rs", 415, 550),
        ("channels.rs", 1487, ts),
    ]:
        (base / n).write_text(h + "".join(lines[s - 1 : e]), encoding="utf-8")
    (base / "tests.rs").write_text(h + "".join(lines[ts + 2 : -1]), encoding="utf-8")

    # orchestrator
    lines = git_lines("src/loader/orchestrator.rs")
    h, imp = "".join(lines[:15]), "".join(lines[15:45])
    base = SRC / "loader/orchestrator"
    (base / "types.rs").write_text(h + imp + "".join(lines[45:156]), encoding="utf-8")
    (base / "load.rs").write_text(
        h + imp + "impl super::types::ImageLoader {\n" + "".join(lines[157:1300]) + "}\n", encoding="utf-8"
    )
    (base / "tiles.rs").write_text(
        h + imp + "impl super::types::ImageLoader {\n" + "".join(lines[1301:1322]) + "}\n", encoding="utf-8"
    )
    (base / "poll.rs").write_text(
        h + imp + "impl super::types::ImageLoader {\n" + "".join(lines[1323:1440]) + "}\n", encoding="utf-8"
    )
    (base / "mod.rs").write_text(
        h
        + """mod load;
mod poll;
mod tiles;
mod types;

#[cfg(test)]
mod tests;

pub use types::ImageLoader;
pub(crate) use types::TileInFlightKey;
""",
        encoding="utf-8",
    )


def post_fixes() -> None:
    run_script("cleanup_splits.py")
    run_script("fix_heif_smart.py")
    run_script("fix_audio_wiring.py")
    run_script("fix_openexr_exports.py")
    # wic imports mod
    wic_mod = SRC / "wic/mod.rs"
    t = wic_mod.read_text(encoding="utf-8")
    if "mod imports;" not in t:
        wic_mod.write_text(t.replace("mod factory;\n", "mod factory;\nmod imports;\n", 1), encoding="utf-8")
    # tiled mod TransitionStyle
    tm = SRC / "app/rendering/tiled/mod.rs"
    tt = tm.read_text(encoding="utf-8")
    if "use crate::settings::TransitionStyle" not in tt:
        tm.write_text(tt.replace(MARKER, MARKER + "use crate::settings::TransitionStyle;\n\n", 1), encoding="utf-8")
    # hotkeys t!
    hu = SRC / "app/hotkeys_ui.rs"
    ht = hu.read_text(encoding="utf-8")
    if "use rust_i18n::t;" not in ht:
        hu.write_text(ht.replace(MARKER, MARKER + "use rust_i18n::t;\n\n", 1), encoding="utf-8")
    # openexr visibility + mod exports already in fix_openexr_exports.py
    # libtiff tiled: no thumbnail import
    tiled = SRC / "libtiff_loader/tiled.rs"
    tiled.write_text(tiled.read_text(encoding="utf-8").replace("use super::thumbnail::extract_embedded_thumbnail;\n", ""), encoding="utf-8")
    # radiance rle import
    rle = SRC / "hdr/radiance_tiled/rle.rs"
    rle.write_text(rle.read_text(encoding="utf-8").replace(", build_radiance_scanline_offsets", ""), encoding="utf-8")
    # avif metadata trait
    av = SRC / "hdr/avif/metadata.rs"
    av.write_text(re.sub(r"(pub\(crate\)\s+)*trait AvifMetadataExt", "pub(crate) trait AvifMetadataExt", av.read_text(encoding="utf-8")))
    # keyboard/wheel dedupe - already fixed in repo
    run_script("finish_wave1.py")
    for p in SRC.rglob("*.rs"):
        n = dedupe_pub(p.read_text(encoding="utf-8"))
        p.write_text(n, encoding="utf-8")


def main() -> None:
    run_script("rebuild_splits.py")
    run_script("fix_split_imports_v2.py")
    run_script("final_split_fixes.py")
    run_script("minimal_split_fixes.py")
    run_script("fix_split_imports.py")
    wave1_bodies()
    post_fixes()
    print("pipeline complete")


if __name__ == "__main__":
    main()
