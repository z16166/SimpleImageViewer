// Simple Image Viewer - A high-performance, cross-platform image viewer
// Copyright (C) 2024-2026 Simple Image Viewer Contributors
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    
    // Explicitly navigate up to workspace root: libraries/monkey-sdk-sys -> libraries -> root
    let workspace_root = manifest_dir.parent().unwrap().parent().unwrap();
    let sdk_root_raw = workspace_root.join("3rdparty/monkey-sdk");
    
    // Physical normalization
    let sdk_root_canonical = std::fs::canonicalize(&sdk_root_raw)
        .expect(&format!("Could not find Monkey's Audio SDK at {:?}. Please ensure it is downloaded in 3rdparty/monkey-sdk", sdk_root_raw));
        
    let sdk_root_str = sdk_root_canonical.to_string_lossy();
    let sdk_root = if sdk_root_str.starts_with(r"\\?\") {
        PathBuf::from(&sdk_root_str[4..])
    } else {
        sdk_root_canonical
    };

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();

    // 1. Build Monkey's Audio SDK with CMake
    // We use our own CMakeLists.txt in the current directory to avoid modifying 3rdparty
    let mut config = cmake::Config::new(&manifest_dir);
    config
        .define("BUILD_SHARED", "OFF")
        .define("BUILD_UTIL", "OFF")
        .define("MONKEY_SDK_ROOT", sdk_root.to_string_lossy().replace("\\", "/"))
        .static_crt(true);

    // Help CMake find the right architecture and enable SIMD
    let arch_macro = match target_arch.as_str() {
        "x86_64" => "x86_64",
        "x86" => "x86",
        "aarch64" => "aarch64",
        "arm" => "armhf", // Assume armhf for 32-bit arm as per CMakeLists.txt logic
        _ => "unknown",
    };
    config.define("ARCHITECTURE", arch_macro);
    
    if target_arch == "aarch64" {
        config.define("CMAKE_SYSTEM_PROCESSOR", "aarch64");
    } else if target_arch == "x86_64" {
        config.define("CMAKE_SYSTEM_PROCESSOR", "x86_64");
    }

    let dst = config.build();

    // 2. Build the wrapper and link with the library
    let mut build = cc::Build::new();
    build.cpp(true)
        .include(sdk_root.join("Source/MACLib"))
        .include(sdk_root.join("Source/Shared"))
        .include(sdk_root.join("Shared"))
        .file(manifest_dir.join("wrapper/monkey_wrapper.cpp"));

    if target_os == "windows" {
        build.define("PLATFORM_WINDOWS", None);
    } else {
        if target_os == "linux" {
            build.define("PLATFORM_LINUX", None);
        } else if target_os == "macos" {
            build.define("PLATFORM_APPLE", None);
        }
    }

    build.compile("monkey_wrapper");

    // 3. Link with the CMake-built library
    println!("cargo:rustc-link-search=native={}", dst.join("lib").display());
    // Some systems might use lib64
    println!("cargo:rustc-link-search=native={}", dst.join("lib64").display());
    println!("cargo:rustc-link-lib=static=MAC");

    println!("cargo:rerun-if-changed={}", sdk_root.display());
    println!("cargo:rerun-if-changed={}", manifest_dir.join("wrapper/monkey_wrapper.cpp").display());
}
