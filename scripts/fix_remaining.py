#!/usr/bin/env python3
"""Fix remaining cross-module visibility for wave-1 splits."""
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[1]


def pubify_struct_fields(text: str, struct_name: str) -> str:
    pattern = rf"(pub(?:\(crate\))? struct {re.escape(struct_name)} \{{)(.*?)(\n\}})"
    m = re.search(pattern, text, re.DOTALL)
    if not m:
        return text
    head, body, tail = m.group(1), m.group(2), m.group(3)
    lines = body.splitlines(keepends=True)
    out = []
    for line in lines:
        if re.match(r"^\s+pub(?:\(crate\)|\(\w+\))?\s+\w", line) or line.strip().startswith("//"):
            out.append(line)
            continue
        out.append(re.sub(r"^(\s+)(\w)", r"\1pub(crate) \2", line, count=1))
    return text[: m.start()] + head + "".join(out) + tail + text[m.end() :]


def main() -> None:
    ch = ROOT / "src/hdr/openexr_core/channels.rs"
    ct = ch.read_text(encoding="utf-8")
    ct = ct.replace(
        "use crate::hdr::openexr_core::{DEFAULT_DECODED_CHUNK_CACHE_BYTES, MAX_DECODED_CHUNK_CACHE_BYTES};",
        "use super::{DEFAULT_DECODED_CHUNK_CACHE_BYTES, MAX_DECODED_CHUNK_CACHE_BYTES};",
    )
    ch.write_text(ct, encoding="utf-8")

    rc = ROOT / "src/hdr/openexr_core/read_context.rs"
    rt = rc.read_text(encoding="utf-8")
    if "configured_decoded_chunk_cache_max_bytes" not in rt:
        rt = rt.replace(
            "compression_name, copy_channels,",
            "compression_name, configured_decoded_chunk_cache_max_bytes, copy_channels,",
        )
        rt = rt.replace(
            "decoded_chunk_key, extent_from_window_axis,",
            "decoded_chunk_key, exr_result, extent_from_window_axis,",
        )
    if "OpenExrCoreRgbaTile" not in rt.split("use super::types")[1].split("};")[0]:
        rt = rt.replace("OpenExrCorePartInfo,", "OpenExrCorePartInfo, OpenExrCoreRgbaTile,")
    rc.write_text(rt, encoding="utf-8")

    pl = ROOT / "src/audio/playlist.rs"
    pt = pl.read_text(encoding="utf-8")
    for fn in ["build_base_non_m3u_set", "expand_m3u_excluding_base", "is_m3u_path", "collect_music_files"]:
        pt = pt.replace(f"\nfn {fn}", f"\npub(crate) fn {fn}", 1)
    pl.write_text(pt, encoding="utf-8")

    ls = ROOT / "src/audio/loop_state.rs"
    lt = ls.read_text(encoding="utf-8")
    lt = lt.replace("use super::cue::load_cue;\n\n", "")
    for fn in [
        "handle_stop",
        "handle_pause",
        "handle_play",
        "handle_seek",
        "handle_set_device",
        "handle_next_file",
        "handle_prev_file",
        "handle_next_track",
        "handle_prev_track",
        "handle_set_playlist",
    ]:
        lt = lt.replace(f"\n    fn {fn}", f"\n    pub(crate) fn {fn}", 1)
    ls.write_text(lt, encoding="utf-8")

    sm = ROOT / "src/audio/sources/mod.rs"
    sm.write_text(sm.read_text(encoding="utf-8").replace("mod symphonia;", "pub(crate) mod symphonia;"), encoding="utf-8")

    types = ROOT / "src/loader/orchestrator/types.rs"
    tt = types.read_text(encoding="utf-8")
    for struct in ["TileRequest", "DelayedFallbackJob", "ImageLoader"]:
        tt = pubify_struct_fields(tt, struct)
    types.write_text(tt, encoding="utf-8")

    orch_import = (
        "use super::types::{\n"
        "    DelayedFallbackJob, ImageLoader, TileInFlightKey, TileRequest, should_spawn_load_task,\n"
        "};\n\n"
    )
    for name in ["load.rs", "poll.rs", "tiles.rs"]:
        p = ROOT / "src/loader/orchestrator" / name
        t = p.read_text(encoding="utf-8")
        if "DelayedFallbackJob" not in t.split("impl ImageLoader")[0]:
            marker = "// along with this program.  If not, see <https://www.gnu.org/licenses/>.\n"
            if marker in t:
                t = t.replace(marker, marker + orch_import, 1)
        t = t.replace(
            "use super::types::{ImageLoader, TileRequest, should_spawn_load_task};",
            "use super::types::{\n    DelayedFallbackJob, ImageLoader, TileInFlightKey, TileRequest, should_spawn_load_task,\n};",
        )
        p.write_text(t, encoding="utf-8")

    print("fix_remaining ok")


if __name__ == "__main__":
    main()
