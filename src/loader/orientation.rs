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
use std::sync::Arc;

use super::types::{AnimationFrame, DecodedImage, HdrAnimationFrame, ImageData, TiledImageSource};

/// Linear luminance ratio (peak / SDR white) used when **decoding** ISO gain maps (JPEG_R,
/// AVIF, JXL). Probed monitor headroom can exceed [`HdrToneMapSettings::max_display_nits`];
/// using the larger value applies more gain-map weight than the same settings use for SDR
/// previews and Reinhard tone mapping, so the HDR float plane appears too bright.
pub(crate) fn hdr_gain_map_decode_capacity(
    hdr_target_capacity: f32,
    hdr_tone_map: &HdrToneMapSettings,
) -> f32 {
    hdr_target_capacity.min(hdr_tone_map.target_hdr_capacity())
}

fn resolve_exif_orientation(path: &Path, file_bytes: Option<&[u8]>) -> u16 {
    match file_bytes {
        Some(bytes) => crate::metadata_utils::get_exif_orientation_from_bytes(bytes, Some(path)),
        None => crate::metadata_utils::get_exif_orientation(path),
    }
}

/// Apply display **Orientation** (JEITA/TIFF values 1–8) via
/// [`metadata_utils::get_exif_orientation_from_bytes`] / [`metadata_utils::get_exif_orientation`] for
/// formats whose loader **does not** already rotate (AVIF, HEIF, JXL, EXR full decode, radiance small buffer,
/// `image-rs` static decode / memory-backed tiling, …).
///
/// **`get_exif_orientation`** reads embedded EXIF when present; for **`.avif`/`.avifs`** it reads container **`irot`/`imir`**.
/// HEIF (**`.heic`/`.heif`/`.hif`**) uses **`Exif` items**, then **`irot`/`imir`** when geometric properties are rotation/mirror-only
/// (**libheif** decodes those with **`ignore_transformations`** so manual orientation matches libavif semantics).
/// **`.jxl`** uses **`JxlDecoderSetKeepOrientation`** probing of codestream basic info when container EXIF is absent
/// (**libjxl**’s JPEG XL decoder is configured with the same flag so viewers do not rotate twice).
/// **Radiance `.hdr`/`.pic`** orientation comes from the resolution line and is applied during decode
/// (not via this EXIF hook).
///
/// **Never chain on JPEG or TIFF extension loads** — that would double-rotate:
/// - **`.jpg`/`.jpeg`** (incl. **JPEG_R / Ultra HDR**): only the JPEG loader may apply
///   `get_exif_orientation` + [`hdr::ultra_hdr::apply_orientation_to_hdr_buffer`] / [`apply_orientation_buffer`](crate::libtiff_loader::apply_orientation_buffer).
/// - **`.tif`/`.tiff`** (incl. **f16/f32 / scene-linear**): only [`crate::libtiff_loader`] applies the TIFF
///   **`Orientation`** tag (`TIFFTAG_ORIENTATION`), not this function.
///
/// **HdrTiled** (large disk-backed EXR/Radiance) is unchanged: no practical container orientation path here.
///
/// **`static` tiling** via [`crate::loader::tiled_sources::MemoryImageSource`]: rotates when
/// [`TiledImageSource::exif_orientation_rotate_in_memory_rgba`] is true (non-HDR-fallback memory buffers only).
/// Disk-backed TIFF/EXR tile sources omit EXIF rotation here by design ([`crate::libtiff_loader::LibTiffTiledSource`]).
pub(crate) fn apply_exif_orientation_to_image_data(
    path: &Path,
    data: ImageData,
    file_bytes: Option<&[u8]>,
) -> ImageData {
    match data {
        ImageData::Hdr { hdr, fallback } => {
            let (hdr, fallback) =
                apply_exif_orientation_to_hdr_pair(path, *hdr, fallback, file_bytes);
            ImageData::Hdr {
                hdr: Box::new(hdr),
                fallback,
            }
        }
        ImageData::Static(mut img) => {
            let o = resolve_exif_orientation(path, file_bytes);
            if o <= 1 {
                return ImageData::Static(img);
            }
            let w = img.width;
            let h = img.height;
            let pixels_arc = img.take_pixels_arc();
            let (ow, oh, opx) = match Arc::try_unwrap(pixels_arc) {
                Ok(px) => crate::libtiff_loader::apply_orientation_buffer(px, w, h, o),
                Err(arc) => crate::libtiff_loader::apply_orientation_buffer_from_slice(
                    arc.as_ref(),
                    w,
                    h,
                    o,
                ),
            };
            img.set_rgba_buffer(ow, oh, opx);
            ImageData::Static(img)
        }
        ImageData::Tiled(source) => {
            if !TiledImageSource::exif_orientation_rotate_in_memory_rgba(source.as_ref()) {
                return ImageData::Tiled(source);
            }
            let o = resolve_exif_orientation(path, file_bytes);
            if o <= 1 {
                return ImageData::Tiled(source);
            }
            let w = source.width();
            let h = source.height();
            let Some(full_px) = source.full_pixels() else {
                return ImageData::Tiled(source);
            };
            drop(source);
            let (ow, oh, opx) =
                crate::libtiff_loader::apply_orientation_buffer_from_slice(&full_px, w, h, o);
            let rebuilt =
                crate::loader::tiled_sources::MemoryImageSource::new(ow, oh, Arc::new(opx));
            ImageData::Tiled(Arc::new(rebuilt))
        }
        ImageData::Animated(frames) => {
            let o = resolve_exif_orientation(path, file_bytes);
            if o <= 1 || frames.is_empty() {
                return ImageData::Animated(frames);
            }
            let out = frames
                .into_iter()
                .map(|f| {
                    let (ow, oh, opx) = crate::libtiff_loader::apply_orientation_buffer_from_slice(
                        f.rgba(),
                        f.width,
                        f.height,
                        o,
                    );
                    AnimationFrame::new(ow, oh, opx, f.delay)
                })
                .collect();
            ImageData::Animated(out)
        }
        ImageData::HdrAnimated(frames) => {
            // Resolve Orientation once for the whole sequence -- do not re-scan
            // ISOBMFF/EXIF per frame via apply_exif_orientation_to_hdr_pair.
            #[cfg(feature = "heif-native")]
            let mut o = resolve_exif_orientation(path, file_bytes);
            #[cfg(not(feature = "heif-native"))]
            let o = resolve_exif_orientation(path, file_bytes);
            if o <= 1 || frames.is_empty() {
                return ImageData::HdrAnimated(frames);
            }
            #[cfg(feature = "heif-native")]
            if let Some(first) = frames.first()
                && crate::hdr::heif::decoded_pixels_match_swapped_ispe(
                    path,
                    first.hdr.width,
                    first.hdr.height,
                    file_bytes,
                )
            {
                o = 1;
            }
            #[cfg(feature = "heif-native")]
            if o <= 1 {
                return ImageData::HdrAnimated(frames);
            }
            let out = frames
                .into_iter()
                .map(|frame| {
                    let (hdr, fallback) =
                        apply_orientation_to_hdr_pair_known(o, frame.hdr, frame.fallback);
                    HdrAnimationFrame::new(hdr, fallback, frame.delay)
                })
                .collect();
            ImageData::HdrAnimated(out)
        }
        other => other,
    }
}

