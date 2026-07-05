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

//

use eframe::egui::{Pos2, Rect, Vec2};
use std::sync::Arc;

use crate::loader::TiledImageSource;
use crate::tile_cache::{PIXEL_CACHE, TileCoord, get_tile_size};

pub(crate) struct PixelHoverCache {
    pub last_screen_pos: Pos2,
    pub zoom_factor: f32,
    pub rotation_steps: i32,
    pub current_index: usize,
    pub pan_offset: Vec2,
    pub display_text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PixelRegionRect {
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32, // exclusive
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PixelRegionValidationError {
    Empty,
    TooLarge { w: u32, h: u32 },
}

#[derive(Debug, Clone, Default)]
pub struct PixelRegion {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<[u8; 4]>,
}

pub(crate) fn validate_pixel_region(
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
    max_dim: u32,
) -> Result<PixelRegionRect, PixelRegionValidationError> {
    if x0 == x1 && y0 == y1 {
        return Err(PixelRegionValidationError::Empty);
    }
    let x_min = x0.min(x1);
    let x_max = x0.max(x1);
    let y_min = y0.min(y1);
    let y_max = y0.max(y1);
    let w = x_max - x_min + 1;
    let h = y_max - y_min + 1;
    if w > max_dim || h > max_dim {
        return Err(PixelRegionValidationError::TooLarge { w, h });
    }
    Ok(PixelRegionRect {
        x0: x_min,
        y0: y_min,
        x1: x_max + 1,
        y1: y_max + 1,
    })
}

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
    if display_rect.width() <= 0.0 || display_rect.height() <= 0.0 {
        return None;
    }
    if !display_rect.contains(screen_pos) {
        return None;
    }

    // Normalize coordinates [0.0, 1.0] relative to the display rectangle.
    // Clamp to less than 1.0 using EPSILON to prevent out of bounds when multiplying by image size.
    let u =
        ((screen_pos.x - display_rect.min.x) / display_rect.width()).clamp(0.0, 1.0 - f32::EPSILON);
    let v = ((screen_pos.y - display_rect.min.y) / display_rect.height())
        .clamp(0.0, 1.0 - f32::EPSILON);

    let rot = rotation_steps.rem_euclid(4);

    // Map normalized coordinates back to the original image space based on orientation
    let (px, py) = match rot {
        0 => (u * img_w as f32, v * img_h as f32),
        1 => ((1.0 - v) * img_w as f32, u * img_h as f32),
        2 => ((1.0 - u) * img_w as f32, (1.0 - v) * img_h as f32),
        3 => (v * img_w as f32, (1.0 - u) * img_h as f32),
        _ => unreachable!(),
    };

    // Epsilon offset to compensate for float-truncation: when the normalized coordinate
    // lands exactly on a pixel boundary (e.g. u=0.0 -> px=0.0), truncation can roundtrip
    // to the previous pixel. Adding a small bias pushes it into the correct pixel.
    // 1e-4 is chosen as the smallest value that reliably rounds up across all rotation
    // mappings while never skipping to the next pixel at normal zoom levels.
    let x = ((px + 1e-4) as u32).min(img_w.saturating_sub(1));
    let y = ((py + 1e-4) as u32).min(img_h.saturating_sub(1));
    Some((x, y))
}

/// Reads a single pixel from the active image (non-blocking, for hover tooltips).
/// For tiled images, queries PIXEL_CACHE using try_write; if missing, returns None.
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

