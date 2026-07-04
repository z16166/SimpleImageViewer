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
use std::sync::Arc;

#[cfg(feature = "heif-native")]
use crate::hdr::heif::{
    HeifDirectoryTreeStripOutcome, HeifThumbProbe, try_heif_directory_tree_strip,
};
#[cfg(any(target_os = "windows", target_os = "macos"))]
use crate::loader::apply_exif_orientation_to_image_data;
use crate::loader::downsample_decoded_for_strip;
use crate::loader::metadata::ExifThumbProbe;
use crate::loader::{
    DecodedImage, ImageData, TiledImageSource, extract_exif_thumbnail_from_mmap_probed,
    extract_exif_thumbnail_probed, preview_aspect_matches_logical,
};

use super::assemble::make_image_data;
use super::detect::load_primary_with_detection_fallback;
use super::hdr_formats::load_hdr;
use super::is_maybe_animated;
use super::jpeg::load_jpeg_with_target_capacity;
use super::modern::{
    load_avif_with_target_capacity, load_heif_hdr_aware, load_jxl_with_target_capacity,
};
use super::open_raw_processor_with_preview;
use super::raster::{load_gif, load_png, load_psd, load_static, load_webp};

mod probe_log;
mod static_raster;

#[cfg(feature = "heif-native")]
use probe_log::log_strip_heif_probe;
use probe_log::{log_strip_decode_path, log_strip_exif_probe};
use static_raster::try_static_raster_strip_fast_path;

type StripWithLogicalSize = (DecodedImage, (u32, u32));
type OptionalStripResult = Option<Result<StripWithLogicalSize, String>>;

/// Controls which strip cold-decode fallbacks are allowed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DirectoryTreeThumbDecodeOptions {
    /// When set, skip strip paths that duplicate the main loader's full-image decode
    /// (`static_raster`, `open_image_data_for_directory_tree_thumb`). Cheap strip paths
    /// (EXIF/HEIF container thumb, JPEG DCT scale, HDR float preview) still run.
    pub skip_slow_embedded_sdr_primary: bool,
    /// When set (together with [`Self::skip_slow_embedded_sdr_primary`]), skip ISO gain-map
    /// baseline strip decode so the main loader's embedded SDR master path owns that work.
    pub defer_iso_gain_map_baseline: bool,
}

/// Returned when only a full decode would apply while the main loader should own it;
/// caller waits for preload install or texture/HDR-cache strip sync.
pub(crate) const STRIP_DEFER_SLOW_EMBEDDED_SDR: &str = "strip_deferred_slow_embedded_sdr_primary";

