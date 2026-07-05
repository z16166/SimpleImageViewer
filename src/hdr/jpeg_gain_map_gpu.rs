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

//! ISO 21496 / Ultra HDR deferred GPU planes (baseline SDR + gain map, compose at display time).

use std::sync::Arc;

use crate::hdr::gain_map::{
    GainMapMetadata, gain_map_metadata_diagnostic, gain_map_weight, luminance_hints_from_gain_map,
    primary_srgb_rgba8_to_linear_rgba_f32, validate_iso_deferred_planes,
};
use crate::hdr::types::{
    HdrColorSpace, HdrGainMapMetadata, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat,
    HdrReference, HdrTransferFunction, IsoGainMapGpuSource,
};

pub(crate) struct IsoGainMapDeferredInput {
    pub(crate) source: &'static str,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) sdr_rgba: Vec<u8>,
    pub(crate) gain_width: u32,
    pub(crate) gain_height: u32,
    pub(crate) gain_rgba: Vec<u8>,
    pub(crate) metadata: GainMapMetadata,
    pub(crate) hdr_target_capacity: f32,
}

pub(crate) fn attach_iso_gain_map_gpu_deferred(
    input: IsoGainMapDeferredInput,
) -> Result<HdrImageBuffer, String> {
    let IsoGainMapDeferredInput {
        source,
        width,
        height,
        sdr_rgba,
        gain_width,
        gain_height,
        gain_rgba,
        metadata,
        hdr_target_capacity,
    } = input;
    validate_iso_deferred_planes(
        width,
        height,
        &sdr_rgba,
        gain_width,
        gain_height,
        &gain_rgba,
    )?;
    if metadata.backward_direction {
        return Err(format!(
            "{source} ISO gain map has backward direction; use HDR primary path instead of deferred forward compose"
        ));
    }

    let sdr_rgba = Arc::new(sdr_rgba);
    let gain_rgba = Arc::new(gain_rgba);
    let weight = gain_map_weight(metadata, hdr_target_capacity);
    let mut image_metadata = HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb);
    image_metadata.transfer_function = HdrTransferFunction::Srgb;
    image_metadata.reference = HdrReference::SdrGainMapBase;
    image_metadata.luminance = luminance_hints_from_gain_map(metadata);
    image_metadata.gain_map = Some(HdrGainMapMetadata {
        source,
        target_hdr_capacity: Some(hdr_target_capacity),
        diagnostic: format!(
            "{source} GPU deferred ({}x{} gain {}x{} weight: {:.3}): {}",
            width,
            height,
            gain_width,
            gain_height,
            weight,
            gain_map_metadata_diagnostic(metadata, hdr_target_capacity)
        ),
        capped_display_referred: false,
        apple_heic_deferred: None,
        iso_deferred: Some(IsoGainMapGpuSource {
            sdr_rgba: Arc::clone(&sdr_rgba),
            gain_rgba: Arc::clone(&gain_rgba),
            gain_width,
            gain_height,
            metadata,
        }),
    });

    Ok(HdrImageBuffer {
        width,
        height,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::LinearSrgb,
        metadata: image_metadata,
        rgba_f32: Arc::new(Vec::new()),
    })
}

/// ISO forward gain-map HDR shown via embedded SDR master only (no gain-map plane decode).
pub(crate) fn attach_iso_embedded_sdr_master_only(
    source: &'static str,
    width: u32,
    height: u32,
    sdr_rgba: Vec<u8>,
    metadata: GainMapMetadata,
) -> Result<HdrImageBuffer, String> {
    let expected_len = (width as usize)
        .checked_mul(height as usize)
        .and_then(|p| p.checked_mul(4))
        .ok_or_else(|| format!("dimension overflow: {width}x{height}"))?;
    if sdr_rgba.len() != expected_len {
        return Err(format!(
            "embedded SDR master RGBA length mismatch: got {}, expected {} for {}x{}",
            sdr_rgba.len(),
            expected_len,
            width,
            height
        ));
    }
    if metadata.backward_direction {
        return Err(format!(
            "{source} ISO gain map has backward direction; embedded SDR master load is invalid"
        ));
    }

    let sdr_rgba = Arc::new(sdr_rgba);
    let empty_gain = Arc::new(Vec::new());
    let mut image_metadata = HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb);
    image_metadata.transfer_function = HdrTransferFunction::Srgb;
    image_metadata.reference = HdrReference::SdrGainMapBase;
    image_metadata.luminance = luminance_hints_from_gain_map(metadata);
    image_metadata.gain_map = Some(HdrGainMapMetadata {
        source,
        target_hdr_capacity: None,
        diagnostic: format!(
            "{source} embedded SDR master (skipped HDR decode): {}",
            gain_map_metadata_diagnostic(metadata, metadata.hdr_capacity_min)
        ),
        capped_display_referred: false,
        apple_heic_deferred: None,
        iso_deferred: Some(IsoGainMapGpuSource {
            sdr_rgba: Arc::clone(&sdr_rgba),
            gain_rgba: empty_gain,
            gain_width: 0,
            gain_height: 0,
            metadata,
        }),
    });

    Ok(HdrImageBuffer {
        width,
        height,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::LinearSrgb,
        metadata: image_metadata,
        rgba_f32: Arc::new(Vec::new()),
    })
}

