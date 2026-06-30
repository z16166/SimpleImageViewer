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

//! Fast directory-tree strip decode for ISO gain-map AVIF (baseline only, no gain-map plane RGB).

use std::path::Path;

use super::avif_cicp_to_metadata;
use super::decode::{
    decode_avif_image_rgba_u16, libavif_result_to_string, read_avif_decoder_image,
};
use super::gain_map::avif_gain_map_to_metadata;
use super::metadata::{AvifMetadataExt, avif_yuv_to_rgb_output_metadata};
use crate::hdr::avif_gain_map_deferred::avif_build_iso_sdr_baseline_rgba8;
use crate::hdr::gain_map::iso_gain_map_skips_forward_compose;
use crate::loader::downsample_decoded_for_strip;
use crate::loader::{DecodedImage, preview_aspect_matches_logical};

#[cfg(feature = "avif-native")]
type StripWithLogicalSize = (DecodedImage, (u32, u32));
#[cfg(feature = "avif-native")]
type OptionalStripResult<T> = Option<Result<T, String>>;

/// Directory-tree strip via embedded EXIF thumbnail + parse-only logical size (no HDR pixel decode).
#[cfg(feature = "avif-native")]
pub(crate) fn decode_avif_strip_exif_thumbnail(
    bytes: &[u8],
    path: &Path,
    max_side: u32,
) -> OptionalStripResult<StripWithLogicalSize> {
    let exif = crate::loader::extract_exif_thumbnail_from_bytes(bytes, path)?;
    let (logical_w, logical_h) = super::orientation::libavif_probe_logical_size_from_bytes(bytes)?;
    if !preview_aspect_matches_logical(exif.width, exif.height, logical_w, logical_h) {
        return None;
    }
    let strip = downsample_decoded_for_strip(&exif, max_side).ok()?;
    if !preview_aspect_matches_logical(strip.width, strip.height, logical_w, logical_h) {
        return None;
    }
    Some(Ok((strip, (logical_w, logical_h))))
}

/// Decode ISO forward gain-map AVIF primary layer to SDR baseline RGBA8 only.
///
/// Skips [`super::gain_map::decode_avif_gain_map`] (second full-plane YUV to RGB).
///
/// Returns:
/// - `None` — file has no forward ISO gain map (precomposed HDR or no gain map at all);
///   callers should try alternate decode paths.
/// - `Some(Err(...))` — file has a forward ISO gain map but decoding failed;
///   callers should propagate the error rather than silently falling back.
/// - `Some(Ok(...))` — baseline RGBA8 pixels ready for downsampling.
#[cfg(feature = "avif-native")]
pub(crate) fn decode_avif_strip_iso_gain_map_baseline_from_image(
    image: libavif_sys::AvifImageOwned,
    path: &Path,
) -> OptionalStripResult<(Vec<u8>, u32, u32)> {
    let image_ptr = image.as_ptr();
    let image_ref = unsafe { &*image_ptr };
    if image_ref.gainMap.is_null() {
        return None;
    }
    let gain_map = unsafe { &*image_ref.gainMap };
    let gain_metadata = match avif_gain_map_to_metadata(gain_map) {
        Ok(metadata) => metadata,
        Err(err) => return Some(Err(format!("{path:?}: parse gain map metadata: {err}"))),
    };
    if iso_gain_map_skips_forward_compose(gain_metadata) {
        return None;
    }

    let width = image_ref.width;
    let height = image_ref.height;
    if width == 0 || height == 0 {
        return Some(Err(format!("{path:?}: libavif decoded zero-sized image")));
    }

    let metadata = avif_cicp_to_metadata(
        image_ref.colorPrimaries,
        image_ref.transferCharacteristics,
        image_ref.matrixCoefficients,
        image_ref.yuvRange == libavif_sys::AVIF_RANGE_FULL,
    )
    .with_clli(image_ref.clli.maxCLL, image_ref.clli.maxPALL);
    let metadata = avif_yuv_to_rgb_output_metadata(&metadata, image_ref);
    let color_space = metadata.color_space_hint();

    let (rgba_u16, rgb_out_depth) =
        match decode_avif_image_rgba_u16(image_ptr, image_ref, &libavif_result_to_string) {
            Ok(ok) => ok,
            Err(err) => {
                return Some(Err(format!(
                    "{path:?}: decode ISO gain-map RGBA u16: {err}"
                )));
            }
        };

    let baseline = avif_build_iso_sdr_baseline_rgba8(
        &rgba_u16,
        rgb_out_depth,
        width,
        height,
        &metadata,
        color_space,
    );
    Some(Ok((baseline, width, height)))
}

