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

//! Embedded ICC **primaries** via **ISO 15076** ICC.1 tags read through **Little CMS 2**.
//! Maps measured **rXYZ / gXYZ / bXYZ** chromaticities to [`HdrColorSpace`] by **nearest** match to
//! tabulated **CIE xy** primaries for **ITU-R BT.709** / **SMPTE EG 432-1 Display P3** / **ITU-R BT.2020**
//! (same tables as CSS Color / MPEG-CICP usage). No profile-name or `mluc` / `desc` text matching.

use crate::hdr::types::HdrColorSpace;

/// Outcome of [`classify_embedded_icc_primaries`].
pub(crate) enum EmbeddedIccHint {
    /// Nearest standard gamut within [`XY_MATCH_MAX`] (chromaticity distance).
    Classified(HdrColorSpace),
    /// Valid **RGB** ICC with **XYZ** colorants, but xy are not close to any of the three reference gamuts.
    RgbPrimariesUnmatched,
    /// `cmsOpenProfileFromMem` failed, or profile data space is not **RGB**, or **`rXYZ`/`gXYZ`/`bXYZ`** missing / unusable.
    IccPrimariesNotReadable,
}

/// BT.709 / sRGB display primaries (xy), ITU-R BT.709-6 / IEC 61966-2.1.
const REF_BT709_SRGB: [(f64, f64); 3] = [
    (0.64, 0.33),
    (0.30, 0.60),
    (0.15, 0.06),
];

/// Display P3 (SMPTE EG 432-1 / common Apple / wide-gamut UI).
const REF_DISPLAY_P3: [(f64, f64); 3] = [
    (0.680, 0.320),
    (0.265, 0.690),
    (0.150, 0.060),
];

/// Rec. ITU-R BT.2020 / BT.2100 primaries (xy).
const REF_BT2020: [(f64, f64); 3] = [
    (0.708, 0.292),
    (0.170, 0.797),
    (0.131, 0.046),
];

/// Application tolerance on **CIE xy** (not an ITU constant; chosen so slight ICC quantization still maps).
const XY_MATCH_MAX: f64 = 0.058;

/// Classify embedded ICC using **lcms2** + ICC **`XYZType`** colorant tags.
pub(crate) fn classify_embedded_icc_primaries(icc: &[u8]) -> EmbeddedIccHint {
    use libjxl_sys::{
        CmsProfile, CMS_SIG_BLUE_COLORANT, CMS_SIG_GREEN_COLORANT, CMS_SIG_RED_COLORANT,
        CMS_SIG_RGB_DATA,
    };

    let Some(profile) = CmsProfile::open_from_mem(icc) else {
        return EmbeddedIccHint::IccPrimariesNotReadable;
    };
    if profile.data_color_space() != CMS_SIG_RGB_DATA {
        return EmbeddedIccHint::IccPrimariesNotReadable;
    }
    let Some(r) = profile.read_tag_ciexyz(CMS_SIG_RED_COLORANT) else {
        return EmbeddedIccHint::IccPrimariesNotReadable;
    };
    let Some(g) = profile.read_tag_ciexyz(CMS_SIG_GREEN_COLORANT) else {
        return EmbeddedIccHint::IccPrimariesNotReadable;
    };
    let Some(b) = profile.read_tag_ciexyz(CMS_SIG_BLUE_COLORANT) else {
        return EmbeddedIccHint::IccPrimariesNotReadable;
    };

    let Some(r_xy) = xyz_to_xy(r) else {
        return EmbeddedIccHint::IccPrimariesNotReadable;
    };
    let Some(g_xy) = xyz_to_xy(g) else {
        return EmbeddedIccHint::IccPrimariesNotReadable;
    };
    let Some(b_xy) = xyz_to_xy(b) else {
        return EmbeddedIccHint::IccPrimariesNotReadable;
    };

    let measured = [r_xy, g_xy, b_xy];
    match best_reference_gamut(measured) {
        Some(cs) => EmbeddedIccHint::Classified(cs),
        None => EmbeddedIccHint::RgbPrimariesUnmatched,
    }
}

fn xyz_to_xy(xyz: libjxl_sys::CmsCiexyz) -> Option<(f64, f64)> {
    let s = xyz.x + xyz.y + xyz.z;
    if s <= 1e-30 || !s.is_finite() || !xyz.x.is_finite() || !xyz.y.is_finite() {
        return None;
    }
    Some((xyz.x / s, xyz.y / s))
}

fn max_euclidean_xy_distance(measured: [(f64, f64); 3], reference: [(f64, f64); 3]) -> f64 {
    let mut m = 0.0_f64;
    for i in 0..3 {
        let dx = measured[i].0 - reference[i].0;
        let dy = measured[i].1 - reference[i].1;
        m = m.max((dx * dx + dy * dy).sqrt());
    }
    m
}

fn best_reference_gamut(measured: [(f64, f64); 3]) -> Option<HdrColorSpace> {
    let candidates = [
        (HdrColorSpace::LinearSrgb, REF_BT709_SRGB),
        (HdrColorSpace::DisplayP3Linear, REF_DISPLAY_P3),
        (HdrColorSpace::Rec2020Linear, REF_BT2020),
    ];
    let (space, dist) = candidates
        .into_iter()
        .map(|(sp, reference)| (sp, max_euclidean_xy_distance(measured, reference)))
        .min_by(|a, b| {
            a.1.partial_cmp(&b.1)
                .unwrap_or(std::cmp::Ordering::Equal)
        })?;
    if dist <= XY_MATCH_MAX {
        Some(space)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_bt709_reference_classifies_linear_srgb() {
        assert_eq!(
            best_reference_gamut(REF_BT709_SRGB),
            Some(HdrColorSpace::LinearSrgb)
        );
    }

    #[test]
    fn p3_reference_classifies_display_p3_linear() {
        assert_eq!(
            best_reference_gamut(REF_DISPLAY_P3),
            Some(HdrColorSpace::DisplayP3Linear)
        );
    }

    #[test]
    fn bt2020_reference_classifies_rec2020() {
        assert_eq!(
            best_reference_gamut(REF_BT2020),
            Some(HdrColorSpace::Rec2020Linear)
        );
    }

    #[test]
    fn built_in_srgb_profile_classifies_via_lcms_xyz_tags() {
        let profile = libjxl_sys::CmsProfile::new_srgb().expect("lcms sRGB");
        let r = profile
            .read_tag_ciexyz(libjxl_sys::CMS_SIG_RED_COLORANT)
            .expect("rXYZ");
        let g = profile
            .read_tag_ciexyz(libjxl_sys::CMS_SIG_GREEN_COLORANT)
            .expect("gXYZ");
        let b = profile
            .read_tag_ciexyz(libjxl_sys::CMS_SIG_BLUE_COLORANT)
            .expect("bXYZ");
        let r_xy = xyz_to_xy(r).expect("r xy");
        let g_xy = xyz_to_xy(g).expect("g xy");
        let b_xy = xyz_to_xy(b).expect("b xy");
        assert_eq!(
            best_reference_gamut([r_xy, g_xy, b_xy]),
            Some(HdrColorSpace::LinearSrgb)
        );
    }
}
