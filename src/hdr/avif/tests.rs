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

#[cfg(feature = "avif-native")]
use crate::hdr::avif::avif_gain_map_to_metadata;
use crate::hdr::avif::{avif_cicp_to_metadata, is_avif_brand};
use crate::hdr::types::{HdrColorProfile, HdrColorSpace, HdrReference, HdrTransferFunction};

#[test]
fn avif_cicp_maps_bt2020_pq_to_hdr_metadata() {
    let metadata = avif_cicp_to_metadata(9, 16, 9, false);

    assert_eq!(metadata.transfer_function, HdrTransferFunction::Pq);
    assert_eq!(metadata.reference, HdrReference::DisplayReferred);
    assert_eq!(
        metadata.color_profile,
        HdrColorProfile::Cicp {
            color_primaries: 9,
            transfer_characteristics: 16,
            matrix_coefficients: 9,
            full_range: false,
        }
    );
}

#[test]
fn avif_cicp_maps_bt2020_hlg_to_rec2020_linear_color_space() {
    let metadata = avif_cicp_to_metadata(9, 18, 9, true);

    assert_eq!(metadata.transfer_function, HdrTransferFunction::Hlg);
    assert_eq!(metadata.reference, HdrReference::SceneLinear);
    assert_eq!(metadata.color_space_hint(), HdrColorSpace::Rec2020Linear);
}

#[test]
fn avif_brand_detection_accepts_avif_and_avis() {
    assert!(is_avif_brand(b"avif"));
    assert!(is_avif_brand(b"avis"));
    assert!(!is_avif_brand(b"heic"));
}

#[cfg(feature = "avif-native")]
#[test]
fn avif_gain_map_fractions_convert_to_shared_metadata() {
    let gain_map = libavif_sys::avifGainMap {
        image: std::ptr::null_mut(),
        gainMapMin: [signed(0, 10), signed(1, 10), signed(2, 10)],
        gainMapMax: [signed(20, 10), signed(30, 10), signed(40, 10)],
        gainMapGamma: [unsigned(10, 10), unsigned(11, 10), unsigned(12, 10)],
        baseOffset: [signed(0, 10), signed(1, 10), signed(2, 10)],
        alternateOffset: [signed(3, 10), signed(4, 10), signed(5, 10)],
        baseHdrHeadroom: unsigned(0, 10),
        alternateHdrHeadroom: unsigned(20, 10),
        useBaseColorSpace: 1,
        altICC: libavif_sys::avifRWData {
            data: std::ptr::null_mut(),
            size: 0,
        },
        altColorPrimaries: 9,
        altTransferCharacteristics: 16,
        altMatrixCoefficients: 9,
        altYUVRange: 1,
        altDepth: 10,
        altPlaneCount: 3,
        altCLLI: libavif_sys::avifContentLightLevelInformationBox {
            maxCLL: 0,
            maxPALL: 0,
        },
    };

    let metadata = avif_gain_map_to_metadata(&gain_map).expect("convert metadata");

    assert_eq!(metadata.gain_map_min, [0.0, 0.1, 0.2]);
    assert_eq!(metadata.gain_map_max, [2.0, 3.0, 4.0]);
    assert_eq!(metadata.gamma, [1.0, 1.1, 1.2]);
    assert_eq!(metadata.offset_sdr, [0.0, 0.1, 0.2]);
    assert_eq!(metadata.offset_hdr, [0.3, 0.4, 0.5]);
    assert_eq!(metadata.hdr_capacity_min, 1.0);
    assert_eq!(metadata.hdr_capacity_max, 4.0);
}

#[cfg(feature = "avif-native")]
fn signed(n: i32, d: u32) -> libavif_sys::avifSignedFraction {
    libavif_sys::avifSignedFraction { n, d }
}

#[cfg(feature = "avif-native")]
fn unsigned(n: u32, d: u32) -> libavif_sys::avifUnsignedFraction {
    libavif_sys::avifUnsignedFraction { n, d }
}

