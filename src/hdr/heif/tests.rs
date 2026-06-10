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

#[cfg(feature = "heif-native")]
use crate::hdr::cicp::{H273_TRANSFER_ITU_BT709, H273_TRANSFER_SMPTE170M};
#[cfg(feature = "heif-native")]
use crate::hdr::heif::heif_nclx_to_metadata;
use crate::hdr::heif::is_heif_brand;
#[cfg(feature = "heif-native")]
use crate::hdr::heif::{HeifAuxiliaryClassification, classify_heif_auxiliary_type};
#[cfg(feature = "heif-native")]
use crate::hdr::types::{HdrColorProfile, HdrReference, HdrTransferFunction};

#[cfg(feature = "heif-native")]
use super::{
    EXIF_ORIENTATION_NORMAL, EXIF_ORIENTATION_ROTATE_90_CCW, EXIF_ORIENTATION_ROTATE_90_CW,
    EXIF_ORIENTATION_ROTATE_180,
};

#[cfg(feature = "heif-native")]
#[test]
fn heif_nclx_bt709_family_primaries_1_prefers_srgb_for_browser_still_parity() {
    let bt709 = heif_nclx_to_metadata(1, H273_TRANSFER_ITU_BT709, 1, true);
    assert_eq!(bt709.transfer_function, HdrTransferFunction::Srgb);
    assert_eq!(bt709.reference, HdrReference::Unknown);

    let smpte = heif_nclx_to_metadata(1, H273_TRANSFER_SMPTE170M, 1, true);
    assert_eq!(smpte.transfer_function, HdrTransferFunction::Srgb);

    // Primaries **≠ 1** keeps strict `cicp` **Bt709** (mastering not a classic phone sRGB still).
    let wide = heif_nclx_to_metadata(9, H273_TRANSFER_ITU_BT709, 9, false);
    assert_eq!(wide.transfer_function, HdrTransferFunction::Bt709);
}

#[test]
fn heif_brand_detection_accepts_heic_family_and_generic_heif() {
    for brand in [b"heic", b"heix", b"hevc", b"hevx", b"mif1", b"msf1"] {
        assert!(is_heif_brand(brand));
    }
    assert!(!is_heif_brand(b"avif"));
}

#[cfg(feature = "heif-native")]
#[test]
fn heif_nclx_pq_maps_to_display_referred_metadata() {
    let metadata = heif_nclx_to_metadata(9, 16, 9, false);

    assert_eq!(metadata.transfer_function, HdrTransferFunction::Pq);
    assert_eq!(metadata.reference, HdrReference::DisplayReferred);
}

#[cfg(feature = "heif-native")]
#[test]
fn heif_transfer_depth_heuristic_pq_8bit_primary_to_srgb() {
    use super::apply_heif_transfer_depth_heuristics;

    let mut m = heif_nclx_to_metadata(9, 16, 9, false);
    assert_eq!(m.transfer_function, HdrTransferFunction::Pq);
    apply_heif_transfer_depth_heuristics(8, &mut m);
    assert_eq!(m.transfer_function, HdrTransferFunction::Srgb);
}

#[cfg(feature = "heif-native")]
#[test]
fn heif_transfer_depth_heuristic_pq_10bit_primary_unchanged() {
    use super::apply_heif_transfer_depth_heuristics;

    let mut m = heif_nclx_to_metadata(9, 16, 9, false);
    apply_heif_transfer_depth_heuristics(10, &mut m);
    assert_eq!(m.transfer_function, HdrTransferFunction::Pq);
}

#[cfg(feature = "heif-native")]
#[test]
fn heif_transfer_depth_heuristic_unknown_8bit_to_srgb() {
    use super::apply_heif_transfer_depth_heuristics;

    let mut m = heif_nclx_to_metadata(9, 99, 9, false);
    assert_eq!(m.transfer_function, HdrTransferFunction::Unknown);
    apply_heif_transfer_depth_heuristics(8, &mut m);
    assert_eq!(m.transfer_function, HdrTransferFunction::Srgb);
}

