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

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use rayon::prelude::*;

use super::types::{HdrColorSpace, HdrImageBuffer, HdrPixelFormat};

const DEFAULT_HDR_TILE_CACHE_MAX_BYTES: usize = 256 * 1024 * 1024;
const MAX_HDR_TILE_CACHE_MAX_BYTES: usize = 4 * 1024 * 1024 * 1024;
pub static HDR_TILE_CACHE_MAX_BYTES: AtomicUsize =
    AtomicUsize::new(initial_hdr_tile_cache_max_bytes());
static NEXT_HDR_TILE_CACHE_ID: AtomicU64 = AtomicU64::new(1);

pub(crate) type HdrTileCacheKey = (u32, u32, u32, u32);

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct HdrTileBuffer {
    pub cache_id: u64,
    pub width: u32,
    pub height: u32,
    pub color_space: HdrColorSpace,
    pub rgba_f32: Arc<Vec<f32>>,
}

impl HdrTileBuffer {
    pub(crate) fn new(
        width: u32,
        height: u32,
        color_space: HdrColorSpace,
        rgba_f32: Arc<Vec<f32>>,
    ) -> Self {
        Self {
            cache_id: NEXT_HDR_TILE_CACHE_ID.fetch_add(1, Ordering::Relaxed),
            width,
            height,
            color_space,
            rgba_f32,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HdrTiledSourceKind {
    InMemory,
    DiskBacked,
}

impl HdrTiledSourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InMemory => "in-memory",
            Self::DiskBacked => "disk-backed",
        }
    }
}

#[allow(dead_code)]
pub trait HdrTiledSource: Send + Sync {
    fn source_kind(&self) -> HdrTiledSourceKind;
    fn source_name(&self) -> String {
        "<memory>".to_string()
    }
    fn width(&self) -> u32;
    fn height(&self) -> u32;
    fn color_space(&self) -> HdrColorSpace;
    fn generate_hdr_preview(&self, max_w: u32, max_h: u32) -> Result<HdrImageBuffer, String>;
    fn generate_sdr_preview(&self, max_w: u32, max_h: u32) -> Result<(u32, u32, Vec<u8>), String>;
    fn cached_tile_rgba32f_arc(
        &self,
        _x: u32,
        _y: u32,
        _width: u32,
        _height: u32,
    ) -> Option<Arc<HdrTileBuffer>> {
        None
    }
    fn protect_cached_tiles(&self, _tiles: &[(u32, u32, u32, u32)]) {}
    fn extract_tile_rgba32f_arc(
        &self,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    ) -> Result<Arc<HdrTileBuffer>, String>;
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

    fn generate_sdr_preview(&self, max_w: u32, max_h: u32) -> Result<(u32, u32, Vec<u8>), String> {
        let preview = self.generate_hdr_preview(max_w, max_h)?;
        let pixels = crate::hdr::decode::hdr_to_sdr_rgba8(&preview, 0.0)?;
        let image = image::ImageBuffer::<image::Rgba<u8>, Vec<u8>>::from_raw(
            preview.width,
            preview.height,
            pixels,
        )
        .ok_or_else(|| "Failed to create HDR SDR preview buffer".to_string())?;
        Ok((image.width(), image.height(), image.into_raw()))
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
        self.tile_cache
            .lock()
            .ok()
            .and_then(|mut cache| cache.get((x, y, width, height)))
    }

    fn protect_cached_tiles(&self, tiles: &[(u32, u32, u32, u32)]) {
        if let Ok(mut cache) = self.tile_cache.lock() {
            cache.set_protected_keys(tiles.iter().copied());
        }
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

        let tile = Arc::new(HdrTileBuffer::new(
            width,
            height,
            self.image.color_space,
            Arc::new(tile),
        ));

        if let Ok(mut cache) = self.tile_cache.lock() {
            cache.insert(key, Arc::clone(&tile));
        }

        Ok(tile)
    }
}

