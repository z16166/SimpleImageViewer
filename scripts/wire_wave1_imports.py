#!/usr/bin/env python3
"""Wire cross-module imports for wave-1 splits."""
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MARKER = "// along with this program.  If not, see <https://www.gnu.org/licenses/>.\n"


def prepend(path: Path, block: str) -> None:
    text = path.read_text(encoding="utf-8")
    if block.strip() not in text:
        idx = text.find(MARKER)
        if idx == -1:
            return
        insert_at = idx + len(MARKER)
        path.write_text(text[:insert_at] + block + text[insert_at:], encoding="utf-8")


def main() -> None:
    prepend(
        ROOT / "src/audio/loop_state.rs",
        "use super::cue::{load_cue, CueSheet};\n"
        "use super::player::AudioError;\n"
        "use super::playlist::{build_base_non_m3u_set, expand_m3u_excluding_base, is_m3u_path};\n"
        "use super::slots::{\n"
        "    set_cue_markers, set_cue_track, set_current_path, set_current_track, set_error, set_metadata,\n"
        "};\n"
        "use super::sources::symphonia::{get_file_metadata, open_source};\n\n",
    )
    prepend(
        ROOT / "src/audio/run_loop.rs",
        "use super::loop_state::{AudioLoopState, AudioSlots};\n"
        "use super::player::{AudioCommand, AudioError};\n\n",
    )
    prepend(
        ROOT / "src/libtiff_loader/load.rs",
        "use std::ffi::CString;\n"
        "use std::path::PathBuf;\n\n"
        "use crate::hdr::types::{\n"
        "    HdrColorProfile, HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrLuminanceMetadata,\n"
        "    HdrPixelFormat, HdrReference, HdrToneMapSettings, HdrTransferFunction,\n"
        "};\n"
        "use super::decode::{\n"
        "    decode_ieee_scene_linear_rgba32f, decode_logl_logluv_scene_linear_rgba32f,\n"
        "    decode_uint16_rgb_scene_linear_rgba32f,\n"
        "    tiff_ieee_scene_linear_eligible, tiff_logl_logluv_hdr_eligible,\n"
        "    tiff_uint16_rgb_scene_linear_eligible,\n"
        "};\n"
        "use super::handle::create_tiff_handle;\n"
        "use super::scanline::manual_decode_scanline;\n\n",
    )
    print("wire_wave1_imports ok")


if __name__ == "__main__":
    main()
