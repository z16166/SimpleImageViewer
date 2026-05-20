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

use std::path::{Path, PathBuf};

/// Linux: if `CARGO_TARGET_*_LINKER` points at `gcc` (not `g++`), the final link often omits libstdc++
/// or mishandles vcpkg C++ archives (`condition_variable`, sized `operator delete`, etc.).
fn linux_warn_if_cpp_linker_is_wrong() {
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let key = match arch.as_str() {
        "x86_64" => "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER",
        "aarch64" => "CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER",
        _ => return,
    };
    if let Ok(linker) = std::env::var(key) {
        let base = Path::new(&linker)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(linker.as_str());
        let is_gcc = base == "gcc" || base.ends_with("-gcc");
        let is_gxx =
            base == "g++" || base.ends_with("-g++") || base == "c++" || base.ends_with("-c++");
        if is_gcc && !is_gxx {
            println!(
                "cargo:warning=Linux needs a C++ link driver (g++), not {linker} ({key}). \
                 Use `.cargo/config.toml` default or export {key}=g++ — gcc causes missing libstdc++ symbols."
            );
        }
    }
}

/// Trailing full paths to `libstdc++.a` for GNU bfd `ld` when mixed static `.a` + `-bundle` pull C++ objects
/// late (e.g. libde265). LLVM lld ignores single-pass ordering; repeating stays safe with `-fuse-ld=lld`.
fn linux_link_libstdcxx_a_last() {
    println!("cargo:rerun-if-env-changed=CXX");

    let cxx = std::env::var("CXX").unwrap_or_else(|_| "g++".to_string());
    let output = std::process::Command::new(&cxx)
        .arg("-print-file-name=libstdc++.a")
        .output()
        .unwrap_or_else(|e| {
            panic!("simple-image-viewer (linux): failed to run `{cxx} -print-file-name=libstdc++.a`: {e}")
        });

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        panic!(
            "simple-image-viewer (linux): `{cxx} -print-file-name=libstdc++.a` failed with {}: {stderr}",
            output.status
        );
    }

    let path_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path_str.is_empty() || path_str == "libstdc++.a" {
        panic!(
            "simple-image-viewer (linux): `{cxx}` did not resolve libstdc++.a (got {path_str:?}). \
             Install libstdc++ dev/static for your toolchain or set CXX."
        );
    }

    let libstd = Path::new(&path_str);
    if !libstd.is_file() {
        panic!(
            "simple-image-viewer (linux): libstdc++.a not found at {} (from `{cxx} -print-file-name=libstdc++.a`)",
            libstd.display()
        );
    }

    let p = libstd.display();
    println!("cargo:rustc-link-arg={}", p);
    println!("cargo:rustc-link-arg={}", p);
}

