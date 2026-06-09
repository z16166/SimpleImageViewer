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

use crate::hdr::types::HdrImageMetadata;

#[allow(dead_code)]
pub(crate) fn avif_cicp_to_metadata(
    color_primaries: u16,
    transfer_characteristics: u16,
    matrix_coefficients: u16,
    full_range: bool,
) -> HdrImageMetadata {
    crate::hdr::cicp::cicp_to_metadata(
        color_primaries,
        transfer_characteristics,
        matrix_coefficients,
        full_range,
        None,
    )
}

pub(crate) trait AvifMetadataExt {
    fn with_clli(self, max_cll: u16, max_fall: u16) -> Self;
}

#[cfg(feature = "avif-native")]
impl AvifMetadataExt for HdrImageMetadata {
    fn with_clli(mut self, max_cll: u16, max_fall: u16) -> Self {
        if max_cll > 0 {
            self.luminance.max_cll_nits = Some(max_cll as f32);
        }
        if max_fall > 0 {
            self.luminance.max_fall_nits = Some(max_fall as f32);
        }
        self
    }
}

pub(crate) fn avif_yuv_to_rgb_output_metadata(
    cicp_metadata: &HdrImageMetadata,
    image_ref: &libavif_sys::avifImage,
) -> HdrImageMetadata {
    use crate::hdr::types::{HdrColorProfile, HdrReference, HdrTransferFunction};

    let mut metadata = cicp_metadata.clone();
    if image_ref.gainMap.is_null()
        && matches!(
            metadata.transfer_function,
            HdrTransferFunction::Pq
                | HdrTransferFunction::Hlg
                // H.273 **unspecified** (code 2) is common in Microsoft / conformance AVIF
                // (e.g. `Mexico_YUV444.avif`). libavif's YUV->RGB output is display-gamma RGB like
                // PNG export - not scene-linear. Without this, WGSL leaves transfer `Unknown` and
                // treats encoded codes as linear - washed "white mist" vs Windows Photos.
                | HdrTransferFunction::Unknown
        )
    {
        log::debug!(
            "[AVIF] YUV??RGB buffer uses display gamma (not PQ/HLG codes); \
             shader transfer {:?} ?? sRGB / linear sRGB (CICP tf={} primaries={} matrix={})",
            metadata.transfer_function,
            image_ref.transferCharacteristics,
            image_ref.colorPrimaries,
            image_ref.matrixCoefficients,
        );
        metadata.transfer_function = HdrTransferFunction::Srgb;
        metadata.reference = HdrReference::Unknown;
        // Numeric values match libavif PNG export / paired SDR references (BT.709-like RGB),
        // not PQ codes in BT.2020 linear light ?? skip Rec.2020 primary conversion in WGSL.
        metadata.color_profile = HdrColorProfile::LinearSrgb;
    }
    metadata
}

