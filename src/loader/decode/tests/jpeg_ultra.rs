//! JPEG EXIF transpose regression and JPEG_R / Ultra HDR corpus routes.

use std::path::PathBuf;

use crate::hdr::types::HdrToneMapSettings;
use crate::loader::ImageData;
use crate::loader::decode::jpeg::{load_jpeg, load_jpeg_with_target_capacity};
use crate::loader::decode::load_image_file;

use super::support::{lock_tiled_threshold_for_test, TiledThresholdOverride};

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
    assert!(
        hdr.rgba_f32
            .chunks_exact(4)
            .any(|pixel| pixel[0] > 1.0 || pixel[1] > 1.0 || pixel[2] > 1.0),
        "Ultra HDR loader should preserve HDR highlights"
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

    let low_peak = low
        .rgba_f32
        .chunks_exact(4)
        .map(|pixel| pixel[0].max(pixel[1]).max(pixel[2]))
        .fold(0.0_f32, f32::max);
    let high_peak = high
        .rgba_f32
        .chunks_exact(4)
        .map(|pixel| pixel[0].max(pixel[1]).max(pixel[2]))
        .fold(0.0_f32, f32::max);

    assert!(
        high_peak > low_peak,
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
                    let has_hdr_highlight = hdr
                        .rgba_f32
                        .chunks_exact(4)
                        .any(|pixel| pixel[0] > 1.0 || pixel[1] > 1.0 || pixel[2] > 1.0);
                    if hdr.width == 0
                        || hdr.height == 0
                        || fallback.width != hdr.width
                        || fallback.height != hdr.height
                        || !has_hdr_highlight
                    {
                        Some(format!("{}: invalid HDR output", path.display()))
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
        tile.rgba_f32
            .chunks_exact(4)
            .any(|pixel| pixel[0] > 1.0 || pixel[1] > 1.0 || pixel[2] > 1.0),
        "Ultra HDR tiled source should preserve HDR highlights"
    );
}
