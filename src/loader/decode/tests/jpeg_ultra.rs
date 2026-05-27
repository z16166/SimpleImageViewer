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

//! JPEG EXIF transpose regression and JPEG_R / Ultra HDR corpus routes.

use std::path::PathBuf;

use crate::hdr::gain_map::gain_map_weight;
use crate::hdr::jpeg_gain_map_gpu::iso_deferred_from_metadata;
use crate::hdr::types::{HdrImageBuffer, HdrToneMapSettings};
use crate::hdr::ultra_hdr::display_to_physical_pixel;
use crate::hdr::ultra_hdr_compose::compose_ultra_hdr_tile_region_cpu;
use crate::loader::ImageData;
use crate::loader::decode::jpeg::{load_jpeg, load_jpeg_with_target_capacity};
use crate::loader::decode::load_image_file;

use super::support::{TiledThresholdOverride, lock_tiled_threshold_for_test};

fn assert_ultra_hdr_gpu_deferred_route(hdr: &HdrImageBuffer) {
    if let Some(err) = ultra_hdr_gpu_deferred_route_error(hdr) {
        panic!("{err}");
    }
}

fn ultra_hdr_gpu_deferred_route_error(hdr: &HdrImageBuffer) -> Option<String> {
    if !hdr.rgba_f32.is_empty() {
        return Some("rgba_f32 should be empty for GPU-deferred JPEG_R".to_string());
    }
    let gain_map = hdr.metadata.gain_map.as_ref()?;
    let deferred = gain_map.iso_deferred.as_ref()?;
    if deferred.sdr_rgba.len() != hdr.width as usize * hdr.height as usize * 4 {
        return Some("baseline SDR plane size mismatch".to_string());
    }
    if deferred.gain_width == 0 || deferred.gain_height == 0 {
        return Some("gain map plane missing".to_string());
    }
    None
}

fn assert_ultra_hdr_hdr_base_route(hdr: &HdrImageBuffer) {
    assert!(
        !hdr.rgba_f32.is_empty(),
        "HDR base JPEG_R should expose eager linear primary"
    );
    assert!(
        iso_deferred_from_metadata(&hdr.metadata).is_none(),
        "HDR base JPEG_R must not use iso_deferred GPU compose"
    );
    assert!(
        hdr.metadata.gain_map.is_some(),
        "HDR base JPEG_R should retain gain-map diagnostic metadata"
    );
}

#[test]
fn gain_map_corpora_samples_load_as_static_hdr() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/gain_map_samples");
    for name in [
        "sample_iso_backward.jpg",
        "sample_base_rendition_is_hdr.jpg",
    ] {
        let path = dir.join(name);
        if !path.is_file() {
            eprintln!("skip: {} missing", path.display());
            continue;
        }
        let info = crate::hdr::ultra_hdr::inspect_ultra_hdr_jpeg_bytes(
            &std::fs::read(&path).expect("read sample"),
        )
        .expect("inspect gain map sample");
        assert!(
            info.is_ultra_hdr,
            "{name} must remain a GContainer Ultra HDR JPEG"
        );

        let image_data = load_jpeg(&path).unwrap_or_else(|err| {
            panic!("load {name} as Ultra HDR JPEG_R: {err}");
        });
        let ImageData::Hdr { hdr, fallback } = image_data else {
            panic!("{name} should load as ImageData::Hdr, not baseline SDR");
        };
        assert_eq!((hdr.width, hdr.height), (4080, 3072));
        assert_eq!((fallback.width, fallback.height), (4080, 3072));
        assert_ultra_hdr_hdr_base_route(&hdr);
    }
}

#[test]
fn paris_exif_orientation_5_jpeg_loads_transposed_dimensions() {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/paris_exif_orientation_5.jpg");
    if !path.is_file() {
        eprintln!("skip: tests/data/paris_exif_orientation_5.jpg missing");
        return;
    }
    assert_eq!(crate::metadata_utils::get_exif_orientation(&path), 5);
    let image_data = load_jpeg_with_target_capacity(
        &path,
        HdrToneMapSettings::default().target_hdr_capacity(),
        HdrToneMapSettings::default(),
    )
    .expect("load paris EXIF orientation 5 JPEG");
    let ImageData::Static(decoded) = image_data else {
        panic!("expected static image data for paris_exif_orientation_5.jpg");
    };
    assert_eq!(
        (decoded.width, decoded.height),
        (302, 403),
        "EXIF 5 should transpose 403×302 stored raster to 302×403 display"
    );
}

