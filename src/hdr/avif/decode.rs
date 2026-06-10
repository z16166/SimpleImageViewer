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

#[allow(dead_code)]
pub(crate) fn decode_avif_hdr(path: &std::path::Path) -> Result<HdrImageBuffer, String> {
    let mmap =
        crate::mmap_util::map_file(path).map_err(|err| format!("Failed to read AVIF: {err}"))?;
    decode_avif_hdr_bytes(&mmap[..])
}

#[cfg(feature = "avif-native")]
#[allow(dead_code)]
pub(crate) fn decode_avif_hdr_bytes(bytes: &[u8]) -> Result<HdrImageBuffer, String> {
    decode_avif_hdr_bytes_with_target_capacity(
        bytes,
        crate::hdr::types::HdrToneMapSettings::default().target_hdr_capacity(),
    )
}

#[cfg(feature = "avif-native")]
#[allow(dead_code)]
pub(crate) fn decode_avif_hdr_with_target_capacity(
    path: &std::path::Path,
    target_hdr_capacity: f32,
) -> Result<HdrImageBuffer, String> {
    let mmap =
        crate::mmap_util::map_file(path).map_err(|err| format!("Failed to read AVIF: {err}"))?;
    decode_avif_hdr_bytes_with_target_capacity(&mmap[..], target_hdr_capacity)
}

