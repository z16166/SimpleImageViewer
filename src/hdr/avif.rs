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

#[cfg(feature = "avif-native")]
use crate::hdr::decode::{
    decode_transfer_to_display_linear, linear_primary_to_linear_srgb, linear_srgb_linear_to_srgb_u8,
};
#[cfg(feature = "avif-native")]
use crate::hdr::gain_map::{
    GainMapMetadata, IsoGainMapFraction, append_hdr_pixel_from_sdr_and_gain,
    gain_map_metadata_diagnostic, sample_gain_map_rgb,
};
#[cfg(feature = "avif-native")]
use std::ffi::CStr;
use crate::hdr::types::{
    HdrColorProfile, HdrImageMetadata, HdrLuminanceMetadata, HdrReference, HdrTransferFunction,
};
#[cfg(feature = "avif-native")]
use crate::hdr::types::{
    DEFAULT_SDR_WHITE_NITS, HdrColorSpace, HdrGainMapMetadata, HdrImageBuffer, HdrPixelFormat,
};
#[cfg(feature = "avif-native")]
use std::sync::Arc;

/// AVIF-related `ftyp` brands (MIAF). `avis` denotes image sequence in the container, not a filename suffix.
pub(crate) fn is_avif_brand(brand: &[u8]) -> bool {
    matches!(brand, b"avif" | b"avis")
}

#[cfg(feature = "avif-native")]
fn avif_ftyp_major_brand(bytes: &[u8]) -> Option<[u8; 4]> {
    if bytes.len() < 12 {
        return None;
    }
    if &bytes[4..8] != b"ftyp" {
        return None;
    }
    Some([bytes[8], bytes[9], bytes[10], bytes[11]])
}

/// Decode an AVIF **image sequence** (`moov` / `avis`) into SDR RGBA8 frames + delays for the
/// existing animated-image pipeline. `bytes` must stay alive for the whole parse (libavif keeps a
/// pointer). Returns `Ok(None)` if the file is not a multi-frame track sequence (caller uses static HDR decode).
///
/// **Playback:** the shared animation UI loops forever (like typical GIF viewing). We do **not** read
/// or apply libavif’s `repetitionCount` from the bitstream.
#[cfg(feature = "avif-native")]
pub(crate) fn try_decode_avif_image_sequence_sdr(
    bytes: &[u8],
) -> Result<Option<Vec<(std::time::Duration, u32, u32, Vec<u8>)>>, String> {
    use crate::constants::{DEFAULT_ANIMATION_DELAY_MS, MIN_ANIMATION_DELAY_THRESHOLD_MS};
    use std::time::Duration;

    fn result_to_string(result: libavif_sys::avifResult) -> String {
        unsafe {
            let ptr = libavif_sys::avifResultToString(result);
            if ptr.is_null() {
                return format!("libavif error {result}");
            }
            CStr::from_ptr(ptr).to_string_lossy().into_owned()
        }
    }

    let Some(decoder) = libavif_sys::AvifDecoderOwned::new() else {
        return Err("Failed to create libavif decoder".to_string());
    };

    unsafe {
        libavif_sys::siv_avif_decoder_set_strict_flags(
            decoder.as_ptr(),
            libavif_sys::AVIF_STRICT_DISABLED,
        );
        libavif_sys::siv_avif_decoder_set_image_content_flags(
            decoder.as_ptr(),
            libavif_sys::AVIF_IMAGE_CONTENT_COLOR_AND_ALPHA,
        );
    }

    if let Some(major) = avif_ftyp_major_brand(bytes) {
        if &major == b"avis" {
            let r = unsafe {
                libavif_sys::avifDecoderSetSource(
                    decoder.as_ptr(),
                    libavif_sys::AVIF_DECODER_SOURCE_TRACKS,
                )
            };
            if r != libavif_sys::AVIF_RESULT_OK {
                return Err(format!(
                    "libavif SetSource(TRACKS): {}",
                    result_to_string(r)
                ));
            }
        }
    }

    let r = unsafe {
        libavif_sys::avifDecoderSetIOMemory(decoder.as_ptr(), bytes.as_ptr(), bytes.len())
    };
    if r != libavif_sys::AVIF_RESULT_OK {
        return Err(format!("libavif SetIOMemory: {}", result_to_string(r)));
    }

    let r = unsafe { libavif_sys::avifDecoderParse(decoder.as_ptr()) };
    if r != libavif_sys::AVIF_RESULT_OK {
        return Ok(None);
    }

    let seq = unsafe { libavif_sys::siv_avif_decoder_image_sequence_track_present(decoder.as_ptr()) };
    let count = unsafe { libavif_sys::siv_avif_decoder_get_image_count(decoder.as_ptr()) };
    if seq == 0 || count <= 1 {
        return Ok(None);
    }

    let count = usize::try_from(count).map_err(|_| "libavif imageCount does not fit usize".to_string())?;

    let mut frames = Vec::with_capacity(count);
    for _ in 0..count {
        let r = unsafe { libavif_sys::avifDecoderNextImage(decoder.as_ptr()) };
        if r != libavif_sys::AVIF_RESULT_OK {
            return Err(format!("libavif NextImage: {}", result_to_string(r)));
        }

        let mut timing = std::mem::MaybeUninit::<libavif_sys::avifImageTiming>::zeroed();
        unsafe {
            libavif_sys::siv_avif_decoder_copy_image_timing(decoder.as_ptr(), timing.as_mut_ptr());
        }
        let timing = unsafe { timing.assume_init() };

        let img_ptr = unsafe { libavif_sys::siv_avif_decoder_get_image(decoder.as_ptr()) };
        if img_ptr.is_null() {
            return Err("libavif decoder image is null".to_string());
        }
        let img_ref = unsafe { &*img_ptr };
        if img_ref.width == 0 || img_ref.height == 0 {
            return Err("libavif sequence frame has zero size".to_string());
        }
        let rgba = decode_avif_image_rgba8_packed(img_ptr, img_ref, &result_to_string)?;

        let delay_ms = (timing.duration * 1000.0).round().clamp(0.0, u32::MAX as f64) as u32;
        let delay_ms = if delay_ms <= MIN_ANIMATION_DELAY_THRESHOLD_MS {
            DEFAULT_ANIMATION_DELAY_MS
        } else {
            delay_ms
        };
        frames.push((
            Duration::from_millis(delay_ms as u64),
            img_ref.width,
            img_ref.height,
            rgba,
        ));
    }

    Ok(Some(frames))
}

