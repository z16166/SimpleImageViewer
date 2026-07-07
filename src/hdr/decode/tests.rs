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
use crate::hdr::decode::exr::decode_exr_display_image;
use crate::hdr::types::{
    HdrColorProfile, HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat, HdrReference,
    HdrToneMapSettings, HdrTransferFunction,
};
use std::path::PathBuf;
use std::sync::Arc;

fn openexr_images_root() -> Option<PathBuf> {
    std::env::var_os("SIV_OPENEXR_IMAGES_DIR")
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from(r"F:\HDR\openexr-images")))
        .filter(|path| path.is_dir())
}

#[test]
fn srgb_transfer_sdr_fallback_uses_piecewise_srgb_curve_not_reinhard() {
    let mut meta = HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb);
    meta.transfer_function = HdrTransferFunction::Srgb;
    meta.reference = HdrReference::DisplayReferred;

    let buffer = HdrImageBuffer {
        width: 1,
        height: 1,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::LinearSrgb,
        metadata: meta,
        // Non-linear sRGB code ~0.5 → linear luminance (~0.214)
        rgba_f32: Arc::new(vec![0.5_f32, 0.5_f32, 0.5_f32, 1.0]),
    };

    let sdr = hdr_to_sdr_rgba8(&buffer, 0.0).expect("sdr fallback");
    let expected_lin = super::srgb_nonlinear_channel_to_linear(0.5);
    let e0 = super::linear_srgb_linear_to_srgb_u8(expected_lin);
    assert_eq!(sdr, vec![e0, e0, e0, 255]);
}

#[test]
fn pq_bt709_primaries_sdr_fallback_matches_reinhard_encode_path() {
    use crate::hdr::cicp::{self, H273_TRANSFER_SMPTE_ST2084_FOR_PQ};

    let tone = HdrToneMapSettings::default();
    let meta709 = cicp::cicp_to_metadata(1, H273_TRANSFER_SMPTE_ST2084_FOR_PQ, 1, true, None);
    assert_eq!(meta709.transfer_function, HdrTransferFunction::Pq);

    let buffer = HdrImageBuffer {
        width: 1,
        height: 1,
        format: HdrPixelFormat::Rgba32Float,
        color_space: meta709.color_space_hint(),
        metadata: meta709.clone(),
        rgba_f32: Arc::new(vec![0.45_f32, 0.42_f32, 0.50_f32, 1.0]),
    };

    let sdr = hdr_to_sdr_rgba8_with_tone_settings(&buffer, 0.0, &tone).expect("sdr pq bt709");

    let tf = buffer.metadata.transfer_function;
    assert_eq!(tf, HdrTransferFunction::Pq);
    let peak_scale = tone.sdr_white_nits / tone.max_display_nits.max(tone.sdr_white_nits);
    let decoded =
        decode_transfer_to_display_linear([0.45_f32, 0.42_f32, 0.50_f32], tf, tone.sdr_white_nits);
    let linear_srgb = linear_primary_to_linear_srgb(decoded, buffer.color_space, &meta709);
    let expected = encode_sdr_rgb8(linear_srgb, 1.0_f32, peak_scale);
    assert_eq!(sdr, vec![expected[0], expected[1], expected[2], 255]);

    let meta2020 = cicp::cicp_to_metadata(9, H273_TRANSFER_SMPTE_ST2084_FOR_PQ, 9, true, None);
    let buffer2020 = HdrImageBuffer {
        color_space: meta2020.color_space_hint(),
        metadata: meta2020,
        ..buffer.clone()
    };
    let pq2020 = hdr_to_sdr_rgba8_with_tone_settings(&buffer2020, 0.0, &tone).expect("sdr pq2020");
    assert_ne!(
        &sdr[..3],
        &pq2020[..3],
        "PQ+Rec709 vs PQ+Rec2100 mastering should diverge via different gamut matrices"
    );
}