#[cfg(feature = "heif-native")]
#[test]
fn heif_unknown_transfer_bt709_primaries_fallback_promotes_srgb_still_decode() {
    use super::{
        apply_heif_transfer_depth_heuristics, apply_heif_unknown_transfer_bt709_primaries_fallback,
    };

    let mut m = heif_nclx_to_metadata(1, 99, 1, true);
    assert_eq!(m.transfer_function, HdrTransferFunction::Unknown);

    apply_heif_transfer_depth_heuristics(10, &mut m);
    assert_eq!(m.transfer_function, HdrTransferFunction::Unknown);

    apply_heif_unknown_transfer_bt709_primaries_fallback(&mut m);
    assert_eq!(m.transfer_function, HdrTransferFunction::Srgb);
    assert_eq!(m.reference, HdrReference::Unknown);
}

#[cfg(feature = "heif-native")]
#[test]
fn heif_unknown_transfer_not_lifted_for_rec2020_primaries() {
    use super::{
        apply_heif_transfer_depth_heuristics, apply_heif_unknown_transfer_bt709_primaries_fallback,
    };

    let mut m = heif_nclx_to_metadata(9, 99, 9, false);
    apply_heif_transfer_depth_heuristics(10, &mut m);
    apply_heif_unknown_transfer_bt709_primaries_fallback(&mut m);
    assert_eq!(m.transfer_function, HdrTransferFunction::Unknown);
}

#[cfg(feature = "heif-native")]
#[test]
fn heif_fallback_without_colour_boxes_is_srgb_transfer_not_scene_linear() {
    let m = super::heif_metadata_without_embedded_colour_info();
    assert_eq!(m.transfer_function, HdrTransferFunction::Srgb);
    assert!(matches!(m.color_profile, HdrColorProfile::LinearSrgb));
}

#[cfg(feature = "heif-native")]
#[test]
fn heif_auxiliary_type_classifies_gain_map_and_tmap_evidence() {
    assert_eq!(
        classify_heif_auxiliary_type("urn:com:apple:photo:2020:aux:hdrgainmap"),
        HeifAuxiliaryClassification::AppleHdrGainMap
    );
    assert_eq!(
        classify_heif_auxiliary_type("urn:mpeg:mpegB:cicp:systems:auxiliary:hdr_gain_map"),
        HeifAuxiliaryClassification::IsoGainMap
    );
    assert_eq!(
        classify_heif_auxiliary_type("urn:com:apple:photo:2023:aux:tmap"),
        HeifAuxiliaryClassification::AppleTmap
    );
    assert_eq!(
        classify_heif_auxiliary_type("urn:mpeg:mpegB:cicp:systems:auxiliary:depth"),
        HeifAuxiliaryClassification::Unknown
    );
}

#[cfg(feature = "heif-native")]
#[test]
fn heif_studio_swing_8bit_neutral_gray_bt709() {
    use super::{HeifYcbcrMatrix, studio_digital_sample_to_normalized, ycbcr_linear_to_rgb};

    let ey = studio_digital_sample_to_normalized(110, 8, true).unwrap();
    assert!((ey - 94.0 / 219.0).abs() < 1e-5);

    let ecb = studio_digital_sample_to_normalized(128, 8, false).unwrap();
    let ecr = studio_digital_sample_to_normalized(128, 8, false).unwrap();
    assert!(ecb.abs() < 1e-5 && ecr.abs() < 1e-5);

    let [r, g, b] = ycbcr_linear_to_rgb(ey, ecb, ecr, HeifYcbcrMatrix::Bt709);
    assert!(
        (r - g).abs() < 2e-4 && (g - b).abs() < 2e-4,
        "neutral chroma should yield R≈G≈B, got ({r},{g},{b})"
    );
}

#[cfg(feature = "heif-native")]
#[test]
fn heif_ycbcr_bt2020_neutral_chroma_gray_axis() {
    use super::{HeifYcbcrMatrix, ycbcr_linear_to_rgb};
    let ey = 0.4123_f32;
    let [r, g, b] = ycbcr_linear_to_rgb(ey, 0.0, 0.0, HeifYcbcrMatrix::Bt2020Ncl);
    assert!((r - ey).abs() < 1e-5);
    assert!((g - ey).abs() < 1e-5);
    assert!((b - ey).abs() < 1e-5);
}

#[cfg(feature = "heif-native")]
#[test]
fn heif_ycbcr_monochrome_replicates_y() {
    use super::{HeifYcbcrMatrix, ycbcr_linear_to_rgb};
    let [r, g, b] = ycbcr_linear_to_rgb(0.42, 0.9, -0.3, HeifYcbcrMatrix::Monochrome);
    assert!((r - 0.42).abs() < 1e-6 && r == g && g == b);
}