#[cfg(feature = "avif-native")]
#[test]
fn avif_irot_ccw_quarter_turns_map_to_exif_like_libavif_table() {
    use super::{AVIF_TRANSFORM_IMIR_FLAG as IMIR, AVIF_TRANSFORM_IROT_FLAG as IROT};

    assert_eq!(super::avif_irot_imir_to_exif_orientation(0, 0, 0), 1);

    assert_eq!(super::avif_irot_imir_to_exif_orientation(IROT, 1, 0), 8);
    assert_eq!(super::avif_irot_imir_to_exif_orientation(IROT, 2, 0), 3);
    assert_eq!(super::avif_irot_imir_to_exif_orientation(IROT, 3, 0), 6);

    assert_eq!(super::avif_irot_imir_to_exif_orientation(IMIR, 0, 0), 4);
    assert_eq!(super::avif_irot_imir_to_exif_orientation(IMIR, 0, 1), 2);

    assert_eq!(
        super::avif_irot_imir_to_exif_orientation(IROT | IMIR, 1, 0),
        5
    );
    assert_eq!(
        super::avif_irot_imir_to_exif_orientation(IROT | IMIR, 1, 1),
        7
    );
    assert_eq!(
        super::avif_irot_imir_to_exif_orientation(IROT | IMIR, 2, 1),
        4
    );
    assert_eq!(
        super::avif_irot_imir_to_exif_orientation(IROT | IMIR, 3, 1),
        5
    );
}

#[test]
fn avif_yuv_to_rgb_metadata_overrides_pq_hlg_without_gain_map() {
    use crate::hdr::types::{HdrColorProfile, HdrReference, HdrTransferFunction};

    let cicp = avif_cicp_to_metadata(9, 16, 9, true);
    assert_eq!(cicp.transfer_function, HdrTransferFunction::Pq);

    let image = libavif_sys::avifImage {
        gainMap: std::ptr::null_mut(),
        transferCharacteristics: 16,
        colorPrimaries: 9,
        matrixCoefficients: 9,
        ..unsafe { std::mem::zeroed() }
    };
    let adjusted = super::avif_yuv_to_rgb_output_metadata(&cicp, &image);
    assert_eq!(adjusted.transfer_function, HdrTransferFunction::Srgb);
    assert_eq!(adjusted.reference, HdrReference::Unknown);
    assert_eq!(adjusted.color_profile, HdrColorProfile::LinearSrgb);
}

#[test]
fn avif_yuv_to_rgb_metadata_overrides_unspecified_cicp_without_gain_map() {
    use crate::hdr::types::{HdrColorProfile, HdrTransferFunction};

    let cicp = avif_cicp_to_metadata(2, 2, 2, true);
    assert_eq!(cicp.transfer_function, HdrTransferFunction::Unknown);

    let image = libavif_sys::avifImage {
        gainMap: std::ptr::null_mut(),
        transferCharacteristics: 2,
        colorPrimaries: 2,
        matrixCoefficients: 2,
        ..unsafe { std::mem::zeroed() }
    };
    let adjusted = super::avif_yuv_to_rgb_output_metadata(&cicp, &image);
    assert_eq!(adjusted.transfer_function, HdrTransferFunction::Srgb);
    assert_eq!(adjusted.color_profile, HdrColorProfile::LinearSrgb);
    assert_eq!(adjusted.color_space_hint(), HdrColorSpace::LinearSrgb);
}

#[cfg(feature = "avif-native")]
#[test]
fn avif_software_gain_map_decode_defers_compose_to_gpu() {
    use crate::hdr::jpeg_gain_map_gpu::iso_deferred_from_metadata;
    use crate::hdr::types::HdrToneMapSettings;
    use std::path::PathBuf;

    let candidates = [
        std::env::var_os("SIV_AVIF_GAIN_MAP_SAMPLE").map(PathBuf::from),
        Some(PathBuf::from(
            r"F:\HDR\libavif\tests\data\seine_sdr_gainmap_srgb.avif",
        )),
        Some(PathBuf::from(
            r"F:\HDR\libavif\tests\data\seine_sdr_gainmap_srgb_icc.avif",
        )),
        Some(PathBuf::from(
            r"F:\HDR\av1-avif\testFiles\Netflix\avif\hdr_cosmos07296_cicp9-16-9_yuv444_full_qp40.avif",
        )),
    ];
    let Some(path) = candidates.into_iter().flatten().find(|p| p.is_file()) else {
        eprintln!(
            "skip avif deferred test; set SIV_AVIF_GAIN_MAP_SAMPLE to an AVIF with ISO gain map"
        );
        return;
    };
    let bytes = std::fs::read(&path).expect("read avif sample");
    let capacity = HdrToneMapSettings::default().target_hdr_capacity();
    let hdr =
        super::decode_avif_hdr_bytes_with_target_capacity(&bytes, capacity).expect("decode avif");
    if iso_deferred_from_metadata(&hdr.metadata).is_some() {
        assert!(
            hdr.rgba_f32.is_empty(),
            "{} should defer ISO gain-map compose to GPU",
            path.display()
        );
        assert_eq!(
            hdr.metadata.gain_map.as_ref().map(|gm| gm.source),
            Some("AVIF")
        );
    } else if !hdr.rgba_f32.is_empty() {
        eprintln!(
            "{} decoded as eager float HDR (precomposed gain-map base or non-gain-map sample)",
            path.display()
        );
    } else {
        panic!(
            "{} decoded to empty HDR buffer without GPU-deferred planes",
            path.display()
        );
    }
}