/// Apply a **known** JEITA/TIFF Orientation (1-8) to an HDR + SDR fallback pair.
/// Callers that already resolved Orientation (e.g. animated sequences) must use this
/// instead of [`apply_exif_orientation_to_hdr_pair`] to avoid repeated EXIF/ISOBMFF scans.
fn apply_orientation_to_hdr_pair_known(
    orientation: u16,
    hdr: crate::hdr::types::HdrImageBuffer,
    fallback: DecodedImage,
) -> (crate::hdr::types::HdrImageBuffer, DecodedImage) {
    if orientation <= 1 {
        return (hdr, fallback);
    }
    let hdr = crate::hdr::ultra_hdr::apply_orientation_to_hdr_buffer(hdr, orientation);
    let w = fallback.width;
    let h = fallback.height;
    let mut fallback = fallback;
    let pixels_arc = fallback.take_pixels_arc();
    let (ow, oh, opx) = match Arc::try_unwrap(pixels_arc) {
        Ok(px) => crate::libtiff_loader::apply_orientation_buffer(px, w, h, orientation),
        Err(arc) => crate::libtiff_loader::apply_orientation_buffer_from_slice(
            arc.as_ref(),
            w,
            h,
            orientation,
        ),
    };
    fallback.set_rgba_buffer_preserving_placeholder(ow, oh, opx, true);
    (hdr, fallback)
}

pub(crate) fn apply_exif_orientation_to_hdr_pair(
    path: &Path,
    hdr: crate::hdr::types::HdrImageBuffer,
    fallback: DecodedImage,
    file_bytes: Option<&[u8]>,
) -> (crate::hdr::types::HdrImageBuffer, DecodedImage) {
    #[cfg(feature = "heif-native")]
    let mut o = resolve_exif_orientation(path, file_bytes);
    #[cfg(not(feature = "heif-native"))]
    let o = resolve_exif_orientation(path, file_bytes);
    #[cfg(feature = "heif-native")]
    if crate::hdr::heif::decoded_pixels_match_swapped_ispe(path, hdr.width, hdr.height, file_bytes)
    {
        o = 1;
    }
    apply_orientation_to_hdr_pair_known(o, hdr, fallback)
}
