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
//!
//! **Platform strip decode:** Windows and macOS use WIC / ImageIO fast paths for many
//! registered extensions and RAW fallbacks. Linux uses the same LibRaw embedded preview path,
//! then half-size LibRaw develop when no embedded thumbnail exists.

use std::path::{Path, PathBuf};

#[cfg(any(target_os = "windows", target_os = "macos"))]
use crate::loader::apply_exif_orientation_to_image_data;
use crate::loader::downsample_decoded_for_strip;
use crate::loader::{
    DecodedImage, ImageData, TiledImageSource, extract_exif_thumbnail,
    extract_exif_thumbnail_from_mmap, preview_aspect_matches_logical,
};

use super::assemble::make_image_data;
use super::detect::{
    load_primary_with_detection_fallback, load_via_content_detection,
    primary_decode_failure_is_final,
};
use super::hdr_formats::load_hdr;
use super::is_maybe_animated;
use super::jpeg::load_jpeg_with_target_capacity;
use super::modern::{
    load_avif_with_target_capacity, load_heif_hdr_aware, load_jxl_with_target_capacity,
};
use super::open_raw_processor_with_preview;
use super::raster::{load_gif, load_png, load_psd, load_static, load_webp};

/// Directory-tree list previews are always SDR thumbnails, independent of main-window HDR output.
const DIRECTORY_TREE_THUMB_HDR_CAPACITY: f32 = 1.0;

fn try_directory_tree_exif_thumb(
    exif: &DecodedImage,
    logical: (u32, u32),
    max_side: u32,
) -> Option<(DecodedImage, (u32, u32))> {
    if !preview_aspect_matches_logical(exif.width, exif.height, logical.0, logical.1) {
        return None;
    }
    let decoded = downsample_decoded_to_max_side(exif.clone(), max_side).ok()?;
    Some((decoded, logical))
}

pub(crate) fn generate_directory_tree_thumb_from_path(
    path: &Path,
    max_side: u32,
) -> Result<(DecodedImage, (u32, u32)), String> {
    let skip_exif_fast_path = super::modern::is_hdr_capable_modern_format_path(path);
    let mmap = crate::mmap_util::map_file(path).ok();
    let exif = if skip_exif_fast_path {
        None
    } else {
        match mmap.as_ref() {
            Some(data) => extract_exif_thumbnail_from_mmap(data, path),
            None => extract_exif_thumbnail(path),
        }
    };
    if let Some(exif) = exif.as_ref() {
        let exif_logical = (exif.width, exif.height);
        let logical = normalize_logical_size(
            mmap.as_ref()
                .and_then(probe_still_image_logical_size_from_mmap)
                .unwrap_or(exif_logical),
            exif_logical,
        );
        if let Some(result) = try_directory_tree_exif_thumb(exif, logical, max_side) {
            return Ok(result);
        }
    }

    let path_buf = path.to_path_buf();
    if let Some(fast) = super::gain_map_strip::try_fast_iso_gain_map_strip_from_path(
        path,
        mmap.as_ref().map(|data| data.as_ref()),
        max_side,
    ) {
        return fast;
    }
    // DCT-scaled baseline-JPEG fast path: when no EXIF thumbnail exists and the file
    // is not Ultra HDR / JPEG_R, decode directly at the scaled output size.  For a
    // 4000×3000 → 256px strip this is ~10× faster and 64× less peak memory than a
    // full-resolution decode followed by a software downsample.
    if let Some(result) = try_jpeg_dct_strip_fast_path(path, mmap.as_ref(), max_side) {
        return result;
    }
    let image_data = open_image_data_for_directory_tree_thumb(&path_buf, mmap.as_ref())?;
    let logical = logical_size_from_image_data(&image_data);

    if let Some(exif) = exif.as_ref()
        && let Some(result) = try_directory_tree_exif_thumb(exif, logical, max_side)
    {
        return Ok(result);
    }

    let decoded = preview_from_image_data(&image_data, max_side)?;
    if !preview_aspect_matches_logical(decoded.width, decoded.height, logical.0, logical.1) {
        return Err(format!(
            "directory tree thumb aspect mismatch: {}x{} vs {}x{}",
            decoded.width, decoded.height, logical.0, logical.1
        ));
    }
    if decoded.is_sdr_deferred_placeholder() {
        return Err(format!(
            "directory tree thumb decode is deferred SDR placeholder for {}",
            path.display()
        ));
    }
    Ok((decoded, logical))
}

fn normalize_logical_size(logical: (u32, u32), fallback: (u32, u32)) -> (u32, u32) {
    if logical.0 == 0 || logical.1 == 0 {
        fallback
    } else {
        logical
    }
}

fn probe_still_image_logical_size_from_mmap(mmap: &memmap2::Mmap) -> Option<(u32, u32)> {
    use std::io::Cursor;
    image::ImageReader::new(Cursor::new(mmap.as_ref()))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

fn path_has_extension(path: &Path, ext: &str) -> bool {
    path.extension()
        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(ext))
}