#[cfg(feature = "avif-native")]
#[test]
fn avif_kimono_rotate90_strip_applies_container_orientation_when_sample_present() {
    use crate::loader::{
        DirectoryTreeThumbDecodeOptions, generate_directory_tree_thumb_decode_from_path,
    };

    let path = std::path::Path::new(r"F:\HDR\av1-avif\testFiles\Link-U\kimono.rotate90.avif");
    if !path.is_file() {
        eprintln!("skip: {}", path.display());
        return;
    }
    let strip = generate_directory_tree_thumb_decode_from_path(
        path,
        128,
        DirectoryTreeThumbDecodeOptions::default(),
    )
    .expect("kimono.rotate90 strip decode");
    assert_eq!(strip.logical_size, (722, 1024));
    assert!(
        strip.preview.height > strip.preview.width,
        "strip preview must apply irot so portrait logical size matches pixel layout"
    );
}

#[cfg(feature = "avif-native")]
#[test]
fn decode_truncated_elementary_stream_8bpc_avif_when_sample_present() {
    use crate::hdr::types::HdrToneMapSettings;

    let path = std::path::Path::new(
        r"F:\HDR\av1-avif\testFiles\Apple\edge_case_testing\non_compliant\truncated_elementary_stream.avif",
    );
    if !path.is_file() {
        eprintln!("skip: {}", path.display());
        return;
    }
    let bytes = std::fs::read(path).expect("read avif");
    let capacity = HdrToneMapSettings::default().target_hdr_capacity();
    let hdr = super::decode_avif_hdr_bytes_with_target_capacity(&bytes, capacity)
        .expect("decode truncated elementary stream avif");
    let w = hdr.width as usize;
    let h = hdr.height as usize;
    let cx = w / 2;
    let avg_r = |y0: usize, y1: usize| -> f32 {
        let mut sum = 0.0_f32;
        for y in y0..y1 {
            sum += hdr.rgba_f32[(y * w + cx) * 4];
        }
        sum / (y1 - y0).max(1) as f32
    };
    let top_r = avg_r(0, h / 2);
    let bot_r = avg_r(h / 2, h);
    eprintln!("truncated_elementary_stream: top_avg_r={top_r:.3} bot_avg_r={bot_r:.3}");
    assert!(
        top_r > 0.05 && bot_r > 0.05,
        "8bpc AVIF must unpack 1 byte/channel RGBA; misreading packed bytes as u16 yields white top / black bottom"
    );
    assert!(
        top_r < 1.5 && bot_r < 1.5,
        "decoded red channel should stay in normalized float range"
    );
}

