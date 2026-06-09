#!/usr/bin/env python3
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MARKER = "// along with this program.  If not, see <https://www.gnu.org/licenses/>.\n"

load = ROOT / "src/libtiff_loader/load.rs"
extra = (
    "use parking_lot::Mutex;\n\n"
    "use crate::hdr::decode::hdr_to_sdr_rgba8_with_tone_settings;\n"
    "use super::decode::try_camera_tiff_rgb8_hdr_upgrade;\n"
    "use super::scanline::LibTiffScanlineSource;\n"
    "use super::tiled::LibTiffTiledSource;\n\n"
)
t = load.read_text(encoding="utf-8")
if "LibTiffTiledSource" not in t.split("fn load_tiff")[0]:
    t = t.replace(MARKER, MARKER + extra, 1)
load.write_text(t, encoding="utf-8")

handle = ROOT / "src/libtiff_loader/handle.rs"
ht = handle.read_text(encoding="utf-8")
ht = ht.replace("    ptr: *mut c_void,", "    pub(crate) ptr: *mut c_void,", 1)
handle.write_text(ht, encoding="utf-8")

decode = ROOT / "src/libtiff_loader/decode.rs"
dt = decode.read_text(encoding="utf-8")
dt = dt.replace("\nfn try_camera_tiff_rgb8_hdr_upgrade", "\npub(crate) fn try_camera_tiff_rgb8_hdr_upgrade", 1)
decode.write_text(dt, encoding="utf-8")

slots = ROOT / "src/audio/slots.rs"
st = slots.read_text(encoding="utf-8")
st = st.replace("\nfn set_error", "\npub(crate) fn set_error", 1)
slots.write_text(st, encoding="utf-8")

print("fix_libtiff_cross ok")
