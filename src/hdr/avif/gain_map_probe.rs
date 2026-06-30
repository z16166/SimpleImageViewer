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

//! Parse-only AVIF gain-map probes (no pixel-plane decode).

#![cfg(feature = "avif-native")]

use super::gain_map::avif_gain_map_to_metadata;
use crate::hdr::gain_map::iso_gain_map_skips_forward_compose;

/// Outcome of a container parse for directory-tree strip scheduling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AvifGainMapStripProbe {
    NoGainMap,
    PrecomposedHdr,
    ForwardIsoGainMap,
}

fn libavif_parse_gain_map_container(bytes: &[u8]) -> Option<libavif_sys::AvifDecoderOwned> {
    let decoder = libavif_sys::AvifDecoderOwned::new()?;
    unsafe {
        libavif_sys::siv_avif_decoder_set_strict_flags(
            decoder.as_ptr(),
            libavif_sys::AVIF_STRICT_DISABLED,
        );
        libavif_sys::siv_avif_decoder_set_image_content_flags(
            decoder.as_ptr(),
            libavif_sys::AVIF_IMAGE_CONTENT_ALL,
        );
    }
    let r = unsafe {
        libavif_sys::avifDecoderSetIOMemory(decoder.as_ptr(), bytes.as_ptr(), bytes.len())
    };
    if r != libavif_sys::AVIF_RESULT_OK {
        return None;
    }
    let r = unsafe { libavif_sys::avifDecoderParse(decoder.as_ptr()) };
    if r != libavif_sys::AVIF_RESULT_OK {
        return None;
    }
    Some(decoder)
}

/// Classify gain-map layout from container metadata only (`avifDecoderParse`, no YUV decode).
pub(crate) fn avif_probe_gain_map_strip_kind(bytes: &[u8]) -> Option<AvifGainMapStripProbe> {
    let decoder = libavif_parse_gain_map_container(bytes)?;
    let img = unsafe { libavif_sys::siv_avif_decoder_get_image(decoder.as_ptr()) };
    if img.is_null() {
        return None;
    }
    let image = unsafe { &*img };
    if image.gainMap.is_null() {
        return Some(AvifGainMapStripProbe::NoGainMap);
    }
    let gain_map = unsafe { &*image.gainMap };
    let metadata = avif_gain_map_to_metadata(gain_map).ok()?;
    if iso_gain_map_skips_forward_compose(metadata) {
        Some(AvifGainMapStripProbe::PrecomposedHdr)
    } else {
        Some(AvifGainMapStripProbe::ForwardIsoGainMap)
    }
}
