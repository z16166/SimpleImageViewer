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

use std::sync::Arc;

use crate::hdr::types::{
    HdrImageBuffer, HdrPixelFormat, HdrReference, HdrToneMapSettings, HdrTransferFunction,
    DEFAULT_SDR_WHITE_NITS,
};
use crate::loader::DecodedImage;

/// True when the HDR buffer carries a deferred ISO gain map (Ultra HDR / AVIF JPEG-R, etc.).
pub(crate) fn hdr_has_iso_deferred_gain_map(hdr: &HdrImageBuffer) -> bool {
    hdr.metadata
        .gain_map
        .as_ref()
        .and_then(|gain_map| gain_map.iso_deferred.as_ref())
        .is_some()
}

#[inline]
pub(crate) fn hdr_to_sdr_with_user_tone(
    buffer: &HdrImageBuffer,
    tone: &HdrToneMapSettings,
) -> Result<Vec<u8>, String> {
    if let Some(gain_map) = buffer.metadata.gain_map.as_ref()
        && let Some(iso) = gain_map.iso_deferred.as_ref()
    {
        return Ok(iso.sdr_rgba.as_ref().clone());
    }
    crate::hdr::decode::hdr_to_sdr_rgba8_with_tone_settings(buffer, tone.exposure_ev, tone)
}

/// CPU tone-mapped SDR for strip thumbnails and other previews.
///
/// Unlike [`hdr_to_sdr_with_user_tone`], deferred ISO gain-map images are forward-composed on
/// the CPU first so previews match the bright GPU display path instead of the dark baseline SDR.
pub(crate) fn hdr_to_display_sdr_rgba8_for_preview(
    buffer: &HdrImageBuffer,
    tone: &HdrToneMapSettings,
) -> Result<Vec<u8>, String> {
    if let Some(gain_map) = buffer.metadata.gain_map.as_ref()
        && let Some(iso) = gain_map.iso_deferred.as_ref()
    {
        let rgba_f32 = crate::hdr::jpeg_gain_map_gpu::compose_iso_deferred_cpu_pixels(
            buffer.width,
            buffer.height,
            iso,
            tone.target_hdr_capacity(),
        )?;
        let composed = HdrImageBuffer {
            width: buffer.width,
            height: buffer.height,
            format: HdrPixelFormat::Rgba32Float,
            color_space: buffer.color_space,
            metadata: buffer.metadata.clone(),
            rgba_f32: Arc::new(rgba_f32),
        };
        return crate::hdr::decode::hdr_to_sdr_rgba8_with_tone_settings(
            &composed,
            tone.exposure_ev,
            tone,
        );
    }
    hdr_to_sdr_with_user_tone(buffer, tone)
}

/// PQ/HLG strip previews use `max_display_nits == sdr_white_nits` so peak scaling does not
/// crush AVIF/JPEG-HDR base layers to near-black SDR while the main viewer runs native HDR.
pub(crate) fn hdr_tone_map_settings_for_directory_tree_strip() -> HdrToneMapSettings {
    HdrToneMapSettings {
        max_display_nits: DEFAULT_SDR_WHITE_NITS,
        ..HdrToneMapSettings::default()
    }
}

/// CPU SDR bytes for directory-tree strip thumbnails (cold worker + post-install upgrade).
pub(crate) fn hdr_to_directory_tree_strip_sdr_rgba8(
    buffer: &HdrImageBuffer,
) -> Result<Vec<u8>, String> {
    let tone = hdr_tone_map_settings_for_directory_tree_strip();
    hdr_to_display_sdr_rgba8_for_preview(buffer, &tone)
}

/// Strip-sized SDR preview: downsample HDR first, then tone-map (avoids full-frame CPU work).
pub(crate) fn hdr_directory_tree_strip_sdr_at_max_side(
    buffer: &HdrImageBuffer,
    max_side: u32,
) -> Result<(u32, u32, Vec<u8>), String> {
    if buffer.rgba_f32.is_empty() {
        return Err(format!(
            "HDR strip preview requires float pixels ({}x{})",
            buffer.width, buffer.height
        ));
    }
    let preview = crate::hdr::tiled::downsample_hdr_image_nearest(
        buffer,
        max_side,
        max_side,
    )?;
    let pixels = hdr_to_directory_tree_strip_sdr_rgba8(&preview)?;
    Ok((preview.width, preview.height, pixels))
}

