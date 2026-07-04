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
use crate::hdr::types::{HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat};
use parking_lot::Mutex;
use std::sync::Arc;

use super::buffer::HdrTileBuffer;
use super::cache::{HdrTileCache, configured_hdr_tile_cache_max_bytes};
use super::kind::{HdrTiledSource, HdrTiledSourceKind};
use super::preview::downsample_hdr_image_nearest;
use super::validate::{validate_rgba32f_len, validate_tile_bounds};

#[derive(Debug)]
pub struct HdrTiledImageSource {
    image: HdrImageBuffer,
    tile_cache: Mutex<HdrTileCache>,
}

impl HdrTiledImageSource {
    pub fn new(image: HdrImageBuffer) -> Result<Self, String> {
        Self::new_with_cache_budget(image, configured_hdr_tile_cache_max_bytes())
    }

    pub fn new_with_cache_budget(
        image: HdrImageBuffer,
        max_cache_bytes: usize,
    ) -> Result<Self, String> {
        if image.format != HdrPixelFormat::Rgba32Float {
            return Err(format!(
                "HDR tiled source currently supports only Rgba32Float buffers, got {:?}",
                image.format
            ));
        }

        validate_rgba32f_len(image.width, image.height, image.rgba_f32.len())?;
        Ok(Self {
            image,
            tile_cache: Mutex::new(HdrTileCache::new(max_cache_bytes)),
        })
    }

    #[allow(dead_code)]
    pub fn extract_tile_rgba32f(
        &self,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    ) -> Result<HdrTileBuffer, String> {
        self.extract_tile_rgba32f_arc(x, y, width, height)
            .map(|tile| (*tile).clone())
    }

    #[cfg(test)]
    pub(crate) fn cached_tile_count(&self) -> usize {
        self.tile_cache.lock().len()
    }

    #[cfg(test)]
    pub(crate) fn cached_tile_bytes(&self) -> usize {
        self.tile_cache.lock().current_bytes()
    }

    #[cfg(test)]
    pub(crate) fn cache_budget_bytes(&self) -> usize {
        self.tile_cache.lock().max_bytes()
    }
}

impl HdrTiledSource for HdrTiledImageSource {
    fn source_kind(&self) -> HdrTiledSourceKind {
        HdrTiledSourceKind::InMemory
    }

    fn width(&self) -> u32 {
        self.image.width
    }

    fn height(&self) -> u32 {
        self.image.height
    }

    fn color_space(&self) -> HdrColorSpace {
        self.image.color_space
    }

    fn metadata(&self) -> HdrImageMetadata {
        self.image.metadata.clone()
    }

    fn generate_sdr_preview(&self, max_w: u32, max_h: u32) -> Result<(u32, u32, Vec<u8>), String> {
        let preview = self.generate_hdr_preview(max_w, max_h)?;
        crate::hdr::tiled::sdr_preview_from_hdr_preview(&preview)
    }

    fn generate_hdr_preview(&self, max_w: u32, max_h: u32) -> Result<HdrImageBuffer, String> {
        downsample_hdr_image_nearest(&self.image, max_w, max_h)
    }

    fn cached_tile_rgba32f_arc(
        &self,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    ) -> Option<Arc<HdrTileBuffer>> {
        self.tile_cache.lock().get((x, y, width, height))
    }

    fn protect_cached_tiles(&self, tiles: &[(u32, u32, u32, u32)]) {
        self.tile_cache
            .lock()
            .set_protected_keys(tiles.iter().copied());
    }

    fn extract_tile_rgba32f_arc(
        &self,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    ) -> Result<Arc<HdrTileBuffer>, String> {
        validate_tile_bounds(self.image.width, self.image.height, x, y, width, height)?;
        let key = (x, y, width, height);
        {
            let mut cache = self.tile_cache.lock();
            if let Some(tile) = cache.get(key) {
                return Ok(tile);
            }
        }

        let mut tile = Vec::with_capacity((width as usize) * (height as usize) * 4);
        let source_stride = self.image.width as usize * 4;
        let row_len = width as usize * 4;
        let start_x = x as usize * 4;

        for row in y..(y + height) {
            let start = row as usize * source_stride + start_x;
            let end = start + row_len;
            tile.extend_from_slice(&self.image.rgba_f32[start..end]);
        }

        let tile = Arc::new(HdrTileBuffer::new_with_metadata(
            width,
            height,
            self.image.color_space,
            self.image.metadata.clone(),
            Arc::new(tile),
        ));

        self.tile_cache.lock().insert(key, Arc::clone(&tile));

        Ok(tile)
    }
}
