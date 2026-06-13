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
use crate::hdr::types::HdrToneMapSettings;
use crate::loader::DecodedImage;
use crate::loader::ImageData;
use crate::loader::raw_osd::RawRenderPixels;
use std::path::PathBuf;

#[test]
fn nikon1_embedded_thumb_covers_sensor() {
    let thumb = DecodedImage::new(3872, 2592, vec![0; 3872 * 2592 * 4]);
    assert!(raw_embedded_preview_covers_sensor(&thumb, 3904, 2604));
    assert!(raw_embedded_preview_meets_hq_requirement(
        &thumb, 3904, 2604
    ));
}

#[test]
fn performance_mode_uses_small_embedded_on_hdr_display_capacity() {
    let thumb = DecodedImage::new(1616, 1080, vec![0; 1616 * 1080 * 4]);
    assert!(!raw_embedded_preview_covers_sensor(&thumb, 6000, 4000));
    assert!(!raw_embedded_preview_meets_hq_requirement(
        &thumb, 6000, 4000
    ));
    // Performance path returns embedded regardless of HDR capacity — verified by load_raw
    // integration; size helpers document the HQ vs performance distinction here.
}

#[test]
fn epson_small_embedded_thumb_does_not_satisfy_hq_requirement() {
    // Epson R-D1 style: 640×424 embedded JPEG vs ~2240×1680 developed output.
    let thumb = DecodedImage::new(640, 424, vec![0; 640 * 424 * 4]);
    assert!(!raw_embedded_preview_covers_sensor(&thumb, 2240, 1680));
    assert!(!raw_embedded_preview_meets_hq_requirement(
        &thumb, 2240, 1680
    ));
}

#[test]
fn epson_rd1_erf_embedded_does_not_skip_hq_demosaic_when_file_present() {
    let path = PathBuf::from(r"F:\win7\raws\epson\rd1\RAW_EPSON_RD1.ERF");
    if !path.is_file() {
        eprintln!("skip: {}", path.display());
        return;
    }

    let mut processor = RawProcessor::new().expect("libraw init");
    processor.open(&path).expect("libraw open");
    let thumb = processor
        .unpack_thumb()
        .expect("epson rd1 should ship an embedded thumb");
    let (iw, ih) = (processor.width(), processor.height());
    let (rw, rh) = (processor.raw_width(), processor.raw_height());
    let (out_w, out_h) = processor.developed_output_dimensions(Some(&thumb));

    eprintln!(
        "ERF thumb={}x{} iwidth/iheight={}x{} raw={}x{} developed_output={}x{} hq_side={}",
        thumb.width,
        thumb.height,
        iw,
        ih,
        rw,
        rh,
        out_w,
        out_h,
        hq_preview_max_side()
    );

    assert_eq!(
        (thumb.width, thumb.height),
        (640, 424),
        "unexpected embedded preview size for RD1 sample"
    );
    assert!(
        out_w > thumb.width && out_h > thumb.height,
        "developed output should exceed embedded thumb (got {out_w}x{out_h})"
    );
    assert!(
        !raw_embedded_preview_meets_hq_requirement(&thumb, out_w, out_h),
        "640×424 thumb must not satisfy HQ requirement for RD1"
    );
}

#[test]
fn epson_rd1_erf_hq_load_uses_tiled_bootstrap_when_file_present() {
    use crossbeam_channel::unbounded;

    let path = PathBuf::from(r"F:\win7\raws\epson\rd1\RAW_EPSON_RD1.ERF");
    if !path.is_file() {
        eprintln!("skip: {}", path.display());
        return;
    }

    let (refine_tx, refine_rx) = unbounded();
    let result = load_raw(
        0,
        0,
        &path,
        refine_tx,
        true,
        crate::settings::RawDemosaicMode::Cpu,
        4.0,
        HdrToneMapSettings::default(),
    )
    .expect("load_raw hq");

    match result.image {
        ImageData::HdrTiled { fallback, .. } => {
            assert_eq!(fallback.width(), 3040);
            assert_eq!(fallback.height(), 2024);
            eprintln!(
                "HQ load: tiled bootstrap {}x{}",
                fallback.width(),
                fallback.height()
            );
        }
        other => panic!(
            "expected HdrTiled HQ bootstrap, got {:?}",
            std::mem::discriminant(&other)
        ),
    }
    assert!(
        refine_rx.try_recv().is_err(),
        "refinement is deferred until the image becomes active"
    );
}

