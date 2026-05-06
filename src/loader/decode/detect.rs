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
use std::path::PathBuf;

use super::hdr_formats::{load_detected_exr, load_hdr};
use super::jpeg::load_jpeg_with_target_capacity;
use super::modern::{load_avif_with_target_capacity, load_heif_hdr_aware, load_jxl_with_target_capacity};
use super::raster::{load_gif, load_png, load_static, load_webp};

const DETECTION_BUFFER_SIZE: usize = 16;

pub(crate) fn load_by_image_format(
    format: image::ImageFormat,
    path: &PathBuf,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
) -> Result<ImageData, String> {
    match format {
        image::ImageFormat::Png => load_png(path, hdr_target_capacity, hdr_tone_map),
        image::ImageFormat::Gif => load_gif(path, hdr_target_capacity, hdr_tone_map),
        image::ImageFormat::WebP => load_webp(path, hdr_target_capacity, hdr_tone_map),
        image::ImageFormat::Tiff => crate::libtiff_loader::load_via_libtiff(
            path,
            hdr_target_capacity,
            hdr_tone_map,
        ),
        // Standard single-frame formats handled by load_static
        image::ImageFormat::Jpeg => {
            load_jpeg_with_target_capacity(path, hdr_target_capacity, hdr_tone_map)
        }
        image::ImageFormat::Bmp
        | image::ImageFormat::Ico
        | image::ImageFormat::Pnm
        | image::ImageFormat::Tga
        | image::ImageFormat::Dds
        | image::ImageFormat::Farbfeld
        | image::ImageFormat::Qoi => load_static(path, hdr_target_capacity, hdr_tone_map),
        // `image` is built without `avif` (ravif); libavif-only (`load_avif_with_target_capacity`).
        image::ImageFormat::Avif => load_avif_with_target_capacity(path, hdr_target_capacity, hdr_tone_map),
        image::ImageFormat::Hdr => load_hdr(path, hdr_target_capacity, hdr_tone_map),
        image::ImageFormat::OpenExr => load_detected_exr(path, hdr_target_capacity, hdr_tone_map),
        _ => Err(rust_i18n::t!(
            "error.unsupported_detected_format",
            format = format!("{:?}", format)
        )
        .to_string()),
    }
}

pub(crate) fn load_via_content_detection(
    path: &PathBuf,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
) -> Result<ImageData, String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;

    // Use constant for buffer size
    let mut header = [0u8; DETECTION_BUFFER_SIZE];
    let n = file.read(&mut header).unwrap_or(0);

    // 1. Try standard image-rs detection
    if let Ok(guessed) = image::guess_format(&header[..n]) {
        return load_by_image_format(guessed, path, hdr_target_capacity, hdr_tone_map);
    }

    if crate::hdr::jpegxl::is_jxl_header(&header[..n]) {
        return load_jxl_with_target_capacity(path, hdr_target_capacity, hdr_tone_map);
    }

    // 2. Manual HEIC detection (since image-rs 0.25 doesn't natively guess it)
    // HEIF/HEIC signature: "ftyp" (at offset 4) followed by various brands.
    if n >= 12 && &header[4..8] == b"ftyp" {
        let sub = &header[8..12];
        if crate::hdr::avif::is_avif_brand(sub) {
            return load_avif_with_target_capacity(path, hdr_target_capacity, hdr_tone_map);
        }
        if crate::hdr::heif::is_heif_brand(sub) {
            return load_heif_hdr_aware(path, hdr_target_capacity, hdr_tone_map);
        }
    }

    Err(rust_i18n::t!("error.detection_failed").to_string())
}
