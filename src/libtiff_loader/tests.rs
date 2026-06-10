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
                            let tif = lib::TIFFOpen(
                                c_path.as_ptr(),
                                b"r\0".as_ptr() as *const std::ffi::c_char,
                            );
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