#[cfg(feature = "heif-native")]
#[test]
fn heif_nclx_maps_matrix_coefficients_to_ycbcr_matrix() {
    use super::{HeifYcbcrMatrix, heif_ycbcr_matrix_from_nclx};
    use crate::hdr::types::{HdrColorProfile, HdrImageMetadata};

    fn meta(mc: u16) -> HdrImageMetadata {
        HdrImageMetadata {
            color_profile: HdrColorProfile::Cicp {
                color_primaries: 1,
                transfer_characteristics: 1,
                matrix_coefficients: mc,
                full_range: true,
            },
            ..Default::default()
        }
    }

    assert_eq!(
        heif_ycbcr_matrix_from_nclx(&meta(0), 640, 480),
        HeifYcbcrMatrix::Bt601
    );
    assert_eq!(
        heif_ycbcr_matrix_from_nclx(&meta(0), 1920, 1080),
        HeifYcbcrMatrix::Bt709
    );
    assert_eq!(
        heif_ycbcr_matrix_from_nclx(&meta(5), 100, 100),
        HeifYcbcrMatrix::Bt601
    );
    assert_eq!(
        heif_ycbcr_matrix_from_nclx(&meta(6), 100, 100),
        HeifYcbcrMatrix::Bt601
    );
    assert_eq!(
        heif_ycbcr_matrix_from_nclx(&meta(9), 100, 100),
        HeifYcbcrMatrix::Bt2020Ncl
    );
    assert_eq!(
        heif_ycbcr_matrix_from_nclx(&meta(10), 100, 100),
        HeifYcbcrMatrix::Bt2020Ncl
    );
    assert_eq!(
        heif_ycbcr_matrix_from_nclx(&meta(12), 100, 100),
        HeifYcbcrMatrix::Bt2020Ncl
    );
    assert_eq!(
        heif_ycbcr_matrix_from_nclx(&meta(1), 100, 100),
        HeifYcbcrMatrix::Bt709
    );
    assert_eq!(
        heif_ycbcr_matrix_from_nclx(&meta(255), 100, 100),
        HeifYcbcrMatrix::Bt709
    );
    assert_eq!(
        heif_ycbcr_matrix_from_nclx(&HdrImageMetadata::default(), 1, 1),
        HeifYcbcrMatrix::Bt709
    );
}

#[cfg(feature = "heif-native")]
fn gradient_gain_rgba(width: u32, height: u32) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            rgba.extend_from_slice(&[x as u8, y as u8, 0, 255]);
        }
    }
    rgba
}

#[cfg(feature = "heif-native")]
#[test]
fn align_apple_gain_map_rotates_landscape_sensor_to_portrait_display() {
    use super::align_apple_gain_map_to_primary_display_orientation;

    let width = 4_u32;
    let height = 3_u32;
    let gain = gradient_gain_rgba(width, height);
    let (out_w, out_h, out) = align_apple_gain_map_to_primary_display_orientation(
        gain,
        width,
        height,
        4032,
        3024,
        3024,
        4032,
        Some(EXIF_ORIENTATION_ROTATE_90_CW),
    );
    assert_eq!((out_w, out_h), (height, width));
    let pixel = |buf: &[u8], w: u32, x: u32, y: u32| {
        let idx = (y as usize * w as usize + x as usize) * 4;
        [buf[idx], buf[idx + 1], buf[idx + 2], buf[idx + 3]]
    };
    // dst(0,0) = src(H-1-0, 0) = src(2,0) → R=0, G=2
    assert_eq!(pixel(&out, out_w, 0, 0), [0, 2, 0, 255]);
    // dst(0,1) = src(2,1) → R=1, G=2
    assert_eq!(pixel(&out, out_w, 0, 1), [1, 2, 0, 255]);
}

#[cfg(feature = "heif-native")]
#[test]
fn align_apple_gain_map_rotates_landscape_sensor_to_portrait_display_ccw() {
    use super::align_apple_gain_map_to_primary_display_orientation;

    let width = 4_u32;
    let height = 3_u32;
    let gain = gradient_gain_rgba(width, height);
    let (out_w, out_h, out) = align_apple_gain_map_to_primary_display_orientation(
        gain,
        width,
        height,
        4032,
        3024,
        3024,
        4032,
        Some(EXIF_ORIENTATION_ROTATE_90_CCW),
    );
    assert_eq!((out_w, out_h), (height, width));
    let pixel = |buf: &[u8], w: u32, x: u32, y: u32| {
        let idx = (y as usize * w as usize + x as usize) * 4;
        [buf[idx], buf[idx + 1], buf[idx + 2], buf[idx + 3]]
    };
    // dst(0,0) = src(0, W-1-0) = src(0,3) → R=3, G=0
    assert_eq!(pixel(&out, out_w, 0, 0), [3, 0, 0, 255]);
}