#[allow(dead_code)]
pub(crate) fn avif_cicp_to_metadata(
    color_primaries: u16,
    transfer_characteristics: u16,
    matrix_coefficients: u16,
    full_range: bool,
) -> HdrImageMetadata {
    // ITU-T H.273 CICP transfer characteristics (not libjxl enums).
    let transfer_function = match transfer_characteristics {
        8 => HdrTransferFunction::Linear,
        13 => HdrTransferFunction::Srgb,
        16 => HdrTransferFunction::Pq,
        18 => HdrTransferFunction::Hlg,
        _ => HdrTransferFunction::Unknown,
    };
    let reference = match transfer_function {
        HdrTransferFunction::Pq => HdrReference::DisplayReferred,
        HdrTransferFunction::Hlg => HdrReference::SceneLinear,
        _ => HdrReference::Unknown,
    };

    HdrImageMetadata {
        transfer_function,
        reference,
        color_profile: HdrColorProfile::Cicp {
            color_primaries,
            transfer_characteristics,
            matrix_coefficients,
            full_range,
        },
        luminance: HdrLuminanceMetadata::default(),
        gain_map: None,
    }
}

#[cfg(feature = "avif-native")]
#[allow(dead_code)]
pub(crate) fn decode_avif_hdr(path: &std::path::Path) -> Result<HdrImageBuffer, String> {
    let mmap = crate::mmap_util::map_file(path)
        .map_err(|err| format!("Failed to read AVIF: {err}"))?;
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
    let mmap = crate::mmap_util::map_file(path)
        .map_err(|err| format!("Failed to read AVIF: {err}"))?;
    decode_avif_hdr_bytes_with_target_capacity(&mmap[..], target_hdr_capacity)
}

