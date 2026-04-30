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

use std::fs::File;
use std::io::{BufRead, Cursor};
use std::path::Path;
use std::sync::Arc;

use image::{ImageReader, Limits};

use super::types::{HdrColorSpace, HdrImageBuffer, HdrPixelFormat};

const HDR_RGBA32F_BYTES_PER_PIXEL: u64 = 4 * std::mem::size_of::<f32>() as u64;
const SDR_RGBA8_BYTES_PER_PIXEL: u64 = 4;
const HDR_FALLBACK_BYTES_PER_PIXEL_WITH_SDR: u64 =
    HDR_RGBA32F_BYTES_PER_PIXEL + SDR_RGBA8_BYTES_PER_PIXEL;
const MAX_HDR_FALLBACK_PIXELS: u64 = 8192 * 8192;
const MAX_HDR_FALLBACK_DECODE_BYTES: u64 = MAX_HDR_FALLBACK_PIXELS * HDR_RGBA32F_BYTES_PER_PIXEL;
const MAX_HDR_FALLBACK_TOTAL_BYTES: u64 =
    MAX_HDR_FALLBACK_PIXELS * HDR_FALLBACK_BYTES_PER_PIXEL_WITH_SDR;
const MAX_HDR_TONE_MAP_INPUT: f32 = f32::MAX;

pub fn is_hdr_candidate_ext(ext: &str) -> bool {
    ext.eq_ignore_ascii_case("exr") || ext.eq_ignore_ascii_case("hdr")
}

