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

//! Single-decode orchestration for directory-tree AVIF gain-map strip fast paths.

use std::path::Path;

use super::decode::read_avif_decoder_image;
use super::gain_map::avif_gain_map_to_metadata;
use super::strip_baseline::{
    decode_avif_strip_iso_gain_map_baseline_from_image,
    decode_avif_strip_precomposed_hdr_from_image,
};
use crate::hdr::gain_map::iso_gain_map_skips_forward_compose;
use crate::loader::downsample_decoded_for_strip;
use crate::loader::{DecodedImage, preview_aspect_matches_logical};

#[cfg(feature = "avif-native")]
pub(crate) struct AvifGainMapStripFastResult {
    pub preview: DecodedImage,
    pub logical_size: (u32, u32),
}

#[cfg(feature = "avif-native")]
type OptionalFastResult = Option<Result<AvifGainMapStripFastResult, String>>;

#[cfg(feature = "avif-native")]
fn finish_baseline_interim_strip(
    baseline: Vec<u8>,
    width: u32,
    height: u32,
    max_side: u32,
    path: &Path,
) -> Result<AvifGainMapStripFastResult, String> {
    if width == 0 || height == 0 {
        return Err(format!(
            "gain-map strip baseline decode returned zero size for {}",
            path.display()
        ));
    }
    let decoded = DecodedImage::new(width, height, baseline);
    let strip = downsample_decoded_for_strip(&decoded, max_side).map_err(|err| err.to_string())?;
    if !preview_aspect_matches_logical(strip.width, strip.height, width, height) {
        return Err(format!(
            "gain-map strip aspect mismatch: {}x{} vs {width}x{height}",
            strip.width, strip.height
        ));
    }
    Ok(AvifGainMapStripFastResult {
        preview: strip,
        logical_size: (width, height),
    })
}

/// One `read_avif_decoder_image` then precomposed HDR or ISO baseline strip decode.
#[cfg(feature = "avif-native")]
pub(crate) fn try_decode_avif_gain_map_strip_fast(
    bytes: &[u8],
    path: &Path,
    max_side: u32,
) -> OptionalFastResult {
    let image = match read_avif_decoder_image(bytes) {
        Ok(image) => image,
        Err(err) => {
            return Some(Err(format!(
                "{path:?}: decode_avif_gain_map_strip_fast: {err}"
            )));
        }
    };
    let image_ref = unsafe { &*image.as_ptr() };
    if image_ref.gainMap.is_null() {
        return None;
    }
    let gain_map = unsafe { &*image_ref.gainMap };
    let gain_metadata = match avif_gain_map_to_metadata(gain_map) {
        Ok(metadata) => metadata,
        Err(err) => return Some(Err(format!("{path:?}: parse gain map metadata: {err}"))),
    };

    if iso_gain_map_skips_forward_compose(gain_metadata) {
        return decode_avif_strip_precomposed_hdr_from_image(image, path, max_side).map(|opt| {
            opt.map(|(preview, logical_size)| AvifGainMapStripFastResult {
                preview,
                logical_size,
            })
        });
    }

    if let Some(Ok((baseline, width, height))) =
        decode_avif_strip_iso_gain_map_baseline_from_image(&image, path)
    {
        return Some(finish_baseline_interim_strip(
            baseline, width, height, max_side, path,
        ));
    }
    None
}
