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

use crate::hdr::types::{DEFAULT_SDR_WHITE_NITS, HdrImageBuffer, HdrToneMapSettings};
use crate::loader::DecodedImage;

/// True when the HDR buffer carries a deferred ISO gain map (Ultra HDR / AVIF JPEG-R, etc.).
pub(crate) fn hdr_has_iso_deferred_gain_map(hdr: &HdrImageBuffer) -> bool {
    hdr.metadata
        .gain_map
        .as_ref()
        .and_then(|gain_map| gain_map.iso_deferred.as_ref())
        .is_some()
}

/// True when cached HDR state depends on [`crate::settings::HdrGainMapSdrDisplayMode`].
pub(crate) fn hdr_is_gain_map_sdr_display_sensitive(hdr: &HdrImageBuffer) -> bool {
    if hdr_has_embedded_sdr_master_display(hdr) {
        return true;
    }
    let Some(gain_map) = hdr.metadata.gain_map.as_ref() else {
        return false;
    };
    if gain_map.apple_heic_deferred.is_some() {
        return true;
    }
    if gain_map
        .iso_deferred
        .as_ref()
        .is_some_and(|iso| !iso_deferred_is_embedded_sdr_master_only(iso))
    {
        return true;
    }
    // HEIC tone-map on SDR: full primary float plane + auxiliary gain-map metadata, but Apple
    // compose skipped at capacity 1.0 (see log: decode_heif_hdr_bytes without apple_heic_deferred).
    !hdr.rgba_f32.is_empty()
}

/// Whether changing [`crate::settings::HdrGainMapSdrDisplayMode`] should evict and reload this image.
pub(crate) fn hdr_gain_map_sdr_display_mode_affects_image(
    hdr: &HdrImageBuffer,
    path: &std::path::Path,
) -> bool {
    if hdr_is_gain_map_sdr_display_sensitive(hdr) {
        return true;
    }
    // Nokia-style HEIF stills: 8-bit primary with no gain-map auxiliary in metadata still toggle
    // between embedded SDR master (8-bit primary) and GPU tone-mapped float plane on SDR output.
    !hdr.rgba_f32.is_empty() && heif_extension_path(path)
}

fn heif_extension_path(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            ext.eq_ignore_ascii_case("heic")
                || ext.eq_ignore_ascii_case("heif")
                || ext.eq_ignore_ascii_case("hif")
        })
}

/// ISO deferred planes from [`crate::hdr::jpeg_gain_map_gpu::attach_iso_embedded_sdr_master_only`]
/// carry no gain-map texels; full HDR decode + compose uses non-zero gain dimensions.
fn iso_deferred_is_embedded_sdr_master_only(iso: &crate::hdr::types::IsoGainMapGpuSource) -> bool {
    iso.gain_width == 0 && iso.gain_height == 0 && iso.gain_rgba.is_empty()
}

/// True when the HDR buffer represents gain-map HDR shown via embedded SDR master (no float plane).
pub(crate) fn hdr_has_embedded_sdr_master_display(hdr: &HdrImageBuffer) -> bool {
    if !hdr.rgba_f32.is_empty() {
        return false;
    }
    hdr.metadata.gain_map.as_ref().is_some_and(|gain_map| {
        if gain_map.is_heif_embedded_sdr_primary_only() {
            return true;
        }
        gain_map
            .iso_deferred
            .as_ref()
            .is_some_and(iso_deferred_is_embedded_sdr_master_only)
    })
}

/// True when ISO gain-map HDR should show the embedded SDR master on an SDR output path
/// instead of routing through the HDR float plane for compose + tone-map.
pub(crate) fn prefer_embedded_iso_gain_map_sdr_on_sdr_output(
    settings: &crate::settings::Settings,
    output_mode: crate::hdr::renderer::HdrRenderOutputMode,
    hdr: Option<&HdrImageBuffer>,
) -> bool {
    if output_mode != crate::hdr::renderer::HdrRenderOutputMode::SdrToneMapped {
        return false;
    }
    if settings.hdr_gain_map_sdr_display
        != crate::settings::HdrGainMapSdrDisplayMode::EmbeddedSdrMaster
    {
        return false;
    }
    hdr.is_some_and(hdr_has_embedded_sdr_master_display)
}

