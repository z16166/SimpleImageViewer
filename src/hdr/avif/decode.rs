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

#![cfg(feature = "avif-native")]

use std::ffi::CStr;

use crate::hdr::types::HdrImageBuffer;

pub(crate) fn avif_ftyp_major_brand(bytes: &[u8]) -> Option<[u8; 4]> {
    if bytes.len() < 12 {
        return None;
    }
    if &bytes[4..8] != b"ftyp" {
        return None;
    }
    Some([bytes[8], bytes[9], bytes[10], bytes[11]])
}

pub(crate) fn libavif_result_to_string(result: libavif_sys::avifResult) -> String {
    unsafe {
        let ptr = libavif_sys::avifResultToString(result);
        if ptr.is_null() {
            return format!("libavif error {result}");
        }
        CStr::from_ptr(ptr).to_string_lossy().into_owned()
    }
}

#[cfg(test)]
pub(crate) fn decode_avif_hdr_bytes(bytes: &[u8]) -> Result<HdrImageBuffer, String> {
    decode_avif_hdr_bytes_with_target_capacity(
        bytes,
        crate::hdr::types::HdrToneMapSettings::default().target_hdr_capacity(),
    )
}

#[cfg(feature = "avif-native")]
pub(crate) fn read_avif_decoder_image(bytes: &[u8]) -> Result<libavif_sys::AvifImageOwned, String> {
    let Some(decoder) = libavif_sys::AvifDecoderOwned::new() else {
        return Err("Failed to create libavif decoder".to_string());
    };
    unsafe {
        libavif_sys::siv_avif_decoder_set_strict_flags(
            decoder.as_ptr(),
            libavif_sys::AVIF_STRICT_DISABLED,
        );
        // Discover all advertised content (incl. gain map metadata) during parse.
        libavif_sys::siv_avif_decoder_set_image_content_flags(
            decoder.as_ptr(),
            libavif_sys::AVIF_IMAGE_CONTENT_ALL,
        );
    }

    let r = unsafe {
        libavif_sys::avifDecoderSetIOMemory(decoder.as_ptr(), bytes.as_ptr(), bytes.len())
    };
    if r != libavif_sys::AVIF_RESULT_OK {
        return Err(format!(
            "libavif SetIOMemory: {}",
            libavif_result_to_string(r)
        ));
    }

    let r = unsafe { libavif_sys::avifDecoderParse(decoder.as_ptr()) };
    if r != libavif_sys::AVIF_RESULT_OK {
        return Err(format!(
            "libavif parse failed: {}",
            libavif_result_to_string(r)
        ));
    }

    read_avif_image_from_parsed_decoder(decoder)
}

#[cfg(feature = "avif-native")]
pub(crate) fn read_avif_image_from_parsed_decoder(
    decoder: libavif_sys::AvifDecoderOwned,
) -> Result<libavif_sys::AvifImageOwned, String> {
    let meta_ptr = unsafe { libavif_sys::siv_avif_decoder_get_image(decoder.as_ptr()) };
    if meta_ptr.is_null() {
        return Err("libavif decoder image is null after parse".to_string());
    }
    let decode_flags = if unsafe { (*meta_ptr).gainMap.is_null() } {
        libavif_sys::AVIF_IMAGE_CONTENT_COLOR_AND_ALPHA
    } else {
        libavif_sys::AVIF_IMAGE_CONTENT_ALL
    };
    unsafe {
        libavif_sys::siv_avif_decoder_set_image_content_flags(decoder.as_ptr(), decode_flags);
    }

    let r = unsafe { libavif_sys::avifDecoderNextImage(decoder.as_ptr()) };
    if r != libavif_sys::AVIF_RESULT_OK {
        return Err(format!(
            "libavif decode failed: {}",
            libavif_result_to_string(r)
        ));
    }

    let decoded_ptr = unsafe { libavif_sys::siv_avif_decoder_get_image(decoder.as_ptr()) };
    if decoded_ptr.is_null() {
        return Err("libavif decoder image is null after decode".to_string());
    }

    let Some(owned) = libavif_sys::AvifImageOwned::create_empty() else {
        return Err("Failed to create libavif image".to_string());
    };
    let r = unsafe {
        libavif_sys::avifImageCopy(owned.as_ptr(), decoded_ptr, libavif_sys::AVIF_PLANES_ALL)
    };
    if r != libavif_sys::AVIF_RESULT_OK {
        return Err(format!(
            "libavif image copy failed: {}",
            libavif_result_to_string(r)
        ));
    }

    Ok(owned)
}

