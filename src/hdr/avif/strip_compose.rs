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

//! Directory-tree strip ISO gain-map compose at strip resolution.

use std::path::Path;
use std::sync::Arc;

use super::avif_cicp_to_metadata;
use super::decode::{
    avif_image_duplicate_for_strip_fallback, avif_image_has_alpha_plane, avif_image_icc_bytes,
    decode_avif_image_rgba_u16, libavif_result_to_string, read_avif_decoder_image,
};
use super::gain_map::{avif_gain_map_to_metadata, decode_avif_gain_map};
use super::metadata::{AvifMetadataExt, avif_yuv_to_rgb_output_metadata};
use crate::hdr::avif_gain_map_deferred::avif_build_iso_sdr_baseline_rgba8;
use crate::hdr::decode::hdr_to_sdr_rgba8_with_tone_settings;
use crate::hdr::gain_map::iso_gain_map_skips_forward_compose;
use crate::hdr::tiled::{downsample_rgba8_nearest, preview_dimensions};
use crate::hdr::types::{HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat, HdrTransferFunction};
use crate::loader::hdr_tone_map_settings_for_directory_tree_strip;
use crate::loader::{DecodedImage, preview_aspect_matches_logical};

#[cfg(feature = "avif-native")]
type StripWithLogicalSize = (DecodedImage, (u32, u32));
#[cfg(feature = "avif-native")]
type OptionalStripResult<T> = Option<Result<T, String>>;

#[cfg(feature = "avif-native")]
fn gain_map_strip_dimensions(
    gain_w: u32,
    gain_h: u32,
    logical_w: u32,
    logical_h: u32,
    strip_w: u32,
    strip_h: u32,
) -> (u32, u32) {
    let gain_strip_w = ((u64::from(gain_w) * u64::from(strip_w)) / u64::from(logical_w))
        .max(1)
        .min(u64::from(gain_w)) as u32;
    let gain_strip_h = ((u64::from(gain_h) * u64::from(strip_h)) / u64::from(logical_h))
        .max(1)
        .min(u64::from(gain_h)) as u32;
    (gain_strip_w, gain_strip_h)
}

#[cfg(feature = "avif-native")]
fn avif_gain_map_alt_icc_bytes(gain_map: &libavif_sys::avifGainMap) -> &[u8] {
    if gain_map.altICC.data.is_null() || gain_map.altICC.size == 0 {
        return &[];
    }
    unsafe { std::slice::from_raw_parts(gain_map.altICC.data, gain_map.altICC.size) }
}

#[cfg(feature = "avif-native")]
fn libavif_apply_gain_map_eligible(
    image_ref: &libavif_sys::avifImage,
    gain_map: &libavif_sys::avifGainMap,
) -> bool {
    avif_image_icc_bytes(image_ref).is_empty() && avif_gain_map_alt_icc_bytes(gain_map).is_empty()
}

#[cfg(feature = "avif-native")]
fn fill_opaque_alpha_u8_if_no_alpha_plane(rgba: &mut [u8], image_ref: &libavif_sys::avifImage) {
    if avif_image_has_alpha_plane(image_ref) {
        return;
    }
    for px in rgba.chunks_exact_mut(4) {
        px[3] = 255;
    }
}

#[cfg(feature = "avif-native")]
fn gain_map_capacity_to_hdr_headroom(capacity: f32) -> Result<f32, String> {
    const MIN_CAPACITY: f32 = 0.001;
    if !capacity.is_finite() || capacity <= 0.0 {
        return Err(format!(
            "gain map compose capacity must be positive and finite, got {capacity}"
        ));
    }
    Ok(capacity.max(MIN_CAPACITY).log2())
}

#[cfg(feature = "avif-native")]
struct AvifRgbImagePixels {
    rgb: libavif_sys::avifRGBImage,
    owns_pixels: bool,
}