#[cfg(feature = "avif-native")]
#[test]
fn decode_mexico_yuv444_avif_metadata_when_sample_present() {
    use crate::hdr::types::HdrToneMapSettings;
    let path = std::path::Path::new(r"F:\HDR\av1-avif\testFiles\Microsoft\Mexico_YUV444.avif");
    if !path.is_file() {
        eprintln!("skip: {}", path.display());
        return;
    }
    let bytes = std::fs::read(path).expect("read avif");
    let capacity = HdrToneMapSettings::default().target_hdr_capacity();
    let hdr = super::decode_avif_hdr_bytes_with_target_capacity(&bytes, capacity)
        .expect("decode mexico avif");
    assert_eq!(
        hdr.metadata.transfer_function,
        HdrTransferFunction::Srgb,
        "unspecified CICP YUV→RGB must use sRGB shader decode"
    );
    assert_eq!(hdr.color_space, HdrColorSpace::LinearSrgb);
    eprintln!(
        "Mexico: {}x{} tf={:?} ref={:?} cs={:?} profile={:?} gain={}",
        hdr.width,
        hdr.height,
        hdr.metadata.transfer_function,
        hdr.metadata.reference,
        hdr.color_space,
        hdr.metadata.color_profile,
        hdr.metadata.gain_map.is_some(),
    );
    let mut mn = f32::INFINITY;
    let mut mx = f32::NEG_INFINITY;
    let mut sum = 0.0_f64;
    let mut n = 0_usize;
    for px in hdr.rgba_f32.chunks_exact(4) {
        for &c in &px[..3] {
            if c.is_finite() {
                mn = mn.min(c);
                mx = mx.max(c);
                sum += c as f64;
                n += 1;
            }
        }
    }
    eprintln!(
        "float RGB min={mn:.4} max={mx:.4} avg={:.4}",
        sum / n.max(1) as f64
    );
}

#[cfg(feature = "avif-native")]
#[test]
fn decode_paris_icc_exif_xmp_avif_when_sample_present() {
    use crate::hdr::types::HdrToneMapSettings;
    use crate::loader::{DecodedImage, hdr_sdr_fallback_rgba8_or_placeholder};

    let path = std::path::Path::new(r"F:\HDR\libavif\tests\data\paris_icc_exif_xmp.avif");
    if !path.is_file() {
        eprintln!("skip: {}", path.display());
        return;
    }
    let bytes = std::fs::read(path).expect("read avif");
    let tone = HdrToneMapSettings::default();
    let capacity = tone.target_hdr_capacity();
    let hdr = super::decode_avif_hdr_bytes_with_target_capacity(&bytes, capacity).expect("decode");
    let fallback = DecodedImage::from_hdr_sdr_fallback(
        hdr.width,
        hdr.height,
        hdr_sdr_fallback_rgba8_or_placeholder(&hdr).expect("fallback"),
    );
    eprintln!(
        "paris: {}x{} tf={:?} ref={:?} cs={:?} profile={:?} gain={:?}",
        hdr.width,
        hdr.height,
        hdr.metadata.transfer_function,
        hdr.metadata.reference,
        hdr.color_space,
        hdr.metadata.color_profile,
        hdr.metadata.gain_map.is_some(),
    );
    let mut min_a = f32::INFINITY;
    let mut max_a = f32::NEG_INFINITY;
    let mut min_rgb = f32::INFINITY;
    let mut max_rgb = f32::NEG_INFINITY;
    let mut zero_alpha_pixels = 0_usize;
    for px in hdr.rgba_f32.chunks_exact(4) {
        let a = px[3];
        min_a = min_a.min(a);
        max_a = max_a.max(a);
        if a <= 0.0 {
            zero_alpha_pixels += 1;
        }
        for &c in &px[..3] {
            if c.is_finite() {
                min_rgb = min_rgb.min(c);
                max_rgb = max_rgb.max(c);
            }
        }
    }
    eprintln!(
        "float alpha min={min_a:.4} max={max_a:.4} zero_alpha_px={zero_alpha_pixels}/{}",
        hdr.rgba_f32.len() / 4
    );
    eprintln!("float rgb min={min_rgb:.4} max={max_rgb:.4}");
    let fb = fallback.rgba();
    let fb_center = fb.len() / 8;
    eprintln!(
        "fallback center rgba8 = [{}, {}, {}, {}]",
        fb[fb_center],
        fb[fb_center + 1],
        fb[fb_center + 2],
        fb[fb_center + 3]
    );
    assert!(
        max_rgb > 0.01,
        "paris ICC AVIF should not decode to all-black RGB"
    );
    assert!(
        max_a > 0.01,
        "paris ICC AVIF alpha must be non-zero for HDR shader (alpha<=0 forces black)"
    );
}

