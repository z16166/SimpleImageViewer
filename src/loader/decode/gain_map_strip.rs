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

//! Fast directory-tree strip decode for ISO gain-map AVIF / JPEG XL (baseline only).

use std::path::Path;

use crate::loader::downsample_decoded_for_strip;
use crate::loader::{DecodedImage, preview_aspect_matches_logical};

fn finish_gain_map_strip(
    baseline: Vec<u8>,
    width: u32,
    height: u32,
    max_side: u32,
    path: &Path,
) -> Result<(DecodedImage, (u32, u32)), String> {
    if width == 0 || height == 0 {
        return Err(format!(
            "gain-map strip baseline decode returned zero size for {}",
            path.display()
        ));
    }
    let decoded = DecodedImage::new(width, height, baseline);
    let strip = downsample_decoded_for_strip(&decoded, max_side)
        .map_err(|err| err.to_string())?
        .into_owned();
    if !preview_aspect_matches_logical(strip.width, strip.height, width, height) {
        return Err(format!(
            "gain-map strip aspect mismatch: {}x{} vs {width}x{height}",
            strip.width, strip.height
        ));
    }
    Ok((strip, (width, height)))
}

/// Try a lightweight ISO gain-map baseline decode for directory-tree strips.
///
/// When `file_bytes` is `Some`, uses the caller's mmap (avoids a second open per checklist #29).
/// Returns `None` when the path is not a supported gain-map container or the file uses a
/// precomposed HDR primary (handled by the normal loader path). Ultra HDR JPEG is unchanged.
pub(crate) fn try_fast_iso_gain_map_strip_from_path(
    path: &Path,
    file_bytes: Option<&[u8]>,
    max_side: u32,
) -> Option<Result<(DecodedImage, (u32, u32)), String>> {
    let ext = path
        .extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let owned_mmap;
    let bytes = match file_bytes {
        Some(bytes) => bytes,
        None => {
            owned_mmap = crate::mmap_util::map_file(path).ok()?;
            owned_mmap.as_ref()
        }
    };

    if ext == "jxl" {
        #[cfg(feature = "jpegxl")]
        {
            if let Some(preview) = crate::hdr::jpegxl::decode_jxl_strip_preview_rgba8(bytes) {
                return Some(preview.and_then(|(rgba8, pw, ph, lw, lh)| {
                    #[cfg(feature = "preload-debug")]
                    crate::preload_debug!(
                        "[PreloadDebug][Strip] jxl preview path {} preview={}x{} logical={}x{}",
                        path.display(),
                        pw,
                        ph,
                        lw,
                        lh
                    );
                    let decoded = DecodedImage::new(pw, ph, rgba8);
                    let strip = downsample_decoded_for_strip(&decoded, max_side)
                        .map_err(|err| err.to_string())?
                        .into_owned();
                    if !preview_aspect_matches_logical(strip.width, strip.height, lw, lh) {
                        return Err(format!(
                            "jxl preview strip aspect mismatch: {}x{} vs {lw}x{lh}",
                            strip.width, strip.height
                        ));
                    }
                    Ok((strip, (lw, lh)))
                }));
            }
            match crate::hdr::jpegxl::decode_jxl_strip_iso_gain_map_baseline(bytes) {
                Ok((baseline, width, height)) => {
                    return Some(finish_gain_map_strip(baseline, width, height, max_side, path));
                }
                Err(err) => {
                    if err.contains("strip baseline") || err.contains("jhgm") {
                        return None;
                    }
                    return Some(Err(err));
                }
            }
        }
        #[cfg(not(feature = "jpegxl"))]
        return None;
    }

    if ext == "avif" || ext == "avifs" {
        #[cfg(feature = "avif-native")]
        {
            if let Some(result) =
                crate::hdr::avif::decode_avif_strip_exif_thumbnail(bytes, path, max_side)
            {
                return Some(result);
            }
            if let Some(result) = crate::hdr::avif::decode_avif_strip_precomposed_hdr(bytes, max_side)
            {
                return Some(result);
            }
            let result = crate::hdr::avif::decode_avif_strip_iso_gain_map_baseline(bytes)?;
            return Some(result.and_then(|(baseline, width, height)| {
                finish_gain_map_strip(baseline, width, height, max_side, path)
            }));
        }
        #[cfg(not(feature = "avif-native"))]
        return None;
    }

    None
}