/// PQ display-linear values above nominal SDR white must not follow the unmanaged **IEC** path
/// (hard clamp-to-1 before piecewise encode), which merges distinct highlight codes.
#[test]
fn pq_sdr_fallback_highlights_grade_with_reinhard_not_iec_hard_clip() {
    use crate::hdr::cicp::{self, H273_TRANSFER_SMPTE_ST2084_FOR_PQ};

    // Use a **smaller** peak scaler than default so PQ‑decoded linear is only moderately above
    // SDR white: IEC clamp+OETF pins both highlights to 255, while Reinhard still rolls off below white.
    let tone = HdrToneMapSettings {
        max_display_nits: 400.0,
        ..HdrToneMapSettings::default()
    };
    let peak_scale = tone.sdr_white_nits / tone.max_display_nits.max(tone.sdr_white_nits);

    let meta = cicp::cicp_to_metadata(1, H273_TRANSFER_SMPTE_ST2084_FOR_PQ, 1, true, None);
    let tf = HdrTransferFunction::Pq;

    fn decode_lin_r(tf: HdrTransferFunction, code_r: f32, sdr_white: f32) -> f32 {
        decode_transfer_to_display_linear([code_r, 0.0_f32, 0.0_f32], tf, sdr_white)[0]
    }

    let c0 = 0.72_f32;
    let c1 = 0.92_f32;
    let lin0 = decode_lin_r(tf, c0, tone.sdr_white_nits);
    let lin1 = decode_lin_r(tf, c1, tone.sdr_white_nits);
    assert!(
        lin0.is_finite()
            && lin1.is_finite()
            && lin1 > lin0 * 2.0
            && lin0 * peak_scale > 1.0_f32
            && lin1 * peak_scale > 1.0_f32,
        "fixture: two PQ reds that IEC clamps to white but Reinhard distinguishes; lin0={lin0} lin1={lin1} peak_scale={peak_scale}"
    );

    let iec_byte0 = encode_linear_display_referred_srgb8([lin0, 0., 0.], 1.0, peak_scale)[0];
    let iec_byte1 = encode_linear_display_referred_srgb8([lin1, 0., 0.], 1.0, peak_scale)[0];
    assert_eq!(
        iec_byte0, 255,
        "IEC path should hard-clip boosted linear red ≥1 to max code"
    );
    assert_eq!(
        iec_byte1, 255,
        "IEC fixture: second highlight should clip too"
    );

    let buffer = HdrImageBuffer {
        width: 2,
        height: 1,
        format: HdrPixelFormat::Rgba32Float,
        color_space: meta.color_space_hint(),
        metadata: meta,
        rgba_f32: Arc::new(vec![c0, 0.0, 0.0, 1.0, c1, 0.0, 0.0, 1.0]),
    };
    let sdr = hdr_to_sdr_rgba8_with_tone_settings(&buffer, 0.0, &tone).expect("two px");
    assert!(
        sdr[0] < 255 && sdr[4] < 255,
        "Reinhard path should roll off HDR peaks instead of pinning to 255: {:?}",
        sdr
    );
    assert_ne!(
        sdr[0], sdr[4],
        "PQ highlight gradation collapses under IEC clamp+OETF; Reinhard preserves separation"
    );
}

#[test]
fn bt709_inverse_transfer_differs_from_iec_srgb_at_same_encoded_value() {
    let v = 0.35_f32;
    let b = super::bt709_nonlinear_channel_to_linear(v);
    let s = super::srgb_nonlinear_channel_to_linear(v);
    assert!(
        (b - s).abs() > 0.002_f32,
        "BT.709 inverse OETF differs from IEC sRGB at encoded {v}: b={b} s={s}"
    );
}

#[test]
fn hdr_candidate_extensions_are_case_insensitive() {
    assert!(is_hdr_candidate_ext("exr"));
    assert!(is_hdr_candidate_ext("EXR"));
    assert!(is_hdr_candidate_ext("hdr"));
    assert!(is_hdr_candidate_ext("HdR"));
    assert!(!is_hdr_candidate_ext("png"));
    assert!(!is_hdr_candidate_ext(""));
}

#[test]
fn tone_map_preserves_alpha_and_maps_rgb_with_exposure() {
    let buffer = HdrImageBuffer {
        width: 2,
        height: 1,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::LinearSrgb,
        metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
        rgba_f32: Arc::new(vec![-1.0, 0.0, 1.0, 0.5, 4.0, 0.25, 0.5, 1.5]),
    };

    let sdr = hdr_to_sdr_rgba8(&buffer, 0.0).expect("tone map valid buffer");

    assert_eq!(sdr, vec![0, 0, 186, 128, 230, 123, 155, 255,]);
}