#[inline]
pub(crate) fn hdr_to_sdr_with_user_tone(
    buffer: &HdrImageBuffer,
    tone: &HdrToneMapSettings,
) -> Result<Vec<u8>, String> {
    if let Some(gain_map) = buffer.metadata.gain_map.as_ref()
        && let Some(iso) = gain_map.iso_deferred.as_ref()
    {
        return Ok((*iso.sdr_rgba).clone());
    }
    crate::hdr::decode::hdr_to_sdr_rgba8_with_tone_settings(buffer, tone.exposure_ev, tone)
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
    if !buffer.rgba_f32.is_empty() {
        return crate::hdr::decode::hdr_to_sdr_rgba8_strip_preview(buffer, tone.exposure_ev, &tone);
    }
    hdr_to_sdr_with_user_tone(buffer, &tone)
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
    let preview = crate::hdr::tiled::downsample_hdr_image_nearest(buffer, max_side, max_side)?;
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

    if !hdr.rgba_f32.is_empty()
        && let Ok((width, height, pixels)) = hdr_directory_tree_strip_sdr_at_max_side(hdr, max_side)
    {
        return Ok(crate::loader::DecodedImage::new(width, height, pixels));
    }

    // ISO deferred: strip thumbnails use the embedded SDR baseline only (no gain-map compose).
    if hdr_has_iso_deferred_gain_map(hdr) {
        if let Some(iso) = hdr
            .metadata
            .gain_map
            .as_ref()
            .and_then(|gain_map| gain_map.iso_deferred.as_ref())
        {
            let decoded = crate::loader::DecodedImage::from_arc(
                hdr.width,
                hdr.height,
                Arc::clone(&iso.sdr_rgba),
            );
            return downsample_decoded_for_strip(&decoded, max_side);
        }
    }

    if !fallback.is_sdr_deferred_placeholder() {
        return downsample_decoded_for_strip(fallback, max_side);
    }

    Err("strip preview unavailable: no HDR pixels or SDR fallback".to_string())
}

/// Display-referred peak headroom used by the loader: values `<= 1.0` (plus epsilon) mean SDR or
/// SDR tone-mapped output where the HDR float plane is tone-mapped on GPU at display time.
#[inline]
pub(crate) fn hdr_display_requests_sdr_preview(hdr_target_capacity: f32) -> bool {
    const MAX_SDR: f32 = 1.0;
    const EPS: f32 = 0.001;
    hdr_target_capacity <= MAX_SDR + EPS
}

