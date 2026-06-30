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

use std::path::Path;

#[cfg(feature = "heif-native")]
use crate::hdr::heif::{
    HeifThumbProbe, HeifThumbProbeDetail, probe_heif_strip_thumbnail,
    probe_heif_strip_thumbnail_from_path,
};
#[cfg(any(target_os = "windows", target_os = "macos"))]
use crate::loader::apply_exif_orientation_to_image_data;
use crate::loader::downsample_decoded_for_strip;
use crate::loader::metadata::{ExifThumbProbe, ExifThumbProbeDetail};
use crate::loader::{
    DecodedImage, ImageData, TiledImageSource, extract_exif_thumbnail_from_mmap_probed,
    extract_exif_thumbnail_probed, preview_aspect_matches_logical,
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

type StripWithLogicalSize = (DecodedImage, (u32, u32));
type OptionalStripResult = Option<Result<StripWithLogicalSize, String>>;

pub(crate) struct DirectoryTreeThumbDecode {
    pub(crate) preview: DecodedImage,
    pub(crate) logical_size: (u32, u32),
    pub(crate) reusable_full: Option<DecodedImage>,
    /// Embedded EXIF or container (e.g. libheif) SDR preview: low-rank strip placeholder on
    /// gain-map-capable modern formats until a refined strip arrives.
    pub(crate) from_embedded_sdr_preview: bool,
}

impl DirectoryTreeThumbDecode {
    fn new(
        preview: DecodedImage,
        logical_size: (u32, u32),
        reusable_full: Option<DecodedImage>,
        from_embedded_sdr_preview: bool,
    ) -> Self {
        Self {
            preview,
            logical_size,
            reusable_full,
            from_embedded_sdr_preview,
        }
    }
}

/// `path_may_have_gain_map_embedded_sdr_preview` is extension-heuristic (all AVIF/HEIF/JXL);
/// only mark fast-path strips as placeholders when that heuristic matches so plain raster
/// formats keep `StripDecodedPixels` and avoid a spurious full-decode upgrade pass.
fn embedded_sdr_strip_may_be_placeholder(gain_map_container: bool) -> bool {
    gain_map_container
}

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

#[cfg(feature = "preload-debug")]
fn log_strip_exif_probe(
    path: &Path,
    probe: ExifThumbProbe,
    detail: ExifThumbProbeDetail,
    logical: Option<(u32, u32)>,
    max_side: u32,
    extra: &str,
) {
    crate::preload_debug!(
        "[PreloadDebug][Strip] exif_probe path={} outcome={} offset={:?} len={:?} thumb={:?} logical={:?} max_side={} {}",
        path.display(),
        probe.label(),
        detail.offset,
        detail.len,
        detail
            .thumb_w
            .zip(detail.thumb_h)
            .map(|(w, h)| format!("{w}x{h}"))
            .unwrap_or_else(|| "-".to_string()),
        logical,
        max_side,
        extra
    );
}

#[cfg(not(feature = "preload-debug"))]
fn log_strip_exif_probe(
    _path: &Path,
    _probe: ExifThumbProbe,
    _detail: ExifThumbProbeDetail,
    _logical: Option<(u32, u32)>,
    _max_side: u32,
    _extra: &str,
) {
}

#[cfg(feature = "preload-debug")]
fn log_strip_heif_probe(
    path: &Path,
    probe: HeifThumbProbe,
    detail: HeifThumbProbeDetail,
    max_side: u32,
    extra: &str,
) {
    crate::preload_debug!(
        "[PreloadDebug][Strip] heif_thumb_probe path={} outcome={} count={:?} id={:?} thumb={:?} primary={:?} decode_ms={:?} max_side={} {}",
        path.display(),
        probe.label(),
        detail.thumb_count,
        detail.thumb_id,
        detail
            .thumb_w
            .zip(detail.thumb_h)
            .map(|(w, h)| format!("{w}x{h}"))
            .unwrap_or_else(|| "-".to_string()),
        detail
            .primary_w
            .zip(detail.primary_h)
            .map(|(w, h)| format!("{w}x{h}"))
            .unwrap_or_else(|| "-".to_string()),
        detail.decode_ms,
        max_side,
        extra
    );
}

#[cfg(all(not(feature = "preload-debug"), feature = "heif-native"))]
fn log_strip_heif_probe(
    _path: &Path,
    _probe: HeifThumbProbe,
    _detail: HeifThumbProbeDetail,
    _max_side: u32,
    _extra: &str,
) {
}

#[cfg(feature = "preload-debug")]
fn log_strip_decode_path(path: &Path, kind: &str, logical: (u32, u32), out_w: u32, out_h: u32) {
    crate::preload_debug!(
        "[PreloadDebug][Strip] decode_path path={} kind={} logical={}x{} out={}x{}",
        path.display(),
        kind,
        logical.0,
        logical.1,
        out_w,
        out_h
    );
}

#[cfg(not(feature = "preload-debug"))]
fn log_strip_decode_path(
    _path: &Path,
    _kind: &str,
    _logical: (u32, u32),
    _out_w: u32,
    _out_h: u32,
) {
}

pub(crate) fn generate_directory_tree_thumb_decode_from_path(
    path: &Path,
    max_side: u32,
) -> Result<DirectoryTreeThumbDecode, String> {
    let gain_map_container = super::modern::path_may_have_gain_map_embedded_sdr_preview(path);
    // Heuristic: all AVIF/HEIF/JXL — wider than verified gain-map detection; see modern.rs.
    let placeholder_if_fast_path = embedded_sdr_strip_may_be_placeholder(gain_map_container);
    let mmap = crate::mmap_util::map_file(path).ok();
    let (exif, exif_probe, exif_probe_detail) = match mmap.as_ref() {
        Some(data) => extract_exif_thumbnail_from_mmap_probed(data, path),
        None => extract_exif_thumbnail_probed(path),
    };
    if gain_map_container {
        log_strip_exif_probe(
            path,
            exif_probe,
            exif_probe_detail,
            None,
            max_side,
            "phase=initial",
        );
    }
    if let Some(exif) = exif.as_ref() {
        let exif_logical = (exif.width, exif.height);
        let logical = normalize_logical_size(
            mmap.as_ref()
                .and_then(probe_still_image_logical_size_from_mmap)
                .unwrap_or(exif_logical),
            exif_logical,
        );
        if let Some(result) = try_directory_tree_exif_thumb(exif, logical, max_side) {
            log_strip_decode_path(
                path,
                "exif_embedded_sdr",
                logical,
                result.0.width,
                result.0.height,
            );
            return Ok(DirectoryTreeThumbDecode::new(
                result.0,
                result.1,
                None,
                placeholder_if_fast_path,
            ));
        }
        if gain_map_container {
            log_strip_exif_probe(
                path,
                ExifThumbProbe::AspectRejected,
                exif_probe_detail,
                Some(logical),
                max_side,
                &format!(
                    "phase=initial_rejected reason=aspect_or_downsample exif={}x{}",
                    exif.width, exif.height
                ),
            );
        }
    }

    #[cfg(feature = "heif-native")]
    if super::modern::is_heif_path(path) {
        let (heif_thumb, heif_probe, heif_detail) = match mmap.as_ref() {
            Some(data) => probe_heif_strip_thumbnail(data.as_ref(), max_side),
            None => probe_heif_strip_thumbnail_from_path(path, max_side),
        };
        log_strip_heif_probe(path, heif_probe, heif_detail, max_side, "phase=initial");
        if let Some((preview, logical)) = heif_thumb {
            log_strip_decode_path(
                path,
                "heif_container_thumb",
                logical,
                preview.width,
                preview.height,
            );
            return Ok(DirectoryTreeThumbDecode::new(
                preview,
                logical,
                None,
                placeholder_if_fast_path,
            ));
        }
        if matches!(
            heif_probe,
            HeifThumbProbe::AspectRejected | HeifThumbProbe::DownsampleFailed
        ) {
            log_strip_heif_probe(
                path,
                heif_probe,
                heif_detail,
                max_side,
                "phase=initial_rejected reason=aspect_or_downsample",
            );
        }
    }

    let path_buf = path.to_path_buf();
    if let Some(fast) = super::gain_map_strip::try_fast_iso_gain_map_strip_from_path(
        path,
        mmap.as_ref().map(|data| data.as_ref()),
        max_side,
    ) {
        return fast.map(|(preview, logical_size)| {
            log_strip_decode_path(
                path,
                "iso_gain_map_baseline",
                logical_size,
                preview.width,
                preview.height,
            );
            DirectoryTreeThumbDecode::new(preview, logical_size, None, false)
        });
    }
    if gain_map_container {
        crate::preload_debug!(
            "[PreloadDebug][Strip] fast_path_miss path={} kind=iso_gain_map_baseline",
            path.display()
        );
    }
    // DCT-scaled baseline-JPEG fast path: when no EXIF thumbnail exists, decode
    // directly at the scaled output size.  Ultra HDR / JPEG_R images also take
    // this path; the gain map is intentionally ignored because the strip only
    // needs a fast SDR preview.  For a 4000×3000 → 256px strip this is ~10×
    // faster and 64× less peak memory than a full-resolution decode followed
    // by a software downsample.
    if let Some(result) = try_jpeg_dct_strip_fast_path(path, mmap.as_ref(), max_side) {
        return result.map(|(preview, logical_size)| {
            log_strip_decode_path(
                path,
                "jpeg_dct",
                logical_size,
                preview.width,
                preview.height,
            );
            DirectoryTreeThumbDecode::new(preview, logical_size, None, false)
        });
    }
    if let Some(result) = try_static_raster_strip_fast_path(path, mmap.as_ref(), max_side) {
        match result {
            Ok(strip) => {
                log_strip_decode_path(
                    path,
                    "static_raster",
                    strip.logical_size,
                    strip.preview.width,
                    strip.preview.height,
                );
                return Ok(strip);
            }
            Err(err) => {
                log::debug!(
                    "[DirectoryTree] static raster strip fast path failed for {:?}: {err}; falling back to regular decode",
                    path.file_name().unwrap_or_default()
                );
            }
        }
    }
    let image_data = open_image_data_for_directory_tree_thumb(&path_buf, mmap.as_ref())?;
    let logical = logical_size_from_image_data(&image_data);

    if let Some(exif) = exif.as_ref()
        && let Some(result) = try_directory_tree_exif_thumb(exif, logical, max_side)
    {
        log_strip_decode_path(
            path,
            "exif_embedded_sdr_after_open",
            logical,
            result.0.width,
            result.0.height,
        );
        return Ok(DirectoryTreeThumbDecode::new(
            result.0,
            result.1,
            None,
            placeholder_if_fast_path,
        ));
    }
    if gain_map_container && exif.is_some() {
        log_strip_exif_probe(
            path,
            ExifThumbProbe::AspectRejected,
            exif_probe_detail,
            Some(logical),
            max_side,
            "phase=after_open_rejected reason=aspect_or_downsample",
        );
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
    log_strip_decode_path(
        path,
        "full_open_preview",
        logical,
        decoded.width,
        decoded.height,
    );
    Ok(DirectoryTreeThumbDecode::new(decoded, logical, None, false))
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
    path: &Path,
    file_mmap: Option<&memmap2::Mmap>,
) -> Result<ImageData, String> {
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown".to_string());
    let ext = path_extension_ascii_lower(path).unwrap_or_default();
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
            || {
                load_heif_hdr_aware(
                    path,
                    hdr_target_capacity,
                    hdr_tone_map,
                    crate::hdr::heif::HeifHdrDecodeDiag {
                        idx: None,
                        path: Some(path),
                    },
                )
            },
        );
    }

    if let Ok(reg) = crate::formats::get_registry().read()
        && reg.extensions.contains(&ext)
        && !is_maybe_animated(&ext)
    {
        #[cfg(target_os = "windows")]
        if let Ok(img) = crate::wic::load_via_wic(path, high_quality, None) {
            return Ok(apply_exif_orientation_to_image_data(path, img));
        }
        #[cfg(target_os = "macos")]
        if let Ok(img) = crate::macos_image_io::load_via_image_io(path, high_quality, None) {
            return Ok(apply_exif_orientation_to_image_data(path, img));
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

fn open_raw_image_data_for_directory_tree_thumb(path: &Path) -> Result<ImageData, String> {
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
    path: &Path,
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
    develop_half_size_sdr_strip_preview(&mut processor, path)
        .map(make_image_data)
        .ok_or_else(|| "no LibRaw strip preview available for RAW".to_string())
}

fn platform_still_image_fallback(
    path: &Path,
    opened_processor: Option<crate::raw_processor::RawProcessor>,
) -> Result<ImageData, String> {
    #[cfg(target_os = "windows")]
    {
        match crate::wic::load_via_wic(path, false, None) {
            Ok(img) => {
                return Ok(apply_exif_orientation_to_image_data(path, img));
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
                return Ok(apply_exif_orientation_to_image_data(path, img));
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
/// Returns `None` when the file is not a `.jpg`/`.jpeg` or no mmap data is
/// available.  Ultra HDR / JPEG_R images are also handled here; the gain map is
/// intentionally ignored because the strip only needs a fast SDR preview.  On
/// success the aspect ratio is guaranteed to match the logical size because DCT
/// scaling applies the same ratio to both axes.
fn try_jpeg_dct_strip_fast_path(
    path: &Path,
    mmap: Option<&memmap2::Mmap>,
    max_side: u32,
) -> OptionalStripResult {
    if !path_has_extension(path, "jpg") && !path_has_extension(path, "jpeg") {
        return None;
    }
    let data = mmap?;
    super::jpeg::try_decode_jpeg_strip_dct(data, max_side)
}

fn try_static_raster_strip_fast_path(
    path: &Path,
    mmap: Option<&memmap2::Mmap>,
    max_side: u32,
) -> Option<Result<DirectoryTreeThumbDecode, String>> {
    let ext = path_extension_ascii_lower(path)?;
    if !matches!(
        ext.as_str(),
        "png" | "apng" | "webp" | "gif" | "bmp" | "tga" | "ico" | "pnm" | "qoi"
    ) {
        return None;
    }
    let data = mmap?;
    Some(decode_static_raster_strip_from_bytes(
        data.as_ref(),
        max_side,
        Some(ext.as_str()),
    ))
}

fn decode_static_raster_strip_from_bytes(
    bytes: &[u8],
    max_side: u32,
    format_hint: Option<&str>,
) -> Result<DirectoryTreeThumbDecode, String> {
    use image::ImageReader;
    use std::io::Cursor;

    if max_side == 0 {
        return Err("static raster strip max_side must be non-zero".to_string());
    }

    let mut dimensions_reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    dimensions_reader.no_limits();
    let mut logical = dimensions_reader
        .into_dimensions()
        .map_err(|e| e.to_string())?;

    let mut decode_reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    decode_reader.no_limits();
    let rgba = decode_reader
        .decode()
        .map_err(|e| e.to_string())?
        .into_rgba8();
    let (width, height) = rgba.dimensions();
    let mut full = DecodedImage::new(width, height, rgba.into_raw());

    let orientation = crate::metadata_utils::get_exif_orientation_from_bytes(bytes);
    if orientation > 4 {
        logical = (logical.1, logical.0);
    }

    let mut decoded = downsample_decoded_to_max_side(full.clone(), max_side)?;
    let reusable_full_allowed = reusable_static_raster_full_decode(bytes, format_hint);

    decoded = apply_orientation_to_owned_decoded(decoded, orientation);
    if reusable_full_allowed {
        full = apply_orientation_to_owned_decoded(full, orientation);
    }

    Ok(DirectoryTreeThumbDecode::new(
        decoded,
        logical,
        reusable_full_allowed.then_some(full),
        false,
    ))
}

fn apply_orientation_to_owned_decoded(mut decoded: DecodedImage, orientation: u16) -> DecodedImage {
    if orientation <= 1 {
        return decoded;
    }
    let pixels = decoded.take_rgba_owned();
    let (width, height, pixels) = crate::libtiff_loader::apply_orientation_buffer(
        pixels,
        decoded.width,
        decoded.height,
        orientation,
    );
    DecodedImage::new(width, height, pixels)
}

fn reusable_static_raster_full_decode(bytes: &[u8], format_hint: Option<&str>) -> bool {
    match format_hint {
        Some("png") => png_bytes_are_static(bytes),
        Some("webp") => webp_bytes_are_static(bytes),
        Some("bmp" | "tga" | "ico" | "pnm" | "qoi") => true,
        _ => false,
    }
}

fn png_bytes_are_static(bytes: &[u8]) -> bool {
    use image::codecs::png::PngDecoder;
    use std::io::Cursor;

    PngDecoder::new(Cursor::new(bytes))
        .and_then(|decoder| decoder.is_apng())
        .map(|is_apng| !is_apng)
        .unwrap_or(false)
}

fn webp_bytes_are_static(bytes: &[u8]) -> bool {
    use image::codecs::webp::WebPDecoder;
    use std::io::Cursor;

    WebPDecoder::new(Cursor::new(bytes))
        .map(|decoder| !decoder.has_animation())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::normalize_logical_size;
    use crate::loader::preview_aspect_matches_logical;
    use image::ImageEncoder;

    fn encode_test_png(width: u32, height: u32) -> Vec<u8> {
        let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
        for y in 0..height {
            for x in 0..width {
                pixels.extend_from_slice(&[
                    (x % 251) as u8,
                    (y % 241) as u8,
                    ((x + y) % 239) as u8,
                    255,
                ]);
            }
        }
        let mut encoded = Vec::new();
        image::codecs::png::PngEncoder::new(&mut encoded)
            .write_image(&pixels, width, height, image::ColorType::Rgba8.into())
            .expect("encode test PNG");
        encoded
    }

    fn encode_test_webp(width: u32, height: u32) -> Vec<u8> {
        let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
        for y in 0..height {
            for x in 0..width {
                pixels.extend_from_slice(&[
                    (x % 251) as u8,
                    (y % 241) as u8,
                    ((x + y) % 239) as u8,
                    255,
                ]);
            }
        }
        let mut encoded = Vec::new();
        image::codecs::webp::WebPEncoder::new_lossless(&mut encoded)
            .write_image(&pixels, width, height, image::ColorType::Rgba8.into())
            .expect("encode test WebP");
        encoded
    }

    fn test_crc32(bytes: &[u8]) -> u32 {
        let mut crc = 0xffff_ffffu32;
        for &byte in bytes {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                let mask = 0u32.wrapping_sub(crc & 1);
                crc = (crc >> 1) ^ (0xedb8_8320 & mask);
            }
        }
        !crc
    }

    fn append_test_png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let mut crc_input = Vec::with_capacity(kind.len() + data.len());
        crc_input.extend_from_slice(kind);
        crc_input.extend_from_slice(data);
        out.extend_from_slice(&test_crc32(&crc_input).to_be_bytes());
    }

    fn inject_test_apng_actl_chunk(png: &[u8]) -> Vec<u8> {
        const PNG_SIGNATURE_LEN: usize = 8;
        const IHDR_CHUNK_TOTAL_LEN: usize = 4 + 4 + 13 + 4;
        let insert_at = PNG_SIGNATURE_LEN + IHDR_CHUNK_TOTAL_LEN;
        let mut out = Vec::with_capacity(png.len() + 20);
        out.extend_from_slice(&png[..insert_at]);
        let mut actl = Vec::with_capacity(8);
        actl.extend_from_slice(&1u32.to_be_bytes());
        actl.extend_from_slice(&0u32.to_be_bytes());
        append_test_png_chunk(&mut out, b"acTL", &actl);
        out.extend_from_slice(&png[insert_at..]);
        out
    }

    fn chunk_payload<'a>(container: &'a [u8], kind: &[u8; 4]) -> &'a [u8] {
        let mut pos = 12usize;
        while pos + 8 <= container.len() {
            let size = u32::from_le_bytes(
                container[pos + 4..pos + 8]
                    .try_into()
                    .expect("chunk size bytes"),
            ) as usize;
            let payload_start = pos + 8;
            let payload_end = payload_start + size;
            if &container[pos..pos + 4] == kind {
                return &container[payload_start..payload_end];
            }
            pos = payload_end + (size % 2);
        }
        panic!("missing WebP chunk {:?}", kind);
    }

    fn append_test_webp_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(kind);
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(data);
        if data.len() % 2 != 0 {
            out.push(0);
        }
    }

    fn animated_webp_from_static_webp(static_webp: &[u8], width: u32, height: u32) -> Vec<u8> {
        let vp8l = chunk_payload(static_webp, b"VP8L");

        let mut chunks = Vec::new();
        let mut vp8x = Vec::with_capacity(10);
        vp8x.push(0b0000_0010);
        vp8x.extend_from_slice(&[0, 0, 0]);
        vp8x.extend_from_slice(&(width - 1).to_le_bytes()[..3]);
        vp8x.extend_from_slice(&(height - 1).to_le_bytes()[..3]);
        append_test_webp_chunk(&mut chunks, b"VP8X", &vp8x);

        let mut anim = Vec::with_capacity(6);
        anim.extend_from_slice(&[0, 0, 0, 0]);
        anim.extend_from_slice(&0u16.to_le_bytes());
        append_test_webp_chunk(&mut chunks, b"ANIM", &anim);

        let mut anmf = Vec::new();
        anmf.extend_from_slice(&[0, 0, 0]);
        anmf.extend_from_slice(&[0, 0, 0]);
        anmf.extend_from_slice(&(width - 1).to_le_bytes()[..3]);
        anmf.extend_from_slice(&(height - 1).to_le_bytes()[..3]);
        anmf.extend_from_slice(&100u32.to_le_bytes()[..3]);
        anmf.push(0);
        append_test_webp_chunk(&mut anmf, b"VP8L", vp8l);
        append_test_webp_chunk(&mut chunks, b"ANMF", &anmf);

        let mut out = Vec::with_capacity(12 + chunks.len());
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&((4 + chunks.len()) as u32).to_le_bytes());
        out.extend_from_slice(b"WEBP");
        out.extend_from_slice(&chunks);
        out
    }

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

    #[test]
    fn static_raster_strip_from_mmap_downsamples_png_to_max_side() {
        let encoded = encode_test_png(120, 60);
        let strip = super::decode_static_raster_strip_from_bytes(&encoded, 30, Some("png"))
            .expect("decode PNG strip");
        let decoded = strip.preview;
        let logical = strip.logical_size;

        assert_eq!(logical, (120, 60));
        assert_eq!(decoded.width, 30);
        assert_eq!(decoded.height, 15);
        assert!(preview_aspect_matches_logical(
            decoded.width,
            decoded.height,
            logical.0,
            logical.1
        ));
        assert_eq!(decoded.rgba().len(), 30 * 15 * 4);
    }

    #[test]
    fn static_raster_strip_from_static_png_keeps_reusable_full_decode() {
        let encoded = encode_test_png(120, 60);
        let strip = super::decode_static_raster_strip_from_bytes(&encoded, 30, Some("png"))
            .expect("decode PNG strip");
        let full = strip
            .reusable_full
            .expect("static PNG strip decode should retain full image for preload reuse");

        assert_eq!(full.width, 120);
        assert_eq!(full.height, 60);
        assert_eq!(full.rgba().len(), 120 * 60 * 4);
    }

    #[test]
    fn static_raster_strip_from_static_webp_keeps_reusable_full_decode() {
        let encoded = encode_test_webp(80, 40);
        let strip = super::decode_static_raster_strip_from_bytes(&encoded, 20, Some("webp"))
            .expect("decode WebP strip");
        let full = strip
            .reusable_full
            .expect("static WebP strip decode should retain full image for preload reuse");

        assert_eq!(full.width, 80);
        assert_eq!(full.height, 40);
        assert_eq!(full.rgba().len(), 80 * 40 * 4);
    }

    #[test]
    fn static_raster_reuse_rejects_apng() {
        let encoded = encode_test_png(8, 4);
        let apng = inject_test_apng_actl_chunk(&encoded);

        assert!(!super::png_bytes_are_static(&apng));
        let strip = super::decode_static_raster_strip_from_bytes(&apng, 8, Some("png"))
            .expect("decode APNG default image");
        assert!(
            strip.reusable_full.is_none(),
            "APNG must not reuse the default image as the main static decode"
        );
    }

    #[test]
    fn static_raster_reuse_rejects_animated_webp() {
        let static_webp = encode_test_webp(8, 4);
        let animated_webp = animated_webp_from_static_webp(&static_webp, 8, 4);

        assert!(!super::webp_bytes_are_static(&animated_webp));
    }

    #[test]
    fn owned_decoded_orientation_rotates_reusable_full_image() {
        let pixels = vec![
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255, 255, 0, 255, 255, 0,
            255, 255, 255,
        ];
        let decoded = super::apply_orientation_to_owned_decoded(
            crate::loader::DecodedImage::new(2, 3, pixels),
            6,
        );

        assert_eq!(decoded.width, 3);
        assert_eq!(decoded.height, 2);
        assert_eq!(decoded.rgba().len(), 2 * 3 * 4);
    }
}