#[cfg(feature = "avif-native")]
#[allow(dead_code)] // Used by AVIF unit tests via `super::`.
pub(crate) fn decode_avif_hdr_bytes_with_target_capacity(
    bytes: &[u8],
    target_hdr_capacity: f32,
) -> Result<HdrImageBuffer, String> {
    let image = read_avif_decoder_image(bytes)?;
    super::avif_image_to_hdr_buffer(image.as_ptr(), target_hdr_capacity)
}

#[cfg(feature = "avif-native")]
pub(crate) fn decode_avif_static_with_optional_embedded_sdr(
    bytes: &[u8],
    path: &std::path::Path,
    decode_capacity: f32,
    try_embedded_sdr_master: bool,
) -> Result<crate::loader::ImageData, String> {
    use super::embedded_sdr::try_avif_embedded_sdr_from_decoded_image;
    use crate::loader::{
        DecodedImage, apply_exif_orientation_to_hdr_pair, hdr_sdr_fallback_rgba8_or_placeholder,
    };

    let image = read_avif_decoder_image(bytes)?;
    if try_embedded_sdr_master {
        match try_avif_embedded_sdr_from_decoded_image(&image, bytes, path) {
            Ok(image_data) => return Ok(image_data),
            Err(err)
                if crate::loader::embedded_sdr_fallback::avif_embedded_sdr_ineligible(&err) =>
            {
                crate::loader::embedded_sdr_fallback::log_embedded_sdr_master_fallback(
                    "AVIF", path, &err,
                );
            }
            Err(err) => return Err(err),
        }
    }

    let hdr = super::avif_image_to_hdr_buffer(image.as_ptr(), decode_capacity)?;
    let fallback = DecodedImage::from_hdr_sdr_fallback(
        hdr.width,
        hdr.height,
        hdr_sdr_fallback_rgba8_or_placeholder(&hdr)?,
    );
    let (hdr, fallback) = apply_exif_orientation_to_hdr_pair(path, hdr, fallback, Some(bytes));
    Ok(crate::loader::ImageData::Hdr {
        hdr: Box::new(hdr),
        fallback,
    })
}
pub(crate) fn avif_image_icc_bytes(image: &libavif_sys::avifImage) -> &[u8] {
    if image.icc.data.is_null() || image.icc.size == 0 {
        return &[];
    }
    unsafe { std::slice::from_raw_parts(image.icc.data, image.icc.size) }
}

/// Run `source ICC → sRGB` perceptual transform on interleaved RGBA f32 in-place.
/// Output: **sRGB-OETF-encoded floats in [0,1]**, alpha passed through unchanged. Returns `false`
/// (and leaves `rgba` untouched) on any failure so the caller can fall back to CICP-based rendering.
///
/// Reuses lcms2 statically linked through `libjxl_sys` (already used for JXL CMYK→sRGB). When the
/// build excludes `jpegxl`, the symbols aren't linked so we silently skip ICC handling.
#[cfg(all(feature = "avif-native", feature = "jpegxl"))]
pub(crate) fn apply_icc_to_srgb_via_lcms(rgba: &mut [f32], source_icc: &[u8]) -> bool {
    let pixel_count = rgba.len() / 4;
    if pixel_count == 0 || source_icc.is_empty() {
        return false;
    }
    if pixel_count > u32::MAX as usize {
        log::warn!("[AVIF] ICC transform skipped: {pixel_count} pixels exceeds lcms2 u32 limit");
        return false;
    }

    let mut output = vec![0.0_f32; rgba.len()];
    let Some(in_profile) = libjxl_sys::CmsProfile::open_from_mem(source_icc) else {
        log::warn!(
            "[AVIF] lcms2 could not parse embedded ICC ({} bytes); falling back to CICP",
            source_icc.len()
        );
        return false;
    };
    let Some(out_profile) = libjxl_sys::CmsProfile::new_srgb() else {
        log::warn!("[AVIF] lcms2 could not build sRGB profile; falling back to CICP");
        return false;
    };
    let Some(transform) = libjxl_sys::CmsTransform::new(
        &in_profile,
        libjxl_sys::LCMS_TYPE_RGBA_FLT,
        &out_profile,
        libjxl_sys::LCMS_TYPE_RGBA_FLT,
        libjxl_sys::LCMS_INTENT_PERCEPTUAL,
        0,
    ) else {
        log::warn!(
            "[AVIF] lcms2 could not build ICC→sRGB transform from {}-byte profile; falling back to CICP",
            source_icc.len()
        );
        return false;
    };
    transform.do_transform(
        rgba.as_ptr().cast(),
        output.as_mut_ptr().cast(),
        pixel_count as u32,
    );
    rgba.copy_from_slice(&output);
    true
}

