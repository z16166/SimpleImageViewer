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

pub(crate) use probe::{
    is_jxl_header, jxl_color_encoding_to_metadata, libjxl_probe_orientation_from_bytes,
    libjxl_probe_orientation_from_path,
};
#[cfg(feature = "jpegxl")]
pub(crate) use decode::{
    decode_jxl_bytes_to_image_data, decode_jxl_hdr, decode_jxl_hdr_bytes,
    decode_jxl_hdr_bytes_with_target_capacity, decode_jxl_hdr_with_target_capacity, load_jxl_hdr,
    load_jxl_hdr_with_target_capacity, srgb_unit_to_u8,
};
#[cfg(feature = "jpegxl")]
pub(crate) use metadata::{
    decode_jxl_gain_map_from_bundle, read_jxl_gain_map_bundle, JxlGainMapBundleRef,
};
