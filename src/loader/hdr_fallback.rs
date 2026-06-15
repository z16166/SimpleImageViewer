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

use crate::hdr::types::{HdrImageBuffer, HdrReference, HdrToneMapSettings, HdrTransferFunction};

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

/// Display-referred peak headroom used by the loader: values `<= 1.0` (plus epsilon) mean SDR or
/// SDR tone-mapped output where an eager full-frame SDR fallback is appropriate.
#[inline]
pub(crate) fn hdr_display_requests_sdr_preview(hdr_target_capacity: f32) -> bool {
    const MAX_SDR: f32 = 1.0;
    const EPS: f32 = 0.001;
    hdr_target_capacity <= MAX_SDR + EPS
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
) -> Result<Arc<Vec<u8>>, String> {
    if let Some(source) = hdr.metadata.raw_gpu_source.as_ref() {
        if let Some(preview) = source.bootstrap_preview.as_ref() {
            return Ok(preview.arc_pixels());
        }
        return Ok(Arc::new(cheap_hdr_sdr_placeholder_rgba8(
            hdr.width, hdr.height,
        )?));
    }
    if let Some(gain_map) = hdr.metadata.gain_map.as_ref() {
        if let Some(iso) = gain_map.iso_deferred.as_ref() {
            // Share deferred baseline planes; avoid cloning multi‑MP RGBA on cold fallback paths.
            return Ok(Arc::clone(&iso.sdr_rgba));
        }
        if gain_map.apple_heic_deferred.is_some() {
            // rgba_f32 holds encoded primary for GPU compose, not display-ready scene-linear HDR.
            return Ok(Arc::new(cheap_hdr_sdr_placeholder_rgba8(
                hdr.width, hdr.height,
            )?));
        }
    }
    if hdr_display_requests_sdr_preview(hdr_target_capacity)
        || libraw_scene_linear_needs_eager_sdr_fallback(hdr)
    {
        Ok(Arc::new(hdr_to_sdr_with_user_tone(hdr, tone)?))
    } else {
        Ok(Arc::new(cheap_hdr_sdr_placeholder_rgba8(
            hdr.width, hdr.height,
        )?))
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
        assert_eq!(out.as_slice(), iso_sdr);
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
        assert!(
            out[0] > 0 || out[1] > 0 || out[2] > 0,
            "LibRaw scene-linear HDR must not use a black SDR placeholder at HDR headroom > 1"
        );
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
        assert_eq!(out.as_slice(), [0, 0, 0, 255]);
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
}
