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

#[cfg(feature = "jpegxl")]
use crate::hdr::jpegxl::read_jxl_gain_map_bundle;
#[cfg(feature = "jpegxl")]
use crate::hdr::jpegxl::{
    JXL_TRANSFER_FUNCTION_HLG, JXL_TRANSFER_FUNCTION_LINEAR, JXL_TRANSFER_FUNCTION_PQ,
    JXL_TRANSFER_FUNCTION_SRGB, is_jxl_header, jxl_color_encoding_to_metadata,
};
#[cfg(feature = "jpegxl")]
use crate::hdr::types::HdrColorSpace;
#[cfg(feature = "jpegxl")]
use crate::hdr::types::{
    HdrImageMetadata, HdrLuminanceMetadata, HdrReference, HdrTransferFunction,
};

#[cfg(feature = "jpegxl")]
fn jxl_sdr_grade_metadata(
    transfer_function: HdrTransferFunction,
    mastering_max_nits: f32,
) -> HdrImageMetadata {
    HdrImageMetadata {
        transfer_function,
        luminance: HdrLuminanceMetadata {
            mastering_max_nits: Some(mastering_max_nits),
            ..Default::default()
        },
        ..Default::default()
    }
}

#[cfg(feature = "jpegxl")]
#[test]
fn jxl_header_detection_accepts_codestream_and_container() {
    assert!(is_jxl_header(&[0xff, 0x0a, 0x00, 0x00]));
    assert!(is_jxl_header(&[
        0x00, 0x00, 0x00, 0x0c, b'J', b'X', b'L', b' ', 0x0d, 0x0a, 0x87, 0x0a,
    ]));
    assert!(!is_jxl_header(b"\x89PNG"));
}

#[cfg(feature = "jpegxl")]
#[test]
fn jxl_pq_metadata_is_display_referred_with_intensity_target() {
    let metadata = jxl_color_encoding_to_metadata(9, JXL_TRANSFER_FUNCTION_PQ, Some(4000.0));

    assert_eq!(metadata.transfer_function, HdrTransferFunction::Pq);
    assert_eq!(metadata.reference, HdrReference::DisplayReferred);
    assert_eq!(metadata.luminance.mastering_max_nits, Some(4000.0));
}

#[cfg(feature = "jpegxl")]
#[test]
fn jxl_linear_transfer_maps_for_float_decoder_output() {
    let metadata = jxl_color_encoding_to_metadata(9, JXL_TRANSFER_FUNCTION_LINEAR, Some(1000.0));

    assert_eq!(metadata.transfer_function, HdrTransferFunction::Linear);
    assert_eq!(metadata.reference, HdrReference::Unknown);
}

#[cfg(feature = "jpegxl")]
#[test]
fn jxl_transfer_named_consts_match_libjxl_headers() {
    assert_eq!(
        JXL_TRANSFER_FUNCTION_LINEAR,
        libjxl_sys::JXL_TRANSFER_FUNCTION_LINEAR as u16
    );
    assert_eq!(
        JXL_TRANSFER_FUNCTION_SRGB,
        libjxl_sys::JXL_TRANSFER_FUNCTION_SRGB as u16
    );
    assert_eq!(
        JXL_TRANSFER_FUNCTION_PQ,
        libjxl_sys::JXL_TRANSFER_FUNCTION_PQ as u16
    );
    assert_eq!(
        JXL_TRANSFER_FUNCTION_HLG,
        libjxl_sys::JXL_TRANSFER_FUNCTION_HLG as u16
    );
}

#[cfg(feature = "jpegxl")]
#[test]
fn jxl_float_decode_metadata_maps_custom_bt2020_xy_to_rec2020_hint() {
    let mut color: libjxl_sys::JxlColorEncoding = unsafe { std::mem::zeroed() };
    color.color_space = libjxl_sys::JXL_COLOR_SPACE_RGB;
    color.primaries = libjxl_sys::JXL_PRIMARIES_CUSTOM;
    color.primaries_red_xy = [0.708, 0.292];
    color.primaries_green_xy = [0.17, 0.797];
    color.primaries_blue_xy = [0.131, 0.046];
    color.transfer_function = libjxl_sys::JXL_TRANSFER_FUNCTION_PQ;

    let m = super::hdr_metadata_from_jxl_float_decode(&color);

    assert_eq!(m.color_space_hint(), HdrColorSpace::Rec2020Linear);
    assert_eq!(m.transfer_function, HdrTransferFunction::Pq);
    assert_eq!(m.reference, HdrReference::DisplayReferred);
}

#[cfg(feature = "jpegxl")]
#[test]
fn jxl_float_decode_metadata_maps_p3_primaries_enum() {
    let mut color: libjxl_sys::JxlColorEncoding = unsafe { std::mem::zeroed() };
    color.color_space = libjxl_sys::JXL_COLOR_SPACE_RGB;
    color.primaries = libjxl_sys::JXL_PRIMARIES_P3;
    color.transfer_function = libjxl_sys::JXL_TRANSFER_FUNCTION_SRGB;

    let m = super::hdr_metadata_from_jxl_float_decode(&color);

    assert_eq!(m.color_space_hint(), HdrColorSpace::DisplayP3Linear);
    assert_eq!(m.transfer_function, HdrTransferFunction::Srgb);
}

#[cfg(feature = "jpegxl")]
#[test]
fn jxl_sdr_grade_fallback_direct_quantize_for_already_encoded_srgb() {
    // libjxl preserves codestream encoding in the float buffer for Modular
    // mode files: TF=sRGB ->already-encoded floats. The fast path quantizes
    // them directly via `value * 255` (no second-pass OETF).
    let rgba = vec![1.0_f32, 0.5, 0.0, 1.0];
    let meta = jxl_sdr_grade_metadata(HdrTransferFunction::Srgb, 255.0);
    let px = super::jxl_sdr_grade_fallback_rgba8(&rgba, HdrColorSpace::LinearSrgb, &meta)
        .expect("sdr-grade content must use direct sRGB encode");
    assert_eq!(px[0], 255, "1.0 ->255, got {}", px[0]);
    assert!(
        (px[1] as i32 - 128).abs() <= 1,
        "0.5 ->~128 (direct quantize, no second-pass OETF), got {}",
        px[1]
    );
    assert_eq!(px[2], 0);
    assert_eq!(px[3], 255);
}

#[cfg(feature = "jpegxl")]
#[test]
fn jxl_sdr_grade_fallback_applies_srgb_oetf_for_truly_linear_floats() {
    // For TF=Linear codestream sources (e.g. conformance `patches/input.jxl`)
    // libjxl emits truly linear floats. The fast path must apply the sRGB
    // OETF before quantizing or shadows quantize ~22 codes too dark.
    // Linear 0.5 ->encoded ~0.735 ->~187 in 8-bit (not 128).
    let rgba = vec![1.0_f32, 0.5, 0.0, 1.0];
    let meta = jxl_sdr_grade_metadata(HdrTransferFunction::Linear, 255.0);
    let px = super::jxl_sdr_grade_fallback_rgba8(&rgba, HdrColorSpace::LinearSrgb, &meta)
        .expect("sdr-grade content must use the OETF + quantize path");
    assert_eq!(px[0], 255, "1.0 ->255, got {}", px[0]);
    assert!(
        (px[1] as i32 - 188).abs() <= 1,
        "linear 0.5 ->encoded ~188 (sRGB OETF), got {}",
        px[1]
    );
    assert_eq!(px[2], 0);
    assert_eq!(px[3], 255);
}

#[cfg(feature = "jpegxl")]
#[test]
fn jxl_sdr_grade_srgb_tags_display_referred() {
    let mut meta = jxl_sdr_grade_metadata(HdrTransferFunction::Srgb, 255.0);
    super::jxl_tag_display_referred_when_sdr_grade(&mut meta);
    assert_eq!(meta.reference, HdrReference::DisplayReferred);
}

#[cfg(feature = "jpegxl")]
#[test]
fn jxl_sdr_grade_linear_does_not_tag_display_referred() {
    let mut meta = jxl_sdr_grade_metadata(HdrTransferFunction::Linear, 255.0);
    super::jxl_tag_display_referred_when_sdr_grade(&mut meta);
    assert_ne!(meta.reference, HdrReference::DisplayReferred);
}

#[cfg(feature = "jpegxl")]
#[test]
fn jxl_sdr_grade_fallback_skipped_for_high_peak_hdr() {
    let rgba = vec![1.0_f32, 1.0, 1.0, 1.0];
    let meta = jxl_sdr_grade_metadata(HdrTransferFunction::Srgb, 1000.0);
    assert!(
        super::jxl_sdr_grade_fallback_rgba8(&rgba, HdrColorSpace::LinearSrgb, &meta).is_none(),
        "HDR (peak > 255 nits) must keep the tone-mapped path"
    );
}

#[cfg(feature = "jpegxl")]
#[test]
fn adobe_jxl_base_sdr_gain_map_uses_deferred_baseline_fallback_when_present() {
    let path = std::path::Path::new(
        r"F:\HDR\Gain_Map_Sample_Photos\Gain_Map_Sample_Photos\samples_jxl_base_sdr\01_base_sdr.jxl",
    );
    if !path.is_file() {
        eprintln!("skip {}", path.display());
        return;
    }

    let bytes = std::fs::read(path).expect("read jxl");
    let tone = crate::hdr::types::HdrToneMapSettings::default();
    let got = super::decode_jxl_bytes_to_image_data(&bytes, tone.target_hdr_capacity(), 1.0, tone)
        .expect("decode jxl base_sdr gain map");

    let crate::loader::ImageData::Hdr { hdr, fallback } = got else {
        panic!("expected static HDR JXL gain-map image");
    };
    assert_eq!((hdr.width, hdr.height), (2400, 3000));
    assert_eq!((fallback.width, fallback.height), (2400, 3000));
    assert!(
        hdr.rgba_f32.is_empty(),
        "JXL base_sdr should defer ISO gain-map compose"
    );
    assert!(
        hdr.metadata
            .gain_map
            .as_ref()
            .and_then(|gain_map| gain_map.iso_deferred.as_ref())
            .is_some(),
        "JXL base_sdr gain-map path must carry ISO deferred planes"
    );
    assert_eq!(
        fallback.rgba().len(),
        (2400_usize * 3000_usize * 4),
        "fallback should use the deferred SDR baseline, not empty rgba_f32"
    );
}

#[cfg(feature = "jpegxl")]
#[test]
fn probe_conformance_animation_metadata_when_present() {
    for rel in [
        r"F:\HDR\conformance\testcases\animation_icos4d\input.jxl",
        r"F:\HDR\conformance\testcases\animation_newtons_cradle\input.jxl",
        r"F:\HDR\conformance\testcases\animation_spline\input.jxl",
    ] {
        let path = std::path::Path::new(rel);
        if !path.is_file() {
            eprintln!("skip {}", path.display());
            continue;
        }
        let bytes = std::fs::read(path).expect("read jxl");
        let tone = crate::hdr::types::HdrToneMapSettings::default();
        let got = super::decode_jxl_bytes_to_image_data(
            &bytes,
            tone.target_hdr_capacity(),
            tone.target_hdr_capacity(),
            tone,
        )
        .expect("decode");
        match got {
            crate::loader::ImageData::HdrAnimated(frames) => {
                let h = &frames[0].hdr;
                eprintln!(
                    "{} -> HdrAnimated frames={} transfer={:?} peak={:?}",
                    path.file_name().unwrap().to_string_lossy(),
                    frames.len(),
                    h.metadata.transfer_function,
                    h.metadata.luminance.mastering_max_nits,
                );
            }
            crate::loader::ImageData::Animated(frames) => {
                eprintln!(
                    "{} -> Animated frames={} (SDR path)",
                    path.file_name().unwrap().to_string_lossy(),
                    frames.len()
                );
            }
            _ => eprintln!("{} -> other variant", path.display()),
        }
    }
}

