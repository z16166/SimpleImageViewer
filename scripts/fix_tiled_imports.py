#!/usr/bin/env python3
"""Fix tiled/radiance import blocks after rebuild."""
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
TILED = ROOT / "src/hdr/tiled"
RADIANCE = ROOT / "src/hdr/radiance_tiled"
MARKER = "// along with this program.  If not, see <https://www.gnu.org/licenses/>.\n"

SOURCE_IMPORTS = (
    "use super::buffer::HdrTileBuffer;\n"
    "use super::cache::{HdrTileCache, configured_hdr_tile_cache_max_bytes};\n"
    "use super::globals::{\n"
    "    DEFAULT_HDR_TILE_CACHE_MAX_BYTES, HDR_TILE_CACHE_MAX_BYTES, HdrTileCacheKey,\n"
    "    MAX_HDR_TILE_CACHE_MAX_BYTES,\n"
    "};\n"
    "use super::kind::{HdrTiledSource, HdrTiledSourceKind};\n"
    "use super::preview::downsample_hdr_image_nearest;\n"
    "use super::validate::{validate_rgba32f_len, validate_tile_bounds};\n\n"
)

PREVIEW_IMPORTS = (
    "use super::buffer::HdrTileBuffer;\n"
    "use super::cache::{HdrTileCache, configured_hdr_tile_cache_max_bytes};\n"
    "use super::globals::{\n"
    "    DEFAULT_HDR_TILE_CACHE_MAX_BYTES, HDR_TILE_CACHE_MAX_BYTES, HdrTileCacheKey,\n"
    "    MAX_HDR_TILE_CACHE_MAX_BYTES,\n"
    "};\n"
    "use super::kind::{HdrTiledSource, HdrTiledSourceKind};\n"
    "use super::validate::{validate_rgba32f_len, validate_tile_bounds};\n\n"
)

CACHE_IMPORTS = (
    "use super::buffer::HdrTileBuffer;\n"
    "use super::globals::{\n"
    "    DEFAULT_HDR_TILE_CACHE_MAX_BYTES, HDR_TILE_CACHE_MAX_BYTES, HdrTileCacheKey,\n"
    "    MAX_HDR_TILE_CACHE_MAX_BYTES,\n"
    "};\n\n"
)

BUFFER_IMPORTS = (
    "use crate::hdr::types::{HdrColorSpace, HdrImageMetadata, IsoDeferredTileContext};\n"
    "use std::sync::atomic::Ordering;\n"
    "use std::sync::Arc;\n\n"
    "use super::globals::NEXT_HDR_TILE_CACHE_ID;\n\n"
)


def replace_imports(path: Path, block: str) -> None:
    t = path.read_text(encoding="utf-8")
    if MARKER not in t:
        return
    head, rest = t.split(MARKER, 1)
    while rest.lstrip("\n").startswith("use "):
        rest = rest.lstrip("\n")
        if "\n\n" in rest:
            _, rest = rest.split("\n\n", 1)
        else:
            break
    if not rest.startswith("\n"):
        rest = "\n" + rest
    path.write_text(head + MARKER + block + rest, encoding="utf-8")


def main() -> None:
    replace_imports(TILED / "source.rs", SOURCE_IMPORTS)
    replace_imports(TILED / "preview.rs", PREVIEW_IMPORTS)
    replace_imports(TILED / "cache.rs", CACHE_IMPORTS)
    replace_imports(TILED / "buffer.rs", BUFFER_IMPORTS)

    cache = TILED / "cache.rs"
    ct = cache.read_text(encoding="utf-8")
    ct = ct.replace("pub fn configured_hdr_tile_cache_max_bytes", "pub(crate) fn configured_hdr_tile_cache_max_bytes", 1)
    cache.write_text(ct, encoding="utf-8")

    src = RADIANCE / "source.rs"
    st = src.read_text(encoding="utf-8")
    st = st.replace(
        "use super::header::{build_radiance_scanline_offsets, read_radiance_header};\n",
        "use super::header::read_radiance_header;\n"
        "use super::layout::{RadianceRasterLayout, build_radiance_scanline_offsets};\n",
    )
    src.write_text(st, encoding="utf-8")

    print("fix_tiled_imports ok")


if __name__ == "__main__":
    main()
