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

use super::header::{build_radiance_scanline_offsets, read_radiance_header};
use super::layout::RadianceRasterLayout;
use super::tile_decode::{
    RadiancePreviewRequest, RadianceTileWindow, decode_radiance_hdr_preview,
    decode_radiance_sdr_preview, decode_radiance_tile_window,
};

use parking_lot::Mutex;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::hdr::tiled::{
    HdrTileBuffer, HdrTileCache, HdrTiledSource, HdrTiledSourceKind,
    configured_hdr_tile_cache_max_bytes, validate_tile_bounds,
};
use crate::hdr::types::{HdrColorSpace, HdrImageBuffer, HdrImageMetadata};

#[derive(Debug)]
pub struct RadianceHdrTiledImageSource {
    #[allow(dead_code)]
    path: PathBuf,
    mmap: Arc<memmap2::Mmap>,
    width: u32,
    height: u32,
    raster: RadianceRasterLayout,
    params: crate::hdr::decode::RadianceHeaderParams,
    #[cfg(test)]
    pub(crate) scanline_offsets: Vec<usize>,
    #[cfg(not(test))]
    scanline_offsets: Vec<usize>,
    tile_cache: Mutex<HdrTileCache>,
}

impl RadianceHdrTiledImageSource {
    pub(crate) fn open(path: &Path) -> Result<Self, String> {
        Self::open_from_mmap(path, Arc::new(crate::mmap_util::map_file(path)?))
    }

    /// Open from caller-provided mmap (avoids a second file map per checklist #29).
    pub(crate) fn open_from_mmap(path: &Path, mmap: Arc<memmap2::Mmap>) -> Result<Self, String> {
        Self::from_mmap(path, mmap)
    }

    fn from_mmap(path: &Path, mmap: Arc<memmap2::Mmap>) -> Result<Self, String> {
        let mut params = crate::hdr::decode::RadianceHeaderParams::default();
        let mut reader = Cursor::new(&mmap[..]);
        let raster = read_radiance_header(&mut reader, &mut params)?;
        let (width, height) = (raster.width, raster.height);
        let data_offset = reader.position() as usize;
        let scanline_offsets = build_radiance_scanline_offsets(&mmap, data_offset, &raster)?;
        log::debug!("[HDR] {}: {}", path.display(), params.diagnostic_label());

        Ok(Self {
            path: path.to_path_buf(),
            mmap,
            width,
            height,
            raster,
            params,
            scanline_offsets,
            tile_cache: Mutex::new(HdrTileCache::new(configured_hdr_tile_cache_max_bytes())),
        })
    }
}

impl HdrTiledSource for RadianceHdrTiledImageSource {
    fn source_kind(&self) -> HdrTiledSourceKind {
        HdrTiledSourceKind::DiskBacked
    }

    fn source_name(&self) -> String {
        self.path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
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
        decode_radiance_hdr_preview(RadiancePreviewRequest {
            mmap: &self.mmap,
            logical_width: self.width,
            logical_height: self.height,
            raster: self.raster,
            params: self.params,
            scanline_offsets: &self.scanline_offsets,
            max_w,
            max_h,
        })
    }

    fn generate_sdr_preview(&self, max_w: u32, max_h: u32) -> Result<(u32, u32, Vec<u8>), String> {
        decode_radiance_sdr_preview(RadiancePreviewRequest {
            mmap: &self.mmap,
            logical_width: self.width,
            logical_height: self.height,
            raster: self.raster,
            params: self.params,
            scanline_offsets: &self.scanline_offsets,
            max_w,
            max_h,
        })
    }

    fn cached_tile_rgba32f_arc(
        &self,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    ) -> Option<Arc<HdrTileBuffer>> {
        self.tile_cache.lock().get((x, y, width, height))
    }

    fn protect_cached_tiles(&self, tiles: &[(u32, u32, u32, u32)]) {
        self.tile_cache
            .lock()
            .set_protected_keys(tiles.iter().copied());
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
        {
            let mut cache = self.tile_cache.lock();
            if let Some(tile) = cache.get(key) {
                return Ok(tile);
            }
        }

        let rgba = decode_radiance_tile_window(
            &self.mmap,
            self.raster,
            self.params,
            &self.scanline_offsets,
            RadianceTileWindow {
                tile_x: x,
                tile_y: y,
                tile_w: width,
                tile_h: height,
            },
        )?;

        let tile = Arc::new(HdrTileBuffer::new_with_metadata(
            width,
            height,
            HdrColorSpace::LinearSrgb,
            HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
            Arc::new(rgba),
        ));

        self.tile_cache.lock().insert(key, Arc::clone(&tile));

        Ok(tile)
    }
}
