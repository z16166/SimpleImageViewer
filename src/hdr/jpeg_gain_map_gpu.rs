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

//! Ultra HDR JPEG deferred GPU planes (baseline SDR + gain map, compose at display time).

use std::sync::Arc;

use crate::hdr::gain_map::{
    GainMapMetadata, gain_map_metadata_diagnostic, gain_map_weight, luminance_hints_from_gain_map,
};
use crate::hdr::types::{
    HdrColorSpace, HdrGainMapMetadata, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat,
    HdrReference, HdrTransferFunction, JpegGainMapGpuSource,
};

pub(crate) fn attach_jpeg_gain_map_gpu_deferred(
    width: u32,
    height: u32,
    sdr_rgba: Vec<u8>,
    gain_width: u32,
    gain_height: u32,
    gain_rgba: Vec<u8>,
    metadata: GainMapMetadata,
    hdr_target_capacity: f32,
) -> HdrImageBuffer {
    let sdr_rgba = Arc::new(sdr_rgba);
    let gain_rgba = Arc::new(gain_rgba);
    let weight = gain_map_weight(metadata, hdr_target_capacity);
    let mut image_metadata = HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb);
    image_metadata.transfer_function = HdrTransferFunction::Srgb;
    image_metadata.reference = HdrReference::SdrGainMapBase;
    image_metadata.luminance = luminance_hints_from_gain_map(metadata);
    image_metadata.gain_map = Some(HdrGainMapMetadata {
        source: "JPEG_R",
        target_hdr_capacity: Some(hdr_target_capacity),
        diagnostic: format!(
            "Ultra HDR JPEG GPU deferred ({}x{} gain {}x{} weight: {:.3}): {}",
            width,
            height,
            gain_width,
            gain_height,
            weight,
            gain_map_metadata_diagnostic(metadata, hdr_target_capacity)
        ),
        capped_display_referred: false,
        apple_heic_deferred: None,
        jpeg_deferred: Some(JpegGainMapGpuSource {
            sdr_rgba: Arc::clone(&sdr_rgba),
            gain_rgba: Arc::clone(&gain_rgba),
            gain_width,
            gain_height,
            metadata,
        }),
    });

    HdrImageBuffer {
        width,
        height,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::LinearSrgb,
        metadata: image_metadata,
        rgba_f32: Arc::new(Vec::new()),
    }
}

pub(crate) fn apply_orientation_to_jpeg_deferred_hdr_buffer(
    mut buffer: HdrImageBuffer,
    orientation: u16,
) -> HdrImageBuffer {
    if orientation <= 1 {
        return buffer;
    }

    let Some(gain_map) = buffer.metadata.gain_map.as_mut() else {
        return buffer;
    };
    let Some(deferred) = gain_map.jpeg_deferred.as_mut() else {
        return buffer;
    };

    let (out_w, out_h, sdr) = crate::libtiff_loader::apply_orientation_buffer(
        Arc::try_unwrap(Arc::clone(&deferred.sdr_rgba)).unwrap_or_else(|arc| (*arc).clone()),
        buffer.width,
        buffer.height,
        orientation,
    );
    let (gain_w, gain_h, gain) = crate::libtiff_loader::apply_orientation_buffer(
        Arc::try_unwrap(Arc::clone(&deferred.gain_rgba)).unwrap_or_else(|arc| (*arc).clone()),
        deferred.gain_width,
        deferred.gain_height,
        orientation,
    );

    buffer.width = out_w;
    buffer.height = out_h;
    deferred.sdr_rgba = Arc::new(sdr);
    deferred.gain_rgba = Arc::new(gain);
    deferred.gain_width = gain_w;
    deferred.gain_height = gain_h;
    buffer
}

pub(crate) fn jpeg_deferred_from_metadata(
    metadata: &HdrImageMetadata,
) -> Option<&JpegGainMapGpuSource> {
    metadata
        .gain_map
        .as_ref()
        .and_then(|gain_map| gain_map.jpeg_deferred.as_ref())
}
