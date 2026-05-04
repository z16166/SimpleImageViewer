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

use image::ImageDecoder;
use image::{ImageReader, Limits};

use crate::hdr::tiled::HdrTiledSource;

use super::types::{
    HdrColorProfile, HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat, HdrToneMapSettings,
    HdrTransferFunction,
};

const HDR_RGBA32F_BYTES_PER_PIXEL: u64 = 4 * std::mem::size_of::<f32>() as u64;
const SDR_RGBA8_BYTES_PER_PIXEL: u64 = 4;
const HDR_FALLBACK_BYTES_PER_PIXEL_WITH_SDR: u64 =
    HDR_RGBA32F_BYTES_PER_PIXEL + SDR_RGBA8_BYTES_PER_PIXEL;
const MAX_HDR_FALLBACK_PIXELS: u64 = 8192 * 8192;
const MAX_HDR_FALLBACK_DECODE_BYTES: u64 = MAX_HDR_FALLBACK_PIXELS * HDR_RGBA32F_BYTES_PER_PIXEL;
const MAX_HDR_FALLBACK_TOTAL_BYTES: u64 =
    MAX_HDR_FALLBACK_PIXELS * HDR_FALLBACK_BYTES_PER_PIXEL_WITH_SDR;
const MAX_HDR_TONE_MAP_INPUT: f32 = f32::MAX;
const INVERSE_DISPLAY_GAMMA: f32 = 1.0 / 2.2;

pub fn is_hdr_candidate_ext(ext: &str) -> bool {
    ext.eq_ignore_ascii_case("exr") || ext.eq_ignore_ascii_case("hdr")
}

pub fn decode_hdr_image(path: &Path) -> Result<HdrImageBuffer, String> {
    if is_exr_path(path) {
        return decode_exr_display_image(path);
    }
    if is_radiance_hdr_path(path) {
        return decode_radiance_hdr_image(path);
    }

    let mmap = crate::mmap_util::map_file(path)?;
    let (width, height) = ImageReader::new(std::io::Cursor::new(&mmap[..]))
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .into_dimensions()
        .map_err(|e| e.to_string())?;
    validate_hdr_fallback_budget(width, height)?;

    let mut decoder = ImageReader::new(std::io::Cursor::new(&mmap[..]))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut limits = Limits::default();
    limits.max_alloc = Some(MAX_HDR_FALLBACK_DECODE_BYTES);
    decoder.limits(limits);

    let rgba = decoder.decode().map_err(|e| e.to_string())?.into_rgba32f();

    Ok(HdrImageBuffer {
        width,
        height,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::LinearSrgb,
        metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
        rgba_f32: Arc::new(rgba.into_raw()),
    })
}

fn decode_radiance_hdr_image(path: &Path) -> Result<HdrImageBuffer, String> {
    let file = File::open(path).map_err(|err| err.to_string())?;
    let mmap = unsafe { memmap2::Mmap::map(&file).map_err(|err| err.to_string())? };
    let radiance_params = RadianceHeaderParams::read_from_bytes(&mmap)?;
    log::debug!(
        "[HDR] {}: {}",
        path.display(),
        radiance_params.diagnostic_label()
    );
    let decoder = image::codecs::hdr::HdrDecoder::new(Cursor::new(&mmap[..]))
        .map_err(|err| err.to_string())?;
    let (width, height) = decoder.dimensions();
    validate_hdr_fallback_budget(width, height)?;

    let mut rgb_bytes = vec![0_u8; decoder.total_bytes() as usize];
    decoder
        .read_image(&mut rgb_bytes)
        .map_err(|err| err.to_string())?;

    let rgb_f32: &[f32] = bytemuck::cast_slice(&rgb_bytes);
    let mut rgba_f32 = Vec::with_capacity(width as usize * height as usize * 4);
    for rgb in rgb_f32.chunks_exact(3) {
        rgba_f32.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 1.0]);
    }
    radiance_params.apply_to_pixels(&mut rgba_f32);

    Ok(HdrImageBuffer {
        width,
        height,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::LinearSrgb,
        metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
        rgba_f32: Arc::new(rgba_f32),
    })
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct RadianceHeaderParams {
    exposure: f32,
    colorcorr: [f32; 3],
}

