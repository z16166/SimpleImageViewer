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

fn main() {
    unsafe {
        std::env::set_var("VCPKG_ALL_STATIC", "1");
    }

    let (installed_dir, vcpkg_triplet) = configure_vcpkg_triplet();
    let mut config = vcpkg::Config::new();
    config.cargo_metadata(true);
    config.target_triplet(&vcpkg_triplet);

    match config.find_package("libjxl") {
        Ok(lib) => {
            for include in lib.include_paths {
                println!("cargo:include={}", include.display());
            }
            // Little CMS 2: needed for CMYK→sRGB when JXL has a black extra channel (see lcms extern in lib.rs).
            // Do **not** call `find_package("lcms2")` on the same `Config` after libjxl: vcpkg-rs leaves
            // `required_libs` populated, so a second `find_package` skips re-resolution but still re-emits
            // every `rustc-link-lib`, which duplicates native libs and rustc reports
            // "overriding linking modifiers from command line is not supported".
            let has_lcms2 = lib.found_names.iter().any(|n| {
                let l = n.to_lowercase();
                l == "lcms2" || l == "lcms"
            });
            if !has_lcms2 {
                let mut lcms_cfg = vcpkg::Config::new();
                lcms_cfg.cargo_metadata(true);
                lcms_cfg.target_triplet(&vcpkg_triplet);
                let _ = lcms_cfg.find_package("lcms2");
            }
        }
        Err(err) => {
            let lib_dir = installed_dir.join(&vcpkg_triplet).join("lib");
            let include_dir = installed_dir.join(&vcpkg_triplet).join("include");
            if !(lib_dir.exists() && include_dir.exists()) {
                panic!("Could not find libjxl via vcpkg: {err:?}");
            }

            println!("cargo:rustc-link-search=native={}", lib_dir.display());
            println!("cargo:include={}", include_dir.display());
            println!("cargo:rustc-link-lib=static=jxl");
            println!("cargo:rustc-link-lib=static=jxl_threads");
            println!("cargo:rustc-link-lib=static=jxl_cms");
            println!("cargo:rustc-link-lib=static=hwy");
            println!("cargo:rustc-link-lib=static=brotlidec");
            println!("cargo:rustc-link-lib=static=brotlienc");
            println!("cargo:rustc-link-lib=static=brotlicommon");
            // Little CMS 2 — used for CMYK→sRGB conversion of JPEG-XL files whose
            // source has a `JXL_CHANNEL_BLACK` extra channel. libjxl's bundled CMS
            // does not auto-convert non-XYB CMYK output; per libjxl PR #237 the
            // proper path is to apply the embedded CMYK ICC profile externally
            // with a 4-channel CMYK input.
            println!("cargo:rustc-link-lib=static=lcms2");
        }
    }
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

    (installed_dir, vcpkg_triplet)
}
