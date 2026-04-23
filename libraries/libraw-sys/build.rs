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

// use std::path::{PathBuf};

fn main() {
    let manifest_dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    
    // Explicitly navigate up to workspace root: libraries/libraw-sys -> libraries -> root
    let workspace_root = manifest_dir.parent().expect("Failed to get libraries parent")
        .parent().expect("Failed to get workspace root");
    let root = workspace_root.join("3rdparty").join("LibRaw");
    
    println!("cargo:info=libraw-sys: Checking for LibRaw source at: {}", root.display());

    if !root.exists() || !root.join("libraw/libraw.h").exists() {
        println!("cargo:warning=libraw-sys: LibRaw source not found or incomplete at {}. Build will likely fail.", root.display());
    } else {
        println!("cargo:info=libraw-sys: Found LibRaw source directory.");
    }

    let src = root.join("src");

    let mut build = cc::Build::new();
    build.cpp(true);
    build.define("NO_LCMS", None);
    build.define("NO_JASPER", None);
    build.define("USE_JPEG", None);
    build.define("USE_X3FTOOLS", None);
    build.define("LIBRAW_NODLL", None);
    // build.define("LIBRAW_NOTHREADS", None); // Experts recommend NOT defining this for thread-safe parallel usage

    // Platform specific flags and macros
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();

    if target_os == "windows" {
        build.define("WIN32", None);
        build.define("_WIN32", None);
    }

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
    
    // Get libjpeg paths from the libjpeg-turbo build script
    if let Ok(jpeg_include) = std::env::var("DEP_JPEG_TURBO_CUSTOM_INCLUDE") {
        build.include(jpeg_include);
    }
    if let Ok(jpeg_src) = std::env::var("DEP_JPEG_TURBO_CUSTOM_SRC") {
        build.include(jpeg_src);
    }

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
    build.file(manifest_dir.join("src").join("libraw_shims.cpp"));

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

    // Link against the libjpeg-turbo static library built by the dependency crate (via CMake)
    if let Ok(jpeg_include) = std::env::var("DEP_JPEG_TURBO_CUSTOM_INCLUDE") {
        // include/ and lib/ are siblings under the CMake install prefix
        let prefix = std::path::PathBuf::from(&jpeg_include)
            .parent()
            .unwrap()
            .to_path_buf();
        println!("cargo:rustc-link-search=native={}", prefix.join("lib").display());
        println!("cargo:rustc-link-search=native={}", prefix.join("lib64").display());
    }
    if let Ok(lib_name) = std::env::var("DEP_JPEG_TURBO_CUSTOM_LIB_NAME") {
        println!("cargo:rustc-link-lib=static={}", lib_name);
    }

    if target_os != "windows" {
        println!("cargo:rustc-link-lib=m");
    }

    println!("cargo:rerun-if-changed={}", root.display());
    println!("cargo:rerun-if-changed={}", manifest_dir.join("src/libraw_shims.cpp").display());
    println!("cargo:rerun-if-changed={}", manifest_dir.join("build.rs").display());
}