pub(crate) fn downsample_hdr_image_nearest(
    image: &HdrImageBuffer,
    max_w: u32,
    max_h: u32,
) -> Result<HdrImageBuffer, String> {
    validate_rgba32f_len(image.width, image.height, image.rgba_f32.len())?;
    let (width, height) = preview_dimensions(image.width, image.height, max_w, max_h);
    if width == 0 || height == 0 {
        return Err("HDR preview dimensions must be non-zero".to_string());
    }

    let mut rgba_f32 = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        let src_y = preview_sample_coord(y, height, image.height) as usize;
        for x in 0..width {
            let src_x = preview_sample_coord(x, width, image.width) as usize;
            let offset = (src_y * image.width as usize + src_x) * 4;
            rgba_f32.extend_from_slice(&image.rgba_f32[offset..offset + 4]);
        }
    }

    Ok(HdrImageBuffer {
        width,
        height,
        format: HdrPixelFormat::Rgba32Float,
        color_space: image.color_space,
        rgba_f32: Arc::new(rgba_f32),
    })
}

pub(crate) fn hdr_preview_from_tiled_source_nearest<S: HdrTiledSource + ?Sized>(
    source: &S,
    max_w: u32,
    max_h: u32,
) -> Result<HdrImageBuffer, String> {
    let (width, height) = preview_dimensions(source.width(), source.height(), max_w, max_h);
    if width == 0 || height == 0 {
        return Err("HDR tiled preview dimensions must be non-zero".to_string());
    }

    let rows = (0..height)
        .into_par_iter()
        .map(|preview_y| sample_tiled_preview_row(source, preview_y, width, height))
        .collect::<Vec<_>>();

    let mut rgba_f32 = Vec::with_capacity(width as usize * height as usize * 4);
    for row in rows {
        rgba_f32.extend(row?);
    }

    Ok(HdrImageBuffer {
        width,
        height,
        format: HdrPixelFormat::Rgba32Float,
        color_space: source.color_space(),
        rgba_f32: Arc::new(rgba_f32),
    })
}

fn sample_tiled_preview_row<S: HdrTiledSource + ?Sized>(
    source: &S,
    preview_y: u32,
    preview_width: u32,
    preview_height: u32,
) -> Result<Vec<f32>, String> {
    let src_y = preview_sample_coord(preview_y, preview_height, source.height());
    let row = source.extract_tile_rgba32f_arc(0, src_y, source.width(), 1)?;
    let mut rgba_f32 = Vec::with_capacity(preview_width as usize * 4);
    for preview_x in 0..preview_width {
        let src_x = preview_sample_coord(preview_x, preview_width, source.width()) as usize;
        let offset = src_x * 4;
        rgba_f32.extend_from_slice(&row.rgba_f32[offset..offset + 4]);
    }
    Ok(rgba_f32)
}

pub(crate) fn sdr_preview_from_hdr_preview(
    preview: &HdrImageBuffer,
) -> Result<(u32, u32, Vec<u8>), String> {
    let mut pixels = crate::hdr::decode::hdr_to_sdr_rgba8(preview, 0.0)?;
    make_visible_preview_opaque_if_alpha_is_empty(&mut pixels);
    let image = image::RgbaImage::from_raw(preview.width, preview.height, pixels)
        .ok_or_else(|| "Failed to build SDR preview image from HDR preview".to_string())?;
    Ok((image.width(), image.height(), image.into_raw()))
}

fn make_visible_preview_opaque_if_alpha_is_empty(pixels: &mut [u8]) {
    if pixels.chunks_exact(4).any(|pixel| pixel[3] != 0) {
        return;
    }

    let has_visible_rgb = pixels
        .chunks_exact(4)
        .any(|pixel| pixel[0] != 0 || pixel[1] != 0 || pixel[2] != 0);
    if !has_visible_rgb {
        return;
    }

    for pixel in pixels.chunks_exact_mut(4) {
        if pixel[0] != 0 || pixel[1] != 0 || pixel[2] != 0 {
            pixel[3] = u8::MAX;
        }
    }
}

