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

mod buffer;
mod cache;
mod globals;
mod kind;
mod preview;
mod source;
mod validate;

#[cfg(test)]
mod tests;

pub(crate) use buffer::HdrTileBuffer;
#[cfg(test)]
pub(crate) use cache::set_global_hdr_tile_cache_max_bytes_for_tests;
pub(crate) use cache::{
    HdrTileCache, configure_hdr_tile_cache_budget_from_system_memory,
    configured_hdr_tile_cache_max_bytes,
};
pub(crate) use globals::HDR_TILE_CACHE_MAX_BYTES;
pub(crate) use kind::{HdrTiledSource, HdrTiledSourceKind};
pub(crate) use preview::{
    downsample_hdr_image_nearest, finalize_sdr_preview_pixels,
    hdr_preview_from_tiled_source_nearest, preview_dimensions, preview_sample_coord,
    sdr_preview_from_hdr_image_nearest, sdr_preview_from_linear_rgba32f,
    sdr_preview_from_tiled_source_nearest, tone_map_linear_rgba_f32_row_to_sdr_u8,
};
pub(crate) use source::HdrTiledImageSource;
pub(crate) use validate::validate_tile_bounds;

#[cfg(test)]
pub(crate) use cache::hdr_tile_cache_budget_for_memory;
