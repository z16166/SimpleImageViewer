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
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

/// Packed tile size (high 32 bits) and GPU tile cache limit (low 32 bits).
/// Updated atomically so readers never observe mismatched pairs.
static TILE_CONFIG: AtomicU64 = AtomicU64::new(pack_tile_config(512, 512));

const fn pack_tile_config(tile_size: u32, max_tiles_base: u32) -> u64 {
    ((tile_size as u64) << 32) | (max_tiles_base as u64)
}

fn unpack_tile_config(packed: u64) -> (u32, u32) {
    ((packed >> 32) as u32, packed as u32)
}

/// Get the current tile size.
pub fn get_tile_size() -> u32 {
    unpack_tile_config(TILE_CONFIG.load(Ordering::Acquire)).0
}

/// Get the current GPU tile cache base limit.
pub fn get_max_tiles_base() -> usize {
    unpack_tile_config(TILE_CONFIG.load(Ordering::Acquire)).1 as usize
}

/// Set tile size based on image dimensions.
/// Also adjusts the GPU tile cache limit to maintain constant VRAM budget.
pub fn set_tile_size_for_image(width: u32, height: u32) {
    let megapixels = (width as u64 * height as u64) / 1_000_000;
    let size = if megapixels > 500 { 1024 } else { 512 };
    // Keep VRAM budget constant: 512 tiles * 512*512*4 = 512MB
    // For 1024 tiles: 128 * 1024*1024*4 = 512MB
    let max_tiles = if size == 1024 { 128 } else { 512 };
    TILE_CONFIG.store(pack_tile_config(size, max_tiles), Ordering::Release);
}

/// Update the GPU tile cache base limit while preserving the current tile size.
pub fn set_max_tiles_base(max_tiles: usize) {
    let max_tiles = u32::try_from(max_tiles).unwrap_or(u32::MAX);
    let mut current = TILE_CONFIG.load(Ordering::Acquire);
    loop {
        let (tile_size, _) = unpack_tile_config(current);
        let next = pack_tile_config(tile_size, max_tiles);
        match TILE_CONFIG.compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => break,
            Err(actual) => current = actual,
        }
    }
}

/// Pixel count threshold above which tiled mode is activated.
/// Updated dynamically based on HardwareTier in app.rs.
pub static TILED_THRESHOLD: AtomicU64 = AtomicU64::new(64_000_000);

/// Get the current tiled-mode pixel threshold.
pub fn get_tiled_threshold() -> u64 {
    TILED_THRESHOLD.load(Ordering::Acquire)
}

/// Maximum texture side length supported by most GPUs (conservative limit).
/// Large images exceeding this will be rendered using tiles.
/// This value is updated dynamically at startup based on GPU hardware limits.
pub static MAX_TEXTURE_SIDE: AtomicU32 =
    AtomicU32::new(crate::constants::ABSOLUTE_MAX_TEXTURE_SIDE);

pub fn get_max_texture_side() -> u32 {
    MAX_TEXTURE_SIDE.load(Ordering::Acquire)
}

