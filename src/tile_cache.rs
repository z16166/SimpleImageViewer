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

use std::collections::{HashMap, VecDeque, HashSet};
use std::sync::Arc;
use egui::TextureHandle;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, LazyLock};

/// Tile size in pixels (each tile is TILE_SIZE x TILE_SIZE).
pub const TILE_SIZE: u32 = 512;

/// Pixel count threshold above which tiled mode is activated.
/// 64 megapixels = 8000x8000. Images below this use the normal full-upload path.
pub const TILED_THRESHOLD: u64 = 64_000_000;

/// Maximum texture side length supported by most GPUs (conservative limit).
/// Large images exceeding this will be rendered using tiles.
/// This value is updated dynamically at startup based on GPU hardware limits.
pub static MAX_TEXTURE_SIDE: AtomicU32 = AtomicU32::new(8192);

pub fn get_max_texture_side() -> u32 {
    MAX_TEXTURE_SIDE.load(Ordering::Relaxed)
}

/// Base maximum number of tile textures kept in GPU memory.
/// 512 tiles * 512x512 * 4 bytes = 512MB VRAM.
/// This acts as a floor; the cache can expand to fit all currently visible tiles.
const MAX_TILES_BASE: usize = 512;

/// Coordinate of a tile within the grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileCoord {
    pub col: u32,
    pub row: u32,
}

/// Global cache for decoded tile pixels (CPU RAM).
/// Each tile (512x512 RGBA) is exactly 1MB.
pub struct TilePixelCache {
    /// Key: (image_index, col, row)
    entries: HashMap<(usize, u32, u32), Arc<Vec<u8>>>,
    /// LRU tracking
    lru: VecDeque<(usize, u32, u32)>,
    current_bytes: usize,
    max_mb: usize,
}

impl TilePixelCache {
    pub fn new(_max_mb: usize) -> Self {
        Self {
            entries: HashMap::new(),
            lru: VecDeque::new(),
            current_bytes: 0,
            max_mb: 1024,
        }
    }

    #[cfg(feature = "tile-debug")]
    pub fn count_for_image(&self, index: usize) -> usize {
        self.entries.keys().filter(|(idx, _, _)| *idx == index).count()
    }

    pub fn get(&mut self, index: usize, coord: TileCoord) -> Option<Arc<Vec<u8>>> {
        let key = (index, coord.col, coord.row);
        if let Some(pixels) = self.entries.get(&key) {
            // Update LRU
            if let Some(pos) = self.lru.iter().position(|k| *k == key) {
                self.lru.remove(pos);
            }
            self.lru.push_back(key);
            Some(Arc::clone(pixels))
        } else {
            None
        }
    }

    pub fn insert(&mut self, index: usize, coord: TileCoord, pixels: Vec<u8>) {
        let key = (index, coord.col, coord.row);
        let bytes = pixels.len();
        let pixels = Arc::new(pixels);
        let max_bytes = self.max_mb * 1024 * 1024;

        // Evict if needed
        while !self.lru.is_empty() && self.current_bytes + bytes > max_bytes {
            if let Some(evicted_key) = self.lru.pop_front() {
                if let Some(evicted_pixels) = self.entries.remove(&evicted_key) {
                    self.current_bytes -= evicted_pixels.len();
                }
            }
        }

        if self.current_bytes + bytes <= max_bytes {
            self.entries.insert(key, Arc::clone(&pixels));
            self.lru.push_back(key);
            self.current_bytes += bytes;
        }
    }
}

/// The global tile pixel cache instance.
pub static PIXEL_CACHE: LazyLock<Mutex<TilePixelCache>> = LazyLock::new(|| {
    Mutex::new(TilePixelCache::new(512)) // Default 512MB, will be updated by settings
});

/// Manages the tiled rendering state for a single large image.
pub struct TileManager {
    /// Current image index in the folder (used for cache lookups).
    pub image_index: usize,
    /// Full image dimensions (original pixels).
    pub full_width: u32,
    pub full_height: u32,
    /// Generation ID of the load that created this manager.
    pub generation: u64,

