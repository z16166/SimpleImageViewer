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
use parking_lot::{Mutex, RwLock};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PendingTileKey {
    pub coord: TileCoord,
    pub pixel_kind: crate::loader::TilePixelKind,
}

impl PendingTileKey {
    pub fn new(coord: TileCoord, pixel_kind: crate::loader::TilePixelKind) -> Self {
        Self { coord, pixel_kind }
    }
}

/// Global cache for decoded tile pixels (CPU RAM).
/// Each tile (512x512 RGBA) is exactly 1MB.
pub struct TilePixelCache {
    /// Key: (image_index, col, row)
    entries: HashMap<(usize, u32, u32), Arc<Vec<u8>>>,
    lru: Mutex<crate::lru_order::LruOrder<(usize, u32, u32)>>,
    current_bytes: usize,
    max_mb: usize,
}

impl TilePixelCache {
    pub fn new(max_mb: usize) -> Self {
        Self {
            entries: HashMap::new(),
            lru: Mutex::new(crate::lru_order::LruOrder::default()),
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

    pub fn contains_tile(&self, index: usize, coord: TileCoord) -> bool {
        self.entries.contains_key(&(index, coord.col, coord.row))
    }

    pub fn get(&self, index: usize, coord: TileCoord) -> Option<Arc<Vec<u8>>> {
        let key = (index, coord.col, coord.row);
        if let Some(pixels) = self.entries.get(&key) {
            self.lru.lock().touch(key);
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
            self.lru.lock().remove(key);
        }

        let bytes = pixels.len();
        let max_bytes = self.max_mb * 1024 * 1024;
        let mut lru = self.lru.lock();

        // Evict if needed
        while !lru.is_empty() && self.current_bytes + bytes > max_bytes {
            if let Some(evicted_key) = lru.pop_oldest()
                && let Some(evicted_pixels) = self.entries.remove(&evicted_key)
            {
                self.current_bytes -= evicted_pixels.len();
            }
        }

        if self.current_bytes + bytes <= max_bytes {
            self.entries.insert(key, Arc::clone(&pixels));
            lru.touch(key);
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
            self.lru.lock().remove(key);
        }
    }

    /// Remove all tiles belonging to any of the provided image indices.
    pub fn remove_images(&mut self, indices: &std::collections::HashSet<usize>) {
        if indices.is_empty() {
            return;
        }
        let keys_to_remove: Vec<_> = self
            .entries
            .keys()
            .filter(|(idx, _, _)| indices.contains(idx))
            .copied()
            .collect();

        for key in keys_to_remove {
            if let Some(pixels) = self.entries.remove(&key) {
                self.current_bytes -= pixels.len();
            }
            self.lru.lock().remove(key);
        }
    }

    pub fn relocate_image(&mut self, from: usize, to: usize) {
        if from == to {
            return;
        }
        let keys_to_relocate: Vec<_> = self
            .entries
            .keys()
            .filter(|&&(idx, _, _)| idx == from)
            .copied()
            .collect();
        let mut lru = self.lru.lock();
        for key in keys_to_relocate {
            if let Some(pixels) = self.entries.remove(&key) {
                let new_key = (to, key.1, key.2);
                self.entries.insert(new_key, pixels);
                lru.rename(key, new_key);
            }
        }
    }

    pub fn permute_images(&mut self, old_to_new: &[usize]) {
        if old_to_new.is_empty() {
            return;
        }
        let keys: Vec<_> = self.entries.keys().copied().collect();
        for key in keys {
            let idx = key.0;
            if idx >= old_to_new.len() {
                continue;
            }
            let new_idx = old_to_new[idx];
            if new_idx == idx {
                continue;
            }
            if let Some(pixels) = self.entries.remove(&key) {
                debug_assert!(
                    !self.entries.contains_key(&(new_idx, key.1, key.2)),
                    "permute_images: duplicate target index {new_idx} for tile key {:?}",
                    key
                );
                self.entries.insert((new_idx, key.1, key.2), pixels);
            }
        }
        self.lru.lock().remap_ordered(|(idx, col, row)| {
            if idx >= old_to_new.len() {
                Some((idx, col, row))
            } else {
                let new_idx = old_to_new[idx];
                if new_idx == idx {
                    Some((idx, col, row))
                } else {
                    Some((new_idx, col, row))
                }
            }
        });
    }

    pub fn remove_images_except(&mut self, except_idx: usize) {
        let keys_to_remove: Vec<_> = self
            .entries
            .keys()
            .filter(|&&(idx, _, _)| idx != except_idx)
            .copied()
            .collect();
        for key in keys_to_remove {
            if let Some(pixels) = self.entries.remove(&key) {
                self.current_bytes -= pixels.len();
            }
            self.lru.lock().remove(key);
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.lru.lock().clear();
        self.current_bytes = 0;
    }

    pub fn has_image(&self, image_index: usize) -> bool {
        self.entries.keys().any(|(idx, _, _)| *idx == image_index)
    }

    /// Unique image indices with at least one cached tile (diagnostics / legacy scans).
    #[allow(dead_code)]
    pub fn distinct_image_indices(&self) -> Vec<usize> {
        let mut indices: Vec<usize> = self.entries.keys().map(|(idx, _, _)| *idx).collect();
        indices.sort_unstable();
        indices.dedup();
        indices
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyTileSource {
        width: u32,
        height: u32,
    }

    impl crate::loader::TiledImageSource for DummyTileSource {
        fn width(&self) -> u32 {
            self.width
        }

        fn height(&self) -> u32 {
            self.height
        }

        fn extract_tile(&self, _x: u32, _y: u32, w: u32, h: u32) -> Arc<Vec<u8>> {
            Arc::new(vec![0; w as usize * h as usize * 4])
        }

        fn generate_preview(&self, _max_w: u32, _max_h: u32) -> (u32, u32, Vec<u8>) {
            (1, 1, vec![0, 0, 0, 255])
        }

        fn full_pixels(&self) -> Option<Arc<Vec<u8>>> {
            None
        }
    }

    #[test]
    fn retain_pending_tiles_drops_offscreen_entries() {
        let source = Arc::new(DummyTileSource {
            width: 4096,
            height: 4096,
        });
        let mut manager = TileManager::with_source(0, crate::loader::decode_profile_stub(), source);
        manager.pending_tiles.insert(PendingTileKey::new(
            TileCoord { col: 0, row: 0 },
            crate::loader::TilePixelKind::Sdr,
        ));
        manager.pending_tiles.insert(PendingTileKey::new(
            TileCoord { col: 4, row: 4 },
            crate::loader::TilePixelKind::Sdr,
        ));

        manager.retain_pending_tiles(&HashSet::from([TileCoord { col: 0, row: 0 }]));

        assert!(manager.pending_tiles.contains(&PendingTileKey::new(
            TileCoord { col: 0, row: 0 },
            crate::loader::TilePixelKind::Sdr,
        )));
        assert!(!manager.pending_tiles.contains(&PendingTileKey::new(
            TileCoord { col: 4, row: 4 },
            crate::loader::TilePixelKind::Sdr,
        )));
    }

    #[test]
    fn pending_tile_keys_distinguish_sdr_and_hdr_for_same_coord() {
        let coord = TileCoord { col: 1, row: 2 };

        assert_ne!(
            PendingTileKey::new(coord, crate::loader::TilePixelKind::Sdr),
            PendingTileKey::new(coord, crate::loader::TilePixelKind::Hdr)
        );
    }

    #[test]
    fn test_tile_pixel_cache_relocate_and_remove_except() {
        let mut cache = TilePixelCache::new(512);
        let pixels = Arc::new(vec![0; 100]);

        cache.insert(3, TileCoord { col: 1, row: 2 }, Arc::clone(&pixels));
        cache.insert(5, TileCoord { col: 3, row: 4 }, Arc::clone(&pixels));

        // Test relocate
        cache.relocate_image(3, 7);
        assert!(cache.get(7, TileCoord { col: 1, row: 2 }).is_some());
        assert!(cache.get(3, TileCoord { col: 1, row: 2 }).is_none());

        // Test remove_except
        cache.remove_images_except(5);
        assert!(cache.get(5, TileCoord { col: 3, row: 4 }).is_some());
        assert!(cache.get(7, TileCoord { col: 1, row: 2 }).is_none());
    }
}

/// The global tile pixel cache instance.
pub static PIXEL_CACHE: LazyLock<RwLock<TilePixelCache>> = LazyLock::new(|| {
    RwLock::new(TilePixelCache::new(512)) // Default 512MB, will be updated by settings
});

/// Manages the tiled rendering state for a single large image.
pub struct TileManager {
    /// Current image index in the folder (used for cache lookups).
    pub image_index: usize,
    /// Full image dimensions (original pixels).
    pub full_width: u32,
    pub full_height: u32,
    /// Decode profile binding for tile / preview stale checks.
    pub decode_profile: crate::loader::DecodeProfile,

    /// The downscaled preview texture (fits on screen).
    pub preview_texture: Option<TextureHandle>,

    /// The source of pixel data (could be CPU RAM or an on-demand Disk source).
    source: Arc<dyn crate::loader::TiledImageSource>,

    /// Cached tile textures already uploaded to GPU.
    tiles: HashMap<TileCoord, TextureHandle>,

    /// Tiles currently being decoded in the background.
    pub pending_tiles: HashSet<PendingTileKey>,

    /// GPU tiles eligible for eviction (oldest first).
    evictable_lru: crate::lru_order::LruOrder<TileCoord>,
    /// GPU tiles in the current visible set; never evicted while pinned.
    visible_pinned: HashSet<TileCoord>,
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
        decode_profile: crate::loader::DecodeProfile,
        source: Arc<dyn crate::loader::TiledImageSource>,
    ) -> Self {
        Self {
            image_index: index,
            full_width: source.width(),
            full_height: source.height(),
            decode_profile,
            preview_texture: None,
            source,
            tiles: HashMap::new(),
            pending_tiles: HashSet::new(),
            evictable_lru: crate::lru_order::LruOrder::default(),
            visible_pinned: HashSet::new(),
            ready_times: HashMap::new(),
        }
    }

    fn touch_gpu_lru(&mut self, coord: TileCoord, visible: bool) {
        if visible {
            self.evictable_lru.remove(coord);
            self.visible_pinned.insert(coord);
        } else {
            self.visible_pinned.remove(&coord);
            if self.tiles.contains_key(&coord) {
                self.evictable_lru.touch(coord);
            }
        }
    }

    fn sync_gpu_visible_protection(&mut self, visible: &HashSet<TileCoord>) {
        for coord in visible {
            if self.tiles.contains_key(coord) {
                self.evictable_lru.remove(*coord);
                self.visible_pinned.insert(*coord);
            }
        }

        let unpinned: Vec<_> = self
            .visible_pinned
            .iter()
            .filter(|coord| !visible.contains(coord))
            .copied()
            .collect();
        for coord in unpinned {
            self.visible_pinned.remove(&coord);
            if self.tiles.contains_key(&coord) {
                self.evictable_lru.push_oldest(coord);
            }
        }
    }

    fn evict_gpu_tiles_over_limit(&mut self, limit: usize) {
        while self.tiles.len() > limit {
            let Some(evicted) = self.evictable_lru.pop_oldest() else {
                break;
            };
            self.tiles.remove(&evicted);
            self.ready_times.remove(&evicted);
            self.visible_pinned.remove(&evicted);
        }
    }

    /// Returns a cheap Arc clone of the raw pixel buffer if available.
    pub fn pixel_buffer_arc(&self) -> Option<Arc<Vec<u8>>> {
        self.source.full_pixels()
    }

    pub fn get_source(&self) -> Arc<dyn crate::loader::TiledImageSource> {
        Arc::clone(&self.source)
    }

    pub fn retain_pending_tiles(&mut self, visible_coords: &HashSet<TileCoord>) {
        self.pending_tiles
            .retain(|key| visible_coords.contains(&key.coord));
    }

    /// Returns counts for the current visible set using a non-blocking try_lock: (gpu, cpu_ready, pending)
    #[cfg(feature = "tile-debug")]
    pub fn stats_for_visible(&self, visible: &HashSet<TileCoord>) -> (usize, usize, usize) {
        let mut gpu = 0;
        let mut cpu = 0;
        let mut pending = 0;

        if let Some(cache) = PIXEL_CACHE.try_read() {
            for coord in visible {
                if self.tiles.contains_key(coord) {
                    gpu += 1;
                } else if cache.contains_tile(self.image_index, *coord) {
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
        let cpu_cached = if let Some(cache) = PIXEL_CACHE.try_read() {
            cache.count_for_image(self.image_index)
        } else {
            0
        };
        (self.tiles.len(), cpu_cached, self.pending_tiles.len())
    }

    /// Returns true if any of the visible tiles are in CPU cache but NOT in GPU.
    pub fn has_ready_to_upload(&self, visible: &HashSet<TileCoord>) -> bool {
        let cache = PIXEL_CACHE.read();

        for coord in visible {
            if !self.tiles.contains_key(coord) && cache.contains_tile(self.image_index, *coord) {
                return true;
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
        self.full_width.div_ceil(ts)
    }

    /// Number of tile rows in the grid.
    pub fn rows(&self) -> u32 {
        let ts = get_tile_size();
        self.full_height.div_ceil(ts)
    }

    /// Get or create a tile texture for the given coordinate.
    /// Returns (TileStatus, newly_uploaded).
    pub fn get_or_create_tile(
        &mut self,
        coord: TileCoord,
        ctx: &egui::Context,
        allow_upload: bool,
        visible_coords: &HashSet<TileCoord>,
    ) -> (TileStatus, bool) {
        let is_visible = visible_coords.contains(&coord);

        // check if exists in GPU
        if self.tiles.contains_key(&coord) {
            self.touch_gpu_lru(coord, is_visible);
            let handle = self.tiles.get(&coord).expect("gpu tile").clone();
            let ready_at = self.ready_times.get(&coord).cloned();
            return (TileStatus::Ready(handle, ready_at), false);
        }

        // 1. Check Global Pixel Cache (CPU)
        let cached_pixels: Option<Arc<Vec<u8>>> = PIXEL_CACHE.read().get(self.image_index, coord);

        if let Some(pixels) = cached_pixels {
            if allow_upload {
                // Strict eviction: Never exceed the base limit determined by HardwareTier.
                // We no longer expand the limit based on visible_count to prevent crashes.
                let current_limit = MAX_TILES_BASE.load(Ordering::Relaxed);

                // Evict if over limit, but NEVER evict tiles currently in the visible set.
                // This prevents the "circular hole" artifact on high-DPI screens.
                self.sync_gpu_visible_protection(visible_coords);
                self.evict_gpu_tiles_over_limit(current_limit);

                let ts = get_tile_size();
                let tw = ts.min(self.full_width - coord.col * ts);
                let th = ts.min(self.full_height - coord.row * ts);

                let color_image =
                    egui::ColorImage::from_rgba_unmultiplied([tw as usize, th as usize], &pixels);
                let name = format!("tile_{}_{}_{}", self.image_index, coord.col, coord.row);
                let handle = ctx.load_texture(name, color_image, egui::TextureOptions::LINEAR);
                self.tiles.insert(coord, handle.clone());

                let now = Instant::now();
                self.ready_times.insert(coord, now);
                self.touch_gpu_lru(coord, is_visible);

                // Remove from pending if it was there
                self.pending_tiles.remove(&PendingTileKey::new(
                    coord,
                    crate::loader::TilePixelKind::Sdr,
                ));

                return (TileStatus::Ready(handle, Some(now)), true);
            }

            // If it's in CPU cache but we didn't upload it (quota reached),
            // tell caller it's pending but DOES NOT need a new background request.
            return (TileStatus::Pending(false), false);
        }

        // If we reach here, it's not in GPU and not in CPU cache.
        let needs_request = !self.pending_tiles.contains(&PendingTileKey::new(
            coord,
            crate::loader::TilePixelKind::Sdr,
        ));
        (TileStatus::Pending(needs_request), false)
    }

    /// Drop uploaded GPU tiles while keeping the bootstrap/HQ preview texture.
    pub fn drop_gpu_tiles(&mut self) {
        self.tiles.clear();
        self.evictable_lru.clear();
        self.visible_pinned.clear();
        self.ready_times.clear();
    }

    /// Clear all cached tiles (e.g. when switching images).
    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.drop_gpu_tiles();
        self.preview_texture = None;
        // The source's lifecycle is managed via Arc.
    }

    /// Compute which tiles are visible given the current viewport mapping.
    /// `viewport` is the screen-space rectangle where the full image would be displayed.
    /// `screen_clip` is the visible screen area (to clip against).
    /// Writes `(TileCoord, screen_rect, uv_rect)` tuples into `out` (reuses capacity).
    pub fn visible_tiles_into(
        &self,
        viewport: egui::Rect,
        screen_clip: egui::Rect,
        padding: f32,
        out: &mut Vec<(TileCoord, egui::Rect, egui::Rect)>,
    ) {
        out.clear();
        let Some((start_col, end_col, start_row, end_row)) = visible_tile_index_bounds(
            self.full_width,
            self.full_height,
            self.cols(),
            self.rows(),
            viewport,
            screen_clip,
            padding,
        ) else {
            return;
        };

        let screen_center = screen_clip.center();
        let ts = get_tile_size();
        let rel_x = (screen_center.x - viewport.min.x) / viewport.width();
        let rel_y = (screen_center.y - viewport.min.y) / viewport.height();
        let center_col = (rel_x * self.full_width as f32 / ts as f32).floor() as i32;
        let center_row = (rel_y * self.full_height as f32 / ts as f32).floor() as i32;

        let max_ring = (start_col..=end_col)
            .flat_map(|c| (start_row..=end_row).map(move |r| (c, r)))
            .map(|(c, r)| {
                let dc = (c as i32 - center_col).unsigned_abs();
                let dr = (r as i32 - center_row).unsigned_abs();
                dc.max(dr)
            })
            .max()
            .unwrap_or(0);

        for ring in 0..=max_ring {
            for r in start_row..=end_row {
                for c in start_col..=end_col {
                    let dc = (c as i32 - center_col).unsigned_abs();
                    let dr = (r as i32 - center_row).unsigned_abs();
                    if dc.max(dr) != ring {
                        continue;
                    }

                    let tile_x0 = c * ts;
                    let tile_y0 = r * ts;
                    let tile_w = ts.min(self.full_width - tile_x0);
                    let tile_h = ts.min(self.full_height - tile_y0);

                    let sx0 = viewport.min.x
                        + (tile_x0 as f32 / self.full_width as f32) * viewport.width();
                    let sy0 = viewport.min.y
                        + (tile_y0 as f32 / self.full_height as f32) * viewport.height();
                    let sx1 = viewport.min.x
                        + ((tile_x0 + tile_w) as f32 / self.full_width as f32) * viewport.width();
                    let sy1 = viewport.min.y
                        + ((tile_y0 + tile_h) as f32 / self.full_height as f32) * viewport.height();

                    let tile_screen_rect = egui::Rect::from_min_max(
                        egui::Pos2::new(sx0, sy0),
                        egui::Pos2::new(sx1, sy1),
                    );

                    let uv = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0));
                    out.push((TileCoord { col: c, row: r }, tile_screen_rect, uv));
                }
            }
        }
    }

    /// Derive primary-visible tiles (no lookahead padding) from a padded visibility scan.
    pub fn primary_visible_from_padded_into(
        &self,
        viewport: egui::Rect,
        screen_clip: egui::Rect,
        padded: &[(TileCoord, egui::Rect, egui::Rect)],
        out: &mut Vec<(TileCoord, egui::Rect, egui::Rect)>,
    ) {
        out.clear();
        let Some((start_col, end_col, start_row, end_row)) = visible_tile_index_bounds(
            self.full_width,
            self.full_height,
            self.cols(),
            self.rows(),
            viewport,
            screen_clip,
            0.0,
        ) else {
            return;
        };
        for &(coord, screen_rect, uv) in padded {
            if coord.col >= start_col
                && coord.col <= end_col
                && coord.row >= start_row
                && coord.row <= end_row
            {
                out.push((coord, screen_rect, uv));
            }
        }
    }
}

/// Tile col/row bounds intersecting `viewport` and `screen_clip` (with optional padding).
fn visible_tile_index_bounds(
    full_width: u32,
    full_height: u32,
    total_cols: u32,
    total_rows: u32,
    viewport: egui::Rect,
    screen_clip: egui::Rect,
    padding: f32,
) -> Option<(u32, u32, u32, u32)> {
    let visible_area = viewport.intersect(screen_clip.expand(padding));

    if visible_area.width() <= 0.0 || visible_area.height() <= 0.0 {
        return None;
    }

    let uv_min_x = ((visible_area.min.x - viewport.min.x) / viewport.width()).clamp(0.0, 1.0);
    let uv_max_x = ((visible_area.max.x - viewport.min.x) / viewport.width()).clamp(0.0, 1.0);
    let uv_min_y = ((visible_area.min.y - viewport.min.y) / viewport.height()).clamp(0.0, 1.0);
    let uv_max_y = ((visible_area.max.y - viewport.min.y) / viewport.height()).clamp(0.0, 1.0);

    let px_min_x = uv_min_x * full_width as f32;
    let px_max_x = uv_max_x * full_width as f32;
    let px_min_y = uv_min_y * full_height as f32;
    let px_max_y = uv_max_y * full_height as f32;

    let ts = get_tile_size() as f32;
    let min_col = (px_min_x.max(0.0) / ts).floor() as u32;
    let max_col = ((px_max_x - 0.01).max(0.0) / ts).floor() as u32;
    let min_row = (px_min_y.max(0.0) / ts).floor() as u32;
    let max_row = ((px_max_y - 0.01).max(0.0) / ts).floor() as u32;

    let start_col = min_col.min(total_cols.saturating_sub(1));
    let end_col = max_col.min(total_cols.saturating_sub(1));
    let start_row = min_row.min(total_rows.saturating_sub(1));
    let end_row = max_row.min(total_rows.saturating_sub(1));

    Some((start_col, end_col, start_row, end_row))
}