#[cfg(feature = "jpegxl")]
#[test]
fn conformance_animation_icos4d_sdr_grade_has_no_rgb_on_fully_transparent_pixels() {
    let path = std::path::Path::new(r"F:\HDR\conformance\testcases\animation_icos4d\input.jxl");
    if !path.is_file() {
        return;
    }
    let bytes = std::fs::read(path).expect("read conformance jxl");
    let tone = crate::hdr::types::HdrToneMapSettings::default();
    let got = super::decode_jxl_bytes_to_image_data(
        &bytes,
        tone.target_hdr_capacity(),
        tone.target_hdr_capacity(),
        tone,
    )
    .expect("decode icos4d");
    let crate::loader::ImageData::HdrAnimated(frames) = got else {
        panic!("icos4d should decode as HdrAnimated on HDR display target capacity");
    };
    let frame = &frames[0];
    let mut leaked = 0_u32;
    for px in frame.hdr.rgba_f32.chunks_exact(4) {
        if px[3] <= 0.0 && (px[0].to_bits() | px[1].to_bits() | px[2].to_bits()) != 0 {
            leaked += 1;
        }
    }
    assert_eq!(
        leaked, 0,
        "fully transparent HDR float pixels must not carry RGB (alpha fringe)"
    );
}

#[cfg(feature = "jpegxl")]
#[test]
fn conformance_animation_icos4d_input_jxl_decodes_when_sample_present() {
    let path = std::path::Path::new(r"F:\HDR\conformance\testcases\animation_icos4d\input.jxl");
    if !path.is_file() {
        return;
    }
    let bytes = std::fs::read(path).expect("read conformance jxl");
    let tone = crate::hdr::types::HdrToneMapSettings::default();
    let got = super::decode_jxl_bytes_to_image_data(
        &bytes,
        tone.target_hdr_capacity(),
        tone.target_hdr_capacity(),
        tone,
    )
    .expect("decoded animation_icos4d");
    match got {
        crate::loader::ImageData::HdrAnimated(frames) => {
            assert!(frames.len() > 1, "expected multi-frame HDR animation");
            assert_eq!(
                frames[0].hdr.metadata.reference,
                HdrReference::DisplayReferred,
                "SDR-grade sRGB JXL animation should be display-referred for EV tone mapping"
            );
        }
        crate::loader::ImageData::Animated(_) => {
            panic!("icos4d must stay on HdrAnimated so EV adjustment works on HDR displays");
        }
        other => panic!(
            "expected ImageData::HdrAnimated, got {:?}",
            std::mem::discriminant(&other)
        ),
    }
}

#[cfg(feature = "jpegxl")]
#[test]
fn conformance_animation_newtons_cradle_input_jxl_decodes_when_sample_present() {
    let path =
        std::path::Path::new(r"F:\HDR\conformance\testcases\animation_newtons_cradle\input.jxl");
    if !path.is_file() {
        return;
    }
    let bytes = std::fs::read(path).expect("read conformance jxl");
    let tone = crate::hdr::types::HdrToneMapSettings::default();

    // Under SDR target capacity, it should decode to standard SDR Animated
    let got_sdr = super::decode_jxl_bytes_to_image_data(&bytes, 1.0, 1.0, tone)
        .expect("decoded animation_newtons_cradle SDR");
    assert!(
        matches!(got_sdr, crate::loader::ImageData::Animated(_)),
        "newtons_cradle should decode to SDR Animated on SDR target capacity"
    );

    // Under HDR target capacity, it should decode to HdrAnimated
    let got_hdr = super::decode_jxl_bytes_to_image_data(
        &bytes,
        tone.target_hdr_capacity(),
        tone.target_hdr_capacity(),
        tone,
    )
    .expect("decoded animation_newtons_cradle HDR");
    assert!(
        matches!(got_hdr, crate::loader::ImageData::HdrAnimated(_)),
        "newtons_cradle should decode to HdrAnimated on HDR target capacity"
    );
}

#[cfg(feature = "jpegxl")]
#[test]
fn conformance_animation_spline_input_jxl_decodes_when_sample_present() {
    let path = std::path::Path::new(r"F:\HDR\conformance\testcases\animation_spline\input.jxl");
    if !path.is_file() {
        return;
    }
    let bytes = std::fs::read(path).expect("read conformance jxl");
    let tone = crate::hdr::types::HdrToneMapSettings::default();

    // Under SDR target capacity, it should decode to standard SDR Animated
    let got_sdr = super::decode_jxl_bytes_to_image_data(&bytes, 1.0, 1.0, tone)
        .expect("decoded animation_spline SDR");
    assert!(
        matches!(got_sdr, crate::loader::ImageData::Animated(_)),
        "animation_spline should decode to SDR Animated on SDR target capacity"
    );

    // Under HDR target capacity, it should decode to HdrAnimated
    let got_hdr = super::decode_jxl_bytes_to_image_data(
        &bytes,
        tone.target_hdr_capacity(),
        tone.target_hdr_capacity(),
        tone,
    )
    .expect("decoded animation_spline HDR");
    assert!(
        matches!(got_hdr, crate::loader::ImageData::HdrAnimated(_)),
        "animation_spline should decode to HdrAnimated on HDR target capacity"
    );
}

#[cfg(feature = "jpegxl")]
#[test]
fn conformance_bench_oriented_brg_input_jxl_color_space_when_sample_present() {
    // libjxl HDR conformance: `bench_oriented_brg/input.jxl` --decoded pixels described by
    // `JXL_COLOR_PROFILE_TARGET_DATA` ICC (BT.709 primaries); see read_jxl_metadata order.
    let path = std::path::Path::new(r"F:\HDR\conformance\testcases\bench_oriented_brg\input.jxl");
    if !path.is_file() {
        return;
    }
    let bytes = std::fs::read(path).expect("read conformance jxl");
    let hdr = super::decode_jxl_hdr_bytes(&bytes).expect("decode conformance jxl");
    assert_eq!(
        hdr.color_space,
        HdrColorSpace::LinearSrgb,
        "expected linear sRGB (BT.709 primaries) for bench_oriented_brg ICC; metadata={:#?}",
        hdr.metadata
    );
    assert_eq!(
        hdr.metadata.transfer_function,
        HdrTransferFunction::Srgb,
        "bench_oriented_brg's libjxl float buffer is sRGB-encoded (the JPEG \
         reconstruction path keeps codestream values intact); the metadata must \
         reflect this so the SDR-grade fallback direct-quantizes (no second-pass OETF)"
    );
}

/// Diagnostic: actual libjxl float output range for `bench_oriented_brg/input.jxl`.
#[cfg(feature = "jpegxl")]
#[test]
fn conformance_bench_oriented_brg_float_pixel_range_when_sample_present() {
    let path = std::path::Path::new(r"F:\HDR\conformance\testcases\bench_oriented_brg\input.jxl");
    if !path.is_file() {
        return;
    }
    let bytes = std::fs::read(path).expect("read conformance jxl");
    let hdr = super::decode_jxl_hdr_bytes(&bytes).expect("decode conformance jxl");
    let mut mn = f32::INFINITY;
    let mut mx = f32::NEG_INFINITY;
    let mut sum = 0.0_f64;
    let mut count = 0_usize;
    for px in hdr.rgba_f32.chunks_exact(4) {
        for &c in &px[..3] {
            if c.is_finite() {
                mn = mn.min(c);
                mx = mx.max(c);
                sum += c as f64;
                count += 1;
            }
        }
    }
    let avg = sum / count.max(1) as f64;
    eprintln!(
        "bench_oriented_brg float RGB: min={mn:.4} max={mx:.4} avg={avg:.4} peak_nits={:?}",
        hdr.metadata.luminance.mastering_max_nits
    );
    assert!(mx.is_finite(), "max should be finite");
}

/// SDR fallback must not Reinhard-clamp almost everything to white (non-HDR monitor path).
#[cfg(feature = "jpegxl")]
#[test]
fn conformance_bench_oriented_brg_sdr_fallback_mean_not_washed_when_sample_present() {
    use crate::hdr::types::HdrToneMapSettings;
    let path = std::path::Path::new(r"F:\HDR\conformance\testcases\bench_oriented_brg\input.jxl");
    if !path.is_file() {
        return;
    }
    let bytes = std::fs::read(path).expect("read conformance jxl");
    let tone = HdrToneMapSettings::default();
    let img = crate::loader::apply_exif_orientation_to_image_data(
        path,
        super::decode_jxl_bytes_to_image_data(
            &bytes,
            tone.target_hdr_capacity(),
            tone.target_hdr_capacity(),
            tone,
        )
        .expect("decode"),
        Some(&bytes),
    );
    let crate::loader::ImageData::Hdr { fallback, .. } = img else {
        panic!("expected ImageData::Hdr");
    };
    let px = fallback.rgba();
    let mut sum = 0_u64;
    for c in px.chunks_exact(4) {
        sum += u64::from(c[0]) + u64::from(c[1]) + u64::from(c[2]);
    }
    let n = (px.len() / 4) as u64;
    let avg = (sum / (n * 3)) as u32;
    let mut darks = 0_u64;
    for c in px.chunks_exact(4) {
        if u32::from(c[0]) + u32::from(c[1]) + u32::from(c[2]) < 60 {
            darks += 1;
        }
    }
    // Reinhard-on-SDR collapses everything into a 153--78 mid band: mean ~180 and zero darks.
    // A correct sRGB encode keeps the mean lower and preserves shadow detail.
    assert!(
        avg < 200,
        "mean RGB {avg}/255 too high on SDR fallback (Reinhard wash-out)"
    );
    assert!(
        darks > 0,
        "no shadow pixels in SDR fallback 鈬?contrast collapsed"
    );
}

/// Pixel-level comparison between our SDR fallback and the conformance `ref.png`. They MUST
/// match closely (<=a few code values mean diff, mostly identical channels) --`ref.png` is the
/// libjxl conformance reference SDR rendering of `input.jxl`. Any larger drift means our
/// `jxl_sdr_grade_fallback_rgba8` is NOT producing what the reference says.
#[cfg(feature = "jpegxl")]
#[test]
fn conformance_bench_oriented_brg_sdr_fallback_matches_ref_png_when_sample_present() {
    use crate::hdr::types::HdrToneMapSettings;
    let jxl_path =
        std::path::Path::new(r"F:\HDR\conformance\testcases\bench_oriented_brg\input.jxl");
    let ref_path = std::path::Path::new(r"F:\HDR\conformance\testcases\bench_oriented_brg\ref.png");
    if !jxl_path.is_file() || !ref_path.is_file() {
        return;
    }
    let bytes = std::fs::read(jxl_path).expect("read conformance jxl");
    let tone = HdrToneMapSettings::default();
    let img = crate::loader::apply_exif_orientation_to_image_data(
        jxl_path,
        super::decode_jxl_bytes_to_image_data(
            &bytes,
            tone.target_hdr_capacity(),
            tone.target_hdr_capacity(),
            tone,
        )
        .expect("decode jxl"),
        Some(&bytes),
    );
    let crate::loader::ImageData::Hdr { fallback, hdr, .. } = img else {
        panic!("expected ImageData::Hdr");
    };
    let jxl_w = hdr.width as usize;
    let jxl_h = hdr.height as usize;
    let jxl_bytes = fallback.rgba().to_vec();

    let ref_img = image::open(ref_path).expect("decode ref.png").to_rgba8();
    let ref_w = ref_img.width() as usize;
    let ref_h = ref_img.height() as usize;
    assert_eq!(
        (jxl_w, jxl_h),
        (ref_w, ref_h),
        "ref.png dimensions {ref_w}x{ref_h} must match JXL fallback {jxl_w}x{jxl_h}"
    );
    let ref_bytes = ref_img.into_raw();
    assert_eq!(jxl_bytes.len(), ref_bytes.len());

    let n_pixels = (ref_bytes.len() / 4) as u64;
    let (mut sum_jxl_r, mut sum_jxl_g, mut sum_jxl_b) = (0_u64, 0_u64, 0_u64);
    let (mut sum_ref_r, mut sum_ref_g, mut sum_ref_b) = (0_u64, 0_u64, 0_u64);
    let (mut diff_r, mut diff_g, mut diff_b) = (0_i64, 0_i64, 0_i64);
    let (mut max_diff_r, mut max_diff_g, mut max_diff_b) = (0_u32, 0_u32, 0_u32);
    for (j, r) in jxl_bytes.chunks_exact(4).zip(ref_bytes.chunks_exact(4)) {
        sum_jxl_r += u64::from(j[0]);
        sum_jxl_g += u64::from(j[1]);
        sum_jxl_b += u64::from(j[2]);
        sum_ref_r += u64::from(r[0]);
        sum_ref_g += u64::from(r[1]);
        sum_ref_b += u64::from(r[2]);
        diff_r += i64::from(j[0]) - i64::from(r[0]);
        diff_g += i64::from(j[1]) - i64::from(r[1]);
        diff_b += i64::from(j[2]) - i64::from(r[2]);
        max_diff_r = max_diff_r.max((j[0] as i32 - r[0] as i32).unsigned_abs());
        max_diff_g = max_diff_g.max((j[1] as i32 - r[1] as i32).unsigned_abs());
        max_diff_b = max_diff_b.max((j[2] as i32 - r[2] as i32).unsigned_abs());
    }
    let avg_jxl_r = sum_jxl_r / n_pixels;
    let avg_jxl_g = sum_jxl_g / n_pixels;
    let avg_jxl_b = sum_jxl_b / n_pixels;
    let avg_ref_r = sum_ref_r / n_pixels;
    let avg_ref_g = sum_ref_g / n_pixels;
    let avg_ref_b = sum_ref_b / n_pixels;
    let bias_r = diff_r as f64 / n_pixels as f64;
    let bias_g = diff_g as f64 / n_pixels as f64;
    let bias_b = diff_b as f64 / n_pixels as f64;
    eprintln!(
        "bench_oriented_brg fallback vs ref.png:\n  \
         JXL avg RGB = ({avg_jxl_r}, {avg_jxl_g}, {avg_jxl_b})\n  \
         REF avg RGB = ({avg_ref_r}, {avg_ref_g}, {avg_ref_b})\n  \
         mean signed diff (jxl-ref) = ({bias_r:+.2}, {bias_g:+.2}, {bias_b:+.2})\n  \
         max abs diff = ({max_diff_r}, {max_diff_g}, {max_diff_b})"
    );
    // Tight: the conformance ref is the canonical libjxl decode; if our pipeline drifts more
    // than ~5 code values on average, it's a real bug (and the user sees washing on screen).
    assert!(
        bias_r.abs() < 5.0 && bias_g.abs() < 5.0 && bias_b.abs() < 5.0,
        "SDR fallback drifts from ref.png --bias=({bias_r:+.2}, {bias_g:+.2}, {bias_b:+.2}); \
         check linear/sRGB transfer handling and intensity_target scaling"
    );
}

