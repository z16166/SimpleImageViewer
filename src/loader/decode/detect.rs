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

//! Content sniffing.

use crate::hdr::types::HdrToneMapSettings;
use crate::loader::ImageData;
use std::path::Path;
use std::sync::Arc;

use super::hdr_formats::{load_detected_exr_from_mmap, load_hdr_from_mmap};
use super::jpeg::load_jpeg_from_mapped;
use super::modern::{
    load_avif_with_target_capacity_from_mmap, load_heif_hdr_aware_from_mmap,
    load_jxl_with_target_capacity_from_mmap,
};
use super::raster::{
    load_gif_from_mmap, load_png_from_mmap, load_static_from_mmap, load_webp_from_mmap,
};

const DETECTION_BUFFER_SIZE: usize = 16;

/// Read the major brand from an ISO BMFF `ftyp` box at the start of `bytes` (offset 4).
pub(crate) fn bmff_ftyp_brand(bytes: &[u8]) -> Option<[u8; 4]> {
    if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        let mut brand = [0u8; 4];
        brand.copy_from_slice(&bytes[8..12]);
        Some(brand)
    } else {
        None
    }
}

/// BMFF brands that denote motion/video containers, not still images (e.g. iPhone Live Photo `.MOV` mislabeled as `.JPG`).
pub(crate) fn is_motion_video_bmff_brand(brand: &[u8; 4]) -> bool {
    matches!(
        brand,
        b"qt  " | b"mov " | b"m4v " | b"3gp " | b"3g2 " | b"mp41" | b"mp42" | b"avc1" | b"iso2"
    )
}

/// Stable marker for errors that must not trigger recovery re-opens (see `primary_decode_failure_is_final`).
pub(crate) const MOTION_VIDEO_BMFF_ERROR_TAG: &str = "MOTION_VIDEO_BMFF";

pub(crate) fn motion_video_bmff_error(brand: &[u8; 4]) -> String {
    let brand_label = std::str::from_utf8(brand).unwrap_or("????");
    format!(
        "[{MOTION_VIDEO_BMFF_ERROR_TAG}] ISO BMFF container (ftyp {brand_label:?}) is a video/Live Photo motion component, not a still image; \
         open the paired photo file or export a JPEG/HEIC still"
    )
}

/// Primary extension-first decode already ruled out recovery (single mmap pass).
pub(crate) fn primary_decode_failure_is_final(primary_err: &str) -> bool {
    primary_err.contains(MOTION_VIDEO_BMFF_ERROR_TAG)
}

fn load_bmff_ftyp_container(
    path: &Path,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
    brand: &[u8],
) -> Result<ImageData, String> {
    if brand.len() == 4 {
        let brand_arr = [brand[0], brand[1], brand[2], brand[3]];
        if is_motion_video_bmff_brand(&brand_arr) {
            return Err(motion_video_bmff_error(&brand_arr));
        }
    }

    let brand_label = std::str::from_utf8(brand).unwrap_or("????");
    log::info!(
        "[Loader] {} has ISO BMFF ftyp brand {brand_label:?}; trying container still-image decoders",
        path.display()
    );

    #[cfg(target_os = "windows")]
    if let Ok(image) = crate::wic::load_via_wic_stream_sniff(path, true, None) {
        // WIC applies EXIF orientation during decode; do not chain apply_exif (double-rotate).
        return Ok(image);
    }

    #[cfg(target_os = "macos")]
    if let Ok(image) = crate::macos_image_io::load_via_image_io(path, true, None) {
        // ImageIO applies EXIF orientation during decode; do not chain apply_exif.
        return Ok(image);
    }

    let _ = (hdr_target_capacity, hdr_tone_map);
    Err(format!(
        "ISO BMFF container (ftyp {brand_label:?}) is not a decodable still image; \
         the file may be a Live Photo motion/video component with a wrong extension"
    ))
}

pub(crate) fn mmap_for_content_detection(path: &Path) -> Result<Arc<memmap2::Mmap>, String> {
    Ok(Arc::new(crate::mmap_util::map_file(path)?))
}