/// CPU strip thumbnail from an installed HDR buffer, or downsampled SDR fallback when empty.
pub(crate) fn directory_tree_strip_from_hdr_or_fallback(
    hdr: &HdrImageBuffer,
    fallback: &crate::loader::DecodedImage,
    max_side: u32,
) -> Result<crate::loader::DecodedImage, String> {
    use crate::loader::downsample_decoded_for_strip;

    if !hdr.rgba_f32.is_empty() {
        if let Ok((width, height, pixels)) = hdr_directory_tree_strip_sdr_at_max_side(hdr, max_side)
        {
            return Ok(crate::loader::DecodedImage::new(width, height, pixels));
        }
    }

    if !fallback.is_sdr_deferred_placeholder() {
        return downsample_decoded_for_strip(fallback, max_side);
    }

    if hdr_has_iso_deferred_gain_map(hdr) {
        let tone = hdr_tone_map_settings_for_directory_tree_strip();
        let pixels = hdr_to_display_sdr_rgba8_for_preview(hdr, &tone)?;
        let decoded = crate::loader::DecodedImage::new(hdr.width, hdr.height, pixels);
        return downsample_decoded_for_strip(&decoded, max_side);
    }

    Err("strip preview unavailable: no HDR pixels or SDR fallback".to_string())
}

/// CPU-composed SDR strip from an ISO deferred HDR buffer (post-GPU / post-install upgrade).
pub(crate) fn directory_tree_strip_composed_from_iso_deferred(
    hdr: &HdrImageBuffer,
    max_side: u32,
) -> Result<crate::loader::DecodedImage, String> {
    if !hdr_has_iso_deferred_gain_map(hdr) {
        return Err("strip compose upgrade requires ISO deferred gain map".to_string());
    }
    if !hdr.rgba_f32.is_empty() {
        let (width, height, pixels) = hdr_directory_tree_strip_sdr_at_max_side(hdr, max_side)?;
        return Ok(crate::loader::DecodedImage::new(width, height, pixels));
    }
    let iso = hdr
        .metadata
        .gain_map
        .as_ref()
        .and_then(|gain_map| gain_map.iso_deferred.as_ref())
        .ok_or_else(|| "strip compose upgrade missing ISO deferred planes".to_string())?;
    let (strip_w, strip_h) =
        crate::hdr::tiled::preview_dimensions(hdr.width, hdr.height, max_side, max_side);
    if strip_w == 0 || strip_h == 0 {
        return Err("strip compose upgrade dimensions must be non-zero".to_string());
    }
    let sdr_strip = downsample_rgba8_nearest(
        iso.sdr_rgba.as_slice(),
        hdr.width,
        hdr.height,
        strip_w,
        strip_h,
    );
    let gain_strip_w = ((u64::from(iso.gain_width) * u64::from(strip_w)) / u64::from(hdr.width))
        .max(1)
        .min(u64::from(iso.gain_width)) as u32;
    let gain_strip_h = ((u64::from(iso.gain_height) * u64::from(strip_h)) / u64::from(hdr.height))
        .max(1)
        .min(u64::from(iso.gain_height)) as u32;
    let gain_strip = downsample_rgba8_nearest(
        iso.gain_rgba.as_slice(),
        iso.gain_width,
        iso.gain_height,
        gain_strip_w,
        gain_strip_h,
    );
    let deferred_strip = crate::hdr::types::IsoGainMapGpuSource {
        sdr_rgba: std::sync::Arc::new(sdr_strip),
        gain_rgba: std::sync::Arc::new(gain_strip),
        gain_width: gain_strip_w,
        gain_height: gain_strip_h,
        metadata: iso.metadata,
    };
    let tone = hdr_tone_map_settings_for_directory_tree_strip();
    let rgba_f32 = crate::hdr::jpeg_gain_map_gpu::compose_iso_deferred_cpu_pixels(
        strip_w,
        strip_h,
        &deferred_strip,
        tone.target_hdr_capacity(),
    )?;
    let composed = HdrImageBuffer {
        width: strip_w,
        height: strip_h,
        format: HdrPixelFormat::Rgba32Float,
        color_space: hdr.color_space,
        metadata: hdr.metadata.clone(),
        rgba_f32: std::sync::Arc::new(rgba_f32),
    };
    let pixels = hdr_to_directory_tree_strip_sdr_rgba8(&composed)?;
    Ok(crate::loader::DecodedImage::new(strip_w, strip_h, pixels))
}