#[cfg(feature = "avif-native")]
pub(crate) fn decode_avif_hdr_bytes_with_target_capacity(
    bytes: &[u8],
    target_hdr_capacity: f32,
) -> Result<HdrImageBuffer, String> {
    fn result_to_string(result: libavif_sys::avifResult) -> String {
        unsafe {
            let ptr = libavif_sys::avifResultToString(result);
            if ptr.is_null() {
                return format!("libavif error {result}");
            }
            CStr::from_ptr(ptr).to_string_lossy().into_owned()
        }
    }

    // Universal-viewer policy: immediately after each `avifDecoderCreate()`, force
    // `decoder->strictFlags = AVIF_STRICT_DISABLED` (0) — same as idiomatic C/C++ — so every
    // strictFlags-gated check in libavif is off (legacy encoders, missing alpha `ispe`, etc.).
    // Note: a few BMFF paths still fail without consulting `strictFlags`.
    let strict_flags = libavif_sys::AVIF_STRICT_DISABLED;

    // Request gain-map items first (`AVIF_IMAGE_CONTENT_ALL`). Some inputs fail when the decoder
    // walks optional gain-map associations; retry with color+alpha only.
    let content_flag_attempts: [(u32, &'static str); 2] = [
        (
            libavif_sys::AVIF_IMAGE_CONTENT_ALL,
            "color+alpha+gainmap",
        ),
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

        let msg = result_to_string(result);
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

    let image = libavif_sys::AvifImageOwned::from_raw_non_null(image_ptr);
    let image_ref = unsafe { &*image.as_ptr() };
    if image_ref.width == 0 || image_ref.height == 0 {
        return Err("libavif decoded zero-sized image".to_string());
    }
    if image_ref.depth == 0 || image_ref.depth > 16 {
        return Err(format!("unsupported AVIF bit depth {}", image_ref.depth));
    }

    let metadata = avif_cicp_to_metadata(
        image_ref.colorPrimaries as u16,
        image_ref.transferCharacteristics as u16,
        image_ref.matrixCoefficients as u16,
        image_ref.yuvRange == libavif_sys::AVIF_RANGE_FULL,
    )
    .with_clli(image_ref.clli.maxCLL, image_ref.clli.maxPALL);
    let color_space = metadata.color_space_hint();

    // Prefer AOMedia **libavif** `avifImageApplyGainMap` (`gainmap.c`): same YUV→RGB + tone map
    // path as the public API. `hdrHeadroom` is **log₂( HDR white / SDR white )** per `avif.h`.
    //
    // Output transfer is **PQ (SMPTE 2084)**, NOT linear: libavif's `avifToGammaLinear` is
    // `AVIF_CLAMP(x, 0, 1)`, which destroys HDR highlights (mid-gray after gain → ≥1 → clipped to
    // white). PQ encodes libavif's "extended SDR linear" (1.0 = 203 nits) into [0,1] without
    // clipping, and our WGSL `pq_to_display_linear` decodes back to "1.0 = SDR white" linear.
    if !image_ref.gainMap.is_null() {
        let gain_map_ref = unsafe { &*image_ref.gainMap };
        if let Ok(gain_metadata) = avif_gain_map_to_metadata(gain_map_ref) {
            let diagnostic = gain_map_metadata_diagnostic(gain_metadata, target_hdr_capacity);
            match avif_image_tone_map_pq_rgba32f(
                image.as_ptr(),
                image_ref.gainMap,
                target_hdr_capacity,
                &result_to_string,
            ) {
                Ok(rgba_f32) => {
                    let luminance = metadata.luminance;
                    let mut metadata = HdrImageMetadata {
                        transfer_function: HdrTransferFunction::Pq,
                        reference: HdrReference::DisplayReferred,
                        color_profile: HdrColorProfile::Cicp {
                            color_primaries: 1, // BT.709 (sRGB primaries)
                            transfer_characteristics: 16, // PQ
                            matrix_coefficients: 0,
                            full_range: true,
                        },
                        luminance,
                        gain_map: None,
                    };
                    metadata.gain_map = Some(HdrGainMapMetadata {
                        source: "AVIF",
                        target_hdr_capacity: Some(target_hdr_capacity),
                        diagnostic: format!("{diagnostic} (libavif avifImageApplyGainMap → PQ BT.709)"),
                        capped_display_referred: false,
                    });
                    return Ok(HdrImageBuffer {
                        width: image_ref.width,
                        height: image_ref.height,
                        format: HdrPixelFormat::Rgba32Float,
                        color_space: HdrColorSpace::LinearSrgb,
                        metadata,
                        rgba_f32: Arc::new(rgba_f32),
                    });
                }
                Err(err) => {
                    log::warn!(
                        "[HDR] libavif avifImageApplyGainMap failed: {err}; using software gain-map path"
                    );
                }
            }
        }
    }

    // **BT.2020 matrix 10** (constant luminance): libavif’s `reformat.c` has **no** dedicated
    // YUV→RGB matrix for CL — it uses an **explicit fallback to BT.2020 NCL (9)** for conversion,
    // same as several other “non-matrix” CICP codes (`avif_matrix_fallback_for_yuv_to_rgb`). That is
    // upstream **design**, not a bug we are papering over.
    //
    // Separately, **Microsoft Chimera** (`…_with_HDR_metadata.avif`) is a known case where the
    // **container CICP says 10** but the **coded luma/chroma matches NCL**; strict CL inverse would
    // skew colours (see AOMediaCodec/libavif#324). Using libavif’s NCL conversion matches that payload
    // and the paired SDR Chimera asset. **True** CL-encoded streams remain theoretically wrong here;
    // they are rare; fixing them would need a normative CL path, not a different “10 vs 9” hack.
    //
    // We do **not** persist any CICP rewrite to disk — only the temporary matrix passed into
    // `avifImageYUVToRGB` for this decode (metadata still reflects the file’s declared CICP).
    if image_ref.gainMap.is_null()
        && image_ref.matrixCoefficients == libavif_sys::AVIF_MATRIX_COEFFICIENTS_BT2020_CL
        && image_ref.transferCharacteristics == libavif_sys::AVIF_TRANSFER_CHARACTERISTICS_SMPTE2084
    {
        log::debug!(
            "[AVIF] CICP matrix 10 + PQ: YUV→RGB via libavif with matrix fallback 10→NCL (reformat has no CL matrix; Chimera-class files are often NCL payload with MC=10 tag)"
        );
    }

    let (rgba_u16, rgb_out_depth) =
        decode_avif_image_rgba_u16(image.as_ptr(), image_ref, &result_to_string)?;

    // Software fallback (e.g. ICC + gain map: `avifImageApplyGainMap` returns NOT_IMPLEMENTED).
    // Base RGB from `avifImageYUVToRGB` uses the image CICP transfer before ISO gain-map recovery.
    if let Some((gain_metadata, gain_width, gain_height, gain_rgba)) =
        decode_avif_gain_map(image_ref, &result_to_string)
    {
        let diagnostic = gain_map_metadata_diagnostic(gain_metadata, target_hdr_capacity);
        let mut rgba_f32 =
            Vec::with_capacity(image_ref.width as usize * image_ref.height as usize * 4);
        // We requested `rgb.depth = 16` from libavif, so values are always in [0, 65535] regardless
        // of source AVIF bit depth. Using `image_ref.depth` here is the bug that made an 8-bit AVIF
        // ~257x too bright (`paris_icc_exif_xmp.avif`, observed ~8 EV over reference).
        let scale = rgb_channel_max_f(rgb_out_depth);
        let sdr_white = DEFAULT_SDR_WHITE_NITS;
        for y in 0..image_ref.height {
            for x in 0..image_ref.width {
                let index = (y as usize * image_ref.width as usize + x as usize) * 4;
                let r = rgba_u16[index] as f32 / scale;
                let g = rgba_u16[index + 1] as f32 / scale;
                let b = rgba_u16[index + 2] as f32 / scale;
                let rgb_display_linear =
                    decode_transfer_to_display_linear([r, g, b], metadata.transfer_function, sdr_white);
                let rgb_linear_srgb =
                    linear_primary_to_linear_srgb(rgb_display_linear, color_space, &metadata);
                let sdr_rgba = [
                    linear_srgb_linear_to_srgb_u8(rgb_linear_srgb[0]),
                    linear_srgb_linear_to_srgb_u8(rgb_linear_srgb[1]),
                    linear_srgb_linear_to_srgb_u8(rgb_linear_srgb[2]),
                    (rgba_u16[index + 3] as f32 / scale * 255.0_f32)
                        .round()
                        .clamp(0.0, 255.0) as u8,
                ];
                let gain_value = sample_gain_map_rgb(
                    &gain_rgba,
                    gain_width,
                    gain_height,
                    x,
                    y,
                    image_ref.width,
                    image_ref.height,
                );
                append_hdr_pixel_from_sdr_and_gain(
                    &mut rgba_f32,
                    &sdr_rgba,
                    gain_value,
                    gain_metadata,
                    target_hdr_capacity,
                );
            }
        }
        // `append_hdr_pixel_from_sdr_and_gain` produces **linear sRGB** floats (same model as Ultra HDR JPEG).
        // The AVIF container still carries PQ + BT.2020 CICP for the coded base; if we keep that on
        // `metadata`, the HDR plane WGSL runs PQ EOTF on pixels that are already linear → blown highlights
        // on NativeHdr displays. Override to linear sRGB like `decode_ultra_hdr_jpeg_bytes_with_target_capacity`.
        let luminance = metadata.luminance;
        let mut metadata = HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb);
        metadata.luminance = luminance;
        metadata.gain_map = Some(HdrGainMapMetadata {
            source: "AVIF",
            target_hdr_capacity: Some(target_hdr_capacity),
            diagnostic,
            capped_display_referred: false,
        });
        return Ok(HdrImageBuffer {
            width: image_ref.width,
            height: image_ref.height,
            format: HdrPixelFormat::Rgba32Float,
            color_space: HdrColorSpace::LinearSrgb,
            metadata,
            rgba_f32: Arc::new(rgba_f32),
        });
    }

    // Normalize using the **output** `avifRGBImage.depth` libavif used (8/10/12/16), not the
    // source YUV bit depth: 8-bit RGB output must use 255, while 16-bit full-range uses 65535.
    let scale = rgb_channel_max_f(rgb_out_depth);
    let mut rgba_f32 = rgba_u16
        .into_iter()
        .map(|value| value as f32 / scale)
        .collect::<Vec<_>>();

    // Honour an embedded ICC profile when present (e.g. `paris_icc_exif_xmp.avif`, Display P3
    // photo). Without this we'd treat DP3-encoded pixels as sRGB primaries → desaturated colours.
    // The lcms2 transform produces **sRGB-OETF-encoded floats in [0,1]** which the WGSL shader
    // then linearises via `srgb_to_linear`. Falls through to CICP interpretation when the file
    // has no ICC, when lcms2 is unavailable (build without `jpegxl`), or when the transform fails.
    //
    // **Do not** run ICC→sRGB when CICP already selects **PQ or HLG**: pixels are HDR display codes
    // for the WGSL `decode_input_transfer` path; lcms perceptual transforms assume SDR-like encodings
    // and will skew hue (often a blue cast on BT.2020 PQ content with an embedded profile).
    let icc_slice = avif_image_icc_bytes(image_ref);
    let hdr_transfer_from_cicp = matches!(
        metadata.transfer_function,
        HdrTransferFunction::Pq | HdrTransferFunction::Hlg
    );
    if !icc_slice.is_empty() && hdr_transfer_from_cicp {
        log::debug!(
            "[AVIF] ignoring embedded ICC ({} bytes): CICP transfer {:?} — use WGSL PQ/HLG + CICP primaries, not ICC→sRGB",
            icc_slice.len(),
            metadata.transfer_function
        );
    }
    let final_metadata = if !icc_slice.is_empty()
        && !hdr_transfer_from_cicp
        && apply_icc_to_srgb_via_lcms(&mut rgba_f32, icc_slice)
    {
        let luminance = metadata.luminance;
        HdrImageMetadata {
            transfer_function: HdrTransferFunction::Srgb,
            reference: HdrReference::Unknown,
            color_profile: HdrColorProfile::Cicp {
                color_primaries: 1,
                transfer_characteristics: 13,
                matrix_coefficients: 0,
                full_range: true,
            },
            luminance,
            gain_map: None,
        }
    } else {
        metadata
    };
    let out_color_space = final_metadata.color_space_hint();

    Ok(HdrImageBuffer {
        width: image_ref.width,
        height: image_ref.height,
        format: HdrPixelFormat::Rgba32Float,
        color_space: out_color_space,
        metadata: final_metadata,
        rgba_f32: Arc::new(rgba_f32),
    })
}

/// Borrows the AVIF ICC profile bytes (or empty slice when absent / lcms2 unavailable). Centralises
/// the unsafe pointer/length read and feature gating used by the non-gain-map path.
#[cfg(feature = "avif-native")]
fn avif_image_icc_bytes(image: &libavif_sys::avifImage) -> &[u8] {
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
fn apply_icc_to_srgb_via_lcms(rgba: &mut [f32], source_icc: &[u8]) -> bool {
    let pixel_count = rgba.len() / 4;
    if pixel_count == 0 || source_icc.is_empty() {
        return false;
    }
    if pixel_count > u32::MAX as usize {
        log::warn!(
            "[AVIF] ICC transform skipped: {pixel_count} pixels exceeds lcms2 u32 limit"
        );
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
fn apply_icc_to_srgb_via_lcms(_rgba: &mut [f32], source_icc: &[u8]) -> bool {
    log::warn!(
        "[AVIF] embedded {} byte ICC profile ignored: build lacks `jpegxl` feature (no lcms2 available)",
        source_icc.len()
    );
    false
}

/// IEEE 754 binary16 → `f32` (libavif float RGB is half precision).
#[cfg(feature = "avif-native")]
#[inline]
fn f16_bits_to_f32(bits: u16) -> f32 {
    let s = ((bits as u32) & 0x8000) << 16;
    let mut e = ((bits >> 10) & 0x1f) as i32;
    let mut m = (bits & 0x3ff) as u32;
    if e == 0 {
        if m == 0 {
            return f32::from_bits(s);
        }
        e = 1;
        while (m & 0x400) == 0 {
            m <<= 1;
            e -= 1;
        }
        m &= 0x3ff;
    } else if e == 31 {
        return f32::from_bits(s | 0x7f80_0000 | (m << 13));
    }
    let exp = ((e + 127 - 15) as u32) << 23;
    f32::from_bits(s | exp | (m << 13))
}

#[cfg(feature = "avif-native")]
fn libavif_diag_cstring(diag: &libavif_sys::avifDiagnostics) -> String {
    unsafe { CStr::from_ptr(diag.error.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}

/// Frees libavif-allocated `avifRGBImage` pixel storage on drop.
#[cfg(feature = "avif-native")]
struct AvifRgbImageAllocatedPixels(libavif_sys::avifRGBImage);

#[cfg(feature = "avif-native")]
impl Drop for AvifRgbImageAllocatedPixels {
    fn drop(&mut self) {
        unsafe {
            if !self.0.pixels.is_null() {
                libavif_sys::avifRGBImageFreePixels(&mut self.0);
            }
        }
    }
}

/// Tone-mapped **PQ BT.709** RGBA, same as `avifImageApplyGainMap` but with the PQ output transfer
/// instead of LINEAR. Critical: libavif's `LINEAR` `linearToGamma` is `AVIF_CLAMP(x, 0, 1)` which
/// drops HDR highlights to white. PQ losslessly encodes libavif's "extended SDR" linear range
/// (1.0 = SDR white = 203 nits) into [0,1]. `target_hdr_capacity` is peak luminance / SDR white
/// (linear ratio); libavif expects **log₂** of that ratio.
#[cfg(feature = "avif-native")]
fn avif_image_tone_map_pq_rgba32f(
    base_image: *const libavif_sys::avifImage,
    gain_map: *mut libavif_sys::avifGainMap,
    target_hdr_capacity: f32,
    result_to_string: &impl Fn(libavif_sys::avifResult) -> String,
) -> Result<Vec<f32>, String> {
    let mut tone_mapped = AvifRgbImageAllocatedPixels(unsafe { std::mem::zeroed() });
    tone_mapped.0.format = libavif_sys::AVIF_RGB_FORMAT_RGBA;
    tone_mapped.0.depth = 16;
    tone_mapped.0.isFloat = 1;
    tone_mapped.0.maxThreads = 0;

    let hdr_headroom = target_hdr_capacity.max(1.0).log2();
    let mut diag: libavif_sys::avifDiagnostics = unsafe { std::mem::zeroed() };

    let result = unsafe {
        libavif_sys::avifImageApplyGainMap(
            base_image,
            gain_map,
            hdr_headroom,
            libavif_sys::AVIF_COLOR_PRIMARIES_BT709,
            libavif_sys::AVIF_TRANSFER_CHARACTERISTICS_SMPTE2084,
            &mut tone_mapped.0,
            std::ptr::null_mut(),
            &mut diag,
        )
    };

    if result != libavif_sys::AVIF_RESULT_OK {
        return Err(format!(
            "{} — {}",
            result_to_string(result),
            libavif_diag_cstring(&diag)
        ));
    }

    copy_avif_tone_mapped_rgbaf16_to_rgba32f(&tone_mapped.0)
}

#[cfg(feature = "avif-native")]
fn copy_avif_tone_mapped_rgbaf16_to_rgba32f(rgb: &libavif_sys::avifRGBImage) -> Result<Vec<f32>, String> {
    if rgb.isFloat == 0 || rgb.depth != 16 || rgb.format != libavif_sys::AVIF_RGB_FORMAT_RGBA {
        return Err(format!(
            "unexpected libavif tone-mapped RGB (isFloat={} depth={} format={})",
            rgb.isFloat, rgb.depth, rgb.format
        ));
    }
    let w = rgb.width as usize;
    let h = rgb.height as usize;
    if w == 0 || h == 0 || rgb.pixels.is_null() {
        return Err("libavif tone-mapped image has no pixels".to_string());
    }
    let row_bytes = rgb.rowBytes as usize;
    let mut out = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        let row = unsafe {
            std::slice::from_raw_parts(rgb.pixels.add(y * row_bytes), w * 8)
        };
        for x in 0..w {
            let i = x * 8;
            let r = f16_bits_to_f32(u16::from_le_bytes([row[i], row[i + 1]]));
            let g = f16_bits_to_f32(u16::from_le_bytes([row[i + 2], row[i + 3]]));
            let b = f16_bits_to_f32(u16::from_le_bytes([row[i + 4], row[i + 5]]));
            let a = f16_bits_to_f32(u16::from_le_bytes([row[i + 6], row[i + 7]]));
            out.extend_from_slice(&[r, g, b, a]);
        }
    }
    Ok(out)
}

#[cfg(feature = "avif-native")]
fn decode_avif_image_rgba8_packed(
    image: *const libavif_sys::avifImage,
    image_ref: &libavif_sys::avifImage,
    result_to_string: &impl Fn(libavif_sys::avifResult) -> String,
) -> Result<Vec<u8>, String> {
    let mut rgb = std::mem::MaybeUninit::<libavif_sys::avifRGBImage>::zeroed();
    unsafe { libavif_sys::avifRGBImageSetDefaults(rgb.as_mut_ptr(), image) };
    let mut rgb = unsafe { rgb.assume_init() };
    rgb.format = libavif_sys::AVIF_RGB_FORMAT_RGBA;
    rgb.depth = 8;
    rgb.isFloat = 0;
    rgb.maxThreads = 0;

    let w = image_ref.width as usize;
    let h = image_ref.height as usize;
    let mut rgba = vec![0u8; w * h * 4];
    rgb.pixels = rgba.as_mut_ptr();
    rgb.rowBytes = image_ref.width * 4;

    let result = unsafe { libavif_sys::avifImageYUVToRGB(image, &mut rgb) };
    if result != libavif_sys::AVIF_RESULT_OK {
        return Err(format!(
            "libavif RGB8 conversion failed: {}",
            result_to_string(result)
        ));
    }
    Ok(rgba)
}

/// Maximum channel value for libavif RGB packed in `u16` lanes (`depth` ∈ {8,10,12,16}).
#[cfg(feature = "avif-native")]
fn rgb_channel_max_f(rgb_depth: u32) -> f32 {
    if !(8..=16).contains(&rgb_depth) {
        return u16::MAX as f32;
    }
    ((1u32 << rgb_depth).saturating_sub(1)).max(1) as f32
}

/// Matrices libavif’s RGB reformat path does not implement directly (`reformat.c` /
/// `avifGetYUVColorSpaceInfo`). Before `avifImageYUVToRGB` we substitute BT.2020 NCL so conversion
/// proceeds — **including MC=10 (CL)**: libavif has no CL matrix there, so this matches upstream
/// behaviour (not an ad‑hoc “fix libavif” patch). ICTCP / chroma-derived CL codes use the same NCL
/// approximation. See decode path comment on Chimera-style **mis-tagged** MC=10.
#[cfg(feature = "avif-native")]
fn avif_matrix_fallback_for_yuv_to_rgb(mc: libavif_sys::avifMatrixCoefficients) -> Option<libavif_sys::avifMatrixCoefficients> {
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
unsafe fn avif_restore_reformat_snap(image: *mut libavif_sys::avifImage, snap: &AvifYuvRgbReformatSnap) {
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
fn decode_avif_image_rgba_u16<F: Fn(libavif_sys::avifResult) -> String>(
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

#[cfg(feature = "avif-native")]
fn decode_avif_gain_map<F: Fn(libavif_sys::avifResult) -> String>(
    image_ref: &libavif_sys::avifImage,
    result_to_string: &F,
) -> Option<(GainMapMetadata, u32, u32, Vec<u8>)> {
    if image_ref.gainMap.is_null() {
        return None;
    }
    let gain_map = unsafe { &*image_ref.gainMap };
    if gain_map.image.is_null() {
        log::warn!("[HDR] AVIF gain map metadata present without gain-map pixels");
        return None;
    }
    let metadata = match avif_gain_map_to_metadata(gain_map) {
        Ok(metadata) => metadata,
        Err(err) => {
            log::warn!("[HDR] AVIF gain map metadata is not usable: {err}");
            return None;
        }
    };
    let gain_image = unsafe { &*gain_map.image };
    let (gain_rgba_u16, gain_rgb_depth) =
        match decode_avif_image_rgba_u16(gain_map.image, gain_image, result_to_string) {
            Ok(pixels) => pixels,
            Err(err) => {
                log::warn!("[HDR] AVIF gain map pixel decode failed: {err}");
                return None;
            }
        };
    let scale = rgb_channel_max_f(gain_rgb_depth);
    let denominator = scale / u8::MAX as f32;
    let gain_rgba = gain_rgba_u16
        .into_iter()
        .map(|value| (value as f32 / denominator).round().clamp(0.0, 255.0) as u8)
        .collect();
    Some((metadata, gain_image.width, gain_image.height, gain_rgba))
}

#[cfg(feature = "avif-native")]
pub(crate) fn avif_gain_map_to_metadata(
    gain_map: &libavif_sys::avifGainMap,
) -> Result<GainMapMetadata, String> {
    let mut fraction = IsoGainMapFraction::default();
    for channel in 0..3 {
        fraction.gain_map_min[channel] = signed(gain_map.gainMapMin[channel]);
        fraction.gain_map_max[channel] = signed(gain_map.gainMapMax[channel]);
        fraction.gamma[channel] = unsigned(gain_map.gainMapGamma[channel]);
        fraction.base_offset[channel] = signed(gain_map.baseOffset[channel]);
        fraction.alternate_offset[channel] = signed(gain_map.alternateOffset[channel]);
    }
    fraction.base_hdr_headroom = unsigned(gain_map.baseHdrHeadroom);
    fraction.alternate_hdr_headroom = unsigned(gain_map.alternateHdrHeadroom);
    fraction.into_gain_map_metadata()
}

#[cfg(feature = "avif-native")]
fn signed(value: libavif_sys::avifSignedFraction) -> (i32, u32) {
    (value.n, value.d)
}

#[cfg(feature = "avif-native")]
fn unsigned(value: libavif_sys::avifUnsignedFraction) -> (u32, u32) {
    (value.n, value.d)
}

#[cfg(feature = "avif-native")]
trait AvifMetadataExt {
    fn with_clli(self, max_cll: u16, max_fall: u16) -> Self;
}

#[cfg(feature = "avif-native")]
impl AvifMetadataExt for HdrImageMetadata {
    fn with_clli(mut self, max_cll: u16, max_fall: u16) -> Self {
        if max_cll > 0 {
            self.luminance.max_cll_nits = Some(max_cll as f32);
        }
        if max_fall > 0 {
            self.luminance.max_fall_nits = Some(max_fall as f32);
        }
        self
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "avif-native")]
    use crate::hdr::avif::avif_gain_map_to_metadata;
    use crate::hdr::avif::{avif_cicp_to_metadata, is_avif_brand};
    use crate::hdr::types::{HdrColorProfile, HdrColorSpace, HdrReference, HdrTransferFunction};

    #[test]
    fn avif_cicp_maps_bt2020_pq_to_hdr_metadata() {
        let metadata = avif_cicp_to_metadata(9, 16, 9, false);

        assert_eq!(metadata.transfer_function, HdrTransferFunction::Pq);
        assert_eq!(metadata.reference, HdrReference::DisplayReferred);
        assert_eq!(
            metadata.color_profile,
            HdrColorProfile::Cicp {
                color_primaries: 9,
                transfer_characteristics: 16,
                matrix_coefficients: 9,
                full_range: false,
            }
        );
    }

    #[test]
    fn avif_cicp_maps_bt2020_hlg_to_rec2020_linear_color_space() {
        let metadata = avif_cicp_to_metadata(9, 18, 9, true);

        assert_eq!(metadata.transfer_function, HdrTransferFunction::Hlg);
        assert_eq!(metadata.reference, HdrReference::SceneLinear);
        assert_eq!(metadata.color_space_hint(), HdrColorSpace::Rec2020Linear);
    }

    #[test]
    fn avif_brand_detection_accepts_avif_and_avis() {
        assert!(is_avif_brand(b"avif"));
        assert!(is_avif_brand(b"avis"));
        assert!(!is_avif_brand(b"heic"));
    }

    #[cfg(feature = "avif-native")]
    #[test]
    fn avif_gain_map_fractions_convert_to_shared_metadata() {
        let gain_map = libavif_sys::avifGainMap {
            image: std::ptr::null_mut(),
            gainMapMin: [signed(0, 10), signed(1, 10), signed(2, 10)],
            gainMapMax: [signed(20, 10), signed(30, 10), signed(40, 10)],
            gainMapGamma: [unsigned(10, 10), unsigned(11, 10), unsigned(12, 10)],
            baseOffset: [signed(0, 10), signed(1, 10), signed(2, 10)],
            alternateOffset: [signed(3, 10), signed(4, 10), signed(5, 10)],
            baseHdrHeadroom: unsigned(0, 10),
            alternateHdrHeadroom: unsigned(20, 10),
            useBaseColorSpace: 1,
            altICC: libavif_sys::avifRWData {
                data: std::ptr::null_mut(),
                size: 0,
            },
            altColorPrimaries: 9,
            altTransferCharacteristics: 16,
            altMatrixCoefficients: 9,
            altYUVRange: 1,
            altDepth: 10,
            altPlaneCount: 3,
            altCLLI: libavif_sys::avifContentLightLevelInformationBox {
                maxCLL: 0,
                maxPALL: 0,
            },
        };

        let metadata = avif_gain_map_to_metadata(&gain_map).expect("convert metadata");

        assert_eq!(metadata.gain_map_min, [0.0, 0.1, 0.2]);
        assert_eq!(metadata.gain_map_max, [2.0, 3.0, 4.0]);
        assert_eq!(metadata.gamma, [1.0, 1.1, 1.2]);
        assert_eq!(metadata.offset_sdr, [0.0, 0.1, 0.2]);
        assert_eq!(metadata.offset_hdr, [0.3, 0.4, 0.5]);
        assert_eq!(metadata.hdr_capacity_min, 1.0);
        assert_eq!(metadata.hdr_capacity_max, 4.0);
    }

    #[cfg(feature = "avif-native")]
    fn signed(n: i32, d: u32) -> libavif_sys::avifSignedFraction {
        libavif_sys::avifSignedFraction { n, d }
    }

    #[cfg(feature = "avif-native")]
    fn unsigned(n: u32, d: u32) -> libavif_sys::avifUnsignedFraction {
        libavif_sys::avifUnsignedFraction { n, d }
    }
}