impl RadianceHeaderParams {
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

    pub(crate) fn diagnostic_label(self) -> String {
        format!(
            "Radiance EXPOSURE={:.3} COLORCORR=[{:.3},{:.3},{:.3}]",
            self.exposure, self.colorcorr[0], self.colorcorr[1], self.colorcorr[2]
        )
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
    let source = crate::hdr::exr_tiled::ExrTiledImageSource::open(path)?;
    let (width, height) = (source.width(), source.height());
    validate_hdr_fallback_budget(width, height)?;
    let tile = source.extract_tile_rgba32f_arc(0, 0, width, height)?;

    Ok(HdrImageBuffer {
        width,
        height,
        format: HdrPixelFormat::Rgba32Float,
        color_space: tile.color_space,
        metadata: tile.metadata.clone(),
        rgba_f32: Arc::clone(&tile.rgba_f32),
    })
}

fn is_exr_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("exr"))
}

fn is_radiance_hdr_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("hdr"))
}

/// Tone-map HDR float RGBA to 8-bit sRGB for SDR displays. Uses [`HdrImageMetadata`]
/// transfer function (PQ / HLG / sRGB / linear) and color space similarly to the
/// HDR image-plane WGSL path, so PQ/HLG wide-gamut sources are not misinterpreted
/// as scene-linear sRGB (which previously washed out detail). Scene-linear EXR /
/// Radiance-style buffers (`HdrTransferFunction::Linear`) keep the prior Reinhard
/// curve without an extra peak-luminance scaler so existing still-HDR behavior stays
/// predictable.
pub fn hdr_to_sdr_rgba8(buffer: &HdrImageBuffer, exposure_ev: f32) -> Result<Vec<u8>, String> {
    let mut tone = HdrToneMapSettings::default();
    if let Some(max) = buffer.metadata.luminance.mastering_max_nits {
        if max.is_finite() && max > tone.sdr_white_nits {
            tone.max_display_nits = max;
        }
    }
    hdr_to_sdr_rgba8_with_tone_settings(buffer, exposure_ev, &tone)
}

/// Same as [`hdr_to_sdr_rgba8`] but uses explicit SDR white / peak display nits
/// (e.g. from user tone-map settings) for PQ/HLG peak scaling. Caller-supplied
/// `max_display_nits` is raised by [`HdrImageMetadata::luminance::mastering_max_nits`]
/// when that hint exceeds it (content peak vs display capability).
pub fn hdr_to_sdr_rgba8_with_tone_settings(
    buffer: &HdrImageBuffer,
    exposure_ev: f32,
    tone: &HdrToneMapSettings,
) -> Result<Vec<u8>, String> {
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

    let mut tone = *tone;
    if let Some(max) = buffer.metadata.luminance.mastering_max_nits {
        if max.is_finite() && max > tone.sdr_white_nits {
            tone.max_display_nits = tone.max_display_nits.max(max);
        }
    }

    let tf = buffer.metadata.transfer_function;
    let apply_peak_scaler = matches!(
        tf,
        HdrTransferFunction::Pq | HdrTransferFunction::Hlg
    );
    let exposure_scale = 2.0_f32.powf(exposure_ev);
    let peak_scale = if apply_peak_scaler {
        tone.sdr_white_nits / tone.max_display_nits.max(tone.sdr_white_nits)
    } else {
        1.0
    };

    let mut pixels = Vec::with_capacity(expected_len);
    for pixel in buffer.rgba_f32.chunks_exact(4) {
        let rgb_in = [pixel[0], pixel[1], pixel[2]];
        let decoded = decode_transfer_to_display_linear(rgb_in, tf, tone.sdr_white_nits);
        let linear_srgb = linear_primary_to_linear_srgb(decoded, buffer.color_space, &buffer.metadata);
        let encoded = encode_sdr_rgb8(linear_srgb, exposure_scale, peak_scale);
        pixels.extend_from_slice(&[encoded[0], encoded[1], encoded[2], float_to_u8(pixel[3].clamp(0.0, 1.0))]);
    }
    Ok(pixels)
}

