// Simple Image Viewer - A high-performance, cross-platform image viewer
// Copyright (C) 2024-2026 Simple Image Viewer Contributors
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

#[allow(dead_code)]

mod channels;
mod chromaticities;
mod mmap;
mod read_context;
mod types;

#[cfg(test)]
mod tests;

pub(crate) const DEFAULT_DECODED_CHUNK_CACHE_BYTES: usize = 512 * 1024 * 1024;
pub(crate) const MAX_DECODED_CHUNK_CACHE_BYTES: usize = 4 * 1024 * 1024 * 1024;
pub(crate) const SCANLINE_BOOTSTRAP_PREVIEW_MAX_SIDE: u32 = 1024;
pub(crate) const SCANLINE_BOOTSTRAP_PREVIEW_SOURCE_ROW_BUDGET: u32 = 192;
pub(crate) const SCANLINE_REFINED_PREVIEW_SOURCE_ROW_BUDGET: u32 = 0;

pub(crate) use chromaticities::{
    chromaticities_looks_like_aces_ap0, deep_scanline_flatten_rgba_via_imf,
    extract_rgba32f_tile_from_flat_buffer, hdr_color_space_from_chromaticities_xy,
    imf_exr_chromaticities_from_path, is_luminance_chroma_scanline_part,
    openexr_luminance_weights_from_chromaticities_xy, rgba_input_scanline_flatten_rgba_via_imf,
};
pub(crate) use read_context::OpenExrCoreReadContext;
pub(crate) use types::{
    OpenExrCoreChannelInfo, OpenExrCorePartInfo, OpenExrCoreRgbaTile,
    OpenExrCoreDecodedChunk, OpenExrCoreDecodedChunkCache, OpenExrCoreDecodedChunkKey,
};