fn load_by_image_format_from_mmap(
    format: image::ImageFormat,
    path: &Path,
    mmap: &Arc<memmap2::Mmap>,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
) -> Result<ImageData, String> {
    let bytes = mmap.as_ref();
    match format {
        image::ImageFormat::Png => {
            load_png_from_mmap(path, bytes, hdr_target_capacity, hdr_tone_map)
        }
        image::ImageFormat::Gif => {
            load_gif_from_mmap(path, bytes, hdr_target_capacity, hdr_tone_map)
        }
        image::ImageFormat::WebP => {
            load_webp_from_mmap(path, bytes, hdr_target_capacity, hdr_tone_map)
        }
        image::ImageFormat::Tiff => crate::libtiff_loader::load_via_libtiff_from_mmap(
            path,
            Arc::clone(mmap),
            hdr_target_capacity,
            hdr_tone_map,
        ),
        image::ImageFormat::Jpeg => load_jpeg_from_mapped(
            path,
            mmap.as_ref(),
            hdr_target_capacity,
            hdr_tone_map,
            false,
        ),
        image::ImageFormat::Bmp
        | image::ImageFormat::Ico
        | image::ImageFormat::Pnm
        | image::ImageFormat::Tga
        | image::ImageFormat::Dds
        | image::ImageFormat::Farbfeld
        | image::ImageFormat::Qoi => {
            load_static_from_mmap(path, bytes, hdr_target_capacity, hdr_tone_map)
        }
        image::ImageFormat::Avif => load_avif_with_target_capacity_from_mmap(
            path,
            mmap,
            hdr_target_capacity,
            hdr_tone_map,
            false,
        ),
        image::ImageFormat::Hdr => {
            load_hdr_from_mmap(path, Arc::clone(mmap), hdr_target_capacity, hdr_tone_map)
        }
        image::ImageFormat::OpenExr => load_detected_exr_from_mmap(path, Arc::clone(mmap)),
        _ => Err(rust_i18n::t!(
            "error.unsupported_detected_format",
            format = format!("{:?}", format)
        )
        .to_string()),
    }
}

/// Outcome of an extension-first decode attempt, optionally retaining the primary mmap.
pub(crate) struct PrimaryDecodeAttempt {
    pub result: Result<ImageData, String>,
    pub detection_mmap: Option<Arc<memmap2::Mmap>>,
}

impl PrimaryDecodeAttempt {
    #[inline]
    pub(crate) fn from_result(result: Result<ImageData, String>) -> Self {
        Self {
            result,
            detection_mmap: None,
        }
    }

    #[inline]
    pub(crate) fn with_mmap(
        result: Result<ImageData, String>,
        detection_mmap: Option<Arc<memmap2::Mmap>>,
    ) -> Self {
        Self {
            result,
            detection_mmap,
        }
    }
}

/// Map `path` once and pass the mmap to `decode` for recovery-path reuse on failure.
pub(crate) fn primary_with_retainable_mmap(
    path: &Path,
    decode: impl FnOnce(Arc<memmap2::Mmap>) -> Result<ImageData, String>,
) -> PrimaryDecodeAttempt {
    match crate::mmap_util::map_file(path) {
        Ok(mmap) => {
            let arc = Arc::new(mmap);
            PrimaryDecodeAttempt::with_mmap(decode(Arc::clone(&arc)), Some(arc))
        }
        Err(e) => PrimaryDecodeAttempt::from_result(Err(e.to_string())),
    }
}

/// Reuse `existing` when already mapped (e.g. directory-tree thumb prefetch), else map once.
pub(crate) fn primary_with_optional_mmap(
    existing: Option<Arc<memmap2::Mmap>>,
    path: &Path,
    decode: impl FnOnce(Arc<memmap2::Mmap>) -> Result<ImageData, String>,
) -> PrimaryDecodeAttempt {
    match existing {
        Some(arc) => PrimaryDecodeAttempt::with_mmap(decode(Arc::clone(&arc)), Some(arc)),
        None => primary_with_retainable_mmap(path, decode),
    }
}