pub(crate) struct DirectoryTreeThumbDecode {
    pub(crate) preview: DecodedImage,
    pub(crate) logical_size: (u32, u32),
    pub(crate) reusable_full: Option<DecodedImage>,
    /// Gain-map modern-format placeholder only (AVIF/HEIF/JXL via EXIF, container thumb, or
    /// primary-SDR fast paths). Not set for other fast paths (`hdr_float_preview`,
    /// `iso_gain_map_baseline`, `jpeg_dct`, etc.).
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

pub(crate) fn generate_directory_tree_thumb_decode_from_path(
    path: &Path,
    max_side: u32,
    options: DirectoryTreeThumbDecodeOptions,
) -> Result<DirectoryTreeThumbDecode, String> {
    let gain_map_container = super::modern::path_may_have_gain_map_embedded_sdr_preview(path);
    // Heuristic: all AVIF/HEIF/JXL — wider than verified gain-map detection; see modern.rs.
    let placeholder_if_fast_path = embedded_sdr_strip_may_be_placeholder(gain_map_container);
    let mmap = crate::mmap_util::map_file(path).ok().map(Arc::new);
    let (exif, exif_probe, exif_probe_detail) = match mmap.as_deref() {
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
                .and_then(|data| probe_still_image_logical_size_from_bytes(data.as_ref(), path))
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
        let allow_primary_sdr = !options.skip_slow_embedded_sdr_primary;
        let heif_outcome = match mmap.as_deref() {
            Some(data) => try_heif_directory_tree_strip(data.as_ref(), max_side, allow_primary_sdr),
            None => crate::mmap_util::map_file(path)
                .ok()
                .map(Arc::new)
                .and_then(|owned| {
                    Some(try_heif_directory_tree_strip(
                        owned.as_ref(),
                        max_side,
                        allow_primary_sdr,
                    ))
                })
                .unwrap_or_else(|| HeifDirectoryTreeStripOutcome {
                    strip: None,
                    thumb_probe: HeifThumbProbe::ContainerUnreadable,
                    thumb_detail: Default::default(),
                    decode_path: None,
                }),
        };
        log_strip_heif_probe(
            path,
            heif_outcome.thumb_probe,
            heif_outcome.thumb_detail,
            max_side,
            "phase=initial",
        );
        if let Some(result) = heif_outcome.strip {
            return result.map(|(preview, logical)| {
                log_strip_decode_path(
                    path,
                    heif_outcome.decode_path.unwrap_or("heif_primary_sdr"),
                    logical,
                    preview.width,
                    preview.height,
                );
                DirectoryTreeThumbDecode::new(preview, logical, None, placeholder_if_fast_path)
            });
        }
        if matches!(
            heif_outcome.thumb_probe,
            HeifThumbProbe::AspectRejected | HeifThumbProbe::DownsampleFailed
        ) {
            log_strip_heif_probe(
                path,
                heif_outcome.thumb_probe,
                heif_outcome.thumb_detail,
                max_side,
                "phase=initial_rejected reason=aspect_or_downsample",
            );
        }
        if !allow_primary_sdr {
            #[cfg(feature = "preload-debug")]
            crate::preload_debug!(
                "[PreloadDebug][Strip] skip slow path path={} kind=heif_primary_sdr reason=preload_shared_primary",
                path.display()
            );
        }
    }

    if let Some(fast) = super::hdr_strip_fast::try_fast_hdr_float_strip_from_path(
        path,
        mmap.as_ref(),
        max_side,
    ) {
        return fast.map(|(preview, logical_size)| {
            log_strip_decode_path(
                path,
                "hdr_float_preview",
                logical_size,
                preview.width,
                preview.height,
            );
            // Float HDR has no gain map; strip is final (no PreloadSdrFallback upgrade).
            DirectoryTreeThumbDecode::new(preview, logical_size, None, false)
        });
    }
    if !options.defer_iso_gain_map_baseline {
        if let Some(fast) = super::gain_map_strip::try_fast_iso_gain_map_strip_from_path(
            path,
            mmap.as_deref().map(|data| data.as_ref()),
            max_side,
        ) {
            match fast {
                Ok(result) => {
                    log_strip_decode_path(
                        path,
                        "iso_gain_map_fast",
                        result.logical_size,
                        result.preview.width,
                        result.preview.height,
                    );
                    return Ok(DirectoryTreeThumbDecode::new(
                        result.preview,
                        result.logical_size,
                        None,
                        false,
                    ));
                }
                Err(err) => {
                    log::debug!(
                        "[DirectoryTree] ISO gain-map strip fast path failed for {:?}: {err}; falling back to regular decode",
                        path.file_name().unwrap_or_default()
                    );
                }
            }
        }
    } else if gain_map_container {
        #[cfg(feature = "preload-debug")]
        crate::preload_debug!(
            "[PreloadDebug][Strip] skip iso_gain_map_baseline path={} reason=embedded_sdr_master_main_loader",
            path.display()
        );
    }
    if gain_map_container && !options.defer_iso_gain_map_baseline {
        let log_iso_gain_map_miss = match mmap.as_ref().map(|data| data.as_ref()) {
            #[cfg(feature = "avif-native")]
            Some(bytes)
                if path_has_extension(path, "avif") || path_has_extension(path, "avifs") =>
            {
                matches!(
                    crate::hdr::avif::avif_probe_gain_map_strip_kind(bytes),
                    Some(crate::hdr::avif::AvifGainMapStripProbe::ForwardIsoGainMap)
                        | Some(crate::hdr::avif::AvifGainMapStripProbe::PrecomposedHdr)
                )
            }
            #[cfg(not(feature = "avif-native"))]
            Some(_) if path_has_extension(path, "avif") || path_has_extension(path, "avifs") => {
                true
            }
            _ => true,
        };
        if log_iso_gain_map_miss {
            crate::preload_debug!(
                "[PreloadDebug][Strip] fast_path_miss path={} kind=iso_gain_map_baseline",
                path.display()
            );
        }
    }
    // DCT-scaled baseline-JPEG fast path: when no EXIF thumbnail exists, decode
    // directly at the scaled output size.  Ultra HDR / JPEG_R images also take
    // this path; the gain map is intentionally ignored because the strip only
    // needs a fast SDR preview.  For a 4000×3000 → 256px strip this is ~10×
    // faster and 64× less peak memory than a full-resolution decode followed
    // by a software downsample.
    if let Some(result) = try_jpeg_dct_strip_fast_path(path, mmap.as_deref(), max_side) {
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
    if options.skip_slow_embedded_sdr_primary {
        #[cfg(feature = "preload-debug")]
        crate::preload_debug!(
            "[PreloadDebug][Strip] defer cold path={} reason=await_main_loader_full_decode",
            path.display()
        );
        return Err(STRIP_DEFER_SLOW_EMBEDDED_SDR.to_string());
    }
    if let Some(result) = try_static_raster_strip_fast_path(path, mmap.as_deref(), max_side) {
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
    let path_buf = path.to_path_buf();
    let image_data = open_image_data_for_directory_tree_thumb(&path_buf, mmap.as_deref())?;
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
    Ok(DirectoryTreeThumbDecode::new(
        decoded,
        logical,
        reusable_full_decoded_from_image_data(&image_data),
        false,
    ))
}

fn reusable_full_decoded_from_image_data(image_data: &ImageData) -> Option<DecodedImage> {
    match image_data {
        ImageData::Static(image) => Some(image.clone()),
        ImageData::Hdr { fallback, .. } if !fallback.is_sdr_deferred_placeholder() => {
            Some(fallback.clone())
        }
        ImageData::Animated(frames) => frames
            .first()
            .map(|frame| DecodedImage::from_arc(frame.width, frame.height, frame.arc_pixels())),
        _ => None,
    }
}

fn normalize_logical_size(logical: (u32, u32), fallback: (u32, u32)) -> (u32, u32) {
    if logical.0 == 0 || logical.1 == 0 {
        fallback
    } else {
        logical
    }
}

fn probe_still_image_logical_size_from_bytes(bytes: &[u8], path: &Path) -> Option<(u32, u32)> {
    let ext = path
        .extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    #[cfg(feature = "avif-native")]
    if (ext == "avif" || ext == "avifs")
        && let Some(logical) = crate::hdr::avif::libavif_probe_logical_size_from_bytes(bytes)
    {
        return Some(logical);
    }
    #[cfg(feature = "jpegxl")]
    if ext == "jxl"
        && let Some(logical) = crate::hdr::jpegxl::libjxl_probe_logical_size_from_bytes(bytes)
    {
        return Some(logical);
    }
    #[cfg(feature = "heif-native")]
    if super::modern::is_heif_path(path)
        && let Some(logical) = crate::hdr::heif::libheif_probe_logical_size_from_bytes(bytes)
    {
        return Some(logical);
    }
    use std::io::Cursor;
    image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

fn path_has_extension(path: &Path, ext: &str) -> bool {
    path.extension()
        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(ext))
}

pub(super) fn path_extension_ascii_lower(path: &Path) -> Option<String> {
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
                        false,
                    )
                } else {
                    load_jpeg_with_target_capacity(path, hdr_target_capacity, hdr_tone_map, false)
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
            || load_avif_with_target_capacity(path, hdr_target_capacity, hdr_tone_map, false),
        );
    }

