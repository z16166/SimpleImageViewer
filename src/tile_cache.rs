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

use eframe::egui::{self, TextureHandle};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

/// Tile size in pixels (each tile is tile_size x tile_size).
/// Dynamically adjusted: 1024 for gigapixel images (>500MP), 512 otherwise.
pub static TILE_SIZE: AtomicU32 = AtomicU32::new(512);

/// Get the current tile size.
pub fn get_tile_size() -> u32 {
    TILE_SIZE.load(Ordering::Relaxed)
}

/// Set tile size based on image dimensions.
/// Also adjusts MAX_TILES_BASE to maintain constant VRAM budget.
pub fn set_tile_size_for_image(width: u32, height: u32) {
    let megapixels = (width as u64 * height as u64) / 1_000_000;
    let size = if megapixels > 500 { 1024 } else { 512 };
    TILE_SIZE.store(size, Ordering::Relaxed);
    // Keep VRAM budget constant: 512 tiles * 512*512*4 = 512MB
    // For 1024 tiles: 128 * 1024*1024*4 = 512MB
    let max_tiles = if size == 1024 { 128 } else { 512 };
    MAX_TILES_BASE.store(max_tiles, Ordering::Relaxed);
}

/// Pixel count threshold above which tiled mode is activated.
/// Updated dynamically based on HardwareTier in app.rs.
pub static TILED_THRESHOLD: AtomicU64 = AtomicU64::new(64_000_000);

/// Maximum texture side length supported by most GPUs (conservative limit).
/// Large images exceeding this will be rendered using tiles.
/// This value is updated dynamically at startup based on GPU hardware limits.
pub static MAX_TEXTURE_SIDE: AtomicU32 =
    AtomicU32::new(crate::constants::ABSOLUTE_MAX_TEXTURE_SIDE);

pub fn get_max_texture_side() -> u32 {
    MAX_TEXTURE_SIDE.load(Ordering::Relaxed)
}

