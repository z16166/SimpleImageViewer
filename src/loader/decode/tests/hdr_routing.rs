//! HDR image construction and Radiance `.hdr` routing.

use std::sync::Arc;

use crate::hdr::types::{
    HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat, HdrToneMapSettings,
};
use crate::loader::{DecodedImage, ImageData};
use crate::loader::decode::assemble::make_hdr_image_data_for_limit;
use crate::loader::decode::hdr_formats::load_hdr;

use super::support::{lock_tiled_threshold_for_test, TiledThresholdOverride};

#[test]
fn supported_hdr_image_data_keeps_float_buffer_with_sdr_fallback() {
    let _threshold_lock = lock_tiled_threshold_for_test();
    let _threshold_override = TiledThresholdOverride::set(u64::MAX);
    let hdr = HdrImageBuffer {
        width: 2,
        height: 1,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::LinearSrgb,
        metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
        rgba_f32: Arc::new(vec![1.0; 2 * 4]),
    };
    let fallback = DecodedImage::new(2, 1, vec![255; 2 * 4]);

    let image_data = make_hdr_image_data_for_limit(hdr.clone(), fallback, 4096);

    match image_data {
        ImageData::Hdr {
            hdr: kept,
            fallback,
        } => {
            assert_eq!(kept.width, hdr.width);
            assert_eq!(kept.height, hdr.height);
            assert!(Arc::ptr_eq(&kept.rgba_f32, &hdr.rgba_f32));
            assert_eq!(fallback.width, hdr.width);
            assert_eq!(fallback.height, hdr.height);
        }
        _ => panic!("expected HDR image data"),
    }
}

#[test]
fn oversized_hdr_uses_existing_sdr_fallback_routing() {
    let hdr = HdrImageBuffer {
        width: 4097,
        height: 1,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::LinearSrgb,
        metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
        rgba_f32: Arc::new(vec![1.0; 4097 * 4]),
    };
    let fallback = DecodedImage::new(4097, 1, vec![255; 4097 * 4]);

    let image_data = make_hdr_image_data_for_limit(hdr, fallback, 4096);

    assert!(matches!(image_data, ImageData::HdrTiled { .. }));
}

#[test]
fn load_hdr_routes_threshold_sized_images_to_tiled_fallback() {
    let _threshold_lock = lock_tiled_threshold_for_test();
    let path = std::env::temp_dir().join(format!(
        "simple_image_viewer_loader_hdr_route_{}.hdr",
        std::process::id()
    ));
    let bytes = b"#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n-Y 1 +X 1\n\x80\x80\x80\x81";
    std::fs::write(&path, bytes).expect("write test HDR");
    let _threshold_override = TiledThresholdOverride::set(1);

    let image_data = load_hdr(&path, 1.0, HdrToneMapSettings::default()).expect("load tiny HDR");

    let ImageData::HdrTiled { hdr, fallback } = image_data else {
        panic!("expected Radiance HDR to route to HDR tiled image data");
    };
    assert_eq!(
        hdr.source_kind(),
        crate::hdr::tiled::HdrTiledSourceKind::DiskBacked
    );
    assert!(fallback.is_hdr_sdr_fallback());
    let tile = hdr
        .extract_tile_rgba32f_arc(0, 0, 1, 1)
        .expect("extract Radiance HDR tile");
    assert_eq!((tile.width, tile.height), (1, 1));
    assert_eq!(tile.color_space, HdrColorSpace::LinearSrgb);
    assert_eq!(tile.rgba_f32.len(), 4);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn load_radiance_hdr_routes_small_images_to_float_image_data() {
    let _threshold_lock = lock_tiled_threshold_for_test();
    let path = std::env::temp_dir().join(format!(
        "simple_image_viewer_loader_hdr_static_route_{}.hdr",
        std::process::id()
    ));
    let bytes = b"#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n-Y 1 +X 1\n\x80\x80\x80\x81";
    std::fs::write(&path, bytes).expect("write test HDR");
    let _threshold_override = TiledThresholdOverride::set(u64::MAX);

    let image_data =
        load_hdr(&path, 1.0, HdrToneMapSettings::default()).expect("load tiny Radiance HDR");

    let ImageData::Hdr { hdr, fallback } = image_data else {
        panic!("expected small Radiance HDR to route to static HDR image data");
    };
    assert_eq!((hdr.width, hdr.height), (1, 1));
    assert_eq!((fallback.width, fallback.height), (1, 1));
    assert_eq!(hdr.color_space, HdrColorSpace::LinearSrgb);
    assert_eq!(hdr.rgba_f32.len(), 4);
    assert!(
        hdr.rgba_f32.iter().any(|value| *value > 0.0),
        "Radiance HDR float buffer should contain visible samples"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn oversized_hdr_tiled_fallback_remembers_hdr_source() {
    let hdr = HdrImageBuffer {
        width: 4097,
        height: 1,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::LinearSrgb,
        metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
        rgba_f32: Arc::new(vec![1.0; 4097 * 4]),
    };
    let fallback = DecodedImage::new(4097, 1, vec![255; 4097 * 4]);

    let image_data = make_hdr_image_data_for_limit(hdr, fallback, 4096);

    let ImageData::HdrTiled { hdr, fallback } = image_data else {
        panic!("expected HDR tiled image data");
    };
    assert_eq!(hdr.width(), 4097);
    assert_eq!(hdr.height(), 1);
    assert!(fallback.is_hdr_sdr_fallback());
}