    /// The downscaled preview texture (fits on screen).
    pub preview_texture: Option<TextureHandle>,

    /// The source of pixel data (could be CPU RAM or an on-demand Disk source).
    source: Arc<dyn crate::loader::TiledImageSource>,

    /// Cached tile textures already uploaded to GPU.
    tiles: HashMap<TileCoord, TextureHandle>,

    /// Tiles currently being decoded in the background.
    pub pending_tiles: HashSet<TileCoord>,

    /// LRU ordering: most recently used tiles at the back.
    lru_order: Vec<TileCoord>,
}

pub enum TileStatus {
    Ready(TextureHandle),
    /// Tile is either being decoded or needs to be requested.
    /// Returns true if it needs to be requested.
    Pending(bool),
}

/// A TiledImageSource implementation for images that are fully loaded in memory.
pub struct MemorySource {
    pub width: u32,
    pub height: u32,
    pub pixels: Arc<Vec<u8>>,
}

impl crate::loader::TiledImageSource for MemorySource {
    fn width(&self) -> u32 { self.width }
    fn height(&self) -> u32 { self.height }
    
    fn extract_tile(&self, x: u32, y: u32, w: u32, h: u32) -> Vec<u8> {
        let src_stride = self.width as usize * 4;
        let mut tile_pixels = vec![0u8; (w * h * 4) as usize];

        for row in 0..h {
            let src_off = ((y + row) as usize) * src_stride + (x as usize * 4);
            let dst_off = (row as usize) * (w as usize * 4);
            let len = w as usize * 4;
            
            let end = src_off + len;
            if end <= self.pixels.len() {
                tile_pixels[dst_off..dst_off + len].copy_from_slice(&self.pixels[src_off..end]);
            }
        }
        tile_pixels
    }

    fn generate_preview(&self, max_w: u32, max_h: u32) -> (u32, u32, Vec<u8>) {
        let scale = (max_w as f64 / self.width as f64)
            .min(max_h as f64 / self.height as f64)
            .min(1.0);
        let out_w = (self.width as f64 * scale).round().max(1.0) as u32;
        let out_h = (self.height as f64 * scale).round().max(1.0) as u32;

        let mut out = vec![0u8; (out_w * out_h * 4) as usize];
        let src_stride = self.width as usize * 4;

        for y in 0..out_h {
            let src_y = ((y as f64 / scale).min((self.height - 1) as f64)) as usize;
            for x in 0..out_w {
                let src_x = ((x as f64 / scale).min((self.width - 1) as f64)) as usize;
                let src_off = src_y * src_stride + src_x * 4;
                let dst_off = (y as usize * out_w as usize + x as usize) * 4;
                if src_off + 4 <= self.pixels.len() {
                    out[dst_off..dst_off + 4].copy_from_slice(&self.pixels[src_off..src_off + 4]);
                }
            }
        }
        (out_w, out_h, out)
    }

    fn full_pixels(&self) -> Option<Arc<Vec<u8>>> {
        Some(Arc::clone(&self.pixels))
    }
}

impl TileManager {
    /// Create a new TileManager from a fully decoded RGBA8 pixel buffer.
    pub fn new(index: usize, generation: u64, width: u32, height: u32, pixels: Vec<u8>) -> Self {
        let source = Arc::new(MemorySource {
            width,
            height,
            pixels: Arc::new(pixels),
        });
        Self::with_source(index, generation, source)
    }

    /// Create a new TileManager from an arbitrary tiled image source.
    pub fn with_source(index: usize, generation: u64, source: Arc<dyn crate::loader::TiledImageSource>) -> Self {
        Self {
            image_index: index,
            full_width: source.width(),
            full_height: source.height(),
            generation,
            preview_texture: None,
            source,
            tiles: HashMap::new(),
            pending_tiles: HashSet::new(),
            lru_order: Vec::new(),
        }
    }

