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

use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=locales");
    println!(
        "cargo:rerun-if-changed={}",
        manifest_dir.join("assets/icon.png").display()
    );

    // Generate the ICO from the source image (PNG)
    let src = manifest_dir.join("assets/icon.png");
    let dst = manifest_dir.join("assets/icon.ico");

    if src.exists() {
        match png_to_ico(&src, &dst) {
            Ok(_) => println!("cargo:warning=icon.ico generated from icon.png"),
            Err(e) => eprintln!("build.rs: icon conversion failed: {e}"),
        }
    } else {
        eprintln!("build.rs: assets/icon.png not found, skipping ICO generation");
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
    }
}

/// Convert a PNG to a multi-resolution ICO (16, 32, 48, 64, 128, 256 px).
fn png_to_ico(
    src: &std::path::Path,
    dst: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    use image::ImageFormat;
    use image::imageops::FilterType;
    use std::fs::File;
    use std::io::{BufWriter, Write};

    let src_img = image::open(src)?;
    let sizes: &[u32] = &[16, 32, 48, 64, 128, 256];

    // Encode each size as a PNG blob (PNG-in-ICO, supported Windows Vista+)
    let mut blobs: Vec<Vec<u8>> = Vec::with_capacity(sizes.len());
    for &sz in sizes {
        let scaled = src_img.resize_exact(sz, sz, FilterType::Lanczos3);
        let mut buf = Vec::new();
        scaled.write_to(&mut std::io::Cursor::new(&mut buf), ImageFormat::Png)?;
        blobs.push(buf);
    }

    let count = sizes.len() as u16;
    let header_sz = 6u32;
    let entry_sz = 16u32 * count as u32;
    let mut data_offset = header_sz + entry_sz;

    let mut offsets: Vec<u32> = Vec::with_capacity(blobs.len());
    for blob in &blobs {
        offsets.push(data_offset);
        data_offset += blob.len() as u32;
    }

    let mut out = BufWriter::new(File::create(dst)?);

    // ICONDIR
    out.write_all(&0u16.to_le_bytes())?; // reserved
    out.write_all(&1u16.to_le_bytes())?; // type = ICON
    out.write_all(&count.to_le_bytes())?;

    // ICONDIRENTRY array
    for (i, &sz) in sizes.iter().enumerate() {
        let dim = if sz >= 256 { 0u8 } else { sz as u8 };
        out.write_all(&[dim, dim, 0u8, 0u8])?;
        out.write_all(&1u16.to_le_bytes())?;
        out.write_all(&32u16.to_le_bytes())?;
        out.write_all(&(blobs[i].len() as u32).to_le_bytes())?;
        out.write_all(&offsets[i].to_le_bytes())?;
    }

    for blob in &blobs {
        out.write_all(blob)?;
    }

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