fn path_extension_ascii_lower(path: &Path) -> Option<String> {
    path.extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
}

fn open_image_data_for_directory_tree_thumb(
    path: &PathBuf,
    file_mmap: Option<&memmap2::Mmap>,
) -> Result<ImageData, String> {
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown".to_string());
    let ext = path_extension_ascii_lower(path.as_path()).unwrap_or_default();
    let hdr_target_capacity = DIRECTORY_TREE_THUMB_HDR_CAPACITY;
    let hdr_tone_map = crate::loader::hdr_tone_map_settings_for_directory_tree_strip();
    let high_quality = false;

    if path_has_extension(path, "exr") {
        return load_primary_with_detection_fallback(
            path,
            file_name.as_str(),
            hdr_target_capacity,
            hdr_tone_map,
            high_quality,
            || load_hdr(path, hdr_target_capacity, hdr_tone_map),
        );
    }

    if crate::hdr::decode::is_hdr_candidate_ext(&ext)
        && let Ok(img) = load_hdr(path, hdr_target_capacity, hdr_tone_map)
    {
        return Ok(img);
    }

    if path_has_extension(path, "psd") || path_has_extension(path, "psb") {
        return load_psd(path);
    }

    if crate::raw_processor::is_raw_extension(&ext) {
        return open_raw_image_data_for_directory_tree_thumb(path);
    }

    if path_has_extension(path, "jpg") || path_has_extension(path, "jpeg") {
        return load_primary_with_detection_fallback(
            path,
            file_name.as_str(),
            hdr_target_capacity,
            hdr_tone_map,
            high_quality,
            || {
                if let Some(mmap) = file_mmap {
                    super::jpeg::load_jpeg_from_mapped(
                        path,
                        mmap,
                        hdr_target_capacity,
                        hdr_tone_map,
                    )
                } else {
                    load_jpeg_with_target_capacity(path, hdr_target_capacity, hdr_tone_map)
                }
            },
        );
    }

    if path_has_extension(path, "tif") || path_has_extension(path, "tiff") {
        let tiff_is_raw = file_mmap
            .map(|data| super::tiff_raw_sniff::tiff_may_be_camera_raw_bytes(data))
            .unwrap_or_else(|| crate::loader::tiff_may_be_camera_raw(path));
        if tiff_is_raw && crate::raw_processor::probe_libraw_can_open(path) {
            return open_raw_image_data_for_directory_tree_thumb(path);
        }
        return load_primary_with_detection_fallback(
            path,
            file_name.as_str(),
            hdr_target_capacity,
            hdr_tone_map,
            high_quality,
            || crate::libtiff_loader::load_via_libtiff(path, hdr_target_capacity, hdr_tone_map),
        );
    }

    if path_has_extension(path, "avif") || path_has_extension(path, "avifs") {
        return load_primary_with_detection_fallback(
            path,
            file_name.as_str(),
            hdr_target_capacity,
            hdr_tone_map,
            high_quality,
            || load_avif_with_target_capacity(path, hdr_target_capacity, hdr_tone_map),
        );
    }

    if path_has_extension(path, "jxl") {
        return load_primary_with_detection_fallback(
            path,
            file_name.as_str(),
            hdr_target_capacity,
            hdr_tone_map,
            high_quality,
            || load_jxl_with_target_capacity(path, hdr_target_capacity, hdr_tone_map),
        );
    }

    if path_has_extension(path, "heif")
        || path_has_extension(path, "heic")
        || path_has_extension(path, "hif")
    {
        return load_primary_with_detection_fallback(
            path,
            file_name.as_str(),
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
            let (width, height) = processor.developed_output_dimensions();
            if width > 0 && height > 0 {
                log::debug!(
                    "[DirectoryTree] RAW {:?} has no embedded preview ({}x{}); trying platform fallback",
                    path.file_name().unwrap_or_default(),
                    width,
                    height
                );
            }
            platform_still_image_fallback(path, Some(processor))
        }
        Err(err) => {
            log::debug!(
                "[DirectoryTree] LibRaw open failed for {:?}: {err}; trying platform fallback",
                path.file_name().unwrap_or_default()
            );
            platform_still_image_fallback(path, None)
        }
    }
}

fn raw_strip_libraw_fallback(
    path: &PathBuf,
    opened_processor: Option<crate::raw_processor::RawProcessor>,
) -> Result<ImageData, String> {
    use super::raw::develop_half_size_sdr_strip_preview;

    let mut processor = match opened_processor {
        Some(processor) => processor,
        None => {
            let mut processor = crate::raw_processor::RawProcessor::new()
                .ok_or_else(|| rust_i18n::t!("error.libraw_init").to_string())?;
            processor.open(path).map_err(|err| {
                format!("LibRaw failed and no platform fallback available: {err}")
            })?;
            processor
        }
    };
    develop_half_size_sdr_strip_preview(&mut processor, path.as_path())
        .map(make_image_data)
        .ok_or_else(|| "no LibRaw strip preview available for RAW".to_string())
}