#[test]
fn epson_rd1_erf_performance_load_uses_embedded_static_when_file_present() {
    use crossbeam_channel::unbounded;

    let path = PathBuf::from(r"F:\win7\raws\epson\rd1\RAW_EPSON_RD1.ERF");
    if !path.is_file() {
        eprintln!("skip: {}", path.display());
        return;
    }

    let (refine_tx, _refine_rx) = unbounded();
    let result = load_raw(
        0,
        0,
        &path,
        refine_tx,
        false,
        crate::settings::RawDemosaicMode::Cpu,
        4.0,
        HdrToneMapSettings::default(),
    )
    .expect("load_raw perf");

    match result.image {
        ImageData::Static(img) => {
            assert_eq!(img.width, 640);
            assert_eq!(img.height, 424);
            eprintln!("Perf load: static embedded {}x{}", img.width, img.height);
        }
        other => panic!(
            "expected static embedded in performance mode, got {:?}",
            std::mem::discriminant(&other)
        ),
    }
}

#[test]
fn canon_10d_hq_load_keeps_hdr_plane_on_sdr_tone_map_when_file_present() {
    use crossbeam_channel::unbounded;

    let path = PathBuf::from(r"F:\win7\raws\canon\10d\RAW_CANON_10D.CRW");
    if !path.is_file() {
        eprintln!("skip: {}", path.display());
        return;
    }

    let (refine_tx, _refine_rx) = unbounded();
    let result = load_raw(
        0,
        0,
        &path,
        refine_tx,
        true,
        crate::settings::RawDemosaicMode::Cpu,
        1.0,
        HdrToneMapSettings::default(),
    )
    .expect("load_raw hq sdr tone map");

    match result.image {
        ImageData::Hdr { hdr, fallback } => {
            assert_eq!((hdr.width, hdr.height), (2056, 3088));
            assert_eq!((fallback.width, fallback.height), (2056, 3088));
            assert_eq!(
                result.osd.render_pixels,
                RawRenderPixels::FullDevelop {
                    width: 2056,
                    height: 3088
                }
            );
        }
        other => panic!(
            "expected HDR plane for SDR tone-map RAW HQ load, got {:?}",
            std::mem::discriminant(&other)
        ),
    }
}

#[test]
fn probe_epson_and_fuji_on_local_samples() {
    // Simulate 5120×1440 HDR monitor HQ cap (physical long edge × 1.1, capped at 4096).
    crate::loader::MONITOR_PREVIEW_CAP.store(4096, std::sync::atomic::Ordering::Relaxed);

    for (path, name) in [
        (
            PathBuf::from(r"F:\win7\raws\epson\rd1\RAW_EPSON_RD1.ERF"),
            "epson_rd1",
        ),
        (
            PathBuf::from(r"F:\win7\raws\fuji\RAW_FUIJI_X-E2.RAF"),
            "fuji_xe2",
        ),
        (
            PathBuf::from(r"F:\win7\raws\canon\RAW_CANON_S90.CR2"),
            "canon_s90",
        ),
    ] {
        if !path.is_file() {
            eprintln!("skip {name}: {}", path.display());
            continue;
        }

        let mut processor = RawProcessor::new().expect("libraw init");
        processor.open(&path).expect("open");
        let thumb = processor.unpack_thumb().ok();
        let (out_w, out_h) = processor.developed_output_dimensions(thumb.as_ref());

        eprintln!("\n=== {name} ===");
        eprintln!(
            "i={}x{} raw={}x{} out={}x{} hq_side={}",
            processor.width(),
            processor.height(),
            processor.raw_width(),
            processor.raw_height(),
            out_w,
            out_h,
            hq_preview_max_side()
        );
        if let Some(ref t) = thumb {
            eprintln!(
                "thumb={}x{} covers={} meets_hq={}",
                t.width,
                t.height,
                raw_embedded_preview_covers_sensor(t, out_w, out_h),
                raw_embedded_preview_meets_hq_requirement(t, out_w, out_h)
            );
        }

        for (label, hq) in [("performance", false), ("high_quality", true)] {
            let (refine_tx, _rx) = crossbeam_channel::unbounded();
            let result = load_raw(
                0,
                0,
                &path,
                refine_tx,
                hq,
                crate::settings::RawDemosaicMode::Cpu,
                4.0,
                HdrToneMapSettings::default(),
            )
            .expect("load_raw");
            match result.image {
                ImageData::Static(img) => {
                    eprintln!("{name} {label}: Static {}x{}", img.width, img.height);
                }
                ImageData::Tiled(src) => {
                    eprintln!(
                        "{name} {label}: Tiled logical {}x{}",
                        src.width(),
                        src.height()
                    );
                }
                ImageData::HdrTiled { fallback, .. } => {
                    eprintln!(
                        "{name} {label}: HdrTiled logical {}x{}",
                        fallback.width(),
                        fallback.height()
                    );
                }
                ImageData::Hdr { fallback, hdr } => {
                    eprintln!(
                        "{name} {label}: Hdr {}x{} fallback {}x{}",
                        hdr.width, hdr.height, fallback.width, fallback.height
                    );
                }
                _ => eprintln!("{name} {label}: other"),
            }
        }
    }
}

