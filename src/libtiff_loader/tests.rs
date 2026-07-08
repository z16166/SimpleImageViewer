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

use super::*;
use crate::loader::ImageData;
use std::ffi::CStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use walkdir::WalkDir;

unsafe extern "C" fn tiff_error_handler(
    module: *const std::ffi::c_char,
    fmt: *const std::ffi::c_char,
    _ap: *mut std::ffi::c_void,
) {
    let module = if module.is_null() {
        "Unknown"
    } else {
        unsafe { CStr::from_ptr(module) }
            .to_str()
            .unwrap_or("Unknown")
    };
    let fmt = if fmt.is_null() {
        ""
    } else {
        unsafe { CStr::from_ptr(fmt) }.to_str().unwrap_or("")
    };
    println!("[TIFF Error] Module: {}, Message: {}", module, fmt);
}

unsafe extern "C" fn tiff_warning_handler(
    module: *const std::ffi::c_char,
    fmt: *const std::ffi::c_char,
    _ap: *mut std::ffi::c_void,
) {
    let module = if module.is_null() {
        "Unknown"
    } else {
        unsafe { CStr::from_ptr(module) }
            .to_str()
            .unwrap_or("Unknown")
    };
    let fmt = if fmt.is_null() {
        ""
    } else {
        unsafe { CStr::from_ptr(fmt) }.to_str().unwrap_or("")
    };
    println!("[TIFF Warning] Module: {}, Message: {}", module, fmt);
}

#[test]
fn tiff_scanline_size_validation() {
    assert_eq!(tiff_min_contig_scanline_bytes(100, 3, 8), Some(300));
    assert_eq!(tiff_min_contig_scanline_bytes(100, 1, 1), Some(13));
    assert_eq!(tiff_min_separate_scanline_bytes(100, 16), Some(200));

    assert!(ensure_tiff_scanline_size(300, 100, 3, 8, CONFIG_CONTIG, "test").is_ok());
    assert!(ensure_tiff_scanline_size(299, 100, 3, 8, CONFIG_CONTIG, "test").is_err());
    assert!(ensure_tiff_scanline_size(0, 100, 3, 8, CONFIG_CONTIG, "test").is_err());
    assert!(ensure_tiff_scanline_size(200, 100, 1, 16, CONFIG_SEPARATE, "test").is_ok());
    assert!(ensure_tiff_scanline_size(199, 100, 1, 16, CONFIG_SEPARATE, "test").is_err());
}

#[test]
fn uint16_rgb_scene_linear_detection_smoke() {
    assert!(tiff_uint16_rgb_scene_linear_eligible(
        lib::SAMPLEFORMAT_UINT,
        16,
        PHOTO_RGB,
        3,
        CONFIG_CONTIG
    ));
    assert!(!tiff_uint16_rgb_scene_linear_eligible(
        lib::SAMPLEFORMAT_UINT,
        8,
        PHOTO_RGB,
        3,
        CONFIG_CONTIG
    ));
    assert!(!tiff_uint16_rgb_scene_linear_eligible(
        lib::SAMPLEFORMAT_IEEEFP,
        16,
        PHOTO_RGB,
        3,
        CONFIG_CONTIG
    ));
}

#[test]
fn ieee_scene_linear_detection_smoke() {
    assert!(tiff_ieee_scene_linear_eligible(
        lib::SAMPLEFORMAT_IEEEFP,
        32,
        PHOTO_RGB,
        3
    ));
    assert!(!tiff_ieee_scene_linear_eligible(
        lib::SAMPLEFORMAT_UINT,
        32,
        PHOTO_RGB,
        3
    ));
    assert!(tiff_ieee_scene_linear_eligible(
        lib::SAMPLEFORMAT_IEEEFP,
        16,
        PHOTO_RGB,
        3
    ));
    assert!(!tiff_ieee_scene_linear_eligible(
        lib::SAMPLEFORMAT_UINT,
        16,
        PHOTO_RGB,
        3
    ));
}