#[cfg(feature = "avif-native")]
#[test]
fn avif_animated_sequence_decodes_as_hdr_frames_when_sample_present() {
    use crate::hdr::types::HdrToneMapSettings;
    use std::path::PathBuf;

    let candidates = [
        PathBuf::from(r"F:\HDR\av1-avif\testFiles\Netflix\avis\Chimera-AV1-10bit-480x270.avif"),
        PathBuf::from(r"F:\HDR\av1-avif\testFiles\Netflix\avis\alpha_video.avif"),
        PathBuf::from(r"F:\HDR\libavif\tests\data\colors-animated-8bpc-alpha-exif-xmp.avif"),
    ];
    let Some(path) = candidates.into_iter().find(|p| p.is_file()) else {
        eprintln!("skip avif animated hdr test; none of the reference samples are present");
        return;
    };
    let bytes = std::fs::read(&path).expect("read avif");
    let capacity = HdrToneMapSettings::default().target_hdr_capacity();
    let decode = super::try_decode_avif_image_sequence_hdr_limited(&bytes, capacity, None)
        .expect("decode avif sequence")
        .expect("animated avif should expose a sequence");
    assert!(
        decode.total_frame_count > 1,
        "{} should have multiple frames",
        path.display()
    );
    for (idx, (_delay, hdr)) in decode.frames.iter().enumerate() {
        use crate::hdr::jpeg_gain_map_gpu::iso_deferred_from_metadata;
        let deferred = iso_deferred_from_metadata(&hdr.metadata).is_some();
        assert!(
            deferred || !hdr.rgba_f32.is_empty(),
            "{} frame {idx} should carry HDR float pixels or GPU-deferred gain-map planes",
            path.display()
        );
        assert!(hdr.width > 0 && hdr.height > 0);
    }
    eprintln!(
        "{} -> {} HdrAnimated frames, tf={:?}",
        path.display(),
        decode.total_frame_count,
        decode.frames[0].1.metadata.transfer_function
    );
}

#[cfg(feature = "avif-native")]
#[test]
fn avif_animated_sequence_bootstrap_decodes_first_frame_only() {
    use crate::hdr::types::HdrToneMapSettings;
    use std::path::PathBuf;

    let path =
        PathBuf::from(r"F:\HDR\av1-avif\testFiles\Netflix\avis\Chimera-AV1-10bit-480x270.avif");
    if !path.is_file() {
        eprintln!("skip avif bootstrap test; {} not present", path.display());
        return;
    }
    let bytes = std::fs::read(&path).expect("read avif");
    let capacity = HdrToneMapSettings::default().target_hdr_capacity();
    let decode = super::try_decode_avif_image_sequence_hdr_limited(&bytes, capacity, Some(1))
        .expect("decode avif sequence")
        .expect("animated avif should expose a sequence");
    assert_eq!(decode.frames.len(), 1);
    assert!(
        decode.total_frame_count > 1,
        "bootstrap sample should be a multi-frame sequence"
    );
}

/// Local probe: `cargo test probe_netflix_cosmos -- --ignored --nocapture`
#[cfg(feature = "avif-native")]
#[test]
#[ignore = "manual probe against Netflix cosmos AVIF on disk"]
fn probe_netflix_cosmos_raw_decode() {
    use crate::hdr::decode::{
        decode_transfer_to_display_linear, hdr_to_sdr_rgba8_with_tone_settings,
    };
    use crate::hdr::types::HdrToneMapSettings;
    use std::path::Path;

    let path = Path::new(
        "/home/happy/Downloads/HDR/av1-avif/testFiles/Netflix/avif/hdr_cosmos07296_cicp9-16-9_yuv444_full_qp40.avif",
    );
    if !path.is_file() {
        eprintln!("skip: {}", path.display());
        return;
    }
    let bytes = std::fs::read(path).expect("read avif");
    let hdr = super::decode_avif_hdr_bytes(&bytes).expect("decode avif");
    let cx = hdr.width as usize / 2;
    let cy = hdr.height as usize / 2;
    let i = (cy * hdr.width as usize + cx) * 4;
    let raw = [hdr.rgba_f32[i], hdr.rgba_f32[i + 1], hdr.rgba_f32[i + 2]];
    eprintln!(
        "metadata tf={:?} cs={:?}",
        hdr.metadata.transfer_function, hdr.color_space
    );
    eprintln!("center raw f32 RGB = {raw:?}");
    let tone = HdrToneMapSettings {
        max_display_nits: 450.0,
        ..HdrToneMapSettings::default()
    };
    let linear =
        decode_transfer_to_display_linear(raw, hdr.metadata.transfer_function, tone.sdr_white_nits);
    eprintln!("center display-linear = {linear:?}");
    assert!(
        linear[0] < 1.5 && linear[1] < 1.5 && linear[2] < 1.5,
        "PQ double-decode would push linear values far above 1.0"
    );
    let sdr = hdr_to_sdr_rgba8_with_tone_settings(&hdr, 0.0, &tone).expect("sdr");
    eprintln!(
        "center sdr rgba8 = [{}, {}, {}]",
        sdr[i],
        sdr[i + 1],
        sdr[i + 2]
    );
}

