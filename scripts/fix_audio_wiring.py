#!/usr/bin/env python3
from pathlib import Path

MARKER = "// along with this program.  If not, see <https://www.gnu.org/licenses/>.\n"
base = Path(__file__).resolve().parents[1] / "src/audio"


def prepend(path, extra):
    t = path.read_text(encoding="utf-8")
    if extra.strip() not in t:
        idx = t.find(MARKER)
        if idx != -1:
            path.write_text(t[: idx + len(MARKER)] + extra + t[idx + len(MARKER) :], encoding="utf-8")


def pub_fn(path, name):
    t = path.read_text(encoding="utf-8")
    t = t.replace(f"\nfn {name}", f"\npub(crate) fn {name}", 1)
    t = t.replace(f"\nstruct {name}", f"\npub(crate) struct {name}", 1)
    path.write_text(t, encoding="utf-8")


prepend(
    base / "run_loop.rs",
    "use super::loop_state::{AudioLoopState, AudioSlots};\n"
    "use super::player::{AudioCommand, AudioError};\n"
    "use super::slots::{set_current_track, set_metadata};\n\n",
)
pub_fn(base / "cue.rs", "load_cue")
for s in ["AudioSlots", "AudioLoopState"]:
    pub_fn(base / "loop_state.rs", s)

run_loop = base / "run_loop.rs"
rt = run_loop.read_text(encoding="utf-8")
if "pub(crate) fn run_audio_loop" not in rt:
    run_loop.write_text(rt.replace("fn run_audio_loop", "pub(crate) fn run_audio_loop", 1), encoding="utf-8")

player = base / "player.rs"
pt = player.read_text(encoding="utf-8")
if "use super::run_loop::run_audio_loop" not in pt:
    player.write_text(pt.replace(MARKER, MARKER + "use super::run_loop::run_audio_loop;\n\n", 1), encoding="utf-8")

sym = base / "sources/symphonia.rs"
st = sym.read_text(encoding="utf-8")
for fn in ["get_file_metadata", "create_source", "open_source"]:
    st = st.replace(f"\nfn {fn}", f"\npub(crate) fn {fn}", 1)
if "use super::ape::ApeSource" not in st:
    st = st.replace(MARKER, MARKER + "use super::ape::ApeSource;\n\n", 1)
sym.write_text(st, encoding="utf-8")

ape = base / "sources/ape.rs"
ape.write_text(ape.read_text(encoding="utf-8").replace("struct ApeSource", "pub(crate) struct ApeSource", 1), encoding="utf-8")
print("audio wiring ok")
