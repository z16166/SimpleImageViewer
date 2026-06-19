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

//! Lightweight directory-tree list thumbnails opened by path (independent of main preload).

use std::path::{Path, PathBuf};

use crate::hdr::types::HdrToneMapSettings;
use crate::loader::{
    DecodedImage, ImageData, TiledImageSource, apply_exif_orientation_to_image_data,
    decoded_looks_like_black_placeholder, extract_exif_thumbnail, hdr_to_sdr_with_user_tone,
    preview_aspect_matches_logical,
};

use super::assemble::make_image_data;
use super::detect::{
    load_primary_with_detection_fallback, load_via_content_detection,
    primary_decode_failure_is_final,
};
use super::hdr_formats::load_hdr;
use super::jpeg::load_jpeg_with_target_capacity;
use super::modern::{
    load_avif_with_target_capacity, load_heif_hdr_aware, load_jxl_with_target_capacity,
};
use super::open_raw_processor_with_preview;
use super::raster::{load_gif, load_png, load_psd, load_static, load_webp};
use super::{is_maybe_animated, tiff_may_be_camera_raw};

/// Directory-tree list previews are always SDR thumbnails, independent of main-window HDR output.
const DIRECTORY_TREE_THUMB_HDR_CAPACITY: f32 = 1.0;

pub(crate) fn generate_directory_tree_thumb_from_path(
    path: &Path,
    max_side: u32,
) -> Result<(DecodedImage, (u32, u32)), String> {
    let first_exif = extract_exif_thumbnail(path);
    if let Some(exif) = first_exif.as_ref() {
        let logical = probe_still_image_logical_size(path).unwrap_or((exif.width, exif.height));
        if preview_aspect_matches_logical(exif.width, exif.height, logical.0, logical.1) {
            let decoded = downsample_decoded_to_max_side(exif, max_side)?;
            return Ok((decoded, logical));
        }
    }

    let path_buf = path.to_path_buf();
    let image_data = open_image_data_for_directory_tree_thumb(&path_buf)?;
    let logical = logical_size_from_image_data(&image_data);

    if let Some(exif) = first_exif.as_ref() {
        if preview_aspect_matches_logical(exif.width, exif.height, logical.0, logical.1) {
            let decoded = downsample_decoded_to_max_side(exif, max_side)?;
            return Ok((decoded, logical));
        }
    }

    let decoded = preview_from_image_data(&image_data, max_side)?;
    if !preview_aspect_matches_logical(decoded.width, decoded.height, logical.0, logical.1) {
        return Err(format!(
            "directory tree thumb aspect mismatch: {}x{} vs {}x{}",
            decoded.width, decoded.height, logical.0, logical.1
        ));
    }
    Ok((decoded, logical))
}

