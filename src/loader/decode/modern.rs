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

//! AVIF, JPEG XL, HEIF/HIF loaders.

use crate::hdr::types::HdrToneMapSettings;
use crate::loader::{
    apply_exif_orientation_to_hdr_pair, apply_exif_orientation_to_image_data,
    hdr_gain_map_decode_capacity, hdr_sdr_fallback_rgba8_eager_or_placeholder,
    AnimationFrame, DecodedImage, ImageData,
};
use std::path::{Path, PathBuf};

use super::assemble::make_hdr_image_data;

#[allow(dead_code)]
pub(crate) fn is_avif_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("avif") || ext.eq_ignore_ascii_case("avifs"))
}

#[allow(dead_code)]
pub(crate) fn is_heif_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            ext.eq_ignore_ascii_case("heic")
                || ext.eq_ignore_ascii_case("heif")
                || ext.eq_ignore_ascii_case("hif")
        })
}

#[allow(dead_code)]
pub(crate) fn is_jxl_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("jxl"))
}

pub(crate) fn is_hdr_capable_modern_format_path(path: &Path) -> bool {
    is_avif_path(path) || is_heif_path(path) || is_jxl_path(path)
}

pub(crate) fn load_avif_with_target_capacity(
    path: &PathBuf,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
) -> Result<ImageData, String> {
    #[cfg(feature = "avif-native")]
    {
        let mmap = crate::mmap_util::map_file(path)
            .map_err(|e| format!("Failed to read AVIF: {e}"))?;

        match crate::hdr::avif::try_decode_avif_image_sequence_sdr(&mmap[..]) {
            Ok(Some(raw)) if raw.len() > 1 => {
                let frames: Vec<AnimationFrame> = raw
                    .into_iter()
                    .map(|(delay, w, h, px)| AnimationFrame::new(w, h, px, delay))
                    .collect();
                log::info!(
                    "[Loader] AVIF image sequence: {} frames (SDR RGBA8) — {}",
                    frames.len(),
                    path.display()
                );
                return Ok(apply_exif_orientation_to_image_data(
                    path.as_path(),
                    ImageData::Animated(frames),
                ));
            }
            Ok(_) => {}
            Err(e) => {
                log::debug!(
                    "[Loader] AVIF sequence decode failed for {} ({e}); trying static HDR path",
                    path.display()
                );
            }
        }

        let decode_capacity = hdr_gain_map_decode_capacity(hdr_target_capacity, &hdr_tone_map);
        match crate::hdr::avif::decode_avif_hdr_bytes_with_target_capacity(&mmap[..], decode_capacity) {
            Ok(hdr) => {
                let fallback_pixels = hdr_sdr_fallback_rgba8_eager_or_placeholder(
                    &hdr,
                    hdr_target_capacity,
                    &hdr_tone_map,
                )?;
                let fallback = DecodedImage::new(hdr.width, hdr.height, fallback_pixels);
                let (hdr, fallback) =
                    apply_exif_orientation_to_hdr_pair(path.as_path(), hdr, fallback);
                Ok(make_hdr_image_data(hdr, fallback))
            }
            Err(err) => {
                log::warn!(
                    "[Loader] libavif decode failed for {}: {err}",
                    path.display()
                );
                #[cfg(all(feature = "avif-native", feature = "heif-native"))]
                {
                    let lower = err.to_ascii_lowercase();
                    if lower.contains("invalid ftyp")
                        || lower.contains("ftyp")
                        || lower.contains("file type box")
                    {
                        log::info!(
                            "[Loader] libavif rejected container/brands — trying libheif for {}",
                            path.display()
                        );
                        return load_heif_hdr_aware(path, hdr_target_capacity, hdr_tone_map)
                            .map_err(|heif_err| {
                                format!(
                                    "[Loader] libavif failed ({err}); HEIF fallback also failed ({heif_err})"
                                )
                            });
                    }
                }
                Err(err)
            }
        }
    }

    #[cfg(not(feature = "avif-native"))]
    {
        let _ = (path, hdr_target_capacity, hdr_tone_map);
        Err(
            "AVIF decoding requires the avif-native feature (e.g. hdr-modern-formats)."
                .to_string(),
        )
    }
}

pub(crate) fn load_jxl_with_target_capacity(
    path: &PathBuf,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
) -> Result<ImageData, String> {
    #[cfg(feature = "jpegxl")]
    {
        let decode_capacity = hdr_gain_map_decode_capacity(hdr_target_capacity, &hdr_tone_map);
        let data = crate::hdr::jpegxl::load_jxl_hdr_with_target_capacity(
            path,
            decode_capacity,
            hdr_target_capacity,
            hdr_tone_map,
        )?;
        Ok(apply_exif_orientation_to_image_data(path.as_path(), data))
    }

    #[cfg(not(feature = "jpegxl"))]
    {
        let _ = (path, hdr_target_capacity, hdr_tone_map);
        Err("JPEG XL support requires the jpegxl feature".to_string())
    }
}

pub(crate) fn load_heif_hdr_aware(
    path: &PathBuf,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
) -> Result<ImageData, String> {
    #[cfg(feature = "heif-native")]
    {
        match crate::hdr::heif::load_heif_hdr(path, hdr_target_capacity, hdr_tone_map) {
            Ok(image) => Ok(apply_exif_orientation_to_image_data(path.as_path(), image)),
            Err(err) => {
                log::warn!(
                    "[Loader] libheif decode failed for {}: {err}",
                    path.display()
                );
                Err(err)
            }
        }
    }

    #[cfg(not(feature = "heif-native"))]
    {
        let _ = (path, hdr_target_capacity, hdr_tone_map);
        Err(
            "HEIF/HEIC decoding requires the heif-native feature (e.g. hdr-modern-formats)."
                .to_string(),
        )
    }
}
