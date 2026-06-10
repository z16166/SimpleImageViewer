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
use crate::hdr::types::{HdrColorSpace, HdrImageMetadata, IsoDeferredTileContext};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use super::globals::NEXT_HDR_TILE_CACHE_ID;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct HdrTileBuffer {
    pub cache_id: u64,
    pub width: u32,
    pub height: u32,
    pub color_space: HdrColorSpace,
    pub metadata: HdrImageMetadata,
    pub rgba_f32: Arc<Vec<f32>>,
    /// Set when [`metadata`](Self::metadata) carries `iso_deferred` and pixels are composed on GPU.
    pub iso_deferred_tile: Option<IsoDeferredTileContext>,
}

impl HdrTileBuffer {
    #[allow(dead_code)]
    pub(crate) fn new(
        width: u32,
        height: u32,
        color_space: HdrColorSpace,
        rgba_f32: Arc<Vec<f32>>,
    ) -> Self {
        Self::new_with_metadata(
            width,
            height,
            color_space,
            HdrImageMetadata::from_color_space(color_space),
            rgba_f32,
        )
    }

    pub(crate) fn new_with_metadata(
        width: u32,
        height: u32,
        color_space: HdrColorSpace,
        metadata: HdrImageMetadata,
        rgba_f32: Arc<Vec<f32>>,
    ) -> Self {
        Self {
            cache_id: NEXT_HDR_TILE_CACHE_ID.fetch_add(1, Ordering::Relaxed),
            width,
            height,
            color_space,
            metadata,
            rgba_f32,
            iso_deferred_tile: None,
        }
    }

    pub(crate) fn new_iso_deferred_tile(
        width: u32,
        height: u32,
        color_space: HdrColorSpace,
        metadata: HdrImageMetadata,
        iso_deferred_tile: IsoDeferredTileContext,
    ) -> Self {
        Self {
            cache_id: NEXT_HDR_TILE_CACHE_ID.fetch_add(1, Ordering::Relaxed),
            width,
            height,
            color_space,
            metadata,
            rgba_f32: Arc::new(Vec::new()),
            iso_deferred_tile: Some(iso_deferred_tile),
        }
    }
}