/// Requires `tests/data/hdr_ieee_rgb_*bit.tif` from `scripts/generate_hdr_float_tiff_samples.py`.
#[test]
fn ieee_float_sample_assets_load_as_hdr() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for name in [
        "hdr_ieee_rgb_16bit.tif",
        "hdr_ieee_rgb_32bit.tif",
        "hdr_ieee_rgb_64bit.tif",
    ] {
        let path = root.join("tests").join("data").join(name);
        assert!(
            path.is_file(),
            "missing {} — run: python scripts/generate_hdr_float_tiff_samples.py",
            path.display()
        );
        let tone = crate::hdr::types::HdrToneMapSettings::default();
        let img = load_via_libtiff(&path, 1.0, tone).unwrap_or_else(|e| panic!("load {name}: {e}"));
        match &img {
            ImageData::Hdr { hdr, .. } => {
                assert_eq!(hdr.width, 64, "{name}");
                assert_eq!(hdr.height, 64, "{name}");
            }
            ImageData::Static(_) => panic!("{name}: expected ImageData::Hdr, got Static"),
            ImageData::Tiled(_) => panic!("{name}: expected ImageData::Hdr, got Tiled"),
            ImageData::Animated(_) => panic!("{name}: expected ImageData::Hdr, got Animated"),
            ImageData::HdrTiled { .. } => {
                panic!("{name}: expected ImageData::Hdr, got HdrTiled");
            }
            ImageData::HdrAnimated(_) => {
                panic!("{name}: expected ImageData::Hdr, got HdrAnimated");
            }
        }
    }
}

#[test]
fn heic_hubble_rgba_strip_read_smoke() {
    unsafe {
        lib::TIFFSetErrorHandler(Some(tiff_error_handler));
        lib::TIFFSetWarningHandler(Some(tiff_warning_handler));
    }

    for (name, width, height, strips) in [
        (
            "heic0601a.tif",
            18000u32,
            18000u32,
            [0u32, 198, 199, 201, 202] as [u32; 5],
        ),
        ("heic0604a.tif", 9500u32, 7400u32, [0u32, 0, 0, 0, 0]),
    ] {
        let path = PathBuf::from(r"F:\win7\top100").join(name);
        if !path.exists() {
            continue;
        }
        let mmap = Arc::new(crate::mmap_util::map_file(&path).expect("mmap"));
        let handle = super::handle::create_tiff_handle(mmap, &path).expect("open");
        let Some(strip_len) = (unsafe {
            super::rgba_buffer::tiff_rgba_strip_buffer_u32_count(handle.as_ptr(), width, height)
        }) else {
            panic!("{name}: strip buffer size unavailable");
        };
        let mut strip = vec![0u32; strip_len];
        let rps =
            unsafe { super::rgba_buffer::tiff_effective_rows_per_strip(handle.as_ptr(), height) };
        let strip_list: &[u32] = if name == "heic0604a.tif" {
            &[0]
        } else {
            &strips
        };
        for strip_idx in strip_list {
            let read_row = strip_idx * rps;
            let ok =
                unsafe { lib::TIFFReadRGBAStrip(handle.as_ptr(), read_row, strip.as_mut_ptr()) };
            assert_ne!(
                ok, 0,
                "{name}: TIFFReadRGBAStrip failed at strip {strip_idx} row {read_row}"
            );
        }
    }
}

#[test]
fn heic0601a_then_heic0604a_preview_smoke() {
    unsafe {
        lib::TIFFSetErrorHandler(Some(tiff_error_handler));
        lib::TIFFSetWarningHandler(Some(tiff_warning_handler));
    }

    for name in ["heic0601a.tif", "heic0604a.tif"] {
        let path = Path::new(r"F:\win7\top100").join(name);
        if !path.exists() {
            return;
        }
        let mmap = Arc::new(crate::mmap_util::map_file(&path).expect("mmap"));
        let image = load_via_libtiff_from_mmap(
            &path,
            mmap,
            1.0,
            crate::hdr::types::HdrToneMapSettings::default(),
        )
        .expect("load");
        let ImageData::Tiled(source) = image else {
            panic!("expected tiled");
        };
        let (pw, ph, pixels) = source.generate_preview(256, 256);
        assert!(pw > 0 && ph > 0, "{}", path.display());
        assert_eq!(pixels.len(), (pw as usize) * (ph as usize) * 4);
    }
}

