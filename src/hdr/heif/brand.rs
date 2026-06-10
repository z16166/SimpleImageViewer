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

use crate::hdr::cicp::{self, H273_TRANSFER_ITU_BT709, H273_TRANSFER_SMPTE170M};
use crate::hdr::types::{HdrImageMetadata, HdrReference, HdrTransferFunction};

pub(crate) fn is_heif_brand(brand: &[u8]) -> bool {
    matches!(
        brand,
        b"heic" | b"heix" | b"hevc" | b"hevx" | b"mif1" | b"msf1"
    )
}

#[allow(dead_code)]
pub(crate) fn heif_nclx_to_metadata(
    color_primaries: u16,
    transfer_characteristics: u16,
    matrix_coefficients: u16,
    full_range: bool,
) -> HdrImageMetadata {
    let mut meta = cicp::cicp_to_metadata(
        color_primaries,
        transfer_characteristics,
        matrix_coefficients,
        full_range,
        None,
    );
    // **`cicp_to_metadata` is format-neutral** (H.273 1/6 → [`HdrTransferFunction::Bt709`]). For **HEIF
    // stills**, **primaries 1** with **transfer 1/6** is overwhelmingly authored as **IEC sRGB-like**
    // display codes — Chrome / OS viewers do **not** route that through BT.709 EOTF inverse + filmic
    // Reinhard on SDR (which reads “灰蒙蒙”). Narrow this override to that common phone/camera case only;
    // e.g. PQ / Rec.2020 mastering keeps strict `cicp` semantics from the block above.
    if color_primaries == 1
        && matches!(
            transfer_characteristics,
            H273_TRANSFER_ITU_BT709 | H273_TRANSFER_SMPTE170M
        )
    {
        meta.transfer_function = HdrTransferFunction::Srgb;
        meta.reference = HdrReference::Unknown;
    }
    meta
}