/// Primary JPEG stores the HDR base rendition (ISO backward / `BaseRenditionIsHDR`); skip forward compose.
pub(crate) fn attach_iso_gain_map_hdr_base_from_primary_rgba8(
    source: &'static str,
    width: u32,
    height: u32,
    primary_rgba: Vec<u8>,
    metadata: GainMapMetadata,
) -> Result<HdrImageBuffer, String> {
    let expected_len = (width as usize)
        .checked_mul(height as usize)
        .and_then(|p| p.checked_mul(4))
        .ok_or_else(|| format!("primary dimension overflow: {width}x{height}"))?;
    if primary_rgba.len() != expected_len {
        return Err(format!(
            "HDR base primary RGBA length mismatch: got {}, expected {} for {}x{}",
            primary_rgba.len(),
            expected_len,
            width,
            height
        ));
    }

    let rgba_f32 = primary_srgb_rgba8_to_linear_rgba_f32(&primary_rgba);
    let mut image_metadata = HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb);
    image_metadata.transfer_function = HdrTransferFunction::Linear;
    image_metadata.reference = HdrReference::Unknown;
    image_metadata.luminance = luminance_hints_from_gain_map(metadata);
    image_metadata.gain_map = Some(HdrGainMapMetadata {
        source,
        target_hdr_capacity: None,
        diagnostic: format!(
            "{source} HDR base (skipping forward compose): {}",
            gain_map_metadata_diagnostic(metadata, metadata.hdr_capacity_min)
        ),
        capped_display_referred: false,
        apple_heic_deferred: None,
        iso_deferred: None,
    });

    Ok(HdrImageBuffer {
        width,
        height,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::LinearSrgb,
        metadata: image_metadata,
        rgba_f32: Arc::new(rgba_f32),
    })
}

pub(crate) struct JpegGainMapDeferredInput {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) sdr_rgba: Vec<u8>,
    pub(crate) gain_width: u32,
    pub(crate) gain_height: u32,
    pub(crate) gain_rgba: Vec<u8>,
    pub(crate) metadata: GainMapMetadata,
    pub(crate) hdr_target_capacity: f32,
}

pub(crate) fn attach_jpeg_gain_map_gpu_deferred(
    input: JpegGainMapDeferredInput,
) -> Result<HdrImageBuffer, String> {
    let JpegGainMapDeferredInput {
        width,
        height,
        sdr_rgba,
        gain_width,
        gain_height,
        gain_rgba,
        metadata,
        hdr_target_capacity,
    } = input;
    attach_iso_gain_map_gpu_deferred(IsoGainMapDeferredInput {
        source: "JPEG_R",
        width,
        height,
        sdr_rgba,
        gain_width,
        gain_height,
        gain_rgba,
        metadata,
        hdr_target_capacity,
    })
}

fn orient_arc_rgba8(
    slot: &mut Arc<Vec<u8>>,
    width: u32,
    height: u32,
    orientation: u16,
) -> (u32, u32) {
    if orientation <= 1 {
        return (width, height);
    }
    let (out_w, out_h, oriented) = match Arc::try_unwrap(std::mem::replace(slot, Arc::new(Vec::new()))) {
        Ok(owned) => crate::libtiff_loader::apply_orientation_buffer(owned, width, height, orientation),
        Err(shared) => crate::libtiff_loader::apply_orientation_buffer_from_slice(
            shared.as_ref(),
            width,
            height,
            orientation,
        ),
    };
    *slot = Arc::new(oriented);
    (out_w, out_h)
}

