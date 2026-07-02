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

//! Ultra HDR JPEG decode without eager CPU gain-map composition.

use std::path::Path;

use crate::hdr::gain_map::gain_map_metadata_diagnostic;
use crate::hdr::gain_map::iso_gain_map_skips_forward_compose;
use crate::hdr::jpeg_gain_map_gpu::{
    attach_iso_gain_map_hdr_base_from_primary_rgba8, attach_jpeg_gain_map_gpu_deferred,
};
use crate::hdr::types::HdrImageBuffer;
use crate::hdr::ultra_hdr::{
    extract_gain_map_jpeg_bytes, gain_map_metadata, inspect_ultra_hdr_jpeg_bytes,
};

struct UltraHdrPrimaryRgba {
    width: u32,
    height: u32,
    sdr_rgba: Vec<u8>,
}

fn decode_ultra_hdr_primary_rgba(
    bytes: &[u8],
    orientation: u16,
) -> Result<UltraHdrPrimaryRgba, String> {
    let (mut width, mut height, mut sdr_rgba) = libjpeg_turbo::decode_to_rgba(bytes)?;
    if orientation > 1 {
        let oriented =
            crate::libtiff_loader::apply_orientation_buffer(sdr_rgba, width, height, orientation);
        width = oriented.0;
        height = oriented.1;
        sdr_rgba = oriented.2;
    }
    Ok(UltraHdrPrimaryRgba {
        width,
        height,
        sdr_rgba,
    })
}

fn finish_ultra_hdr_from_primary_rgba(
    bytes: &[u8],
    primary: UltraHdrPrimaryRgba,
    target_hdr_capacity: f32,
) -> Result<HdrImageBuffer, String> {
    let UltraHdrPrimaryRgba {
        width,
        height,
        sdr_rgba,
    } = primary;
    let gain_map_jpeg = extract_gain_map_jpeg_bytes(bytes)?;
    let metadata = gain_map_metadata(&gain_map_jpeg)?;
    log::debug!(
        "[HDR] Ultra HDR JPEG_R deferred metadata: {}",
        gain_map_metadata_diagnostic(metadata, target_hdr_capacity)
    );

    if iso_gain_map_skips_forward_compose(metadata) {
        log::debug!(
            "[HDR] Ultra HDR JPEG_R HDR base (backward/precomposed); skipping forward compose: {}",
            gain_map_metadata_diagnostic(metadata, target_hdr_capacity)
        );
        return attach_iso_gain_map_hdr_base_from_primary_rgba8(
            "JPEG_R", width, height, sdr_rgba, metadata,
        );
    }

    let (gain_width, gain_height, gain_rgba) = libjpeg_turbo::decode_to_rgba(&gain_map_jpeg)?;

    attach_jpeg_gain_map_gpu_deferred(crate::hdr::jpeg_gain_map_gpu::JpegGainMapDeferredInput {
        width,
        height,
        sdr_rgba,
        gain_width,
        gain_height,
        gain_rgba,
        metadata,
        hdr_target_capacity: target_hdr_capacity,
    })
}

/// One primary baseline decode; optionally try embedded-SDR attach before gain-map decode.
pub(crate) fn decode_ultra_hdr_jpeg_with_optional_embedded_sdr_master(
    bytes: &[u8],
    target_hdr_capacity: f32,
    orientation: u16,
    try_embedded_sdr_master: bool,
    path: Option<&Path>,
) -> Result<HdrImageBuffer, String> {
    let info = inspect_ultra_hdr_jpeg_bytes(bytes)?;
    if !info.is_ultra_hdr {
        return Err("JPEG does not advertise Ultra HDR gain map metadata".to_string());
    }

    let primary = decode_ultra_hdr_primary_rgba(bytes, orientation)?;
    if try_embedded_sdr_master {
        let gain_map_jpeg = extract_gain_map_jpeg_bytes(bytes)?;
        let metadata = gain_map_metadata(&gain_map_jpeg)?;
        if iso_gain_map_skips_forward_compose(metadata) {
            let err = "Ultra HDR JPEG primary is HDR base; embedded SDR master load is invalid"
                .to_string();
            if let Some(path) = path {
                crate::loader::embedded_sdr_fallback::log_embedded_sdr_master_fallback(
                    "Ultra HDR JPEG",
                    path,
                    &err,
                );
            }
        } else {
            match crate::hdr::jpeg_gain_map_gpu::attach_iso_embedded_sdr_master_only(
                "JPEG_R",
                primary.width,
                primary.height,
                primary.sdr_rgba.clone(),
                metadata,
            ) {
                Ok(hdr) => return Ok(hdr),
                Err(err)
                    if crate::loader::embedded_sdr_fallback::ultra_hdr_embedded_sdr_ineligible(
                        &err,
                    ) =>
                {
                    if let Some(path) = path {
                        crate::loader::embedded_sdr_fallback::log_embedded_sdr_master_fallback(
                            "Ultra HDR JPEG",
                            path,
                            &err,
                        );
                    }
                }
                Err(err) => return Err(err),
            }
        }
    }

    finish_ultra_hdr_from_primary_rgba(bytes, primary, target_hdr_capacity)
}

/// Ultra HDR JPEG loaded as embedded SDR master only (primary baseline, no gain-map decode).
#[allow(dead_code)] // Standalone entry; main loader uses `decode_ultra_hdr_jpeg_with_optional_embedded_sdr_master`.
pub(crate) fn load_ultra_hdr_embedded_sdr_master_bytes(
    bytes: &[u8],
    orientation: u16,
) -> Result<HdrImageBuffer, String> {
    decode_ultra_hdr_jpeg_with_optional_embedded_sdr_master(bytes, 1.0, orientation, true, None)
}