#[test]
fn tone_map_uses_exposure_ev_scale() {
    let buffer = HdrImageBuffer {
        width: 1,
        height: 1,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::LinearSrgb,
        metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
        rgba_f32: Arc::new(vec![0.25, 0.25, 0.25, 1.0]),
    };

    let sdr = hdr_to_sdr_rgba8(&buffer, 1.0).expect("tone map valid buffer");

    assert_eq!(sdr, vec![155, 155, 155, 255]);
}

#[test]
fn tone_map_sanitizes_non_finite_rgb_values() {
    let buffer = HdrImageBuffer {
        width: 2,
        height: 1,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::LinearSrgb,
        metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
        rgba_f32: Arc::new(vec![
            f32::NAN,
            f32::NEG_INFINITY,
            f32::INFINITY,
            1.0,
            0.5,
            f32::NAN,
            f32::INFINITY,
            -1.0,
        ]),
    };

    let sdr = hdr_to_sdr_rgba8(&buffer, 0.0).expect("tone map non-finite buffer");

    assert_eq!(sdr, vec![0, 0, 255, 255, 155, 0, 255, 0]);
}

#[test]
fn tone_map_extreme_finite_rgb_with_high_exposure_saturates() {
    let buffer = HdrImageBuffer {
        width: 1,
        height: 1,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::LinearSrgb,
        metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
        rgba_f32: Arc::new(vec![f32::MAX, f32::MAX, f32::MAX, 1.0]),
    };

    let sdr = hdr_to_sdr_rgba8(&buffer, 16.0).expect("tone map extreme finite buffer");

    assert_eq!(sdr, vec![255, 255, 255, 255]);
}

#[test]
fn pq_oetf_normalizes_absolute_nits_by_reference_luminance() {
    fn display_linear_nits_to_pq(nits: f32) -> f32 {
        const PQ_REFERENCE_LUMINANCE_NITS: f32 = 10000.0;
        let m1 = crate::constants::PQ_M1;
        let m2 = crate::constants::PQ_M2;
        let c1 = crate::constants::PQ_C1;
        let c2 = crate::constants::PQ_C2;
        let c3 = crate::constants::PQ_C3;
        if !nits.is_finite() {
            return nits;
        }
        let normalized = nits.max(0.0) / PQ_REFERENCE_LUMINANCE_NITS;
        let lm1 = normalized.powf(m1);
        let num = c1 + c2 * lm1;
        let den = 1.0 + c3 * lm1;
        (num / den).powf(m2)
    }

    let code = display_linear_nits_to_pq(203.0);
    assert!(
        (code - 0.580_688_9).abs() < 1e-4,
        "203 nit SDR white should map to ~0.5807 PQ code, got {code}"
    );
    assert!(
        code < 1.0,
        "SDR white must stay inside PQ code range, got {code}"
    );
    let round_trip = super::pq_nonlinear_to_display_linear(code, 203.0);
    assert!(
        (round_trip - 1.0).abs() < 1e-3,
        "PQ encode/decode round trip for SDR white: expected 1.0, got {round_trip}"
    );
}

#[test]
fn pq_transfer_eotf_and_rec2020_matrix_produce_reasonable_sdr_fallback() {
    let meta = HdrImageMetadata {
        transfer_function: HdrTransferFunction::Pq,
        color_profile: HdrColorProfile::Cicp {
            color_primaries: 9,
            transfer_characteristics: 16,
            matrix_coefficients: 0,
            full_range: true,
        },
        ..Default::default()
    };
    let buffer = HdrImageBuffer {
        width: 1,
        height: 1,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::Rec2020Linear,
        metadata: meta,
        rgba_f32: Arc::new(vec![0.45, 0.45, 0.45, 1.0]),
    };
    let sdr = hdr_to_sdr_rgba8(&buffer, 0.0).expect("pq tone map");

    assert!(
        sdr[0] > 8 && sdr[0] < 250 && sdr[1] > 8 && sdr[1] < 250 && sdr[2] > 8 && sdr[2] < 250,
        "unexpected PQ SDR fallback RGB {:?}",
        &sdr[..3]
    );
    assert_eq!(sdr[3], 255);
}

