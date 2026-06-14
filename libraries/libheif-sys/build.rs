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

/// On Linux, Rust defaults to `static:+bundle`, which copies archive members into dependent rlibs.
/// That defers C++ objects (e.g. from `libheif.a`) until after `libstdc++.a` is scanned and can force
/// `libstdc++.so.6` into `DT_NEEDED`. `static:-bundle` keeps the `.a` for the final link only.
fn rustc_link_lib_static(name: &str) {
    if std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() == "linux" {
        println!("cargo:rustc-link-lib=static:-bundle={name}");
    } else {
        println!("cargo:rustc-link-lib=static={name}");
    }
}

fn main() {
    unsafe {
        std::env::set_var("VCPKG_ALL_STATIC", "1");
    }

    let (installed_dir, vcpkg_triplet) = configure_vcpkg_triplet();
    let mut config = vcpkg::Config::new();
    config.cargo_metadata(true);
    config.target_triplet(&vcpkg_triplet);

    match config.find_package("libheif") {
        Ok(lib) => {
            for include in lib.include_paths {
                println!("cargo:include={}", include.display());
            }
            // Manifest-mode installs sometimes omit transitive ports from metadata; libheif.lib
            // still references openjpeg / jpeg / dav1d / brotli / zlib when matching vcpkg.json features.
            let names = &lib.found_names;
            let has_openjp2 = names.iter().any(|n| n.eq_ignore_ascii_case("openjp2"));
            let has_dav1d = names.iter().any(|n| n.eq_ignore_ascii_case("dav1d"));
            let has_brotli_enc = names.iter().any(|n| n.eq_ignore_ascii_case("brotlienc"));
            let has_brotli = names.iter().any(|n| {
                let l = n.to_lowercase();
                l.contains("brotli")
            });
            if !(has_openjp2 && has_dav1d && has_brotli) {
                println_libheif_optional_codec_libs_static(&installed_dir, &vcpkg_triplet);
            } else if !has_brotli_enc {
                // UNCI compressor uses encoder API (BrotliEncoder*); manifest-mode libs often omit brotlienc.
                rustc_link_lib_static("brotlienc");
            }
        }
        Err(err) => {
            let lib_dir = installed_dir.join(&vcpkg_triplet).join("lib");
            let include_dir = installed_dir.join(&vcpkg_triplet).join("include");
            if !(lib_dir.exists() && include_dir.exists()) {
                panic!("Could not find libheif via vcpkg: {err:?}");
            }

            println!("cargo:rustc-link-search=native={}", lib_dir.display());
            let debug_lib_dir = installed_dir.join(&vcpkg_triplet).join("debug").join("lib");
            let profile = std::env::var("PROFILE").unwrap_or_else(|_| "debug".into());
            if profile != "release" && debug_lib_dir.exists() {
                println!("cargo:rustc-link-search=native={}", debug_lib_dir.display());
            }
            println!("cargo:include={}", include_dir.display());
            println_libheif_core_static_libs();
            println_libheif_optional_codec_libs_static(&installed_dir, &vcpkg_triplet);
        }
    }
}

/// Core codec libraries for static libheif. **MSVC** vcpkg uses `libde265.lib` / `x265-static.lib`
/// naming in this tree; **Unix** linkers expect `-lde265` / `-lx265` (i.e. `libde265.a`, never
/// `llibde265` — do not pass the `lib` prefix to `rustc-link-lib`).
fn println_libheif_core_static_libs() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let is_msvc = target_os == "windows" && target_env == "msvc";

    rustc_link_lib_static("heif");
    if is_msvc {
        rustc_link_lib_static("libde265");
        rustc_link_lib_static("x265-static");
    } else {
        rustc_link_lib_static("de265");
        rustc_link_lib_static("x265");
    }
}