pub(crate) fn preview_dimensions(width: u32, height: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    if width == 0 || height == 0 || max_w == 0 || max_h == 0 {
        return (0, 0);
    }
    let scale = (max_w as f32 / width as f32)
        .min(max_h as f32 / height as f32)
        .min(1.0);
    let preview_width = ((width as f32 * scale).round() as u32).clamp(1, max_w);
    let preview_height = ((height as f32 * scale).round() as u32).clamp(1, max_h);
    (preview_width, preview_height)
}

pub(crate) fn preview_sample_coord(
    preview_coord: u32,
    preview_extent: u32,
    source_extent: u32,
) -> u32 {
    if preview_extent <= 1 {
        return 0;
    }
    ((u64::from(preview_coord) * u64::from(source_extent - 1)) / u64::from(preview_extent - 1))
        as u32
}

const fn initial_hdr_tile_cache_max_bytes() -> usize {
    DEFAULT_HDR_TILE_CACHE_MAX_BYTES
}

pub fn configured_hdr_tile_cache_max_bytes() -> usize {
    HDR_TILE_CACHE_MAX_BYTES.load(Ordering::Relaxed)
}

pub(crate) fn configure_hdr_tile_cache_budget_from_system_memory() {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    HDR_TILE_CACHE_MAX_BYTES.store(
        hdr_tile_cache_budget_for_memory(sys.total_memory() as usize),
        Ordering::Relaxed,
    );
}

fn hdr_tile_cache_budget_for_memory(total_memory_bytes: usize) -> usize {
    (total_memory_bytes / 16).clamp(
        DEFAULT_HDR_TILE_CACHE_MAX_BYTES,
        MAX_HDR_TILE_CACHE_MAX_BYTES,
    )
}

#[cfg(test)]
fn set_global_hdr_tile_cache_max_bytes_for_tests(max_bytes: usize) {
    HDR_TILE_CACHE_MAX_BYTES.store(max_bytes, Ordering::Relaxed);
}

#[derive(Debug)]
pub(crate) struct HdrTileCache {
    entries: HashMap<HdrTileCacheKey, Arc<HdrTileBuffer>>,
    lru: VecDeque<HdrTileCacheKey>,
    protected: HashSet<HdrTileCacheKey>,
    current_bytes: usize,
    max_bytes: usize,
}

