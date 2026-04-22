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

    let mut build = cc::Build::new();
    build.cpp(true)
        .include(sdk_root.join("Source/MACLib"))
        .include(sdk_root.join("Source/Shared"))
        .file(manifest_dir.join("wrapper/monkey_wrapper.cpp"))
        // Shared Source
        .file(sdk_root.join("Source/Shared/BufferIO.cpp"))
        .file(sdk_root.join("Source/Shared/CharacterHelper.cpp"))
        .file(sdk_root.join("Source/Shared/CircleBuffer.cpp"))
        .file(sdk_root.join("Source/Shared/CPUFeatures.cpp"))
        .file(sdk_root.join("Source/Shared/CRC.cpp"))
        .file(sdk_root.join("Source/Shared/GlobalFunctions.cpp"))
        .file(sdk_root.join("Source/Shared/MemoryIO.cpp"))
        .file(sdk_root.join("Source/Shared/Semaphore.cpp"))
        .file(sdk_root.join("Source/Shared/Thread.cpp"))
        .file(sdk_root.join("Source/Shared/WholeFileIO.cpp"))
        // MACLib Source
        .file(sdk_root.join("Source/MACLib/APECompress.cpp"))
        .file(sdk_root.join("Source/MACLib/APECompressCore.cpp"))
        .file(sdk_root.join("Source/MACLib/APECompressCreate.cpp"))
        .file(sdk_root.join("Source/MACLib/APEDecompress.cpp"))
        .file(sdk_root.join("Source/MACLib/APEDecompressCore.cpp"))
        .file(sdk_root.join("Source/MACLib/APEHeader.cpp"))
        .file(sdk_root.join("Source/MACLib/APEInfo.cpp"))
        .file(sdk_root.join("Source/MACLib/APELink.cpp"))
        .file(sdk_root.join("Source/MACLib/APETag.cpp"))
        .file(sdk_root.join("Source/MACLib/BitArray.cpp"))
        .file(sdk_root.join("Source/MACLib/FloatTransform.cpp"))
        .file(sdk_root.join("Source/MACLib/MACLib.cpp"))
        .file(sdk_root.join("Source/MACLib/MACProgressHelper.cpp"))
        .file(sdk_root.join("Source/MACLib/MD5.cpp"))
        .file(sdk_root.join("Source/MACLib/NewPredictor.cpp"))
        .file(sdk_root.join("Source/MACLib/NNFilter.cpp"))
        .file(sdk_root.join("Source/MACLib/NNFilterGeneric.cpp"))
        .file(sdk_root.join("Source/MACLib/Prepare.cpp"))
        .file(sdk_root.join("Source/MACLib/UnBitArray.cpp"))
        .file(sdk_root.join("Source/MACLib/UnBitArrayBase.cpp"))
        .file(sdk_root.join("Source/MACLib/WAVInputSource.cpp"))
        // Old Source
        .file(sdk_root.join("Source/MACLib/Old/AntiPredictorOld.cpp"))
        .file(sdk_root.join("Source/MACLib/Old/AntiPredictorExtraHighOld.cpp"))
        .file(sdk_root.join("Source/MACLib/Old/AntiPredictorFastOld.cpp"))
        .file(sdk_root.join("Source/MACLib/Old/AntiPredictorHighOld.cpp"))
        .file(sdk_root.join("Source/MACLib/Old/AntiPredictorNormalOld.cpp"))
        .file(sdk_root.join("Source/MACLib/Old/APEDecompressCoreOld.cpp"))
        .file(sdk_root.join("Source/MACLib/Old/APEDecompressOld.cpp"))
        .file(sdk_root.join("Source/MACLib/Old/UnBitArrayOld.cpp"))
        .file(sdk_root.join("Source/MACLib/Old/UnMACOld.cpp"));

    if target_os == "windows" {
        build.define("PLATFORM_WINDOWS", None);
        build.file(sdk_root.join("Source/Shared/WinFileIO.cpp"));
    } else {
        if target_os == "linux" {
            build.define("PLATFORM_LINUX", None);
        } else if target_os == "macos" {
            build.define("PLATFORM_APPLE", None);
        }
        build.file(sdk_root.join("Source/Shared/StdLibFileIO.cpp"));
    }

    // SIMD optimizations
    if target_arch == "x86_64" || target_arch == "x86" {
        build.file(sdk_root.join("Source/MACLib/NNFilterAVX512.cpp"))
            .file(sdk_root.join("Source/MACLib/NNFilterAVX2.cpp"))
            .file(sdk_root.join("Source/MACLib/NNFilterSSE2.cpp"))
            .file(sdk_root.join("Source/MACLib/NNFilterSSE4.1.cpp"));
    } else if target_arch == "aarch64" {
        build.file(sdk_root.join("Source/MACLib/NNFilterNeon.cpp"));
    }

    build.compile("monkey_sdk");
    
    println!("cargo:rerun-if-changed={}", sdk_root.display());
    println!("cargo:rerun-if-changed={}", manifest_dir.join("wrapper/monkey_wrapper.cpp").display());
}
