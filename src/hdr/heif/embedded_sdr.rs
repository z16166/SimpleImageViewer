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

//! HEIF embedded SDR primary (gain-map HDR shown without float-plane decode).

use std::sync::Arc;

use super::metadata::{
    inspect_heif_gain_map_auxiliaries, read_heif_metadata,
    refine_heif_transfer_for_primary_bit_depth,
};
use super::session::open_heif_primary_from_bytes;

use crate::hdr::types::{
    HEIF_EMBEDDED_SDR_PRIMARY_GAIN_MAP_SOURCE, HdrGainMapMetadata, HdrImageBuffer, HdrPixelFormat,
};

#[cfg(feature = "heif-native")]
pub(crate) fn build_heif_embedded_sdr_master_hdr(
    bytes: &[u8],
    logical: (u32, u32),
) -> Result<HdrImageBuffer, String> {
    let (_ctx, primary) = open_heif_primary_from_bytes(bytes)?;
    let handle = primary.as_ptr();
    let mut metadata = read_heif_metadata(handle);
    refine_heif_transfer_for_primary_bit_depth(handle, &mut metadata);
    metadata.gain_map = inspect_heif_gain_map_auxiliaries(handle).map(|mut gain_map| {
        gain_map.source = HEIF_EMBEDDED_SDR_PRIMARY_GAIN_MAP_SOURCE;
        gain_map
    });
    if metadata.gain_map.is_none() {
        metadata.gain_map = Some(HdrGainMapMetadata {
            source: HEIF_EMBEDDED_SDR_PRIMARY_GAIN_MAP_SOURCE,
            target_hdr_capacity: None,
            diagnostic: "HEIF primary SDR (embedded master)".to_string(),
            capped_display_referred: true,
            apple_heic_deferred: None,
            iso_deferred: None,
        });
    }
    let color_space = metadata.color_space_hint();
    Ok(HdrImageBuffer {
        width: logical.0,
        height: logical.1,
        format: HdrPixelFormat::Rgba32Float,
        color_space,
        metadata,
        rgba_f32: Arc::new(Vec::new()),
    })
}