/// Stub used for builds that exclude `jpegxl` (lcms2 isn't statically linked then). Always returns
/// `false`, so the caller falls back to CICP interpretation. Logs once per call site so a missing
/// feature flag is observable in diagnostics rather than silently misrendering.
#[cfg(all(feature = "avif-native", not(feature = "jpegxl")))]
pub(crate) fn apply_icc_to_srgb_via_lcms(_rgba: &mut [f32], source_icc: &[u8]) -> bool {
    log::warn!(
        "[AVIF] embedded {} byte ICC profile ignored: build lacks `jpegxl` feature (no lcms2 available)",
        source_icc.len()
    );
    false
}

/// Bytes per RGBA channel in libavif packed output (`rgb.depth` 8 -> 1, 10/12/16 -> 2).
#[cfg(feature = "avif-native")]
fn avif_rgba_channel_bytes(depth_out: u32) -> usize {
    if depth_out > 8 { 2 } else { 1 }
}

/// Expand libavif packed RGBA bytes into `u16` lanes (one lane per channel sample).
///
/// - **8-bit**: 1 byte per channel; lane holds 0..255.
/// - **10/12/16-bit**: little-endian `u16` per channel; lane holds the native sample.
#[cfg(feature = "avif-native")]
pub(crate) fn avif_unpack_rgba_bytes_to_u16_lanes(
    rgba_bytes: &[u8],
    depth_out: u32,
    pixel_count: usize,
) -> Result<Vec<u16>, String> {
    let channel_bytes = avif_rgba_channel_bytes(depth_out);
    let expected = pixel_count
        .checked_mul(4 * channel_bytes)
        .ok_or_else(|| "AVIF RGBA byte length overflow".to_string())?;
    if rgba_bytes.len() != expected {
        return Err(format!(
            "AVIF RGBA byte length mismatch: got {} expected {expected} for depth {depth_out}",
            rgba_bytes.len()
        ));
    }
    let mut rgba_u16 = vec![0_u16; pixel_count * 4];
    if depth_out == 8 {
        simple_image_viewer::simd_pixel_convert::unpack_u8_to_u16_lanes(&mut rgba_u16, rgba_bytes);
    } else {
        simple_image_viewer::simd_pixel_convert::copy_le_u16_lanes(&mut rgba_u16, rgba_bytes);
    }
    Ok(rgba_u16)
}

#[cfg(all(test, feature = "avif-native"))]
#[test]
fn avif_unpack_rgba_bytes_to_u16_lanes_8_and_16_bit() {
    let bytes_8 = [10_u8, 20, 30, 40, 50, 60, 70, 80];
    let u16_8 = avif_unpack_rgba_bytes_to_u16_lanes(&bytes_8, 8, 2).expect("8-bit unpack");
    assert_eq!(u16_8, [10, 20, 30, 40, 50, 60, 70, 80]);

    let bytes_16 = [0x34, 0x12, 0x78, 0x56, 0xBC, 0x9A, 0xDE, 0xF0];
    let u16_16 = avif_unpack_rgba_bytes_to_u16_lanes(&bytes_16, 16, 1).expect("16-bit unpack");
    assert_eq!(u16_16, [0x1234, 0x5678, 0x9ABC, 0xF0DE]);
}

