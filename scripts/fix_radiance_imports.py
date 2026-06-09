#!/usr/bin/env python3
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MARKER = "// along with this program.  If not, see <https://www.gnu.org/licenses/>.\n"

# radiance
src = ROOT / "src/hdr/radiance_tiled/source.rs"
st = src.read_text(encoding="utf-8")
st = st.replace("use super::layout::{RadianceRasterLayout, build_radiance_scanline_offsets};\n", "")
if "use super::header::read_radiance_header" not in st:
    st = st.replace(
        MARKER,
        MARKER
        + "use super::header::{build_radiance_scanline_offsets, read_radiance_header};\n"
        + "use super::layout::RadianceRasterLayout;\n\n",
        1,
    )
src.write_text(st, encoding="utf-8")

rle = ROOT / "src/hdr/radiance_tiled/rle.rs"
rt = rle.read_text(encoding="utf-8")
# Strip accidental tile_decode imports prepended into rle.rs
bad = (
    "use super::header::{decode_radiance_rgba32f_from_mmap, read_radiance_header};\n"
    "use super::layout::{RadianceRasterLayout, RadianceScanAxis, RadianceScanSign};\n"
    "use super::rle::decode_radiance_rle_scanline;\n\n"
    "use parking_lot::Mutex;\n"
    "use std::io::{BufRead, Cursor, Read};\n"
    "use std::path::{Path, PathBuf};\n"
    "use std::sync::Arc;\n\n"
    "use crate::hdr::tiled::{\n"
    "    HdrTileBuffer, HdrTileCache, HdrTiledSource, HdrTiledSourceKind,\n"
    "    configured_hdr_tile_cache_max_bytes, validate_tile_bounds,\n"
    "};\n"
    "use crate::hdr::types::{HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat};\n\n"
)
rt = rt.replace(bad, "")
rle.write_text(rt, encoding="utf-8")

hdr = ROOT / "src/hdr/radiance_tiled/header.rs"
ht = hdr.read_text(encoding="utf-8")
ht = ht.replace(
    "use super::layout::{RadianceRasterLayout, RadianceScanAxis, RadianceScanSign};\n",
    "",
    1,
)
hdr.write_text(ht, encoding="utf-8")

td = ROOT / "src/hdr/radiance_tiled/tile_decode.rs"
td.write_text(
    td.read_text(encoding="utf-8").replace(
        "use super::rle::decode_radiance_rle_scanline;",
        "use super::rle::read_scanline;",
    ),
    encoding="utf-8",
)

print("fix_radiance_imports ok")
