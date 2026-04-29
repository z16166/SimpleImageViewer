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

use image::ImageReader;

use super::types::{HdrColorSpace, HdrImageBuffer, HdrPixelFormat};

pub fn is_hdr_candidate_ext(ext: &str) -> bool {
    ext.eq_ignore_ascii_case("exr") || ext.eq_ignore_ascii_case("hdr")
}

pub fn decode_hdr_image(path: &Path) -> Result<HdrImageBuffer, String> {
    let reader = ImageReader::open(path).map_err(|e| e.to_string())?;
    let mut decoder = reader.with_guessed_format().map_err(|e| e.to_string())?;
    decoder.no_limits();

    let rgba = decoder.decode().map_err(|e| e.to_string())?.into_rgba32f();
    let (width, height) = rgba.dimensions();

    Ok(HdrImageBuffer {
        width,
        height,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::LinearSrgb,
        rgba_f32: Arc::new(rgba.into_raw()),
    })
}

pub fn hdr_to_sdr_rgba8(buffer: &HdrImageBuffer, exposure_ev: f32) -> Result<Vec<u8>, String> {
    let expected_len = buffer
        .width
        .checked_mul(buffer.height)
        .and_then(|pixels| pixels.checked_mul(4))
        .map(|len| len as usize)
        .ok_or_else(|| {
            format!(
                "HDR buffer dimensions overflow: {}x{}",
                buffer.width, buffer.height
            )
        })?;

    if buffer.rgba_f32.len() != expected_len {
        return Err(format!(
            "Malformed HDR buffer: expected {} floats for {}x{} RGBA, got {}",
            expected_len,
            buffer.width,
            buffer.height,
            buffer.rgba_f32.len()
        ));
    }

    let exposure_scale = 2.0_f32.powf(exposure_ev);
    let mut pixels = Vec::with_capacity(expected_len);

    for pixel in buffer.rgba_f32.chunks_exact(4) {
        for channel in &pixel[..3] {
            let exposed = sanitize_hdr_rgb(*channel) * exposure_scale;
            let mapped = exposed / (1.0 + exposed);
            let encoded = mapped.powf(1.0 / 2.2).clamp(0.0, 1.0);
            pixels.push(float_to_u8(encoded));
        }

        pixels.push(float_to_u8(pixel[3].clamp(0.0, 1.0)));
    }

    Ok(pixels)
}

fn sanitize_hdr_rgb(value: f32) -> f32 {
    if value.is_nan() || value <= 0.0 {
        0.0
    } else if value.is_infinite() {
        f32::MAX
    } else {
        value
    }
}

