#!/usr/bin/env python3
"""Pubify orchestrator cross-module types and helpers."""
from pathlib import Path

base = Path(__file__).resolve().parents[1] / "src/loader/orchestrator"
types = base / "types.rs"
t = types.read_text(encoding="utf-8")
t = t.replace("// along with this program.  If not, see <https://www.gnu.org/licenses/>.\n// along with this program.", "// along with this program.")
for sym, kind in [
    ("TileRequest", "struct"),
    ("DelayedFallbackJob", "struct"),
    ("EitherDevelop", "enum"),
]:
    t = t.replace(f"{kind} {sym}", f"pub(crate) {kind} {sym}", 1)
t = t.replace("\nfn should_spawn_load_task", "\npub(crate) fn should_spawn_load_task", 1)
types.write_text(t, encoding="utf-8")

for name in ["load.rs", "poll.rs", "tiles.rs"]:
    p = base / name
    pt = p.read_text(encoding="utf-8")
    pt = pt.replace(
        "// along with this program.  If not, see <https://www.gnu.org/licenses/>.\n// along with this program.",
        "// along with this program.",
    )
    p.write_text(pt, encoding="utf-8")

print("fix_orchestrator ok")