/// Quantize libavif packed RGBA bytes to 8-bit lanes (ISO gain map path).
///
/// 8-bit input is returned as-is; deeper samples map `lane * 255 / channel_max`.
#[cfg(feature = "avif-native")]
pub(crate) fn avif_quantize_rgba_bytes_to_u8(
    rgba_bytes: Vec<u8>,
    depth: u32,
    pixel_count: usize,
) -> Result<Vec<u8>, String> {
    let channel_bytes = avif_rgba_channel_bytes(depth);
    let expected = pixel_count
        .checked_mul(4 * channel_bytes)
        .ok_or_else(|| "AVIF RGBA byte length overflow".to_string())?;
    if rgba_bytes.len() != expected {
        return Err(format!(
            "AVIF RGBA byte length mismatch: got {} expected {expected} for depth {depth}",
            rgba_bytes.len()
        ));
    }
    if depth == 8 {
        return Ok(rgba_bytes);
    }
    let inv_denom = u8::MAX as f32 / rgb_channel_max_f(depth);
    let mut out = vec![0_u8; pixel_count * 4];
    for (dst, chunk) in out.iter_mut().zip(rgba_bytes.chunks_exact(2)) {
        let value = u16::from_le_bytes([chunk[0], chunk[1]]);
        *dst = (value as f32 * inv_denom).round().clamp(0.0, 255.0) as u8;
    }
    Ok(out)
}

#[cfg(all(test, feature = "avif-native"))]
#[test]
fn avif_quantize_rgba_bytes_to_u8_8_and_16_bit() {
    let bytes_8 = vec![10_u8, 20, 30, 40];
    let out_8 = avif_quantize_rgba_bytes_to_u8(bytes_8, 8, 1).expect("8-bit quantize");
    assert_eq!(out_8, [10, 20, 30, 40]);

    let bytes_16 = vec![0x00, 0x04, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x00];
    let out_16 = avif_quantize_rgba_bytes_to_u8(bytes_16, 16, 1).expect("16-bit quantize");
    assert_eq!(out_16[0], 4);
    assert_eq!(out_16[1], 255);
    assert_eq!(out_16[2], 0);
    assert_eq!(out_16[3], 0);
}

/// Maximum channel value for libavif RGB packed in `u16` lanes (`depth` in {8,10,12,16}).
#[cfg(feature = "avif-native")]
pub(crate) fn rgb_channel_max_f(rgb_depth: u32) -> f32 {
    if !(8..=16).contains(&rgb_depth) {
        return u16::MAX as f32;
    }
    ((1u32 << rgb_depth).saturating_sub(1)).max(1) as f32
}

#[cfg(feature = "avif-native")]
pub(crate) fn avif_image_has_alpha_plane(image_ref: &libavif_sys::avifImage) -> bool {
    !image_ref.alphaPlane.is_null()
}

/// libavif leaves alpha at **0** when the bitstream has no alpha plane (even with `ignoreAlpha=0`).
/// The HDR plane WGSL treats `a <= 0` as fully transparent → black screen on opaque stills
/// (e.g. `paris_icc_exif_xmp.avif`).
#[cfg(feature = "avif-native")]
pub(crate) fn avif_fill_opaque_alpha_u16_if_no_alpha_plane(
    rgba_u16: &mut [u16],
    rgb_out_depth: u32,
    image_ref: &libavif_sys::avifImage,
) {
    if avif_image_has_alpha_plane(image_ref) {
        return;
    }
    let alpha_max = rgb_channel_max_f(rgb_out_depth) as u16;
    for px in rgba_u16.chunks_exact_mut(4) {
        px[3] = alpha_max;
    }
}

#[cfg(feature = "avif-native")]
pub(crate) fn avif_fill_opaque_alpha_f32_if_no_alpha_plane(
    rgba_f32: &mut [f32],
    image_ref: &libavif_sys::avifImage,
) {
    if avif_image_has_alpha_plane(image_ref) {
        return;
    }
    for px in rgba_f32.chunks_exact_mut(4) {
        px[3] = 1.0;
    }
}
fn avif_matrix_fallback_for_yuv_to_rgb(
    mc: libavif_sys::avifMatrixCoefficients,
) -> Option<libavif_sys::avifMatrixCoefficients> {
    match mc {
        3 => Some(libavif_sys::AVIF_MATRIX_COEFFICIENTS_BT2020_NCL),
        m if m == libavif_sys::AVIF_MATRIX_COEFFICIENTS_BT2020_CL => {
            Some(libavif_sys::AVIF_MATRIX_COEFFICIENTS_BT2020_NCL)
        }
        // SMPTE 2085, chroma-derived CL, ICTCP — no RGB matrix path in libavif; approximate with NCL.
        11 | 13 | 14 => Some(libavif_sys::AVIF_MATRIX_COEFFICIENTS_BT2020_NCL),
        _ => None,
    }
}

