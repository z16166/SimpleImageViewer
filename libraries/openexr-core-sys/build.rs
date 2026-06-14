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

use std::path::Path;

/// `rustc-link-lib=static=stdc++` only resolves if `libstdc++.a` is on a `-L` path; it lives under
/// the toolchain (not vcpkg). Query the same C++ driver as `.cargo/config.toml` (`linker = "g++"`).
fn link_linux_libstdcxx_static() {
    println!("cargo:rerun-if-env-changed=CXX");

    let cxx = std::env::var("CXX").unwrap_or_else(|_| "g++".to_string());
    let output = std::process::Command::new(&cxx)
        .arg("-print-file-name=libstdc++.a")
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "openexr-core-sys (linux): failed to run `{cxx} -print-file-name=libstdc++.a`: {e}"
            )
        });

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        panic!(
            "openexr-core-sys (linux): `{cxx} -print-file-name=libstdc++.a` failed with {}: {stderr}",
            output.status
        );
    }

    let path_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path_str.is_empty() || path_str == "libstdc++.a" {
        panic!(
            "openexr-core-sys (linux): `{cxx}` did not resolve libstdc++.a (got {path_str:?}). \
             Install the static libstdc++ package for your distro (e.g. libstdc++-static) or set CXX."
        );
    }

    let libstd = Path::new(&path_str);
    if !libstd.is_file() {
        panic!(
            "openexr-core-sys (linux): libstdc++.a not found at {} (from `{cxx} -print-file-name=libstdc++.a`)",
            libstd.display()
        );
    }

    let Some(dir) = libstd.parent() else {
        panic!(
            "openexr-core-sys (linux): no directory for libstdc++ path {}",
            libstd.display()
        );
    };

    println!("cargo:rustc-link-search=native={}", dir.display());
    println!("cargo:rustc-link-lib=static=stdc++");
}

fn main() {
    unsafe {
        std::env::set_var("VCPKG_ALL_STATIC", "1");
    }

    let manifest_dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let workspace_root = manifest_dir.parent().unwrap().parent().unwrap();
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
    // Linux + g++: vcpkg-generated link metadata order/peers are unreliable; emit a single explicit
    // static chain (same as fallback) after probing includes only.
    config.cargo_metadata(target_os != "linux");

    let include_dirs = match config.find_package("openexr") {
        Ok(lib) => {
            for include in &lib.include_paths {
                println!("cargo:include={}", include.display());
            }
            if target_os == "linux" {
                let lib_dir = installed_dir.join(&vcpkg_triplet).join("lib");
                if !lib_dir.exists() {
                    panic!(
                        "openexr (linux): expected vcpkg lib dir at {}",
                        lib_dir.display()
                    );
                }
                println!("cargo:rustc-link-search=native={}", lib_dir.display());
                println!("cargo:rustc-link-lib=static=OpenEXRCore-3_4");
                println!("cargo:rustc-link-lib=static=OpenEXR-3_4");
                println!("cargo:rustc-link-lib=static=Iex-3_4");
                println!("cargo:rustc-link-lib=static=IlmThread-3_4");
                println!("cargo:rustc-link-lib=static=Imath-3_2");
                println!("cargo:rustc-link-lib=static=openjph");
                println!("cargo:rustc-link-lib=static=deflate");
                println!("cargo:rustc-link-lib=static=z");
                println!("cargo:rustc-link-lib=dylib=m");
                // `cpp_link_stdlib(None)` silences cc's dynamic stdc++; need `libstdc++.a` dir for `static=stdc++`.
                link_linux_libstdcxx_static();
            }
            lib.include_paths.clone()
        }
        Err(e) => {
            let lib_dir = installed_dir.join(&vcpkg_triplet).join("lib");
            let include_dir = installed_dir.join(&vcpkg_triplet).join("include");
            if !(lib_dir.exists() && include_dir.exists()) {
                panic!("Could not find OpenEXR via vcpkg: {e:?}");
            }

            println!("cargo:rustc-link-search=native={}", lib_dir.display());
            println!("cargo:include={}", include_dir.display());

            if target_os == "windows" {
                println!("cargo:rustc-link-lib=static=OpenEXRCore-3_4");
                println!("cargo:rustc-link-lib=static=OpenEXR-3_4");
                println!("cargo:rustc-link-lib=static=Iex-3_4");
                println!("cargo:rustc-link-lib=static=IlmThread-3_4");
                println!("cargo:rustc-link-lib=static=Imath-3_2");
                println!("cargo:rustc-link-lib=static=openjph.0.27");
                println!("cargo:rustc-link-lib=static=deflatestatic");
                println!("cargo:rustc-link-lib=static=zlibstatic");
            } else {
                println!("cargo:rustc-link-lib=static=OpenEXRCore-3_4");
                println!("cargo:rustc-link-lib=static=OpenEXR-3_4");
                println!("cargo:rustc-link-lib=static=Iex-3_4");
                println!("cargo:rustc-link-lib=static=IlmThread-3_4");
                println!("cargo:rustc-link-lib=static=Imath-3_2");
                println!("cargo:rustc-link-lib=static=openjph");
                println!("cargo:rustc-link-lib=static=deflate");
                println!("cargo:rustc-link-lib=static=z");
                if target_os == "macos" {
                    println!("cargo:rustc-link-lib=dylib=c++");
                } else if target_os == "linux" {
                    println!("cargo:rustc-link-lib=dylib=m");
                    link_linux_libstdcxx_static();
                }
            }
            vec![include_dir]
        }
    };

    let cpp = manifest_dir.join("src/deep_flatten.cpp");
    println!("cargo:rerun-if-changed={}", cpp.display());
    let mut build = cc::Build::new();
    build.cpp(true);
    // Linux: `link_linux_libstdcxx_static()` adds toolchain `-L` + `static=stdc++`; pair with
    // `-static-libstdc++` in `.cargo/config.toml` for the final artifact.
    if target_os == "linux" {
        build.cpp_link_stdlib(None);
    }
    build.file(&cpp);
    for inc in &include_dirs {
        build.include(inc);
        let imath = inc.join("Imath");
        if imath.exists() {
            build.include(imath);
        }
        let openexr = inc.join("OpenEXR");
        if openexr.exists() {
            build.include(openexr);
        }
    }
    if target_os == "windows" {
        build.flag("/std:c++17");
        build.flag("/EHsc");
    } else {
        build.flag("-std=c++17");
    }
    build.compile("siv_openexr_deep_flatten");
}
