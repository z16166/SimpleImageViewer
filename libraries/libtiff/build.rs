fn main() {
    // Force static linking
    unsafe { std::env::set_var("VCPKG_ALL_STATIC", "1"); }
    
    // In Manifest Mode, vcpkg installs to vcpkg_installed/ in the workspace root
    let manifest_dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let workspace_root = manifest_dir.parent().unwrap().parent().unwrap(); // libraries/pkg -> libraries -> root
    let installed_dir = workspace_root.join("vcpkg_installed");
    
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let vcpkg_triplet = std::env::var("VCPKG_DEFAULT_TRIPLET").unwrap_or_else(|_| {
        match (target_os.as_str(), target_arch.as_str()) {
            ("windows", "x86_64") => "x64-windows-static".to_string(),
            ("windows", "x86") => "x86-windows-static".to_string(),
            ("windows", "aarch64") => "arm64-windows-static".to_string(),
            ("macos", "x86_64") => "x64-osx".to_string(),
            ("macos", "aarch64") => "arm64-osx".to_string(),
            ("linux", "x86_64") => "x64-linux".to_string(),
            ("linux", "aarch64") => "arm64-linux".to_string(),
            _ => "x64-windows-static".to_string(),
        }
    });

    if installed_dir.exists() {
        unsafe { 
            std::env::set_var("VCPKG_INSTALLED_DIR", &installed_dir); 
            std::env::set_var("VCPKG_TARGET_TRIPLET", &vcpkg_triplet);
        }
    }

    let mut config = vcpkg::Config::new();
    config.cargo_metadata(true);

    match config.find_package("tiff") {
        Ok(lib) => {
            for include in lib.include_paths {
                println!("cargo:include={}", include.display());
            }
        }
        Err(e) => {
            let lib_dir = installed_dir.join(&vcpkg_triplet).join("lib");
            let include_dir = installed_dir.join(&vcpkg_triplet).join("include");
            if lib_dir.exists() {
                println!("cargo:rustc-link-search=native={}", lib_dir.display());
                println!("cargo:rustc-link-lib=static=tiff");
                
                println!("cargo:rustc-link-lib=static=jpeg");
                
                if lib_dir.join("zlibstatic.lib").exists() {
                    println!("cargo:rustc-link-lib=static=zlibstatic");
                } else if lib_dir.join("zlib.lib").exists() {
                    println!("cargo:rustc-link-lib=static=zlib");
                } else if lib_dir.join("libz-ng.a").exists() || lib_dir.join("z-ng.lib").exists() {
                    println!("cargo:rustc-link-lib=static=z-ng");
                } else {
                    println!("cargo:rustc-link-lib=static=z"); // libz.a
                }
                
                if lib_dir.join("liblzma.a").exists() || lib_dir.join("lzma.lib").exists() {
                    println!("cargo:rustc-link-lib=static=lzma");
                }

                // libdeflate
                if lib_dir.join("deflatestatic.lib").exists() {
                    println!("cargo:rustc-link-lib=static=deflatestatic");
                } else if lib_dir.join("libdeflate.a").exists() || lib_dir.join("libdeflate.lib").exists() {
                    let name = if target_os == "windows" { "libdeflate" } else { "deflate" };
                    println!("cargo:rustc-link-lib=static={}", name);
                }

                // Lerc
                if lib_dir.join("libLerc.a").exists() || lib_dir.join("Lerc.lib").exists() {
                    println!("cargo:rustc-link-lib=static=Lerc");
                }

                // Zstd
                if lib_dir.join("libzstd.a").exists() || lib_dir.join("zstd.lib").exists() {
                    println!("cargo:rustc-link-lib=static=zstd");
                }

                // WebP
                if lib_dir.join("libwebp.a").exists() || lib_dir.join("libwebp.lib").exists() {
                    let name = if target_os == "windows" { "libwebp" } else { "webp" };
                    println!("cargo:rustc-link-lib=static={}", name);
                } else if lib_dir.join("webp.lib").exists() {
                    println!("cargo:rustc-link-lib=static=webp");
                }
                if lib_dir.join("libsharpyuv.a").exists() || lib_dir.join("libsharpyuv.lib").exists() {
                    let name = if target_os == "windows" { "libsharpyuv" } else { "sharpyuv" };
                    println!("cargo:rustc-link-lib=static={}", name);
                }
                if lib_dir.join("libwebpdecoder.a").exists() || lib_dir.join("libwebpdecoder.lib").exists() {
                    let name = if target_os == "windows" { "libwebpdecoder" } else { "webpdecoder" };
                    println!("cargo:rustc-link-lib=static={}", name);
                }
                println!("cargo:include={}", include_dir.display());
            } else {
                panic!("Could not find tiff via vcpkg or fallback: {:?}", e);
            }
        }
    }
}
