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
use std::path::{Path, PathBuf};

fn ultra_hdr_samples_root() -> Option<PathBuf> {
    std::env::var_os("SIV_ULTRA_HDR_SAMPLES_DIR")
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from(r"F:\HDR\Ultra_HDR_Samples")))
        .filter(|path| path.is_dir())
}

fn sample_path(root: &Path, relative: &str) -> PathBuf {
    relative
        .split('/')
        .fold(root.to_path_buf(), |path, segment| path.join(segment))
}

fn gain_map_samples_root() -> Option<PathBuf> {
    std::env::var_os("SIV_GAIN_MAP_SAMPLES_DIR")
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from(r"F:\HDR\GainMap")))
        .filter(|path| path.is_dir())
}

#[test]
fn gain_map_corpus_mpf_jpegs_are_detected_as_ultra_hdr() {
    let Some(root) = gain_map_samples_root() else {
        eprintln!("skipping gain map corpus test; set SIV_GAIN_MAP_SAMPLES_DIR");
        return;
    };

    let samples = [
        "7007688-Edit-2_1000x667_100_3x2__benz8GainMap.jpg",
        "DSC0538-Edit_1000x667_100_3x2_benz10GainMap.jpg",
        "DSC0656-Edit_1000x667_100_3x2__benz8GainMap.jpg",
        "DSC0796-Edit_1000x667_100_3x2_benz10GainMap.jpg",
        "DSC2306-Edit_1000x667_100_3x2__benz8GainMap.jpg",
        "DSC3827-Panorama-final_1000x667_100_3x2__benz8GainMap.jpg",
        "DSC4743-Edit_1000x667_100_3x2_benz10GainMap.jpg",
        "DSC4752_1000x667_100_3x2_benz12GainMap.jpg",
        "DSC5182-2-Edit_1000x667_100_3x2__benz8GainMap.jpg",
        "DSC5447-Edit_1000x667_100_3x2_benz10GainMap.jpg",
        "Triad-gain-map.jpg",
    ];

    for name in samples {
        let path = root.join(name);
        if !path.is_file() {
            eprintln!("skipping gain map sample {}; file missing", path.display());
            continue;
        }

        let info = inspect_ultra_hdr_jpeg(&path).expect("inspect gain map JPEG");
        assert!(
            info.is_ultra_hdr,
            "{} should be detected as Ultra HDR",
            path.display()
        );
        assert!(
            info.primary_xmp_has_gain_map,
            "{} should advertise hdrgm metadata",
            path.display()
        );
        assert!(
            info.gain_map_item_count >= 1 || info.mpf_has_gain_map,
            "{} should locate a gain map via GContainer or MPF",
            path.display()
        );
    }
}

#[test]
fn production_decode_defers_gain_map_compose_to_gpu() {
    let Some(root) = gain_map_samples_root() else {
        eprintln!("skipping deferred decode test; set SIV_GAIN_MAP_SAMPLES_DIR");
        return;
    };
    let path = root.join("Triad-gain-map.jpg");
    if !path.is_file() {
        eprintln!("skipping deferred decode test; {} missing", path.display());
        return;
    }

    let file = std::fs::File::open(&path).expect("open gain map JPEG");
    let bytes = unsafe { memmap2::Mmap::map(&file).expect("mmap gain map JPEG") };
    let capacity = HdrToneMapSettings::default().target_hdr_capacity();

    let deferred = decode_ultra_hdr_jpeg_bytes_with_target_capacity(&bytes, capacity)
        .expect("deferred Ultra HDR decode");
    assert!(
        deferred.rgba_f32.is_empty(),
        "production decode should defer HDR pixels"
    );
    let gain_map = deferred
        .metadata
        .gain_map
        .as_ref()
        .expect("gain map metadata");
    let iso_deferred = gain_map
        .iso_deferred
        .as_ref()
        .expect("jpeg deferred GPU source");
    assert_eq!(
        iso_deferred.sdr_rgba.len(),
        deferred.width as usize * deferred.height as usize * 4
    );
    assert!(iso_deferred.gain_width > 0 && iso_deferred.gain_height > 0);

    let (_, _, baseline_sdr) = libjpeg_turbo::decode_to_rgba(&bytes).expect("baseline SDR");
    assert_eq!(iso_deferred.sdr_rgba.as_slice(), baseline_sdr.as_slice());

    let composed = decode_ultra_hdr_jpeg_bytes_with_cpu_compose(&bytes, capacity)
        .expect("CPU compose reference");
    assert!(
        composed
            .rgba_f32
            .chunks_exact(4)
            .any(|pixel| pixel[0] > 1.0 || pixel[1] > 1.0 || pixel[2] > 1.0),
        "CPU reference should still recover HDR highlights"
    );
}