#[test]
fn canon_s90_hq_load_routes_hdr_tiled_on_hdr_display_when_file_present() {
    use crossbeam_channel::unbounded;

    let path = PathBuf::from(r"F:\win7\raws\canon\RAW_CANON_S90.CR2");
    if !path.is_file() {
        eprintln!("skip: {}", path.display());
        return;
    }

    let (refine_tx, _refine_rx) = unbounded();
    let result = load_raw(
        0,
        0,
        &path,
        refine_tx,
        true,
        crate::settings::RawDemosaicMode::Cpu,
        4.0,
        HdrToneMapSettings::default(),
    )
    .expect("load_raw hq hdr");

    match result.image {
        ImageData::HdrTiled { hdr, fallback } => {
            assert_eq!(fallback.width(), 3684);
            assert_eq!(fallback.height(), 2760);
            assert_eq!(hdr.width(), 3684);
            assert_eq!(hdr.height(), 2760);
            eprintln!(
                "Canon S90 HQ HDR bootstrap: logical {}x{}",
                fallback.width(),
                fallback.height()
            );
        }
        other => panic!(
            "expected HdrTiled on HDR display for Canon S90 HQ, got {:?}",
            std::mem::discriminant(&other)
        ),
    }
}

#[test]
fn finalize_hq_develop_keeps_canon_s90_full_resolution() {
    use crate::loader::preview_caps::finalize_raw_hq_developed_image;
    use image::{DynamicImage, GenericImageView, RgbaImage};

    crate::loader::MONITOR_PREVIEW_CAP.store(2048, std::sync::atomic::Ordering::Relaxed);
    let rgba = RgbaImage::from_pixel(3684, 2760, image::Rgba([128, 128, 128, 255]));
    let img = DynamicImage::ImageRgba8(rgba);
    let result = finalize_raw_hq_developed_image(img, 3684, 2760);
    assert_eq!(result.dimensions(), (3684, 2760));
}

#[test]
fn finalize_hq_develop_keeps_large_sensor_despite_monitor_cap() {
    use crate::loader::preview_caps::finalize_raw_hq_developed_image;
    use image::{DynamicImage, GenericImageView, RgbaImage};

    crate::loader::MONITOR_PREVIEW_CAP.store(2048, std::sync::atomic::Ordering::Relaxed);
    let rgba = RgbaImage::from_pixel(5536, 3692, image::Rgba([128, 128, 128, 255]));
    let img = DynamicImage::ImageRgba8(rgba);
    let result = finalize_raw_hq_developed_image(img, 5536, 3692);
    assert_eq!(result.dimensions(), (5536, 3692));
}

#[test]
fn canon_s90_cr2_develop_dimensions_when_file_present() {
    let path = PathBuf::from(r"F:\win7\raws\canon\RAW_CANON_S90.CR2");
    if !path.is_file() {
        eprintln!("skip: {}", path.display());
        return;
    }

    let mut processor = RawProcessor::new().expect("libraw init");
    processor.open(&path).expect("open");
    let thumb = processor.unpack_thumb().expect("thumb");
    let (out_w, out_h) = processor.developed_output_dimensions(Some(&thumb));
    let sdr = processor.develop().expect("develop");
    let finalized = crate::loader::preview_caps::finalize_raw_hq_developed_image(sdr, out_w, out_h);
    let finalized_rgba = finalized.to_rgba8();

    eprintln!(
        "canon_s90 thumb={}x{} logical={}x{} finalized={}x{}",
        thumb.width,
        thumb.height,
        out_w,
        out_h,
        finalized_rgba.width(),
        finalized_rgba.height()
    );
    assert_eq!((out_w, out_h), (3684, 2760), "unexpected logical output");
    assert_eq!(
        (finalized_rgba.width(), finalized_rgba.height()),
        (3684, 2760),
        "HQ refine must keep full develop resolution for Canon S90"
    );
}

#[test]
fn raw_embedded_preview_covers_sensor_requires_near_full_resolution() {
    let misleading_wic = DecodedImage::new(4096, 3067, vec![0; 4096 * 3067 * 4]);
    assert!(!raw_embedded_preview_covers_sensor(
        &misleading_wic,
        4992,
        6666
    ));
    let full = DecodedImage::new(4992, 6666, vec![0; 4992 * 6666 * 4]);
    assert!(raw_embedded_preview_covers_sensor(&full, 4992, 6666));
}