#[cfg(feature = "avif-native")]
#[test]
fn probe_gain_map_sample_avif_base_hdr_folder() {
    use crate::hdr::decode::hdr_to_sdr_rgba8_with_tone_settings;
    use crate::hdr::types::{DEFAULT_SDR_WHITE_NITS, HdrToneMapSettings};
    use crate::loader::hdr_has_iso_deferred_gain_map;

    let dir = std::path::Path::new(
        r"F:\HDR\Gain_Map_Sample_Photos\Gain_Map_Sample_Photos\samples_avif_base_hdr",
    );
    if !dir.is_dir() {
        eprintln!("skip: {}", dir.display());
        return;
    }
    let path = dir.join("01_base_hdr.avif");
    let bytes = std::fs::read(&path).expect("read");
    let hdr = super::decode_avif_hdr_bytes_with_target_capacity(&bytes, 1.0).expect("decode");
    assert!(!hdr_has_iso_deferred_gain_map(&hdr));
    let mean_luma = |pixels: &[u8]| -> f32 {
        pixels
            .chunks_exact(4)
            .map(|px| 0.2126 * px[0] as f32 + 0.7152 * px[1] as f32 + 0.0722 * px[2] as f32)
            .sum::<f32>()
            / (pixels.len() / 4).max(1) as f32
    };
    let default_tone = HdrToneMapSettings::default();
    let bright_tone = HdrToneMapSettings {
        max_display_nits: DEFAULT_SDR_WHITE_NITS,
        ..default_tone
    };
    let default_sdr =
        hdr_to_sdr_rgba8_with_tone_settings(&hdr, 0.0, &default_tone).expect("default sdr");
    let bright_sdr =
        hdr_to_sdr_rgba8_with_tone_settings(&hdr, 0.0, &bright_tone).expect("bright sdr");
    eprintln!(
        "01_base_hdr luma default={:.1} bright_peak={:.1}",
        mean_luma(&default_sdr),
        mean_luma(&bright_sdr)
    );
}

