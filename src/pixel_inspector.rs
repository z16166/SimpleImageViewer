// Simple Image Viewer - A high-performance, cross-platform image viewer
// Copyright (C) 2024-2026 Simple Image Viewer Contributors
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//

use eframe::egui::{Pos2, Rect};
use std::sync::Arc;

use crate::loader::TiledImageSource;
use crate::tile_cache::{PIXEL_CACHE, TileCoord, get_tile_size};

pub(crate) enum PixelDataSource {
    Static {
        width: u32,
        height: u32,
        pixels: Arc<Vec<u8>>, // RGBA8
    },
    Tiled(Arc<dyn TiledImageSource>),
}

/// Translate screen coordinates on the canvas display rectangle into original image pixel coordinates (top-left origin).
pub fn screen_to_image_coord(
    screen_pos: Pos2,
    display_rect: Rect, // display rect of the image on canvas (after rotation)
    rotation_steps: i32,
    img_w: u32,
    img_h: u32,
) -> Option<(u32, u32)> {
    if !display_rect.contains(screen_pos) {
        return None;
    }

    // Normalize coordinates [0.0, 1.0] relative to the display rectangle
    let u = ((screen_pos.x - display_rect.min.x) / display_rect.width()).clamp(0.0, 0.99999);
    let v = ((screen_pos.y - display_rect.min.y) / display_rect.height()).clamp(0.0, 0.99999);

    let rot = rotation_steps.rem_euclid(4);

    // Map normalized coordinates back to the original image space based on orientation
    let (px, py) = match rot {
        0 => (u * img_w as f32, v * img_h as f32),
        1 => ((1.0 - v) * img_w as f32, u * img_h as f32),
        2 => ((1.0 - u) * img_w as f32, (1.0 - v) * img_h as f32),
        3 => (v * img_w as f32, (1.0 - u) * img_h as f32),
        _ => unreachable!(),
    };

    Some((px as u32, py as u32))
}

/// Reads a single pixel from the active image (non-blocking, for hover tooltips).
/// For tiled images, queries PIXEL_CACHE using try_lock; if missing, returns None.
pub fn sample_hover_pixel(
    source: &PixelDataSource,
    image_index: usize,
    x: u32,
    y: u32,
) -> Option<[u8; 4]> {
    match source {
        PixelDataSource::Static {
            width,
            height,
            pixels,
        } => {
            if x >= *width || y >= *height {
                return None;
            }
            let idx = (y * width + x) as usize * 4;
            if idx + 3 < pixels.len() {
                Some([
                    pixels[idx],
                    pixels[idx + 1],
                    pixels[idx + 2],
                    pixels[idx + 3],
                ])
            } else {
                None
            }
        }
        PixelDataSource::Tiled(tiled_source) => {
            let width = tiled_source.width();
            let height = tiled_source.height();
            if x >= width || y >= height {
                return None;
            }
            let tile_size = get_tile_size();
            let col = x / tile_size;
            let row = y / tile_size;
            let coord = TileCoord { col, row };

            if let Some(mut cache) = PIXEL_CACHE.try_lock() {
                if let Some(tile_pixels) = cache.get(image_index, coord) {
                    let tile_x = x % tile_size;
                    let tile_y = y % tile_size;

                    let tile_w = tile_size.min(width.saturating_sub(col * tile_size));
                    let tile_h = tile_size.min(height.saturating_sub(row * tile_size));

                    if tile_x < tile_w && tile_y < tile_h {
                        let idx = (tile_y * tile_w + tile_x) as usize * 4;
                        if idx + 3 < tile_pixels.len() {
                            return Some([
                                tile_pixels[idx],
                                tile_pixels[idx + 1],
                                tile_pixels[idx + 2],
                                tile_pixels[idx + 3],
                            ]);
                        }
                    }
                }
            }
            None
        }
    }
}

impl Clone for PixelDataSource {
    fn clone(&self) -> Self {
        match self {
            Self::Static {
                width,
                height,
                pixels,
            } => Self::Static {
                width: *width,
                height: *height,
                pixels: Arc::clone(pixels),
            },
            Self::Tiled(source) => Self::Tiled(Arc::clone(source)),
        }
    }
}

/// Translate original image pixel coordinates (top-left origin) into screen coordinates on the canvas display rectangle.
pub fn image_to_screen_coord(
    img_pos: (u32, u32),
    display_rect: Rect,
    rotation_steps: i32,
    img_w: u32,
    img_h: u32,
) -> Pos2 {
    let px = img_pos.0 as f32;
    let py = img_pos.1 as f32;

    let rot = rotation_steps.rem_euclid(4);

    let (u, v) = match rot {
        0 => (px / img_w as f32, py / img_h as f32),
        1 => (py / img_h as f32, 1.0 - (px / img_w as f32)),
        2 => (1.0 - (px / img_w as f32), 1.0 - (py / img_h as f32)),
        3 => (1.0 - (py / img_h as f32), px / img_w as f32),
        _ => unreachable!(),
    };

    let x = display_rect.min.x + u * display_rect.width();
    let y = display_rect.min.y + v * display_rect.height();

    Pos2::new(x, y)
}

/// Extracts a rectangular region of pixels. Since this can block or decode tiles,
/// it should be executed off the main UI thread.
pub fn extract_region(
    source: &PixelDataSource,
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
) -> Vec<Vec<[u8; 4]>> {
    if x1 <= x0 || y1 <= y0 {
        return Vec::new();
    }
    let w = x1 - x0;
    let h = y1 - y0;
    let mut out = vec![vec![[0, 0, 0, 0]; w as usize]; h as usize];

    match source {
        PixelDataSource::Static {
            width,
            height,
            pixels,
        } => {
            for y in y0..y1 {
                if y >= *height {
                    continue;
                }
                let dest_row = (y - y0) as usize;
                for x in x0..x1 {
                    if x >= *width {
                        continue;
                    }
                    let dest_col = (x - x0) as usize;
                    let idx = (y * width + x) as usize * 4;
                    if idx + 3 < pixels.len() {
                        out[dest_row][dest_col] = [
                            pixels[idx],
                            pixels[idx + 1],
                            pixels[idx + 2],
                            pixels[idx + 3],
                        ];
                    }
                }
            }
        }
        PixelDataSource::Tiled(tiled_source) => {
            let tile_pixels = tiled_source.extract_tile(x0, y0, w, h);
            for r in 0..h {
                let dest_row = r as usize;
                for c in 0..w {
                    let dest_col = c as usize;
                    let idx = (dest_row * w as usize + dest_col) * 4;
                    if idx + 3 < tile_pixels.len() {
                        out[dest_row][dest_col] = [
                            tile_pixels[idx],
                            tile_pixels[idx + 1],
                            tile_pixels[idx + 2],
                            tile_pixels[idx + 3],
                        ];
                    }
                }
            }
        }
    }
    out
}