/// When `SIV_CI_LINK_MAP` is set, write a link map (`-Map=`) for the **this package's** link only (not deps),
/// so CI can grep what pulled `libstdc++.so.6`. Path is stable for Alma: `target/simple-image-viewer-final.link.map`.
fn linux_ci_link_map(manifest_dir: &Path) {
    let enabled = matches!(
        std::env::var("SIV_CI_LINK_MAP").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    );
    if !enabled {
        return;
    }
    println!("cargo:rerun-if-env-changed=SIV_CI_LINK_MAP");
    println!("cargo:rerun-if-env-changed=CARGO_TARGET_DIR");

    let target_root = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest_dir.join("target"));
    let map_path = target_root.join("simple-image-viewer-final.link.map");
    println!(
        "cargo:rustc-link-arg=-Wl,-Map={}",
        map_path.to_string_lossy().replace('\\', "/")
    );
}

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=locales");
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("assets/icon.png").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("assets/icon.ico").display()
    );

    // vcpkg libtiff pkg-config lists webpdecoder/webpmux but not libwebp; tif_webp.c still
    // needs encoder APIs. Do not use cargo:rustc-link-lib here: Cargo splits native libs away from
    // adjacent rustc-link-arg, so --push-state/--pop-state ended up empty while -lwebp was moved
    // early and dropped under -Wl,--as-needed before libtiff.a.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "linux" {
        linux_warn_if_cpp_linker_is_wrong();
        let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
        let vcpkg_triplet =
            std::env::var("VCPKG_DEFAULT_TRIPLET").unwrap_or_else(|_| match target_arch.as_str() {
                "x86_64" => "x64-linux".to_string(),
                "aarch64" => "arm64-linux".to_string(),
                _ => String::new(),
            });
        if !vcpkg_triplet.is_empty() {
            let lib_dir = manifest_dir
                .join("vcpkg_installed")
                .join(&vcpkg_triplet)
                .join("lib");
            let webp_a = lib_dir.join("libwebp.a");
            let sharpyuv_a = lib_dir.join("libsharpyuv.a");
            if webp_a.is_file() && sharpyuv_a.is_file() {
                println!("cargo:rustc-link-arg=-Wl,--push-state,--no-as-needed");
                println!("cargo:rustc-link-arg={}", webp_a.display());
                println!("cargo:rustc-link-arg={}", sharpyuv_a.display());
                println!("cargo:rustc-link-arg=-Wl,--pop-state");
            }
        }

        linux_ci_link_map(&manifest_dir);

        linux_link_libstdcxx_a_last();
    }

    // Generate the ICO from the source image (PNG)
    let src = manifest_dir.join("assets/icon.png");
    let dst = manifest_dir.join("assets/icon.ico");

    if src.exists() {
        match png_to_ico(&src, &dst) {
            Ok(()) => {}
            Err(e) => eprintln!("build.rs: icon conversion failed: {e}"),
        }
    } else {
        eprintln!("build.rs: assets/icon.png not found, skipping ICO generation");
    }

    // Non-Windows: embed 256×256 RGBA for `ViewportBuilder::with_icon`. Windows reads the same
    // pixels from the PE icon resource (winresource id 1) so the exe only carries one icon copy.
    if target_os != "windows" {
        match emit_viewport_icon_rgba(&manifest_dir, &out_dir) {
            Ok(()) => {}
            Err(e) => panic!("build.rs: emit_viewport_icon_rgba failed: {e}"),
        }
    }

    // Embed Windows resources (icon + metadata) into the PE
    // Compile C++ WASAPI helper and Windows resources
    #[cfg(target_os = "windows")]
    {
        embed_resources(&dst);

        let mut b = cc::Build::new();
        b.cpp(true);
        b.file("src/audio_helper.cpp");

        let target_features = std::env::var("CARGO_CFG_TARGET_FEATURE").unwrap_or_default();
        b.static_crt(target_features.contains("crt-static"));

        b.compile("audio_helper");

        println!("cargo:rustc-link-lib=ole32");
        println!("cargo:rustc-link-lib=uuid");
        println!("cargo:rustc-link-lib=dbghelp");

        // When building for legacy Windows 7 (using YY-Thunks and VC-LTL5),
        // we must ensure the correct subsystem and entry point are set.
        // We do this here rather than via global RUSTFLAGS to avoid affecting
        // intermediate DLLs or proc-macros.
        if std::env::var("CARGO_FEATURE_LEGACY_WIN7").is_ok() {
            println!("cargo:rustc-link-arg=/SUBSYSTEM:WINDOWS,6.01");
            println!("cargo:rustc-link-arg=/ENTRY:mainCRTStartup");
        }
    }
}