#[cfg(feature = "avif-native")]
fn avif_matrix_for_yuv_depth(depth: u32) -> libavif_sys::avifMatrixCoefficients {
    if depth >= 10 {
        libavif_sys::AVIF_MATRIX_COEFFICIENTS_BT2020_NCL
    } else {
        libavif_sys::AVIF_MATRIX_COEFFICIENTS_BT709
    }
}

/// Fields libavif consults in `avifPrepareReformatState` / `avifGetYUVColorSpaceInfo` that we may
/// temporarily override. Snapshotted at decode entry because `image_ref` aliases `*image`.
#[cfg(feature = "avif-native")]
#[derive(Clone, Copy)]
struct AvifYuvRgbReformatSnap {
    matrix_coefficients: libavif_sys::avifMatrixCoefficients,
    yuv_range: libavif_sys::avifRange,
    yuv_format: libavif_sys::avifPixelFormat,
    depth: u32,
}

#[cfg(feature = "avif-native")]
fn avif_reformat_snapshot(image_ref: &libavif_sys::avifImage) -> AvifYuvRgbReformatSnap {
    AvifYuvRgbReformatSnap {
        matrix_coefficients: image_ref.matrixCoefficients,
        yuv_range: image_ref.yuvRange,
        yuv_format: image_ref.yuvFormat,
        depth: image_ref.depth,
    }
}

#[cfg(feature = "avif-native")]
unsafe fn avif_restore_reformat_snap(
    image: *mut libavif_sys::avifImage,
    snap: &AvifYuvRgbReformatSnap,
) {
    unsafe {
        (*image).matrixCoefficients = snap.matrix_coefficients;
        (*image).yuvRange = snap.yuv_range;
    }
}

/// Apply every image-side adjustment that libavif `avifGetYUVColorSpaceInfo` / `reformat.c` need
/// before the first `avifImageYUVToRGB` call. Uses the **original** CICP snapshot from the file.
#[cfg(feature = "avif-native")]
unsafe fn avif_apply_yuv_to_rgb_image_fixes(
    image: *mut libavif_sys::avifImage,
    snap: &AvifYuvRgbReformatSnap,
) {
    unsafe {
        let img = &mut *image;
        let snap_mc = snap.matrix_coefficients;

        if snap_mc == libavif_sys::AVIF_MATRIX_COEFFICIENTS_UNSPECIFIED {
            img.matrixCoefficients = avif_matrix_for_yuv_depth(snap.depth);
        }

        if snap_mc == libavif_sys::AVIF_MATRIX_COEFFICIENTS_IDENTITY
            && (snap.yuv_format == libavif_sys::AVIF_PIXEL_FORMAT_YUV422
                || snap.yuv_format == libavif_sys::AVIF_PIXEL_FORMAT_YUV420)
        {
            img.matrixCoefficients = avif_matrix_for_yuv_depth(snap.depth);
        }

        if matches!(
            snap_mc,
            libavif_sys::AVIF_MATRIX_COEFFICIENTS_YCGCO
                | libavif_sys::AVIF_MATRIX_COEFFICIENTS_YCGCO_RE
                | libavif_sys::AVIF_MATRIX_COEFFICIENTS_YCGCO_RO
        ) && snap.yuv_range == libavif_sys::AVIF_RANGE_LIMITED
        {
            img.yuvRange = libavif_sys::AVIF_RANGE_FULL;
        }

        // Matrices without an RGB path (e.g. BT.2020 CL) must be substituted **before** conversion:
        // libavif can return OK for MC=10 while using a non-NCL path, skewing chroma on mis-tagged
        // NCL payloads (Chimera-class files).
        if let Some(fb) = avif_matrix_fallback_for_yuv_to_rgb(img.matrixCoefficients) {
            img.matrixCoefficients = fb;
            log::debug!(
                "[AVIF] YUV→RGB: matrixCoefficients {} → {} before conversion",
                snap_mc,
                fb
            );
        }
    }
}

/// `rgb.depth` override required by `avifPrepareReformatState` for YCgCo-Re/Ro (original CICP).
#[cfg(feature = "avif-native")]
fn avif_yuv_to_rgb_force_depth(
    orig_matrix: libavif_sys::avifMatrixCoefficients,
    yuv_depth: u32,
) -> Option<u32> {
    let bit_offset = match orig_matrix {
        libavif_sys::AVIF_MATRIX_COEFFICIENTS_YCGCO_RE => 2,
        libavif_sys::AVIF_MATRIX_COEFFICIENTS_YCGCO_RO => 1,
        _ => return None,
    };
    let d = yuv_depth.checked_sub(bit_offset)?;
    matches!(d, 8 | 10 | 12 | 16).then_some(d)
}