pub(crate) fn apply_orientation_to_iso_deferred_hdr_buffer(
    mut buffer: HdrImageBuffer,
    orientation: u16,
) -> HdrImageBuffer {
    if orientation <= 1 {
        return buffer;
    }

    let Some(gain_map) = buffer.metadata.gain_map.as_mut() else {
        return buffer;
    };
    let Some(deferred) = gain_map.iso_deferred.as_mut() else {
        return buffer;
    };

    let (out_w, out_h) =
        orient_arc_rgba8(&mut deferred.sdr_rgba, buffer.width, buffer.height, orientation);
    let (gain_w, gain_h) = orient_arc_rgba8(
        &mut deferred.gain_rgba,
        deferred.gain_width,
        deferred.gain_height,
        orientation,
    );

    buffer.width = out_w;
    buffer.height = out_h;
    deferred.gain_width = gain_w;
    deferred.gain_height = gain_h;
    buffer
}

pub(crate) fn iso_deferred_from_metadata(
    metadata: &HdrImageMetadata,
) -> Option<&IsoGainMapGpuSource> {
    metadata
        .gain_map
        .as_ref()
        .and_then(|gain_map| gain_map.iso_deferred.as_ref())
}

pub(crate) struct IsoDeferredTileMetadataInput {
    pub(crate) source: &'static str,
    pub(crate) sdr_rgba: Arc<Vec<u8>>,
    pub(crate) gain_rgba: Arc<Vec<u8>>,
    pub(crate) gain_width: u32,
    pub(crate) gain_height: u32,
    pub(crate) metadata: GainMapMetadata,
    pub(crate) hdr_target_capacity: f32,
    pub(crate) physical_width: u32,
    pub(crate) physical_height: u32,
}

pub(crate) fn attach_iso_deferred_tile_metadata(
    input: IsoDeferredTileMetadataInput,
) -> HdrImageMetadata {
    let IsoDeferredTileMetadataInput {
        source,
        sdr_rgba,
        gain_rgba,
        gain_width,
        gain_height,
        metadata,
        hdr_target_capacity,
        physical_width,
        physical_height,
    } = input;
    let weight = gain_map_weight(metadata, hdr_target_capacity);
    let mut image_metadata = HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb);
    image_metadata.transfer_function = HdrTransferFunction::Srgb;
    image_metadata.reference = HdrReference::SdrGainMapBase;
    image_metadata.luminance = luminance_hints_from_gain_map(metadata);
    image_metadata.gain_map = Some(HdrGainMapMetadata {
        source,
        target_hdr_capacity: Some(hdr_target_capacity),
        diagnostic: format!(
            "{source} GPU deferred tiled ({}x{} gain {}x{} weight: {:.3}): {}",
            physical_width,
            physical_height,
            gain_width,
            gain_height,
            weight,
            gain_map_metadata_diagnostic(metadata, hdr_target_capacity)
        ),
        capped_display_referred: false,
        apple_heic_deferred: None,
        iso_deferred: Some(IsoGainMapGpuSource {
            sdr_rgba,
            gain_rgba,
            gain_width,
            gain_height,
            metadata,
        }),
    });
    image_metadata
}

/// CPU compose when GPU ISO gain-map compose in [`crate::hdr::renderer::jpeg_compose_gpu`] is unavailable.
///
/// Keep the plane validation here even though normal decode paths validate in
/// `attach_iso_gain_map_gpu_deferred`: renderer fallback code may receive deferred
/// planes from tiled metadata or future loaders, and this function indexes both buffers directly.
pub(crate) fn compose_iso_deferred_cpu_pixels(
    width: u32,
    height: u32,
    deferred: &IsoGainMapGpuSource,
    target_hdr_capacity: f32,
) -> Result<Vec<f32>, String> {
    use crate::hdr::gain_map::{append_hdr_pixel_from_sdr_and_gain, sample_gain_map_rgb};

    crate::hdr::gain_map::validate_iso_deferred_planes(
        width,
        height,
        deferred.sdr_rgba.as_slice(),
        deferred.gain_width,
        deferred.gain_height,
        deferred.gain_rgba.as_slice(),
    )?;

    let mut rgba_f32 = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            let sdr_index = (y as usize * width as usize + x as usize) * 4;
            let gain_value = sample_gain_map_rgb(
                deferred.gain_rgba.as_slice(),
                deferred.gain_width,
                deferred.gain_height,
                x,
                y,
                width,
                height,
            );
            append_hdr_pixel_from_sdr_and_gain(
                &mut rgba_f32,
                &deferred.sdr_rgba[sdr_index..sdr_index + 4],
                gain_value,
                deferred.metadata,
                target_hdr_capacity,
            );
        }
    }
    Ok(rgba_f32)
}

