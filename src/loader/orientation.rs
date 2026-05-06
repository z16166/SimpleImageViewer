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

use crate::hdr::types::HdrToneMapSettings;
use std::path::Path;

use super::types::{AnimationFrame, DecodedImage, ImageData};

/// Linear luminance ratio (peak / SDR white) used when **decoding** ISO gain maps (JPEG_R,
/// AVIF, JXL). Probed monitor headroom can exceed [`HdrToneMapSettings::max_display_nits`];
/// using the larger value applies more gain-map weight than the same settings use for SDR
/// previews and Reinhard tone mapping, so the HDR float plane appears too bright.
pub(crate) fn hdr_gain_map_decode_capacity(hdr_target_capacity: f32, hdr_tone_map: &HdrToneMapSettings) -> f32 {
    hdr_target_capacity.min(hdr_tone_map.target_hdr_capacity())
}

/// Apply EXIF **Orientation** (values 1–8) via [`metadata_utils::get_exif_orientation`] for formats whose
/// loader **does not** already rotate (AVIF, HEIF, JXL, EXR full decode, radiance small buffer, …).
///
/// **Never chain on JPEG or TIFF extension loads** — that would double-rotate:
/// - **`.jpg`/`.jpeg`** (incl. **JPEG_R / Ultra HDR**): only the JPEG loader may apply
///   `get_exif_orientation` + [`hdr::ultra_hdr::apply_orientation_to_hdr_buffer`] / [`apply_orientation_buffer`](crate::libtiff_loader::apply_orientation_buffer).
/// - **`.tif`/`.tiff`** (incl. **f16/f32 / scene-linear**): only [`crate::libtiff_loader`] applies the TIFF
///   **`Orientation`** tag (`TIFFTAG_ORIENTATION`), not this function.
///
/// **HdrTiled** (large disk-backed EXR/Radiance) is unchanged: no practical container orientation path here.
pub(crate) fn apply_exif_orientation_to_image_data(path: &Path, data: ImageData) -> ImageData {
    match data {
        ImageData::Hdr { hdr, fallback } => {
            let (hdr, fallback) = apply_exif_orientation_to_hdr_pair(path, hdr, fallback);
            ImageData::Hdr { hdr, fallback }
        }
        ImageData::Animated(frames) => {
            let o = crate::metadata_utils::get_exif_orientation(path);
            if o <= 1 || frames.is_empty() {
                return ImageData::Animated(frames);
            }
            let out = frames
                .into_iter()
                .map(|f| {
                    let px = f.rgba().to_vec();
                    let (ow, oh, opx) =
                        crate::libtiff_loader::apply_orientation_buffer(px, f.width, f.height, o);
                    AnimationFrame::new(ow, oh, opx, f.delay)
                })
                .collect();
            ImageData::Animated(out)
        }
        other => other,
    }
}

pub(crate) fn apply_exif_orientation_to_hdr_pair(
    path: &Path,
    hdr: crate::hdr::types::HdrImageBuffer,
    fallback: DecodedImage,
) -> (crate::hdr::types::HdrImageBuffer, DecodedImage) {
    let mut o = crate::metadata_utils::get_exif_orientation(path);
    #[cfg(feature = "heif-native")]
    if crate::hdr::heif::decoded_pixels_match_swapped_ispe(path, hdr.width, hdr.height) {
        o = 1;
    }
    if o <= 1 {
        return (hdr, fallback);
    }
    let hdr = crate::hdr::ultra_hdr::apply_orientation_to_hdr_buffer(hdr, o);
    let w = fallback.width;
    let h = fallback.height;
    let mut fallback = fallback;
    let px = fallback.take_rgba_owned();
    let (ow, oh, opx) = crate::libtiff_loader::apply_orientation_buffer(px, w, h, o);
    fallback.set_rgba_buffer(ow, oh, opx);
    (hdr, fallback)
}