/// Decode full-range RGB **code values** (0–1) per CICP transfer to **display-linear**
/// channels in the same primary space as the codes (matches libavif `gammaToLinear` input).
pub(crate) fn decode_transfer_to_display_linear(
    rgb: [f32; 3],
    tf: HdrTransferFunction,
    sdr_white_nits: f32,
) -> [f32; 3] {
    let clamp01 = |v: f32| v.clamp(0.0, 1.0);
    match tf {
        HdrTransferFunction::Linear => rgb,
        HdrTransferFunction::Srgb => [
            srgb_nonlinear_channel_to_linear(rgb[0]),
            srgb_nonlinear_channel_to_linear(rgb[1]),
            srgb_nonlinear_channel_to_linear(rgb[2]),
        ],
        HdrTransferFunction::Pq => [
            pq_nonlinear_to_display_linear(clamp01(rgb[0]), sdr_white_nits),
            pq_nonlinear_to_display_linear(clamp01(rgb[1]), sdr_white_nits),
            pq_nonlinear_to_display_linear(clamp01(rgb[2]), sdr_white_nits),
        ],
        HdrTransferFunction::Hlg => [
            hlg_nonlinear_to_scene_linear(clamp01(rgb[0])),
            hlg_nonlinear_to_scene_linear(clamp01(rgb[1])),
            hlg_nonlinear_to_scene_linear(clamp01(rgb[2])),
        ],
        HdrTransferFunction::Gamma | HdrTransferFunction::Unknown => rgb,
    }
}

/// Linear sRGB / extended linear where 1.0 is SDR white → nonlinear sRGB 8-bit (ISO gain-map SDR base).
pub(crate) fn linear_srgb_linear_to_srgb_u8(linear: f32) -> u8 {
    let linear = linear.clamp(0.0, 1.0);
    let encoded = if linear <= 0.0031308 {
        linear * 12.92
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    };
    (encoded * 255.0).round().clamp(0.0, 255.0) as u8
}