    if path_has_extension(path, "jxl") {
        return load_primary_with_detection_fallback(
            path,
            file_name.as_str(),
            hdr_target_capacity,
            hdr_tone_map,
            high_quality,
            || load_jxl_with_target_capacity(path, hdr_target_capacity, hdr_tone_map, false),
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
                    false,
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
            return Ok(apply_exif_orientation_to_image_data(path, img, None));
        }
        #[cfg(target_os = "macos")]
        if let Ok(img) = crate::macos_image_io::load_via_image_io(path, high_quality, None) {
            return Ok(apply_exif_orientation_to_image_data(path, img, None));
        }
    }

    load_primary_with_detection_fallback(
        path,
        file_name.as_str(),
        hdr_target_capacity,
        hdr_tone_map,
        high_quality,
        || match ext.as_str() {
            "png" | "apng" => load_png(path, hdr_target_capacity, hdr_tone_map),
            "webp" => load_webp(path, hdr_target_capacity, hdr_tone_map),
            "gif" => load_gif(path, hdr_target_capacity, hdr_tone_map),
            _ => load_static(path, hdr_target_capacity, hdr_tone_map),
        },
    )
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
                return Ok(apply_exif_orientation_to_image_data(path, img, None));
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
                return Ok(apply_exif_orientation_to_image_data(path, img, None));
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