/// 256×256 RGBA8 for [`egui::IconData`], matching runtime `load_icon` resize (Lanczos3).
/// Written to `OUT_DIR` so `main.rs` can `include_bytes!` without decoding PNG at startup.
fn emit_viewport_icon_rgba(
    manifest_dir: &Path,
    out_dir: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    use image::imageops::FilterType;

    const W: u32 = 256;
    const H: u32 = 256;
    let src = manifest_dir.join("assets/icon.png");
    let dst = out_dir.join("siv_window_icon_rgba256.bin");

    let raw: Vec<u8> = if src.is_file() {
        let img = image::open(&src)?;
        img.resize_exact(W, H, FilterType::Lanczos3)
            .to_rgba8()
            .into_raw()
    } else {
        vec![0u8; (W * H * 4) as usize]
    };

    debug_assert_eq!(raw.len(), (W * H * 4) as usize);
    std::fs::write(&dst, raw)?;
    Ok(())
}

/// Convert a PNG to a multi-resolution ICO (16, 32, 48, 64, 128, 256 px).
///
/// Always encodes 32-bit RGBA PNG frames so Windows keeps per-pixel alpha (PNG-in-ICO).
fn png_to_ico(
    src: &std::path::Path,
    dst: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    use image::ExtendedColorType;
    use image::codecs::ico::{IcoEncoder, IcoFrame};
    use image::imageops::FilterType;
    use std::fs::File;
    use std::io::BufWriter;

    let src_rgba = image::open(src)?.to_rgba8();
    if !src_rgba.pixels().any(|px| px[3] < 255) {
        println!(
            "cargo:warning=icon.png has no transparent pixels; \
             re-export as RGBA PNG if the icon should have a transparent background"
        );
    }
    let sizes: &[u32] = &[16, 32, 48, 64, 128, 256];

    let mut frames = Vec::with_capacity(sizes.len());
    for &sz in sizes {
        let scaled = image::imageops::resize(&src_rgba, sz, sz, FilterType::Lanczos3);
        frames.push(IcoFrame::as_png(
            scaled.as_raw(),
            sz,
            sz,
            ExtendedColorType::Rgba8,
        )?);
    }

    IcoEncoder::new(BufWriter::new(File::create(dst)?)).encode_images(&frames)?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn embed_resources(ico_path: &std::path::Path) {
    let mut res = winresource::WindowsResource::new();

    if ico_path.exists() {
        res.set_icon(&ico_path.display().to_string());
    }

    // 1. Get version from Cargo.toml
    let pkg_version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string());

    // 2. Attempt to get Git Commit ID (short hash)
    let git_hash = std::process::Command::new("git")
        .args(&["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
            } else {
                None
            }
        });

    // 3. Prepare String Version (e.g. 0.9.1-abc1234)
    let display_version = if let Some(hash) = git_hash {
        format!("{}-{}", pkg_version, hash)
    } else {
        pkg_version.clone()
    };

    // 4. Prepare Numeric Version (u64 - A.B.C.D where each is 16-bits)
    let parts: Vec<u64> = pkg_version
        .split('.')
        .map(|s| s.parse::<u64>().unwrap_or(0))
        .collect();
    let major = *parts.get(0).unwrap_or(&0);
    let minor = *parts.get(1).unwrap_or(&0);
    let patch = *parts.get(2).unwrap_or(&0);
    let build = *parts.get(3).unwrap_or(&0);

    // Construct u64: AAAA BBBB CCCC DDDD in hex
    let version_u64: u64 = (major << 48) | (minor << 32) | (patch << 16) | build;

    res.set_version_info(winresource::VersionInfo::FILEVERSION, version_u64);
    res.set_version_info(winresource::VersionInfo::PRODUCTVERSION, version_u64);

    res.set("ProductName", "Simple Image Viewer");
    res.set("FileDescription", "Simple Image Viewer");
    res.set("InternalName", "SimpleImageViewer.exe");
    res.set("OriginalFilename", "SimpleImageViewer.exe");

    // Set String Versions (visible in Windows properties)
    res.set("FileVersion", &display_version);
    res.set("ProductVersion", &display_version);

    res.set("LegalCopyright", "\u{a9} 2026");
    res.set("Comments", "https://github.com/z16166/SimpleImageViewer/");

    if let Err(e) = res.compile() {
        eprintln!("build.rs: winresource error: {e}");
    }
}
