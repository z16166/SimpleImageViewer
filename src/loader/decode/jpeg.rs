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

//! Baseline JPEG and Ultra HDR (JPEG_R).

use crate::hdr::types::HdrToneMapSettings;
use crate::loader::{DecodedImage, ImageData};
use crate::loader::{hdr_gain_map_decode_capacity, hdr_sdr_fallback_rgba8_eager_or_placeholder};
use std::path::PathBuf;
use std::sync::Arc;

use super::assemble::{make_hdr_image_data, make_image_data};
use crate::loader::tiled_sources::MemoryImageSource;

#[cfg(test)]
pub(crate) fn load_jpeg(path: &PathBuf) -> Result<ImageData, String> {
    load_jpeg_with_target_capacity(
        path,
        HdrToneMapSettings::default().target_hdr_capacity(),
        HdrToneMapSettings::default(),
    )
}

pub(crate) fn load_jpeg_with_target_capacity(
    path: &PathBuf,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
) -> Result<ImageData, String> {
    let mmap = crate::mmap_util::map_file(path)?;
    load_jpeg_from_mapped(path, &mmap, hdr_target_capacity, hdr_tone_map)
}

pub(crate) fn load_jpeg_from_mapped(
    path: &PathBuf,
    mmap: &memmap2::Mmap,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
) -> Result<ImageData, String> {
    let decode_capacity = hdr_gain_map_decode_capacity(hdr_target_capacity, &hdr_tone_map);
    if mmap.len() < 3 || !mmap.starts_with(&[0xFF, 0xD8, 0xFF]) {
        if let Some(brand) = super::detect::bmff_ftyp_brand(mmap)
            && super::detect::is_motion_video_bmff_brand(&brand)
        {
            return Err(super::detect::motion_video_bmff_error(&brand));
        }
        return Err(format!(
            "not a JPEG bitstream (header {:02x?}); file extension may not match container",
            &mmap[..mmap.len().min(4)]
        ));
    }
    // Sole orientation pass for all JPEG decodes (baseline SDR, **JPEG_R / Ultra HDR**). Do not
    // combine with [`apply_exif_orientation_to_image_data`] — that would double-rotate.
    let orientation = crate::metadata_utils::get_exif_orientation(path);
    // Apply EXIF Orientation per TIFF/EXIF rules (same transform family as Pillow `exif_transpose`).
    // Some reference JPEGs (e.g. libavif `paris_exif_orientation_5.jpg`) store a raster that already
    // looks like a normal landscape before correction; the tag still requests transpose, so the
    // result can differ from viewers that ignore the tag or use heuristics.
    match crate::hdr::ultra_hdr::decode_ultra_hdr_jpeg_bytes_with_target_capacity(
        mmap,
        decode_capacity,
    ) {
        Ok(hdr) => {
            let pixel_count = hdr.width as u64 * hdr.height as u64;
            let tiled_limit =
                crate::tile_cache::TILED_THRESHOLD.load(std::sync::atomic::Ordering::Relaxed);
            let max_side = hdr.width.max(hdr.height);
            let use_tiled_deferred = hdr.rgba_f32.is_empty()
                && crate::hdr::jpeg_gain_map_gpu::iso_deferred_from_metadata(&hdr.metadata)
                    .is_some()
                && (pixel_count >= tiled_limit
                    || max_side >= crate::constants::ABSOLUTE_MAX_TEXTURE_SIDE);
            if use_tiled_deferred {
                let (mut w, mut h, mut pixels) = libjpeg_turbo::decode_to_rgba(mmap)?;
                if orientation > 1 {
                    let oriented =
                        crate::libtiff_loader::apply_orientation_buffer(pixels, w, h, orientation);
                    w = oriented.0;
                    h = oriented.1;
                    pixels = oriented.2;
                }
                if let Ok(hdr_source) =
                    crate::hdr::ultra_hdr::UltraHdrTiledImageSource::open_with_target_capacity(
                        path.clone(),
                        orientation,
                        decode_capacity,
                    )
                {
                    let fallback = Arc::new(MemoryImageSource::new_with_hdr_sdr_fallback(
                        w,
                        h,
                        Arc::new(pixels),
                        true,
                    ));
                    return Ok(ImageData::HdrTiled {
                        hdr: Arc::new(hdr_source),
                        fallback,
                    });
                }
            }

            let hdr = crate::hdr::ultra_hdr::apply_orientation_to_hdr_buffer(hdr, orientation);
            let fallback = DecodedImage::from_hdr_sdr_fallback(
                hdr.width,
                hdr.height,
                hdr_sdr_fallback_rgba8_eager_or_placeholder(
                    &hdr,
                    hdr_target_capacity,
                    &hdr_tone_map,
                )?,
            );
            return Ok(make_hdr_image_data(hdr, fallback));
        }
        Err(err) => {
            if crate::hdr::ultra_hdr::inspect_ultra_hdr_jpeg_bytes(mmap)
                .ok()
                .is_some_and(|info| info.is_ultra_hdr)
            {
                log::warn!(
                    "[Loader] Ultra HDR JPEG decode failed for {}: {err}; falling back to baseline SDR (no HDR OSD)",
                    path.display()
                );
            }
        }
    }

    let (mut w, mut h, mut pixels) = libjpeg_turbo::decode_to_rgba(mmap)?;

    if orientation > 1 {
        let (out_w, out_h, out_pixels) =
            crate::libtiff_loader::apply_orientation_buffer(pixels, w, h, orientation);
        w = out_w;
        h = out_h;
        pixels = out_pixels;
    }

    Ok(make_image_data(DecodedImage::new(w, h, pixels)))
}

