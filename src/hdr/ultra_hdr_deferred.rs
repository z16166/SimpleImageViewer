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

use crate::hdr::gain_map::gain_map_metadata_diagnostic;
use crate::hdr::gain_map::iso_gain_map_skips_forward_compose;
use crate::hdr::jpeg_gain_map_gpu::{
    attach_iso_gain_map_hdr_base_from_primary_rgba8, attach_jpeg_gain_map_gpu_deferred,
};
use crate::hdr::types::HdrImageBuffer;
use crate::hdr::ultra_hdr::{
    extract_gain_map_jpeg_bytes, gain_map_metadata, inspect_ultra_hdr_jpeg_bytes,
};

pub(crate) fn decode_ultra_hdr_jpeg_deferred_bytes(
    bytes: &[u8],
    target_hdr_capacity: f32,
) -> Result<HdrImageBuffer, String> {
    let info = inspect_ultra_hdr_jpeg_bytes(bytes)?;
    if !info.is_ultra_hdr {
        return Err("JPEG does not advertise Ultra HDR gain map metadata".to_string());
    }

    let (width, height, sdr_rgba) = libjpeg_turbo::decode_to_rgba(bytes)?;
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

    attach_jpeg_gain_map_gpu_deferred(
        width,
        height,
        sdr_rgba,
        gain_width,
        gain_height,
        gain_rgba,
        metadata,
        target_hdr_capacity,
    )
}
