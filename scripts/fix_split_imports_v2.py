#!/usr/bin/env python3
"""Final import fixes after rebuild_splits."""
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

WIC_SUB_USE = "use super::imports::*;\nuse windows::Win32::Graphics::Imaging::*;\nuse windows::Win32::System::Com::*;\nuse windows::core::*;\n\n"


def fix_types_imports(directory: Path) -> None:
    for p in directory.rglob("*.rs"):
        if p.name == "mod.rs":
            continue
        t = p.read_text(encoding="utf-8")
        o = t
        t = t.replace("use super::types::", "use crate::hdr::types::")
        t = t.replace("use super::types;", "use crate::hdr::types;")
        if t != o:
            p.write_text(t, encoding="utf-8")


def fix_wic() -> None:
    base = ROOT / "src/wic"
    marker = "// along with this program.  If not, see <https://www.gnu.org/licenses/>.\n\n"
    for name in ["com.rs", "discovery.rs", "tiled_source.rs"]:
        p = base / name
        t = p.read_text(encoding="utf-8")
        if "use super::imports" not in t:
            extra = WIC_SUB_USE
            if name == "discovery.rs":
                extra = "use super::imports::get_wic_factory;\n" + extra
            p.write_text(t.replace(marker, marker + extra, 1), encoding="utf-8")


def fix_radiance_tiled() -> None:
    base = ROOT / "src/hdr/radiance_tiled"
    marker = "// along with this program.  If not, see <https://www.gnu.org/licenses/>.\n\n"
    for fname in ["source.rs", "tile_decode.rs", "header.rs", "rle.rs"]:
        p = base / fname
        t = p.read_text(encoding="utf-8")
        if "use crate::hdr::tiled::" in t and "HdrTiledSource" in t:
            continue
        if fname == "source.rs":
            ins = (
                "use super::header::read_radiance_header;\n"
                "use super::layout::{RadianceRasterLayout, build_radiance_scanline_offsets};\n\n"
            )
        elif fname == "tile_decode.rs":
            ins = "use super::layout::RadianceRasterLayout;\nuse super::rle::decode_radiance_rle_scanline;\n\n"
        elif fname == "header.rs":
            ins = "use super::layout::{RadianceRasterLayout, RadianceScanAxis, RadianceScanSign};\n\n"
        else:
            ins = ""
        if ins and ins.strip() not in t:
            t = t.replace(marker, marker + ins, 1)
            p.write_text(t, encoding="utf-8")
        t = p.read_text(encoding="utf-8").replace("use super::types::", "use crate::hdr::types::")
        p.write_text(t, encoding="utf-8")


def fix_tiled_super() -> None:
    base = ROOT / "src/hdr/tiled"
    marker = "// along with this program.  If not, see <https://www.gnu.org/licenses/>.\n\n"
    ins = (
        "use super::buffer::HdrTileBuffer;\n"
        "use super::cache::{HdrTileCache, configured_hdr_tile_cache_max_bytes};\n"
        "use super::globals::{\n"
        "    DEFAULT_HDR_TILE_CACHE_MAX_BYTES, HDR_TILE_CACHE_MAX_BYTES, HdrTileCacheKey,\n"
        "    MAX_HDR_TILE_CACHE_MAX_BYTES,\n"
        "};\n"
        "use super::kind::{HdrTiledSource, HdrTiledSourceKind};\n"
        "use super::validate::{validate_rgba32f_len, validate_tile_bounds};\n\n"
    )
    for fname in ["buffer.rs", "source.rs", "preview.rs", "cache.rs"]:
        p = base / fname
        t = p.read_text(encoding="utf-8")
        if "use super::buffer::HdrTileBuffer" not in t:
            # strip duplicate monolith import block
            rest = t.split(marker, 1)[-1]
            while rest.startswith("use parking_lot") or rest.startswith("use std::") or rest.startswith("use rayon"):
                rest = rest.split("\n\n", 1)[-1]
            p.write_text(marker.join(t.split(marker)[:1]) + marker + ins + rest, encoding="utf-8")


def main() -> None:
    fix_types_imports(ROOT / "src/hdr/decode")
    fix_types_imports(ROOT / "src/hdr/tiled")
    fix_tiled_super()
    fix_wic()
    fix_radiance_tiled()
    print("import fixes applied")


if __name__ == "__main__":
    main()