#[cfg(feature = "avif-native")]
impl AvifRgbImagePixels {
    fn new(image_ptr: *mut libavif_sys::avifImage) -> Self {
        let mut rgb = std::mem::MaybeUninit::<libavif_sys::avifRGBImage>::zeroed();
        unsafe {
            libavif_sys::avifRGBImageSetDefaults(rgb.as_mut_ptr(), image_ptr);
        }
        Self {
            rgb: unsafe { rgb.assume_init() },
            owns_pixels: false,
        }
    }

    fn configure_linear_float(&mut self) {
        self.rgb.format = libavif_sys::AVIF_RGB_FORMAT_RGBA;
        self.rgb.depth = 32;
        self.rgb.isFloat = 1;
        self.rgb.ignoreAlpha = 0;
    }

    fn mark_pixels_allocated(&mut self) {
        self.owns_pixels = true;
    }
}

#[cfg(feature = "avif-native")]
impl Drop for AvifRgbImagePixels {
    fn drop(&mut self) {
        if self.owns_pixels {
            unsafe {
                libavif_sys::avifRGBImageFreePixels(&mut self.rgb);
            }
            self.owns_pixels = false;
        }
    }
}

#[cfg(feature = "avif-native")]
fn avif_rgb_image_to_rgba32f(rgb: &libavif_sys::avifRGBImage) -> Result<Vec<f32>, String> {
    if rgb.pixels.is_null() {
        return Err("libavif ApplyGainMap returned null RGB pixels".to_string());
    }
    if rgb.width == 0 || rgb.height == 0 {
        return Err("libavif ApplyGainMap returned empty RGB image".to_string());
    }
    if rgb.isFloat == 0 || rgb.depth != 32 {
        return Err(format!(
            "strip ApplyGainMap expected 32-bit float RGBA, got depth={} isFloat={}",
            rgb.depth, rgb.isFloat
        ));
    }
    let w = rgb.width as usize;
    let h = rgb.height as usize;
    let row_bytes = rgb.rowBytes as usize;
    let float_bytes = w * 4 * std::mem::size_of::<f32>();
    let mut out = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        let src_row =
            unsafe { std::slice::from_raw_parts(rgb.pixels.add(y * row_bytes), row_bytes) };
        if src_row.len() < float_bytes {
            return Err(format!(
                "libavif float RGB row too short: need {float_bytes} bytes, have {}",
                src_row.len()
            ));
        }
        let floats: &[f32] = bytemuck::cast_slice(&src_row[..float_bytes]);
        out.extend_from_slice(floats);
    }
    Ok(out)
}

#[cfg(feature = "avif-native")]
fn tone_map_libavif_gain_map_strip(
    rgba_f32: Vec<f32>,
    strip_w: u32,
    strip_h: u32,
    color_space: HdrColorSpace,
) -> Result<Vec<u8>, String> {
    let composed = HdrImageBuffer {
        width: strip_w,
        height: strip_h,
        format: HdrPixelFormat::Rgba32Float,
        color_space,
        metadata: HdrImageMetadata {
            transfer_function: HdrTransferFunction::Linear,
            ..HdrImageMetadata::default()
        },
        rgba_f32: Arc::new(rgba_f32),
    };
    let tone = hdr_tone_map_settings_for_directory_tree_strip();
    hdr_to_sdr_rgba8_with_tone_settings(&composed, tone.exposure_ev, &tone)
}