    /// Returns a cheap Arc clone of the raw pixel buffer if available.
    pub fn pixel_buffer_arc(&self) -> Option<Arc<Vec<u8>>> {
        self.source.full_pixels()
    }

    pub fn get_source(&self) -> Arc<dyn crate::loader::TiledImageSource> {
        Arc::clone(&self.source)
    }

    /// Returns counts for the current visible set using a non-blocking try_lock: (gpu, cpu_ready, pending)
    #[cfg(feature = "tile-debug")]
    pub fn stats_for_visible(&self, visible: &[TileCoord]) -> (usize, usize, usize) {
        let mut gpu = 0;
        let mut cpu = 0;
        let mut pending = 0;
        
        if let Ok(cache) = PIXEL_CACHE.try_lock() {
            for coord in visible {
                if self.tiles.contains_key(coord) {
                    gpu += 1;
                } else if cache.entries.contains_key(&(self.image_index, coord.col, coord.row)) {
                    cpu += 1;
                } else {
                    pending += 1;
                }
            }
        }
        (gpu, cpu, pending)
    }

    /// Returns global counts using a non-blocking try_lock
    #[cfg(feature = "tile-debug")]
    pub fn tiles_and_pending(&self) -> (usize, usize, usize) {
        let cpu_cached = if let Ok(cache) = PIXEL_CACHE.try_lock() {
            cache.count_for_image(self.image_index)
        } else {
            0
        };
        (self.tiles.len(), cpu_cached, self.pending_tiles.len())
    }

    /// Returns true if any of the visible tiles are in CPU cache but NOT in GPU.
    pub fn has_ready_to_upload(&self, visible: &[TileCoord]) -> bool {
        let cache = match PIXEL_CACHE.lock() {
            Ok(c) => c,
            Err(_) => return false,
        };
        
        for coord in visible {
            if !self.tiles.contains_key(coord) {
                if cache.entries.contains_key(&(self.image_index, coord.col, coord.row)) {
                    return true;
                }
            }
        }
        false
    }

    pub fn set_preview(&mut self, preview: crate::loader::DecodedImage, ctx: &egui::Context) {
        let name = format!("preview_{}", self.image_index);
        let color_image = egui::ColorImage::from_rgba_unmultiplied(
            [preview.width as usize, preview.height as usize],
            &preview.pixels,
        );
        self.preview_texture = Some(ctx.load_texture(name, color_image, egui::TextureOptions::LINEAR));
    }

    /// Number of tile columns in the grid.
    pub fn cols(&self) -> u32 {
        (self.full_width + TILE_SIZE - 1) / TILE_SIZE
    }

    /// Number of tile rows in the grid.
    pub fn rows(&self) -> u32 {
        (self.full_height + TILE_SIZE - 1) / TILE_SIZE
    }