fn downsample_rgba8_nearest(
    pixels: &[u8],
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
) -> Vec<u8> {
    let mut out = vec![0_u8; dst_w as usize * dst_h as usize * 4];
    for y in 0..dst_h {
        let src_y = crate::hdr::tiled::preview_sample_coord(y, dst_h, src_h);
        for x in 0..dst_w {
            let src_x = crate::hdr::tiled::preview_sample_coord(x, dst_w, src_w);
            let src_i = (src_y as usize * src_w as usize + src_x as usize) * 4;
            let dst_i = (y as usize * dst_w as usize + x as usize) * 4;
            out[dst_i..dst_i + 4].copy_from_slice(&pixels[src_i..src_i + 4]);
        }
    }
    out
}

/// Display-referred peak headroom used by the loader: values `<= 1.0` (plus epsilon) mean SDR or
/// SDR tone-mapped output where an eager full-frame SDR fallback is appropriate.
#[inline]
pub(crate) fn hdr_display_requests_sdr_preview(hdr_target_capacity: f32) -> bool {
    const MAX_SDR: f32 = 1.0;
    const EPS: f32 = 0.001;
    hdr_target_capacity <= MAX_SDR + EPS
}

/// RGBA8 SDR fallback bytes plus whether they are a cheap deferred placeholder buffer.
#[derive(Clone)]
pub(crate) struct HdrSdrFallbackRgba8 {
    pub pixels: Arc<Vec<u8>>,
    pub is_deferred_placeholder: bool,
}

pub(crate) fn cheap_hdr_sdr_placeholder_rgba8(width: u32, height: u32) -> Result<Vec<u8>, String> {
    crate::hdr::decode::validate_hdr_fallback_budget(width, height)?;
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(|| format!("HDR SDR placeholder dimension overflow: {width}x{height}"))?;
    let byte_len = pixels
        .checked_mul(4)
        .ok_or_else(|| format!("HDR SDR placeholder byte overflow: {width}x{height}"))?;
    let mut out = vec![0_u8; byte_len as usize];
    for px in out.chunks_exact_mut(4) {
        px[3] = 255;
    }
    Ok(out)
}

pub(crate) fn libraw_scene_linear_needs_eager_sdr_fallback(hdr: &HdrImageBuffer) -> bool {
    hdr.metadata.gain_map.is_none()
        && hdr.metadata.raw_gpu_source.is_none()
        && hdr.metadata.transfer_function == HdrTransferFunction::Linear
        && hdr.metadata.reference == HdrReference::SceneLinear
}

/// Embedded preview attached for GPU demosaic bootstrap (real pixels, not a black placeholder).
pub(crate) fn raw_gpu_source_has_bootstrap_preview(hdr: &HdrImageBuffer) -> bool {
    hdr.metadata
        .raw_gpu_source
        .as_ref()
        .and_then(|source| source.bootstrap_preview.as_ref())
        .is_some()
}

/// Clone embedded GPU-RAW bootstrap SDR for strip fallback paths.
pub(crate) fn hdr_raw_gpu_bootstrap_fallback_decoded(hdr: &HdrImageBuffer) -> Option<DecodedImage> {
    hdr.metadata
        .raw_gpu_source
        .as_ref()
        .and_then(|source| source.bootstrap_preview.clone())
}

/// Logical size stored with a strip thumbnail for aspect validation.
///
/// GPU RAW files may ship an embedded bootstrap whose aspect differs from HQ demosaic output
/// (e.g. Samsung EX2F bootstrap 4000x2248 vs HQ 4040x3029). Until float HDR pixels exist,
/// match the fallback/bootstrap aspect instead of forcing HQ logical.
pub(crate) fn directory_tree_strip_logical_for_preview(
    hdr_width: u32,
    hdr_height: u32,
    fallback_width: u32,
    fallback_height: u32,
    strip_width: u32,
    strip_height: u32,
    hdr_has_float_pixels: bool,
) -> (u32, u32) {
    if hdr_has_float_pixels
        && crate::loader::preview_aspect_matches_logical(
            strip_width,
            strip_height,
            hdr_width,
            hdr_height,
        )
    {
        (hdr_width, hdr_height)
    } else if crate::loader::preview_aspect_matches_logical(
        strip_width,
        strip_height,
        fallback_width,
        fallback_height,
    ) {
        (fallback_width, fallback_height)
    } else {
        (hdr_width, hdr_height)
    }
}