fn srgb_nonlinear_channel_to_linear(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Reference **PQ EOTF** (non-linear code → absolute luminance, then ÷ `sdr_white_nits` for display-relative linear).
///
/// Normative: **ITU-R BT.2100-3** Table 4 (PQ system reference EOTF); same rational coefficients as
/// **SMPTE ST 2084** and the HDR plane WGSL in `renderer.rs`.
pub(crate) fn pq_nonlinear_to_display_linear(code: f32, sdr_white_nits: f32) -> f32 {
    let m1 = 2610.0 / 16384.0;
    let m2 = 2523.0 / 32.0;
    let c1 = 3424.0 / 4096.0;
    let c2 = 2413.0 / 128.0;
    let c3 = 2392.0 / 128.0;
    let code_m2 = code.powf(1.0 / m2);
    let numerator = (code_m2 - c1).max(0.0);
    let denominator = (c2 - c3 * code_m2).max(0.000001);
    let absolute_nits = 10000.0 * (numerator / denominator).powf(1.0 / m1);
    absolute_nits / sdr_white_nits.max(1.0)
}

/// BT.2100 HLG OETF inverse (scene linear), matching `hlg_to_scene_linear` in `renderer.rs`.
fn hlg_nonlinear_to_scene_linear(e_prime: f32) -> f32 {
    let a = 0.17883277_f32;
    let b = 0.28466892_f32;
    let c = 0.55991073_f32;
    if e_prime <= 0.5 {
        (e_prime * e_prime) / 3.0
    } else {
        (((e_prime - c).max(0.0) / a).exp() + b) / 12.0
    }
}

pub(crate) fn linear_primary_to_linear_srgb(rgb: [f32; 3], color_space: HdrColorSpace, meta: &HdrImageMetadata) -> [f32; 3] {
    match color_space {
        HdrColorSpace::LinearSrgb | HdrColorSpace::LinearScRgb => rgb,
        HdrColorSpace::Rec2020Linear => rec2020_linear_to_linear_srgb(rgb),
        HdrColorSpace::DisplayP3Linear => display_p3_linear_to_linear_srgb(rgb),
        HdrColorSpace::Aces2065_1 => aces2065_1_linear_to_linear_srgb(rgb),
        HdrColorSpace::Xyz => xyz_to_linear_srgb(rgb),
        HdrColorSpace::Unknown => {
            if matches!(
                meta.color_profile,
                HdrColorProfile::Cicp {
                    color_primaries: 9,
                    ..
                }
            ) {
                rec2020_linear_to_linear_srgb(rgb)
            } else if matches!(
                meta.color_profile,
                HdrColorProfile::Cicp {
                    color_primaries: 11,
                    ..
                }
            ) {
                display_p3_linear_to_linear_srgb(rgb)
            } else {
                rgb
            }
        }
    }
}

/// Display P3 (D65) linear RGB → linear sRGB (same white point; matrix from Skia/CSS pipelines).
fn display_p3_linear_to_linear_srgb(rgb: [f32; 3]) -> [f32; 3] {
    [
        1.2249401 * rgb[0] - 0.2249402 * rgb[1],
        -0.0420569 * rgb[0] + 1.0420571 * rgb[1],
        -0.0196376 * rgb[0] - 0.0786507 * rgb[1] + 1.0982884 * rgb[2],
    ]
}

fn rec2020_linear_to_linear_srgb(rgb: [f32; 3]) -> [f32; 3] {
    [
        1.6605 * rgb[0] - 0.5876 * rgb[1] - 0.0728 * rgb[2],
        -0.1246 * rgb[0] + 1.1329 * rgb[1] - 0.0083 * rgb[2],
        -0.0182 * rgb[0] - 0.1006 * rgb[1] + 1.1187 * rgb[2],
    ]
}

fn aces2065_1_linear_to_linear_srgb(rgb: [f32; 3]) -> [f32; 3] {
    [
        2.5216 * rgb[0] - 1.1369 * rgb[1] - 0.3849 * rgb[2],
        -0.2762 * rgb[0] + 1.3697 * rgb[1] - 0.0935 * rgb[2],
        -0.0159 * rgb[0] - 0.1478 * rgb[1] + 1.1638 * rgb[2],
    ]
}

fn xyz_to_linear_srgb(xyz: [f32; 3]) -> [f32; 3] {
    [
        3.2404 * xyz[0] - 1.5371 * xyz[1] - 0.4985 * xyz[2],
        -0.9692 * xyz[0] + 1.8760 * xyz[1] + 0.0415 * xyz[2],
        0.0556 * xyz[0] - 0.2040 * xyz[1] + 1.0572 * xyz[2],
    ]
}

fn encode_sdr_rgb8(linear_srgb: [f32; 3], exposure_scale: f32, peak_scale: f32) -> [u8; 3] {
    let mut out = [0_u8; 3];
    for i in 0..3 {
        let exposed = clamp_hdr_tone_map_input(sanitize_hdr_rgb(linear_srgb[i]) * exposure_scale * peak_scale);
        let mapped = exposed / (1.0 + exposed);
        let encoded = mapped.powf(INVERSE_DISPLAY_GAMMA).clamp(0.0, 1.0);
        out[i] = float_to_u8(encoded);
    }
    out
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
    use crate::hdr::types::{
        HdrColorProfile, HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat, HdrToneMapSettings,
        HdrTransferFunction,
    };
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
            metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
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
            metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
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
            metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
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
            metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
            rgba_f32: Arc::new(vec![f32::MAX, f32::MAX, f32::MAX, 1.0]),
        };

        let sdr = hdr_to_sdr_rgba8(&buffer, 16.0).expect("tone map extreme finite buffer");

        assert_eq!(sdr, vec![255, 255, 255, 255]);
    }

    #[test]
    fn pq_transfer_eotf_and_rec2020_matrix_produce_reasonable_sdr_fallback() {
        let mut meta = HdrImageMetadata::default();
        meta.transfer_function = HdrTransferFunction::Pq;
        meta.color_profile = HdrColorProfile::Cicp {
            color_primaries: 9,
            transfer_characteristics: 16,
            matrix_coefficients: 0,
            full_range: true,
        };
        let buffer = HdrImageBuffer {
            width: 1,
            height: 1,
            format: HdrPixelFormat::Rgba32Float,
            color_space: HdrColorSpace::Rec2020Linear,
            metadata: meta,
            rgba_f32: Arc::new(vec![0.45, 0.45, 0.45, 1.0]),
        };
        let sdr = hdr_to_sdr_rgba8(&buffer, 0.0).expect("pq tone map");

        assert!(
            sdr[0] > 8 && sdr[0] < 250 && sdr[1] > 8 && sdr[1] < 250 && sdr[2] > 8 && sdr[2] < 250,
            "unexpected PQ SDR fallback RGB {:?}",
            &sdr[..3]
        );
        assert_eq!(sdr[3], 255);
    }

    #[test]
    fn hdr_to_sdr_rgba8_with_tone_settings_respects_max_display_nits() {
        let mut meta = HdrImageMetadata::default();
        meta.transfer_function = HdrTransferFunction::Pq;
        meta.color_profile = HdrColorProfile::Cicp {
            color_primaries: 9,
            transfer_characteristics: 16,
            matrix_coefficients: 0,
            full_range: true,
        };
        let buffer = HdrImageBuffer {
            width: 1,
            height: 1,
            format: HdrPixelFormat::Rgba32Float,
            color_space: HdrColorSpace::Rec2020Linear,
            metadata: meta,
            rgba_f32: Arc::new(vec![0.6, 0.6, 0.6, 1.0]),
        };
        // Smaller max_display_nits → larger peak_scale → brighter SDR after PQ decode + Reinhard.
        let narrow_peak = HdrToneMapSettings {
            max_display_nits: 600.0,
            ..HdrToneMapSettings::default()
        };
        let wide_peak = HdrToneMapSettings {
            max_display_nits: 4000.0,
            ..HdrToneMapSettings::default()
        };
        let brighter = hdr_to_sdr_rgba8_with_tone_settings(&buffer, 0.0, &narrow_peak).expect("tone map");
        let darker = hdr_to_sdr_rgba8_with_tone_settings(&buffer, 0.0, &wide_peak).expect("tone map");
        let sum_brighter: u32 = brighter[..3].iter().map(|&b| b as u32).sum();
        let sum_darker: u32 = darker[..3].iter().map(|&b| b as u32).sum();
        assert!(
            sum_brighter > sum_darker,
            "PQ SDR fallback should brighten when max_display_nits is lower: {sum_brighter} vs {sum_darker}"
        );
    }

    #[test]
    fn tone_map_rejects_malformed_buffer_length() {
        let buffer = HdrImageBuffer {
            width: 1,
            height: 1,
            format: HdrPixelFormat::Rgba32Float,
            color_space: HdrColorSpace::LinearSrgb,
            metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
            rgba_f32: Arc::new(vec![0.0, 0.0, 0.0]),
        };

        let err = hdr_to_sdr_rgba8(&buffer, 0.0).expect_err("reject malformed HDR buffer");

        assert!(err.contains("expected 4 floats"));
        assert!(err.contains("got 3"));
    }

    #[test]
    fn radiance_header_params_diagnostic_reports_exposure_and_colorcorr() {
        let params = RadianceHeaderParams::read_from_bytes(
            b"#?RADIANCE\nFORMAT=32-bit_rle_rgbe\nEXPOSURE=2\nCOLORCORR=2 4 8\n\n-Y 1 +X 1\n",
        )
        .expect("parse Radiance header params");

        assert_eq!(
            params.diagnostic_label(),
            "Radiance EXPOSURE=2.000 COLORCORR=[2.000,4.000,8.000]"
        );
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