fn float_to_u8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hdr::types::{HdrColorSpace, HdrImageBuffer, HdrPixelFormat};
    use std::sync::Arc;

    #[test]
    fn hdr_candidate_extensions_are_case_insensitive() {
        assert!(is_hdr_candidate_ext("exr"));
        assert!(is_hdr_candidate_ext("EXR"));
        assert!(is_hdr_candidate_ext("hdr"));
        assert!(is_hdr_candidate_ext("HdR"));
        assert!(!is_hdr_candidate_ext("png"));
        assert!(!is_hdr_candidate_ext(""));
    }

    #[test]
    fn tone_map_preserves_alpha_and_maps_rgb_with_exposure() {
        let buffer = HdrImageBuffer {
            width: 2,
            height: 1,
            format: HdrPixelFormat::Rgba32Float,
            color_space: HdrColorSpace::LinearSrgb,
            rgba_f32: Arc::new(vec![-1.0, 0.0, 1.0, 0.5, 4.0, 0.25, 0.5, 1.5]),
        };

        let sdr = hdr_to_sdr_rgba8(&buffer, 0.0).expect("tone map valid buffer");

        assert_eq!(sdr, vec![0, 0, 186, 128, 230, 123, 155, 255,]);
    }

    #[test]
    fn tone_map_uses_exposure_ev_scale() {
        let buffer = HdrImageBuffer {
            width: 1,
            height: 1,
            format: HdrPixelFormat::Rgba32Float,
            color_space: HdrColorSpace::LinearSrgb,
            rgba_f32: Arc::new(vec![0.25, 0.25, 0.25, 1.0]),
        };

        let sdr = hdr_to_sdr_rgba8(&buffer, 1.0).expect("tone map valid buffer");

        assert_eq!(sdr, vec![155, 155, 155, 255]);
    }

    #[test]
    fn tone_map_sanitizes_non_finite_rgb_values() {
        let buffer = HdrImageBuffer {
            width: 2,
            height: 1,
            format: HdrPixelFormat::Rgba32Float,
            color_space: HdrColorSpace::LinearSrgb,
            rgba_f32: Arc::new(vec![
                f32::NAN,
                f32::NEG_INFINITY,
                f32::INFINITY,
                1.0,
                0.5,
                f32::NAN,
                f32::INFINITY,
                -1.0,
            ]),
        };

        let sdr = hdr_to_sdr_rgba8(&buffer, 0.0).expect("tone map non-finite buffer");

        assert_eq!(sdr, vec![0, 0, 255, 255, 155, 0, 255, 0]);
    }

    #[test]
    fn tone_map_rejects_malformed_buffer_length() {
        let buffer = HdrImageBuffer {
            width: 1,
            height: 1,
            format: HdrPixelFormat::Rgba32Float,
            color_space: HdrColorSpace::LinearSrgb,
            rgba_f32: Arc::new(vec![0.0, 0.0, 0.0]),
        };

        let err = hdr_to_sdr_rgba8(&buffer, 0.0).expect_err("reject malformed HDR buffer");

        assert!(err.contains("expected 4 floats"));
        assert!(err.contains("got 3"));
    }

    #[test]
    fn decode_hdr_image_reads_radiance_hdr_as_rgba32f() {
        let path = std::env::temp_dir().join(format!(
            "simple_image_viewer_hdr_decode_{}.hdr",
            std::process::id()
        ));
        let bytes = b"#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n-Y 1 +X 1\n\x80\x80\x80\x81";
        std::fs::write(&path, bytes).expect("write test HDR");

        let buffer = decode_hdr_image(&path).expect("decode test HDR");
        let _ = std::fs::remove_file(&path);

        assert_eq!(buffer.width, 1);
        assert_eq!(buffer.height, 1);
        assert_eq!(buffer.format, HdrPixelFormat::Rgba32Float);
        assert_eq!(buffer.color_space, HdrColorSpace::LinearSrgb);
        assert_eq!(buffer.rgba_f32.len(), 4);
        assert!((buffer.rgba_f32[0] - 1.0).abs() < 0.01);
        assert!((buffer.rgba_f32[1] - 1.0).abs() < 0.01);
        assert!((buffer.rgba_f32[2] - 1.0).abs() < 0.01);
        assert_eq!(buffer.rgba_f32[3], 1.0);
    }

    #[test]
    fn decode_hdr_image_reads_generated_exr_as_rgba32f() {
        let path = std::env::temp_dir().join(format!(
            "simple_image_viewer_hdr_decode_{}.exr",
            std::process::id()
        ));
        let img = image::ImageBuffer::<image::Rgba<f32>, Vec<f32>>::from_raw(
            1,
            1,
            vec![0.25, 0.5, 2.0, 1.0],
        )
        .expect("build test EXR image");
        image::DynamicImage::ImageRgba32F(img)
            .save_with_format(&path, image::ImageFormat::OpenExr)
            .expect("write test EXR");

        let buffer = decode_hdr_image(&path).expect("decode test EXR");
        let _ = std::fs::remove_file(&path);

        assert_eq!(buffer.width, 1);
        assert_eq!(buffer.height, 1);
        assert_eq!(buffer.format, HdrPixelFormat::Rgba32Float);
        assert_eq!(buffer.color_space, HdrColorSpace::LinearSrgb);
        assert_eq!(buffer.rgba_f32.len(), 4);
        assert!((buffer.rgba_f32[0] - 0.25).abs() < 0.01);
        assert!((buffer.rgba_f32[1] - 0.5).abs() < 0.01);
        assert!((buffer.rgba_f32[2] - 2.0).abs() < 0.01);
        assert_eq!(buffer.rgba_f32[3], 1.0);
    }
}