/// Diagnostic: dump basic info + color encoding + extra channels for `cmyk_layers/input.jxl`
/// so we can see how libjxl describes the source. Symptom: rendered image is missing the
/// "black" word and shifts greens (lime instead of teal) compared to `ref.png`. Hypothesis:
/// the source is CMYK (3 color channels + black extra channel) and we drop the K plane when
/// requesting `JXL_TYPE_FLOAT` RGBA output.
#[cfg(feature = "jpegxl")]
#[test]
fn conformance_cmyk_layers_basic_info_and_channels_when_sample_present() {
    let path = std::path::Path::new(r"F:\HDR\conformance\testcases\cmyk_layers\input.jxl");
    if !path.is_file() {
        return;
    }
    let bytes = std::fs::read(path).expect("read cmyk_layers/input.jxl");

    unsafe {
        let decoder = libjxl_sys::JxlDecoderCreate(std::ptr::null());
        assert!(!decoder.is_null());
        let subscribed = libjxl_sys::JXL_DEC_BASIC_INFO
            | libjxl_sys::JXL_DEC_COLOR_ENCODING
            | libjxl_sys::JXL_DEC_FRAME;
        assert_eq!(
            libjxl_sys::JxlDecoderSubscribeEvents(decoder, subscribed),
            libjxl_sys::JXL_DEC_SUCCESS
        );
        assert_eq!(
            libjxl_sys::JxlDecoderSetInput(decoder, bytes.as_ptr(), bytes.len()),
            libjxl_sys::JXL_DEC_SUCCESS
        );
        libjxl_sys::JxlDecoderCloseInput(decoder);

        let mut info: libjxl_sys::JxlBasicInfo = std::mem::zeroed();
        let mut got_basic = false;
        loop {
            let st = libjxl_sys::JxlDecoderProcessInput(decoder);
            if st == libjxl_sys::JXL_DEC_BASIC_INFO {
                assert_eq!(
                    libjxl_sys::JxlDecoderGetBasicInfo(decoder, &mut info),
                    libjxl_sys::JXL_DEC_SUCCESS
                );
                got_basic = true;
            } else if st == libjxl_sys::JXL_DEC_COLOR_ENCODING {
                let mut color: libjxl_sys::JxlColorEncoding = std::mem::zeroed();
                let cs = libjxl_sys::JxlDecoderGetColorAsEncodedProfile(
                    decoder.cast_const(),
                    libjxl_sys::JXL_COLOR_PROFILE_TARGET_ORIGINAL,
                    &mut color,
                );
                eprintln!(
                    "TARGET_ORIGINAL color: status={cs} color_space={} primaries={} transfer={} rendering_intent={}",
                    color.color_space,
                    color.primaries,
                    color.transfer_function,
                    color.rendering_intent
                );
                let mut color_data: libjxl_sys::JxlColorEncoding = std::mem::zeroed();
                let ds = libjxl_sys::JxlDecoderGetColorAsEncodedProfile(
                    decoder.cast_const(),
                    libjxl_sys::JXL_COLOR_PROFILE_TARGET_DATA,
                    &mut color_data,
                );
                eprintln!(
                    "TARGET_DATA color: status={ds} color_space={} primaries={} transfer={}",
                    color_data.color_space, color_data.primaries, color_data.transfer_function
                );
                break;
            } else if st == libjxl_sys::JXL_DEC_ERROR || st == libjxl_sys::JXL_DEC_NEED_MORE_INPUT {
                panic!("libjxl process error/need-more-input: {st}");
            }
        }
        assert!(got_basic);
        eprintln!(
            "BasicInfo: xsize={} ysize={} bits_per_sample={} num_color_channels={} num_extra_channels={} alpha_bits={} have_animation={} intensity_target={}",
            info.xsize,
            info.ysize,
            info.bits_per_sample,
            info.num_color_channels,
            info.num_extra_channels,
            info.alpha_bits,
            info.have_animation,
            info.intensity_target
        );
        for i in 0..info.num_extra_channels {
            let mut ec: libjxl_sys::JxlExtraChannelInfo = std::mem::zeroed();
            let st = libjxl_sys::JxlDecoderGetExtraChannelInfo(
                decoder.cast_const(),
                i as usize,
                &mut ec,
            );
            if st != libjxl_sys::JXL_DEC_SUCCESS {
                eprintln!("extra channel #{i}: GetExtraChannelInfo status={st}");
                continue;
            }
            let mut name = vec![0u8; (ec.name_length as usize).max(1) + 1];
            let _ = libjxl_sys::JxlDecoderGetExtraChannelName(
                decoder.cast_const(),
                i as usize,
                name.as_mut_ptr().cast(),
                name.len(),
            );
            let name = std::ffi::CStr::from_ptr(name.as_ptr().cast())
                .to_string_lossy()
                .into_owned();
            eprintln!(
                "extra channel #{i}: type={} bits_per_sample={} name=\"{}\"",
                ec.type_, ec.bits_per_sample, name
            );
        }
        libjxl_sys::JxlDecoderDestroy(decoder);
    }
}