/// Static libs for `vcpkg.json` libheif features: `openjpeg`, `jpeg`, `dav1d`,
/// UNCI zlib + brotli (`iso23001-17` installs both; port enables `VCPKG_LOCK_FIND_PACKAGE_Brotli`).
/// `dav1d` enables AV1 items without libaom.
///
/// OpenH264 (`h264-decoder`) is intentionally omitted: the pinned vcpkg port's
/// `vcpkg_check_features` key does not match the feature name (`openh264` vs `h264-decoder`);
/// add `openh264` here if you enable a fixed port or use a global vcpkg tree that links it.
fn println_libheif_optional_codec_libs_static(
    installed_dir: &std::path::Path,
    vcpkg_triplet: &str,
) {
    rustc_link_lib_static("openjp2");
    rustc_link_lib_static("jpeg");
    rustc_link_lib_static("dav1d");
    rustc_link_lib_static("brotlienc");
    rustc_link_lib_static("brotlidec");
    rustc_link_lib_static("brotlicommon");
    println_static_zlib_for_vcpkg_installed(installed_dir, vcpkg_triplet);
}

fn zlib_lib_search_dirs(
    installed_dir: &std::path::Path,
    vcpkg_triplet: &str,
) -> Vec<std::path::PathBuf> {
    let lib_dir = installed_dir.join(vcpkg_triplet).join("lib");
    let debug_lib_dir = installed_dir.join(vcpkg_triplet).join("debug").join("lib");
    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "debug".into());
    if profile != "release" && debug_lib_dir.exists() {
        vec![debug_lib_dir, lib_dir]
    } else {
        vec![lib_dir]
    }
}

/// zlib-ng in compat mode (`vcpkg-overlays/zlib`) typically ships `zlibstatic.lib` / `zlib.lib`; the stock
/// vcpkg zlib port uses MSVC import libs named `zs` / `zsd`. Probe installed artifacts like `libraw-sys`.
fn println_static_zlib_for_vcpkg_installed(installed_dir: &std::path::Path, vcpkg_triplet: &str) {
    let dirs = zlib_lib_search_dirs(installed_dir, vcpkg_triplet);
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "debug".into());

    let msvc_windows = target_os == "windows" && target_env == "msvc";
    if msvc_windows {
        for dir in &dirs {
            if dir.join("zlibstatic.lib").exists() {
                rustc_link_lib_static("zlibstatic");
                return;
            }
            if dir.join("zlib.lib").exists() {
                rustc_link_lib_static("zlib");
                return;
            }
            let stem = if profile == "release" { "zs" } else { "zsd" };
            if dir.join(format!("{stem}.lib")).exists() {
                rustc_link_lib_static(stem);
                return;
            }
        }
        rustc_link_lib_static(if profile == "release" { "zs" } else { "zsd" });
        return;
    }

    for dir in &dirs {
        if dir.join("zlibstatic.lib").exists() {
            rustc_link_lib_static("zlibstatic");
            return;
        }
        if dir.join("zlib.lib").exists() {
            rustc_link_lib_static("zlib");
            return;
        }
        if dir.join("libz-ng.a").exists() || dir.join("z-ng.lib").exists() {
            rustc_link_lib_static("z-ng");
            return;
        }
    }

    rustc_link_lib_static("z");
}

fn configure_vcpkg_triplet() -> (std::path::PathBuf, String) {
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
            ("linux", "aarch64") => "arm64-linux-v8a".to_string(),
            _ => "x64-windows-static".to_string(),
        }
    });

    if installed_dir.exists() {
        unsafe {
            std::env::set_var("VCPKG_INSTALLED_DIR", &installed_dir);
            std::env::set_var("VCPKG_TARGET_TRIPLET", &vcpkg_triplet);
            std::env::set_var("VCPKGRS_TRIPLET", &vcpkg_triplet);
        }
    }

    let vcpkg_manifest = workspace_root.join("vcpkg.json");
    if vcpkg_manifest.exists() {
        println!("cargo:rerun-if-changed={}", vcpkg_manifest.display());
    }

    (installed_dir, vcpkg_triplet)
}