            if let Some(cache) = PIXEL_CACHE.try_write()
                && let Some(tile_pixels) = cache.get(image_index, coord)
            {
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

pub fn image_to_screen_coord(
    img_pos: (u32, u32),
    display_rect: Rect,
    rotation_steps: i32,
    img_w: u32,
    img_h: u32,
) -> Pos2 {
    if img_w == 0 || img_h == 0 {
        return Pos2::ZERO;
    }
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
pub fn extract_region(source: &PixelDataSource, x0: u32, y0: u32, x1: u32, y1: u32) -> PixelRegion {
    if x1 <= x0 || y1 <= y0 {
        return PixelRegion::default();
    }
    let w = (x1 - x0) as usize;
    let h = (y1 - y0) as usize;
    let mut out_pixels = vec![[0, 0, 0, 0]; w * h];

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
                        out_pixels[dest_row * w + dest_col] = [
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
            let tile_pixels = tiled_source.extract_tile(x0, y0, w as u32, h as u32);
            for r in 0..h {
                for c in 0..w {
                    let idx = (r * w + c) * 4;
                    if idx + 3 < tile_pixels.len() {
                        out_pixels[r * w + c] = [
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
    PixelRegion {
        width: w,
        height: h,
        pixels: out_pixels,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_coordinate_mapping_rotation_0() {
        let display_rect = Rect::from_min_max(Pos2::new(10.0, 10.0), Pos2::new(110.0, 110.0));
        let img_w = 100;
        let img_h = 100;

        // Top-left
        let p = Pos2::new(10.0, 10.0);
        let img_coord = screen_to_image_coord(p, display_rect, 0, img_w, img_h).unwrap();
        assert_eq!(img_coord, (0, 0));
        assert_eq!(
            image_to_screen_coord(img_coord, display_rect, 0, img_w, img_h),
            p
        );

        // Center
        let p = Pos2::new(60.0, 60.0);
        let img_coord = screen_to_image_coord(p, display_rect, 0, img_w, img_h).unwrap();
        assert_eq!(img_coord, (50, 50));
        assert_eq!(
            image_to_screen_coord(img_coord, display_rect, 0, img_w, img_h),
            p
        );
    }

    #[test]
    fn test_coordinate_mapping_rotations() {
        let display_rect = Rect::from_min_max(Pos2::new(10.0, 10.0), Pos2::new(110.0, 210.0));
        let img_w = 100;
        let img_h = 200;

        let img_pos = (20, 30);
        for rot in 0..4 {
            let screen = image_to_screen_coord(img_pos, display_rect, rot, img_w, img_h);
            let mapped = screen_to_image_coord(screen, display_rect, rot, img_w, img_h).unwrap();
            assert_eq!(mapped, img_pos, "Failed on rotation {}", rot);
        }
    }

    #[test]
    fn test_zero_dimensions_defense() {
        let display_rect = Rect::from_min_max(Pos2::new(10.0, 10.0), Pos2::new(10.0, 110.0));
        let res = screen_to_image_coord(Pos2::new(10.0, 50.0), display_rect, 0, 100, 100);
        assert!(res.is_none());

        let screen = image_to_screen_coord((10, 10), display_rect, 0, 0, 100);
        assert_eq!(screen, Pos2::ZERO);
    }

    #[test]
    fn test_extract_region_static() {
        let pixels = vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
        let source = PixelDataSource::Static {
            width: 2,
            height: 2,
            pixels: Arc::new(pixels),
        };

        let region = extract_region(&source, 0, 0, 2, 2);
        assert_eq!(region.width, 2);
        assert_eq!(region.height, 2);
        assert_eq!(region.pixels.len(), 4);
        assert_eq!(region.pixels[0], [0, 1, 2, 3]);
        assert_eq!(region.pixels[1], [4, 5, 6, 7]);
        assert_eq!(region.pixels[2], [8, 9, 10, 11]);
        assert_eq!(region.pixels[3], [12, 13, 14, 15]);
    }

    #[test]
    fn test_validate_pixel_region_cases() {
        let max_dim = 128;

        // 1. Identical coordinates -> Err(Empty)
        assert_eq!(
            validate_pixel_region(10, 20, 10, 20, max_dim),
            Err(PixelRegionValidationError::Empty)
        );

        // 2. Same row, different columns -> Ok (1xN region)
        assert_eq!(
            validate_pixel_region(10, 20, 50, 20, max_dim),
            Ok(PixelRegionRect {
                x0: 10,
                y0: 20,
                x1: 51,
                y1: 21,
            })
        );

        // 3. Same column, different rows -> Ok (Nx1 region)
        assert_eq!(
            validate_pixel_region(30, 5, 30, 80, max_dim),
            Ok(PixelRegionRect {
                x0: 30,
                y0: 5,
                x1: 31,
                y1: 81,
            })
        );

        // 4. Over limit dimensions -> Err(TooLarge)
        assert_eq!(
            validate_pixel_region(10, 10, 150, 10, max_dim),
            Err(PixelRegionValidationError::TooLarge { w: 141, h: 1 })
        );
    }
}
