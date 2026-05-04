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
    let bytes = std::fs::read(path).map_err(|err| format!("Failed to read AVIF: {err}"))?;
    decode_avif_hdr_bytes(&bytes)
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
pub(crate) fn decode_avif_hdr_with_target_capacity(
    path: &std::path::Path,
    target_hdr_capacity: f32,
) -> Result<HdrImageBuffer, String> {
    let bytes = std::fs::read(path).map_err(|err| format!("Failed to read AVIF: {err}"))?;
    decode_avif_hdr_bytes_with_target_capacity(&bytes, target_hdr_capacity)
}

#[cfg(feature = "avif-native")]
pub(crate) fn decode_avif_hdr_bytes_with_target_capacity(
    bytes: &[u8],
    target_hdr_capacity: f32,
) -> Result<HdrImageBuffer, String> {
    struct AvifImage(*mut libavif_sys::avifImage);
    impl Drop for AvifImage {
        fn drop(&mut self) {
            unsafe { libavif_sys::avifImageDestroy(self.0) };
        }
    }

    struct AvifDecoder(*mut libavif_sys::avifDecoder);
    impl Drop for AvifDecoder {
        fn drop(&mut self) {
            unsafe { libavif_sys::avifDecoderDestroy(self.0) };
        }
    }

    fn result_to_string(result: libavif_sys::avifResult) -> String {
        unsafe {
            let ptr = libavif_sys::avifResultToString(result);
            if ptr.is_null() {
                return format!("libavif error {result}");
            }
            CStr::from_ptr(ptr).to_string_lossy().into_owned()
        }
    }

    let decoder = AvifDecoder(unsafe { libavif_sys::avifDecoderCreate() });
    if decoder.0.is_null() {
        return Err("Failed to create libavif decoder".to_string());
    }
    unsafe { libavif_sys::siv_avif_decoder_decode_all_content(decoder.0) };
    let image = AvifImage(unsafe { libavif_sys::avifImageCreateEmpty() });
    if image.0.is_null() {
        return Err("Failed to create libavif image".to_string());
    }

    let result = unsafe {
        libavif_sys::avifDecoderReadMemory(decoder.0, image.0, bytes.as_ptr(), bytes.len())
    };
    if result != libavif_sys::AVIF_RESULT_OK {
        return Err(format!(
            "libavif decode failed: {}",
            result_to_string(result)
        ));
    }

    let image_ref = unsafe { &*image.0 };
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
        true,
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
                image.0,
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

    let rgba_u16 = decode_avif_image_rgba_u16(image.0, image_ref, result_to_string)?;

    // Software fallback (e.g. ICC + gain map: `avifImageApplyGainMap` returns NOT_IMPLEMENTED).
    // Base RGB from `avifImageYUVToRGB` uses the image CICP transfer before ISO gain-map recovery.
    if let Some((gain_metadata, gain_width, gain_height, gain_rgba)) =
        decode_avif_gain_map(image_ref, result_to_string)
    {
        let diagnostic = gain_map_metadata_diagnostic(gain_metadata, target_hdr_capacity);
        let mut rgba_f32 =
            Vec::with_capacity(image_ref.width as usize * image_ref.height as usize * 4);
        // We requested `rgb.depth = 16` from libavif, so values are always in [0, 65535] regardless
        // of source AVIF bit depth. Using `image_ref.depth` here is the bug that made an 8-bit AVIF
        // ~257x too bright (`paris_icc_exif_xmp.avif`, observed ~8 EV over reference).
        let scale = u16::MAX as f32;
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
                    (rgba_u16[index + 3] as f32 / scale * 255.0)
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

    // Same bit-depth caveat as the gain-map software path: libavif fills the output buffer with
    // 16-bit values (we set `rgb.depth = 16`), so 8/10/12-bit AVIFs must still be normalized by
    // 65535. Using the source `image_ref.depth` would make 8-bit content ~257x too bright (~8 EV).
    let scale = u16::MAX as f32;
    let mut rgba_f32 = rgba_u16
        .into_iter()
        .map(|value| value as f32 / scale)
        .collect::<Vec<_>>();

    // Honour an embedded ICC profile when present (e.g. `paris_icc_exif_xmp.avif`, Display P3
    // photo). Without this we'd treat DP3-encoded pixels as sRGB primaries → desaturated colours.
    // The lcms2 transform produces **sRGB-OETF-encoded floats in [0,1]** which the WGSL shader
    // then linearises via `srgb_to_linear`. Falls through to CICP interpretation when the file
    // has no ICC, when lcms2 is unavailable (build without `jpegxl`), or when the transform fails.
    let icc_slice = avif_image_icc_bytes(image_ref);
    let (final_color_space, final_metadata) = if !icc_slice.is_empty()
        && apply_icc_to_srgb_via_lcms(&mut rgba_f32, icc_slice)
    {
        let luminance = metadata.luminance;
        let icc_metadata = HdrImageMetadata {
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
        };
        (HdrColorSpace::LinearSrgb, icc_metadata)
    } else {
        (color_space, metadata)
    };

    Ok(HdrImageBuffer {
        width: image_ref.width,
        height: image_ref.height,
        format: HdrPixelFormat::Rgba32Float,
        color_space: final_color_space,
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
    let ok = unsafe {
        let in_profile = libjxl_sys::cmsOpenProfileFromMem(
            source_icc.as_ptr().cast(),
            source_icc.len() as u32,
        );
        if in_profile.is_null() {
            log::warn!(
                "[AVIF] lcms2 could not parse embedded ICC ({} bytes); falling back to CICP",
                source_icc.len()
            );
            return false;
        }
        let out_profile = libjxl_sys::cmsCreate_sRGBProfile();
        if out_profile.is_null() {
            libjxl_sys::cmsCloseProfile(in_profile);
            log::warn!("[AVIF] lcms2 could not build sRGB profile; falling back to CICP");
            return false;
        }
        let transform = libjxl_sys::cmsCreateTransform(
            in_profile,
            libjxl_sys::LCMS_TYPE_RGBA_FLT,
            out_profile,
            libjxl_sys::LCMS_TYPE_RGBA_FLT,
            libjxl_sys::LCMS_INTENT_PERCEPTUAL,
            0,
        );
        let built = !transform.is_null();
        if built {
            libjxl_sys::cmsDoTransform(
                transform,
                rgba.as_ptr().cast(),
                output.as_mut_ptr().cast(),
                pixel_count as u32,
            );
            libjxl_sys::cmsDeleteTransform(transform);
        } else {
            log::warn!(
                "[AVIF] lcms2 could not build ICC→sRGB transform from {}-byte profile; falling back to CICP",
                source_icc.len()
            );
        }
        libjxl_sys::cmsCloseProfile(in_profile);
        libjxl_sys::cmsCloseProfile(out_profile);
        built
    };

    if ok {
        rgba.copy_from_slice(&output);
    }
    ok
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
    let mut tone_mapped: libavif_sys::avifRGBImage = unsafe { std::mem::zeroed() };
    tone_mapped.format = libavif_sys::AVIF_RGB_FORMAT_RGBA;
    tone_mapped.depth = 16;
    tone_mapped.isFloat = 1;
    tone_mapped.maxThreads = 0;

    let hdr_headroom = target_hdr_capacity.max(1.0).log2();
    let mut diag: libavif_sys::avifDiagnostics = unsafe { std::mem::zeroed() };

    let result = unsafe {
        libavif_sys::avifImageApplyGainMap(
            base_image,
            gain_map,
            hdr_headroom,
            libavif_sys::AVIF_COLOR_PRIMARIES_BT709,
            libavif_sys::AVIF_TRANSFER_CHARACTERISTICS_SMPTE2084,
            &mut tone_mapped,
            std::ptr::null_mut(),
            &mut diag,
        )
    };

    if result != libavif_sys::AVIF_RESULT_OK {
        unsafe {
            if !tone_mapped.pixels.is_null() {
                libavif_sys::avifRGBImageFreePixels(&mut tone_mapped);
            }
        }
        return Err(format!(
            "{} — {}",
            result_to_string(result),
            libavif_diag_cstring(&diag)
        ));
    }

    let out = copy_avif_tone_mapped_rgbaf16_to_rgba32f(&tone_mapped)?;
    unsafe {
        libavif_sys::avifRGBImageFreePixels(&mut tone_mapped);
    }
    Ok(out)
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
fn decode_avif_image_rgba_u16(
    image: *const libavif_sys::avifImage,
    image_ref: &libavif_sys::avifImage,
    result_to_string: impl Fn(libavif_sys::avifResult) -> String,
) -> Result<Vec<u16>, String> {
    let mut rgb = std::mem::MaybeUninit::<libavif_sys::avifRGBImage>::zeroed();
    unsafe { libavif_sys::avifRGBImageSetDefaults(rgb.as_mut_ptr(), image) };
    let mut rgb = unsafe { rgb.assume_init() };
    rgb.format = libavif_sys::AVIF_RGB_FORMAT_RGBA;
    rgb.depth = 16;
    rgb.isFloat = 0;
    rgb.maxThreads = 0;

    let pixel_count = image_ref.width as usize * image_ref.height as usize;
    let mut rgba_u16 = vec![0_u16; pixel_count * 4];
    rgb.pixels = rgba_u16.as_mut_ptr().cast::<u8>();
    rgb.rowBytes = image_ref.width * 4 * std::mem::size_of::<u16>() as u32;

    let result = unsafe { libavif_sys::avifImageYUVToRGB(image, &mut rgb) };
    if result != libavif_sys::AVIF_RESULT_OK {
        return Err(format!(
            "libavif RGB conversion failed: {}",
            result_to_string(result)
        ));
    }

    Ok(rgba_u16)
}

#[cfg(feature = "avif-native")]
fn decode_avif_gain_map(
    image_ref: &libavif_sys::avifImage,
    result_to_string: impl Fn(libavif_sys::avifResult) -> String,
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
    let gain_rgba_u16 =
        match decode_avif_image_rgba_u16(gain_map.image, gain_image, result_to_string) {
            Ok(pixels) => pixels,
            Err(err) => {
                log::warn!("[HDR] AVIF gain map pixel decode failed: {err}");
                return None;
            }
        };
    // libavif always fills the requested 16-bit output, so u16/257 → u8 regardless of source depth.
    // Using `gain_image.depth` here was symmetric to the base-image bug (saturated to 255 for 8-bit
    // gain maps → max gain everywhere, helping the "too bright" symptom).
    let denominator = u16::MAX as f32 / u8::MAX as f32;
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