/// GPU CFA extract finished but demosaic has not yet populated `rgba_f32`.
pub(crate) fn hdr_raw_gpu_demosaic_pending(hdr: &HdrImageBuffer) -> bool {
    hdr.metadata.raw_gpu_source.is_some() && hdr.rgba_f32.is_empty()
}

/// Empty GPU RAW buffers cannot be tone-mapped on the refinement worker.
pub(crate) fn hdr_raw_gpu_refinement_is_pointless(hdr: &HdrImageBuffer) -> bool {
    hdr_raw_gpu_demosaic_pending(hdr)
}

/// Whether a loader worker should upload the static HDR float plane in the background.
///
/// GPU RAW sources need the HDR callback path (demosaic runs in `prepare()`). Background CFA
/// upload during preload avoids an SDR preview flash when demosaic is already complete, but only
/// when the main thread has an active HDR callback target format. Non-RAW images follow the same
/// HDR callback guard plus static render-plan eligibility.
pub(crate) fn static_hdr_background_plane_upload_eligible(
    hdr: &HdrImageBuffer,
    hdr_target_capacity: f32,
    hdr_callback_active: bool,
) -> bool {
    if hdr.metadata.raw_gpu_source.is_some() {
        return hdr_callback_active;
    }
    if !hdr_callback_active {
        return false;
    }
    let has_sdr_fallback = !hdr_sdr_fallback_is_placeholder_for_load(hdr, hdr_target_capacity);
    static_hdr_plane_preload_needs_upload(has_sdr_fallback, hdr_target_capacity)
}

fn static_hdr_plane_preload_needs_upload(has_sdr_fallback: bool, hdr_target_capacity: f32) -> bool {
    use crate::hdr::renderer::HdrRenderOutputMode;

    let output_mode = if hdr_display_requests_sdr_preview(hdr_target_capacity) {
        HdrRenderOutputMode::SdrToneMapped
    } else {
        HdrRenderOutputMode::NativeHdr
    };
    // Mirrors [`crate::app::rendering::plan::select_render_backend`] for static preload:
    // `has_hdr_plane` and `has_hdr_target` are true once the callback target format is active.
    if output_mode.is_native_hdr() {
        true
    } else if !has_sdr_fallback {
        true
    } else {
        output_mode == HdrRenderOutputMode::SdrToneMapped
    }
}

/// True when the loader attached a black SDR placeholder instead of a tone-mapped fallback.
pub(crate) fn hdr_sdr_fallback_is_placeholder_for_load(
    hdr: &HdrImageBuffer,
    hdr_target_capacity: f32,
) -> bool {
    if raw_gpu_source_has_bootstrap_preview(hdr) {
        return false;
    }
    if hdr_raw_gpu_demosaic_pending(hdr) {
        return true;
    }
    if hdr_display_requests_sdr_preview(hdr_target_capacity) {
        return false;
    }
    if libraw_scene_linear_needs_eager_sdr_fallback(hdr) {
        return false;
    }
    if hdr
        .metadata
        .gain_map
        .as_ref()
        .and_then(|g| g.iso_deferred.as_ref())
        .is_some()
    {
        return false;
    }
    true
}