/// After extension-first decode fails: platform decoder (WIC/ImageIO), then magic-byte routing.
pub(crate) fn recover_via_platform_and_content_detection(
    path: &Path,
    file_name: &str,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
    #[cfg_attr(
        not(any(target_os = "windows", target_os = "macos")),
        allow(unused_variables)
    )]
    high_quality: bool,
    detection_mmap: Option<Arc<memmap2::Mmap>>,
    primary_err: String,
) -> Result<ImageData, String> {
    if primary_decode_failure_is_final(&primary_err) {
        return Err(primary_err);
    }

    #[cfg(target_os = "windows")]
    if let Ok(image) = match detection_mmap.as_ref() {
        Some(mmap) => crate::wic::load_via_wic_from_mmap(
            path,
            std::sync::Arc::clone(mmap),
            high_quality,
            None,
        ),
        None => crate::wic::load_via_wic_stream_sniff(path, high_quality, None),
    } {
        log::info!(
            "[{}] Recovered via WIC after extension-first decode failed",
            file_name
        );
        // WIC applies EXIF orientation during decode; do not chain apply_exif (double-rotate).
        return Ok(image);
    }
    #[cfg(target_os = "macos")]
    if let Ok(image) = match detection_mmap.as_ref() {
        Some(mmap) => crate::macos_image_io::load_via_image_io_from_mmap(
            path,
            std::sync::Arc::clone(mmap),
            high_quality,
            None,
        ),
        None => crate::macos_image_io::load_via_image_io(path, high_quality, None),
    } {
        log::info!(
            "[{}] Recovered via ImageIO after extension-first decode failed",
            file_name
        );
        // ImageIO applies EXIF orientation during decode; do not chain apply_exif.
        return Ok(image);
    }

    match load_via_content_detection(path, detection_mmap, hdr_target_capacity, hdr_tone_map) {
        Ok(image) => {
            log::info!(
                "[{}] Recovered via content-based detection after extension-first decode failed",
                file_name
            );
            Ok(image)
        }
        Err(detection_err)
            if detection_err.contains("ISO BMFF")
                || detection_err.contains("Live Photo")
                || detection_err.contains("detection_failed") =>
        {
            Err(detection_err)
        }
        Err(_) => Err(primary_err),
    }
}

/// Run the extension-matched loader first; only mislabeled or mismatched files pay sniffing cost.
pub(crate) fn load_primary_with_detection_fallback(
    path: &Path,
    file_name: &str,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
    high_quality: bool,
    primary: impl FnOnce() -> PrimaryDecodeAttempt,
) -> Result<ImageData, String> {
    let PrimaryDecodeAttempt {
        result,
        detection_mmap,
    } = primary();
    match result {
        Ok(image) => Ok(image),
        Err(primary_err) => {
            log::debug!(
                "[{}] Extension-first decode failed ({primary_err}); trying recovery loaders",
                file_name
            );
            recover_via_platform_and_content_detection(
                path,
                file_name,
                hdr_target_capacity,
                hdr_tone_map,
                high_quality,
                detection_mmap,
                primary_err,
            )
        }
    }
}

pub(crate) fn load_via_content_detection(
    path: &Path,
    detection_mmap: Option<Arc<memmap2::Mmap>>,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
) -> Result<ImageData, String> {
    let mmap = match detection_mmap {
        Some(mmap) => mmap,
        None => mmap_for_content_detection(path)?,
    };
    let n = mmap.len().min(DETECTION_BUFFER_SIZE);
    let header = &mmap[..n];

    // 1. Try standard image-rs detection
    if let Ok(guessed) = image::guess_format(header) {
        return load_by_image_format_from_mmap(
            guessed,
            path,
            &mmap,
            hdr_target_capacity,
            hdr_tone_map,
        );
    }

    if crate::hdr::jpegxl::is_jxl_header(header) {
        return load_jxl_with_target_capacity_from_mmap(
            path,
            &mmap,
            hdr_target_capacity,
            hdr_tone_map,
            false,
        );
    }

    // 2. Manual BMFF detection (image-rs 0.25 does not guess HEIF/AVIF/QuickTime).
    if n >= 12 && &header[4..8] == b"ftyp" {
        let brand = &header[8..12];
        if crate::hdr::avif::is_avif_brand(brand) {
            return load_avif_with_target_capacity_from_mmap(
                path,
                &mmap,
                hdr_target_capacity,
                hdr_tone_map,
                false,
            );
        }
        if crate::hdr::heif::is_heif_brand(brand) {
            return load_heif_hdr_aware_from_mmap(
                path,
                mmap.as_ref(),
                hdr_target_capacity,
                hdr_tone_map,
                crate::hdr::heif::HeifHdrDecodeDiag {
                    idx: None,
                    path: Some(path),
                },
                false,
            );
        }
        return load_bmff_ftyp_container(path, hdr_target_capacity, hdr_tone_map, brand);
    }

    Err(rust_i18n::t!("error.detection_failed").to_string())
}

#[cfg(test)]
mod tests {
    use super::load_via_content_detection;

    fn is_jpeg_magic(header: &[u8]) -> bool {
        header.len() >= 3 && header[0] == 0xFF && header[1] == 0xD8 && header[2] == 0xFF
    }

    fn is_png_magic(header: &[u8]) -> bool {
        header.len() >= 8 && header.starts_with(b"\x89PNG\r\n\x1a\n")
    }