#[test]
fn gain_map_corpus_decodes_to_hdr_float_buffer() {
    let Some(root) = gain_map_samples_root() else {
        eprintln!("skipping gain map corpus test; set SIV_GAIN_MAP_SAMPLES_DIR");
        return;
    };

    for name in [
        "DSC2306-Edit_1000x667_100_3x2__benz8GainMap.jpg",
        "Triad-gain-map.jpg",
    ] {
        let path = root.join(name);
        if !path.is_file() {
            eprintln!("skipping gain map decode test; {} missing", path.display());
            continue;
        }

        let hdr = decode_ultra_hdr_jpeg(&path).expect("decode gain map JPEG");
        assert_eq!(hdr.format, crate::hdr::types::HdrPixelFormat::Rgba32Float);
        assert!(
            hdr.rgba_f32
                .chunks_exact(4)
                .any(|pixel| pixel[0] > 1.0 || pixel[1] > 1.0 || pixel[2] > 1.0),
            "{} should recover highlights above SDR white",
            path.display()
        );
    }
}

#[test]
fn ultra_hdr_original_samples_are_detected_as_jpeg_r() {
    let Some(root) = ultra_hdr_samples_root() else {
        eprintln!(
            "skipping Ultra HDR corpus test; set SIV_ULTRA_HDR_SAMPLES_DIR to Ultra_HDR_Samples"
        );
        return;
    };

    for index in 1..=10 {
        let path = sample_path(
            &root,
            &format!("Originals/Ultra_HDR_Samples_Originals_{index:02}.jpg"),
        );
        if !path.is_file() {
            eprintln!("skipping Ultra HDR sample {}; file missing", path.display());
            continue;
        }

        let info = inspect_ultra_hdr_jpeg(&path).expect("inspect Ultra HDR JPEG_R sample");
        assert!(
            info.is_ultra_hdr,
            "{} should be detected as Ultra HDR",
            path.display()
        );
        assert!(
            info.primary_xmp_has_gain_map,
            "{} should advertise hdrgm metadata",
            path.display()
        );
        assert!(
            info.gain_map_item_count >= 1,
            "{} should include a gain map item",
            path.display()
        );
    }
}

#[test]
fn plain_jpeg_xmp_is_not_detected_as_jpeg_r() {
    let bytes = minimal_jpeg_with_app1_xmp(
        r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF></rdf:RDF></x:xmpmeta>"#,
    );

    let info = inspect_ultra_hdr_jpeg_bytes(&bytes).expect("inspect plain JPEG");

    assert!(!info.is_ultra_hdr);
    assert!(!info.primary_xmp_has_gain_map);
    assert_eq!(info.gain_map_item_count, 0);
    assert!(!info.mpf_has_gain_map);
}

#[test]
fn ultra_hdr_original_gain_map_jpeg_is_extractable() {
    let Some(root) = ultra_hdr_samples_root() else {
        eprintln!(
            "skipping Ultra HDR corpus test; set SIV_ULTRA_HDR_SAMPLES_DIR to Ultra_HDR_Samples"
        );
        return;
    };
    let path = sample_path(&root, "Originals/Ultra_HDR_Samples_Originals_01.jpg");
    if !path.is_file() {
        eprintln!("skipping Ultra HDR gain map extraction test; sample missing");
        return;
    }

    let gain_map_jpeg = extract_gain_map_jpeg(&path).expect("extract embedded gain map JPEG");
    let (width, height, pixels) =
        libjpeg_turbo::decode_to_rgba(gain_map_jpeg.as_slice()).expect("decode gain map JPEG");

    assert_eq!((width, height), (1020, 768));
    assert_eq!(pixels.len(), width as usize * height as usize * 4);
}

#[test]
fn ultra_hdr_original_decodes_to_hdr_float_buffer() {
    let Some(root) = ultra_hdr_samples_root() else {
        eprintln!(
            "skipping Ultra HDR corpus test; set SIV_ULTRA_HDR_SAMPLES_DIR to Ultra_HDR_Samples"
        );
        return;
    };
    let path = sample_path(&root, "Originals/Ultra_HDR_Samples_Originals_01.jpg");
    if !path.is_file() {
        eprintln!("skipping Ultra HDR decode test; sample missing");
        return;
    }

    let hdr = decode_ultra_hdr_jpeg(&path).expect("decode Ultra HDR JPEG_R");

    assert_eq!((hdr.width, hdr.height), (4080, 3072));
    assert_eq!(hdr.format, crate::hdr::types::HdrPixelFormat::Rgba32Float);
    assert_eq!(
        hdr.color_space,
        crate::hdr::types::HdrColorSpace::LinearSrgb
    );
    assert_eq!(
        hdr.rgba_f32.len(),
        hdr.width as usize * hdr.height as usize * 4
    );
    assert!(
        hdr.rgba_f32
            .chunks_exact(4)
            .any(|pixel| pixel[0] > 1.0 || pixel[1] > 1.0 || pixel[2] > 1.0),
        "Ultra HDR decode should recover highlights above SDR white"
    );
}