#[test]
fn hdr_to_sdr_rgba8_with_tone_settings_respects_max_display_nits() {
    let meta = HdrImageMetadata {
        transfer_function: HdrTransferFunction::Pq,
        color_profile: HdrColorProfile::Cicp {
            color_primaries: 9,
            transfer_characteristics: 16,
            matrix_coefficients: 0,
            full_range: true,
        },
        ..Default::default()
    };
    let buffer = HdrImageBuffer {
        width: 1,
        height: 1,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::Rec2020Linear,
        metadata: meta,
        rgba_f32: Arc::new(vec![0.6, 0.6, 0.6, 1.0]),
    };
    // Smaller max_display_nits → larger peak_scale → brighter SDR after PQ decode + Reinhard.
    let narrow_peak = HdrToneMapSettings {
        max_display_nits: 600.0,
        ..HdrToneMapSettings::default()
    };
    let wide_peak = HdrToneMapSettings {
        max_display_nits: 4000.0,
        ..HdrToneMapSettings::default()
    };
    let brighter =
        hdr_to_sdr_rgba8_with_tone_settings(&buffer, 0.0, &narrow_peak).expect("tone map");
    let darker = hdr_to_sdr_rgba8_with_tone_settings(&buffer, 0.0, &wide_peak).expect("tone map");
    let sum_brighter: u32 = brighter[..3].iter().map(|&b| b as u32).sum();
    let sum_darker: u32 = darker[..3].iter().map(|&b| b as u32).sum();
    assert!(
        sum_brighter > sum_darker,
        "PQ SDR fallback should brighten when max_display_nits is lower: {sum_brighter} vs {sum_darker}"
    );
}

#[test]
fn strip_pinned_max_display_nits_not_raised_by_mastering_peak() {
    use crate::hdr::types::DEFAULT_SDR_WHITE_NITS;

    let meta = HdrImageMetadata {
        transfer_function: HdrTransferFunction::Pq,
        luminance: crate::hdr::types::HdrLuminanceMetadata {
            mastering_max_nits: Some(1000.0),
            ..Default::default()
        },
        color_profile: HdrColorProfile::Cicp {
            color_primaries: 9,
            transfer_characteristics: 16,
            matrix_coefficients: 0,
            full_range: true,
        },
        ..Default::default()
    };
    let buffer = HdrImageBuffer {
        width: 1,
        height: 1,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::Rec2020Linear,
        metadata: meta,
        rgba_f32: Arc::new(vec![0.6, 0.6, 0.6, 1.0]),
    };
    let strip_tone = HdrToneMapSettings {
        max_display_nits: DEFAULT_SDR_WHITE_NITS,
        ..HdrToneMapSettings::default()
    };
    let crushed = HdrToneMapSettings {
        max_display_nits: 1000.0,
        ..HdrToneMapSettings::default()
    };
    let bright =
        hdr_to_sdr_rgba8_with_tone_settings(&buffer, 0.0, &strip_tone).expect("strip tone");
    let dark = hdr_to_sdr_rgba8_with_tone_settings(&buffer, 0.0, &crushed).expect("crushed tone");
    let sum_bright: u32 = bright[..3].iter().map(|&b| b as u32).sum();
    let sum_dark: u32 = dark[..3].iter().map(|&b| b as u32).sum();
    assert!(
        sum_bright > sum_dark,
        "strip-pinned nits should stay bright despite mastering peak: {sum_bright} vs {sum_dark}"
    );
}

#[test]
fn tone_map_rejects_malformed_buffer_length() {
    let buffer = HdrImageBuffer {
        width: 1,
        height: 1,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::LinearSrgb,
        metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
        rgba_f32: Arc::new(vec![0.0, 0.0, 0.0]),
    };

    let err = hdr_to_sdr_rgba8(&buffer, 0.0).expect_err("reject malformed HDR buffer");

    assert!(err.contains("expected 4 floats"));
    assert!(err.contains("got 3"));
}