#[test]
fn ultra_hdr_jpeg_sample_loads_as_hdr_image_data() {
    let _threshold_lock = lock_tiled_threshold_for_test();
    let root = std::env::var_os("SIV_ULTRA_HDR_SAMPLES_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"F:\HDR\Ultra_HDR_Samples"));
    let path = root
        .join("Originals")
        .join("Ultra_HDR_Samples_Originals_01.jpg");
    if !path.is_file() {
        eprintln!("skipping Ultra HDR loader test; sample missing");
        return;
    }

    let image_data = load_jpeg(&path).expect("load Ultra HDR JPEG_R sample");

    let ImageData::Hdr { hdr, fallback } = image_data else {
        panic!("expected Ultra HDR JPEG_R to load as HDR image data");
    };
    assert_eq!((hdr.width, hdr.height), (4080, 3072));
    assert_eq!((fallback.width, fallback.height), (4080, 3072));
    assert_ultra_hdr_gpu_deferred_route(&hdr);
    assert!(
        fallback
            .rgba()
            .chunks_exact(4)
            .any(|px| px[0] > 0 || px[1] > 0 || px[2] > 0),
        "Ultra HDR loader should expose baseline SDR fallback pixels"
    );
}

#[test]
fn ultra_hdr_loader_uses_target_hdr_capacity() {
    let _threshold_lock = lock_tiled_threshold_for_test();
    let root = std::env::var_os("SIV_ULTRA_HDR_SAMPLES_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"F:\HDR\Ultra_HDR_Samples"));
    let path = root
        .join("Originals")
        .join("Ultra_HDR_Samples_Originals_01.jpg");
    if !path.is_file() {
        eprintln!("skipping Ultra HDR loader target capacity test; sample missing");
        return;
    }

    let low = load_jpeg_with_target_capacity(&path, 1.0, HdrToneMapSettings::default())
        .expect("load low-capacity Ultra HDR JPEG_R sample");
    // `hdr_gain_map_decode_capacity` clamps to `HdrToneMapSettings::target_hdr_capacity()`;
    // raise the configured peak so an 8× probe survives the min() and exercises strong gain.
    let high_tone = HdrToneMapSettings {
        max_display_nits: HdrToneMapSettings::default().sdr_white_nits * 8.0,
        ..HdrToneMapSettings::default()
    };
    let high = load_jpeg_with_target_capacity(&path, 8.0, high_tone)
        .expect("load high-capacity Ultra HDR JPEG_R sample");

    let ImageData::Hdr { hdr: low, .. } = low else {
        panic!("expected low-capacity Ultra HDR JPEG_R to load as HDR image data");
    };
    let ImageData::Hdr { hdr: high, .. } = high else {
        panic!("expected high-capacity Ultra HDR JPEG_R to load as HDR image data");
    };

    assert_ultra_hdr_gpu_deferred_route(&low);
    assert_ultra_hdr_gpu_deferred_route(&high);

    let low_meta = low
        .metadata
        .gain_map
        .as_ref()
        .and_then(|gm| gm.iso_deferred.as_ref())
        .expect("low-capacity deferred metadata")
        .metadata;
    let high_meta = high
        .metadata
        .gain_map
        .as_ref()
        .and_then(|gm| gm.iso_deferred.as_ref())
        .expect("high-capacity deferred metadata")
        .metadata;

    assert!(
        gain_map_weight(high_meta, 8.0) > gain_map_weight(low_meta, 1.0),
        "loader should pass target HDR capacity into JPEG_R gain-map recovery"
    );
}

#[test]
fn ultra_hdr_load_result_is_capacity_sensitive() {
    let _threshold_lock = lock_tiled_threshold_for_test();
    let root = std::env::var_os("SIV_ULTRA_HDR_SAMPLES_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"F:\HDR\Ultra_HDR_Samples"));
    let path = root
        .join("Originals")
        .join("Ultra_HDR_Samples_Originals_01.jpg");
    if !path.is_file() {
        eprintln!("skipping Ultra HDR load result marker test; sample missing");
        return;
    }

    let (tx, _rx) = crossbeam_channel::unbounded();
    let (refine_tx, _refine_rx) = crossbeam_channel::unbounded();
    let result = load_image_file(
        1,
        7,
        &path,
        tx,
        refine_tx,
        false,
        HdrToneMapSettings::default().target_hdr_capacity(),
        HdrToneMapSettings::default(),
    );

    assert!(
        result.ultra_hdr_capacity_sensitive,
        "JPEG_R load results should be marked for capacity-based invalidation"
    );
}

#[test]
fn ultra_hdr_original_corpus_loads_as_hdr_image_data() {
    let _threshold_lock = lock_tiled_threshold_for_test();
    let root = std::env::var_os("SIV_ULTRA_HDR_SAMPLES_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"F:\HDR\Ultra_HDR_Samples"));
    let originals = root.join("Originals");
    if !originals.is_dir() {
        eprintln!("skipping Ultra HDR corpus loader test; Originals directory missing");
        return;
    }

    let failures = (1..=10)
        .filter_map(|index| {
            let path = originals.join(format!("Ultra_HDR_Samples_Originals_{index:02}.jpg"));
            if !path.is_file() {
                return Some(format!("{}: missing", path.display()));
            }

            match load_jpeg(&path) {
                Ok(ImageData::Hdr { hdr, fallback }) => {
                    if hdr.width == 0
                        || hdr.height == 0
                        || fallback.width != hdr.width
                        || fallback.height != hdr.height
                    {
                        Some(format!("{}: invalid HDR output", path.display()))
                    } else if let Some(err) = ultra_hdr_gpu_deferred_route_error(&hdr) {
                        Some(format!("{}: {err}", path.display()))
                    } else {
                        None
                    }
                }
                Ok(_) => Some(format!("{}: loaded as non-HDR image data", path.display())),
                Err(err) => Some(format!("{}: {err}", path.display())),
            }
        })
        .collect::<Vec<_>>();

    assert!(
        failures.is_empty(),
        "Ultra HDR corpus failures:\n{}",
        failures.join("\n")
    );
}

#[test]
fn ultra_hdr_threshold_sized_jpeg_routes_to_file_backed_hdr_tiles() {
    let _threshold_lock = lock_tiled_threshold_for_test();
    let root = std::env::var_os("SIV_ULTRA_HDR_SAMPLES_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"F:\HDR\Ultra_HDR_Samples"));
    let path = root
        .join("Originals")
        .join("Ultra_HDR_Samples_Originals_01.jpg");
    if !path.is_file() {
        eprintln!("skipping Ultra HDR tiled loader test; sample missing");
        return;
    }
    let _threshold_override = TiledThresholdOverride::set(1);

    let image_data = load_jpeg(&path).expect("load Ultra HDR JPEG_R sample as tiled HDR");

    let ImageData::HdrTiled { hdr, fallback } = image_data else {
        panic!("expected Ultra HDR JPEG_R to route to HDR tiled image data");
    };
    assert_eq!(
        hdr.source_kind(),
        crate::hdr::tiled::HdrTiledSourceKind::DiskBacked
    );
    assert!(fallback.is_hdr_sdr_fallback());
    let tile = hdr
        .extract_tile_rgba32f_arc(0, 0, 64, 64)
        .expect("extract Ultra HDR tile");
    assert_eq!((tile.width, tile.height), (64, 64));
    assert!(
        tile.rgba_f32.is_empty(),
        "Ultra HDR tiled source should defer compose to GPU"
    );
    let deferred = iso_deferred_from_metadata(&tile.metadata).expect("iso deferred metadata");
    let ctx = tile.iso_deferred_tile.expect("iso deferred tile context");
    let composed = compose_ultra_hdr_tile_region_cpu(
        tile.width,
        tile.height,
        ctx.origin_x,
        ctx.origin_y,
        ctx.physical_width,
        ctx.physical_height,
        ctx.orientation,
        deferred.sdr_rgba.as_slice(),
        deferred.gain_rgba.as_slice(),
        deferred.gain_width,
        deferred.gain_height,
        deferred.metadata,
        8.0,
        display_to_physical_pixel,
    );
    assert!(
        composed
            .chunks_exact(4)
            .any(|pixel| pixel[0] > 1.0 || pixel[1] > 1.0 || pixel[2] > 1.0),
        "Ultra HDR tiled source should preserve HDR highlights"
    );
}

#[test]
fn generated_8k_gcontainer_routes_to_hdr_tiled_when_present() {
    let path = std::env::var_os("SIV_GENERATED_ULTRA_HDR_8K")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"f:\hdr\ultra_hdr_8192.jpg"));
    if !path.is_file() {
        eprintln!(
            "skipping generated 8K GContainer test; set SIV_GENERATED_ULTRA_HDR_8K or create {}",
            path.display()
        );
        return;
    }

    let bytes = std::fs::read(&path).expect("read generated Ultra HDR JPEG");
    let info = crate::hdr::ultra_hdr::inspect_ultra_hdr_jpeg_bytes(&bytes).expect("inspect");
    assert!(
        info.is_ultra_hdr,
        "generated sample must pass inspect_ultra_hdr_jpeg_bytes"
    );

    let image_data = load_jpeg(&path).expect("load generated 8K Ultra HDR JPEG");
    let ImageData::HdrTiled { hdr, fallback } = image_data else {
        panic!("generated 8K Ultra HDR JPEG should route to ImageData::HdrTiled for HDR swapchain");
    };
    assert!(fallback.is_hdr_sdr_fallback());
    assert!(hdr.width() >= 8192, "expected upscaled long edge >= 8192");
    let tile = hdr
        .extract_tile_rgba32f_arc(0, 0, 64, 64)
        .expect("extract deferred tile");
    assert!(
        iso_deferred_from_metadata(&tile.metadata).is_some(),
        "tiled Ultra HDR JPEG_R should expose iso_deferred metadata"
    );
}
