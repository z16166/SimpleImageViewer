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

use super::buffer::HdrTileBuffer;
use std::sync::Arc;

use crate::hdr::types::{HdrColorSpace, HdrImageBuffer, HdrImageMetadata};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HdrTiledSourceKind {
    InMemory,
    DiskBacked,
}

impl HdrTiledSourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InMemory => "in-memory",
            Self::DiskBacked => "disk-backed",
        }
    }
}

#[allow(dead_code)]
pub trait HdrTiledSource: Send + Sync {
    fn source_kind(&self) -> HdrTiledSourceKind;
    fn source_name(&self) -> String {
        "<memory>".to_string()
    }
    fn width(&self) -> u32;
    fn height(&self) -> u32;
    fn color_space(&self) -> HdrColorSpace;
    fn metadata(&self) -> HdrImageMetadata {
        HdrImageMetadata::from_color_space(self.color_space())
    }
    /// True when this tiled HDR source has an SDR base image that can be shown directly for
    /// embedded-SDR-master mode on SDR outputs.
    fn embedded_sdr_master_available(&self) -> bool {
        false
    }
    fn generate_hdr_preview(&self, max_w: u32, max_h: u32) -> Result<HdrImageBuffer, String>;
    fn generate_sdr_preview(&self, max_w: u32, max_h: u32) -> Result<(u32, u32, Vec<u8>), String>;
    fn cached_tile_rgba32f_arc(
        &self,
        _x: u32,
        _y: u32,
        _width: u32,
        _height: u32,
    ) -> Option<Arc<HdrTileBuffer>> {
        None
    }
    fn protect_cached_tiles(&self, _tiles: &[(u32, u32, u32, u32)]) {}
    fn extract_tile_rgba32f_arc(
        &self,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    ) -> Result<Arc<HdrTileBuffer>, String>;
    /// When true, async LibRaw HQ refine owns preview generation; skip capped loader HQ preview.
    fn defers_loader_hq_preview(&self) -> bool {
        false
    }
}