/// Base maximum number of tile textures kept in GPU memory.
/// 1024 tiles * 512x512 * 4 bytes = 1GB VRAM.
/// This acts as a floor; the cache can expand to fit all currently visible tiles.
pub static MAX_TILES_BASE: AtomicUsize = AtomicUsize::new(512);

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
    pub fn new(max_mb: usize) -> Self {
        Self {
            entries: HashMap::new(),
            lru: VecDeque::new(),
            current_bytes: 0,
            max_mb,
        }
    }

    pub fn set_max_mb(&mut self, max_mb: usize) {
        self.max_mb = max_mb;
    }

    #[cfg(feature = "tile-debug")]
    pub fn count_for_image(&self, index: usize) -> usize {
        self.entries
            .keys()
            .filter(|(idx, _, _)| *idx == index)
            .count()
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

    pub fn insert(&mut self, index: usize, coord: TileCoord, pixels: Arc<Vec<u8>>) {
        let key = (index, coord.col, coord.row);

        // Handle duplicate insertions: remove old entry first to update memory accounting and LRU
        if let Some(old_pixels) = self.entries.remove(&key) {
            self.current_bytes -= old_pixels.len();
            self.lru.retain(|&k| k != key);
        }

        let bytes = pixels.len();
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

    /// Remove all tiles belonging to a specific image index.
    pub fn remove_image(&mut self, index: usize) {
        let keys_to_remove: Vec<_> = self
            .entries
            .keys()
            .filter(|(idx, _, _)| *idx == index)
            .copied()
            .collect();

        for key in keys_to_remove {
            if let Some(pixels) = self.entries.remove(&key) {
                self.current_bytes -= pixels.len();
            }
            if let Some(pos) = self.lru.iter().position(|k| *k == key) {
                self.lru.remove(pos);
            }
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.lru.clear();
        self.current_bytes = 0;
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
    /// Timestamps when each tile was uploaded to GPU (for cross-fading).
    ready_times: HashMap<TileCoord, Instant>,
}

pub enum TileStatus {
    Ready(TextureHandle, Option<Instant>),
    /// Tile is either being decoded or needs to be requested.
    /// Returns true if it needs to be requested.
    Pending(bool),
}

impl TileManager {
    /// Create a new TileManager from an arbitrary tiled image source.
    pub fn with_source(
        index: usize,
        generation: u64,
        source: Arc<dyn crate::loader::TiledImageSource>,
    ) -> Self {
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
            ready_times: HashMap::new(),
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
                } else if cache
                    .entries
                    .contains_key(&(self.image_index, coord.col, coord.row))
                {
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
                if cache
                    .entries
                    .contains_key(&(self.image_index, coord.col, coord.row))
                {
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
            preview.rgba(),
        );
        self.preview_texture =
            Some(ctx.load_texture(name, color_image, egui::TextureOptions::LINEAR));
    }

    /// Number of tile columns in the grid.
    pub fn cols(&self) -> u32 {
        let ts = get_tile_size();
        (self.full_width + ts - 1) / ts
    }

    /// Number of tile rows in the grid.
    pub fn rows(&self) -> u32 {
        let ts = get_tile_size();
        (self.full_height + ts - 1) / ts
    }

    /// Get or create a tile texture for the given coordinate.
    /// Returns (TileStatus, newly_uploaded).
    pub fn get_or_create_tile(
        &mut self,
        coord: TileCoord,
        ctx: &egui::Context,
        allow_upload: bool,
        visible_coords: &[TileCoord],
    ) -> (TileStatus, bool) {
        // Touch LRU
        if let Some(pos) = self.lru_order.iter().position(|c| *c == coord) {
            self.lru_order.remove(pos);
        }
        self.lru_order.push(coord);

        // check if exists in GPU
        if let Some(handle) = self.tiles.get(&coord) {
            let ready_at = self.ready_times.get(&coord).cloned();
            return (TileStatus::Ready(handle.clone(), ready_at), false);
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
                // Strict eviction: Never exceed the base limit determined by HardwareTier.
                // We no longer expand the limit based on visible_count to prevent crashes.
                let current_limit = MAX_TILES_BASE.load(Ordering::Relaxed);

                // Evict if over limit, but NEVER evict tiles currently in the visible set.
                // This prevents the "circular hole" artifact on high-DPI screens.
                while self.lru_order.len() > current_limit {
                    let mut found_non_visible = false;
                    for i in 0..self.lru_order.len() {
                        let potential_evict = self.lru_order[i];
                        if !visible_coords.contains(&potential_evict) {
                            self.lru_order.remove(i);
                            self.tiles.remove(&potential_evict);
                            found_non_visible = true;
                            break;
                        }
                    }
                    if !found_non_visible {
                        // All cached tiles are currently visible!
                        // We must temporarily exceed the limit to maintain visual integrity.
                        break;
                    }
                }

                let ts = get_tile_size();
                let tw = ts.min(self.full_width - coord.col * ts);
                let th = ts.min(self.full_height - coord.row * ts);

                let color_image =
                    egui::ColorImage::from_rgba_unmultiplied([tw as usize, th as usize], &**pixels);
                let name = format!("tile_{}_{}_{}", self.image_index, coord.col, coord.row);
                let handle = ctx.load_texture(name, color_image, egui::TextureOptions::LINEAR);
                self.tiles.insert(coord, handle.clone());

                let now = Instant::now();
                self.ready_times.insert(coord, now);

                // Remove from pending if it was there
                self.pending_tiles.remove(&coord);

                return (TileStatus::Ready(handle, Some(now)), true);
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
        self.ready_times.clear();
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
        padding: f32,
    ) -> Vec<(TileCoord, egui::Rect, egui::Rect)> {
        // Look-ahead padding: Inflate the visible area to trigger background requests
        // for neighbor tiles BEFORE they actually enter the screen.
        let visible_area = viewport.intersect(screen_clip.expand(padding));

        if visible_area.width() <= 0.0 || visible_area.height() <= 0.0 {
            return Vec::new();
        }

        let mut result = Vec::new();

        // Compute UV bounds of the visible area relative to the full image viewport
        let uv_min_x = ((visible_area.min.x - viewport.min.x) / viewport.width()).clamp(0.0, 1.0);
        let uv_max_x = ((visible_area.max.x - viewport.min.x) / viewport.width()).clamp(0.0, 1.0);
        let uv_min_y = ((visible_area.min.y - viewport.min.y) / viewport.height()).clamp(0.0, 1.0);
        let uv_max_y = ((visible_area.max.y - viewport.min.y) / viewport.height()).clamp(0.0, 1.0);

        // Map to pixel coordinates
        let px_min_x = uv_min_x * self.full_width as f32;
        let px_max_x = uv_max_x * self.full_width as f32;
        let px_min_y = uv_min_y * self.full_height as f32;
        let px_max_y = uv_max_y * self.full_height as f32;

        // Determine the range of tile indices (cols/rows) that are visible.
        // We subtract a tiny epsilon from max bounds to avoid including an extra tile when
        // the viewport edge aligns exactly with a tile boundary.
        let ts = get_tile_size() as f32;
        let min_col = (px_min_x / ts).floor() as u32;
        let max_col = ((px_max_x - 0.01) / ts).floor() as u32;
        let min_row = (px_min_y / ts).floor() as u32;
        let max_row = ((px_max_y - 0.01) / ts).floor() as u32;

        let total_cols = self.cols();
        let total_rows = self.rows();

        let start_col = min_col.min(total_cols.saturating_sub(1));
        let end_col = max_col.min(total_cols.saturating_sub(1));
        let start_row = min_row.min(total_rows.saturating_sub(1));
        let end_row = max_row.min(total_rows.saturating_sub(1));

        for r in start_row..=end_row {
            for c in start_col..=end_col {
                let ts = get_tile_size();
                let tile_x0 = c * ts;
                let tile_y0 = r * ts;
                let tile_w = ts.min(self.full_width - tile_x0);
                let tile_h = ts.min(self.full_height - tile_y0);

                // Map tile pixel bounds to screen coordinates
                let sx0 =
                    viewport.min.x + (tile_x0 as f32 / self.full_width as f32) * viewport.width();
                let sy0 =
                    viewport.min.y + (tile_y0 as f32 / self.full_height as f32) * viewport.height();
                let sx1 = viewport.min.x
                    + ((tile_x0 + tile_w) as f32 / self.full_width as f32) * viewport.width();
                let sy1 = viewport.min.y
                    + ((tile_y0 + tile_h) as f32 / self.full_height as f32) * viewport.height();

                let tile_screen_rect =
                    egui::Rect::from_min_max(egui::Pos2::new(sx0, sy0), egui::Pos2::new(sx1, sy1));

                let uv = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0));
                result.push((TileCoord { col: c, row: r }, tile_screen_rect, uv));
            }
        }

        // Sort by distance from screen center (Center-Out Priority)
        let screen_center = screen_clip.center();
        result.sort_by_key(|(_, rect, _)| {
            let dist_sq = (rect.center().x - screen_center.x).powi(2)
                + (rect.center().y - screen_center.y).powi(2);
            (dist_sq * 10.0) as i32
        });

        result
    }
}
