// Simple Image Viewer - A high-performance, cross-platform image viewer
// Copyright (C) 2024-2026 Simple Image Viewer Contributors
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

use std::path::{PathBuf};

fn main() {
    let root = PathBuf::from("../../3rdparty/LibRaw");
    let src = root.join("src");

    let mut build = cc::Build::new();
    build.cpp(true);
    build.define("NO_LCMS", None);
    build.define("NO_JASPER", None);
    build.define("NO_JPEG", None);
    build.define("LIBRAW_NOTHREADS", None); // Simplified single-threaded build for now

    // Platform specific flags and macros
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();

    if target_env == "msvc" {
        build.define("LIBRAW_NODLL", None);
        build.define("_CRT_SECURE_NO_DEPRECATE", None);
        build.define("_CRT_NONSTDC_NO_DEPRECATE", None);
        build.flag("/utf-8"); // Ensure source files are read as UTF-8
    } else {
        // Unix-like systems (Linux, macOS)
        build.flag("-fPIC"); 
    }

    if target_os == "macos" {
        // Handle Apple Silicon vs Intel if needed, but cc handles -arch
    }

    build.include(&root);
    build.include(&src);
    build.include(root.join("libraw")); // Canonical header location

    let subdirs = [
        "decoders",
        "decompressors",
        "demosaic",
        "integration",
        "metadata",
        "postprocessing",
        "preprocessing",
        "tables",
        "utils",
        "write",
        "x3f",
        "internal",
    ];

    build.file(src.join("libraw_c_api.cpp"));
    build.file(src.join("libraw_datastream.cpp"));

    for subdir in &subdirs {
        let dir = src.join(subdir);
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().map_or(false, |ext| ext == "cpp") {
                    let filename = path.file_name().unwrap().to_string_lossy();
                    if filename.contains("dngsdk_glue") || 
                       filename.contains("rawspeed_glue") ||
                       filename.ends_with("_ph.cpp") 
                    {
                        continue;
                    }
                    build.file(path);
                }
            }
        }
    }

    build.warnings(false);
    build.compile("raw");

    if target_os != "windows" {
        println!("cargo:rustc-link-lib=m");
    }

    println!("cargo:rerun-if-changed=../../3rdparty/LibRaw");
}