#[cfg(feature = "heif-native")]
#[test]
fn align_apple_gain_map_rotates_180() {
    use super::align_apple_gain_map_to_primary_display_orientation;

    let width = 4_u32;
    let height = 3_u32;
    let gain = gradient_gain_rgba(width, height);
    let (out_w, out_h, out) = align_apple_gain_map_to_primary_display_orientation(
        gain,
        width,
        height,
        4032,
        3024,
        4032,
        3024,
        Some(EXIF_ORIENTATION_ROTATE_180),
    );
    assert_eq!((out_w, out_h), (width, height));
    let pixel = |buf: &[u8], w: u32, x: u32, y: u32| {
        let idx = (y as usize * w as usize + x as usize) * 4;
        [buf[idx], buf[idx + 1], buf[idx + 2], buf[idx + 3]]
    };
    // dst(0,0) = src(W*H - 1 - 0) = src(2,3) → R=3, G=2
    assert_eq!(pixel(&out, out_w, 0, 0), [3, 2, 0, 255]);
}

#[cfg(feature = "heif-native")]
#[test]
fn align_apple_gain_map_skips_rotation_when_orientations_match() {
    use super::align_apple_gain_map_to_primary_display_orientation;

    let width = 4_u32;
    let height = 3_u32;
    let gain = gradient_gain_rgba(width, height);
    let (out_w, out_h, out) = align_apple_gain_map_to_primary_display_orientation(
        gain.clone(),
        width,
        height,
        width,
        height,
        width,
        height,
        None,
    );
    assert_eq!((out_w, out_h), (width, height));
    assert_eq!(out, gain);
}

#[cfg(feature = "heif-native")]
#[test]
fn align_apple_gain_map_falls_back_to_ispe_heuristic_when_exif_normal() {
    use super::align_apple_gain_map_to_primary_display_orientation;

    let width = 4_u32;
    let height = 3_u32;
    let gain = gradient_gain_rgba(width, height);
    let (out_w, out_h, out) = align_apple_gain_map_to_primary_display_orientation(
        gain,
        width,
        height,
        4032,
        3024,
        3024,
        4032,
        Some(EXIF_ORIENTATION_NORMAL),
    );
    assert_eq!((out_w, out_h), (height, width));
    let pixel = |buf: &[u8], w: u32, x: u32, y: u32| {
        let idx = (y as usize * w as usize + x as usize) * 4;
        [buf[idx], buf[idx + 1], buf[idx + 2], buf[idx + 3]]
    };
    assert_eq!(pixel(&out, out_w, 0, 0), [0, 2, 0, 255]);
}

#[cfg(feature = "heif-native")]
#[test]
fn align_apple_gain_map_heuristic_skips_when_gain_matches_ispe_dimensions() {
    use super::align_apple_gain_map_to_primary_display_orientation;

    let width = 4032_u32;
    let height = 3024_u32;
    let gain = gradient_gain_rgba(width, height);
    let (out_w, out_h, out) = align_apple_gain_map_to_primary_display_orientation(
        gain.clone(),
        width,
        height,
        width,
        height,
        3024,
        4032,
        None,
    );
    assert_eq!((out_w, out_h), (width, height));
    assert_eq!(out, gain);
}

#[cfg(feature = "heif-native")]
#[test]
fn align_apple_gain_map_exif_rotates_even_when_gain_matches_ispe_dimensions() {
    use super::align_apple_gain_map_to_primary_display_orientation;

    let width = 4_u32;
    let height = 3_u32;
    let gain = gradient_gain_rgba(width, height);
    let (out_w, out_h, _) = align_apple_gain_map_to_primary_display_orientation(
        gain,
        width,
        height,
        width,
        height,
        height,
        width,
        Some(EXIF_ORIENTATION_ROTATE_90_CW),
    );
    assert_eq!((out_w, out_h), (height, width));
}
