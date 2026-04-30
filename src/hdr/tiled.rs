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

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::types::{HdrColorSpace, HdrImageBuffer, HdrPixelFormat};

const DEFAULT_HDR_TILE_CACHE_MAX_BYTES: usize = 256 * 1024 * 1024;
pub static HDR_TILE_CACHE_MAX_BYTES: AtomicUsize =
    AtomicUsize::new(DEFAULT_HDR_TILE_CACHE_MAX_BYTES);

type HdrTileCacheKey = (u32, u32, u32, u32);

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct HdrTileBuffer {
    pub width: u32,
    pub height: u32,
    pub color_space: HdrColorSpace,
    pub rgba_f32: Arc<Vec<f32>>,
}

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

    pub fn width(&self) -> u32 {
        self.image.width
    }

    pub fn height(&self) -> u32 {
        self.image.height
    }

    #[allow(dead_code)]
    pub fn color_space(&self) -> HdrColorSpace {
        self.image.color_space
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

    pub fn extract_tile_rgba32f_arc(
        &self,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    ) -> Result<Arc<HdrTileBuffer>, String> {
        validate_tile_bounds(self.image.width, self.image.height, x, y, width, height)?;
        let key = (x, y, width, height);
        if let Ok(mut cache) = self.tile_cache.lock() {
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

        let tile = Arc::new(HdrTileBuffer {
            width,
            height,
            color_space: self.image.color_space,
            rgba_f32: Arc::new(tile),
        });

        if let Ok(mut cache) = self.tile_cache.lock() {
            cache.insert(key, Arc::clone(&tile));
        }

        Ok(tile)
    }

    #[cfg(test)]
    fn cached_tile_count(&self) -> usize {
        self.tile_cache
            .lock()
            .map(|cache| cache.len())
            .unwrap_or_default()
    }

    #[cfg(test)]
    fn cached_tile_bytes(&self) -> usize {
        self.tile_cache
            .lock()
            .map(|cache| cache.current_bytes())
            .unwrap_or_default()
    }

    #[cfg(test)]
    fn cache_budget_bytes(&self) -> usize {
        self.tile_cache
            .lock()
            .map(|cache| cache.max_bytes())
            .unwrap_or_default()
    }
}

pub fn configured_hdr_tile_cache_max_bytes() -> usize {
    HDR_TILE_CACHE_MAX_BYTES.load(Ordering::Relaxed)
}

#[cfg(test)]
fn set_global_hdr_tile_cache_max_bytes_for_tests(max_bytes: usize) {
    HDR_TILE_CACHE_MAX_BYTES.store(max_bytes, Ordering::Relaxed);
}

#[derive(Debug)]
struct HdrTileCache {
    entries: HashMap<HdrTileCacheKey, Arc<HdrTileBuffer>>,
    lru: VecDeque<HdrTileCacheKey>,
    current_bytes: usize,
    max_bytes: usize,
}

impl HdrTileCache {
    fn new(max_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            lru: VecDeque::new(),
            current_bytes: 0,
            max_bytes,
        }
    }

    fn get(&mut self, key: HdrTileCacheKey) -> Option<Arc<HdrTileBuffer>> {
        let tile = self.entries.get(&key).cloned()?;
        self.touch(key);
        Some(tile)
    }

    fn insert(&mut self, key: HdrTileCacheKey, tile: Arc<HdrTileBuffer>) {
        if let Some(old_tile) = self.entries.remove(&key) {
            self.current_bytes = self.current_bytes.saturating_sub(tile_len_bytes(&old_tile));
            self.lru.retain(|existing| *existing != key);
        }

        let bytes = tile_len_bytes(&tile);
        while !self.lru.is_empty() && self.current_bytes.saturating_add(bytes) > self.max_bytes {
            if let Some(evicted_key) = self.lru.pop_front() {
                if let Some(evicted_tile) = self.entries.remove(&evicted_key) {
                    self.current_bytes = self
                        .current_bytes
                        .saturating_sub(tile_len_bytes(&evicted_tile));
                }
            }
        }

        if self.current_bytes.saturating_add(bytes) <= self.max_bytes {
            self.entries.insert(key, tile);
            self.lru.push_back(key);
            self.current_bytes += bytes;
        }
    }

    fn touch(&mut self, key: HdrTileCacheKey) {
        if let Some(pos) = self.lru.iter().position(|existing| *existing == key) {
            self.lru.remove(pos);
        }
        self.lru.push_back(key);
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    fn current_bytes(&self) -> usize {
        self.current_bytes
    }

    #[cfg(test)]
    fn max_bytes(&self) -> usize {
        self.max_bytes
    }
}

fn tile_len_bytes(tile: &HdrTileBuffer) -> usize {
    tile.rgba_f32.len() * std::mem::size_of::<f32>()
}

fn validate_rgba32f_len(width: u32, height: u32, actual_len: usize) -> Result<(), String> {
    let expected_len = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .map(|len| len as usize)
        .ok_or_else(|| format!("HDR tiled source dimensions overflow: {width}x{height}"))?;

    if actual_len != expected_len {
        return Err(format!(
            "Malformed HDR tiled source: expected {expected_len} floats for {width}x{height} RGBA, got {actual_len}",
        ));
    }

    Ok(())
}

#[allow(dead_code)]
fn validate_tile_bounds(
    image_width: u32,
    image_height: u32,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
) -> Result<(), String> {
    if width == 0 || height == 0 {
        return Err(format!(
            "HDR tile requires non-zero dimensions, got {width}x{height}"
        ));
    }

    let end_x = x
        .checked_add(width)
        .ok_or_else(|| format!("HDR tile x range overflows: x={x}, width={width}"))?;
    let end_y = y
        .checked_add(height)
        .ok_or_else(|| format!("HDR tile y range overflows: y={y}, height={height}"))?;

    if end_x > image_width || end_y > image_height {
        return Err(format!(
            "HDR tile {x},{y} {width}x{height} exceeds image bounds {image_width}x{image_height}",
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::hdr::tiled::{
        HdrTiledImageSource, configured_hdr_tile_cache_max_bytes,
        set_global_hdr_tile_cache_max_bytes_for_tests,
    };
    use crate::hdr::types::{HdrColorSpace, HdrImageBuffer, HdrPixelFormat};

    #[test]
    fn extracts_rgba32f_tile_from_in_memory_hdr_buffer() {
        let image = HdrImageBuffer {
            width: 3,
            height: 2,
            format: HdrPixelFormat::Rgba32Float,
            color_space: HdrColorSpace::LinearSrgb,
            rgba_f32: Arc::new(vec![
                0.0, 0.1, 0.2, 1.0, 1.0, 1.1, 1.2, 1.0, 2.0, 2.1, 2.2, 1.0, 3.0, 3.1, 3.2, 1.0,
                4.0, 4.1, 4.2, 1.0, 5.0, 5.1, 5.2, 1.0,
            ]),
        };

        let source = HdrTiledImageSource::new(image).expect("valid HDR tile source");
        let tile = source
            .extract_tile_rgba32f(1, 0, 2, 2)
            .expect("extract valid tile");

        assert_eq!(tile.width, 2);
        assert_eq!(tile.height, 2);
        assert_eq!(
            tile.rgba_f32.as_slice(),
            &[
                1.0, 1.1, 1.2, 1.0, 2.0, 2.1, 2.2, 1.0, 4.0, 4.1, 4.2, 1.0, 5.0, 5.1, 5.2, 1.0,
            ]
        );
    }

    #[test]
    fn rejects_malformed_hdr_tile_source_buffer() {
        let image = HdrImageBuffer {
            width: 2,
            height: 2,
            format: HdrPixelFormat::Rgba32Float,
            color_space: HdrColorSpace::LinearSrgb,
            rgba_f32: Arc::new(vec![1.0; 4]),
        };

        let err = HdrTiledImageSource::new(image).expect_err("reject malformed source");

        assert!(err.contains("expected 16 floats"));
    }

    #[test]
    fn repeated_tile_extraction_reuses_cached_tile_buffer() {
        let image = HdrImageBuffer {
            width: 2,
            height: 1,
            format: HdrPixelFormat::Rgba32Float,
            color_space: HdrColorSpace::LinearSrgb,
            rgba_f32: Arc::new(vec![1.0; 2 * 4]),
        };
        let source = HdrTiledImageSource::new(image).expect("valid HDR tile source");

        let first = source
            .extract_tile_rgba32f_arc(0, 0, 1, 1)
            .expect("extract first tile");
        let second = source
            .extract_tile_rgba32f_arc(0, 0, 1, 1)
            .expect("extract cached tile");

        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn hdr_tile_cache_evicts_least_recently_used_tile_when_over_budget() {
        let source =
            HdrTiledImageSource::new_with_cache_budget(test_image(4, 1), 2 * tile_bytes(1, 1))
                .expect("valid HDR tile source");

        let first = source
            .extract_tile_rgba32f_arc(0, 0, 1, 1)
            .expect("extract first tile");
        let _second = source
            .extract_tile_rgba32f_arc(1, 0, 1, 1)
            .expect("extract second tile");
        let _third = source
            .extract_tile_rgba32f_arc(2, 0, 1, 1)
            .expect("extract third tile");
        let first_after_eviction = source
            .extract_tile_rgba32f_arc(0, 0, 1, 1)
            .expect("re-extract first tile");

        assert!(!Arc::ptr_eq(&first, &first_after_eviction));
        assert_eq!(source.cached_tile_count(), 2);
        assert!(source.cached_tile_bytes() <= 2 * tile_bytes(1, 1));
    }

    #[test]
    fn hdr_tile_cache_refreshes_lru_on_repeated_access() {
        let source =
            HdrTiledImageSource::new_with_cache_budget(test_image(4, 1), 2 * tile_bytes(1, 1))
                .expect("valid HDR tile source");

        let first = source
            .extract_tile_rgba32f_arc(0, 0, 1, 1)
            .expect("extract first tile");
        let second = source
            .extract_tile_rgba32f_arc(1, 0, 1, 1)
            .expect("extract second tile");
        let first_refreshed = source
            .extract_tile_rgba32f_arc(0, 0, 1, 1)
            .expect("refresh first tile");
        let _third = source
            .extract_tile_rgba32f_arc(2, 0, 1, 1)
            .expect("extract third tile");
        let second_after_eviction = source
            .extract_tile_rgba32f_arc(1, 0, 1, 1)
            .expect("re-extract second tile");

        assert!(Arc::ptr_eq(&first, &first_refreshed));
        assert!(!Arc::ptr_eq(&second, &second_after_eviction));
    }

    #[test]
    fn default_hdr_tile_source_uses_global_cache_budget() {
        let old_budget = configured_hdr_tile_cache_max_bytes();
        set_global_hdr_tile_cache_max_bytes_for_tests(tile_bytes(1, 1));

        let source = HdrTiledImageSource::new(test_image(2, 1)).expect("valid HDR tile source");

        set_global_hdr_tile_cache_max_bytes_for_tests(old_budget);
        assert_eq!(source.cache_budget_bytes(), tile_bytes(1, 1));
    }

    fn test_image(width: u32, height: u32) -> HdrImageBuffer {
        HdrImageBuffer {
            width,
            height,
            format: HdrPixelFormat::Rgba32Float,
            color_space: HdrColorSpace::LinearSrgb,
            rgba_f32: Arc::new(vec![1.0; width as usize * height as usize * 4]),
        }
    }

    fn tile_bytes(width: u32, height: u32) -> usize {
        width as usize * height as usize * 4 * std::mem::size_of::<f32>()
    }
}