#[test]
fn tiled_source_reuses_base_jpeg_decode_for_distinct_tiles() {
    let Some(root) = ultra_hdr_samples_root() else {
        eprintln!(
            "skipping Ultra HDR corpus test; set SIV_ULTRA_HDR_SAMPLES_DIR to Ultra_HDR_Samples"
        );
        return;
    };
    let path = sample_path(&root, "Originals/Ultra_HDR_Samples_Originals_01.jpg");
    if !path.is_file() {
        eprintln!("skipping Ultra HDR tiled decode count test; sample missing");
        return;
    }

    reset_base_jpeg_decode_count();
    let source = UltraHdrTiledImageSource::open_with_target_capacity(
        path,
        1,
        HdrToneMapSettings::default().target_hdr_capacity(),
    )
    .expect("open Ultra HDR tiled source");

    source
        .extract_tile_rgba32f_arc(0, 0, 64, 64)
        .expect("extract first Ultra HDR tile");
    source
        .extract_tile_rgba32f_arc(64, 0, 64, 64)
        .expect("extract second Ultra HDR tile");

    assert_eq!(
        base_jpeg_decode_count(),
        1,
        "Ultra HDR tiled source should decode the base JPEG once and reuse it for distinct tiles"
    );
}

#[test]
fn tiled_source_uses_target_hdr_capacity() {
    let Some(root) = ultra_hdr_samples_root() else {
        eprintln!(
            "skipping Ultra HDR corpus test; set SIV_ULTRA_HDR_SAMPLES_DIR to Ultra_HDR_Samples"
        );
        return;
    };
    let path = sample_path(&root, "Originals/Ultra_HDR_Samples_Originals_01.jpg");
    if !path.is_file() {
        eprintln!("skipping Ultra HDR tiled target capacity test; sample missing");
        return;
    }

    let low = UltraHdrTiledImageSource::open_with_target_capacity(path.clone(), 1, 1.0)
        .expect("open low-capacity Ultra HDR tiled source");
    let high = UltraHdrTiledImageSource::open_with_target_capacity(path, 1, 8.0)
        .expect("open high-capacity Ultra HDR tiled source");
    let low_rgba = compose_ultra_hdr_tile_region_cpu(
        64,
        64,
        0,
        0,
        low.physical_width,
        low.physical_height,
        low.orientation,
        low.sdr_rgba.as_slice(),
        low.gain_rgba.as_slice(),
        low.gain_width,
        low.gain_height,
        low.metadata,
        low.target_hdr_capacity,
        display_to_physical_pixel,
    );
    let high_rgba = compose_ultra_hdr_tile_region_cpu(
        64,
        64,
        0,
        0,
        high.physical_width,
        high.physical_height,
        high.orientation,
        high.sdr_rgba.as_slice(),
        high.gain_rgba.as_slice(),
        high.gain_width,
        high.gain_height,
        high.metadata,
        high.target_hdr_capacity,
        display_to_physical_pixel,
    );

    let low_peak = low_rgba
        .chunks_exact(4)
        .map(|pixel| pixel[0].max(pixel[1]).max(pixel[2]))
        .fold(0.0_f32, f32::max);
    let high_peak = high_rgba
        .chunks_exact(4)
        .map(|pixel| pixel[0].max(pixel[1]).max(pixel[2]))
        .fold(0.0_f32, f32::max);

    assert!(
        high_peak > low_peak,
        "higher target HDR capacity should recover brighter tiled JPEG_R highlights"
    );
}

#[test]
fn gain_map_sampling_interpolates_between_source_pixels() {
    let gain_rgba = vec![
        0, 0, 0, 255, //
        255, 255, 255, 255,
    ];

    let sampled = sample_gain_map_rgb(&gain_rgba, 2, 1, 1, 0, 3, 1)[0];

    assert!((sampled - 0.5).abs() < 0.01);
}

#[test]
fn gain_map_item_length_accepts_length_after_semantic() {
    let xmp = r#"
        <Container:Item
          Item:Mime="image/jpeg"
          Item:Semantic="GainMap"
          Item:Length="12345"/>
    "#;

    assert_eq!(gain_map_item_length(xmp), Some(12345));
}

#[test]
fn gain_map_metadata_parses_hdr_capacity_bounds() {
    let gain_map_jpeg = minimal_jpeg_with_app1_xmp(
        r#"
        <rdf:Description
          xmlns:hdrgm="http://ns.adobe.com/hdr-gain-map/1.0/"
          hdrgm:Version="1.0"
          hdrgm:GainMapMax="3.0"
          hdrgm:HDRCapacityMin="1.25"
          hdrgm:HDRCapacityMax="4.5"/>
    "#,
    );

    let metadata = gain_map_metadata(&gain_map_jpeg).expect("parse gain map metadata");

    assert!((metadata.hdr_capacity_min - 2.0_f32.powf(1.25)).abs() < 0.001);
    assert!((metadata.hdr_capacity_max - 2.0_f32.powf(4.5)).abs() < 0.001);
}

#[test]