/// **Validate the lcms2-based CMYK->sRGB path** end-to-end on `cmyk_layers/input.jxl`.
///
/// Per libjxl PR #237, JPEG-recompressed CMYK files require external color management
/// (4-channel CMYK input ->3-channel sRGB output). libjxl's `JxlDecoderSetOutputColorProfile`
/// is a no-op for non-XYB sources even with a CMS attached.
///
/// Plumbing:
///   1. Decode RGBA float (CMY in RGB slots) + K extra channel (`JXL_CHANNEL_BLACK`).
///   2. Build an interleaved CMYK buffer, **inverting** values: libjxl uses
///      `0 = max ink, 1 = no ink` (per `cms_interface.h`); lcms2 `TYPE_CMYK_FLT` uses the
///      opposite (`0 = no ink, 1 = max ink`).
///   3. Apply the embedded CMYK ICC via `cmsCreateTransform(... LCMS_TYPE_CMYK_FLT, sRGB,
///      LCMS_TYPE_RGBA_FLT, INTENT_PERCEPTUAL, 0)`. Alpha rides as an "extra" channel.
///   4. Quantize to 8-bit and compare against `ref.png` --should match within ~+/-2 codes
///      per channel.
#[cfg(feature = "jpegxl")]
#[test]
fn conformance_cmyk_layers_cms_srgb_output_matches_ref_png_when_sample_present() {
    let jxl_path = std::path::Path::new(r"F:\HDR\conformance\testcases\cmyk_layers\input.jxl");
    let ref_path = std::path::Path::new(r"F:\HDR\conformance\testcases\cmyk_layers\ref.png");
    if !jxl_path.is_file() || !ref_path.is_file() {
        return;
    }
    let bytes = std::fs::read(jxl_path).expect("read cmyk_layers/input.jxl");

    let mut composed: Vec<u8> = Vec::new();
    let mut width = 0_u32;
    let mut height = 0_u32;
    let mut rgba_f32: Vec<f32> = Vec::new();
    let mut k_f32: Vec<f32> = Vec::new();
    let mut source_icc: Vec<u8> = Vec::new();
    unsafe {
        let decoder = libjxl_sys::JxlDecoderCreate(std::ptr::null());
        assert!(!decoder.is_null());

        let subscribed = libjxl_sys::JXL_DEC_BASIC_INFO
            | libjxl_sys::JXL_DEC_COLOR_ENCODING
            | libjxl_sys::JXL_DEC_FRAME
            | libjxl_sys::JXL_DEC_FULL_IMAGE;
        assert_eq!(
            libjxl_sys::JxlDecoderSubscribeEvents(decoder, subscribed),
            libjxl_sys::JXL_DEC_SUCCESS
        );
        assert_eq!(
            libjxl_sys::JxlDecoderSetInput(decoder, bytes.as_ptr(), bytes.len()),
            libjxl_sys::JXL_DEC_SUCCESS
        );
        libjxl_sys::JxlDecoderCloseInput(decoder);

        let pixel_format = libjxl_sys::JxlPixelFormat {
            num_channels: 4,
            data_type: libjxl_sys::JXL_TYPE_FLOAT,
            endianness: libjxl_sys::JXL_NATIVE_ENDIAN,
            align: 0,
        };
        let k_format = libjxl_sys::JxlPixelFormat {
            num_channels: 1,
            ..pixel_format
        };

        let mut info: libjxl_sys::JxlBasicInfo = std::mem::zeroed();
        let mut k_idx = None::<u32>;
        loop {
            let st = libjxl_sys::JxlDecoderProcessInput(decoder);
            if st == libjxl_sys::JXL_DEC_BASIC_INFO {
                assert_eq!(
                    libjxl_sys::JxlDecoderGetBasicInfo(decoder, &mut info),
                    libjxl_sys::JXL_DEC_SUCCESS
                );
                width = info.xsize;
                height = info.ysize;
                k_idx = super::jxl_find_black_extra_channel_index(decoder, &info);
                assert!(
                    k_idx.is_some(),
                    "expected a JXL_CHANNEL_BLACK extra channel"
                );
            } else if st == libjxl_sys::JXL_DEC_COLOR_ENCODING {
                let mut icc_size = 0_usize;
                assert_eq!(
                    libjxl_sys::JxlDecoderGetICCProfileSize(
                        decoder.cast_const(),
                        libjxl_sys::JXL_COLOR_PROFILE_TARGET_ORIGINAL,
                        &mut icc_size,
                    ),
                    libjxl_sys::JXL_DEC_SUCCESS
                );
                source_icc = vec![0u8; icc_size];
                assert_eq!(
                    libjxl_sys::JxlDecoderGetColorAsICCProfile(
                        decoder.cast_const(),
                        libjxl_sys::JXL_COLOR_PROFILE_TARGET_ORIGINAL,
                        source_icc.as_mut_ptr(),
                        icc_size,
                    ),
                    libjxl_sys::JXL_DEC_SUCCESS
                );
                eprintln!("source CMYK ICC: {} bytes", source_icc.len());
            } else if st == libjxl_sys::JXL_DEC_NEED_IMAGE_OUT_BUFFER {
                let mut size = 0_usize;
                assert_eq!(
                    libjxl_sys::JxlDecoderImageOutBufferSize(
                        decoder.cast_const(),
                        &pixel_format,
                        &mut size
                    ),
                    libjxl_sys::JXL_DEC_SUCCESS
                );
                rgba_f32.resize(size / std::mem::size_of::<f32>(), 0.0);
                assert_eq!(
                    libjxl_sys::JxlDecoderSetImageOutBuffer(
                        decoder,
                        &pixel_format,
                        rgba_f32.as_mut_ptr().cast(),
                        size
                    ),
                    libjxl_sys::JXL_DEC_SUCCESS
                );
                let idx = k_idx.expect("k channel index");
                let mut k_size = 0_usize;
                assert_eq!(
                    libjxl_sys::JxlDecoderExtraChannelBufferSize(
                        decoder.cast_const(),
                        &k_format,
                        &mut k_size,
                        idx,
                    ),
                    libjxl_sys::JXL_DEC_SUCCESS
                );
                k_f32.resize(k_size / std::mem::size_of::<f32>(), 0.0);
                assert_eq!(
                    libjxl_sys::JxlDecoderSetExtraChannelBuffer(
                        decoder,
                        &k_format,
                        k_f32.as_mut_ptr().cast(),
                        k_size,
                        idx,
                    ),
                    libjxl_sys::JXL_DEC_SUCCESS
                );
            } else if st == libjxl_sys::JXL_DEC_FULL_IMAGE {
                break;
            } else if st == libjxl_sys::JXL_DEC_ERROR || st == libjxl_sys::JXL_DEC_NEED_MORE_INPUT {
                panic!("libjxl process error/need-more-input: {st}");
            }
        }
        libjxl_sys::JxlDecoderDestroy(decoder);
    }

    // Build CMYK input following libjxl's `enc_color_management.cc` LCMS path
    // (the "0=white, 100=max ink" comment + `100 - 100 * v` line). lcms2's
    // `TYPE_CMYK_FLT` is encoded in **PostScript percent units** (0..100),
    // and libjxl's RGBA float output uses `0=max ink, 1=white` for CMYK
    // sources. Channel order is (C, M, Y) from RGB slots + K from the
    // BLACK extra channel (matching `CopyToT` in `enc_image_bundle.cc`).
    let n_pixels = (rgba_f32.len() / 4) as u32;
    assert_eq!(n_pixels as usize, k_f32.len());
    let mut cmyk_input = Vec::<f32>::with_capacity(n_pixels as usize * 4);
    let mut alpha = Vec::<f32>::with_capacity(n_pixels as usize);
    for (px, &k) in rgba_f32.chunks_exact(4).zip(k_f32.iter()) {
        cmyk_input.push(100.0 - 100.0 * px[0].clamp(0.0, 1.0));
        cmyk_input.push(100.0 - 100.0 * px[1].clamp(0.0, 1.0));
        cmyk_input.push(100.0 - 100.0 * px[2].clamp(0.0, 1.0));
        cmyk_input.push(100.0 - 100.0 * k.clamp(0.0, 1.0));
        alpha.push(px[3]);
    }

    let mut rgba_out = vec![0.0_f32; n_pixels as usize * 4];
    let in_profile =
        libjxl_sys::CmsProfile::open_from_mem(&source_icc).expect("lcms could not parse CMYK ICC");
    let out_profile =
        libjxl_sys::CmsProfile::new_srgb().expect("lcms could not build sRGB profile");
    // djxl converts CMYK->sRGB with the destination's rendering intent.
    // For its `ColorEncoding::SRGB(false)` target the default intent is
    // perceptual (matches `INTENT_PERCEPTUAL = 0`).
    let transform = libjxl_sys::CmsTransform::new(
        &in_profile,
        libjxl_sys::LCMS_TYPE_CMYK_FLT,
        &out_profile,
        libjxl_sys::LCMS_TYPE_RGBA_FLT,
        libjxl_sys::LCMS_INTENT_PERCEPTUAL,
        0,
    )
    .expect("lcms could not build CMYK->sRGB transform");
    transform.do_transform(
        cmyk_input.as_ptr().cast(),
        rgba_out.as_mut_ptr().cast(),
        n_pixels,
    );

    composed.reserve(n_pixels as usize * 4);
    for (i, px) in rgba_out.chunks_exact(4).enumerate() {
        composed.push(super::srgb_unit_to_u8(px[0]));
        composed.push(super::srgb_unit_to_u8(px[1]));
        composed.push(super::srgb_unit_to_u8(px[2]));
        composed.push(super::srgb_unit_to_u8(alpha[i]));
    }

    let ref_img = image::open(ref_path).expect("decode ref.png").to_rgba8();
    let ref_bytes_for_diag = ref_img.clone().into_raw();
    let pick = |bytes: &[u8], x: u32, y: u32| {
        let i = (y as usize * width as usize + x as usize) * 4;
        (bytes[i], bytes[i + 1], bytes[i + 2])
    };
    eprintln!(
        "lcms diagnostic samples (jxl vs ref.png):\n  black-area(135,14): jxl={:?} ref={:?}\n  bg(256,225):       jxl={:?} ref={:?}\n  bg(220,360):       jxl={:?} ref={:?}",
        pick(&composed, 135, 14),
        pick(&ref_bytes_for_diag, 135, 14),
        pick(&composed, 256, 225),
        pick(&ref_bytes_for_diag, 256, 225),
        pick(&composed, 220, 360),
        pick(&ref_bytes_for_diag, 220, 360),
    );

    assert_eq!((width, height), (ref_img.width(), ref_img.height()));
    let ref_bytes = ref_img.into_raw();
    assert_eq!(composed.len(), ref_bytes.len());
    let n = (composed.len() / 4) as i64;
    let (mut diff_r, mut diff_g, mut diff_b) = (0_i64, 0_i64, 0_i64);
    let (mut max_r, mut max_g, mut max_b) = (0_u32, 0_u32, 0_u32);
    for (j, r) in composed.chunks_exact(4).zip(ref_bytes.chunks_exact(4)) {
        diff_r += i64::from(j[0]) - i64::from(r[0]);
        diff_g += i64::from(j[1]) - i64::from(r[1]);
        diff_b += i64::from(j[2]) - i64::from(r[2]);
        max_r = max_r.max((j[0] as i32 - r[0] as i32).unsigned_abs());
        max_g = max_g.max((j[1] as i32 - r[1] as i32).unsigned_abs());
        max_b = max_b.max((j[2] as i32 - r[2] as i32).unsigned_abs());
    }
    let bias_r = diff_r as f64 / n as f64;
    let bias_g = diff_g as f64 / n as f64;
    let bias_b = diff_b as f64 / n as f64;
    eprintln!(
        "cmyk_layers (lcms2 CMYK->sRGB) vs ref.png:\n  mean signed diff = ({bias_r:+.2}, {bias_g:+.2}, {bias_b:+.2})\n  max abs diff = ({max_r}, {max_g}, {max_b})"
    );
    // ref.png was rendered by djxl with skcms; we use lcms2. Both should
    // produce the same colorimetric transform; small (<5 codes) bias is
    // tolerable due to differences in profile-internal LUT interpolation
    // and intent handling between the two CMSes.
    assert!(
        bias_r.abs() < 5.0 && bias_g.abs() < 5.0 && bias_b.abs() < 5.0,
        "lcms2 CMYK->sRGB drifts too far from ref.png: bias=({bias_r:+.2}, {bias_g:+.2}, {bias_b:+.2})"
    );
}

