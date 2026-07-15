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

//! AVIF ISO gain-map HDR loaded as embedded SDR master only (skip gain-map plane decode).

use std::path::Path;

use super::decode::read_avif_decoder_image;
use super::gain_map::avif_gain_map_to_metadata;
use super::strip_baseline::decode_avif_strip_iso_gain_map_baseline_from_image;
use crate::hdr::gain_map::iso_gain_map_skips_forward_compose;
use crate::hdr::jpeg_gain_map_gpu::attach_iso_embedded_sdr_master_only;
use crate::hdr::types::GAIN_MAP_SOURCE_AVIF;
use crate::loader::{DecodedImage, ImageData, apply_exif_orientation_to_hdr_pair};

#[cfg(feature = "avif-native")]
pub(crate) fn try_avif_embedded_sdr_from_decoded_image(
    image: &libavif_sys::AvifImageOwned,
    bytes: &[u8],
    path: &Path,
) -> Result<ImageData, String> {
    let image_ref = unsafe { &*image.as_ptr() };
    if image_ref.gainMap.is_null() {
        return Err(format!(
            "{path:?}: AVIF has no gain map for embedded SDR master load"
        ));
    }
    let gain_map = unsafe { &*image_ref.gainMap };
    let gain_metadata = avif_gain_map_to_metadata(gain_map)
        .map_err(|err| format!("{path:?}: parse gain map metadata: {err}"))?;
    if iso_gain_map_skips_forward_compose(gain_metadata) {
        return Err(format!(
            "{path:?}: AVIF primary is HDR base; embedded SDR master load is invalid"
        ));
    }

    let (sdr_rgba, width, height) =
        match decode_avif_strip_iso_gain_map_baseline_from_image(image, path) {
            Some(Ok((baseline, width, height))) => (baseline, width, height),
            Some(Err(err)) => return Err(err),
            None => {
                return Err(format!(
                    "{path:?}: AVIF has no forward ISO gain map for embedded SDR master load"
                ));
            }
        };

    let hdr = attach_iso_embedded_sdr_master_only(
        GAIN_MAP_SOURCE_AVIF,
        width,
        height,
        sdr_rgba,
        gain_metadata,
    )?;
    let fallback = DecodedImage::new(width, height, {
        hdr.metadata
            .gain_map
            .as_ref()
            .and_then(|gain_map| gain_map.iso_deferred.as_ref())
            .map(|iso| (*iso.sdr_rgba).clone())
            .ok_or_else(|| "AVIF embedded SDR master missing baseline pixels".to_string())?
    });
    let (hdr, fallback) = apply_exif_orientation_to_hdr_pair(path, hdr, fallback, Some(bytes));

    Ok(ImageData::Hdr {
        hdr: Box::new(hdr),
        fallback,
    })
}

#[cfg(feature = "avif-native")]
#[allow(dead_code)] // Path-based wrapper; production uses `decode_avif_static_with_optional_embedded_sdr`.
pub(crate) fn load_avif_embedded_sdr_master(path: &Path) -> Result<ImageData, String> {
    #[cfg(feature = "preload-debug")]
    let total_start = std::time::Instant::now();

    let (mmap, _) =
        crate::mmap_util::map_file(path).map_err(|err| format!("Failed to read AVIF: {err}"))?;
    let image = read_avif_decoder_image(&mmap[..])?;
    let image_data = try_avif_embedded_sdr_from_decoded_image(&image, &mmap[..], path)?;

    #[cfg(feature = "preload-debug")]
    {
        let total_ms = total_start.elapsed().as_millis();
        if let ImageData::Hdr { hdr, .. } = &image_data {
            crate::preload_debug!(
                "[PreloadDebug][AVIF] embedded_sdr_master total_ms={total_ms} path={} size={}x{}",
                path.display(),
                hdr.width,
                hdr.height
            );
        }
    }

    Ok(image_data)
}
