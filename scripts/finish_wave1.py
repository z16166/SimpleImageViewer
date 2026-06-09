#!/usr/bin/env python3
"""Split macos_image_io.rs and simplify main.rs."""
from __future__ import annotations

import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SRC = ROOT / "src"


def git_lines(rel: str) -> list[str]:
    return subprocess.check_output(
        ["git", "show", f"HEAD:{rel}"], cwd=ROOT, text=True, encoding="utf-8"
    ).splitlines(keepends=True)


def split_macos_image_io() -> None:
    src = SRC / "macos_image_io.rs"
    if not src.exists():
        return
    lines = git_lines("src/macos_image_io.rs")
    header = "".join(lines[:15])
    imports = "".join(lines[15:43])
    base = SRC / "macos_image_io"
    base.mkdir(exist_ok=True)
    for name, start, end in [
        ("ffi.rs", 44, 204),
        ("tiled.rs", 205, 430),
        ("strip_cache.rs", 431, 685),
        ("stride_decoder.rs", 686, 968),
        ("orientation.rs", 969, 1082),
        ("load.rs", 1083, 1332),
        ("discovery.rs", 1333, 1352),
    ]:
        (base / name).write_text(header + imports + "".join(lines[start - 1 : end]), encoding="utf-8")
    test_body = "".join(lines[1352:])
    mod_rs = header + """mod discovery;
mod ffi;
mod load;
mod orientation;
mod stride_decoder;
mod strip_cache;
mod tiled;

pub use discovery::discover_imageio_codecs;
pub use load::load_via_image_io;
"""
    (base / "mod.rs").write_text(mod_rs, encoding="utf-8")
    if test_body.strip():
        (base / "tests.rs").write_text(header + test_body, encoding="utf-8")
        mod_text = (base / "mod.rs").read_text(encoding="utf-8")
        if "mod tests" not in mod_text:
            (base / "mod.rs").write_text(
                mod_text.replace(
                    "pub use load::load_via_image_io;\n",
                    "pub use load::load_via_image_io;\n\n#[cfg(test)]\nmod tests;\n",
                ),
                encoding="utf-8",
            )
    src.unlink()


def simplify_main() -> None:
    main = SRC / "main.rs"
    text = main.read_text(encoding="utf-8")
    # Keep through module declarations (through windows_utils / wgpu_preprobe)
    end_marker = "#[cfg(target_os = \"windows\")]\nmod windows_utils;"
    idx = text.find(end_marker)
    if idx == -1:
        return
    head_end = idx + len(end_marker)
    head = text[:head_end]
    if "mod startup;" not in head:
        head += "\nmod startup;\n"
    new_main = head + "\n\nfn main() -> eframe::Result {\n    startup::run()\n}\n"
    main.write_text(new_main, encoding="utf-8")


def fix_hdr_native_test() -> None:
    path = ROOT / "tests/hdr_native_dependencies.rs"
    text = path.read_text(encoding="utf-8")

    def read_dir(name: str) -> str:
        d = SRC / "hdr" / name
        if d.is_dir():
            return "".join(p.read_text(encoding="utf-8") for p in sorted(d.rglob("*.rs")))
        p = SRC / "hdr" / f"{name}.rs"
        return p.read_text(encoding="utf-8") if p.exists() else ""

    text = text.replace(
        'let avif = fs::read_to_string(hdr_dir.join("avif.rs")).expect("read avif backend");',
        'let avif = read_hdr_module("avif");',
    )
    text = text.replace(
        'let heif = fs::read_to_string(hdr_dir.join("heif.rs")).expect("read heif backend");',
        'let heif = read_hdr_module("heif");',
    )
    text = text.replace(
        'let jxl = fs::read_to_string(hdr_dir.join("jpegxl.rs")).expect("read jpegxl backend");',
        'let jxl = read_hdr_module("jpegxl");',
    )
    if "fn read_hdr_module" not in text:
        helper = """
fn read_hdr_module(name: &str) -> String {
    let dir = repo_root().join("src").join("hdr").join(name);
    if dir.is_dir() {
        let mut out = String::new();
        for entry in walkdir::WalkDir::new(&dir).into_iter().filter_map(Result::ok) {
            if entry.file_type().is_file() && entry.path().extension().map_or(false, |e| e == "rs") {
                out.push_str(&fs::read_to_string(entry.path()).unwrap());
            }
        }
        return out;
    }
    fs::read_to_string(repo_root().join("src").join("hdr").join(format!("{name}.rs")))
        .unwrap_or_default()
}

"""
        text = text.replace("fn read_loader_source()", helper + "fn read_loader_source()")
    path.write_text(text, encoding="utf-8")


def main() -> None:
    split_macos_image_io()
    simplify_main()
    fix_hdr_native_test()
    print("main/macos/test updates done")


if __name__ == "__main__":
    main()