/// Historical diagnostic: dumps libjxl's CMYK output as raw RGB plus a few
/// hand-rolled compositing models (`R*K`, `R*(1-K)`, `min(R,K)`, etc.) and
/// reports the per-channel pixel diff against the conformance ref.png.
/// All such models are wrong without proper ICC-managed CMYK->sRGB
/// conversion (see PR #237 in libjxl). We retain the test as a debugging
/// aid --it documents how the old "guess the formula" approach misbehaves
/// across ink mixes --but the real fix lives in
/// `apply_cmyk_to_srgb_via_lcms`.
#[cfg(feature = "jpegxl")]
#[test]
fn conformance_cmyk_layers_naive_composition_models_are_all_wrong_when_sample_present() {
    let jxl_path = std::path::Path::new(r"F:\HDR\conformance\testcases\cmyk_layers\input.jxl");
    let ref_path = std::path::Path::new(r"F:\HDR\conformance\testcases\cmyk_layers\ref.png");
    if !jxl_path.is_file() || !ref_path.is_file() {
        return;
    }
    let bytes = std::fs::read(jxl_path).expect("read cmyk_layers/input.jxl");

    unsafe {
        let decoder = libjxl_sys::JxlDecoderCreate(std::ptr::null());
        assert!(!decoder.is_null());
        let subscribed = libjxl_sys::JXL_DEC_BASIC_INFO
            | libjxl_sys::JXL_DEC_COLOR_ENCODING
            | libjxl_sys::JXL_DEC_FRAME
            | libjxl_sys::JXL_DEC_FULL_IMAGE;
        assert_eq!(
            libjxl_sys::JxlDecoderSubscribeEvents(decoder, subscribed),
            libjxl_sys::JXL_DEC_SUCCESS
        );
        assert_eq!(
            libjxl_sys::JxlDecoderSetInput(decoder, bytes.as_ptr(), bytes.len()),
            libjxl_sys::JXL_DEC_SUCCESS
        );
        libjxl_sys::JxlDecoderCloseInput(decoder);

        let pixel_format = libjxl_sys::JxlPixelFormat {
            num_channels: 4,
            data_type: libjxl_sys::JXL_TYPE_FLOAT,
            endianness: libjxl_sys::JXL_NATIVE_ENDIAN,
            align: 0,
        };
        let k_format = libjxl_sys::JxlPixelFormat {
            num_channels: 1,
            ..pixel_format
        };

        let mut info: libjxl_sys::JxlBasicInfo = std::mem::zeroed();
        let mut rgba_f32: Vec<f32> = Vec::new();
        let mut k_f32: Vec<f32> = Vec::new();
        loop {
            let st = libjxl_sys::JxlDecoderProcessInput(decoder);
            if st == libjxl_sys::JXL_DEC_BASIC_INFO {
                assert_eq!(
                    libjxl_sys::JxlDecoderGetBasicInfo(decoder, &mut info),
                    libjxl_sys::JXL_DEC_SUCCESS
                );
            } else if st == libjxl_sys::JXL_DEC_NEED_IMAGE_OUT_BUFFER {
                let mut size = 0_usize;
                assert_eq!(
                    libjxl_sys::JxlDecoderImageOutBufferSize(
                        decoder.cast_const(),
                        &pixel_format,
                        &mut size
                    ),
                    libjxl_sys::JXL_DEC_SUCCESS
                );
                rgba_f32.resize(size / std::mem::size_of::<f32>(), 0.0);
                assert_eq!(
                    libjxl_sys::JxlDecoderSetImageOutBuffer(
                        decoder,
                        &pixel_format,
                        rgba_f32.as_mut_ptr().cast(),
                        size
                    ),
                    libjxl_sys::JXL_DEC_SUCCESS
                );
                // Channel 0 is type=BLACK (per the diagnostic above).
                let mut k_size = 0_usize;
                assert_eq!(
                    libjxl_sys::JxlDecoderExtraChannelBufferSize(
                        decoder.cast_const(),
                        &k_format,
                        &mut k_size,
                        0
                    ),
                    libjxl_sys::JXL_DEC_SUCCESS
                );
                k_f32.resize(k_size / std::mem::size_of::<f32>(), 0.0);
                assert_eq!(
                    libjxl_sys::JxlDecoderSetExtraChannelBuffer(
                        decoder,
                        &k_format,
                        k_f32.as_mut_ptr().cast(),
                        k_size,
                        0
                    ),
                    libjxl_sys::JXL_DEC_SUCCESS
                );
            } else if st == libjxl_sys::JXL_DEC_FULL_IMAGE {
                break;
            } else if st == libjxl_sys::JXL_DEC_ERROR || st == libjxl_sys::JXL_DEC_NEED_MORE_INPUT {
                panic!("libjxl process error/need-more-input: {st}");
            }
        }
        libjxl_sys::JxlDecoderDestroy(decoder);

        let n = (rgba_f32.len() / 4) as u64;
        let denom = n.max(1) as f64;

        // K stats --is it "0=no ink, 1=full ink" or "0=black, 1=white" (visible intensity)?
        let (mut k_min, mut k_max, mut k_sum) = (1.0_f32, 0.0_f32, 0.0_f64);
        for &k in &k_f32 {
            k_min = k_min.min(k);
            k_max = k_max.max(k);
            k_sum += k as f64;
        }
        eprintln!(
            "K channel: min={k_min:.4} max={k_max:.4} mean={:.4}",
            k_sum / denom
        );

        // Sample a few specific pixels at known regions: top of image (the "black" word
        // region) and middle (the "Background" green text region).
        let pick = |x: u32, y: u32| {
            let idx = (y * info.xsize + x) as usize;
            (
                rgba_f32[idx * 4],
                rgba_f32[idx * 4 + 1],
                rgba_f32[idx * 4 + 2],
                rgba_f32[idx * 4 + 3],
                k_f32[idx],
            )
        };
        // Approximate text positions on a 512x512 conformance test card.
        for (label, x, y) in [
            ("top-center  ", 256, 75),
            ("background  ", 256, 200),
            ("layer1      ", 256, 256),
            ("test-name   ", 256, 380),
            ("white-bg    ", 50, 50),
        ] {
            let (r, g, b, a, k) = pick(x, y);
            eprintln!("px ({label}) ({x:3}, {y:3}): R={r:.3} G={g:.3} B={b:.3} A={a:.3} K={k:.3}");
        }

        // Try several compositing models and report mean diff to ref.png.
        let ref_img = image::open(ref_path).expect("decode ref.png").to_rgba8();
        assert_eq!(
            (ref_img.width(), ref_img.height()),
            (info.xsize, info.ysize)
        );
        let ref_bytes = ref_img.into_raw();

        let try_compose = |name: &str, compose: fn(f32, f32, f32, f32) -> [f32; 3]| {
            let mut diff_r = 0_i64;
            let mut diff_g = 0_i64;
            let mut diff_b = 0_i64;
            let (mut max_r, mut max_g, mut max_b) = (0_u32, 0_u32, 0_u32);
            for (i, (px, k)) in rgba_f32.chunks_exact(4).zip(k_f32.iter()).enumerate() {
                let [r, g, b] = compose(px[0], px[1], px[2], *k);
                let r_u = super::srgb_unit_to_u8(r) as i32;
                let g_u = super::srgb_unit_to_u8(g) as i32;
                let b_u = super::srgb_unit_to_u8(b) as i32;
                let ref_r = ref_bytes[i * 4] as i32;
                let ref_g = ref_bytes[i * 4 + 1] as i32;
                let ref_b = ref_bytes[i * 4 + 2] as i32;
                diff_r += (r_u - ref_r) as i64;
                diff_g += (g_u - ref_g) as i64;
                diff_b += (b_u - ref_b) as i64;
                max_r = max_r.max((r_u - ref_r).unsigned_abs());
                max_g = max_g.max((g_u - ref_g).unsigned_abs());
                max_b = max_b.max((b_u - ref_b).unsigned_abs());
            }
            eprintln!(
                "{name}: bias=({:+.2}, {:+.2}, {:+.2}) max=({max_r}, {max_g}, {max_b})",
                diff_r as f64 / n as f64,
                diff_g as f64 / n as f64,
                diff_b as f64 / n as f64
            );
        };

        try_compose("RGB                    ", |r, g, b, _k| [r, g, b]);
        try_compose("RGB * (1 - K)          ", |r, g, b, k| {
            [r * (1.0 - k), g * (1.0 - k), b * (1.0 - k)]
        });
        try_compose("RGB * K                ", |r, g, b, k| {
            [r * k, g * k, b * k]
        });
        try_compose("min(RGB, K)            ", |r, g, b, k| {
            [r.min(k), g.min(k), b.min(k)]
        });
        try_compose("RGB - (1 - K)          ", |r, g, b, k| {
            [
                (r - (1.0 - k)).max(0.0),
                (g - (1.0 - k)).max(0.0),
                (b - (1.0 - k)).max(0.0),
            ]
        });

        // Find the 5 worst-mismatch pixels using the raw RGB output, dump (x, y, JXL, K, ref).
        let mut diffs: Vec<(u32, i64)> = (0..n as u32)
            .map(|i| {
                let j = i as usize;
                let dr = (super::srgb_unit_to_u8(rgba_f32[j * 4]) as i32 - ref_bytes[j * 4] as i32)
                    .abs();
                let dg = (super::srgb_unit_to_u8(rgba_f32[j * 4 + 1]) as i32
                    - ref_bytes[j * 4 + 1] as i32)
                    .abs();
                let db = (super::srgb_unit_to_u8(rgba_f32[j * 4 + 2]) as i32
                    - ref_bytes[j * 4 + 2] as i32)
                    .abs();
                (i, (dr + dg + db) as i64)
            })
            .collect();
        diffs.sort_by_key(|(_, d)| std::cmp::Reverse(*d));
        eprintln!("--- top 8 worst-mismatch pixels (raw RGB vs ref.png) ---");
        for &(i, d) in diffs.iter().take(8) {
            let x = i % info.xsize;
            let y = i / info.xsize;
            let j = i as usize;
            let r = rgba_f32[j * 4];
            let g = rgba_f32[j * 4 + 1];
            let b = rgba_f32[j * 4 + 2];
            let a = rgba_f32[j * 4 + 3];
            let k = k_f32[j];
            let rr = ref_bytes[j * 4];
            let rg = ref_bytes[j * 4 + 1];
            let rb = ref_bytes[j * 4 + 2];
            let ra = ref_bytes[j * 4 + 3];
            eprintln!(
                "({x:3},{y:3}) sum_diff={d:3}: \
                 JXL(R={r:.3} G={g:.3} B={b:.3} A={a:.3} K={k:.3}) \
                 ref(R={rr} G={rg} B={rb} A={ra})"
            );
        }
    }
}

/// End-to-end regression: the live decode pipeline now applies the embedded
/// CMYK ICC profile through lcms2 (`apply_cmyk_to_srgb_via_lcms`) when a
/// `JXL_CHANNEL_BLACK` extra channel is present. The resulting SDR fallback
/// for `cmyk_layers/input.jxl` must reproduce the conformance `ref.png`
/// (which djxl rendered with the same CMS pipeline) to within ~5 code
/// values mean signed diff. Without ICC-managed conversion the K plane is
/// dropped (missing "black" word) and process colors render flat (lime
/// instead of teal background).
#[cfg(feature = "jpegxl")]
#[test]
fn conformance_cmyk_layers_sdr_fallback_matches_ref_png_when_sample_present() {
    use crate::hdr::types::HdrToneMapSettings;
    let jxl_path = std::path::Path::new(r"F:\HDR\conformance\testcases\cmyk_layers\input.jxl");
    let ref_path = std::path::Path::new(r"F:\HDR\conformance\testcases\cmyk_layers\ref.png");
    if !jxl_path.is_file() || !ref_path.is_file() {
        return;
    }
    let bytes = std::fs::read(jxl_path).expect("read cmyk_layers/input.jxl");
    let tone = HdrToneMapSettings::default();
    let img = crate::loader::apply_exif_orientation_to_image_data(
        jxl_path,
        super::decode_jxl_bytes_to_image_data(
            &bytes,
            tone.target_hdr_capacity(),
            tone.target_hdr_capacity(),
            tone,
        )
        .expect("decode cmyk_layers"),
        Some(&bytes),
    );
    let crate::loader::ImageData::Hdr { fallback, hdr, .. } = img else {
        panic!("expected ImageData::Hdr");
    };
    let jxl_bytes = fallback.rgba().to_vec();
    let ref_img = image::open(ref_path).expect("decode ref.png").to_rgba8();
    assert_eq!(
        (hdr.width, hdr.height),
        (ref_img.width(), ref_img.height()),
        "ref.png dimensions must match cmyk_layers JXL"
    );
    let ref_bytes = ref_img.into_raw();
    assert_eq!(jxl_bytes.len(), ref_bytes.len());
    let n = (jxl_bytes.len() / 4) as i64;
    let (mut diff_r, mut diff_g, mut diff_b) = (0_i64, 0_i64, 0_i64);
    let (mut max_r, mut max_g, mut max_b) = (0_u32, 0_u32, 0_u32);
    for (j, r) in jxl_bytes.chunks_exact(4).zip(ref_bytes.chunks_exact(4)) {
        diff_r += i64::from(j[0]) - i64::from(r[0]);
        diff_g += i64::from(j[1]) - i64::from(r[1]);
        diff_b += i64::from(j[2]) - i64::from(r[2]);
        max_r = max_r.max((j[0] as i32 - r[0] as i32).unsigned_abs());
        max_g = max_g.max((j[1] as i32 - r[1] as i32).unsigned_abs());
        max_b = max_b.max((j[2] as i32 - r[2] as i32).unsigned_abs());
    }
    let bias_r = diff_r as f64 / n as f64;
    let bias_g = diff_g as f64 / n as f64;
    let bias_b = diff_b as f64 / n as f64;
    eprintln!(
        "cmyk_layers fallback vs ref.png:\n  mean signed diff = ({bias_r:+.2}, {bias_g:+.2}, {bias_b:+.2})\n  max abs diff = ({max_r}, {max_g}, {max_b})"
    );
    assert!(
        bias_r.abs() < 5.0 && bias_g.abs() < 5.0 && bias_b.abs() < 5.0,
        "lcms2 CMYK->sRGB SDR fallback bias too large: ({bias_r:+.2}, {bias_g:+.2}, {bias_b:+.2}) --\
         check JxlDecoderSetExtraChannelBuffer wiring + jxl_decoder_copy_target_original_icc + \
         apply_cmyk_to_srgb_via_lcms (libjxl CMYK convention 0=max ink, lcms2 0=no ink in 0..100)"
    );
}

#[cfg(feature = "jpegxl")]
#[test]
fn jxl_gain_map_bundle_rejects_malformed_payload() {
    let err = read_jxl_gain_map_bundle(&[0, 0, 1, 0]).expect_err("reject malformed jhgm");

    assert!(err.contains("jhgm"));
}

#[cfg(feature = "jpegxl")]
#[test]
fn jxl_gain_map_bundle_parses_metadata_and_embedded_codestream() {
    let metadata = [1_u8, 2, 3];
    let gain_map = [0xff_u8, 0x0a, 0x55];
    let mut bundle = Vec::new();
    bundle.push(0);
    bundle.extend_from_slice(&(metadata.len() as u16).to_be_bytes());
    bundle.extend_from_slice(&metadata);
    bundle.push(0); // no compressed color encoding
    bundle.extend_from_slice(&0_u32.to_be_bytes()); // no compressed ICC
    bundle.extend_from_slice(&gain_map);

    let parsed = read_jxl_gain_map_bundle(&bundle).expect("parse jhgm");

    assert_eq!(parsed.version, 0);
    assert_eq!(parsed.metadata, metadata);
    assert_eq!(parsed.gain_map, gain_map);
}

