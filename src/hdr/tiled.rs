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

use std::sync::Arc;

use super::types::{HdrColorSpace, HdrImageBuffer, HdrPixelFormat};

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct HdrTileBuffer {
    pub width: u32,
    pub height: u32,
    pub color_space: HdrColorSpace,
    pub rgba_f32: Arc<Vec<f32>>,
}

#[derive(Debug, Clone)]
pub struct HdrTiledImageSource {
    image: HdrImageBuffer,
}

impl HdrTiledImageSource {
    pub fn new(image: HdrImageBuffer) -> Result<Self, String> {
        if image.format != HdrPixelFormat::Rgba32Float {
            return Err(format!(
                "HDR tiled source currently supports only Rgba32Float buffers, got {:?}",
                image.format
            ));
        }

        validate_rgba32f_len(image.width, image.height, image.rgba_f32.len())?;
        Ok(Self { image })
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
        validate_tile_bounds(self.image.width, self.image.height, x, y, width, height)?;

        let mut tile = Vec::with_capacity((width as usize) * (height as usize) * 4);
        let source_stride = self.image.width as usize * 4;
        let row_len = width as usize * 4;
        let start_x = x as usize * 4;

        for row in y..(y + height) {
            let start = row as usize * source_stride + start_x;
            let end = start + row_len;
            tile.extend_from_slice(&self.image.rgba_f32[start..end]);
        }

        Ok(HdrTileBuffer {
            width,
            height,
            color_space: self.image.color_space,
            rgba_f32: Arc::new(tile),
        })
    }
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
        return Err(format!("HDR tile requires non-zero dimensions, got {width}x{height}"));
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

    use crate::hdr::tiled::HdrTiledImageSource;
    use crate::hdr::types::{HdrColorSpace, HdrImageBuffer, HdrPixelFormat};

    #[test]
    fn extracts_rgba32f_tile_from_in_memory_hdr_buffer() {
        let image = HdrImageBuffer {
            width: 3,
            height: 2,
            format: HdrPixelFormat::Rgba32Float,
            color_space: HdrColorSpace::LinearSrgb,
            rgba_f32: Arc::new(vec![
                0.0, 0.1, 0.2, 1.0, 1.0, 1.1, 1.2, 1.0, 2.0, 2.1, 2.2, 1.0, 3.0, 3.1, 3.2,
                1.0, 4.0, 4.1, 4.2, 1.0, 5.0, 5.1, 5.2, 1.0,
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
                1.0, 1.1, 1.2, 1.0, 2.0, 2.1, 2.2, 1.0, 4.0, 4.1, 4.2, 1.0, 5.0, 5.1, 5.2,
                1.0,
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
}