impl HdrTileCache {
    pub(crate) fn new(max_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            lru: VecDeque::new(),
            protected: HashSet::new(),
            current_bytes: 0,
            max_bytes,
        }
    }

    pub(crate) fn get(&mut self, key: HdrTileCacheKey) -> Option<Arc<HdrTileBuffer>> {
        let tile = self.entries.get(&key).cloned()?;
        self.touch(key);
        Some(tile)
    }

    pub(crate) fn insert(&mut self, key: HdrTileCacheKey, tile: Arc<HdrTileBuffer>) {
        if let Some(old_tile) = self.entries.remove(&key) {
            self.current_bytes = self.current_bytes.saturating_sub(tile_len_bytes(&old_tile));
            self.lru.retain(|existing| *existing != key);
        }

        let bytes = tile_len_bytes(&tile);
        while !self.lru.is_empty() && self.current_bytes.saturating_add(bytes) > self.max_bytes {
            let evict_pos = self
                .lru
                .iter()
                .position(|existing| !self.protected.contains(existing))
                .unwrap_or(0);
            let Some(evicted_key) = self.lru.remove(evict_pos) else {
                break;
            };
            if let Some(evicted_tile) = self.entries.remove(&evicted_key) {
                self.current_bytes = self
                    .current_bytes
                    .saturating_sub(tile_len_bytes(&evicted_tile));
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

    pub(crate) fn set_protected_keys(&mut self, keys: impl IntoIterator<Item = HdrTileCacheKey>) {
        self.protected.clear();
        self.protected.extend(keys);
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    pub(crate) fn current_bytes(&self) -> usize {
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
pub(crate) fn validate_tile_bounds(
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
    use std::sync::Mutex;

    use crate::hdr::tiled::{
        HdrTileBuffer, HdrTiledImageSource, HdrTiledSource, HdrTiledSourceKind,
        configured_hdr_tile_cache_max_bytes, set_global_hdr_tile_cache_max_bytes_for_tests,
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
    fn in_memory_hdr_tile_source_can_be_used_through_trait_object() {
        let source: Arc<dyn HdrTiledSource> =
            Arc::new(HdrTiledImageSource::new(test_image(2, 1)).expect("valid HDR tile source"));

        assert_eq!(source.source_kind(), HdrTiledSourceKind::InMemory);
        assert_eq!(source.source_kind().as_str(), "in-memory");
        assert_eq!(source.width(), 2);
        assert_eq!(source.height(), 1);
        let tile = source
            .extract_tile_rgba32f_arc(1, 0, 1, 1)
            .expect("extract through trait object");
        assert_eq!(tile.width, 1);
        assert_eq!(tile.height, 1);
        assert_eq!(tile.color_space, HdrColorSpace::LinearSrgb);
        assert_eq!(tile.rgba_f32.as_slice(), &[1.0, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn in_memory_hdr_tile_source_generates_hdr_preview() {
        let source = HdrTiledImageSource::new(test_image(4, 2)).expect("valid HDR tile source");

        let preview = source
            .generate_hdr_preview(2, 1)
            .expect("generate HDR preview");

        assert_eq!((preview.width, preview.height), (2, 1));
        assert_eq!(preview.format, HdrPixelFormat::Rgba32Float);
        assert_eq!(preview.color_space, HdrColorSpace::LinearSrgb);
        assert_eq!(preview.rgba_f32.len(), 2 * 4);
    }

    #[test]
    fn tile_backed_hdr_preview_samples_expected_source_pixels() {
        let pixels = (0..12)
            .flat_map(|value| {
                let value = value as f32;
                [value, value, value, 1.0]
            })
            .collect();
        let source = HdrTiledImageSource::new(HdrImageBuffer {
            width: 4,
            height: 3,
            format: HdrPixelFormat::Rgba32Float,
            color_space: HdrColorSpace::LinearSrgb,
            rgba_f32: Arc::new(pixels),
        })
        .expect("valid HDR tile source");

        let preview = super::hdr_preview_from_tiled_source_nearest(&source, 2, 2)
            .expect("generate tiled HDR preview");

        assert_eq!((preview.width, preview.height), (2, 2));
        assert_eq!(
            preview.rgba_f32.as_slice(),
            &[
                0.0, 0.0, 0.0, 1.0, 3.0, 3.0, 3.0, 1.0, 8.0, 8.0, 8.0, 1.0, 11.0, 11.0, 11.0, 1.0,
            ]
        );
    }

    #[test]
    fn disk_backed_hdr_preview_samples_each_output_row() {
        let source = RecordingDiskBackedSource::new(1, 128);

        let preview = super::hdr_preview_from_tiled_source_nearest(&source, 64, 64)
            .expect("generate disk-backed preview");

        let mut requested_rows = source
            .requested_rows
            .lock()
            .expect("read requested rows")
            .clone();
        requested_rows.sort_unstable();
        requested_rows.dedup();
        assert_eq!((preview.width, preview.height), (1, 64));
        assert_eq!(requested_rows.len(), 64);
        assert_eq!(requested_rows[0], 0);
        assert_eq!(requested_rows[63], 127);
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
    fn hdr_tile_cache_keeps_protected_visible_tiles_when_over_budget() {
        let mut cache = super::HdrTileCache::new(2 * tile_bytes(1, 1));
        let first_key = (0, 0, 1, 1);
        let second_key = (1, 0, 1, 1);
        let third_key = (2, 0, 1, 1);

        cache.insert(first_key, Arc::new(hdr_tile(1, 1, 1.0)));
        cache.insert(second_key, Arc::new(hdr_tile(1, 1, 2.0)));
        cache.set_protected_keys([first_key]);
        cache.insert(third_key, Arc::new(hdr_tile(1, 1, 3.0)));

        assert!(cache.get(first_key).is_some());
        assert!(cache.get(third_key).is_some());
        assert!(cache.get(second_key).is_none());
        assert!(cache.current_bytes() <= 2 * tile_bytes(1, 1));
    }

    #[test]
    fn hdr_tile_cache_budget_scales_with_physical_memory() {
        let gib = 1024 * 1024 * 1024;

        assert_eq!(
            super::hdr_tile_cache_budget_for_memory(4 * gib),
            256 * 1024 * 1024
        );
        assert_eq!(super::hdr_tile_cache_budget_for_memory(32 * gib), 2 * gib);
        assert_eq!(super::hdr_tile_cache_budget_for_memory(128 * gib), 4 * gib);
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

    #[test]
    fn sdr_preview_is_exposure_neutral_for_hdr_source_tiles() {
        let source = HdrTiledImageSource::new(HdrImageBuffer {
            width: 1,
            height: 1,
            format: HdrPixelFormat::Rgba32Float,
            color_space: HdrColorSpace::LinearSrgb,
            rgba_f32: Arc::new(vec![4.0, 4.0, 4.0, 1.0]),
        })
        .expect("valid HDR tile source");

        let (_width, _height, pixels) = source
            .generate_sdr_preview(1, 1)
            .expect("generate SDR preview");

        assert_eq!(
            pixels[0], 230,
            "fallback previews intentionally use neutral exposure; user exposure is applied by HDR rendering uniforms"
        );
    }

    #[test]
    fn sdr_preview_keeps_visible_rgb_opaque_when_alpha_is_zero_everywhere() {
        let preview = HdrImageBuffer {
            width: 1,
            height: 1,
            format: HdrPixelFormat::Rgba32Float,
            color_space: HdrColorSpace::LinearSrgb,
            rgba_f32: Arc::new(vec![0.25, 0.5, 1.0, 0.0]),
        };

        let (_width, _height, pixels) =
            super::sdr_preview_from_hdr_preview(&preview).expect("generate SDR preview");

        assert_ne!(
            pixels[3], 0,
            "visible RGB previews should not become fully transparent"
        );
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

    fn hdr_tile(width: u32, height: u32, value: f32) -> super::HdrTileBuffer {
        super::HdrTileBuffer::new(
            width,
            height,
            HdrColorSpace::LinearSrgb,
            Arc::new(vec![value; width as usize * height as usize * 4]),
        )
    }

    struct RecordingDiskBackedSource {
        width: u32,
        height: u32,
        requested_rows: Mutex<Vec<u32>>,
    }

    impl RecordingDiskBackedSource {
        fn new(width: u32, height: u32) -> Self {
            Self {
                width,
                height,
                requested_rows: Mutex::new(Vec::new()),
            }
        }
    }

    impl HdrTiledSource for RecordingDiskBackedSource {
        fn source_kind(&self) -> HdrTiledSourceKind {
            HdrTiledSourceKind::DiskBacked
        }

        fn width(&self) -> u32 {
            self.width
        }

        fn height(&self) -> u32 {
            self.height
        }

        fn color_space(&self) -> HdrColorSpace {
            HdrColorSpace::LinearSrgb
        }

        fn generate_hdr_preview(&self, max_w: u32, max_h: u32) -> Result<HdrImageBuffer, String> {
            super::hdr_preview_from_tiled_source_nearest(self, max_w, max_h)
        }

        fn generate_sdr_preview(
            &self,
            max_w: u32,
            max_h: u32,
        ) -> Result<(u32, u32, Vec<u8>), String> {
            let preview = self.generate_hdr_preview(max_w, max_h)?;
            super::sdr_preview_from_hdr_preview(&preview)
        }

        fn extract_tile_rgba32f_arc(
            &self,
            x: u32,
            y: u32,
            width: u32,
            height: u32,
        ) -> Result<Arc<HdrTileBuffer>, String> {
            assert_eq!(x, 0);
            assert_eq!(width, self.width);
            assert_eq!(height, 1);
            self.requested_rows
                .lock()
                .expect("record requested row")
                .push(y);
            Ok(Arc::new(HdrTileBuffer::new(
                width,
                height,
                HdrColorSpace::LinearSrgb,
                Arc::new(vec![y as f32, y as f32, y as f32, 1.0]),
            )))
        }
    }
}