/// Coordinate of a tile within the grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileCoord {
    pub col: u32,
    pub row: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TileRect {
    pub(crate) x: u32,
    pub(crate) y: u32,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

const RGBA8_CHANNELS: usize = 4;

pub(crate) fn tile_count_for_extent(extent: u32) -> u32 {
    let tile_size = get_tile_size();
    if tile_size == 0 {
        return 0;
    }
    extent.div_ceil(tile_size)
}

pub(crate) fn tile_rect_for_dimensions(
    full_width: u32,
    full_height: u32,
    coord: TileCoord,
) -> Option<TileRect> {
    let tile_size = get_tile_size();
    if tile_size == 0 {
        return None;
    }

    let x = coord.col.checked_mul(tile_size)?;
    let y = coord.row.checked_mul(tile_size)?;
    let remaining_width = full_width.checked_sub(x)?;
    let remaining_height = full_height.checked_sub(y)?;
    let width = tile_size.min(remaining_width);
    let height = tile_size.min(remaining_height);
    if width == 0 || height == 0 {
        return None;
    }

    Some(TileRect {
        x,
        y,
        width,
        height,
    })
}

pub(crate) fn rgba8_len_for_dimensions(width: u32, height: u32) -> Option<usize> {
    (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(RGBA8_CHANNELS))
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

    /// Remove one tile entry. Returns true if an entry was removed.
    pub fn remove_tile(&mut self, index: usize, coord: TileCoord) -> bool {
        let key = (index, coord.col, coord.row);
        if let Some(pixels) = self.entries.remove(&key) {
            self.current_bytes -= pixels.len();
            self.lru.lock().remove(key);
            true
        } else {
            false
        }
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
        self.entries.retain(|key, pixels| {
            if key.0 == index {
                self.current_bytes -= pixels.len();
                false
            } else {
                true
            }
        });
        self.lru.lock().retain_keys(|key| key.0 != index);
    }

    /// Remove all tiles belonging to any of the provided image indices.
    pub fn remove_images(&mut self, indices: &std::collections::HashSet<usize>) {
        if indices.is_empty() {
            return;
        }
        self.entries.retain(|key, pixels| {
            if indices.contains(&key.0) {
                self.current_bytes -= pixels.len();
                false
            } else {
                true
            }
        });
        self.lru.lock().retain_keys(|key| !indices.contains(&key.0));
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
        self.entries.retain(|key, pixels| {
            if key.0 != except_idx {
                self.current_bytes -= pixels.len();
                false
            } else {
                true
            }
        });
        self.lru.lock().retain_keys(|key| key.0 == except_idx);
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
        let Some(cache) = PIXEL_CACHE.try_read() else {
            return false;
        };

        for coord in visible {
            if !self.tiles.contains_key(coord) && cache.contains_tile(self.image_index, *coord) {
                return true;
            }
        }
        false
    }

    /// Drop CPU pixel buffers for tiles already resident on GPU.
    /// Called after upload bursts so redundant CPU copies can be freed without
    /// affecting on-screen tiles (GPU textures remain authoritative).
    pub fn release_cpu_pixels_for_coords(&self, coords: &[TileCoord]) {
        if coords.is_empty() {
            return;
        }
        let mut cache = PIXEL_CACHE.write();
        for coord in coords {
            cache.remove_tile(self.image_index, *coord);
        }
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

    fn tile_rect(&self, coord: TileCoord) -> Option<TileRect> {
        tile_rect_for_dimensions(self.full_width, self.full_height, coord)
    }

    fn rgba8_upload_dimensions(&self, coord: TileCoord, pixel_len: usize) -> Option<[usize; 2]> {
        let rect = self.tile_rect(coord)?;
        let expected_len = rgba8_len_for_dimensions(rect.width, rect.height)?;
        if pixel_len != expected_len {
            return None;
        }
        Some([rect.width as usize, rect.height as usize])
    }

    /// Number of tile columns in the grid.
    pub fn cols(&self) -> u32 {
        tile_count_for_extent(self.full_width)
    }

    /// Number of tile rows in the grid.
    pub fn rows(&self) -> u32 {
        tile_count_for_extent(self.full_height)
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
        let pending_key = PendingTileKey::new(coord, crate::loader::TilePixelKind::Sdr);
        if self.tile_rect(coord).is_none() {
            self.tiles.remove(&coord);
            self.ready_times.remove(&coord);
            self.evictable_lru.remove(coord);
            self.visible_pinned.remove(&coord);
            self.pending_tiles.remove(&pending_key);
            PIXEL_CACHE.write().remove_tile(self.image_index, coord);
            return (TileStatus::Pending(false), false);
        }

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
            let Some(upload_dimensions) = self.rgba8_upload_dimensions(coord, pixels.len()) else {
                PIXEL_CACHE.write().remove_tile(self.image_index, coord);
                self.pending_tiles.remove(&pending_key);
                return (TileStatus::Pending(true), false);
            };

            if allow_upload {
                // Strict eviction: Never exceed the base limit determined by HardwareTier.
                // We no longer expand the limit based on visible_count to prevent crashes.
                let current_limit = get_max_tiles_base();

                // Evict if over limit, but NEVER evict tiles currently in the visible set.
                // This prevents the "circular hole" artifact on high-DPI screens.
                self.sync_gpu_visible_protection(visible_coords);
                self.evict_gpu_tiles_over_limit(current_limit);

                let color_image =
                    egui::ColorImage::from_rgba_unmultiplied(upload_dimensions, &pixels);
                let name = format!("tile_{}_{}_{}", self.image_index, coord.col, coord.row);
                let handle = ctx.load_texture(name, color_image, egui::TextureOptions::LINEAR);
                self.tiles.insert(coord, handle.clone());

                let now = Instant::now();
                self.ready_times.insert(coord, now);
                self.touch_gpu_lru(coord, is_visible);

                // Remove from pending if it was there
                self.pending_tiles.remove(&pending_key);

                return (TileStatus::Ready(handle, Some(now)), true);
            }

            // If it's in CPU cache but we didn't upload it (quota reached),
            // tell caller it's pending but DOES NOT need a new background request.
            return (TileStatus::Pending(false), false);
        }

        // If we reach here, it's not in GPU and not in CPU cache.
        let needs_request = !self.pending_tiles.contains(&pending_key);
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
    /// When `primary_out` is `Some`, also writes tiles visible without lookahead padding
    /// in the same center-first scan (no second O(cols x rows) pass).
    pub fn visible_tiles_into(
        &self,
        viewport: egui::Rect,
        screen_clip: egui::Rect,
        padding: f32,
        out: &mut Vec<(TileCoord, egui::Rect, egui::Rect)>,
        mut primary_out: Option<&mut Vec<(TileCoord, egui::Rect, egui::Rect)>>,
    ) {
        out.clear();
        if let Some(primary) = primary_out.as_deref_mut() {
            primary.clear();
        }
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

        let primary_bounds = if primary_out.is_some() {
            visible_tile_index_bounds(
                self.full_width,
                self.full_height,
                self.cols(),
                self.rows(),
                viewport,
                screen_clip,
                0.0,
            )
        } else {
            None
        };

        let screen_center = screen_clip.center();
        let ts = get_tile_size();
        let rel_x = (screen_center.x - viewport.min.x) / viewport.width();
        let rel_y = (screen_center.y - viewport.min.y) / viewport.height();
        let center_col = (rel_x * self.full_width as f32 / ts as f32).floor() as i32;
        let center_row = (rel_y * self.full_height as f32 / ts as f32).floor() as i32;

        let start_col_i = i64::from(start_col);
        let end_col_i = i64::from(end_col);
        let start_row_i = i64::from(start_row);
        let end_row_i = i64::from(end_row);
        let center_col_i = i64::from(center_col);
        let center_row_i = i64::from(center_row);
        let distance_to_interval = |value: i64, start: i64, end: i64| {
            if value < start {
                start - value
            } else if value > end {
                value - end
            } else {
                0
            }
        };
        let farthest_distance_to_interval =
            |value: i64, start: i64, end: i64| (start - value).abs().max((end - value).abs());
        let min_ring = distance_to_interval(center_col_i, start_col_i, end_col_i)
            .max(distance_to_interval(center_row_i, start_row_i, end_row_i));
        let max_ring = farthest_distance_to_interval(center_col_i, start_col_i, end_col_i).max(
            farthest_distance_to_interval(center_row_i, start_row_i, end_row_i),
        );

        let mut push_tile = |c: u32, r: u32| {
            let tile_x0 = c.saturating_mul(ts);
            let tile_y0 = r.saturating_mul(ts);
            let Some(remaining_w) = self.full_width.checked_sub(tile_x0) else {
                return;
            };
            let Some(remaining_h) = self.full_height.checked_sub(tile_y0) else {
                return;
            };
            if remaining_w == 0 || remaining_h == 0 {
                return;
            }
            let tile_w = ts.min(remaining_w);
            let tile_h = ts.min(remaining_h);

            let sx0 = viewport.min.x + (tile_x0 as f32 / self.full_width as f32) * viewport.width();
            let sy0 =
                viewport.min.y + (tile_y0 as f32 / self.full_height as f32) * viewport.height();
            let sx1 = viewport.min.x
                + ((tile_x0 + tile_w) as f32 / self.full_width as f32) * viewport.width();
            let sy1 = viewport.min.y
                + ((tile_y0 + tile_h) as f32 / self.full_height as f32) * viewport.height();

            let tile_screen_rect =
                egui::Rect::from_min_max(egui::Pos2::new(sx0, sy0), egui::Pos2::new(sx1, sy1));

            let uv = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0));
            let coord = TileCoord { col: c, row: r };
            out.push((coord, tile_screen_rect, uv));
            if let (Some(primary), Some((p_start_col, p_end_col, p_start_row, p_end_row))) =
                (primary_out.as_deref_mut(), primary_bounds)
                && c >= p_start_col
                && c <= p_end_col
                && r >= p_start_row
                && r <= p_end_row
            {
                primary.push((coord, tile_screen_rect, uv));
            }
        };

        for ring in min_ring..=max_ring {
            let top = center_row_i - ring;
            let bottom = center_row_i + ring;
            let left = center_col_i - ring;
            let right = center_col_i + ring;

            let row_min = top.max(start_row_i);
            let row_max = bottom.min(end_row_i);
            let col_min = left.max(start_col_i);
            let col_max = right.min(end_col_i);
            if row_min > row_max || col_min > col_max {
                continue;
            }

            for r in row_min..=row_max {
                if r == top || r == bottom {
                    for c in col_min..=col_max {
                        push_tile(c as u32, r as u32);
                    }
                } else {
                    if left >= start_col_i && left <= end_col_i {
                        push_tile(left as u32, r as u32);
                    }
                    if right != left && right >= start_col_i && right <= end_col_i {
                        push_tile(right as u32, r as u32);
                    }
                }
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
    fn set_tile_size_for_image_updates_size_and_limit_atomically() {
        set_tile_size_for_image(8000, 6000);
        assert_eq!(get_tile_size(), 512);
        assert_eq!(get_max_tiles_base(), 512);

        set_tile_size_for_image(30_000, 20_000);
        assert_eq!(get_tile_size(), 1024);
        assert_eq!(get_max_tiles_base(), 128);

        set_max_tiles_base(448);
        assert_eq!(get_tile_size(), 1024);
        assert_eq!(get_max_tiles_base(), 448);
    }

    #[test]
    fn visible_tiles_into_emits_primary_subset_in_same_pass() {
        let source = Arc::new(DummyTileSource {
            width: 4096,
            height: 4096,
        });
        let manager = TileManager::with_source(0, crate::loader::decode_profile_stub(), source);
        let viewport = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(800.0, 600.0));
        let screen_clip =
            egui::Rect::from_min_max(egui::pos2(100.0, 50.0), egui::pos2(700.0, 550.0));
        let padding = 64.0;

        let mut padded = Vec::new();
        let mut primary = Vec::new();
        manager.visible_tiles_into(
            viewport,
            screen_clip,
            padding,
            &mut padded,
            Some(&mut primary),
        );

        assert!(!padded.is_empty());
        assert!(!primary.is_empty());
        assert!(primary.len() <= padded.len());

        let mut padded_only = Vec::new();
        let mut primary_only = Vec::new();
        manager.visible_tiles_into(viewport, screen_clip, padding, &mut padded_only, None);
        manager.visible_tiles_into(viewport, screen_clip, 0.0, &mut primary_only, None);
        assert_eq!(padded, padded_only);
        assert_eq!(primary, primary_only);
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
    fn tile_rect_rejects_out_of_bounds_and_overflow_coords() {
        let valid = tile_rect_for_dimensions(1, 1, TileCoord { col: 0, row: 0 })
            .expect("origin tile is valid");
        assert_eq!(valid.width, 1);
        assert_eq!(valid.height, 1);

        assert_eq!(
            tile_rect_for_dimensions(1, 1, TileCoord { col: 1, row: 0 }),
            None
        );
        assert_eq!(
            tile_rect_for_dimensions(1, 1, TileCoord { col: 0, row: 1 }),
            None
        );
        assert_eq!(
            tile_rect_for_dimensions(
                1,
                1,
                TileCoord {
                    col: u32::MAX,
                    row: 0
                }
            ),
            None
        );
    }

    #[test]
    fn stale_invalid_cached_tile_does_not_upload_or_stay_pending() {
        let image_index = 7001;
        let source = Arc::new(DummyTileSource {
            width: 1,
            height: 1,
        });
        let mut manager =
            TileManager::with_source(image_index, crate::loader::decode_profile_stub(), source);
        let coord = TileCoord { col: 1, row: 0 };
        let pending_key = PendingTileKey::new(coord, crate::loader::TilePixelKind::Sdr);

        PIXEL_CACHE
            .write()
            .insert(image_index, coord, Arc::new(vec![0; 4]));
        manager.pending_tiles.insert(pending_key);

        let ctx = egui::Context::default();
        let (status, uploaded) = manager.get_or_create_tile(coord, &ctx, true, &HashSet::new());

        assert!(matches!(status, TileStatus::Pending(false)));
        assert!(!uploaded);
        assert!(!manager.pending_tiles.contains(&pending_key));
        assert!(!PIXEL_CACHE.read().contains_tile(image_index, coord));
    }

    #[test]
    fn malformed_cached_tile_pixels_are_removed_before_upload() {
        let image_index = 7002;
        let source = Arc::new(DummyTileSource {
            width: 1,
            height: 1,
        });
        let mut manager =
            TileManager::with_source(image_index, crate::loader::decode_profile_stub(), source);
        let coord = TileCoord { col: 0, row: 0 };
        let pending_key = PendingTileKey::new(coord, crate::loader::TilePixelKind::Sdr);

        PIXEL_CACHE
            .write()
            .insert(image_index, coord, Arc::new(vec![0; 3]));
        manager.pending_tiles.insert(pending_key);

        let ctx = egui::Context::default();
        let (status, uploaded) = manager.get_or_create_tile(coord, &ctx, true, &HashSet::new());

        assert!(matches!(status, TileStatus::Pending(true)));
        assert!(!uploaded);
        assert!(!manager.pending_tiles.contains(&pending_key));
        assert!(!PIXEL_CACHE.read().contains_tile(image_index, coord));
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

        // Test remove_tile
        assert!(cache.remove_tile(5, TileCoord { col: 3, row: 4 }));
        assert!(!cache.contains_tile(5, TileCoord { col: 3, row: 4 }));
        assert!(!cache.remove_tile(5, TileCoord { col: 3, row: 4 }));
    }
}