/// Strip preview fast path: decode a baseline JPEG with DCT-domain scaling.
///
/// Returns `None` when this is an Ultra HDR / JPEG_R image that must go through the
/// full HDR-aware decode path.  Otherwise returns the DCT-scaled thumbnail plus the
/// original (logical) image dimensions.
pub(crate) fn try_decode_jpeg_strip_dct(
    jpeg_data: &[u8],
    max_side: u32,
) -> Option<Result<(DecodedImage, (u32, u32)), String>> {
    // Ultra HDR / JPEG_R images must go through the full HDR-aware decode path.
    if crate::hdr::ultra_hdr::inspect_ultra_hdr_jpeg_bytes(jpeg_data)
        .ok()
        .is_some_and(|info| info.is_ultra_hdr)
    {
        return None;
    }

    // Use the bytes variant to avoid re-opening the already mmap'd file
    // (checklist #29 — "avoid opening the same file multiple times").
    let orientation = crate::metadata_utils::get_exif_orientation_from_bytes(jpeg_data);
    let (orig_w, orig_h, scaled_w, scaled_h, pixels) =
        match libjpeg_turbo::decode_to_rgba_with_max_side(jpeg_data, max_side) {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
    // Logical = oriented original dimensions (rotation swaps width/height).
    let logical = if orientation > 4 {
        (orig_h, orig_w)
    } else {
        (orig_w, orig_h)
    };

    if orientation > 1 {
        let (out_w, out_h, out_pixels) = crate::libtiff_loader::apply_orientation_buffer(
            pixels,
            scaled_w,
            scaled_h,
            orientation,
        );
        Some(Ok((DecodedImage::new(out_w, out_h, out_pixels), logical)))
    } else {
        Some(Ok((DecodedImage::new(scaled_w, scaled_h, pixels), logical)))
    }
}

#[cfg(test)]
mod tests {
    use super::load_jpeg_with_target_capacity;
    use crate::hdr::types::HdrToneMapSettings;
    use std::path::PathBuf;

    #[test]
    fn mislabeled_quicktime_jpg_errors_on_first_mmap_pass() {
        let Some(path) = std::env::var_os("SIV_QT_JPG_SAMPLE").map(PathBuf::from) else {
            eprintln!("skip; set SIV_QT_JPG_SAMPLE");
            return;
        };
        let settings = HdrToneMapSettings::default();
        let err =
            match load_jpeg_with_target_capacity(&path, settings.target_hdr_capacity(), settings) {
                Err(err) => err,
                Ok(_) => panic!("expected QuickTime mislabeled JPG to fail"),
            };
        assert!(
            err.contains(crate::loader::decode::detect::MOTION_VIDEO_BMFF_ERROR_TAG),
            "unexpected error: {err}"
        );
        assert!(crate::loader::decode::detect::primary_decode_failure_is_final(&err));
    }
}
