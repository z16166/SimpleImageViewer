#!/usr/bin/env python3
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

SKIP = {"mod", "tests"}


def pubify(path: Path) -> None:
    text = path.read_text(encoding="utf-8")
    original = text
    for kw in ["fn", "struct", "enum", "trait", "type"]:
        text = re.sub(rf"(?m)^({kw}) ", rf"pub(crate) \1 ", text)
    text = re.sub(r"pub\(crate\) pub\(crate\)", "pub(crate)", text)
    text = re.sub(r"pub pub\(crate\)", "pub(crate)", text)
    text = re.sub(r"pub\(crate\) pub ", "pub ", text)
    if text != original:
        path.write_text(text, encoding="utf-8")


def main() -> None:
    for sub in [
        ROOT / "src/hdr/openexr_core",
        ROOT / "src/libtiff_loader/decode.rs",
    ]:
        if sub.is_file():
            pubify(sub)
        else:
            for path in sub.glob("*.rs"):
                if path.stem in SKIP:
                    continue
                pubify(path)
    print("pubify_wave1 ok")


if __name__ == "__main__":
    main()