/// CPU compose for a deferred ISO gain-map tile region.
///
/// The full physical planes are validated defensively before sampling tile pixels;
/// not every caller necessarily constructed the metadata through the static-image attach helper.
pub(crate) fn compose_iso_deferred_tile_cpu_pixels(
    deferred: &IsoGainMapGpuSource,
    tile_ctx: &crate::hdr::types::IsoDeferredTileContext,
    tile_width: u32,
    tile_height: u32,
    target_hdr_capacity: f32,
) -> Result<Vec<f32>, String> {
    crate::hdr::gain_map::validate_iso_deferred_planes(
        tile_ctx.physical_width,
        tile_ctx.physical_height,
        deferred.sdr_rgba.as_slice(),
        deferred.gain_width,
        deferred.gain_height,
        deferred.gain_rgba.as_slice(),
    )?;
    Ok(
        crate::hdr::ultra_hdr_compose::compose_ultra_hdr_tile_region_cpu(
            crate::hdr::ultra_hdr_compose::UltraHdrTileRegionCompose {
                tile_width,
                tile_height,
                origin_x: tile_ctx.origin_x,
                origin_y: tile_ctx.origin_y,
                physical_width: tile_ctx.physical_width,
                physical_height: tile_ctx.physical_height,
                orientation: tile_ctx.orientation,
                sdr_rgba: deferred.sdr_rgba.as_slice(),
                gain_rgba: deferred.gain_rgba.as_slice(),
                gain_width: deferred.gain_width,
                gain_height: deferred.gain_height,
                metadata: deferred.metadata,
                target_hdr_capacity,
                display_to_physical: crate::hdr::ultra_hdr::display_to_physical_pixel,
            },
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_gain_map_metadata() -> GainMapMetadata {
        GainMapMetadata {
            gain_map_min: [0.0; 3],
            gain_map_max: [1.0; 3],
            gamma: [1.0; 3],
            offset_sdr: [0.0; 3],
            offset_hdr: [0.0; 3],
            hdr_capacity_min: 1.0,
            hdr_capacity_max: 4.0,
            backward_direction: false,
        }
    }

    #[test]
    fn cpu_compose_rejects_short_sdr_baseline() {
        let deferred = IsoGainMapGpuSource {
            sdr_rgba: Arc::new(vec![0; 4]),
            gain_rgba: Arc::new(vec![0; 4]),
            gain_width: 1,
            gain_height: 1,
            metadata: test_gain_map_metadata(),
        };

        let err = compose_iso_deferred_cpu_pixels(2, 1, &deferred, 1.0)
            .expect_err("short SDR baseline must be rejected before indexing");

        assert!(err.contains("SDR baseline RGBA length mismatch"));
    }

    #[test]
    fn tile_cpu_compose_rejects_short_sdr_baseline() {
        let deferred = IsoGainMapGpuSource {
            sdr_rgba: Arc::new(vec![0; 4]),
            gain_rgba: Arc::new(vec![0; 4]),
            gain_width: 1,
            gain_height: 1,
            metadata: test_gain_map_metadata(),
        };
        let tile_ctx = crate::hdr::types::IsoDeferredTileContext {
            origin_x: 0,
            origin_y: 0,
            physical_width: 2,
            physical_height: 1,
            orientation: 1,
        };

        let err = compose_iso_deferred_tile_cpu_pixels(&deferred, &tile_ctx, 2, 1, 1.0)
            .expect_err("short tiled SDR baseline must be rejected before indexing");

        assert!(err.contains("SDR baseline RGBA length mismatch"));
    }
}