/// RGB-side options derived from image metadata and libavif defaults (`avifRGBImageSetDefaults`).
#[cfg(feature = "avif-native")]
struct AvifYuvToRgbParams {
    /// When `None`, keep `avifRGBImageSetDefaults` depth (= source YUV depth).
    force_depth: Option<u32>,
    /// PQ 10/12-bit: skip libyuv fast paths that skew subsampled HDR chroma (conformance samples).
    avoid_libyuv: bool,
}

#[cfg(feature = "avif-native")]
fn avif_yuv_to_rgb_params(
    snap: &AvifYuvRgbReformatSnap,
    image_ref: &libavif_sys::avifImage,
) -> AvifYuvToRgbParams {
    AvifYuvToRgbParams {
        force_depth: avif_yuv_to_rgb_force_depth(snap.matrix_coefficients, snap.depth),
        avoid_libyuv: image_ref.transferCharacteristics
            == libavif_sys::AVIF_TRANSFER_CHARACTERISTICS_SMPTE2084
            && snap.depth >= 10,
    }
}

#[cfg(feature = "avif-native")]
fn avif_yuv_to_rgb_output_depth(snap: &AvifYuvRgbReformatSnap, params: &AvifYuvToRgbParams) -> u32 {
    params.force_depth.unwrap_or(snap.depth)
}

#[cfg(feature = "avif-native")]
fn try_avif_yuv_to_rgb_rgba(
    image: *const libavif_sys::avifImage,
    image_ref: &libavif_sys::avifImage,
    rgba_bytes: &mut [u8],
    params: AvifYuvToRgbParams,
) -> Result<u32, libavif_sys::avifResult> {
    let pixel_count = image_ref.width as usize * image_ref.height as usize;
    let expected_depth = params.force_depth.unwrap_or(image_ref.depth);
    let channel_bytes = avif_rgba_channel_bytes(expected_depth);
    if rgba_bytes.len() != pixel_count * 4 * channel_bytes {
        return Err(libavif_sys::AVIF_RESULT_REFORMAT_FAILED);
    }

    let mut rgb = std::mem::MaybeUninit::<libavif_sys::avifRGBImage>::zeroed();
    unsafe { libavif_sys::avifRGBImageSetDefaults(rgb.as_mut_ptr(), image) };
    let mut rgb = unsafe { rgb.assume_init() };
    rgb.format = libavif_sys::AVIF_RGB_FORMAT_RGBA;
    rgb.isFloat = 0;
    rgb.maxThreads = 0;
    rgb.avoidLibYUV = if params.avoid_libyuv { 1 } else { 0 };
    if let Some(d) = params.force_depth {
        rgb.depth = d;
    }
    let depth_out = rgb.depth;
    if depth_out != 8 && depth_out != 10 && depth_out != 12 && depth_out != 16 {
        return Err(libavif_sys::AVIF_RESULT_REFORMAT_FAILED);
    }
    let channel_bytes = avif_rgba_channel_bytes(depth_out);
    let row_bytes = image_ref
        .width
        .checked_mul(4 * channel_bytes as u32)
        .ok_or(libavif_sys::AVIF_RESULT_REFORMAT_FAILED)?;
    rgb.rowBytes = row_bytes;
    rgb.pixels = rgba_bytes.as_mut_ptr();

    let result = unsafe { libavif_sys::avifImageYUVToRGB(image, &mut rgb) };
    if result != libavif_sys::AVIF_RESULT_OK {
        return Err(result);
    }
    Ok(depth_out)
}

#[cfg(feature = "avif-native")]
fn avif_rgba_bytes_to_u16_lanes(
    rgba_bytes: Vec<u8>,
    rgb_depth: u32,
    pixel_count: usize,
) -> Result<Vec<u16>, String> {
    if rgb_depth == 8 {
        avif_unpack_rgba_bytes_to_u16_lanes(&rgba_bytes, rgb_depth, pixel_count)
    } else if cfg!(target_endian = "little") {
        bytemuck::try_cast_vec(rgba_bytes).map_err(|(_err, bytes)| {
            format!(
                "AVIF RGBA cast to u16 lanes failed (byte len={})",
                bytes.len()
            )
        })
    } else {
        avif_unpack_rgba_bytes_to_u16_lanes(&rgba_bytes, rgb_depth, pixel_count)
    }
}

