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

use std::collections::HashMap;
use std::sync::Arc;
use egui::TextureHandle;
use std::sync::atomic::{AtomicU32, Ordering};

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

/// Maximum number of tile textures kept in GPU memory.
/// 256 tiles * 512x512 * 4 bytes = 256MB VRAM.
const MAX_TILES: usize = 256;

/// Coordinate of a tile within the grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileCoord {
    pub col: u32,
    pub row: u32,
}

/// Manages the tiled rendering state for a single large image.
pub struct TileManager {
    /// Full image dimensions (original pixels).
    pub full_width: u32,
    pub full_height: u32,

    /// The downscaled preview texture (fits on screen).
    pub preview_texture: Option<TextureHandle>,

    /// The source of pixel data (could be CPU RAM or an on-demand Disk source).
    source: Arc<dyn crate::loader::TiledImageSource>,

    /// Cached tile textures already uploaded to GPU.
    tiles: HashMap<TileCoord, TextureHandle>,

    /// LRU ordering: most recently used tiles at the back.
    lru_order: Vec<TileCoord>,
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
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Self {
        let source = Arc::new(MemorySource {
            width,
            height,
            pixels: Arc::new(pixels),
        });
        Self::with_source(source)
    }

    /// Create a new TileManager from an arbitrary tiled image source.
    pub fn with_source(source: Arc<dyn crate::loader::TiledImageSource>) -> Self {
        Self {
            full_width: source.width(),
            full_height: source.height(),
            preview_texture: None,
            source,
            tiles: HashMap::new(),
            lru_order: Vec::new(),
        }
    }

    /// Returns a cheap Arc clone of the raw pixel buffer if available.
    pub fn pixel_buffer_arc(&self) -> Option<Arc<Vec<u8>>> {
        self.source.full_pixels()
    }

    /// Number of tile columns in the grid.
    pub fn cols(&self) -> u32 {
        (self.full_width + TILE_SIZE - 1) / TILE_SIZE
    }

    /// Number of tile rows in the grid.
    pub fn rows(&self) -> u32 {
        (self.full_height + TILE_SIZE - 1) / TILE_SIZE
    }


    /// Extract a single tile's pixel data.
    fn extract_tile(&self, coord: TileCoord) -> (u32, u32, Vec<u8>) {
        let x0 = coord.col * TILE_SIZE;
        let y0 = coord.row * TILE_SIZE;
        let tw = TILE_SIZE.min(self.full_width - x0);
        let th = TILE_SIZE.min(self.full_height - y0);

        let pixels = self.source.extract_tile(x0, y0, tw, th);
        (tw, th, pixels)
    }

    /// Get or create a tile texture for the given coordinate.
    /// Returns (handle, newly_created).
    /// If allow_create is false and the tile is missing, returns (None, false).
    pub fn get_or_create_tile(
        &mut self,
        coord: TileCoord,
        ctx: &egui::Context,
        allow_create: bool,
    ) -> (Option<&TextureHandle>, bool) {
        // Touch LRU
        if let Some(pos) = self.lru_order.iter().position(|c| *c == coord) {
            self.lru_order.remove(pos);
        }
        self.lru_order.push(coord);

        // check if exists
        if self.tiles.contains_key(&coord) {
            return (self.tiles.get(&coord), false);
        }

        // Create if missing and allowed
        if allow_create {
            // Evict if over limit
            while self.lru_order.len() > MAX_TILES {
                let evicted = self.lru_order.remove(0);
                self.tiles.remove(&evicted);
            }

            let (tw, th, pixels) = self.extract_tile(coord);
            let color_image = egui::ColorImage::from_rgba_unmultiplied(
                [tw as usize, th as usize],
                &pixels,
            );
            let name = format!("tile_{}_{}", coord.col, coord.row);
            let handle = ctx.load_texture(name, color_image, egui::TextureOptions::LINEAR);
            self.tiles.insert(coord, handle);
            (self.tiles.get(&coord), true)
        } else {
            (None, false)
        }
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
        let visible = viewport.intersect(screen_clip);
        if visible.width() <= 0.0 || visible.height() <= 0.0 {
            return Vec::new();
        }

        let img_w = self.full_width as f32;
        let img_h = self.full_height as f32;
        let vp_w = viewport.width();
        let vp_h = viewport.height();

        // Map visible screen rect back to image pixel coordinates
        let img_x0 = ((visible.min.x - viewport.min.x) / vp_w * img_w).max(0.0);
        let img_y0 = ((visible.min.y - viewport.min.y) / vp_h * img_h).max(0.0);
        let img_x1 = ((visible.max.x - viewport.min.x) / vp_w * img_w).min(img_w);
        let img_y1 = ((visible.max.y - viewport.min.y) / vp_h * img_h).min(img_h);

        let col_start = (img_x0 / TILE_SIZE as f32).floor() as u32;
        let col_end = ((img_x1 / TILE_SIZE as f32).ceil() as u32).min(self.cols());
        let row_start = (img_y0 / TILE_SIZE as f32).floor() as u32;
        let row_end = ((img_y1 / TILE_SIZE as f32).ceil() as u32).min(self.rows());

        let mut result = Vec::new();
        for row in row_start..row_end {
            for col in col_start..col_end {
                let tile_x0 = col * TILE_SIZE;
                let tile_y0 = row * TILE_SIZE;
                let tile_w = TILE_SIZE.min(self.full_width - tile_x0);
                let tile_h = TILE_SIZE.min(self.full_height - tile_y0);

                // Map tile pixel bounds back to screen coordinates
                let sx0 = viewport.min.x + (tile_x0 as f32 / img_w) * vp_w;
                let sy0 = viewport.min.y + (tile_y0 as f32 / img_h) * vp_h;
                let sx1 = viewport.min.x + ((tile_x0 + tile_w) as f32 / img_w) * vp_w;
                let sy1 = viewport.min.y + ((tile_y0 + tile_h) as f32 / img_h) * vp_h;

                let screen_rect = egui::Rect::from_min_max(
                    egui::Pos2::new(sx0, sy0),
                    egui::Pos2::new(sx1, sy1),
                );

                // UV is always full tile (0,0)-(1,1)
                let uv = egui::Rect::from_min_max(
                    egui::Pos2::ZERO,
                    egui::Pos2::new(1.0, 1.0),
                );

                result.push((TileCoord { col, row }, screen_rect, uv));
            }
        }
        result
    }
}
