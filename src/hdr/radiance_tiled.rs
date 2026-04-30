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

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use image::ImageReader;

use crate::hdr::tiled::{
    HdrTileBuffer, HdrTileCache, HdrTiledSource, HdrTiledSourceKind,
    configured_hdr_tile_cache_max_bytes, validate_tile_bounds,
};
use crate::hdr::types::{HdrColorSpace, HdrImageBuffer, HdrPixelFormat};

#[derive(Debug)]
pub struct RadianceHdrTiledImageSource {
    path: PathBuf,
    width: u32,
    height: u32,
    tile_cache: Mutex<HdrTileCache>,
}

impl RadianceHdrTiledImageSource {
    pub(crate) fn open(path: &Path) -> Result<Self, String> {
        let (width, height) = ImageReader::open(path)
            .map_err(|err| err.to_string())?
            .with_guessed_format()
            .map_err(|err| err.to_string())?
            .into_dimensions()
            .map_err(|err| err.to_string())?;

        Ok(Self {
            path: path.to_path_buf(),
            width,
            height,
            tile_cache: Mutex::new(HdrTileCache::new(configured_hdr_tile_cache_max_bytes())),
        })
    }
}

impl HdrTiledSource for RadianceHdrTiledImageSource {
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

    fn generate_sdr_preview(&self, max_w: u32, max_h: u32) -> Result<(u32, u32, Vec<u8>), String> {
        let tile = self.extract_tile_rgba32f_arc(0, 0, self.width, self.height)?;
        let pixels = crate::hdr::decode::hdr_to_sdr_rgba8(
            &HdrImageBuffer {
                width: tile.width,
                height: tile.height,
                format: HdrPixelFormat::Rgba32Float,
                color_space: tile.color_space,
                rgba_f32: Arc::clone(&tile.rgba_f32),
            },
            0.0,
        )?;
        let image = image::RgbaImage::from_raw(tile.width, tile.height, pixels)
            .ok_or_else(|| "Failed to build Radiance HDR SDR preview image".to_string())?;
        let preview = image::imageops::thumbnail(&image, max_w, max_h);
        Ok((preview.width(), preview.height(), preview.into_raw()))
    }

    fn extract_tile_rgba32f_arc(
        &self,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    ) -> Result<Arc<HdrTileBuffer>, String> {
        validate_tile_bounds(self.width, self.height, x, y, width, height)?;
        let key = (x, y, width, height);
        if let Ok(mut cache) = self.tile_cache.lock() {
            if let Some(tile) = cache.get(key) {
                return Ok(tile);
            }
        }

        let image = crate::hdr::decode::decode_hdr_image(&self.path)?;
        let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
        let source_stride = image.width as usize * 4;
        let row_len = width as usize * 4;
        let start_x = x as usize * 4;

        for row in y..(y + height) {
            let start = row as usize * source_stride + start_x;
            let end = start + row_len;
            rgba.extend_from_slice(&image.rgba_f32[start..end]);
        }

        let tile = Arc::new(HdrTileBuffer {
            width,
            height,
            color_space: HdrColorSpace::LinearSrgb,
            rgba_f32: Arc::new(rgba),
        });

        if let Ok(mut cache) = self.tile_cache.lock() {
            cache.insert(key, Arc::clone(&tile));
        }

        Ok(tile)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_tile_applies_radiance_exposure_and_colorcorr() {
        let path = std::env::temp_dir().join(format!(
            "simple_image_viewer_radiance_tile_params_{}.hdr",
            std::process::id()
        ));
        let bytes = b"#?RADIANCE\nFORMAT=32-bit_rle_rgbe\nEXPOSURE=2\nCOLORCORR=2 4 8\n\n-Y 1 +X 1\n\x80\x80\x80\x81";
        std::fs::write(&path, bytes).expect("write test HDR");

        let source = RadianceHdrTiledImageSource::open(&path).expect("open Radiance HDR source");
        let tile = source
            .extract_tile_rgba32f_arc(0, 0, 1, 1)
            .expect("extract Radiance HDR tile");
        let _ = std::fs::remove_file(&path);

        assert!((tile.rgba_f32[0] - 0.25).abs() < 0.01);
        assert!((tile.rgba_f32[1] - 0.125).abs() < 0.01);
        assert!((tile.rgba_f32[2] - 0.0625).abs() < 0.01);
        assert_eq!(tile.rgba_f32[3], 1.0);
    }
}
