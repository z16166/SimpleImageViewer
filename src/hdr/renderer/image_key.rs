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

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct HdrImageKey {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) format: HdrPixelFormat,
    pub(super) rgba_ptr: usize,
    pub(super) rgba_len: usize,
    pub(super) rgba_sample_hash: u64,
    pub(super) iso_deferred_sdr_ptr: Option<usize>,
    pub(super) iso_deferred_sdr_len: Option<usize>,
    pub(super) iso_deferred_sdr_sample_hash: Option<u64>,
    pub(super) iso_deferred_gain_ptr: Option<usize>,
    pub(super) iso_deferred_gain_len: Option<usize>,
    pub(super) iso_deferred_gain_sample_hash: Option<u64>,
    pub(super) iso_deferred_metadata_hash: Option<u64>,
    pub(super) apple_deferred_ptr: Option<usize>,
    pub(super) apple_deferred_len: Option<usize>,
    pub(super) apple_deferred_sample_hash: Option<u64>,
    pub(super) apple_deferred_headroom_bits: Option<u32>,
    pub(super) apple_deferred_stops_bits: Option<u32>,
    pub(super) gain_map_target_capacity_bits: Option<u32>,
    pub(super) gain_map_capped_display_referred: bool,
    /// GPU RAW demosaic CFA buffer identity (empty `rgba_f32` HDR stills).
    pub(super) raw_pixels_ptr: Option<usize>,
    pub(super) raw_pixels_len: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct HdrTileKey {
    pub(super) cache_id: u64,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) rgba_len: usize,
    pub(super) uv_min_bits: [u32; 2],
    pub(super) uv_max_bits: [u32; 2],
}

impl HdrTileKey {
    #[allow(dead_code)]
    pub(super) fn from_tile(tile: &crate::hdr::tiled::HdrTileBuffer) -> Self {
        Self::from_tile_with_uv(
            tile,
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
        )
    }

    pub(super) fn from_tile_with_uv(
        tile: &crate::hdr::tiled::HdrTileBuffer,
        uv_rect: egui::Rect,
    ) -> Self {
        Self {
            cache_id: tile.cache_id,
            width: tile.width,
            height: tile.height,
            rgba_len: tile.rgba_f32.len(),
            uv_min_bits: [uv_rect.min.x.to_bits(), uv_rect.min.y.to_bits()],
            uv_max_bits: [uv_rect.max.x.to_bits(), uv_rect.max.y.to_bits()],
        }
    }
}