/// Conformance regression: `patches/input.jxl` ships TF=Linear in the
/// codestream (libjxl emits truly linear floats). Before fixing the SDR-
/// grade fallback to honor the actual TF reported by
/// `JxlDecoderGetColorAsEncodedProfile`, our renderer treated those linear
/// floats as if they were sRGB-encoded and quantized them directly,
/// producing a uniformly ~22-code darker image (mean signed diff
/// (-19.76, -22.22, -25.93), max diff 75) and effectively losing the gray
/// table grid lines that should be visible. After the fix every pixel
/// matches `ref.png` to within <= codes.
#[cfg(feature = "jpegxl")]
#[test]
fn conformance_patches_sdr_fallback_matches_ref_png_when_sample_present() {
    use crate::hdr::types::HdrToneMapSettings;
    let jxl_path = std::path::Path::new(r"F:\HDR\conformance\testcases\patches\input.jxl");
    let ref_path = std::path::Path::new(r"F:\HDR\conformance\testcases\patches\ref.png");
    if !jxl_path.is_file() || !ref_path.is_file() {
        return;
    }
    let bytes = std::fs::read(jxl_path).expect("read jxl");
    let img = crate::loader::apply_exif_orientation_to_image_data(
        jxl_path,
        super::decode_jxl_bytes_to_image_data(
            &bytes,
            HdrToneMapSettings::default().target_hdr_capacity(),
            HdrToneMapSettings::default().target_hdr_capacity(),
            HdrToneMapSettings::default(),
        )
        .expect("decode jxl"),
        Some(&bytes),
    );
    let crate::loader::ImageData::Hdr { fallback, hdr } = img else {
        panic!("unexpected ImageData variant");
    };
    assert_eq!(
        hdr.metadata.transfer_function,
        HdrTransferFunction::Linear,
        "patches.jxl ships TF=Linear in the codestream --read_jxl_metadata must surface that"
    );
    let ours = fallback.rgba();
    let ref_img = image::open(ref_path).expect("decode ref.png").to_rgba8();
    assert_eq!((hdr.width, hdr.height), (ref_img.width(), ref_img.height()));
    let r = ref_img.into_raw();
    let n = (ours.len() / 4) as i64;
    let (mut sr, mut sg, mut sb) = (0_i64, 0_i64, 0_i64);
    let (mut mr, mut mg, mut mb) = (0_u32, 0_u32, 0_u32);
    for (j, p) in ours.chunks_exact(4).zip(r.chunks_exact(4)) {
        let dr = j[0] as i32 - p[0] as i32;
        let dg = j[1] as i32 - p[1] as i32;
        let db = j[2] as i32 - p[2] as i32;
        sr += dr as i64;
        sg += dg as i64;
        sb += db as i64;
        mr = mr.max(dr.unsigned_abs());
        mg = mg.max(dg.unsigned_abs());
        mb = mb.max(db.unsigned_abs());
    }
    let bias = (
        sr as f64 / n as f64,
        sg as f64 / n as f64,
        sb as f64 / n as f64,
    );
    assert!(
        bias.0.abs() < 2.0 && bias.1.abs() < 2.0 && bias.2.abs() < 2.0,
        "mean signed diff vs ref.png must stay within +/-2 codes (was -19.76 / -22.22 / -25.93 \
         before treating TF=Linear codestream as truly linear); got {bias:?}"
    );
    assert!(
        mr <= 5 && mg <= 5 && mb <= 5,
        "max abs diff vs ref.png must stay within 5 codes (was 75 / 74 / 73 before the fix); \
         got ({mr}, {mg}, {mb})"
    );
}

/// Conformance regression: `patches_lossless/input.jxl`. Distinct from
/// `patches/input.jxl` in that the lossless variant ships a 2924-byte
/// **embedded ICC profile** (Display P3 primaries with a *linear* `rTRC`)
/// that libjxl can't represent as a `JxlColorEncoding` enum
/// (`JxlDecoderGetColorAsEncodedProfile` returns `JXL_DEC_ERROR` here).
/// libjxl emits truly linear floats per the codestream, so the SDR-grade
/// fallback must apply the sRGB OETF before quantizing. Before parsing
/// `rTRC`, we'd guess "non-sRGB primaries ->PQ" for any non-sRGB ICC and
/// route the linear floats through the HDR tone-mapping pipeline, which
/// produced a uniformly ~16-code darker image (mean diff
/// (-14.26, -16.16, -19.21), max 51) and washed out the gray table grid
/// lines plus cell background grays.
#[cfg(feature = "jpegxl")]
#[test]
fn conformance_patches_lossless_sdr_fallback_matches_ref_png_when_sample_present() {
    use crate::hdr::types::HdrToneMapSettings;
    let jxl_path = std::path::Path::new(r"F:\HDR\conformance\testcases\patches_lossless\input.jxl");
    let ref_path = std::path::Path::new(r"F:\HDR\conformance\testcases\patches_lossless\ref.png");
    if !jxl_path.is_file() || !ref_path.is_file() {
        return;
    }
    let bytes = std::fs::read(jxl_path).expect("read jxl");
    let img = crate::loader::apply_exif_orientation_to_image_data(
        jxl_path,
        super::decode_jxl_bytes_to_image_data(
            &bytes,
            HdrToneMapSettings::default().target_hdr_capacity(),
            HdrToneMapSettings::default().target_hdr_capacity(),
            HdrToneMapSettings::default(),
        )
        .expect("decode jxl"),
        Some(&bytes),
    );
    let crate::loader::ImageData::Hdr { fallback, hdr } = img else {
        panic!("unexpected ImageData variant");
    };
    // The metadata TF is whatever the rTRC parser decides --it can be
    // `Srgb` (piecewise / parametric / LUT curve) or `Linear` (count=0 or
    // count=1@1.0). Either way the SDR-grade fallback must produce bytes
    // that match ref.png; the previous bug was routing through the HDR
    // tone-mapping pipeline (because the old code guessed PQ for any non-
    // sRGB primary ICC).
    assert!(
        !matches!(
            hdr.metadata.transfer_function,
            HdrTransferFunction::Pq | HdrTransferFunction::Hlg
        ),
        "patches_lossless is SDR --must not route through the HDR pipeline; \
         got transfer_function={:?}",
        hdr.metadata.transfer_function
    );
    let ours = fallback.rgba();
    let ref_img = image::open(ref_path).expect("decode ref.png").to_rgba8();
    assert_eq!((hdr.width, hdr.height), (ref_img.width(), ref_img.height()));
    let r = ref_img.into_raw();
    let n = (ours.len() / 4) as i64;
    let (mut sr, mut sg, mut sb) = (0_i64, 0_i64, 0_i64);
    let (mut mr, mut mg, mut mb) = (0_u32, 0_u32, 0_u32);
    for (j, p) in ours.chunks_exact(4).zip(r.chunks_exact(4)) {
        let dr = j[0] as i32 - p[0] as i32;
        let dg = j[1] as i32 - p[1] as i32;
        let db = j[2] as i32 - p[2] as i32;
        sr += dr as i64;
        sg += dg as i64;
        sb += db as i64;
        mr = mr.max(dr.unsigned_abs());
        mg = mg.max(dg.unsigned_abs());
        mb = mb.max(db.unsigned_abs());
    }
    let bias = (
        sr as f64 / n as f64,
        sg as f64 / n as f64,
        sb as f64 / n as f64,
    );
    assert!(
        bias.0.abs() < 2.0 && bias.1.abs() < 2.0 && bias.2.abs() < 2.0,
        "mean signed diff vs ref.png must stay within +/-2 codes (was -14.26 / -16.16 / -19.21 \
         before parsing rTRC); got {bias:?}"
    );
    assert!(
        mr <= 5 && mg <= 5 && mb <= 5,
        "max abs diff vs ref.png must stay within 5 codes (was 51 / 49 / 49 before the fix); \
         got ({mr}, {mg}, {mb})"
    );
}

/// Unit coverage for the ICC `rTRC` classifier: synthetic `curveType`
/// payloads exercise the linear (count=0, count=1@1.0), gamma
/// (count=1@2.2), and LUT (count>=2) branches.
#[cfg(feature = "jpegxl")]
#[test]
fn icc_trc_kind_classifies_linear_gamma_and_lut() {
    // Build a minimal MOCK_ICC_PROFILE_SIZE-byte ICC profile with a single rTRC tag at a
    // known offset. We don't need a valid header --`icc_find_tag_element_offset`
    // only reads the tag-table at offset 128.
    fn make(count: u32, payload_after_count: &[u8]) -> Vec<u8> {
        let trc_offset = 256_u32;
        let mut icc = vec![0u8; crate::constants::MOCK_ICC_PROFILE_SIZE];
        icc[128..132].copy_from_slice(&1_u32.to_be_bytes()); // tag_count
        icc[132..136].copy_from_slice(b"rTRC");
        icc[136..140].copy_from_slice(&trc_offset.to_be_bytes());
        // size (unused by the parser but spec-correct):
        let size = (12 + payload_after_count.len()) as u32;
        icc[140..144].copy_from_slice(&size.to_be_bytes());
        let off = trc_offset as usize;
        icc[off..off + 4].copy_from_slice(b"curv");
        icc[off + 4..off + 8].fill(0); // reserved
        icc[off + 8..off + 12].copy_from_slice(&count.to_be_bytes());
        icc[off + 12..off + 12 + payload_after_count.len()].copy_from_slice(payload_after_count);
        icc
    }

    let linear_zero = make(0, &[]);
    assert_eq!(
        super::icc_trc_kind(&linear_zero),
        Some(HdrTransferFunction::Linear)
    );

    let linear_one = make(1, &[0x01, 0x00]); // u8.8 = 1.0
    assert_eq!(
        super::icc_trc_kind(&linear_one),
        Some(HdrTransferFunction::Linear)
    );

    let gamma_22 = make(1, &[0x02, 0x33]); // u8.8 <=2.2
    assert_eq!(
        super::icc_trc_kind(&gamma_22),
        Some(HdrTransferFunction::Gamma)
    );

    let lut = make(1024, &[0u8; 2048]);
    assert_eq!(super::icc_trc_kind(&lut), Some(HdrTransferFunction::Srgb));
}

/// Conformance regression: `blendmodes/input.jxl` (12-bit Modular, multiple
/// blend modes, TF=sRGB codestream). The float buffer libjxl gives us is
/// already sRGB-encoded; the SDR-grade fallback must direct-quantize
/// (`value * 255`) without re-applying the OETF. The blend-mode arithmetic
/// libjxl uses for partially-transparent / HDR-alpha (>1.0) pixels can
/// disagree with the reference compositor by up to ~90 codes on the
/// diagonal-stripe regions, so we lock the global statistics rather than
/// pixel-exact equality. Any future regression that accidentally re-applies
/// the OETF or routes through the HDR pipeline will blow these bounds.
#[cfg(feature = "jpegxl")]
#[test]
fn conformance_blendmodes_sdr_fallback_close_to_ref_png_when_sample_present() {
    use crate::hdr::types::HdrToneMapSettings;
    let jxl_path = std::path::Path::new(r"F:\HDR\conformance\testcases\blendmodes\input.jxl");
    let ref_path = std::path::Path::new(r"F:\HDR\conformance\testcases\blendmodes\ref.png");
    if !jxl_path.is_file() || !ref_path.is_file() {
        return;
    }
    let bytes = std::fs::read(jxl_path).expect("read jxl");
    let img = crate::loader::apply_exif_orientation_to_image_data(
        jxl_path,
        super::decode_jxl_bytes_to_image_data(
            &bytes,
            HdrToneMapSettings::default().target_hdr_capacity(),
            HdrToneMapSettings::default().target_hdr_capacity(),
            HdrToneMapSettings::default(),
        )
        .expect("decode jxl"),
        Some(&bytes),
    );
    let crate::loader::ImageData::Hdr { fallback, hdr } = img else {
        panic!("unexpected ImageData variant");
    };
    assert_eq!(
        hdr.metadata.transfer_function,
        HdrTransferFunction::Srgb,
        "blendmodes.jxl ships TF=sRGB in the codestream --read_jxl_metadata must surface \
         that so the SDR-grade fallback direct-quantizes without re-applying the OETF"
    );
    let ours = fallback.rgba();
    let ref_img = image::open(ref_path).expect("decode ref.png").to_rgba8();
    assert_eq!((hdr.width, hdr.height), (ref_img.width(), ref_img.height()));
    let r = ref_img.into_raw();
    let total = (ours.len() / 4) as f64;
    let (mut sr, mut sg, mut sb) = (0_i64, 0_i64, 0_i64);
    let mut exact = 0_u32;
    let mut close_15 = 0_u32;
    for (j, p) in ours.chunks_exact(4).zip(r.chunks_exact(4)) {
        let dr = j[0] as i32 - p[0] as i32;
        let dg = j[1] as i32 - p[1] as i32;
        let db = j[2] as i32 - p[2] as i32;
        sr += dr as i64;
        sg += dg as i64;
        sb += db as i64;
        let m = dr
            .unsigned_abs()
            .max(dg.unsigned_abs())
            .max(db.unsigned_abs());
        if m == 0 {
            exact += 1;
        }
        if m <= 15 {
            close_15 += 1;
        }
    }
    let bias = (sr as f64 / total, sg as f64 / total, sb as f64 / total);
    assert!(
        bias.0.abs() < 5.0 && bias.1.abs() < 5.0 && bias.2.abs() < 5.0,
        "global mean RGB bias vs ref.png must stay within +/-5 codes (we currently sit at \
         ~+1.55, -2.76, -0.75); got {bias:?}"
    );
    let exact_pct = exact as f64 / total;
    let close_15_pct = close_15 as f64 / total;
    assert!(
        exact_pct >= 0.30,
        "Icc core tests: at least 30% of pixels must match ref.png exactly (currently ~37%); got {:.1}%",
        exact_pct * 100.0
    );
    assert!(
        close_15_pct >= 0.55,
        "at least 55% of pixels must be within 15 codes of ref.png; got {:.1}%",
        close_15_pct * 100.0
    );
}

