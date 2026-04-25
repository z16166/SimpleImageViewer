fn main() {
    // Force static linking
    unsafe {
        std::env::set_var("VCPKG_ALL_STATIC", "1");
    }

    // In Manifest Mode, vcpkg installs to vcpkg_installed/ in the workspace root
    let manifest_dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let workspace_root = manifest_dir.parent().unwrap().parent().unwrap(); // libraries/pkg -> libraries -> root
    let installed_dir = workspace_root.join("vcpkg_installed");
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let vcpkg_triplet = match (target_os.as_str(), target_arch.as_str()) {
        ("windows", "x86_64") => "x64-windows-static",
        ("windows", "x86") => "x86-windows-static",
        ("windows", "aarch64") => "arm64-windows-static",
        ("macos", "x86_64") => "x64-osx",
        ("macos", "aarch64") => "arm64-osx",
        ("linux", "x86_64") => "x64-linux",
        ("linux", "aarch64") => "arm64-linux",
        _ => "x64-windows-static",
    };

    if installed_dir.exists() {
        unsafe {
            std::env::set_var("VCPKG_INSTALLED_DIR", &installed_dir);
            std::env::set_var("VCPKG_TARGET_TRIPLET", vcpkg_triplet);
        }
    }

    let mut include_paths = Vec::new();

    if let Ok(lib) = vcpkg::Config::new()
        .cargo_metadata(true)
        .find_package("monkeys-audio")
    {
        include_paths = lib.include_paths;
    } else {
        let sdk_root = workspace_root.join("3rdparty").join("monkey-sdk");

        let mut build = cc::Build::new();
        build.cpp(true);

        // Add include paths
        let maclib_inc = sdk_root.join("Source").join("MACLib");
        let shared_inc = sdk_root.join("Source").join("Shared");
        build.include(&maclib_inc);
        build.include(&shared_inc);
        include_paths.push(maclib_inc);
        include_paths.push(shared_inc);

        // Define platform
        let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
        if target_os == "windows" {
            build.define("PLATFORM_WINDOWS", None);
        } else if target_os == "linux" {
            build.define("PLATFORM_LINUX", None);
        } else if target_os == "macos" {
            build.define("PLATFORM_APPLE", None);
        }

        let target_features = std::env::var("CARGO_CFG_TARGET_FEATURE").unwrap_or_default();
        if target_os == "windows" {
            build.static_crt(target_features.contains("crt-static"));
        } // Add all necessary source files from Monkey Audio
        // This is based on the CMakeLists.txt findings
        let shared_src = sdk_root.join("Source").join("Shared");
        let maclib_src = sdk_root.join("Source").join("MACLib");

        let files = [
            shared_src.join("BufferIO.cpp"),
            shared_src.join("CharacterHelper.cpp"),
            shared_src.join("CircleBuffer.cpp"),
            shared_src.join("CPUFeatures.cpp"),
            shared_src.join("CRC.cpp"),
            shared_src.join("GlobalFunctions.cpp"),
            shared_src.join("MemoryIO.cpp"),
            shared_src.join("Semaphore.cpp"),
            shared_src.join("Thread.cpp"),
            shared_src.join("WholeFileIO.cpp"),
            shared_src.join(if target_os == "windows" {
                "WinFileIO.cpp"
            } else {
                "StdLibFileIO.cpp"
            }),
            maclib_src.join("APECompress.cpp"),
            maclib_src.join("APECompressCore.cpp"),
            maclib_src.join("APECompressCreate.cpp"),
            maclib_src.join("APEDecompress.cpp"),
            maclib_src.join("APEDecompressCore.cpp"),
            maclib_src.join("APEHeader.cpp"),
            maclib_src.join("APEInfo.cpp"),
            maclib_src.join("APELink.cpp"),
            maclib_src.join("APETag.cpp"),
            maclib_src.join("BitArray.cpp"),
            maclib_src.join("FloatTransform.cpp"),
            maclib_src.join("MACLib.cpp"),
            maclib_src.join("MACProgressHelper.cpp"),
            maclib_src.join("MD5.cpp"),
            maclib_src.join("NewPredictor.cpp"),
            maclib_src.join("NNFilter.cpp"),
            maclib_src.join("NNFilterGeneric.cpp"),
            maclib_src.join("Prepare.cpp"),
            maclib_src.join("UnBitArray.cpp"),
            maclib_src.join("UnBitArrayBase.cpp"),
            maclib_src.join("WAVInputSource.cpp"),
            maclib_src.join("Old").join("AntiPredictorOld.cpp"),
            maclib_src.join("Old").join("AntiPredictorExtraHighOld.cpp"),
            maclib_src.join("Old").join("AntiPredictorFastOld.cpp"),
            maclib_src.join("Old").join("AntiPredictorHighOld.cpp"),
            maclib_src.join("Old").join("AntiPredictorNormalOld.cpp"),
            maclib_src.join("Old").join("APEDecompressCoreOld.cpp"),
            maclib_src.join("Old").join("APEDecompressOld.cpp"),
            maclib_src.join("Old").join("UnBitArrayOld.cpp"),
            maclib_src.join("Old").join("UnMACOld.cpp"),
        ];

        for file in &files {
            build.file(file);
        }

        // SIMD files
        let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
        if target_arch == "x86" || target_arch == "x86_64" {
            build.file(maclib_src.join("NNFilterAVX2.cpp"));
            build.file(maclib_src.join("NNFilterAVX512.cpp"));
            build.file(maclib_src.join("NNFilterSSE2.cpp"));
            build.file(maclib_src.join("NNFilterSSE4.1.cpp"));

            if build.get_compiler().is_like_msvc() {
                // MSVC doesn't need special flags for SSE2/SSE4 but needs for AVX
                // Wait! cc crate's build.file doesn't allow per-file flags easily.
                // But Monkey Audio's source has #ifdefs or we can just try to compile and see.
                // Actually, I'll skip AVX512 for now if it causes issues, but let's try.
            }
        } else if target_arch == "aarch64" || target_arch == "arm" {
            build.file(maclib_src.join("NNFilterNeon.cpp"));
        }

        build.compile("MACLib");
    }

    let mut build = cc::Build::new();
    build.cpp(true);

    for include in &include_paths {
        build.include(include);
    }

    build.file(manifest_dir.join("wrapper").join("monkey_wrapper.cpp"));

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "windows" {
        build.define("PLATFORM_WINDOWS", None);
        let target_features = std::env::var("CARGO_CFG_TARGET_FEATURE").unwrap_or_default();
        build.static_crt(target_features.contains("crt-static"));
    } else if target_os == "linux" {
        build.define("PLATFORM_LINUX", None);
    } else if target_os == "macos" {
        build.define("PLATFORM_APPLE", None);
    }

    build.compile("monkey_wrapper");

    println!("cargo:rerun-if-changed=wrapper/monkey_wrapper.cpp");
}