/// Standalone bytes entry for tests; production uses [`super::strip_fast`].
#[cfg(feature = "avif-native")]
#[allow(dead_code)]
pub(crate) fn decode_avif_strip_iso_gain_map_baseline(
    bytes: &[u8],
    path: &Path,
) -> OptionalStripResult<(Vec<u8>, u32, u32)> {
    let image = match read_avif_decoder_image(bytes) {
        Ok(image) => image,
        Err(err) => return Some(Err(format!("{path:?}: decode_avif_strip_iso: {err}"))),
    };
    decode_avif_strip_iso_gain_map_baseline_from_image(image, path)
}

/// Fast directory-tree strip for precomposed PQ/HLG AVIF (`base_hdr` layout).
#[cfg(feature = "avif-native")]
pub(crate) fn decode_avif_strip_precomposed_hdr_from_image(
    image: libavif_sys::AvifImageOwned,
    path: &Path,
    max_side: u32,
) -> OptionalStripResult<StripWithLogicalSize> {
    let image_ptr = image.as_ptr();
    let image_ref = unsafe { &*image_ptr };
    if image_ref.gainMap.is_null() {
        return None;
    }
    let gain_map = unsafe { &*image_ref.gainMap };
    let gain_metadata = match avif_gain_map_to_metadata(gain_map) {
        Ok(metadata) => metadata,
        Err(err) => return Some(Err(format!("{path:?}: parse gain map metadata: {err}"))),
    };
    if !iso_gain_map_skips_forward_compose(gain_metadata) {
        return None;
    }

    let hdr = match super::avif_image_to_hdr_buffer(image_ptr, 1.0) {
        Ok(hdr) => hdr,
        Err(err) => return Some(Err(format!("{path:?}: convert to HDR buffer: {err}"))),
    };
    if hdr.rgba_f32.is_empty() {
        return Some(Err(format!(
            "{path:?}: precomposed AVIF strip requires float HDR pixels ({}x{})",
            hdr.width, hdr.height
        )));
    }
    let logical = (hdr.width, hdr.height);
    let (width, height, pixels) =
        match crate::loader::hdr_directory_tree_strip_sdr_at_max_side(&hdr, max_side) {
            Ok(ok) => ok,
            Err(err) => return Some(Err(format!("{path:?}: tone-map HDR strip: {err}"))),
        };
    Some(Ok((
        crate::loader::DecodedImage::new(width, height, pixels),
        logical,
    )))
}

/// Fast directory-tree strip for precomposed PQ/HLG AVIF (`base_hdr` layout).
///
/// Skips gain-map plane RGB and full [`ImageData`] assembly; tone-maps a downsampled HDR buffer.
/// Standalone bytes entry for tests; production uses [`super::strip_fast`].
#[cfg(feature = "avif-native")]
#[allow(dead_code)]
pub(crate) fn decode_avif_strip_precomposed_hdr(
    bytes: &[u8],
    path: &Path,
    max_side: u32,
) -> OptionalStripResult<StripWithLogicalSize> {
    let image = match read_avif_decoder_image(bytes) {
        Ok(image) => image,
        Err(err) => {
            return Some(Err(format!(
                "{path:?}: decode_avif_strip_precomposed: {err}"
            )));
        }
    };
    decode_avif_strip_precomposed_hdr_from_image(image, path, max_side)
}