/// True when the main loader should skip HDR float-plane decode and use embedded SDR master only.
pub(crate) fn should_use_embedded_sdr_master_load(
    prefer_embedded_sdr_master: bool,
    hdr_target_capacity: f32,
) -> bool {
    prefer_embedded_sdr_master && hdr_display_requests_sdr_preview(hdr_target_capacity)
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

pub(crate) fn hdr_float_plane_defers_sdr_tone_map_to_gpu(hdr: &HdrImageBuffer) -> bool {
    !hdr.rgba_f32.is_empty()
        || hdr
            .metadata
            .gain_map
            .as_ref()
            .is_some_and(|g| g.apple_heic_deferred.is_some())
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

/// Whether a loader worker should upload the static HDR float plane in the background.
///
/// GPU RAW sources need the HDR callback path (demosaic runs in `prepare()`). Background CFA
/// upload during preload avoids an SDR preview flash when demosaic is already complete, but only
/// when the main thread has an active HDR callback target format. Non-RAW images follow the same
/// HDR callback guard plus static render-plan eligibility.
///
/// Embedded ISO gain-map SDR master on an SDR monitor never draws the HDR float plane; skip GPU
/// pre-upload so loader work stays on the SDR texture path.
pub(crate) fn static_hdr_background_plane_upload_eligible(
    hdr: &HdrImageBuffer,
    hdr_target_capacity: f32,
    hdr_callback_active: bool,
    embedded_iso_gain_map_sdr_master: bool,
) -> bool {
    if hdr.metadata.raw_gpu_source.is_some() {
        return hdr_callback_active;
    }
    if embedded_iso_gain_map_sdr_master
        && hdr_display_requests_sdr_preview(hdr_target_capacity)
        && hdr_has_embedded_sdr_master_display(hdr)
    {
        return false;
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
    if output_mode.is_native_hdr() || !has_sdr_fallback {
        true
    } else {
        output_mode == HdrRenderOutputMode::SdrToneMapped
    }
}

/// True when the loader attached a black SDR placeholder instead of display-ready fallback pixels.
///
/// Embedded SDR master loads (HEIF primary-only, ISO gain-map baseline, etc.) ship real SDR bytes in
/// the fallback even when `rgba_f32` is empty; those are not placeholders.
pub(crate) fn hdr_sdr_fallback_is_placeholder_for_load(
    hdr: &HdrImageBuffer,
    hdr_target_capacity: f32,
) -> bool {
    let _ = hdr_target_capacity;
    if raw_gpu_source_has_bootstrap_preview(hdr) {
        return false;
    }
    if hdr_raw_gpu_demosaic_pending(hdr) {
        return true;
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
    if hdr_has_embedded_sdr_master_display(hdr) {
        return false;
    }
    hdr_float_plane_defers_sdr_tone_map_to_gpu(hdr) || hdr.rgba_f32.is_empty()
}

/// SDR fallback RGBA8: embedded ISO baseline, RAW bootstrap preview, or deferred black placeholder.
pub(crate) fn hdr_sdr_fallback_rgba8_or_placeholder(
    hdr: &HdrImageBuffer,
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
    Ok(HdrSdrFallbackRgba8 {
        pixels: Arc::new(cheap_hdr_sdr_placeholder_rgba8(hdr.width, hdr.height)?),
        is_deferred_placeholder: true,
    })
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
        let out = hdr_sdr_fallback_rgba8_or_placeholder(&hdr).expect("fallback");
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
        let strip = directory_tree_strip_from_hdr_or_fallback(&hdr, &fallback, 128).expect("strip");
        assert_eq!(strip.width, 4);
        assert_eq!(strip.height, 4);
        assert_eq!(strip.rgba()[0], 200);
    }

    #[test]
    fn directory_tree_strip_iso_deferred_uses_baseline_not_compose() {
        use crate::hdr::types::IsoGainMapGpuSource;
        use crate::loader::DecodedImage;

        let iso_sdr = Arc::new(vec![32_u8; 4 * 4 * 4]);
        let mut metadata = HdrImageMetadata::default();
        metadata.gain_map = Some(HdrGainMapMetadata {
            source: "AVIF",
            target_hdr_capacity: Some(4.0),
            diagnostic: String::new(),
            capped_display_referred: false,
            apple_heic_deferred: None,
            iso_deferred: Some(IsoGainMapGpuSource {
                sdr_rgba: Arc::clone(&iso_sdr),
                gain_rgba: Arc::new(vec![255_u8; 4]),
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
        let strip = directory_tree_strip_from_hdr_or_fallback(&hdr, &fallback, 128).expect("strip");
        assert_eq!(strip.rgba()[0], 32);
    }

    #[test]
    fn directory_tree_strip_iso_deferred_output_is_strip_sized() {
        use crate::hdr::types::IsoGainMapGpuSource;
        use crate::loader::DecodedImage;

        let width = 400_u32;
        let height = 300_u32;
        let pixel_count = width as usize * height as usize * 4;
        let iso_sdr = Arc::new(vec![64_u8; pixel_count]);
        let iso_gain = Arc::new(vec![200_u8; pixel_count]);
        let mut metadata = HdrImageMetadata::default();
        metadata.gain_map = Some(HdrGainMapMetadata {
            source: "AVIF",
            target_hdr_capacity: Some(4.0),
            diagnostic: String::new(),
            capped_display_referred: false,
            apple_heic_deferred: None,
            iso_deferred: Some(IsoGainMapGpuSource {
                sdr_rgba: Arc::clone(&iso_sdr),
                gain_rgba: iso_gain,
                gain_width: width,
                gain_height: height,
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
            width,
            height,
            format: HdrPixelFormat::Rgba32Float,
            color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
            metadata,
            rgba_f32: Arc::new(Vec::new()),
        };
        let fallback = DecodedImage::new(width, height, iso_sdr.to_vec());
        let strip = directory_tree_strip_from_hdr_or_fallback(&hdr, &fallback, 128).expect("strip");
        assert!(strip.width <= 128, "strip width should be downsampled");
        assert!(strip.height <= 128, "strip height should be downsampled");
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
    }

    #[test]
    fn libraw_scene_linear_load_uses_gpu_tone_map_placeholder() {
        let metadata = crate::raw_processor::raw_scene_linear_metadata();
        let hdr = HdrImageBuffer {
            width: 1,
            height: 1,
            format: HdrPixelFormat::Rgba32Float,
            color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
            metadata,
            rgba_f32: Arc::new(vec![2.0, 2.0, 2.0, 1.0]),
        };
        assert!(hdr_sdr_fallback_is_placeholder_for_load(&hdr, 4.0));
        assert!(hdr_sdr_fallback_is_placeholder_for_load(&hdr, 1.0));
    }

    #[test]
    fn sdr_fallback_defers_libraw_scene_linear_tone_map_to_gpu() {
        let metadata = crate::raw_processor::raw_scene_linear_metadata();
        let hdr = HdrImageBuffer {
            width: 1,
            height: 1,
            format: HdrPixelFormat::Rgba32Float,
            color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
            metadata,
            rgba_f32: Arc::new(vec![2.0, 2.0, 2.0, 1.0]),
        };
        let out = hdr_sdr_fallback_rgba8_or_placeholder(&hdr).expect("fallback");
        assert!(out.is_deferred_placeholder);
        assert_eq!(out.pixels.as_slice(), [0, 0, 0, 255]);
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
        let out = hdr_sdr_fallback_rgba8_or_placeholder(&hdr).expect("fallback");
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
            &hdr, 1.0, false, false
        ));
        assert!(super::static_hdr_background_plane_upload_eligible(
            &hdr, 1.0, true, false
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
            &hdr, 1.0, false, false
        ));
        assert!(super::static_hdr_background_plane_upload_eligible(
            &hdr, 1.0, true, false
        ));
    }

    #[test]
    fn embedded_iso_gain_map_sdr_master_skips_non_raw_background_upload() {
        let mut metadata = HdrImageMetadata::default();
        metadata.gain_map = Some(HdrGainMapMetadata {
            source: "test",
            target_hdr_capacity: None,
            diagnostic: String::new(),
            capped_display_referred: false,
            apple_heic_deferred: None,
            iso_deferred: Some(crate::hdr::types::IsoGainMapGpuSource {
                sdr_rgba: Arc::new(vec![128; 4]),
                gain_rgba: Arc::new(Vec::new()),
                gain_width: 0,
                gain_height: 0,
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

        assert!(!super::static_hdr_background_plane_upload_eligible(
            &hdr, 1.0, true, true
        ));
        assert!(super::static_hdr_background_plane_upload_eligible(
            &hdr, 1.0, true, false
        ));
    }

    #[test]
    fn full_iso_deferred_is_not_embedded_sdr_master_display() {
        let mut metadata = HdrImageMetadata::default();
        metadata.gain_map = Some(HdrGainMapMetadata {
            source: "AVIF",
            target_hdr_capacity: Some(4.0),
            diagnostic: String::new(),
            capped_display_referred: false,
            apple_heic_deferred: None,
            iso_deferred: Some(crate::hdr::types::IsoGainMapGpuSource {
                sdr_rgba: Arc::new(vec![128; 4]),
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

        assert!(!super::hdr_has_embedded_sdr_master_display(&hdr));
        assert!(super::hdr_is_gain_map_sdr_display_sensitive(&hdr));
    }

    #[test]
    fn heif_float_plane_without_gain_map_aux_is_sdr_display_toggleable() {
        let hdr = HdrImageBuffer {
            width: 1440,
            height: 960,
            format: HdrPixelFormat::Rgba32Float,
            color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
            metadata: HdrImageMetadata::default(),
            rgba_f32: Arc::new(vec![0.5; 1440 * 960 * 4]),
        };
        let path = std::path::Path::new("autumn.heic");

        assert!(!super::hdr_is_gain_map_sdr_display_sensitive(&hdr));
        assert!(super::hdr_gain_map_sdr_display_mode_affects_image(
            &hdr, path
        ));
    }

    #[test]
    fn heif_tone_mapped_primary_float_is_gain_map_sdr_display_sensitive() {
        let mut metadata = HdrImageMetadata::default();
        metadata.gain_map = Some(HdrGainMapMetadata {
            source: "HEIF",
            target_hdr_capacity: None,
            diagnostic: "#63 urn:com:apple:photo:2020:aux:hdrgainmap (AppleHdrGainMap)".to_string(),
            capped_display_referred: false,
            apple_heic_deferred: None,
            iso_deferred: None,
        });
        let hdr = HdrImageBuffer {
            width: 4032,
            height: 3024,
            format: HdrPixelFormat::Rgba32Float,
            color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
            metadata,
            rgba_f32: Arc::new(vec![0.5; 4032 * 3024 * 4]),
        };

        assert!(!super::hdr_has_embedded_sdr_master_display(&hdr));
        assert!(super::hdr_is_gain_map_sdr_display_sensitive(&hdr));
    }

    #[test]
    fn apple_heic_deferred_is_gain_map_sdr_display_sensitive() {
        let mut metadata = HdrImageMetadata::default();
        metadata.gain_map = Some(HdrGainMapMetadata {
            source: "HEIF",
            target_hdr_capacity: Some(2.0),
            diagnostic: String::new(),
            capped_display_referred: false,
            apple_heic_deferred: Some(crate::hdr::types::AppleHeicGainMapGpuSource {
                gain_rgba: Arc::new(vec![0; 4]),
                gain_width: 1,
                gain_height: 1,
                headroom_span: 1.0,
                stops: 1.0,
            }),
            iso_deferred: None,
        });
        let hdr = HdrImageBuffer {
            width: 4032,
            height: 3024,
            format: HdrPixelFormat::Rgba32Float,
            color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
            metadata,
            rgba_f32: Arc::new(vec![0.5; 4032 * 3024 * 4]),
        };

        assert!(!super::hdr_has_embedded_sdr_master_display(&hdr));
        assert!(super::hdr_is_gain_map_sdr_display_sensitive(&hdr));
    }

    #[test]
    fn heif_embedded_sdr_primary_is_embedded_sdr_master_display() {
        use crate::hdr::types::HEIF_EMBEDDED_SDR_PRIMARY_GAIN_MAP_SOURCE;

        let mut metadata = HdrImageMetadata::default();
        metadata.gain_map = Some(HdrGainMapMetadata {
            source: HEIF_EMBEDDED_SDR_PRIMARY_GAIN_MAP_SOURCE,
            target_hdr_capacity: None,
            diagnostic: "test".to_string(),
            capped_display_referred: true,
            apple_heic_deferred: None,
            iso_deferred: None,
        });
        let hdr = HdrImageBuffer {
            width: 6000,
            height: 4500,
            format: HdrPixelFormat::Rgba32Float,
            color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
            metadata,
            rgba_f32: Arc::new(Vec::new()),
        };

        assert!(super::hdr_has_embedded_sdr_master_display(&hdr));
        assert!(!hdr_sdr_fallback_is_placeholder_for_load(&hdr, 1.0));
        assert!(super::prefer_embedded_iso_gain_map_sdr_on_sdr_output(
            &crate::settings::Settings::default(),
            crate::hdr::renderer::HdrRenderOutputMode::SdrToneMapped,
            Some(&hdr),
        ));
        assert!(!super::static_hdr_background_plane_upload_eligible(
            &hdr, 1.0, true, true
        ));
    }

    #[test]
    fn strip_logical_prefers_bootstrap_aspect_before_hdr_float_ready() {
        let logical =
            super::directory_tree_strip_logical_for_preview(4040, 3029, 4000, 2248, 128, 72, false);
        assert_eq!(logical, (4000, 2248));
    }

    #[test]
    fn strip_logical_uses_hdr_when_float_pixels_match() {
        let logical =
            super::directory_tree_strip_logical_for_preview(4040, 3029, 4000, 2248, 128, 96, true);
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
