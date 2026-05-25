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

//! GPU-side Apple HEIC gain-map compose (`textureLoad` 4-tap, BT.709 then bilinear).
//!
//! Wired from [`HdrGainMapMetadata::apple_heic_deferred`] in the HDR image-plane shader.
//! The encoded primary stays in [`HdrImageBuffer::rgba_f32`] and is uploaded as
//! `Rgba32Float`; only the gain map uses `Rgba8Unorm`.

use crate::hdr::heif_apple_gain_map::apple_gain_map_display_weight;
use crate::hdr::types::{
    AppleHeicGainMapGpuSource, HdrGainMapMetadata, HdrImageBuffer, HdrImageMetadata,
};
use std::sync::Arc;

/// Build deferred GPU planes from a pre-compose primary buffer and decoded gain-map RGBA8.
pub(crate) fn attach_apple_heic_gpu_deferred(
    hdr: HdrImageBuffer,
    gain_w: u32,
    gain_h: u32,
    gain_rgba: Vec<u8>,
    headroom_span: f32,
    stops: f32,
    hdr_target_capacity: f32,
) -> HdrImageBuffer {
    let pixel_count = hdr.width as usize * hdr.height as usize * 4;
    debug_assert_eq!(hdr.rgba_f32.len(), pixel_count);
    debug_assert_eq!(gain_rgba.len(), gain_w as usize * gain_h as usize * 4);

    let gain_rgba = Arc::new(gain_rgba);
    let weight = apple_gain_map_display_weight(hdr_target_capacity, stops);

    let mut metadata = hdr.metadata.clone();
    metadata.gain_map = Some(HdrGainMapMetadata {
        source: "HEIF",
        target_hdr_capacity: Some(hdr_target_capacity),
        diagnostic: format!(
            "Apple HDR Gain Map GPU deferred ({}x{} pixels, stops: {:.2}, weight: {:.2})",
            gain_w, gain_h, stops, weight
        ),
        capped_display_referred: false,
        apple_heic_deferred: Some(AppleHeicGainMapGpuSource {
            gain_rgba: Arc::clone(&gain_rgba),
            gain_width: gain_w,
            gain_height: gain_h,
            headroom_span,
            stops,
        }),
    });

    HdrImageBuffer { metadata, ..hdr }
}

pub(crate) fn apple_heic_deferred_from_metadata(
    metadata: &HdrImageMetadata,
) -> Option<&AppleHeicGainMapGpuSource> {
    metadata
        .gain_map
        .as_ref()
        .and_then(|gm| gm.apple_heic_deferred.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hdr::types::{HdrColorSpace, HdrPixelFormat, HdrTransferFunction};

    #[test]
    fn attach_deferred_populates_gain_map_metadata() {
        let hdr = HdrImageBuffer {
            width: 2,
            height: 2,
            format: HdrPixelFormat::Rgba32Float,
            color_space: HdrColorSpace::DisplayP3Linear,
            metadata: HdrImageMetadata {
                transfer_function: HdrTransferFunction::Srgb,
                ..Default::default()
            },
            rgba_f32: Arc::new(vec![
                0.5, 0.25, 0.125, 1.0, //
                0.0, 0.0, 0.0, 1.0, //
                1.0, 1.0, 1.0, 1.0, //
                0.25, 0.5, 0.75, 1.0,
            ]),
        };
        let gain = vec![128u8; 2 * 2 * 4];
        let out = attach_apple_heic_gpu_deferred(hdr, 2, 2, gain, 1.0, 2.0, 4.0);
        let deferred = apple_heic_deferred_from_metadata(&out.metadata).expect("deferred");
        assert_eq!(deferred.gain_width, 2);
        assert_eq!(deferred.gain_rgba.len(), 2 * 2 * 4);
        assert_eq!(out.rgba_f32.len(), 2 * 2 * 4);
    }
}
