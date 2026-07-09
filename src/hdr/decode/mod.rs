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

mod constants;
mod decode_image;
mod exr;
mod paths;
mod radiance;
mod tone_map;
mod tone_map_simd;

#[cfg(test)]
mod tests;

pub use decode_image::decode_hdr_image;
pub use decode_image::is_hdr_candidate_ext;
pub(crate) use exr::decode_exr_display_image_from_mmap;
pub(crate) use paths::looks_like_radiance_hdr_bytes;
pub(crate) use radiance::{RadianceHeaderParams, decode_radiance_hdr_image_from_mmap};
pub use tone_map::hdr_to_sdr_rgba8;
pub(crate) use tone_map::{
    bt709_nonlinear_channel_to_linear, decode_transfer_to_display_linear,
    hlg_nonlinear_to_scene_linear, linear_primary_to_linear_srgb, linear_srgb_linear_to_srgb_u8,
    pq_nonlinear_to_absolute_nits, validate_hdr_fallback_budget,
};
pub use tone_map_simd::hdr_to_sdr_rgba8_with_tone_settings;

#[cfg(test)]
pub(crate) use tone_map::{
    encode_linear_display_referred_srgb8, encode_sdr_rgb8, pq_nonlinear_to_display_linear,
    srgb_nonlinear_channel_to_linear,
};

pub(crate) use constants::MAX_HDR_FALLBACK_DECODE_BYTES;
#[cfg(test)]
pub(crate) use constants::MAX_HDR_FALLBACK_PIXELS;
pub(crate) use tone_map_simd::hdr_to_sdr_rgba8_strip_preview;