    /// Get or create a tile texture for the given coordinate.
    /// Returns (TileStatus, newly_uploaded).
    pub fn get_or_create_tile(
        &mut self,
        coord: TileCoord,
        ctx: &egui::Context,
        allow_upload: bool,
        visible_count: usize,
    ) -> (TileStatus, bool) {
        // Touch LRU
        if let Some(pos) = self.lru_order.iter().position(|c| *c == coord) {
            self.lru_order.remove(pos);
        }
        self.lru_order.push(coord);

        // check if exists in GPU
        if let Some(handle) = self.tiles.get(&coord) {
            return (TileStatus::Ready(handle.clone()), false);
        }

        // 1. Check Global Pixel Cache (CPU)
        let cached_pixels: Option<Arc<Vec<u8>>> = {
            if let Ok(mut cache) = PIXEL_CACHE.lock() {
                cache.get(self.image_index, coord)
            } else {
                None
            }
        };

        if let Some(pixels) = cached_pixels {
            if allow_upload {
                // Adaptive eviction: Ensure we never evict tiles that are in the current visible set.
                // Limit = max(BASE_LIMIT, visible_count)
                let current_limit = MAX_TILES_BASE.max(visible_count);

                // Evict if over limit
                while self.lru_order.len() > current_limit {
                    let evicted = self.lru_order.remove(0);
                    self.tiles.remove(&evicted);
                }

                let tw = TILE_SIZE.min(self.full_width - coord.col * TILE_SIZE);
                let th = TILE_SIZE.min(self.full_height - coord.row * TILE_SIZE);

                let color_image = egui::ColorImage::from_rgba_unmultiplied(
                    [tw as usize, th as usize],
                    &**pixels,
                );
                let name = format!("tile_{}_{}_{}", self.image_index, coord.col, coord.row);
                let handle = ctx.load_texture(name, color_image, egui::TextureOptions::LINEAR);
                self.tiles.insert(coord, handle.clone());
                
                // Remove from pending if it was there
                self.pending_tiles.remove(&coord);
                
                return (TileStatus::Ready(handle), true);
            }

            // If it's in CPU cache but we didn't upload it (quota reached),
            // tell caller it's pending but DOES NOT need a new background request.
            return (TileStatus::Pending(false), false);
        }

        // If we reach here, it's not in GPU and not in CPU cache.
        let needs_request = !self.pending_tiles.contains(&coord);
        (TileStatus::Pending(needs_request), false)
    }

    /// Clear all cached tiles (e.g. when switching images).
    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.tiles.clear();
        self.lru_order.clear();
        self.preview_texture = None;
        // The source's lifecycle is managed via Arc.
    }

    /// Compute which tiles are visible given the current viewport mapping.
    /// `viewport` is the screen-space rectangle where the full image would be displayed.
    /// `screen_clip` is the visible screen area (to clip against).
    /// Returns a list of (TileCoord, screen_rect, uv_rect) tuples.
    pub fn visible_tiles(
        &self,
        viewport: egui::Rect,
        screen_clip: egui::Rect,
    ) -> Vec<(TileCoord, egui::Rect, egui::Rect)> {
        let visible_area = viewport.intersect(screen_clip);
        if visible_area.width() <= 0.0 || visible_area.height() <= 0.0 {
            return Vec::new();
        }

        let mut result = Vec::new();
        let cols = self.cols();
        let rows = self.rows();

        for r in 0..rows {
            for c in 0..cols {
                let tile_x0 = c * TILE_SIZE;
                let tile_y0 = r * TILE_SIZE;
                let tile_w = TILE_SIZE.min(self.full_width - tile_x0);
                let tile_h = TILE_SIZE.min(self.full_height - tile_y0);

                // Map tile pixel bounds to screen coordinates
                let sx0 = viewport.min.x + (tile_x0 as f32 / self.full_width as f32) * viewport.width();
                let sy0 = viewport.min.y + (tile_y0 as f32 / self.full_height as f32) * viewport.height();
                let sx1 = viewport.min.x + ((tile_x0 + tile_w) as f32 / self.full_width as f32) * viewport.width();
                let sy1 = viewport.min.y + ((tile_y0 + tile_h) as f32 / self.full_height as f32) * viewport.height();

                let tile_screen_rect = egui::Rect::from_min_max(
                    egui::Pos2::new(sx0, sy0),
                    egui::Pos2::new(sx1, sy1),
                );

                if screen_clip.intersects(tile_screen_rect) {
                    let uv = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0));
                    result.push((TileCoord { col: c, row: r }, tile_screen_rect, uv));
                }
            }
        }

        // Sort by distance from screen center (Center-Out Priority)
        let screen_center = screen_clip.center();
        result.sort_by_key(|(_, rect, _)| {
            let dist_sq = (rect.center().x - screen_center.x).powi(2) + 
                          (rect.center().y - screen_center.y).powi(2);
            (dist_sq * 10.0) as i32
        });

        result
    }
}