#[cfg(feature = "avif-native")]
fn strip_compose_libavif_scale_apply(
    image: libavif_sys::AvifImageOwned,
    strip_w: u32,
    strip_h: u32,
    gain_map_compose_capacity: f32,
    color_space: HdrColorSpace,
) -> Result<Vec<u8>, String> {
    let image_ptr = image.as_ptr();
    let image_ref = unsafe { &*image_ptr };
    if image_ref.gainMap.is_null() {
        return Err("strip ApplyGainMap: missing gain map".to_string());
    }
    let gain_map = image_ref.gainMap;
    let mut diag = libavif_sys::avifDiagnostics { error: [0; 256] };

    if strip_w != image_ref.width || strip_h != image_ref.height {
        let scale = unsafe { libavif_sys::avifImageScale(image_ptr, strip_w, strip_h, &mut diag) };
        if scale != libavif_sys::AVIF_RESULT_OK {
            return Err(format!(
                "avifImageScale: {}",
                libavif_result_to_string(scale)
            ));
        }
    }

    let scaled_ref = unsafe { &*image_ptr };
    let mut rgb_out = AvifRgbImagePixels::new(image_ptr);
    rgb_out.configure_linear_float();

    let hdr_headroom = gain_map_capacity_to_hdr_headroom(gain_map_compose_capacity)?;
    let apply = unsafe {
        libavif_sys::avifImageApplyGainMap(
            image_ptr,
            gain_map,
            hdr_headroom,
            libavif_sys::AVIF_COLOR_PRIMARIES_BT709,
            libavif_sys::AVIF_TRANSFER_CHARACTERISTICS_LINEAR,
            &mut rgb_out.rgb,
            std::ptr::null_mut(),
            &mut diag,
        )
    };
    if apply != libavif_sys::AVIF_RESULT_OK {
        return Err(format!(
            "avifImageApplyGainMap: {}",
            libavif_result_to_string(apply)
        ));
    }
    rgb_out.mark_pixels_allocated();
    let rgba_f32 = avif_rgb_image_to_rgba32f(&rgb_out.rgb)?;
    drop(rgb_out);
    let mut pixels = tone_map_libavif_gain_map_strip(rgba_f32, strip_w, strip_h, color_space)?;
    fill_opaque_alpha_u8_if_no_alpha_plane(&mut pixels, scaled_ref);
    Ok(pixels)
}

#[cfg(feature = "avif-native")]
fn baseline_sdr_for_strip(
    image_ptr: *mut libavif_sys::avifImage,
    image_ref: &libavif_sys::avifImage,
    strip_w: u32,
    strip_h: u32,
) -> Result<Vec<u8>, String> {
    let metadata = avif_cicp_to_metadata(
        image_ref.colorPrimaries,
        image_ref.transferCharacteristics,
        image_ref.matrixCoefficients,
        image_ref.yuvRange == libavif_sys::AVIF_RANGE_FULL,
    )
    .with_clli(image_ref.clli.maxCLL, image_ref.clli.maxPALL);
    let metadata = avif_yuv_to_rgb_output_metadata(&metadata, image_ref);
    let color_space = metadata.color_space_hint();

    let decode_w = image_ref.width;
    let decode_h = image_ref.height;
    let (rgba_u16, rgb_out_depth) =
        decode_avif_image_rgba_u16(image_ptr, image_ref, &libavif_result_to_string)
            .map_err(|err| format!("strip compose base YUV->RGB: {err}"))?;
    let baseline = avif_build_iso_sdr_baseline_rgba8(
        &rgba_u16,
        rgb_out_depth,
        decode_w,
        decode_h,
        &metadata,
        color_space,
    );
    if decode_w == strip_w && decode_h == strip_h {
        return Ok(baseline);
    }
    Ok(downsample_rgba8_nearest(
        &baseline, decode_w, decode_h, strip_w, strip_h,
    ))
}