pub fn decode_hdr_image(path: &Path) -> Result<HdrImageBuffer, String> {
    if is_exr_path(path) {
        return decode_exr_display_image(path);
    }
    let radiance_params = RadianceHeaderParams::read_from_path(path)?;

    let (width, height) = ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .into_dimensions()
        .map_err(|e| e.to_string())?;
    validate_hdr_fallback_budget(width, height)?;

    let mut decoder = ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut limits = Limits::default();
    limits.max_alloc = Some(MAX_HDR_FALLBACK_DECODE_BYTES);
    decoder.limits(limits);

    let mut rgba = decoder.decode().map_err(|e| e.to_string())?.into_rgba32f();
    radiance_params.apply_to_pixels(rgba.as_flat_samples_mut().samples);

    Ok(HdrImageBuffer {
        width,
        height,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::LinearSrgb,
        rgba_f32: Arc::new(rgba.into_raw()),
    })
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct RadianceHeaderParams {
    exposure: f32,
    colorcorr: [f32; 3],
}

impl RadianceHeaderParams {
    pub(crate) fn read_from_path(path: &Path) -> Result<Self, String> {
        let file = File::open(path).map_err(|err| err.to_string())?;
        let mmap = unsafe { memmap2::Mmap::map(&file).map_err(|err| err.to_string())? };
        Self::read_from_bytes(&mmap)
    }

    pub(crate) fn read_from_bytes(bytes: &[u8]) -> Result<Self, String> {
        let mut reader = Cursor::new(bytes);
        let mut params = Self::default();
        let mut line = String::new();

        loop {
            line.clear();
            let bytes_read = reader.read_line(&mut line).map_err(|err| err.to_string())?;
            if bytes_read == 0 {
                break;
            }

            let trimmed = line.trim();
            if trimmed.is_empty() {
                break;
            }
            params.apply_header_line(trimmed);
        }

        Ok(params)
    }

    pub(crate) fn apply_header_line(&mut self, line: &str) {
        if let Some(value) = line.strip_prefix("EXPOSURE=") {
            if let Ok(exposure) = value.trim().parse::<f32>() {
                if exposure.is_finite() && exposure > 0.0 {
                    self.exposure *= exposure;
                }
            }
        } else if let Some(value) = line.strip_prefix("COLORCORR=") {
            let mut parts = value.split_whitespace();
            let (Some(r), Some(g), Some(b), None) =
                (parts.next(), parts.next(), parts.next(), parts.next())
            else {
                return;
            };
            let Ok(r) = r.parse::<f32>() else { return };
            let Ok(g) = g.parse::<f32>() else { return };
            let Ok(b) = b.parse::<f32>() else { return };
            if r.is_finite() && r > 0.0 && g.is_finite() && g > 0.0 && b.is_finite() && b > 0.0 {
                self.colorcorr[0] *= r;
                self.colorcorr[1] *= g;
                self.colorcorr[2] *= b;
            }
        }
    }

    pub(crate) fn apply_to_pixels(self, pixels: &mut [f32]) {
        let scale = [
            1.0 / (self.exposure * self.colorcorr[0]),
            1.0 / (self.exposure * self.colorcorr[1]),
            1.0 / (self.exposure * self.colorcorr[2]),
        ];
        if scale
            .iter()
            .all(|value| (*value - 1.0).abs() <= f32::EPSILON)
        {
            return;
        }

        for pixel in pixels.chunks_exact_mut(4) {
            pixel[0] *= scale[0];
            pixel[1] *= scale[1];
            pixel[2] *= scale[2];
        }
    }
}

impl Default for RadianceHeaderParams {
    fn default() -> Self {
        Self {
            exposure: 1.0,
            colorcorr: [1.0; 3],
        }
    }
}

pub(crate) fn decode_exr_display_image(path: &Path) -> Result<HdrImageBuffer, String> {
    let (width, height) = crate::hdr::exr_tiled::exr_dimensions_unvalidated(path)?;
    validate_hdr_fallback_budget(width, height)?;

    let pixels = exr::prelude::read_first_rgba_layer_from_file(
        path,
        move |resolution, _channels| vec![0.0_f32; resolution.width() * resolution.height() * 4],
        move |pixels, position, (r, g, b, a): (f32, f32, f32, f32)| {
            let index = (position.y() * width as usize + position.x()) * 4;
            pixels[index] = r;
            pixels[index + 1] = g;
            pixels[index + 2] = b;
            pixels[index + 3] = a;
        },
    )
    .map_err(|err| err.to_string())?
    .layer_data
    .channel_data
    .pixels;

    Ok(HdrImageBuffer {
        width,
        height,
        format: HdrPixelFormat::Rgba32Float,
        color_space: crate::hdr::exr_tiled::exr_color_space(path)?,
        rgba_f32: Arc::new(pixels),
    })
}

fn is_exr_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("exr"))
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
            let exposed = clamp_hdr_tone_map_input(sanitize_hdr_rgb(*channel) * exposure_scale);
            let mapped = exposed / (1.0 + exposed);
            let encoded = mapped.powf(1.0 / 2.2).clamp(0.0, 1.0);
            pixels.push(float_to_u8(encoded));
        }

        pixels.push(float_to_u8(pixel[3].clamp(0.0, 1.0)));
    }

    Ok(pixels)
}