/// Diagnostic harness for investigating SDR-fallback regressions on JXL
/// conformance files. For a given (jxl, ref_png) pair: dumps `JxlBasicInfo`
/// plus extra-channel info, runs the live decode pipeline, and reports per-
/// channel bias / max-abs / pixel histogram of differences vs ref.png so
/// we can localize where rendering goes wrong (gamma, primaries, blend
/// modes, patches, ...). Skipped silently when the conformance corpus is
/// not present on the host.
#[cfg(feature = "jpegxl")]
fn diagnose_conformance_pair(name: &str, jxl_path: &std::path::Path, ref_path: &std::path::Path) {
    use crate::hdr::types::HdrToneMapSettings;
    if !jxl_path.is_file() || !ref_path.is_file() {
        eprintln!("[{name}] skipped --conformance file not present");
        return;
    }
    let bytes = std::fs::read(jxl_path).expect("read jxl");
    unsafe {
        let decoder = libjxl_sys::JxlDecoderCreate(std::ptr::null());
        assert!(!decoder.is_null());
        let subscribed = libjxl_sys::JXL_DEC_BASIC_INFO | libjxl_sys::JXL_DEC_COLOR_ENCODING;
        libjxl_sys::JxlDecoderSubscribeEvents(decoder, subscribed);
        libjxl_sys::JxlDecoderSetInput(decoder, bytes.as_ptr(), bytes.len());
        libjxl_sys::JxlDecoderCloseInput(decoder);
        let mut info: libjxl_sys::JxlBasicInfo = std::mem::zeroed();
        loop {
            let st = libjxl_sys::JxlDecoderProcessInput(decoder);
            if st == libjxl_sys::JXL_DEC_BASIC_INFO {
                libjxl_sys::JxlDecoderGetBasicInfo(decoder, &mut info);
            } else if st == libjxl_sys::JXL_DEC_COLOR_ENCODING {
                eprintln!(
                    "[{name}] dims={}x{} bits={} float={} num_color={} num_extra={} have_anim={} intensity_target={} min_nits={} alpha_bits={}",
                    info.xsize,
                    info.ysize,
                    info.bits_per_sample,
                    info.exponent_bits_per_sample,
                    info.num_color_channels,
                    info.num_extra_channels,
                    info.have_animation,
                    info.intensity_target,
                    info.min_nits,
                    info.alpha_bits,
                );
                for i in 0..info.num_extra_channels {
                    let mut ec = std::mem::MaybeUninit::<libjxl_sys::JxlExtraChannelInfo>::zeroed();
                    if libjxl_sys::JxlDecoderGetExtraChannelInfo(
                        decoder.cast_const(),
                        i as usize,
                        ec.as_mut_ptr(),
                    ) == libjxl_sys::JXL_DEC_SUCCESS
                    {
                        let ec = ec.assume_init();
                        eprintln!(
                            "[{name}]   extra channel #{i}: type={} bits={}",
                            ec.type_, ec.bits_per_sample,
                        );
                    }
                }
                let mut orig_size = 0_usize;
                libjxl_sys::JxlDecoderGetICCProfileSize(
                    decoder.cast_const(),
                    libjxl_sys::JXL_COLOR_PROFILE_TARGET_ORIGINAL,
                    &mut orig_size,
                );
                let mut data_size = 0_usize;
                libjxl_sys::JxlDecoderGetICCProfileSize(
                    decoder.cast_const(),
                    libjxl_sys::JXL_COLOR_PROFILE_TARGET_DATA,
                    &mut data_size,
                );
                let mut enc: libjxl_sys::JxlColorEncoding = std::mem::zeroed();
                let enc_st = libjxl_sys::JxlDecoderGetColorAsEncodedProfile(
                    decoder.cast_const(),
                    libjxl_sys::JXL_COLOR_PROFILE_TARGET_DATA,
                    &mut enc,
                );
                eprintln!(
                    "[{name}] icc_orig={} icc_data={} encoded_st={} (cs={} wp={} prim={} tf={} ri={})",
                    orig_size,
                    data_size,
                    enc_st,
                    enc.color_space,
                    enc.white_point,
                    enc.primaries,
                    enc.transfer_function,
                    enc.rendering_intent
                );
                break;
            } else if st == libjxl_sys::JXL_DEC_ERROR || st == libjxl_sys::JXL_DEC_NEED_MORE_INPUT {
                break;
            }
        }
        libjxl_sys::JxlDecoderDestroy(decoder);
    }

    let tone = HdrToneMapSettings::default();
    let img = match super::decode_jxl_bytes_to_image_data(
        &bytes,
        tone.target_hdr_capacity(),
        tone.target_hdr_capacity(),
        tone,
    ) {
        Ok(img) => crate::loader::apply_exif_orientation_to_image_data(jxl_path, img, Some(&bytes)),
        Err(e) => {
            eprintln!("[{name}] decode failed: {e}");
            return;
        }
    };
    let crate::loader::ImageData::Hdr { fallback, hdr, .. } = img else {
        eprintln!("[{name}] unexpected ImageData variant");
        return;
    };
    let jxl_bytes = fallback.rgba().to_vec();
    let ref_img = image::open(ref_path).expect("decode ref.png").to_rgba8();
    if (hdr.width, hdr.height) != (ref_img.width(), ref_img.height()) {
        eprintln!(
            "[{name}] dim mismatch jxl={}x{} ref={}x{}",
            hdr.width,
            hdr.height,
            ref_img.width(),
            ref_img.height()
        );
        return;
    }
    let ref_bytes = ref_img.into_raw();
    let n = (jxl_bytes.len() / 4) as i64;
    let (mut dr, mut dg, mut db, mut da) = (0_i64, 0_i64, 0_i64, 0_i64);
    let (mut mr, mut mg, mut mb, mut ma) = (0_u32, 0_u32, 0_u32, 0_u32);
    let mut buckets = [0_u32; 8]; // 0,1-3,4-7,8-15,16-31,32-63,64-127,128+
    for (j, r) in jxl_bytes.chunks_exact(4).zip(ref_bytes.chunks_exact(4)) {
        let cr = j[0] as i32 - r[0] as i32;
        let cg = j[1] as i32 - r[1] as i32;
        let cb = j[2] as i32 - r[2] as i32;
        let ca = j[3] as i32 - r[3] as i32;
        dr += cr as i64;
        dg += cg as i64;
        db += cb as i64;
        da += ca as i64;
        mr = mr.max(cr.unsigned_abs());
        mg = mg.max(cg.unsigned_abs());
        mb = mb.max(cb.unsigned_abs());
        ma = ma.max(ca.unsigned_abs());
        let max_abs = cr
            .unsigned_abs()
            .max(cg.unsigned_abs())
            .max(cb.unsigned_abs());
        let bin = match max_abs {
            0 => 0,
            1..=3 => 1,
            4..=7 => 2,
            8..=15 => 3,
            16..=31 => 4,
            32..=63 => 5,
            64..=127 => 6,
            _ => 7,
        };
        buckets[bin] += 1;
    }
    eprintln!(
        "[{name}] vs ref.png: bias=({:+.2},{:+.2},{:+.2},a:{:+.2}) max=({},{},{},a:{}) hist[==,1-3,4-7,8-15,16-31,32-63,64-127,>=128]={:?}",
        dr as f64 / n as f64,
        dg as f64 / n as f64,
        db as f64 / n as f64,
        da as f64 / n as f64,
        mr,
        mg,
        mb,
        ma,
        buckets,
    );
}

#[cfg(feature = "jpegxl")]
#[test]
fn diagnose_conformance_blendmodes_and_patches_when_sample_present() {
    // Kept as a hand-runnable diagnostic: prints per-channel bias / max-abs
    // / histogram for a handful of conformance pairs. Drop by name into
    // `cargo test -- --nocapture diagnose_conformance` when investigating
    // a new SDR-fallback regression.
    for case in [
        "bench_oriented_brg",
        "blendmodes",
        "patches",
        "cmyk_layers",
        "bike",
    ] {
        let jxl = std::path::Path::new(r"F:\HDR\conformance\testcases")
            .join(case)
            .join("input.jxl");
        let png = std::path::Path::new(r"F:\HDR\conformance\testcases")
            .join(case)
            .join("ref.png");
        diagnose_conformance_pair(case, &jxl, &png);
    }
}

/// For each conformance file, sample a few specific pixels that are NOT
/// pure black/white in `ref.png` and report:
/// - libjxl's raw float values out of `JxlDecoderProcessInput`
/// - what `srgb_unit_to_u8(v*255)` produces (direct quantize)
/// - what `linear_to_srgb_u8(v)` produces (apply sRGB OETF first)
/// - what `ref.png` actually has at that location
///
/// The encoding that matches ref.png tells us how libjxl emitted floats
/// for that bitstream --Modular-mode files with TF=Linear preserve linear
/// values, while sRGB-tagged Modular-mode files preserve sRGB-encoded
/// values, etc.
///
/// Count `JXL_DEC_FRAME` events fired by libjxl for `blendmodes/input.jxl`
/// --if it's >1 with `have_animation=0` we know the file ships multiple
/// blend-mode layers that libjxl coalesces; if our pipeline is somehow
/// giving back the un-coalesced last layer that explains why our SDR
/// fallback differs from `ref.png` on partially-transparent pixels.
#[cfg(feature = "jpegxl")]
#[test]
fn diagnose_blendmodes_frame_count_when_sample_present() {
    let path = std::path::Path::new(r"F:\HDR\conformance\testcases\blendmodes\input.jxl");
    if !path.is_file() {
        return;
    }
    let bytes = std::fs::read(path).expect("read");
    let mut frame_count = 0_u32;
    let mut full_image_count = 0_u32;
    unsafe {
        let decoder = libjxl_sys::JxlDecoderCreate(std::ptr::null());
        let subscribed = libjxl_sys::JXL_DEC_BASIC_INFO
            | libjxl_sys::JXL_DEC_COLOR_ENCODING
            | libjxl_sys::JXL_DEC_FRAME
            | libjxl_sys::JXL_DEC_FULL_IMAGE;
        libjxl_sys::JxlDecoderSubscribeEvents(decoder, subscribed);
        libjxl_sys::JxlDecoderSetInput(decoder, bytes.as_ptr(), bytes.len());
        libjxl_sys::JxlDecoderCloseInput(decoder);
        let pf = libjxl_sys::JxlPixelFormat {
            num_channels: 4,
            data_type: libjxl_sys::JXL_TYPE_FLOAT,
            endianness: libjxl_sys::JXL_NATIVE_ENDIAN,
            align: 0,
        };
        let mut buf: Vec<f32> = Vec::new();
        loop {
            let st = libjxl_sys::JxlDecoderProcessInput(decoder);
            if st == libjxl_sys::JXL_DEC_FRAME {
                frame_count += 1;
            } else if st == libjxl_sys::JXL_DEC_FULL_IMAGE {
                full_image_count += 1;
            } else if st == libjxl_sys::JXL_DEC_NEED_IMAGE_OUT_BUFFER {
                let mut size = 0_usize;
                libjxl_sys::JxlDecoderImageOutBufferSize(decoder.cast_const(), &pf, &mut size);
                buf.resize(size / 4, 0.0);
                libjxl_sys::JxlDecoderSetImageOutBuffer(
                    decoder,
                    &pf,
                    buf.as_mut_ptr().cast(),
                    size,
                );
            } else if st == libjxl_sys::JXL_DEC_SUCCESS
                || st == libjxl_sys::JXL_DEC_ERROR
                || st == libjxl_sys::JXL_DEC_NEED_MORE_INPUT
            {
                break;
            }
        }
        libjxl_sys::JxlDecoderDestroy(decoder);
    }
    eprintln!(
        "[blendmodes] JXL_DEC_FRAME fired {frame_count}x JXL_DEC_FULL_IMAGE fired {full_image_count}x"
    );
}

