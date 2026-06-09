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

#![cfg(feature = "avif-native")]

use crate::hdr::gain_map::{GainMapMetadata, IsoGainMapFraction};

use super::decode::{decode_avif_image_rgba_u16, rgb_channel_max_f};

pub(crate) fn decode_avif_gain_map<F: Fn(libavif_sys::avifResult) -> String>(
    image_ref: &libavif_sys::avifImage,
    result_to_string: &F,
) -> Option<(GainMapMetadata, u32, u32, Vec<u8>)> {
    if image_ref.gainMap.is_null() {
        return None;
    }
    let gain_map = unsafe { &*image_ref.gainMap };
    if gain_map.image.is_null() {
        log::warn!("[HDR] AVIF gain map metadata present without gain-map pixels");
        return None;
    }
    let metadata = match avif_gain_map_to_metadata(gain_map) {
        Ok(metadata) => metadata,
        Err(err) => {
            log::warn!("[HDR] AVIF gain map metadata is not usable: {err}");
            return None;
        }
    };
    let gain_image = unsafe { &*gain_map.image };
    let (gain_rgba_u16, gain_rgb_depth) =
        match decode_avif_image_rgba_u16(gain_map.image, gain_image, result_to_string) {
            Ok(pixels) => pixels,
            Err(err) => {
                log::warn!("[HDR] AVIF gain map pixel decode failed: {err}");
                return None;
            }
        };
    let scale = rgb_channel_max_f(gain_rgb_depth);
    let denominator = scale / u8::MAX as f32;
    let gain_rgba = gain_rgba_u16
        .into_iter()
        .map(|value| (value as f32 / denominator).round().clamp(0.0, 255.0) as u8)
        .collect();
    Some((metadata, gain_image.width, gain_image.height, gain_rgba))
}

#[cfg(feature = "avif-native")]
pub(crate) fn avif_gain_map_to_metadata(
    gain_map: &libavif_sys::avifGainMap,
) -> Result<GainMapMetadata, String> {
    let mut fraction = IsoGainMapFraction::default();
    for channel in 0..3 {
        fraction.gain_map_min[channel] = signed(gain_map.gainMapMin[channel]);
        fraction.gain_map_max[channel] = signed(gain_map.gainMapMax[channel]);
        fraction.gamma[channel] = unsigned(gain_map.gainMapGamma[channel]);
        fraction.base_offset[channel] = signed(gain_map.baseOffset[channel]);
        fraction.alternate_offset[channel] = signed(gain_map.alternateOffset[channel]);
    }
    fraction.base_hdr_headroom = unsigned(gain_map.baseHdrHeadroom);
    fraction.alternate_hdr_headroom = unsigned(gain_map.alternateHdrHeadroom);
    fraction.into_gain_map_metadata(0)
}

#[cfg(feature = "avif-native")]
fn signed(value: libavif_sys::avifSignedFraction) -> (i32, u32) {
    (value.n, value.d)
}

#[cfg(feature = "avif-native")]
fn unsigned(value: libavif_sys::avifUnsignedFraction) -> (u32, u32) {
    (value.n, value.d)
}

#[cfg(test)]
pub(crate) fn test_signed_fraction(n: i32, d: u32) -> libavif_sys::avifSignedFraction {
    libavif_sys::avifSignedFraction { n, d }
}

#[cfg(test)]
pub(crate) fn test_unsigned_fraction(n: u32, d: u32) -> libavif_sys::avifUnsignedFraction {
    libavif_sys::avifUnsignedFraction { n, d }
}