fn validate_hdr_fallback_budget(width: u32, height: u32) -> Result<(), String> {
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(|| format!("HDR image dimensions overflow: {width}x{height}"))?;
    let total_bytes = pixels
        .checked_mul(HDR_FALLBACK_BYTES_PER_PIXEL_WITH_SDR)
        .ok_or_else(|| format!("HDR fallback byte size overflow: {width}x{height}"))?;

    if pixels > MAX_HDR_FALLBACK_PIXELS || total_bytes > MAX_HDR_FALLBACK_TOTAL_BYTES {
        return Err(format!(
            "HDR image {width}x{height} requires {total_bytes} bytes for full-float fallback, \
             exceeds HDR fallback limit of {MAX_HDR_FALLBACK_PIXELS} pixels / \
             {MAX_HDR_FALLBACK_TOTAL_BYTES} bytes"
        ));
    }

    Ok(())
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

fn clamp_hdr_tone_map_input(value: f32) -> f32 {
    if value.is_nan() || value <= 0.0 {
        0.0
    } else if value.is_infinite() {
        MAX_HDR_TONE_MAP_INPUT
    } else {
        value.min(MAX_HDR_TONE_MAP_INPUT)
    }
}

fn float_to_u8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hdr::types::{HdrColorSpace, HdrImageBuffer, HdrPixelFormat};
    use std::path::PathBuf;
    use std::sync::Arc;

    fn openexr_images_root() -> Option<PathBuf> {
        std::env::var_os("SIV_OPENEXR_IMAGES_DIR")
            .map(PathBuf::from)
            .or_else(|| Some(PathBuf::from(r"F:\HDR\openexr-images")))
            .filter(|path| path.is_dir())
    }

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
    fn tone_map_extreme_finite_rgb_with_high_exposure_saturates() {
        let buffer = HdrImageBuffer {
            width: 1,
            height: 1,
            format: HdrPixelFormat::Rgba32Float,
            color_space: HdrColorSpace::LinearSrgb,
            rgba_f32: Arc::new(vec![f32::MAX, f32::MAX, f32::MAX, 1.0]),
        };

        let sdr = hdr_to_sdr_rgba8(&buffer, 16.0).expect("tone map extreme finite buffer");

        assert_eq!(sdr, vec![255, 255, 255, 255]);
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
    fn decode_hdr_image_applies_radiance_exposure_and_colorcorr() {
        let path = std::env::temp_dir().join(format!(
            "simple_image_viewer_hdr_decode_params_{}.hdr",
            std::process::id()
        ));
        let bytes = b"#?RADIANCE\nFORMAT=32-bit_rle_rgbe\nEXPOSURE=2\nCOLORCORR=2 4 8\n\n-Y 1 +X 1\n\x80\x80\x80\x81";
        std::fs::write(&path, bytes).expect("write test HDR");

        let buffer = decode_hdr_image(&path).expect("decode test HDR with header params");
        let _ = std::fs::remove_file(&path);

        assert!((buffer.rgba_f32[0] - 0.25).abs() < 0.01);
        assert!((buffer.rgba_f32[1] - 0.125).abs() < 0.01);
        assert!((buffer.rgba_f32[2] - 0.0625).abs() < 0.01);
        assert_eq!(buffer.rgba_f32[3], 1.0);
    }

    #[test]
    fn decode_hdr_image_rejects_oversized_hdr_header_before_pixel_decode() {
        let path = std::env::temp_dir().join(format!(
            "simple_image_viewer_hdr_decode_huge_{}.hdr",
            std::process::id()
        ));
        let width = (MAX_HDR_FALLBACK_PIXELS + 1) as u32;
        let bytes = format!("#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n-Y 1 +X {width}\n");
        std::fs::write(&path, bytes).expect("write oversized test HDR");

        let err = decode_hdr_image(&path).expect_err("reject oversized HDR fallback");
        let _ = std::fs::remove_file(&path);

        assert!(err.contains("exceeds HDR fallback limit"));
        assert!(err.contains(&width.to_string()));
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

    #[test]
    fn decode_exr_display_image_reads_multipart_color_layer() {
        let Some(root) = openexr_images_root() else {
            eprintln!(
                "skipping OpenEXR multipart decode test; set SIV_OPENEXR_IMAGES_DIR to openexr-images"
            );
            return;
        };
        let path = root.join("v2/Stereo/composited.exr");
        if !path.is_file() {
            eprintln!("skipping OpenEXR multipart decode test; stereo composited sample missing");
            return;
        }

        let buffer = decode_exr_display_image(&path).expect("decode multipart EXR display layer");

        assert_eq!((buffer.width, buffer.height), (1918, 1078));
        assert!(
            buffer
                .rgba_f32
                .chunks_exact(4)
                .any(|pixel| pixel[0] > 0.0 || pixel[1] > 0.0 || pixel[2] > 0.0),
            "multipart display layer should contain visible RGB content"
        );
    }
}