#[test]
fn radiance_header_params_diagnostic_reports_exposure_and_colorcorr() {
    let params = RadianceHeaderParams::read_from_bytes(
        b"#?RADIANCE\nFORMAT=32-bit_rle_rgbe\nEXPOSURE=2\nCOLORCORR=2 4 8\n\n-Y 1 +X 1\n",
    )
    .expect("parse Radiance header params");

    assert_eq!(
        params.diagnostic_label(),
        "Radiance EXPOSURE=2.000 COLORCORR=[2.000,4.000,8.000]"
    );
}

#[test]
fn decode_hdr_image_reads_radiance_hdr_as_rgba32f() {
    let path = std::env::temp_dir().join(format!(
        "simple_image_viewer_hdr_decode_{}.hdr",
        std::process::id()
    ));
    let bytes = b"#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n-Y 1 +X 1\n\x80\x80\x80\x81";
    std::fs::write(&path, bytes).expect("write test HDR");

    let buffer = decode_hdr_image(&path).expect("decode test HDR");
    let _ = std::fs::remove_file(&path);

    assert_eq!(buffer.width, 1);
    assert_eq!(buffer.height, 1);
    assert_eq!(buffer.format, HdrPixelFormat::Rgba32Float);
    assert_eq!(buffer.color_space, HdrColorSpace::LinearSrgb);
    assert_eq!(buffer.rgba_f32.len(), 4);
    assert!((buffer.rgba_f32[0] - 1.0).abs() < 0.01);
    assert!((buffer.rgba_f32[1] - 1.0).abs() < 0.01);
    assert!((buffer.rgba_f32[2] - 1.0).abs() < 0.01);
    assert_eq!(buffer.rgba_f32[3], 1.0);
}

#[test]
fn decode_hdr_image_applies_radiance_exposure_and_colorcorr() {
    let path = std::env::temp_dir().join(format!(
        "simple_image_viewer_hdr_decode_params_{}.hdr",
        std::process::id()
    ));
    let bytes = b"#?RADIANCE\nFORMAT=32-bit_rle_rgbe\nEXPOSURE=2\nCOLORCORR=2 4 8\n\n-Y 1 +X 1\n\x80\x80\x80\x81";
    std::fs::write(&path, bytes).expect("write test HDR");

    let buffer = decode_hdr_image(&path).expect("decode test HDR with header params");
    let _ = std::fs::remove_file(&path);

    assert!((buffer.rgba_f32[0] - 0.25).abs() < 0.01);
    assert!((buffer.rgba_f32[1] - 0.125).abs() < 0.01);
    assert!((buffer.rgba_f32[2] - 0.0625).abs() < 0.01);
    assert_eq!(buffer.rgba_f32[3], 1.0);
}

#[test]
fn decode_hdr_image_rejects_oversized_hdr_header_before_pixel_decode() {
    let path = std::env::temp_dir().join(format!(
        "simple_image_viewer_hdr_decode_huge_{}.hdr",
        std::process::id()
    ));
    let width = (MAX_HDR_FALLBACK_PIXELS + 1) as u32;
    let bytes = format!("#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n-Y 1 +X {width}\n");
    std::fs::write(&path, bytes).expect("write oversized test HDR");

    let err = decode_hdr_image(&path).expect_err("reject oversized HDR fallback");
    let _ = std::fs::remove_file(&path);

    assert!(err.contains("exceeds HDR fallback limit"));
    assert!(err.contains(&width.to_string()));
}

#[test]
fn decode_exr_display_image_reads_multipart_color_layer() {
    let Some(root) = openexr_images_root() else {
        eprintln!(
            "skipping OpenEXR multipart decode test; set SIV_OPENEXR_IMAGES_DIR to openexr-images"
        );
        return;
    };
    let path = root.join("v2/Stereo/composited.exr");
    if !path.is_file() {
        eprintln!("skipping OpenEXR multipart decode test; stereo composited sample missing");
        return;
    }

    let buffer = decode_exr_display_image(&path).expect("decode multipart EXR display layer");

    assert_eq!((buffer.width, buffer.height), (1918, 1078));
    assert!(
        buffer
            .rgba_f32
            .chunks_exact(4)
            .any(|pixel| pixel[0] > 0.0 || pixel[1] > 0.0 || pixel[2] > 0.0),
        "multipart display layer should contain visible RGB content"
    );
}