    fn is_webp_magic(header: &[u8]) -> bool {
        header.len() >= 12 && &header[0..4] == b"RIFF" && &header[8..12] == b"WEBP"
    }

    #[test]
    fn magic_helpers_detect_common_mislabeled_heif_containers() {
        assert!(is_jpeg_magic(&[0xFF, 0xD8, 0xFF, 0xE0]));
        assert!(is_png_magic(b"\x89PNG\r\n\x1a\n"));
        assert!(is_webp_magic(b"RIFFxxxxWEBP"));
        assert!(!is_jpeg_magic(&[0x00, 0x00, 0x00, 0x18]));
    }

    fn libheif_container_rejected(err: &str) -> bool {
        let lower = err.to_ascii_lowercase();
        lower.contains("ftyp") || lower.contains("does not start with")
    }

    #[test]
    fn primary_decode_failure_is_final_uses_stable_tag() {
        let err = super::motion_video_bmff_error(b"qt  ");
        assert!(super::primary_decode_failure_is_final(&err));
        assert!(!super::primary_decode_failure_is_final(
            "ISO BMFF container is not a decodable still image"
        ));
    }

    #[test]
    fn motion_video_bmff_brand_includes_quicktime() {
        assert!(super::is_motion_video_bmff_brand(b"qt  "));
        assert!(!super::is_motion_video_bmff_brand(b"heic"));
    }

    #[test]
    fn libheif_container_rejected_matches_ftyp_errors() {
        assert!(libheif_container_rejected(
            "Failed to read HEIF from memory: Invalid input: No 'ftyp' box"
        ));
        assert!(!libheif_container_rejected("decoder plugin not found"));
    }

    #[test]
    fn optional_mislabeled_png_jpeg_recovers_via_content_detection() {
        let candidates = [
            std::env::var_os("SIV_MISLABELED_PNG_JPEG").map(std::path::PathBuf::from),
            Some(std::path::PathBuf::from(r"F:\win7\64MP_Raw\20250615.png")),
        ];
        let Some(path) = candidates.into_iter().flatten().find(|p| p.is_file()) else {
            eprintln!("skip; set SIV_MISLABELED_PNG_JPEG to a JPEG file with a .png extension");
            return;
        };
        let tone = crate::hdr::types::HdrToneMapSettings::default();
        let capacity = tone.target_hdr_capacity();
        let result = load_via_content_detection(&path, None, capacity, tone);
        match result {
            Ok(image) => {
                let (w, h) = match image {
                    crate::loader::ImageData::Static(ref img) => (img.width, img.height),
                    crate::loader::ImageData::Tiled(ref src) => (src.width(), src.height()),
                    other => panic!(
                        "mislabeled PNG/JPEG should decode as static or tiled, got {:?}",
                        std::mem::discriminant(&other)
                    ),
                };
                assert!(
                    w > 0 && h > 0,
                    "{} should decode to non-zero dimensions",
                    path.display()
                );
            }
            Err(err) => panic!("mislabeled PNG/JPEG should recover via content detection: {err}"),
        }
    }

    #[test]
    fn optional_mislabeled_quicktime_jpg_rejects_without_wic_decode() {
        let Some(path) = std::env::var_os("SIV_QT_JPG_SAMPLE").map(std::path::PathBuf::from) else {
            eprintln!("skip; set SIV_QT_JPG_SAMPLE");
            return;
        };
        let result = load_via_content_detection(
            &path,
            None,
            crate::hdr::types::HdrToneMapSettings::default().target_hdr_capacity(),
            crate::hdr::types::HdrToneMapSettings::default(),
        );
        match result {
            Ok(image) => {
                let (w, h) = match image {
                    crate::loader::ImageData::Static(ref img) => (img.width, img.height),
                    crate::loader::ImageData::Tiled(ref src) => (src.width(), src.height()),
                    other => panic!(
                        "unexpected image variant for QT sample: {:?}",
                        std::mem::discriminant(&other)
                    ),
                };
                assert!(
                    w <= crate::constants::ABSOLUTE_MAX_TEXTURE_SIDE
                        && h <= crate::constants::ABSOLUTE_MAX_TEXTURE_SIDE,
                    "QT mislabeled JPG decoded to {w}x{h}"
                );
            }
            Err(err) => {
                assert!(
                    err.contains(super::MOTION_VIDEO_BMFF_ERROR_TAG)
                        || err.contains("ISO BMFF")
                        || err.contains("Live Photo"),
                    "unexpected error for QT sample: {err}"
                );
            }
        }
    }
}