#[cfg(feature = "avif-native")]
fn strip_compose_rgb_downsample(
    image: libavif_sys::AvifImageOwned,
    path: &Path,
    gain_map_compose_capacity: f32,
    logical_w: u32,
    logical_h: u32,
    strip_w: u32,
    strip_h: u32,
) -> Result<StripWithLogicalSize, String> {
    let image_ptr = image.as_ptr();
    let image_ref = unsafe { &*image_ptr };
    let gain_map = unsafe { &*image_ref.gainMap };
    let gain_metadata = avif_gain_map_to_metadata(gain_map)
        .map_err(|err| format!("{path:?}: parse gain map metadata: {err}"))?;

    let gain_image_ref = unsafe { &*gain_map.image };
    let (gain_strip_w, gain_strip_h) = gain_map_strip_dimensions(
        gain_image_ref.width,
        gain_image_ref.height,
        logical_w,
        logical_h,
        strip_w,
        strip_h,
    );

    let Some((_, _, _, gain_rgba_full)) =
        decode_avif_gain_map(image_ref, &libavif_result_to_string)
    else {
        return Err(format!(
            "{path:?}: strip compose gain map RGB decode failed"
        ));
    };
    let gain_rgba = downsample_rgba8_nearest(
        &gain_rgba_full,
        gain_image_ref.width,
        gain_image_ref.height,
        gain_strip_w,
        gain_strip_h,
    );

    let sdr_baseline = baseline_sdr_for_strip(image_ptr, image_ref, strip_w, strip_h)
        .map_err(|err| format!("{path:?}: {err}"))?;

    let metadata = avif_cicp_to_metadata(
        image_ref.colorPrimaries,
        image_ref.transferCharacteristics,
        image_ref.matrixCoefficients,
        image_ref.yuvRange == libavif_sys::AVIF_RANGE_FULL,
    )
    .with_clli(image_ref.clli.maxCLL, image_ref.clli.maxPALL);
    let metadata = avif_yuv_to_rgb_output_metadata(&metadata, image_ref);
    let color_space = metadata.color_space_hint();

    let deferred_strip = crate::hdr::types::IsoGainMapGpuSource {
        sdr_rgba: Arc::new(sdr_baseline),
        gain_rgba: Arc::new(gain_rgba),
        gain_width: gain_strip_w,
        gain_height: gain_strip_h,
        metadata: gain_metadata,
    };
    let rgba_f32 = crate::hdr::jpeg_gain_map_gpu::compose_iso_deferred_cpu_pixels(
        strip_w,
        strip_h,
        &deferred_strip,
        gain_map_compose_capacity,
    )
    .map_err(|err| format!("{path:?}: strip compose CPU: {err}"))?;

    let composed = HdrImageBuffer {
        width: strip_w,
        height: strip_h,
        format: HdrPixelFormat::Rgba32Float,
        color_space,
        metadata: HdrImageMetadata {
            transfer_function: metadata.transfer_function,
            luminance: metadata.luminance,
            ..HdrImageMetadata::default()
        },
        rgba_f32: Arc::new(rgba_f32),
    };
    let tone = hdr_tone_map_settings_for_directory_tree_strip();
    let pixels = hdr_to_sdr_rgba8_with_tone_settings(&composed, tone.exposure_ev, &tone)
        .map_err(|err| format!("{path:?}: strip compose tone-map: {err}"))?;
    Ok((
        DecodedImage::new(strip_w, strip_h, pixels),
        (logical_w, logical_h),
    ))
}

#[cfg(feature = "avif-native")]
fn finish_strip(
    path: &Path,
    strip: DecodedImage,
    logical_w: u32,
    logical_h: u32,
) -> Result<StripWithLogicalSize, String> {
    if !preview_aspect_matches_logical(strip.width, strip.height, logical_w, logical_h) {
        return Err(format!(
            "{path:?}: strip compose aspect mismatch {}x{} vs {logical_w}x{logical_h}",
            strip.width, strip.height
        ));
    }
    Ok((strip, (logical_w, logical_h)))
}

