#!/usr/bin/env python3
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
base = ROOT / "src/audio"


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


def pub_method(path: Path, name: str) -> None:
    t = path.read_text(encoding="utf-8")
    t = t.replace(f"\n    fn {name}", f"\n    pub(crate) fn {name}", 1)
    path.write_text(t, encoding="utf-8")


def main() -> None:
    cue = base / "cue.rs"
    ct = cue.read_text(encoding="utf-8")
    ct = pubify_struct_fields(ct, "CueSheet")
    cue.write_text(ct, encoding="utf-8")

    ls = base / "loop_state.rs"
    lt = ls.read_text(encoding="utf-8")
    lt = pubify_struct_fields(lt, "AudioLoopState")
    lt = pubify_struct_fields(lt, "AudioSlots")
    ls.write_text(lt, encoding="utf-8")

    for fn in [
        "new",
        "feed_next_file",
        "recover_orphaned_backend",
        "update_cue_track_highlight",
        "update_position",
    ]:
        pub_method(ls, fn)

    rl = base / "run_loop.rs"
    rt = rl.read_text(encoding="utf-8")
    rt = rt.replace("\nuse super::player::AudioCommand;\n", "\n")
    if "Ordering" not in rt.split("impl")[0]:
        rt = rt.replace(
            "use std::sync::atomic::AtomicBool;\n",
            "use std::sync::atomic::{AtomicBool, Ordering};\n",
            1,
        )
    rl.write_text(rt, encoding="utf-8")

    pl = base / "playlist.rs"
    pt = pl.read_text(encoding="utf-8")
    if "read_text_file_with_fallback" in pt and "pub(crate) fn read_text_file_with_fallback" not in pt:
        pt = pt.replace(
            "\nfn read_text_file_with_fallback",
            "\npub(crate) fn read_text_file_with_fallback",
            1,
        )
        pl.write_text(pt, encoding="utf-8")

    print("fix_audio_loop_state ok")


if __name__ == "__main__":
    main()
