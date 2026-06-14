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

/// Final-link OpenMP runtime used by LibRaw (`raw_r`) when built with `openmp` feature.
fn emit_openmp_link_args(target_os: &str) {
    match target_os {
        "linux" => link_linux_libgomp_static(),
        "windows" => {
            // MSVC OpenMP import lib; runtime is vcomp140.dll (or arch-matched vcomp*.dll).
            println!("cargo:rustc-link-lib=dylib=vcomp");
        }
        "macos" => {
            for prefix in ["/opt/homebrew/opt/libomp", "/usr/local/opt/libomp"] {
                let lib_dir = format!("{prefix}/lib");
                if std::path::Path::new(&lib_dir).join("libomp.dylib").exists() {
                    println!("cargo:rustc-link-search=native={lib_dir}");
                    break;
                }
            }
            println!("cargo:rustc-link-lib=dylib=omp");
        }
        _ => {}
    }
}

/// `rustc-link-lib=static=gomp` needs `libgomp.a` on a `-L` path (toolchain dir, not vcpkg).
fn link_linux_libgomp_static() {
    use std::path::Path;

    println!("cargo:rerun-if-env-changed=CXX");

    let cxx = std::env::var("CXX").unwrap_or_else(|_| "g++".to_string());
    let output = std::process::Command::new(&cxx)
        .arg("-print-file-name=libgomp.a")
        .output()
        .unwrap_or_else(|e| {
            panic!("libraw-sys (linux): failed to run `{cxx} -print-file-name=libgomp.a`: {e}")
        });

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        panic!(
            "libraw-sys (linux): `{cxx} -print-file-name=libgomp.a` failed with {}: {stderr}",
            output.status
        );
    }

    let path_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path_str.is_empty() || path_str == "libgomp.a" {
        panic!(
            "libraw-sys (linux): `{cxx}` did not resolve libgomp.a (got {path_str:?}). \
             Enable gcc-toolset OpenMP (libgomp.a ships with the gcc C++ driver) or set CXX."
        );
    }

    let libgomp = Path::new(&path_str);
    if !libgomp.is_file() {
        panic!(
            "libraw-sys (linux): libgomp.a not found at {} (from `{cxx} -print-file-name=libgomp.a`)",
            libgomp.display()
        );
    }

    let Some(dir) = libgomp.parent() else {
        panic!(
            "libraw-sys (linux): no directory for libgomp path {}",
            libgomp.display()
        );
    };

    println!("cargo:rustc-link-search=native={}", dir.display());
    println!("cargo:rustc-link-lib=static=gomp");
}

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

    let vcpkg_lib = config.find_package("libraw");

    let mut build = cc::Build::new();
    build.cpp(true);
    if target_os == "linux" {
        build.cpp_link_stdlib(None);
    }

    match &vcpkg_lib {
        Ok(lib) => {
            for include in &lib.include_paths {
                build.include(include);
            }
            if target_os == "linux" {
                let lib_dir = installed_dir.join(&vcpkg_triplet).join("lib");
                println!("cargo:rustc-link-search=native={}", lib_dir.display());
                println!("cargo:rustc-link-lib=static=sharpyuv");
            }
        }
        Err(e) => {
            let include_dir = installed_dir.join(&vcpkg_triplet).join("include");
            let lib_dir = installed_dir.join(&vcpkg_triplet).join("lib");
            if include_dir.exists() {
                build.include(include_dir);
                println!("cargo:rustc-link-search=native={}", lib_dir.display());

                // On Windows it's raw_r.lib, on Unix it's libraw_r.a or libraw.a
                if lib_dir.join("libraw_r.a").exists() || lib_dir.join("raw_r.lib").exists() {
                    println!("cargo:rustc-link-lib=static=raw_r");
                } else {
                    println!("cargo:rustc-link-lib=static=raw");
                }

                println!("cargo:rustc-link-lib=static=jasper");
                println!("cargo:rustc-link-lib=static=lcms2");
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

                // libtiff and its transitive dependencies
                println!("cargo:rustc-link-lib=static=tiff");
                if lib_dir.join("deflatestatic.lib").exists() {
                    println!("cargo:rustc-link-lib=static=deflatestatic");
                } else if lib_dir.join("libdeflate.a").exists()
                    || lib_dir.join("libdeflate.lib").exists()
                {
                    let name = if target_os == "windows" {
                        "libdeflate"
                    } else {
                        "deflate"
                    };
                    println!("cargo:rustc-link-lib=static={}", name);
                }
                if lib_dir.join("libLerc.a").exists() || lib_dir.join("Lerc.lib").exists() {
                    println!("cargo:rustc-link-lib=static=Lerc");
                }
                if lib_dir.join("libzstd.a").exists() || lib_dir.join("zstd.lib").exists() {
                    println!("cargo:rustc-link-lib=static=zstd");
                }
                if lib_dir.join("libwebp.a").exists() || lib_dir.join("libwebp.lib").exists() {
                    let name = if target_os == "windows" {
                        "libwebp"
                    } else {
                        "webp"
                    };
                    println!("cargo:rustc-link-lib=static={}", name);
                } else if lib_dir.join("webp.lib").exists() {
                    println!("cargo:rustc-link-lib=static=webp");
                }
                if lib_dir.join("libsharpyuv.a").exists()
                    || lib_dir.join("libsharpyuv.lib").exists()
                {
                    let name = if target_os == "windows" {
                        "libsharpyuv"
                    } else {
                        "sharpyuv"
                    };
                    println!("cargo:rustc-link-lib=static={}", name);
                }
            } else {
                panic!("Could not find libraw via vcpkg or fallback: {:?}", e);
            }
        }
    }

    build.file(manifest_dir.join("src").join("libraw_shims.cpp"));
    build.warnings(false);

    // Explicitly set static_crt based on CARGO_CFG_TARGET_FEATURE
    let target_features = std::env::var("CARGO_CFG_TARGET_FEATURE").unwrap_or_default();
    if target_os == "windows" {
        build.static_crt(target_features.contains("crt-static"));
    }

    build.compile("raw_shims");

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "windows" {
        println!("cargo:rustc-link-lib=m");

        // macOS: system libc++. Linux: do not emit dylib=stdc++ — that forces NEEDED libstdc++.so.6
        // and defeats `.cargo/config.toml` / CI `RUSTFLAGS` `-static-libstdc++` for release binaries.
        if target_os == "macos" {
            println!("cargo:rustc-link-lib=dylib=c++");
        } else if target_os == "linux" {
            // Explicitly dynamically link libc to assist rust-lld in resolving glibc 2.38+ symbols
            // like __isoc23_strtol that may be referenced by statically compiled vcpkg C dependencies.
            println!("cargo:rustc-link-lib=dylib=c");
        }
    }

    emit_openmp_link_args(&target_os);

    println!("cargo:rerun-if-changed=src/libraw_shims.cpp");
    println!("cargo:rerun-if-changed=../../vcpkg.json");
}
