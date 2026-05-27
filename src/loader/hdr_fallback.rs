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

use crate::hdr::types::{HdrImageBuffer, HdrToneMapSettings};

#[inline]
pub(crate) fn hdr_to_sdr_with_user_tone(
    buffer: &HdrImageBuffer,
    tone: &HdrToneMapSettings,
) -> Result<Vec<u8>, String> {
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

pub(crate) fn hdr_sdr_fallback_rgba8_eager_or_placeholder(
    hdr: &HdrImageBuffer,
    hdr_target_capacity: f32,
    tone: &HdrToneMapSettings,
) -> Result<Arc<Vec<u8>>, String> {
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
    if hdr_display_requests_sdr_preview(hdr_target_capacity) {
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
}