/// ISO forward gain-map strip from an already-decoded libavif image.
#[cfg(feature = "avif-native")]
pub(crate) fn decode_avif_strip_iso_gain_map_composed_from_image(
    image: libavif_sys::AvifImageOwned,
    bytes: &[u8],
    path: &Path,
    max_side: u32,
    gain_map_compose_capacity: f32,
) -> OptionalStripResult<StripWithLogicalSize> {
    let image_ptr = image.as_ptr();
    let image_ref = unsafe { &*image_ptr };
    if image_ref.gainMap.is_null() {
        return None;
    }
    let gain_map = unsafe { &*image_ref.gainMap };
    let gain_metadata = match avif_gain_map_to_metadata(gain_map) {
        Ok(metadata) => metadata,
        Err(err) => return Some(Err(format!("{path:?}: parse gain map metadata: {err}"))),
    };
    if iso_gain_map_skips_forward_compose(gain_metadata) {
        return None;
    }
    if gain_map.image.is_null() {
        return Some(Err(format!(
            "{path:?}: ISO gain map metadata without gain-map pixels"
        )));
    }

    let logical_w = image_ref.width;
    let logical_h = image_ref.height;
    if logical_w == 0 || logical_h == 0 || max_side == 0 {
        return Some(Err(format!(
            "{path:?}: invalid strip compose dimensions {logical_w}x{logical_h} max_side={max_side}"
        )));
    }

    let (strip_w, strip_h) = preview_dimensions(logical_w, logical_h, max_side, max_side);
    if strip_w == 0 || strip_h == 0 {
        return Some(Err(format!(
            "{path:?}: strip preview dimensions collapsed for {logical_w}x{logical_h}"
        )));
    }

    let compose_metadata = avif_cicp_to_metadata(
        image_ref.colorPrimaries,
        image_ref.transferCharacteristics,
        image_ref.matrixCoefficients,
        image_ref.yuvRange == libavif_sys::AVIF_RANGE_FULL,
    )
    .with_clli(image_ref.clli.maxCLL, image_ref.clli.maxPALL);
    let compose_metadata = avif_yuv_to_rgb_output_metadata(&compose_metadata, image_ref);
    let compose_color_space = compose_metadata.color_space_hint();

    if libavif_apply_gain_map_eligible(image_ref, gain_map) {
        let backup = avif_image_duplicate_for_strip_fallback(&image).ok();
        let apply_started = std::time::Instant::now();
        match strip_compose_libavif_scale_apply(
            image,
            strip_w,
            strip_h,
            gain_map_compose_capacity,
            compose_color_space,
        )
        {
            Ok(pixels) => {
                let strip = DecodedImage::new(strip_w, strip_h, pixels);
                return Some(finish_strip(path, strip, logical_w, logical_h));
            }
            Err(err) => {
                log::debug!(
                    "[AVIF] strip scale+ApplyGainMap failed for {:?} after {:?}: {err}; RGB fallback",
                    path,
                    apply_started.elapsed(),
                );
            }
        }
        let fallback_started = std::time::Instant::now();
        let fallback_image = match backup {
            Some(copy) => copy,
            None => {
                log::debug!(
                    "[AVIF] strip compose YUV copy unavailable for {:?}; re-decode from mmap",
                    path
                );
                match read_avif_decoder_image(bytes) {
                    Ok(image) => image,
                    Err(err) => {
                        return Some(Err(format!(
                            "{path:?}: decode_avif_strip_compose fallback re-decode: {err}"
                        )));
                    }
                }
            }
        };
        let result = strip_compose_rgb_downsample(
            fallback_image,
            path,
            gain_map_compose_capacity,
            logical_w,
            logical_h,
            strip_w,
            strip_h,
        );
        log::debug!(
            "[AVIF] strip RGB fallback for {:?} in {:?}",
            path,
            fallback_started.elapsed(),
        );
        return Some(result);
    }

    Some(strip_compose_rgb_downsample(
        image,
        path,
        gain_map_compose_capacity,
        logical_w,
        logical_h,
        strip_w,
        strip_h,
    ))
}

/// ISO forward gain-map strip: one libavif read, YUV scale + ApplyGainMap when eligible,
/// otherwise full-res YUV->RGB with RGB downsample + CPU compose fallback.
#[cfg(feature = "avif-native")]
pub(crate) fn decode_avif_strip_iso_gain_map_composed(
    bytes: &[u8],
    path: &Path,
    max_side: u32,
    gain_map_compose_capacity: f32,
) -> OptionalStripResult<StripWithLogicalSize> {
    let image = match read_avif_decoder_image(bytes) {
        Ok(image) => image,
        Err(err) => return Some(Err(format!("{path:?}: decode_avif_strip_compose: {err}"))),
    };
    decode_avif_strip_iso_gain_map_composed_from_image(
        image,
        bytes,
        path,
        max_side,
        gain_map_compose_capacity,
    )
}