    #[test]
    fn defer_constant_is_stable_for_poll_matching() {
        assert_eq!(
            super::STRIP_DEFER_SLOW_EMBEDDED_SDR,
            "strip_deferred_slow_embedded_sdr_primary"
        );
    }

    #[test]
    fn defer_full_decode_to_main_loader_applies_to_static_raster() {
        use image::ColorType;
        use image::ImageEncoder;
        use image::codecs::png::PngEncoder;
        use std::io::Cursor;

        let mut png = Cursor::new(Vec::new());
        PngEncoder::new(&mut png)
            .write_image(&[255, 0, 0, 255], 1, 1, ColorType::Rgba8.into())
            .expect("encode 1x1 png");
        let path =
            std::env::temp_dir().join(format!("siv_strip_defer_test_{}.png", std::process::id()));
        std::fs::write(&path, png.into_inner()).expect("write png");

        let defer_err = super::generate_directory_tree_thumb_decode_from_path(
            &path,
            256,
            super::DirectoryTreeThumbDecodeOptions {
                skip_slow_embedded_sdr_primary: true,
                defer_iso_gain_map_baseline: false,
            },
        );
        match defer_err {
            Err(err) => assert_eq!(err, super::STRIP_DEFER_SLOW_EMBEDDED_SDR),
            Ok(_) => panic!("PNG strip should defer when main loader owns full decode"),
        }

        let ok = super::generate_directory_tree_thumb_decode_from_path(
            &path,
            256,
            super::DirectoryTreeThumbDecodeOptions::default(),
        )
        .expect("PNG strip without defer");
        assert_eq!((ok.preview.width, ok.preview.height), (1, 1));
        let _ = std::fs::remove_file(path);
    }

    /// libavif cannot decode some libavif test vectors; strip cold path must fall back to WIC.
    #[cfg(all(feature = "avif-native", target_os = "windows"))]
    #[test]
    fn avif_strip_falls_back_when_libavif_primary_scaled_fails() {
        let base = std::path::Path::new(r"F:\HDR\libavif\tests\data");
        for name in [
            "clap_irot_imir_non_essential.avif",
            "color_grid_alpha_grid_tile_shared_in_dimg.avif",
        ] {
            let path = base.join(name);
            if !path.is_file() {
                eprintln!("skip missing {}", path.display());
                continue;
            }
            let strip = super::generate_directory_tree_thumb_decode_from_path(
                &path,
                128,
                super::DirectoryTreeThumbDecodeOptions::default(),
            )
            .unwrap_or_else(|err| panic!("strip decode for {name}: {err}"));
            assert!(
                strip.preview.width > 0 && strip.preview.height > 0,
                "{name}: expected non-empty strip preview"
            );
            assert!(
                !strip.preview.is_sdr_deferred_placeholder(),
                "{name}: strip must not be deferred placeholder"
            );
        }
    }
}
