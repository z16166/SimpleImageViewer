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
use crate::hdr::jpegxl::{
    JXL_TRANSFER_FUNCTION_HLG, JXL_TRANSFER_FUNCTION_LINEAR, JXL_TRANSFER_FUNCTION_PQ,
    JXL_TRANSFER_FUNCTION_SRGB, is_jxl_header, jxl_color_encoding_to_metadata,
};
#[cfg(feature = "jpegxl")]
use crate::hdr::types::HdrColorSpace;
use crate::hdr::types::{HdrImageMetadata, HdrReference, HdrTransferFunction};

#[test]
fn jxl_header_detection_accepts_codestream_and_container() {
    assert!(is_jxl_header(&[0xff, 0x0a, 0x00, 0x00]));
    assert!(is_jxl_header(&[
        0x00, 0x00, 0x00, 0x0c, b'J', b'X', b'L', b' ', 0x0d, 0x0a, 0x87, 0x0a,
    ]));
    assert!(!is_jxl_header(b"\x89PNG"));
}

#[test]
fn jxl_pq_metadata_is_display_referred_with_intensity_target() {
    let metadata = jxl_color_encoding_to_metadata(9, JXL_TRANSFER_FUNCTION_PQ, Some(4000.0));

    assert_eq!(metadata.transfer_function, HdrTransferFunction::Pq);
    assert_eq!(metadata.reference, HdrReference::DisplayReferred);
    assert_eq!(metadata.luminance.mastering_max_nits, Some(4000.0));
}

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
    // mode files: TF=sRGB → already-encoded floats. The fast path quantizes
    // them directly via `value * 255` (no second-pass OETF).
    let rgba = vec![1.0_f32, 0.5, 0.0, 1.0];
    let mut meta = HdrImageMetadata::default();
    meta.transfer_function = HdrTransferFunction::Srgb;
    meta.luminance.mastering_max_nits = Some(255.0);
    let px = super::jxl_sdr_grade_fallback_rgba8(&rgba, HdrColorSpace::LinearSrgb, &meta)
        .expect("sdr-grade content must use direct sRGB encode");
    assert_eq!(px[0], 255, "1.0 → 255, got {}", px[0]);
    assert!(
        (px[1] as i32 - 128).abs() <= 1,
        "0.5 → ~128 (direct quantize, no second-pass OETF), got {}",
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
    // Linear 0.5 → encoded ~0.735 → ~187 in 8-bit (not 128).
    let rgba = vec![1.0_f32, 0.5, 0.0, 1.0];
    let mut meta = HdrImageMetadata::default();
    meta.transfer_function = HdrTransferFunction::Linear;
    meta.luminance.mastering_max_nits = Some(255.0);
    let px = super::jxl_sdr_grade_fallback_rgba8(&rgba, HdrColorSpace::LinearSrgb, &meta)
        .expect("sdr-grade content must use the OETF + quantize path");
    assert_eq!(px[0], 255, "1.0 → 255, got {}", px[0]);
    assert!(
        (px[1] as i32 - 188).abs() <= 1,
        "linear 0.5 → encoded ~188 (sRGB OETF), got {}",
        px[1]
    );
    assert_eq!(px[2], 0);
    assert_eq!(px[3], 255);
}

#[cfg(feature = "jpegxl")]
#[test]
fn jxl_sdr_grade_srgb_tags_display_referred() {
    let mut meta = HdrImageMetadata::default();
    meta.transfer_function = HdrTransferFunction::Srgb;
    meta.luminance.mastering_max_nits = Some(255.0);
    super::jxl_tag_display_referred_when_sdr_grade(&mut meta);
    assert_eq!(meta.reference, HdrReference::DisplayReferred);
}

#[cfg(feature = "jpegxl")]
#[test]
fn jxl_sdr_grade_linear_does_not_tag_display_referred() {
    let mut meta = HdrImageMetadata::default();
    meta.transfer_function = HdrTransferFunction::Linear;
    meta.luminance.mastering_max_nits = Some(255.0);
    super::jxl_tag_display_referred_when_sdr_grade(&mut meta);
    assert_ne!(meta.reference, HdrReference::DisplayReferred);
}

#[cfg(feature = "jpegxl")]
#[test]
fn jxl_sdr_grade_fallback_skipped_for_high_peak_hdr() {
    let rgba = vec![1.0_f32, 1.0, 1.0, 1.0];
    let mut meta = HdrImageMetadata::default();
    meta.transfer_function = HdrTransferFunction::Srgb;
    meta.luminance.mastering_max_nits = Some(1000.0);
    assert!(
        super::jxl_sdr_grade_fallback_rgba8(&rgba, HdrColorSpace::LinearSrgb, &meta).is_none(),
        "HDR (peak > 255 nits) must keep the tone-mapped path"
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
    // libjxl HDR conformance: `bench_oriented_brg/input.jxl` — decoded pixels described by
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
    // Reinhard-on-SDR collapses everything into a 153–178 mid band: mean ~180 and zero darks.
    // A correct sRGB encode keeps the mean lower and preserves shadow detail.
    assert!(
        avg < 200,
        "mean RGB {avg}/255 too high on SDR fallback (Reinhard wash-out)"
    );
    assert!(
        darks > 0,
        "no shadow pixels in SDR fallback ⇒ contrast collapsed"
    );
}

/// Pixel-level comparison between our SDR fallback and the conformance `ref.png`. They MUST
/// match closely (≤ a few code values mean diff, mostly identical channels) — `ref.png` is the
/// libjxl conformance reference SDR rendering of `input.jxl`. Any larger drift means our
/// `jxl_sdr_grade_fallback_rgba8` is NOT producing what the reference says.
#[cfg(feature = "jpegxl")]
#[test]
