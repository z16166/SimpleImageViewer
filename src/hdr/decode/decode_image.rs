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
use std::path::Path;
use std::sync::Arc;

use image::{DynamicImage, ImageDecoder, ImageReader, Limits};

use super::constants::MAX_HDR_FALLBACK_DECODE_BYTES;
use crate::hdr::types::{HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat};

pub fn is_hdr_candidate_ext(ext: &str) -> bool {
    ext.eq_ignore_ascii_case("exr")
        || ext.eq_ignore_ascii_case("hdr")
        || ext.eq_ignore_ascii_case("pic")
}

pub fn decode_hdr_image(path: &Path) -> Result<HdrImageBuffer, String> {
    if super::paths::is_exr_path(path) {
        return super::exr::decode_exr_display_image(path);
    }
    if super::paths::is_radiance_hdr_path(path) {
        return super::radiance::decode_radiance_hdr_image(path);
    }

    let mmap = crate::mmap_util::map_file(path)?;
    let reader = ImageReader::new(std::io::Cursor::new(mmap.as_ref()))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut decoder = reader.into_decoder().map_err(|e| e.to_string())?;
    let (width, height) = decoder.dimensions();
    super::tone_map::validate_hdr_fallback_budget(width, height)?;

    let mut limits = Limits::default();
    limits.max_alloc = Some(MAX_HDR_FALLBACK_DECODE_BYTES);
    decoder.set_limits(limits).map_err(|e| e.to_string())?;

    let image = DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    let rgba = image.into_rgba32f();

    Ok(HdrImageBuffer {
        width,
        height,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::LinearSrgb,
        metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
        rgba_f32: Arc::new(rgba.into_raw()),
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn hdr_fallback_validates_dimensions_before_decode() {
        let source = include_str!("decode_image.rs");
        let validate_pos = source
            .find("validate_hdr_fallback_budget(width, height)")
            .expect("decode_hdr_image should validate HDR fallback dimensions");
        let decode_pos = source
            .find("DynamicImage::from_decoder(decoder)")
            .expect("decode_hdr_image should decode after budget validation");
        let convert_pos = source
            .find("let rgba = image.into_rgba32f()")
            .expect("decode_hdr_image should convert after decode");

        assert!(validate_pos < decode_pos);
        assert!(decode_pos < convert_pos);
    }
}
