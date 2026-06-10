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

mod decode;
mod metadata;
mod probe;
mod runner;

#[cfg(test)]
mod tests;

#[cfg(all(test, feature = "jpegxl"))]
pub(crate) use decode::decode_jxl_bytes_to_image_data;
#[cfg(all(test, feature = "jpegxl"))]
pub(crate) use decode::{
    decode_jxl_hdr_bytes, jxl_find_black_extra_channel_index, jxl_sdr_grade_fallback_rgba8,
    jxl_tag_display_referred_when_sdr_grade,
};
#[cfg(feature = "jpegxl")]
pub(crate) use decode::{load_jxl_hdr_with_target_capacity, srgb_unit_to_u8};
#[cfg(feature = "jpegxl")]
pub(crate) use metadata::{
    JxlGainMapBundleRef, decode_jxl_gain_map_from_bundle, read_jxl_gain_map_bundle,
};
#[cfg(all(test, feature = "jpegxl"))]
pub(crate) use metadata::{hdr_metadata_from_jxl_float_decode, icc_trc_kind, linear_to_srgb_u8};
#[cfg(all(test, feature = "jpegxl"))]
pub(crate) use probe::{
    JXL_TRANSFER_FUNCTION_HLG, JXL_TRANSFER_FUNCTION_LINEAR, JXL_TRANSFER_FUNCTION_PQ,
    JXL_TRANSFER_FUNCTION_SRGB, jxl_color_encoding_to_metadata,
};
pub(crate) use probe::{is_jxl_header, libjxl_probe_orientation_from_path};