impl HdrImageKey {
    pub(crate) fn from_image(image: &HdrImageBuffer) -> Self {
        let (
            iso_deferred_sdr_ptr,
            iso_deferred_sdr_len,
            iso_deferred_sdr_sample_hash,
            iso_deferred_gain_ptr,
            iso_deferred_gain_len,
            iso_deferred_gain_sample_hash,
            iso_deferred_metadata_hash,
            apple_deferred_ptr,
            apple_deferred_len,
            apple_deferred_sample_hash,
            apple_deferred_headroom_bits,
            apple_deferred_stops_bits,
            gain_map_target_capacity_bits,
            gain_map_capped_display_referred,
        ) = image
            .metadata
            .gain_map
            .as_ref()
            .map(|gm| {
                let (iso_sdr_ptr, iso_sdr_len, iso_sdr_hash) = gm
                    .iso_deferred
                    .as_ref()
                    .map(|d| {
                        (
                            Some(std::sync::Arc::as_ptr(&d.sdr_rgba) as usize),
                            Some(d.sdr_rgba.len()),
                            Some(sample_hash_u8(d.sdr_rgba.as_slice())),
                        )
                    })
                    .unwrap_or((None, None, None));
                let (iso_gain_ptr, iso_gain_len, iso_gain_hash) = gm
                    .iso_deferred
                    .as_ref()
                    .map(|d| {
                        (
                            Some(std::sync::Arc::as_ptr(&d.gain_rgba) as usize),
                            Some(d.gain_rgba.len()),
                            Some(sample_hash_u8(d.gain_rgba.as_slice())),
                        )
                    })
                    .unwrap_or((None, None, None));
                let iso_metadata_hash = gm
                    .iso_deferred
                    .as_ref()
                    .map(|d| gain_map_metadata_hash(d.metadata));
                let (apple_ptr, apple_len, apple_hash) = gm
                    .apple_heic_deferred
                    .as_ref()
                    .map(|d| {
                        (
                            Some(std::sync::Arc::as_ptr(&d.gain_rgba) as usize),
                            Some(d.gain_rgba.len()),
                            Some(sample_hash_u8(d.gain_rgba.as_slice())),
                        )
                    })
                    .unwrap_or((None, None, None));
                let apple_headroom_bits = gm
                    .apple_heic_deferred
                    .as_ref()
                    .map(|d| d.headroom_span.to_bits());
                let apple_stops_bits = gm.apple_heic_deferred.as_ref().map(|d| d.stops.to_bits());
                (
                    iso_sdr_ptr,
                    iso_sdr_len,
                    iso_sdr_hash,
                    iso_gain_ptr,
                    iso_gain_len,
                    iso_gain_hash,
                    iso_metadata_hash,
                    apple_ptr,
                    apple_len,
                    apple_hash,
                    apple_headroom_bits,
                    apple_stops_bits,
                    gm.target_hdr_capacity.map(f32::to_bits),
                    gm.capped_display_referred,
                )
            })
            .unwrap_or((
                None, None, None, None, None, None, None, None, None, None, None, None, None, false,
            ));
        let (raw_pixels_ptr, raw_pixels_len) = image
            .metadata
            .raw_gpu_source
            .as_ref()
            .map(|source| {
                (
                    Some(std::sync::Arc::as_ptr(&source.raw_pixels) as usize),
                    Some(source.raw_pixels.len()),
                )
            })
            .unwrap_or((None, None));
        Self {
            width: image.width,
            height: image.height,
            format: image.format,
            rgba_ptr: std::sync::Arc::as_ptr(&image.rgba_f32) as usize,
            rgba_len: image.rgba_f32.len(),
            rgba_sample_hash: sample_hash_f32(image.rgba_f32.as_slice()),
            iso_deferred_sdr_ptr,
            iso_deferred_sdr_len,
            iso_deferred_sdr_sample_hash,
            iso_deferred_gain_ptr,
            iso_deferred_gain_len,
            iso_deferred_gain_sample_hash,
            iso_deferred_metadata_hash,
            apple_deferred_ptr,
            apple_deferred_len,
            apple_deferred_sample_hash,
            apple_deferred_headroom_bits,
            apple_deferred_stops_bits,
            gain_map_target_capacity_bits,
            gain_map_capped_display_referred,
            raw_pixels_ptr,
            raw_pixels_len,
        }
    }
}

fn gain_map_metadata_hash(metadata: GainMapMetadata) -> u64 {
    let mut h = 0x475f_4d41_505f_4d45_u64; // "G_MAP_ME"
    for value in metadata
        .gain_map_min
        .into_iter()
        .chain(metadata.gain_map_max)
        .chain(metadata.gamma)
        .chain(metadata.offset_sdr)
        .chain(metadata.offset_hdr)
        .chain([metadata.hdr_capacity_min, metadata.hdr_capacity_max])
    {
        h = h.rotate_left(9) ^ u64::from(value.to_bits());
    }
    h ^ u64::from(metadata.backward_direction)
}

/// Content fingerprint for HDR float planes (preview bind-group / texture cache keys).
pub(crate) fn sample_hash_f32_for_preview(values: &[f32]) -> u64 {
    sample_hash_f32(values)
}

fn sample_hash_f32(values: &[f32]) -> u64 {
    if values.is_empty() {
        return 0;
    }
    let mut h: u64 = values.len() as u64;
    let sample_points = [
        0usize,
        values.len() / 3,
        (values.len() * 2) / 3,
        values.len() - 1,
    ];
    for idx in sample_points {
        h = h.rotate_left(7) ^ u64::from(values[idx].to_bits());
    }
    h
}

fn sample_hash_u8(values: &[u8]) -> u64 {
    if values.is_empty() {
        return 0;
    }
    let mut h: u64 = values.len() as u64;
    let sample_points = [
        0usize,
        values.len() / 3,
        (values.len() * 2) / 3,
        values.len() - 1,
    ];
    for idx in sample_points {
        h = h.rotate_left(5) ^ u64::from(values[idx]);
    }
    h
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RawGpuDemosaicBakedNotice {
    pub key: HdrImageKey,
    /// Wall time for CFA upload (first prepare) + compute encode on the GPU thread.
    pub demosaic_ms: u32,
}