fn probe_still_image_logical_size(path: &Path) -> Option<(u32, u32)> {
    image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

fn open_image_data_for_directory_tree_thumb(path: &PathBuf) -> Result<ImageData, String> {
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default();
    let hdr_target_capacity = DIRECTORY_TREE_THUMB_HDR_CAPACITY;
    let hdr_tone_map = HdrToneMapSettings::default();
    let high_quality = false;

    if ext == "exr" {
        return load_primary_with_detection_fallback(
            path,
            file_name,
            hdr_target_capacity,
            hdr_tone_map,
            high_quality,
            || load_hdr(path, hdr_target_capacity, hdr_tone_map),
        );
    }

    if crate::hdr::decode::is_hdr_candidate_ext(&ext) {
        if let Ok(img) = load_hdr(path, hdr_target_capacity, hdr_tone_map) {
            return Ok(img);
        }
    }

    if ext == "psd" || ext == "psb" {
        return load_psd(path);
    }

    if crate::raw_processor::is_raw_extension(&ext) {
        return open_raw_image_data_for_directory_tree_thumb(path);
    }

    if ext == "jpg" || ext == "jpeg" {
        return load_primary_with_detection_fallback(
            path,
            file_name,
            hdr_target_capacity,
            hdr_tone_map,
            high_quality,
            || load_jpeg_with_target_capacity(path, hdr_target_capacity, hdr_tone_map),
        );
    }

    if ext == "tif" || ext == "tiff" {
        if tiff_may_be_camera_raw(path) && crate::raw_processor::probe_libraw_can_open(path) {
            return open_raw_image_data_for_directory_tree_thumb(path);
        }
        return load_primary_with_detection_fallback(
            path,
            file_name,
            hdr_target_capacity,
            hdr_tone_map,
            high_quality,
            || crate::libtiff_loader::load_via_libtiff(path, hdr_target_capacity, hdr_tone_map),
        );
    }

    if ext == "avif" || ext == "avifs" {
        return load_primary_with_detection_fallback(
            path,
            file_name,
            hdr_target_capacity,
            hdr_tone_map,
            high_quality,
            || load_avif_with_target_capacity(path, hdr_target_capacity, hdr_tone_map),
        );
    }

    if ext == "jxl" {
        return load_primary_with_detection_fallback(
            path,
            file_name,
            hdr_target_capacity,
            hdr_tone_map,
            high_quality,
            || load_jxl_with_target_capacity(path, hdr_target_capacity, hdr_tone_map),
        );
    }

    if ext == "heif" || ext == "heic" || ext == "hif" {
        return load_primary_with_detection_fallback(
            path,
            file_name,
            hdr_target_capacity,
            hdr_tone_map,
            high_quality,
            || load_heif_hdr_aware(path, hdr_target_capacity, hdr_tone_map),
        );
    }

    if let Ok(reg) = crate::formats::get_registry().read()
        && reg.extensions.contains(&ext)
        && !is_maybe_animated(&ext)
    {
        #[cfg(target_os = "windows")]
        if let Ok(img) = crate::wic::load_via_wic(path, high_quality, None) {
            return Ok(apply_exif_orientation_to_image_data(path.as_path(), img));
        }
        #[cfg(target_os = "macos")]
        if let Ok(img) = crate::macos_image_io::load_via_image_io(path, high_quality, None) {
            return Ok(apply_exif_orientation_to_image_data(path.as_path(), img));
        }
    }

    match ext.as_str() {
        "png" => load_png(path, hdr_target_capacity, hdr_tone_map),
        "webp" => load_webp(path, hdr_target_capacity, hdr_tone_map),
        "gif" => load_gif(path, hdr_target_capacity, hdr_tone_map),
        _ => load_static(path, hdr_target_capacity, hdr_tone_map),
    }
    .or_else(|primary_err: String| {
        if primary_decode_failure_is_final(&primary_err) {
            return Err(primary_err);
        }
        load_via_content_detection(path, hdr_target_capacity, hdr_tone_map)
    })
}

fn open_raw_image_data_for_directory_tree_thumb(path: &PathBuf) -> Result<ImageData, String> {
    match open_raw_processor_with_preview(path) {
        Ok((processor, preview_opt, _, _)) => {
            if let Some(preview) = preview_opt {
                return Ok(make_image_data(preview));
            }
            let (width, height) = processor.developed_output_dimensions(None);
            if width > 0 && height > 0 {
                log::debug!(
                    "[DirectoryTree] RAW {:?} has no embedded preview ({}x{}); trying platform fallback",
                    path.file_name().unwrap_or_default(),
                    width,
                    height
                );
            }
        }
        Err(err) => {
            log::debug!(
                "[DirectoryTree] LibRaw open failed for {:?}: {err}; trying platform fallback",
                path.file_name().unwrap_or_default()
            );
        }
    }

    platform_still_image_fallback(path)
}

fn platform_still_image_fallback(path: &PathBuf) -> Result<ImageData, String> {
    #[cfg(target_os = "windows")]
    {
        return crate::wic::load_via_wic(path, false, None)
            .map(|img| apply_exif_orientation_to_image_data(path.as_path(), img));
    }
    #[cfg(target_os = "macos")]
    {
        return crate::macos_image_io::load_via_image_io(path, false, None)
            .map(|img| apply_exif_orientation_to_image_data(path.as_path(), img));
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = path;
        Err("no platform still-image fallback for RAW thumbnail".to_string())
    }
}

fn logical_size_from_image_data(image_data: &ImageData) -> (u32, u32) {
    match image_data {
        ImageData::Static(image) => (image.width, image.height),
        ImageData::Hdr { hdr, .. } => (hdr.width, hdr.height),
        ImageData::HdrTiled { hdr, .. } => (hdr.width(), hdr.height()),
        ImageData::Tiled(source) => (source.width(), source.height()),
        ImageData::Animated(frames) => frames
            .first()
            .map(|frame| (frame.width, frame.height))
            .unwrap_or((0, 0)),
        ImageData::HdrAnimated(frames) => frames
            .first()
            .map(|frame| (frame.fallback.width, frame.fallback.height))
            .unwrap_or((0, 0)),
    }
}

fn preview_from_image_data(image_data: &ImageData, max_side: u32) -> Result<DecodedImage, String> {
    let tone = HdrToneMapSettings::default();
    match image_data {
        ImageData::Static(image) => downsample_decoded_to_max_side(image, max_side),
        ImageData::Hdr { hdr, fallback, .. } => {
            let sdr = sdr_preview_for_hdr_fallback(hdr, fallback, &tone)?;
            downsample_decoded_to_max_side(&sdr, max_side)
        }
        ImageData::Animated(frames) => frames
            .first()
            .map(|frame| {
                downsample_decoded_to_max_side(
                    &DecodedImage::from_arc(frame.width, frame.height, frame.arc_pixels()),
                    max_side,
                )
            })
            .transpose()?
            .ok_or_else(|| "animated image has no frames".to_string()),
        ImageData::Tiled(source) => tiled_source_preview(source.as_ref(), max_side),
        ImageData::HdrTiled { hdr, fallback } => {
            let preview = tiled_source_preview(fallback.as_ref(), max_side)?;
            if decoded_looks_like_black_placeholder(&preview) {
                let (width, height, rgba) = hdr.generate_sdr_preview(max_side, max_side)?;
                downsample_decoded_to_max_side(&DecodedImage::new(width, height, rgba), max_side)
            } else {
                Ok(preview)
            }
        }
        ImageData::HdrAnimated(frames) => frames
            .first()
            .map(|frame| {
                let sdr = sdr_preview_for_hdr_fallback(&frame.hdr, &frame.fallback, &tone)?;
                downsample_decoded_to_max_side(&sdr, max_side)
            })
            .transpose()?
            .ok_or_else(|| "animated HDR image has no frames".to_string()),
    }
}

fn sdr_preview_for_hdr_fallback(
    hdr: &crate::hdr::types::HdrImageBuffer,
    fallback: &DecodedImage,
    tone: &HdrToneMapSettings,
) -> Result<DecodedImage, String> {
    if decoded_looks_like_black_placeholder(fallback) {
        Ok(DecodedImage::new(
            hdr.width,
            hdr.height,
            hdr_to_sdr_with_user_tone(hdr, tone)?,
        ))
    } else {
        Ok(fallback.clone())
    }
}

fn tiled_source_preview(
    source: &dyn TiledImageSource,
    max_side: u32,
) -> Result<DecodedImage, String> {
    let gen_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        source.generate_full_image_preview(max_side, max_side)
    }));
    match gen_result {
        Ok((width, height, pixels)) if width > 0 && height > 0 => {
            Ok(DecodedImage::new(width, height, pixels))
        }
        Ok(_) => Err("generate_full_image_preview returned empty preview".to_string()),
        Err(_) => Err("generate_full_image_preview panicked".to_string()),
    }
}

fn downsample_decoded_to_max_side(
    decoded: &DecodedImage,
    max_side: u32,
) -> Result<DecodedImage, String> {
    let max_dim = decoded.width.max(decoded.height);
    if max_dim <= max_side {
        return Ok(decoded.clone());
    }
    let src = decoded.clone().into_rgba8_image()?;
    let scale = max_side as f32 / max_dim as f32;
    let out_w = ((decoded.width as f32 * scale).round() as u32).max(1);
    let out_h = ((decoded.height as f32 * scale).round() as u32).max(1);
    let resized =
        image::imageops::resize(&src, out_w, out_h, image::imageops::FilterType::Triangle);
    Ok(DecodedImage::from(resized))
}
