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

//! Shared **ITU-T H.273** CICP (a.k.a. NCLX / color encoding) mapping to [`HdrImageMetadata`].
//!
//! Used by AVIF, HEIF, and JPEG XL metadata paths (`transfer_characteristics` are CICP codes;
//! JPEG XL enums align numerically where applicable).

use crate::hdr::types::{
    HdrColorProfile, HdrImageMetadata, HdrLuminanceMetadata, HdrReference, HdrTransferFunction,
};

/// Coded **`transfer_characteristics`** values (**ITU-T H.273**), same integers as MPEG / MP4
/// **`TransferCharacteristics`** and AVIF **`colr` / Cicp**.
///
/// See H.273 Table 2 (Transfer characteristics) — we only wire the subsets this viewer maps into
/// [`HdrTransferFunction`]; other codes still flow through untouched in [`HdrImageMetadata`] via
/// [`HdrColorProfile::Cicp`].
pub(crate) const H273_TRANSFER_LINEAR: u16 = 8;
/// ITU-T H.273 **`transfer_characteristics = 1`**: **BT.709 opto-electronic / inverse OETF** (not IEC 61966‑2‑1 sRGB).
pub(crate) const H273_TRANSFER_ITU_BT709: u16 = 1;
/// **SMPTE 170 / BT‑601-compatible** nonlinear transfer (**H.273** code **6**): same transfer parameters as BT.709
/// for RGB in HD/broadcast mastering; wired to [`HdrTransferFunction::Bt709`].
pub(crate) const H273_TRANSFER_SMPTE170M: u16 = 6;
pub(crate) const H273_TRANSFER_IEC61966_2_1_SRGB: u16 = 13;
pub(crate) const H273_TRANSFER_SMPTE_ST2084_FOR_PQ: u16 = 16;
pub(crate) const H273_TRANSFER_ARIB_STD_B67_FOR_HLG: u16 = 18;

pub(crate) fn cicp_to_metadata(
    color_primaries: u16,
    transfer_characteristics: u16,
    matrix_coefficients: u16,
    full_range: bool,
    intensity_target_nits: Option<f32>,
) -> HdrImageMetadata {
    let transfer_function = match transfer_characteristics {
        H273_TRANSFER_LINEAR => HdrTransferFunction::Linear,
        H273_TRANSFER_ITU_BT709 | H273_TRANSFER_SMPTE170M => HdrTransferFunction::Bt709,
        H273_TRANSFER_IEC61966_2_1_SRGB => HdrTransferFunction::Srgb,
        H273_TRANSFER_SMPTE_ST2084_FOR_PQ => HdrTransferFunction::Pq,
        H273_TRANSFER_ARIB_STD_B67_FOR_HLG => HdrTransferFunction::Hlg,
        _ => HdrTransferFunction::Unknown,
    };
    let reference = match transfer_function {
        HdrTransferFunction::Pq => HdrReference::DisplayReferred,
        HdrTransferFunction::Hlg => HdrReference::SceneLinear,
        _ => HdrReference::Unknown,
    };

    let luminance = match intensity_target_nits {
        Some(nits) => HdrLuminanceMetadata {
            mastering_max_nits: Some(nits),
            ..HdrLuminanceMetadata::default()
        },
        None => HdrLuminanceMetadata::default(),
    };

    HdrImageMetadata {
        transfer_function,
        reference,
        color_profile: HdrColorProfile::Cicp {
            color_primaries,
            transfer_characteristics,
            matrix_coefficients,
            full_range,
        },
        luminance,
        gain_map: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::hdr::types::{HdrReference, HdrTransferFunction};

    #[test]
    fn cicp_bt709_transfer_maps_to_bt709_not_iec_srgb() {
        let meta = cicp_to_metadata(
            /* color_primaries */ 1,
            H273_TRANSFER_ITU_BT709,
            /* matrix */ 1,
            true,
            None,
        );
        assert_eq!(meta.transfer_function, HdrTransferFunction::Bt709);
        assert_eq!(meta.reference, HdrReference::Unknown);
    }

    #[test]
    fn cicp_transfer_13_keeps_explicit_srgb() {
        let meta = cicp_to_metadata(1, H273_TRANSFER_IEC61966_2_1_SRGB, 1, true, None);
        assert_eq!(meta.transfer_function, HdrTransferFunction::Srgb);
    }

    #[test]
    fn cicp_transfer_6_maps_to_bt709_transfer() {
        let meta = cicp_to_metadata(1, H273_TRANSFER_SMPTE170M, 1, true, None);
        assert_eq!(meta.transfer_function, HdrTransferFunction::Bt709);
        assert_eq!(meta.reference, HdrReference::Unknown);
    }
}