/// Probe (A) ApplyGainMap output sizing and (B) safe YUV scale via SetViewRect.
#[cfg(feature = "avif-native")]
#[test]
fn probe_avif_strip_scale_and_apply_gain_map() {
    use std::time::Instant;

    use super::decode::read_avif_decoder_image;
    use crate::hdr::tiled::preview_dimensions;

    let path = std::path::Path::new(
        r"F:\HDR\Gain_Map_Sample_Photos\Gain_Map_Sample_Photos\samples_avif_base_sdr\01_base_sdr.avif",
    );
    if !path.is_file() {
        eprintln!("skip: {}", path.display());
        return;
    }
    let bytes = std::fs::read(path).expect("read");
    let max_side = 128_u32;
    let image = read_avif_decoder_image(&bytes).expect("decode");
    let image_ptr = image.as_ptr();
    let image_ref = unsafe { &*image_ptr };
    assert!(!image_ref.gainMap.is_null());
    let (strip_w, strip_h) =
        preview_dimensions(image_ref.width, image_ref.height, max_side, max_side);
    eprintln!(
        "logical={}x{} strip={strip_w}x{strip_h}",
        image_ref.width, image_ref.height
    );
    let mut diag = libavif_sys::avifDiagnostics { error: [0; 256] };

    eprintln!("(B0) in-place noop scale on decoder image...");
    let noop = unsafe {
        libavif_sys::avifImageScale(image_ptr, image_ref.width, image_ref.height, &mut diag)
    };
    eprintln!("(B0) -> {}", super::decode::libavif_result_to_string(noop));

    eprintln!("(B0b) in-place strip scale on decoder image (same decode)...");
    let image2 = read_avif_decoder_image(&bytes).expect("decode2");
    let t0 = Instant::now();
    let in_place =
        unsafe { libavif_sys::avifImageScale(image2.as_ptr(), strip_w, strip_h, &mut diag) };
    eprintln!(
        "(B0b) -> {} in {}ms",
        super::decode::libavif_result_to_string(in_place),
        t0.elapsed().as_millis()
    );

    // (B) Deep-copy base YUV then scale (owned planes).
    eprintln!("(B) copy YUV + progressive scale...");
    let t_copy_scale = Instant::now();
    let scaled_base = {
        let Some(copy) = libavif_sys::AvifImageOwned::create_empty() else {
            panic!("create_empty");
        };
        let copy_res = unsafe {
            libavif_sys::avifImageCopy(copy.as_ptr(), image_ptr, libavif_sys::AVIF_PLANES_YUV)
        };
        eprintln!(
            "(B) avifImageCopy -> {}",
            super::decode::libavif_result_to_string(copy_res)
        );
        assert_eq!(copy_res, libavif_sys::AVIF_RESULT_OK);
        unsafe {
            (*copy.as_ptr()).gainMap = std::ptr::null_mut();
        }
        let copy_ref = unsafe { &*copy.as_ptr() };
        eprintln!(
            "(B) copy planes: {}x{} depth={} format={}",
            copy_ref.width, copy_ref.height, copy_ref.depth, copy_ref.yuvFormat
        );
        for (label, tw, th) in [
            ("noop", image_ref.width, image_ref.height),
            ("half", copy_ref.width / 2, copy_ref.height / 2),
            ("strip", strip_w, strip_h),
        ] {
            eprintln!("(B) avifImageScale {label} -> {tw}x{th}...");
            let scale = unsafe { libavif_sys::avifImageScale(copy.as_ptr(), tw, th, &mut diag) };
            eprintln!(
                "(B) {label} -> {}",
                super::decode::libavif_result_to_string(scale)
            );
            if label == "noop" {
                assert_eq!(scale, libavif_sys::AVIF_RESULT_OK);
                continue;
            }
            assert_eq!(scale, libavif_sys::AVIF_RESULT_OK);
        }
        copy
    };
    eprintln!(
        "(B) copy+scale total {}ms",
        t_copy_scale.elapsed().as_millis()
    );

    // (A) ApplyGainMap on strip-sized base: output dims follow baseImage, not caller preset.
    {
        let gain_map = unsafe { &*image_ref.gainMap };
        let scaled_ref = unsafe { &*scaled_base.as_ptr() };
        let mut rgb_out = std::mem::MaybeUninit::<libavif_sys::avifRGBImage>::zeroed();
        unsafe {
            libavif_sys::avifRGBImageSetDefaults(rgb_out.as_mut_ptr(), scaled_base.as_ptr());
        }
        let mut rgb_out = unsafe { rgb_out.assume_init() };
        let hdr_headroom = 4.9_f32.log2();
        let t0 = Instant::now();
        let result = unsafe {
            libavif_sys::avifImageApplyGainMap(
                scaled_base.as_ptr(),
                gain_map,
                hdr_headroom,
                libavif_sys::AVIF_COLOR_PRIMARIES_BT709,
                libavif_sys::AVIF_TRANSFER_CHARACTERISTICS_LINEAR,
                &mut rgb_out,
                std::ptr::null_mut(),
                &mut diag,
            )
        };
        eprintln!(
            "(A) ApplyGainMap -> {} out={}x{} (base={}x{}) in {}ms",
            super::decode::libavif_result_to_string(result),
            rgb_out.width,
            rgb_out.height,
            scaled_ref.width,
            scaled_ref.height,
            t0.elapsed().as_millis()
        );
        assert_eq!(result, libavif_sys::AVIF_RESULT_OK);
        assert_eq!(rgb_out.width, strip_w);
        assert_eq!(rgb_out.height, strip_h);
        unsafe { libavif_sys::avifRGBImageFreePixels(&mut rgb_out) };
    }
}