pub(crate) fn hdr_sdr_fallback_rgba8_eager_or_placeholder(
    hdr: &HdrImageBuffer,
    hdr_target_capacity: f32,
    tone: &HdrToneMapSettings,
) -> Result<HdrSdrFallbackRgba8, String> {
    if let Some(source) = hdr.metadata.raw_gpu_source.as_ref() {
        if let Some(preview) = source.bootstrap_preview.as_ref() {
            return Ok(HdrSdrFallbackRgba8 {
                pixels: preview.arc_pixels(),
                is_deferred_placeholder: false,
            });
        }
        return Ok(HdrSdrFallbackRgba8 {
            pixels: Arc::new(cheap_hdr_sdr_placeholder_rgba8(hdr.width, hdr.height)?),
            is_deferred_placeholder: true,
        });
    }
    if let Some(gain_map) = hdr.metadata.gain_map.as_ref() {
        if let Some(iso) = gain_map.iso_deferred.as_ref() {
            // Share deferred baseline planes; avoid cloning multi‑MP RGBA on cold fallback paths.
            return Ok(HdrSdrFallbackRgba8 {
                pixels: Arc::clone(&iso.sdr_rgba),
                is_deferred_placeholder: false,
            });
        }
        if gain_map.apple_heic_deferred.is_some() {
            // rgba_f32 holds encoded primary for GPU compose, not display-ready scene-linear HDR.
            return Ok(HdrSdrFallbackRgba8 {
                pixels: Arc::new(cheap_hdr_sdr_placeholder_rgba8(hdr.width, hdr.height)?),
                is_deferred_placeholder: true,
            });
        }
    }
    if hdr_display_requests_sdr_preview(hdr_target_capacity)
        || libraw_scene_linear_needs_eager_sdr_fallback(hdr)
    {
        Ok(HdrSdrFallbackRgba8 {
            pixels: Arc::new(hdr_to_sdr_with_user_tone(hdr, tone)?),
            is_deferred_placeholder: false,
        })
    } else {
        Ok(HdrSdrFallbackRgba8 {
            pixels: Arc::new(cheap_hdr_sdr_placeholder_rgba8(hdr.width, hdr.height)?),
            is_deferred_placeholder: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::hdr::types::{
        AppleHeicGainMapGpuSource, HdrGainMapMetadata, HdrImageBuffer, HdrImageMetadata,
        HdrPixelFormat, IsoGainMapGpuSource,
    };

    #[test]
    fn sdr_fallback_uses_iso_deferred_baseline_not_rgba_f32() {
        let iso_sdr = vec![128_u8, 64, 32, 255];
        let mut metadata = HdrImageMetadata::default();
        metadata.gain_map = Some(HdrGainMapMetadata {
            source: "JPEG_R",
            target_hdr_capacity: Some(4.0),
            diagnostic: String::new(),
            capped_display_referred: false,
            apple_heic_deferred: None,
            iso_deferred: Some(IsoGainMapGpuSource {
                sdr_rgba: Arc::new(iso_sdr.clone()),
                gain_rgba: Arc::new(vec![0; 4]),
                gain_width: 1,
                gain_height: 1,
                metadata: crate::hdr::gain_map::GainMapMetadata {
                    gain_map_min: [0.0; 3],
                    gain_map_max: [1.0; 3],
                    gamma: [1.0; 3],
                    offset_sdr: [0.0; 3],
                    offset_hdr: [0.0; 3],
                    hdr_capacity_min: 1.0,
                    hdr_capacity_max: 4.0,
                    backward_direction: false,
                },
            }),
        });
        let hdr = HdrImageBuffer {
            width: 1,
            height: 1,
            format: HdrPixelFormat::Rgba32Float,
            color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
            metadata,
            rgba_f32: Arc::new(Vec::new()),
        };
        let out =
            hdr_sdr_fallback_rgba8_eager_or_placeholder(&hdr, 4.0, &HdrToneMapSettings::default())
                .expect("fallback");
        assert_eq!(out.pixels.as_slice(), iso_sdr);
        assert!(!out.is_deferred_placeholder);
    }

    #[test]
    fn directory_tree_strip_from_empty_hdr_uses_sdr_fallback() {
        use crate::loader::DecodedImage;

        let hdr = HdrImageBuffer {
            width: 4,
            height: 4,
            format: HdrPixelFormat::Rgba32Float,
            color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
            metadata: HdrImageMetadata::default(),
            rgba_f32: Arc::new(Vec::new()),
        };
        let fallback = DecodedImage::new(4, 4, vec![200_u8; 4 * 4 * 4]);
        let strip =
            directory_tree_strip_from_hdr_or_fallback(&hdr, &fallback, 128).expect("strip");
        assert_eq!(strip.width, 4);
        assert_eq!(strip.height, 4);
        assert_eq!(strip.rgba()[0], 200);
    }

    #[test]
    fn directory_tree_strip_iso_deferred_prefers_fallback_baseline() {
        use crate::loader::DecodedImage;
        use crate::hdr::types::IsoGainMapGpuSource;

        let iso_sdr = Arc::new(vec![180_u8; 4 * 4 * 4]);
        let mut metadata = HdrImageMetadata::default();
        metadata.gain_map = Some(HdrGainMapMetadata {
            source: "AVIF",
            target_hdr_capacity: Some(4.0),
            diagnostic: String::new(),
            capped_display_referred: false,
            apple_heic_deferred: None,
            iso_deferred: Some(IsoGainMapGpuSource {
                sdr_rgba: Arc::clone(&iso_sdr),
                gain_rgba: Arc::new(vec![0; 4]),
                gain_width: 1,
                gain_height: 1,
                metadata: crate::hdr::gain_map::GainMapMetadata {
                    gain_map_min: [0.0; 3],
                    gain_map_max: [1.0; 3],
                    gamma: [1.0; 3],
                    offset_sdr: [0.0; 3],
                    offset_hdr: [0.0; 3],
                    hdr_capacity_min: 1.0,
                    hdr_capacity_max: 4.0,
                    backward_direction: false,
                },
            }),
        });
        let hdr = HdrImageBuffer {
            width: 4,
            height: 4,
            format: HdrPixelFormat::Rgba32Float,
            color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
            metadata,
            rgba_f32: Arc::new(Vec::new()),
        };
        let fallback = DecodedImage::new(4, 4, iso_sdr.to_vec());
        let strip =
            directory_tree_strip_from_hdr_or_fallback(&hdr, &fallback, 128).expect("strip");
        assert_eq!(strip.rgba()[0], 180);
    }

    #[test]
    fn display_sdr_preview_composes_iso_deferred_baseline() {
        let iso_sdr = vec![32_u8, 32, 32, 255];
        let mut metadata = HdrImageMetadata::default();
        metadata.gain_map = Some(HdrGainMapMetadata {
            source: "JPEG_R",
            target_hdr_capacity: Some(4.0),
            diagnostic: String::new(),
            capped_display_referred: false,
            apple_heic_deferred: None,
            iso_deferred: Some(IsoGainMapGpuSource {
                sdr_rgba: Arc::new(iso_sdr.clone()),
                gain_rgba: Arc::new(vec![0; 4]),
                gain_width: 1,
                gain_height: 1,
                metadata: crate::hdr::gain_map::GainMapMetadata {
                    gain_map_min: [0.0; 3],
                    gain_map_max: [4.0; 3],
                    gamma: [1.0; 3],
                    offset_sdr: [0.0; 3],
                    offset_hdr: [0.0; 3],
                    hdr_capacity_min: 1.0,
                    hdr_capacity_max: 4.0,
                    backward_direction: false,
                },
            }),
        });
        let hdr = HdrImageBuffer {
            width: 1,
            height: 1,
            format: HdrPixelFormat::Rgba32Float,
            color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
            metadata,
            rgba_f32: Arc::new(Vec::new()),
        };
        let tone = HdrToneMapSettings::default();
        let baseline = hdr_to_sdr_with_user_tone(&hdr, &tone).expect("baseline");
        let preview = hdr_to_display_sdr_rgba8_for_preview(&hdr, &tone).expect("preview");
        assert_eq!(baseline, iso_sdr);
        assert_ne!(preview, iso_sdr);
    }

    #[test]
    fn sdr_fallback_with_iso_deferred_baseline_works_after_placeholder() {
        let iso_sdr = vec![64_u8, 128, 192, 255];
        let mut metadata = HdrImageMetadata::default();
        metadata.gain_map = Some(HdrGainMapMetadata {
            source: "JPEG_R",
            target_hdr_capacity: Some(4.0),
            diagnostic: String::new(),
            capped_display_referred: false,
            apple_heic_deferred: None,
            iso_deferred: Some(IsoGainMapGpuSource {
                sdr_rgba: Arc::new(iso_sdr.clone()),
                gain_rgba: Arc::new(vec![0; 4]),
                gain_width: 1,
                gain_height: 1,
                metadata: crate::hdr::gain_map::GainMapMetadata {
                    gain_map_min: [0.0; 3],
                    gain_map_max: [1.0; 3],
                    gamma: [1.0; 3],
                    offset_sdr: [0.0; 3],
                    offset_hdr: [0.0; 3],
                    hdr_capacity_min: 1.0,
                    hdr_capacity_max: 4.0,
                    backward_direction: false,
                },
            }),
        });
        let hdr = HdrImageBuffer {
            width: 1,
            height: 1,
            format: HdrPixelFormat::Rgba32Float,
            color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
            metadata,
            rgba_f32: Arc::new(Vec::new()),
        };

        let out = hdr_to_sdr_with_user_tone(&hdr, &HdrToneMapSettings::default())
            .expect("ISO deferred fallback should use baseline SDR");

        assert_eq!(out, iso_sdr);
    }

    #[test]
    fn gpu_raw_bootstrap_preview_is_not_sdr_placeholder() {
        let mut metadata = crate::raw_processor::raw_scene_linear_metadata();
        metadata.raw_gpu_source = Some(crate::hdr::types::RawGpuSource {
            raw_width: 4,
            raw_height: 4,
            width: 4,
            height: 4,
            raw_pixels: Arc::new(vec![0; 16]),
            black_level: [0.0; 4],
            cfa_scale: [1.0; 4],
            rgb_cam: [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            maximum: 65535.0,
            bayer_pattern: [0, 1, 1, 2],
            scene_color_scale: [1.0, 1.0, 1.0],
            demosaic_method: crate::settings::RawDemosaicMethod::Ppg,
            bootstrap_preview: Some(crate::loader::DecodedImage::new(2, 2, vec![1; 16])),
        });
        let hdr = HdrImageBuffer {
            width: 4,
            height: 4,
            format: HdrPixelFormat::Rgba32Float,
            color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
            metadata,
            rgba_f32: Arc::new(Vec::new()),
        };
        assert!(!hdr_sdr_fallback_is_placeholder_for_load(&hdr, 4.0));
        assert!(hdr_raw_gpu_demosaic_pending(&hdr));
        assert!(hdr_raw_gpu_refinement_is_pointless(&hdr));
    }

    #[test]
    fn libraw_scene_linear_load_is_not_sdr_placeholder_at_hdr_headroom() {
        let metadata = crate::raw_processor::raw_scene_linear_metadata();
        let hdr = HdrImageBuffer {
            width: 1,
            height: 1,
            format: HdrPixelFormat::Rgba32Float,
            color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
            metadata,
            rgba_f32: Arc::new(vec![2.0, 2.0, 2.0, 1.0]),
        };
        assert!(!hdr_sdr_fallback_is_placeholder_for_load(&hdr, 4.0));
    }

    #[test]
    fn sdr_fallback_tone_maps_libraw_scene_linear_instead_of_black_placeholder() {
        let metadata = crate::raw_processor::raw_scene_linear_metadata();
        let hdr = HdrImageBuffer {
            width: 1,
            height: 1,
            format: HdrPixelFormat::Rgba32Float,
            color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
            metadata,
            rgba_f32: Arc::new(vec![2.0, 2.0, 2.0, 1.0]),
        };
        let out =
            hdr_sdr_fallback_rgba8_eager_or_placeholder(&hdr, 4.0, &HdrToneMapSettings::default())
                .expect("fallback");
        let px = out.pixels.as_slice();
        assert!(
            px[0] > 0 || px[1] > 0 || px[2] > 0,
            "LibRaw scene-linear HDR must not use a black SDR placeholder at HDR headroom > 1"
        );
        assert!(!out.is_deferred_placeholder);
    }

    #[test]
    fn sdr_fallback_never_tone_maps_apple_deferred_encoded_primary() {
        let mut metadata = HdrImageMetadata::default();
        metadata.gain_map = Some(HdrGainMapMetadata {
            source: "HEIF",
            target_hdr_capacity: Some(4.0),
            diagnostic: String::new(),
            capped_display_referred: false,
            apple_heic_deferred: Some(AppleHeicGainMapGpuSource {
                gain_rgba: Arc::new(vec![128; 4]),
                gain_width: 1,
                gain_height: 1,
                headroom_span: 1.0,
                stops: 2.0,
            }),
            iso_deferred: None,
        });
        let hdr = HdrImageBuffer {
            width: 1,
            height: 1,
            format: HdrPixelFormat::Rgba32Float,
            color_space: crate::hdr::types::HdrColorSpace::DisplayP3Linear,
            metadata,
            rgba_f32: Arc::new(vec![10.0, 0.0, 0.0, 1.0]),
        };
        let out =
            hdr_sdr_fallback_rgba8_eager_or_placeholder(&hdr, 0.5, &HdrToneMapSettings::default())
                .expect("fallback");
        assert!(out.is_deferred_placeholder);
        assert_eq!(out.pixels.as_slice(), [0, 0, 0, 255]);
    }

    #[test]
    fn raw_gpu_source_requires_hdr_callback_for_background_plane_upload() {
        let mut metadata = HdrImageMetadata::default();
        metadata.raw_gpu_source = Some(crate::hdr::types::RawGpuSource {
            raw_width: 1,
            raw_height: 1,
            width: 1,
            height: 1,
            raw_pixels: Arc::new(vec![0]),
            black_level: [0.0; 4],
            cfa_scale: [1.0; 4],
            rgb_cam: [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            maximum: 1.0,
            bayer_pattern: [0, 1, 1, 2],
            scene_color_scale: [1.0, 1.0, 1.0],
            demosaic_method: crate::settings::RawDemosaicMethod::Ppg,
            bootstrap_preview: None,
        });
        let hdr = HdrImageBuffer {
            width: 1,
            height: 1,
            format: HdrPixelFormat::Rgba32Float,
            color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
            metadata,
            rgba_f32: Arc::new(Vec::new()),
        };

        assert!(!super::static_hdr_background_plane_upload_eligible(
            &hdr, 1.0, false
        ));
        assert!(super::static_hdr_background_plane_upload_eligible(
            &hdr, 1.0, true
        ));
    }

    #[test]
    fn non_raw_skips_background_upload_when_hdr_callback_inactive() {
        let hdr = HdrImageBuffer {
            width: 1,
            height: 1,
            format: HdrPixelFormat::Rgba32Float,
            color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
            metadata: HdrImageMetadata::default(),
            rgba_f32: Arc::new(vec![0.0; 4]),
        };

        assert!(!super::static_hdr_background_plane_upload_eligible(
            &hdr, 1.0, false
        ));
        assert!(super::static_hdr_background_plane_upload_eligible(
            &hdr, 1.0, true
        ));
    }

    #[test]
    fn strip_logical_prefers_bootstrap_aspect_before_hdr_float_ready() {
        let logical = super::directory_tree_strip_logical_for_preview(
            4040,
            3029,
            4000,
            2248,
            128,
            72,
            false,
        );
        assert_eq!(logical, (4000, 2248));
    }

    #[test]
    fn strip_logical_uses_hdr_when_float_pixels_match() {
        let logical = super::directory_tree_strip_logical_for_preview(
            4040,
            3029,
            4000,
            2248,
            128,
            96,
            true,
        );
        assert_eq!(logical, (4040, 3029));
    }

    #[test]
    fn hdr_raw_gpu_bootstrap_fallback_decoded_clones_embedded_preview() {
        let preview = DecodedImage::new(4000, 2248, vec![128; 4000 * 2248 * 4]);
        let mut metadata = crate::raw_processor::raw_scene_linear_metadata();
        metadata.raw_gpu_source = Some(crate::hdr::types::RawGpuSource {
            raw_width: 4040,
            raw_height: 3029,
            width: 4040,
            height: 3029,
            raw_pixels: Arc::new(vec![0; 16]),
            black_level: [0.0; 4],
            cfa_scale: [1.0; 4],
            rgb_cam: [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            maximum: 65535.0,
            bayer_pattern: [0, 1, 1, 2],
            scene_color_scale: [1.0, 1.0, 1.0],
            demosaic_method: crate::settings::RawDemosaicMethod::Ppg,
            bootstrap_preview: Some(preview.clone()),
        });
        let hdr = HdrImageBuffer {
            width: 4040,
            height: 3029,
            format: HdrPixelFormat::Rgba32Float,
            color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
            metadata,
            rgba_f32: Arc::new(Vec::new()),
        };
        let cloned = super::hdr_raw_gpu_bootstrap_fallback_decoded(&hdr).expect("bootstrap");
        assert_eq!(cloned.width, preview.width);
        assert_eq!(cloned.height, preview.height);
        assert!(!cloned.is_sdr_deferred_placeholder());
    }
}