#[test]
fn top100_strip_preview_parallel_smoke() {
    unsafe {
        lib::TIFFSetErrorHandler(Some(tiff_error_handler));
        lib::TIFFSetWarningHandler(Some(tiff_warning_handler));
    }

    let root = Path::new(r"F:\win7\top100");
    if !root.exists() {
        return;
    }

    let mut paths: Vec<PathBuf> = std::fs::read_dir(root)
        .expect("read_dir")
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();
            let ext = path.extension()?.to_str()?.to_ascii_lowercase();
            if ext == "tif" || ext == "tiff" {
                Some(path)
            } else {
                None
            }
        })
        .collect();
    paths.sort();

    use rayon::prelude::*;
    paths.par_iter().for_each(|path| {
        let mmap = Arc::new(crate::mmap_util::map_file(path).expect("mmap"));
        let image = load_via_libtiff_from_mmap(
            path,
            mmap,
            1.0,
            crate::hdr::types::HdrToneMapSettings::default(),
        )
        .expect("load");
        let ImageData::Tiled(source) = image else {
            return;
        };
        let (pw, ph, pixels) = source.generate_preview(256, 256);
        assert!(pw > 0 && ph > 0, "{} -> empty preview", path.display());
        assert_eq!(
            pixels.len(),
            (pw as usize) * (ph as usize) * 4,
            "{} -> preview bytes",
            path.display()
        );
    });
}

#[test]
fn tiff_stress_test() {
    unsafe {
        lib::TIFFSetErrorHandler(Some(tiff_error_handler));
        lib::TIFFSetWarningHandler(Some(tiff_warning_handler));
    }

    let root = Path::new(r"F:\win7\libtiffpic\");
    if !root.exists() {
        println!(
            "Root path {} does not exist, skipping stress test.",
            root.display()
        );
        return;
    }

    let mut total = 0;
    let mut failed = 0;

    for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_file() {
            let ext = path
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_lowercase();
            if ext == "tif" || ext == "tiff" {
                total += 1;
                match load_via_libtiff(path, 1.0, crate::hdr::types::HdrToneMapSettings::default())
                {
                    Ok(_) => {
                        // println!("OK: {}", path.display());
                    }
                    Err(e) => {
                        failed += 1;
                        println!("FAILED: {} - Reason: {}", path.display(), e);

                        // Debug tags
                        unsafe {
                            let c_path = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
                            let tif = lib::TIFFOpen(c_path.as_ptr(), c"r".as_ptr());
                            if !tif.is_null() {
                                let mut w: u32 = 0;
                                let mut h: u32 = 0;
                                let mut bps: u16 = 0;
                                let mut spp: u16 = 0;
                                let mut comp: u16 = 0;
                                let mut photo: u16 = 0;
                                lib::TIFFSetDirectory(tif, 0);
                                let r1 = lib::TIFFGetField(tif, lib::TIFFTAG_IMAGEWIDTH, &mut w);
                                let r2 = lib::TIFFGetField(tif, lib::TIFFTAG_IMAGELENGTH, &mut h);
                                let r3 =
                                    lib::TIFFGetField(tif, lib::TIFFTAG_BITSPERSAMPLE, &mut bps);
                                let r4 =
                                    lib::TIFFGetField(tif, lib::TIFFTAG_SAMPLESPERPIXEL, &mut spp);
                                let r5 =
                                    lib::TIFFGetField(tif, lib::TIFFTAG_COMPRESSION, &mut comp);
                                let r6 =
                                    lib::TIFFGetField(tif, lib::TIFFTAG_PHOTOMETRIC, &mut photo);
                                println!(
                                    "  TAGS: Res={}{}{}{}{}{}, Size={}x{}, BPS={}, SPP={}, Comp={}, Photo={}",
                                    r1, r2, r3, r4, r5, r6, w, h, bps, spp, comp, photo
                                );
                                lib::TIFFClose(tif);
                            }
                        }
                    }
                }
            }
        }
    }

    println!("Summary: Total: {}, Failed: {}", total, failed);
}
