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
mod brand;

#[cfg(feature = "heif-native")]
mod decode;
#[cfg(feature = "heif-native")]
mod gain_map;
#[cfg(feature = "heif-native")]
mod load;
#[cfg(feature = "heif-native")]
mod metadata;
#[cfg(feature = "heif-native")]
mod orientation;
#[cfg(feature = "heif-native")]
mod session;
#[cfg(feature = "heif-native")]
mod ycbcr;

#[cfg(test)]
mod tests;

pub(crate) use brand::is_heif_brand;
#[cfg(feature = "heif-native")]
pub(crate) use load::load_heif_hdr;
#[cfg(feature = "heif-native")]
pub(crate) use orientation::{
    decoded_pixels_match_swapped_ispe, libheif_exif_orientation_tag,
    libheif_manual_geometry_exif_orientation_from_path,
};

#[cfg(all(test, feature = "heif-native"))]
pub(crate) use brand::heif_nclx_to_metadata;
#[cfg(all(test, feature = "heif-native"))]
pub(crate) use gain_map::{
    EXIF_ORIENTATION_NORMAL, EXIF_ORIENTATION_ROTATE_90_CCW, EXIF_ORIENTATION_ROTATE_90_CW,
    EXIF_ORIENTATION_ROTATE_180, align_apple_gain_map_to_primary_display_orientation,
};
#[cfg(all(test, feature = "heif-native"))]
pub(crate) use metadata::{
    HeifAuxiliaryClassification, apply_heif_transfer_depth_heuristics,
    apply_heif_unknown_transfer_bt709_primaries_fallback, classify_heif_auxiliary_type,
    heif_metadata_without_embedded_colour_info,
};
#[cfg(all(test, feature = "heif-native"))]
pub(crate) use ycbcr::{
    HeifYcbcrMatrix, heif_ycbcr_matrix_from_nclx, studio_digital_sample_to_normalized,
    ycbcr_linear_to_rgb,
};