#[cfg(feature = "avif-native")]
pub(crate) fn decode_avif_hdr_bytes_with_target_capacity(
    bytes: &[u8],
    target_hdr_capacity: f32,
) -> Result<HdrImageBuffer, String> {
    // Universal-viewer policy: immediately after each `avifDecoderCreate()`, force
    // `decoder->strictFlags = AVIF_STRICT_DISABLED` (0) — same as idiomatic C/C++ — so every
    // strictFlags-gated check in libavif is off (legacy encoders, missing alpha `ispe`, etc.).
    // Note: a few BMFF paths still fail without consulting `strictFlags`.
    let strict_flags = libavif_sys::AVIF_STRICT_DISABLED;

    // Request gain-map items first (`AVIF_IMAGE_CONTENT_ALL`). Some inputs fail when the decoder
    // walks optional gain-map associations; retry with color+alpha only.
    let content_flag_attempts: [(u32, &'static str); 2] = [
        (libavif_sys::AVIF_IMAGE_CONTENT_ALL, "color+alpha+gainmap"),
        (
            libavif_sys::AVIF_IMAGE_CONTENT_COLOR_AND_ALPHA,
            "color+alpha",
        ),
    ];

    let mut image_ptr: *mut libavif_sys::avifImage = std::ptr::null_mut();
    let mut last_err: Option<String> = None;
    for (attempt_idx, &(flags, label)) in content_flag_attempts.iter().enumerate() {
        let Some(decoder) = libavif_sys::AvifDecoderOwned::new() else {
            return Err("Failed to create libavif decoder".to_string());
        };
        unsafe {
            libavif_sys::siv_avif_decoder_set_strict_flags(decoder.as_ptr(), strict_flags);
            libavif_sys::siv_avif_decoder_set_image_content_flags(decoder.as_ptr(), flags);
        }
        let Some(img) = libavif_sys::AvifImageOwned::create_empty() else {
            return Err("Failed to create libavif image".to_string());
        };
        let result = unsafe {
            libavif_sys::avifDecoderReadMemory(
                decoder.as_ptr(),
                img.as_ptr(),
                bytes.as_ptr(),
                bytes.len(),
            )
        };

        if result == libavif_sys::AVIF_RESULT_OK {
            if attempt_idx > 0 {
                log::debug!(
                    "[AVIF] decoded with imageContentToDecode={label} after first attempt failed"
                );
            }
            image_ptr = img.into_raw();
            break;
        }

        let msg = libavif_result_to_string(result);
        if attempt_idx == 0 {
            log::debug!(
                "[AVIF] libavif decode with {} failed ({msg}); retrying with color+alpha only",
                content_flag_attempts[0].1
            );
        }
        last_err = Some(format!("libavif decode failed: {msg}"));
    }

    if image_ptr.is_null() {
        return Err(last_err.unwrap_or_else(|| "libavif decode failed".to_string()));
    }

    // SAFETY: `image_ptr` is only set from `AvifImageOwned::into_raw()` after
    // `avifDecoderReadMemory` succeeds — caller-owned empty image, not `siv_avif_decoder_get_image`.
    let image = unsafe { libavif_sys::AvifImageOwned::from_owned_raw_non_null(image_ptr) };
    super::avif_image_to_hdr_buffer(image.as_ptr(), target_hdr_capacity)
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

/// Maximum channel value for libavif RGB packed in `u16` lanes (`depth` ∈ {8,10,12,16}).
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

/// Work around `REFORMAT_FAILED` cases in libavif `reformat.c`: unspecified matrix, identity +
/// chroma subsampling, YCgCo family + limited range, and matrices with no dedicated RGB path.
#[cfg(feature = "avif-native")]
unsafe fn avif_apply_full_reformat_relax(
    image: *mut libavif_sys::avifImage,
    snap: &AvifYuvRgbReformatSnap,
) {
    unsafe {
        let img = &mut *image;
        let snap_mc = snap.matrix_coefficients;

        if snap_mc == libavif_sys::AVIF_MATRIX_COEFFICIENTS_UNSPECIFIED {
            img.matrixCoefficients = if snap.depth >= 10 {
                libavif_sys::AVIF_MATRIX_COEFFICIENTS_BT2020_NCL
            } else {
                libavif_sys::AVIF_MATRIX_COEFFICIENTS_BT709
            };
        }

        if snap_mc == libavif_sys::AVIF_MATRIX_COEFFICIENTS_IDENTITY {
            if snap.yuv_format == libavif_sys::AVIF_PIXEL_FORMAT_YUV422
                || snap.yuv_format == libavif_sys::AVIF_PIXEL_FORMAT_YUV420
            {
                img.matrixCoefficients = if snap.depth >= 10 {
                    libavif_sys::AVIF_MATRIX_COEFFICIENTS_BT2020_NCL
                } else {
                    libavif_sys::AVIF_MATRIX_COEFFICIENTS_BT709
                };
            }
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

        if let Some(fb) = avif_matrix_fallback_for_yuv_to_rgb(img.matrixCoefficients) {
            img.matrixCoefficients = fb;
        }
    }
}

#[cfg(feature = "avif-native")]
fn avif_yuv_to_rgb_option_grid(yuv_format: libavif_sys::avifPixelFormat) -> Vec<(bool, bool)> {
    let subsampled = yuv_format == libavif_sys::AVIF_PIXEL_FORMAT_YUV420
        || yuv_format == libavif_sys::AVIF_PIXEL_FORMAT_YUV422;
    let mut v = vec![(false, false), (true, false)];
    if subsampled {
        v.push((false, true));
        v.push((true, true));
    }
    v
}

/// Extra RGB conversion options. YCgCo-Re requires `rgb.depth == image.depth - 2` (libavif
/// `reformat.c`); include those depths here using the **original** CICP matrix from the snapshot.
#[cfg(feature = "avif-native")]
fn rgb_depth_candidates(
    orig_matrix: libavif_sys::avifMatrixCoefficients,
    yuv_depth: u32,
) -> Vec<Option<u32>> {
    let mut out: Vec<Option<u32>> = Vec::new();
    let mut push = |d: Option<u32>| {
        if !out.contains(&d) {
            out.push(d);
        }
    };
    push(Some(16));
    push(None);
    match orig_matrix {
        libavif_sys::AVIF_MATRIX_COEFFICIENTS_YCGCO_RE => {
            if let Some(d) = yuv_depth.checked_sub(2) {
                if matches!(d, 8 | 10 | 12 | 16) {
                    push(Some(d));
                }
            }
        }
        libavif_sys::AVIF_MATRIX_COEFFICIENTS_YCGCO_RO => {
            if let Some(d) = yuv_depth.checked_sub(1) {
                if matches!(d, 8 | 10 | 12 | 16) {
                    push(Some(d));
                }
            }
        }
        _ => {}
    }
    out
}

#[cfg(feature = "avif-native")]
struct AvifYuvToRgbExtra {
    ignore_alpha: bool,
    chroma_nearest: bool,
    avoid_libyuv: bool,
}

#[cfg(feature = "avif-native")]
fn try_avif_yuv_to_rgb_rgba(
    image: *const libavif_sys::avifImage,
    image_ref: &libavif_sys::avifImage,
    force_depth: Option<u32>,
    extra: AvifYuvToRgbExtra,
) -> Result<(Vec<u16>, u32), libavif_sys::avifResult> {
    let mut rgb = std::mem::MaybeUninit::<libavif_sys::avifRGBImage>::zeroed();
    unsafe { libavif_sys::avifRGBImageSetDefaults(rgb.as_mut_ptr(), image) };
    let mut rgb = unsafe { rgb.assume_init() };
    rgb.format = libavif_sys::AVIF_RGB_FORMAT_RGBA;
    rgb.isFloat = 0;
    rgb.maxThreads = 0;
    rgb.ignoreAlpha = if extra.ignore_alpha { 1 } else { 0 };
    rgb.avoidLibYUV = if extra.avoid_libyuv { 1 } else { 0 };
    if extra.chroma_nearest
        && (image_ref.yuvFormat == libavif_sys::AVIF_PIXEL_FORMAT_YUV420
            || image_ref.yuvFormat == libavif_sys::AVIF_PIXEL_FORMAT_YUV422)
    {
        rgb.chromaUpsampling = libavif_sys::AVIF_CHROMA_UPSAMPLING_NEAREST;
    }
    if let Some(d) = force_depth {
        rgb.depth = d;
    }
    let depth_out = rgb.depth;
    if depth_out != 8 && depth_out != 10 && depth_out != 12 && depth_out != 16 {
        return Err(libavif_sys::AVIF_RESULT_REFORMAT_FAILED);
    }
    let channel_bytes = if depth_out > 8 { 2 } else { 1 };
    let row_bytes = image_ref
        .width
        .checked_mul(4 * channel_bytes as u32)
        .ok_or(libavif_sys::AVIF_RESULT_REFORMAT_FAILED)?;
    rgb.rowBytes = row_bytes;

    let pixel_count = image_ref.width as usize * image_ref.height as usize;
    let mut rgba_u16 = vec![0_u16; pixel_count * 4];
    rgb.pixels = rgba_u16.as_mut_ptr().cast::<u8>();

    let result = unsafe { libavif_sys::avifImageYUVToRGB(image, &mut rgb) };
    if result != libavif_sys::AVIF_RESULT_OK {
        return Err(result);
    }
    Ok((rgba_u16, depth_out))
}

#[cfg(feature = "avif-native")]
pub(crate) fn decode_avif_image_rgba_u16<F: Fn(libavif_sys::avifResult) -> String>(
    image: *mut libavif_sys::avifImage,
    image_ref: &libavif_sys::avifImage,
    result_to_string: &F,
) -> Result<(Vec<u16>, u32), String> {
    let snap = avif_reformat_snapshot(image_ref);
    let image_const: *const libavif_sys::avifImage = image;
    let mut last_err = String::new();

    let depth_list = rgb_depth_candidates(snap.matrix_coefficients, snap.depth);
    let opt_grid = avif_yuv_to_rgb_option_grid(snap.yuv_format);

    // PQ + 10/12-bit BT.2020: try **non–libyuv** path first — libyuv fast paths have historically been
    // a chroma source for HDR conformance samples (blue skew) when subsampled.
    let prefer_software_yuv = image_ref.transferCharacteristics
        == libavif_sys::AVIF_TRANSFER_CHARACTERISTICS_SMPTE2084
        && image_ref.depth >= 10;
    let avoid_libyuv_order = if prefer_software_yuv {
        [true, false]
    } else {
        [false, true]
    };

    let run_attempts = |last_err: &mut String| -> Option<(Vec<u16>, u32)> {
        for avoid_libyuv in avoid_libyuv_order {
            for &(ignore_alpha, chroma_nearest) in &opt_grid {
                for force_depth in &depth_list {
                    match try_avif_yuv_to_rgb_rgba(
                        image_const,
                        image_ref,
                        *force_depth,
                        AvifYuvToRgbExtra {
                            ignore_alpha,
                            chroma_nearest,
                            avoid_libyuv,
                        },
                    ) {
                        Ok(ok) => return Some(ok),
                        Err(code) => {
                            *last_err = format!(
                                "libavif RGB conversion failed: {}",
                                result_to_string(code)
                            );
                        }
                    }
                }
            }
        }
        None
    };

    // Matrices in `avif_matrix_fallback_for_yuv_to_rgb` (e.g. BT.2020 **CL = 10**) must be
    // substituted **before** the first attempt: libavif can return **OK** for MC=10 while using a
    // non‑NCL RGB path, which skews chroma (often blue) on mis‑tagged NCL payloads (e.g. Chimera).
    // Waiting until REFORMAT_FAILED to substitute is too late — we would already have returned bad RGB.
    unsafe {
        avif_restore_reformat_snap(image, &snap);
        if let Some(mc) = avif_matrix_fallback_for_yuv_to_rgb(snap.matrix_coefficients) {
            (*image).matrixCoefficients = mc;
            log::debug!(
                "[AVIF] YUV→RGB: matrixCoefficients {} → {} before reformat attempts",
                snap.matrix_coefficients,
                mc
            );
        }
    }
    if let Some(ok) = run_attempts(&mut last_err) {
        unsafe {
            avif_restore_reformat_snap(image, &snap);
        }
        return Ok(ok);
    }

    unsafe {
        avif_restore_reformat_snap(image, &snap);
        avif_apply_full_reformat_relax(image, &snap);
    }
    log::debug!(
        "[AVIF] YUV→RGB reformat: applying full CICP/range relaxations (matrix={} range={} format={} depth={})",
        snap.matrix_coefficients,
        snap.yuv_range,
        snap.yuv_format,
        snap.depth
    );
    if let Some(ok) = run_attempts(&mut last_err) {
        unsafe {
            avif_restore_reformat_snap(image, &snap);
        }
        return Ok(ok);
    }

    unsafe {
        avif_restore_reformat_snap(image, &snap);
    }
    Err(last_err)
}
