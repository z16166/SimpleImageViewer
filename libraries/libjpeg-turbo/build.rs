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
    // Force static linking across all platforms
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

    // libjpeg-turbo 3.2 links PNG I/O against libspng (`WITH_SYSTEM_SPNG`).
    // When `find_package` succeeds, the patched vcpkg crate already emits
    // `static:-bundle=spng_static` because the port `Depends:` on libspng.
    // A second `static=spng_static` (default `+bundle`) makes rustc reject the
    // crate: "overriding linking modifiers from command line is not supported".
    // That is the Linux x64 CI failure. The fallback path does not list libspng,
    // so only then do we add it ourselves.
    let spng_already_linked = match config.find_package("libjpeg-turbo") {
        Ok(lib) => {
            for include in lib.include_paths {
                println!("cargo:include={}", include.display());
            }
            lib.found_names
                .iter()
                .any(|name| name.to_ascii_lowercase().contains("spng"))
        }
        Err(e) => {
            // Fallback: manually set paths if we know where they are
            let lib_dir = installed_dir.join(&vcpkg_triplet).join("lib");
            let include_dir = installed_dir.join(&vcpkg_triplet).join("include");
            if lib_dir.exists() {
                println!("cargo:rustc-link-search=native={}", lib_dir.display());
                println!("cargo:rustc-link-lib=static=jpeg");
                println!("cargo:rustc-link-lib=static=turbojpeg");
                println!("cargo:include={}", include_dir.display());
            } else {
                panic!("Could not find libjpeg-turbo and fallback failed: {:?}", e);
            }
            false
        }
    };

    if !spng_already_linked {
        link_system_spng(&installed_dir, &vcpkg_triplet);
    }
}

/// Static libspng is `spng_static.lib` / `libspng_static.a`. Shared builds use `spng`.
fn link_system_spng(installed_dir: &std::path::Path, vcpkg_triplet: &str) {
    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "debug".into());
    let mut dirs = Vec::new();
    if profile != "release" {
        dirs.push(installed_dir.join(vcpkg_triplet).join("debug").join("lib"));
    }
    dirs.push(installed_dir.join(vcpkg_triplet).join("lib"));

    for dir in dirs {
        if !dir.is_dir() {
            continue;
        }
        println!("cargo:rerun-if-changed={}", dir.display());
        if dir.join("spng_static.lib").exists() || dir.join("libspng_static.a").exists() {
            println!("cargo:rustc-link-search=native={}", dir.display());
            rustc_link_lib_static("spng_static");
            return;
        }
        if dir.join("spng.lib").exists() || dir.join("libspng.a").exists() {
            println!("cargo:rustc-link-search=native={}", dir.display());
            rustc_link_lib_static("spng");
            return;
        }
    }
}

/// Linux uses `static:-bundle`, same as patched vcpkg-rs, so the archive is linked
/// at the final link instead of being packed into this rlib.
fn rustc_link_lib_static(name: &str) {
    if std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() == "linux" {
        println!("cargo:rustc-link-lib=static:-bundle={name}");
    } else {
        println!("cargo:rustc-link-lib=static={name}");
    }
}