/// Hunt for pixels with the largest channel diff between our SDR fallback
/// bytes and `ref.png` for `blendmodes/input.jxl` and dump float plus alpha
/// and neighbours so we can identify whether the discrepancy is a clamp,
/// alpha-compositing, or layer blend bug.
#[cfg(feature = "jpegxl")]
#[test]
fn diagnose_blendmodes_worst_pixels_when_sample_present() {
    use crate::hdr::types::HdrToneMapSettings;
    let jxl_path = std::path::Path::new(r"F:\HDR\conformance\testcases\blendmodes\input.jxl");
    let ref_path = std::path::Path::new(r"F:\HDR\conformance\testcases\blendmodes\ref.png");
    if !jxl_path.is_file() || !ref_path.is_file() {
        return;
    }
    let bytes = std::fs::read(jxl_path).expect("read jxl");
    let img = crate::loader::apply_exif_orientation_to_image_data(
        jxl_path,
        super::decode_jxl_bytes_to_image_data(
            &bytes,
            HdrToneMapSettings::default().target_hdr_capacity(),
            HdrToneMapSettings::default().target_hdr_capacity(),
            HdrToneMapSettings::default(),
        )
        .expect("decode"),
        Some(&bytes),
    );
    let crate::loader::ImageData::Hdr { fallback, hdr, .. } = img else {
        return;
    };
    let our = fallback.rgba().to_vec();
    let r = image::open(ref_path).expect("ref").to_rgba8().into_raw();
    let w = hdr.width as usize;
    let mut worst = Vec::<(i32, usize, usize)>::new();
    for y in (0..hdr.height as usize).step_by(8) {
        for x in (0..w).step_by(8) {
            let i = (y * w + x) * 4;
            let dr = (our[i] as i32 - r[i] as i32).abs();
            let dg = (our[i + 1] as i32 - r[i + 1] as i32).abs();
            let db = (our[i + 2] as i32 - r[i + 2] as i32).abs();
            let m = dr.max(dg).max(db);
            if m >= 30 {
                worst.push((m, x, y));
            }
        }
    }
    worst.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    worst.truncate(10);
    for &(diff, x, y) in &worst {
        let i = (y * w + x) * 4;
        let f_i = (y * hdr.width as usize + x) * 4;
        let f = &hdr.rgba_f32[f_i..f_i + 4];
        eprintln!(
            "[worst] ({x:4},{y:4}) diff={diff} ours=({},{},{},a:{}) ref=({},{},{},a:{}) f32=({:.3},{:.3},{:.3},a:{:.3})",
            our[i],
            our[i + 1],
            our[i + 2],
            our[i + 3],
            r[i],
            r[i + 1],
            r[i + 2],
            r[i + 3],
            f[0],
            f[1],
            f[2],
            f[3]
        );
    }
}

#[cfg(feature = "jpegxl")]
#[test]
fn diagnose_jxl_float_buffer_encoding_when_samples_present() {
    for case in ["blendmodes", "patches", "bench_oriented_brg", "bike"] {
        let jxl_path = std::path::Path::new(r"F:\HDR\conformance\testcases")
            .join(case)
            .join("input.jxl");
        let ref_path = std::path::Path::new(r"F:\HDR\conformance\testcases")
            .join(case)
            .join("ref.png");
        if !jxl_path.is_file() || !ref_path.is_file() {
            continue;
        }
        let bytes = std::fs::read(&jxl_path).expect("read jxl");
        let ref_img = image::open(&ref_path).expect("decode ref.png").to_rgba8();
        let (rw, rh) = (ref_img.width(), ref_img.height());
        let ref_bytes = ref_img.into_raw();

        let mut rgba_f32: Vec<f32> = Vec::new();
        let mut width: u32 = 0;
        unsafe {
            let decoder = libjxl_sys::JxlDecoderCreate(std::ptr::null());
            let subscribed = libjxl_sys::JXL_DEC_BASIC_INFO
                | libjxl_sys::JXL_DEC_COLOR_ENCODING
                | libjxl_sys::JXL_DEC_FRAME
                | libjxl_sys::JXL_DEC_FULL_IMAGE;
            libjxl_sys::JxlDecoderSubscribeEvents(decoder, subscribed);
            libjxl_sys::JxlDecoderSetInput(decoder, bytes.as_ptr(), bytes.len());
            libjxl_sys::JxlDecoderCloseInput(decoder);
            let pixel_format = libjxl_sys::JxlPixelFormat {
                num_channels: 4,
                data_type: libjxl_sys::JXL_TYPE_FLOAT,
                endianness: libjxl_sys::JXL_NATIVE_ENDIAN,
                align: 0,
            };
            let mut info: libjxl_sys::JxlBasicInfo = std::mem::zeroed();
            loop {
                let st = libjxl_sys::JxlDecoderProcessInput(decoder);
                if st == libjxl_sys::JXL_DEC_BASIC_INFO {
                    libjxl_sys::JxlDecoderGetBasicInfo(decoder, &mut info);
                    width = info.xsize;
                } else if st == libjxl_sys::JXL_DEC_NEED_IMAGE_OUT_BUFFER {
                    let mut size = 0_usize;
                    libjxl_sys::JxlDecoderImageOutBufferSize(
                        decoder.cast_const(),
                        &pixel_format,
                        &mut size,
                    );
                    rgba_f32.resize(size / std::mem::size_of::<f32>(), 0.0);
                    libjxl_sys::JxlDecoderSetImageOutBuffer(
                        decoder,
                        &pixel_format,
                        rgba_f32.as_mut_ptr().cast(),
                        size,
                    );
                } else if st == libjxl_sys::JXL_DEC_FULL_IMAGE
                    || st == libjxl_sys::JXL_DEC_ERROR
                    || st == libjxl_sys::JXL_DEC_NEED_MORE_INPUT
                {
                    break;
                }
            }
            libjxl_sys::JxlDecoderDestroy(decoder);
        }
        if width == 0 || rgba_f32.is_empty() {
            continue;
        }
        // Pick 6 sample pixels evenly spaced
        let samples: [(u32, u32); 6] = [
            (rw / 8, rh / 8),
            (rw / 4, rh / 4),
            (rw / 2, rh / 4),
            (rw / 2, rh / 2),
            (rw * 3 / 4, rh / 2),
            (rw * 3 / 4, rh * 3 / 4),
        ];
        eprintln!("\n--- {case} ({rw}x{rh}) --float vs ref.png ---");
        for (x, y) in samples {
            let i = (y as usize * width as usize + x as usize) * 4;
            if i + 2 >= rgba_f32.len() {
                continue;
            }
            let r = rgba_f32[i];
            let g = rgba_f32[i + 1];
            let b = rgba_f32[i + 2];
            let direct = (
                super::srgb_unit_to_u8(r),
                super::srgb_unit_to_u8(g),
                super::srgb_unit_to_u8(b),
            );
            let linear_to_srgb = (
                super::linear_to_srgb_u8(r),
                super::linear_to_srgb_u8(g),
                super::linear_to_srgb_u8(b),
            );
            let ref_pix = (ref_bytes[i], ref_bytes[i + 1], ref_bytes[i + 2]);
            eprintln!(
                "  ({x:4},{y:4}) f32=({r:.3},{g:.3},{b:.3}) direct={direct:?} linear->srgb={linear_to_srgb:?} ref={ref_pix:?}"
            );
        }
    }
}
/// App primary load always passes `bootstrap_animation=true` for `.jxl`
/// (`loader/decode/mod.rs`). For still images that probe returns SUCCESS on
/// the first FULL_IMAGE; that early-return path must still run CMYK→sRGB via
/// lcms2. Without it, `cmyk_layers` shows missing "black" text and lime greens
/// while the non-bootstrap decode path looks correct.
#[cfg(feature = "jpegxl")]
#[test]
fn conformance_cmyk_layers_bootstrap_path_applies_cms_when_sample_present() {
    use crate::hdr::types::HdrToneMapSettings;
    let jxl_path = std::path::Path::new(r"F:\HDR\conformance\testcases\cmyk_layers\input.jxl");
    let ref_path = std::path::Path::new(r"F:\HDR\conformance\testcases\cmyk_layers\ref.png");
    if !jxl_path.is_file() || !ref_path.is_file() {
        return;
    }
    let bytes = std::fs::read(jxl_path).expect("read cmyk_layers/input.jxl");
    let tone = HdrToneMapSettings::default();
    let out =
        super::load_jxl_hdr_with_target_capacity_from_bytes(super::JxlHdrLoadFromBytesInput {
            path: jxl_path,
            bytes: &bytes,
            decode_target_hdr_capacity: tone.target_hdr_capacity(),
            display_hdr_target_capacity: tone.target_hdr_capacity(),
            tone_map: tone,
            bootstrap_animation: true, // matches app primary `.jxl` load
            try_embedded_sdr_master: false,
            cancel: None,
        })
        .expect("bootstrap decode cmyk_layers");
    assert!(
        !out.animation_remainder,
        "cmyk_layers is a still image; bootstrap must not schedule remainder"
    );
    let crate::loader::ImageData::Hdr { hdr, fallback, .. } = out.image else {
        panic!("expected ImageData::Hdr from bootstrap still decode");
    };
    assert_eq!(
        hdr.color_space,
        crate::hdr::types::HdrColorSpace::LinearSrgb,
        "after CMYK CMS, color_space must not stay Unknown (CMYK ICC)"
    );
    assert!(
        matches!(
            hdr.metadata.color_profile,
            crate::hdr::types::HdrColorProfile::Cicp {
                color_primaries: 1,
                ..
            }
        ),
        "after CMYK CMS, profile must be retagged to sRGB CICP"
    );
    // "black" text center (from converted decode bbox)
    let i = (47 * hdr.width as usize + 188) * 4;
    let f = hdr.rgba_f32.as_slice();
    let lum = 0.2126 * f[i] + 0.7152 * f[i + 1] + 0.0722 * f[i + 2];
    assert!(
        lum < 0.25,
        "bootstrap path must keep K ink so 'black' text is dark (lum={lum:.3}, rgb=({:.3},{:.3},{:.3}))",
        f[i],
        f[i + 1],
        f[i + 2]
    );
    // Teal "Background"/"layer" sample -- raw CMY is lime (~0.21,1.0,0.63)
    let ig = (190 * hdr.width as usize + 200) * 4;
    assert!(
        f[ig] < 0.05 && f[ig + 1] > 0.5 && f[ig + 1] < 0.85,
        "bootstrap path must CMS-convert process green to teal, not lime (rgb=({:.3},{:.3},{:.3}))",
        f[ig],
        f[ig + 1],
        f[ig + 2]
    );
    let jxl_bytes = fallback.rgba();
    let ref_img = image::open(ref_path).expect("decode ref.png").to_rgba8();
    let ref_bytes = ref_img.as_raw();
    assert_eq!(jxl_bytes.len(), ref_bytes.len());
    let n = (jxl_bytes.len() / 4) as i64;
    let (mut diff_r, mut diff_g, mut diff_b) = (0_i64, 0_i64, 0_i64);
    for (j, r) in jxl_bytes.chunks_exact(4).zip(ref_bytes.chunks_exact(4)) {
        diff_r += i64::from(j[0]) - i64::from(r[0]);
        diff_g += i64::from(j[1]) - i64::from(r[1]);
        diff_b += i64::from(j[2]) - i64::from(r[2]);
    }
    let bias_r = diff_r as f64 / n as f64;
    let bias_g = diff_g as f64 / n as f64;
    let bias_b = diff_b as f64 / n as f64;
    eprintln!(
        "cmyk_layers bootstrap fallback vs ref.png: bias=({bias_r:+.2}, {bias_g:+.2}, {bias_b:+.2})"
    );
    assert!(
        bias_r.abs() < 5.0 && bias_g.abs() < 5.0 && bias_b.abs() < 5.0,
        "bootstrap CMYK CMS bias too large: ({bias_r:+.2}, {bias_g:+.2}, {bias_b:+.2})"
    );
}