#[cfg(feature = "avif-native")]
fn decode_avif_image_rgba_bytes<F: Fn(libavif_sys::avifResult) -> String>(
    image: *mut libavif_sys::avifImage,
    image_ref: &libavif_sys::avifImage,
    result_to_string: &F,
) -> Result<(Vec<u8>, u32), String> {
    let snap = avif_reformat_snapshot(image_ref);
    let image_const: *const libavif_sys::avifImage = image;

    unsafe {
        avif_apply_yuv_to_rgb_image_fixes(image, &snap);
    }
    let params = avif_yuv_to_rgb_params(&snap, image_ref);
    let pixel_count = image_ref.width as usize * image_ref.height as usize;
    let depth_out = avif_yuv_to_rgb_output_depth(&snap, &params);
    if depth_out != 8 && depth_out != 10 && depth_out != 12 && depth_out != 16 {
        unsafe {
            avif_restore_reformat_snap(image, &snap);
        }
        return Err(format!("unsupported AVIF RGB output depth {depth_out}"));
    }
    let channel_bytes = avif_rgba_channel_bytes(depth_out);
    let mut rgba_bytes = vec![0_u8; pixel_count * 4 * channel_bytes];

    let result = try_avif_yuv_to_rgb_rgba(image_const, image_ref, &mut rgba_bytes, params);
    unsafe {
        avif_restore_reformat_snap(image, &snap);
    }
    match result {
        Ok(rgb_depth) => Ok((rgba_bytes, rgb_depth)),
        Err(code) => Err(format!(
            "libavif RGB conversion failed: {}",
            result_to_string(code)
        )),
    }
}

#[cfg(feature = "avif-native")]
pub(crate) fn decode_avif_image_rgba_u16<F: Fn(libavif_sys::avifResult) -> String>(
    image: *mut libavif_sys::avifImage,
    image_ref: &libavif_sys::avifImage,
    result_to_string: &F,
) -> Result<(Vec<u16>, u32), String> {
    let pixel_count = image_ref.width as usize * image_ref.height as usize;
    let snap = avif_reformat_snapshot(image_ref);
    let image_const: *const libavif_sys::avifImage = image;

    unsafe {
        avif_apply_yuv_to_rgb_image_fixes(image, &snap);
    }
    let params = avif_yuv_to_rgb_params(&snap, image_ref);
    let depth_out = avif_yuv_to_rgb_output_depth(&snap, &params);
    if depth_out != 8 && depth_out != 10 && depth_out != 12 && depth_out != 16 {
        unsafe {
            avif_restore_reformat_snap(image, &snap);
        }
        return Err(format!("unsupported AVIF RGB output depth {depth_out}"));
    }
    let mut rgba_u16 = vec![0_u16; pixel_count * 4];
    let decode_params = if depth_out == 8 {
        AvifYuvToRgbParams {
            force_depth: Some(16),
            avoid_libyuv: params.avoid_libyuv,
        }
    } else {
        params
    };
    let rgba_bytes = bytemuck::cast_slice_mut(&mut rgba_u16);

    let result = try_avif_yuv_to_rgb_rgba(image_const, image_ref, rgba_bytes, decode_params);
    unsafe {
        avif_restore_reformat_snap(image, &snap);
    }
    match result {
        Ok(rgb_depth) => {
            let reported_depth = if depth_out == 8 && rgb_depth == 16 {
                8
            } else {
                rgb_depth
            };
            Ok((rgba_u16, reported_depth))
        }
        Err(code) => Err(format!(
            "libavif RGB conversion failed: {}",
            result_to_string(code)
        )),
    }
}

/// Decode libavif YUV to packed RGBA and quantize to 8-bit lanes (gain map plane).
#[cfg(feature = "avif-native")]
pub(crate) fn decode_avif_image_rgba_u8<F: Fn(libavif_sys::avifResult) -> String>(
    image: *mut libavif_sys::avifImage,
    image_ref: &libavif_sys::avifImage,
    result_to_string: &F,
) -> Result<Vec<u8>, String> {
    let pixel_count = image_ref.width as usize * image_ref.height as usize;
    let (rgba_bytes, rgb_depth) = decode_avif_image_rgba_bytes(image, image_ref, result_to_string)?;
    avif_quantize_rgba_bytes_to_u8(rgba_bytes, rgb_depth, pixel_count)
}
