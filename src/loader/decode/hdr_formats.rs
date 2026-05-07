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

//! Radiance `.hdr`, OpenEXR routing, disk-backed probing.

use crate::hdr::tiled::HdrTiledSource;
use crate::hdr::types::HdrToneMapSettings;
use crate::loader::{
    apply_exif_orientation_to_hdr_pair, hdr_sdr_fallback_rgba8_eager_or_placeholder,
    DecodedImage, ImageData, TiledImageSource,
};
use std::path::Path;
use std::sync::Arc;

use super::assemble::make_hdr_image_data;
use crate::loader::tiled_sources::HdrSdrTiledFallbackSource;

pub(crate) fn load_hdr(
    path: &Path,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
) -> Result<ImageData, String> {
    if is_exr_path(path) {
        return load_detected_exr(path, hdr_target_capacity, hdr_tone_map);
    } else if let Some(image_data) = try_load_disk_backed_radiance_hdr(path, hdr_tone_map)? {
        return Ok(image_data);
    }

    let hdr = match crate::hdr::decode::decode_hdr_image(path) {
        Ok(hdr) => hdr,
        Err(err) if is_exr_deep_data_unsupported_error(&err) => {
            log::warn!(
                "[Loader] Deep EXR data needs custom compositing for {}; using deep decoder",
                path.display()
            );
            return load_deep_exr(path, hdr_target_capacity, hdr_tone_map);
        }
        Err(err) => return Err(err),
    };
    let pixels = hdr_sdr_fallback_rgba8_eager_or_placeholder(
        &hdr,
        hdr_target_capacity,
        &hdr_tone_map,
    )?;
    let fallback = DecodedImage::new(hdr.width, hdr.height, pixels);
    let (hdr, fallback) = apply_exif_orientation_to_hdr_pair(path, hdr, fallback);
    Ok(make_hdr_image_data(hdr, fallback))
}

pub(crate) fn is_exr_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("exr"))
}

pub(crate) fn try_load_disk_backed_exr_hdr(
    path: &Path,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
) -> Result<Option<ImageData>, String> {
    let source = match crate::hdr::exr_tiled::ExrTiledImageSource::open(path) {
        Ok(source) => source,
        Err(err) if is_exr_disk_backed_probe_fallback_error(&err) => {
            log::warn!(
                "[Loader] Disk-backed EXR tiled source unavailable for {}: {err}; falling back to full HDR decode",
                path.display()
            );
            return Ok(None);
        }
        Err(err) => return Err(err),
    };
    let pixel_count = source.width() as u64 * source.height() as u64;
    let tiled_limit = crate::tile_cache::TILED_THRESHOLD.load(std::sync::atomic::Ordering::Relaxed);
    let max_side = source.width().max(source.height());
    if pixel_count < tiled_limit && max_side <= crate::constants::ABSOLUTE_MAX_TEXTURE_SIDE {
        if source.has_subsampled_channels() {
            let hdr: Arc<dyn crate::hdr::tiled::HdrTiledSource> = Arc::new(source);
            let fallback: Arc<dyn TiledImageSource> =
                Arc::new(HdrSdrTiledFallbackSource::new(Arc::clone(&hdr), hdr_tone_map));
            log::info!(
                "[Loader] subsampled EXR {}x{} kept as disk-backed HDR tiles.",
                hdr.width(),
                hdr.height()
            );
            return Ok(Some(ImageData::HdrTiled { hdr, fallback }));
        }
        if source.requires_disk_backed_decode() {
            return exr_tiled_source_to_static_hdr(path, source, hdr_target_capacity, hdr_tone_map).map(Some);
        }
        return Ok(None);
    }

    let hdr: Arc<dyn crate::hdr::tiled::HdrTiledSource> = Arc::new(source);
    let fallback: Arc<dyn TiledImageSource> =
        Arc::new(HdrSdrTiledFallbackSource::new(Arc::clone(&hdr), hdr_tone_map));
    log::info!(
        "[Loader] EXR {}x{} routed to disk-backed HDR tiles.",
        hdr.width(),
        hdr.height()
    );
    Ok(Some(ImageData::HdrTiled { hdr, fallback }))
}

pub(crate) fn exr_tiled_source_to_static_hdr(
    path: &Path,
    source: crate::hdr::exr_tiled::ExrTiledImageSource,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
) -> Result<ImageData, String> {
    let tile = source.extract_tile_rgba32f_arc(0, 0, source.width(), source.height())?;
    let hdr = crate::hdr::types::HdrImageBuffer {
        width: tile.width,
        height: tile.height,
        format: crate::hdr::types::HdrPixelFormat::Rgba32Float,
        color_space: tile.color_space,
        metadata: tile.metadata.clone(),
        rgba_f32: Arc::clone(&tile.rgba_f32),
    };
    let pixels = hdr_sdr_fallback_rgba8_eager_or_placeholder(
        &hdr,
        hdr_target_capacity,
        &hdr_tone_map,
    )?;
    let fallback = DecodedImage::new(hdr.width, hdr.height, pixels);
    log::info!(
        "[Loader] EXR {}x{} routed to static HDR via disk-backed decoder: {}",
        hdr.width,
        hdr.height,
        path.display()
    );
    let (hdr, fallback) = apply_exif_orientation_to_hdr_pair(path, hdr, fallback);
    Ok(make_hdr_image_data(hdr, fallback))
}

