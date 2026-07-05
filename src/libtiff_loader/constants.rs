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

// TIFF Photometric Interpretations
pub(crate) const PHOTO_MINISWHITE: u16 = 0;
pub(crate) const PHOTO_MINISBLACK: u16 = 1;
pub(crate) const PHOTO_RGB: u16 = 2;
pub(crate) const PHOTO_PALETTE: u16 = 3;
pub(crate) const PHOTO_SEPARATED: u16 = 5;
pub(crate) const PHOTO_LOGL: u16 = 32844;
pub(crate) const PHOTO_LOGLUV: u16 = 32845;
pub(crate) const FORMAT_UINT: u16 = 1;
pub(crate) const FORMAT_INT: u16 = 2;
pub(crate) const FORMAT_IEEEFP: u16 = 3;
pub(crate) const CONFIG_CONTIG: u16 = 1;
pub(crate) const CONFIG_SEPARATE: u16 = 2;
#[allow(dead_code)]
pub(crate) const COMPRESSION_THUNDERSCAN: u16 = 32809;

/// Upper bound on TIFF tile width/height from tags; rejects absurd values before allocation.
pub(crate) const MAX_TIFF_TILE_DIMENSION: u32 = crate::constants::ABSOLUTE_MAX_TEXTURE_SIDE;

/// Budget (bytes) for strip/tile cache sizing in large TIFFs.
pub(crate) const STRIP_CACHE_BUDGET_BYTES: usize = 256 * 1024 * 1024;
pub(crate) const TILE_CACHE_BUDGET_BYTES: usize = STRIP_CACHE_BUDGET_BYTES;

/// Maximum pixel count for static full-image HDR decode paths (256 megapixels).
pub(crate) const MAX_STATIC_HDR_DECODE_PIXELS: u64 = 256 * 1024 * 1024;

/// Upper bound on concurrent libtiff handles per tiled/scanline source (2x img-loader threads).
pub(crate) const MAX_TIFF_HANDLE_POOL_SIZE: usize = crate::loader::MAX_IMG_LOADER_THREADS * 2;