fn platform_still_image_fallback(
    path: &PathBuf,
    opened_processor: Option<crate::raw_processor::RawProcessor>,
) -> Result<ImageData, String> {
    #[cfg(target_os = "windows")]
    {
        match crate::wic::load_via_wic(path, false, None) {
            Ok(img) => {
                return Ok(apply_exif_orientation_to_image_data(path.as_path(), img));
            }
            Err(wic_err) => {
                log::debug!(
                    "[DirectoryTree] WIC strip fallback failed for {:?}: {wic_err}; trying LibRaw half-size develop",
                    path.file_name().unwrap_or_default()
                );
            }
        }
        raw_strip_libraw_fallback(path, opened_processor)
    }
    #[cfg(target_os = "macos")]
    {
        match crate::macos_image_io::load_via_image_io(path, false, None) {
            Ok(img) => {
                return Ok(apply_exif_orientation_to_image_data(path.as_path(), img));
            }
            Err(io_err) => {
                log::debug!(
                    "[DirectoryTree] ImageIO strip fallback failed for {:?}: {io_err}; trying LibRaw half-size develop",
                    path.file_name().unwrap_or_default()
                );
            }
        }
        return raw_strip_libraw_fallback(path, opened_processor);
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        raw_strip_libraw_fallback(path, opened_processor)
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
    match image_data {
        ImageData::Static(image) => downsample_decoded_to_max_side(image.clone(), max_side),
        ImageData::Hdr { hdr, fallback, .. } => {
            sdr_preview_for_hdr_fallback(hdr, fallback, max_side)
        }
        ImageData::Animated(frames) => frames
            .first()
            .map(|frame| {
                downsample_decoded_to_max_side(
                    DecodedImage::from_arc(frame.width, frame.height, frame.arc_pixels()),
                    max_side,
                )
            })
            .transpose()?
            .ok_or_else(|| "animated image has no frames".to_string()),
        ImageData::Tiled(source) => tiled_source_preview(source.as_ref(), max_side),
        ImageData::HdrTiled { hdr, fallback } => {
            let preview = tiled_source_preview(fallback.as_ref(), max_side)?;
            if preview.is_sdr_deferred_placeholder() {
                let (width, height, rgba) = hdr.generate_sdr_preview(max_side, max_side)?;
                downsample_decoded_to_max_side(DecodedImage::new(width, height, rgba), max_side)
            } else {
                Ok(preview)
            }
        }
        ImageData::HdrAnimated(frames) => frames
            .first()
            .map(|frame| sdr_preview_for_hdr_fallback(&frame.hdr, &frame.fallback, max_side))
            .transpose()?
            .ok_or_else(|| "animated HDR image has no frames".to_string()),
    }
}

fn sdr_preview_for_hdr_fallback(
    hdr: &crate::hdr::types::HdrImageBuffer,
    fallback: &DecodedImage,
    max_side: u32,
) -> Result<DecodedImage, String> {
    crate::loader::directory_tree_strip_from_hdr_or_fallback(hdr, fallback, max_side)
}

fn tiled_source_preview(
    source: &dyn TiledImageSource,
    max_side: u32,
) -> Result<DecodedImage, String> {
    // SAFETY: panic in generate_full_image_preview is caught below; the caller thread
    // stays healthy without spawning a nested OS thread.
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
    decoded: DecodedImage,
    max_side: u32,
) -> Result<DecodedImage, String> {
    downsample_decoded_for_strip(&decoded, max_side)
}

/// Try to produce a strip thumbnail from a baseline JPEG using DCT-domain scaling.
///
/// Returns `None` when the file is not a `.jpg`/`.jpeg`, no mmap data is available,
/// or the JPEG is Ultra HDR / JPEG_R.  On success the aspect ratio is guaranteed to
/// match the logical size because DCT scaling applies the same ratio to both axes.
fn try_jpeg_dct_strip_fast_path(
    path: &Path,
    mmap: Option<&memmap2::Mmap>,
    max_side: u32,
) -> Option<Result<(DecodedImage, (u32, u32)), String>> {
    if !path_has_extension(path, "jpg") && !path_has_extension(path, "jpeg") {
        return None;
    }
    let data = mmap?;
    super::jpeg::try_decode_jpeg_strip_dct(data, max_side)
}

#[cfg(test)]
mod tests {
    use super::normalize_logical_size;

    #[test]
    fn zero_logical_falls_back_to_exif() {
        assert_eq!(normalize_logical_size((0, 0), (1920, 1080)), (1920, 1080));
        assert_eq!(
            normalize_logical_size((0, 1080), (1920, 1080)),
            (1920, 1080)
        );
        assert_eq!(
            normalize_logical_size((1920, 0), (1920, 1080)),
            (1920, 1080)
        );
    }

    #[test]
    fn non_zero_logical_is_preserved() {
        assert_eq!(
            normalize_logical_size((4000, 3000), (1920, 1080)),
            (4000, 3000)
        );
    }
}