pub(crate) fn try_load_disk_backed_radiance_hdr(path: &Path, hdr_tone_map: HdrToneMapSettings) -> Result<Option<ImageData>, String> {
    let source = crate::hdr::radiance_tiled::RadianceHdrTiledImageSource::open(path)?;
    let pixel_count = source.width() as u64 * source.height() as u64;
    let tiled_limit = crate::tile_cache::TILED_THRESHOLD.load(std::sync::atomic::Ordering::Relaxed);
    let max_side = source.width().max(source.height());
    if pixel_count < tiled_limit && max_side <= crate::constants::ABSOLUTE_MAX_TEXTURE_SIDE {
        return Ok(None);
    }

    let hdr: Arc<dyn crate::hdr::tiled::HdrTiledSource> = Arc::new(source);
    let fallback: Arc<dyn TiledImageSource> =
        Arc::new(HdrSdrTiledFallbackSource::new(Arc::clone(&hdr), hdr_tone_map));
    log::info!(
        "[Loader] Radiance HDR {}x{} routed to disk-backed HDR tiles.",
        hdr.width(),
        hdr.height()
    );
    Ok(Some(ImageData::HdrTiled { hdr, fallback }))
}

pub(crate) fn is_exr_disk_backed_probe_fallback_error(err: &str) -> bool {
    err.contains("channel subsampling not supported yet")
        || err.contains("EXR layer does not contain required")
        || err.contains("deep data not supported yet")
}

pub(crate) fn is_exr_deep_data_unsupported_error(err: &str) -> bool {
    err.contains("deep data not supported yet")
}

pub(crate) fn load_deep_exr(
    path: &Path,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
) -> Result<ImageData, String> {
    match crate::hdr::exr_tiled::decode_deep_exr_image(path) {
        Ok(hdr) => {
            let pixels = hdr_sdr_fallback_rgba8_eager_or_placeholder(
                &hdr,
                hdr_target_capacity,
                &hdr_tone_map,
            )?;
            let fallback = DecodedImage::new(hdr.width, hdr.height, pixels);
            let (hdr, fallback) = apply_exif_orientation_to_hdr_pair(path, hdr, fallback);
            Ok(make_hdr_image_data(hdr, fallback))
        }
        Err(err) => {
            log::warn!(
                "[Loader] Deep EXR compositing failed for {}: {err}; using visible placeholder",
                path.display()
            );
            make_deep_exr_placeholder(path)
        }
    }
}

pub(crate) fn make_deep_exr_placeholder(path: &Path) -> Result<ImageData, String> {
    let (width, height) = crate::hdr::exr_tiled::exr_dimensions_unvalidated(path)?;
    let pixel_count = width
        .checked_mul(height)
        .ok_or_else(|| format!("Deep EXR placeholder dimensions overflow: {width}x{height}"))?;
    let mut rgba_f32 = vec![0.0_f32; pixel_count as usize * 4];
    for alpha in rgba_f32.chunks_exact_mut(4).map(|pixel| &mut pixel[3]) {
        *alpha = 1.0;
    }
    let mut fallback_pixels = vec![0_u8; pixel_count as usize * 4];
    for alpha in fallback_pixels
        .chunks_exact_mut(4)
        .map(|pixel| &mut pixel[3])
    {
        *alpha = 255;
    }
    let hdr = crate::hdr::types::HdrImageBuffer {
        width,
        height,
        format: crate::hdr::types::HdrPixelFormat::Rgba32Float,
        color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
        metadata: crate::hdr::types::HdrImageMetadata::from_color_space(
            crate::hdr::types::HdrColorSpace::LinearSrgb,
        ),
        rgba_f32: Arc::new(rgba_f32),
    };
    let fallback = DecodedImage::new(width, height, fallback_pixels);
    Ok(make_hdr_image_data(hdr, fallback))
}

pub(crate) fn load_detected_exr(
    path: &Path,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
) -> Result<ImageData, String> {
    if let Some(image_data) =
        try_load_disk_backed_exr_hdr(path, hdr_target_capacity, hdr_tone_map)?
    {
        return Ok(image_data);
    }

    let hdr = match crate::hdr::decode::decode_exr_display_image(path) {
        Ok(hdr) => hdr,
        Err(err) if is_exr_deep_data_unsupported_error(&err) => {
            log::warn!(
                "[Loader] Deep EXR data needs custom compositing for {}; using deep decoder",
                path.display()
            );
            return load_deep_exr(path, hdr_target_capacity, hdr_tone_map);
        }
        Err(err) => return Err(err),
    };
    let pixels = hdr_sdr_fallback_rgba8_eager_or_placeholder(
        &hdr,
        hdr_target_capacity,
        &hdr_tone_map,
    )?;
    let fallback = DecodedImage::new(hdr.width, hdr.height, pixels);
    let (hdr, fallback) = apply_exif_orientation_to_hdr_pair(path, hdr, fallback);
    Ok(make_hdr_image_data(hdr, fallback))
}
