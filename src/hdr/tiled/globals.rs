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

use parking_lot::Mutex;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use rayon::prelude::*;

use crate::hdr::types::{
    HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat, IsoDeferredTileContext,
};

pub(crate) const DEFAULT_HDR_TILE_CACHE_MAX_BYTES: usize = 256 * 1024 * 1024;
pub(crate) const MAX_HDR_TILE_CACHE_MAX_BYTES: usize = 4 * 1024 * 1024 * 1024;
pub(crate) static HDR_TILE_CACHE_MAX_BYTES: AtomicUsize =
    AtomicUsize::new(DEFAULT_HDR_TILE_CACHE_MAX_BYTES);
pub(crate) static NEXT_HDR_TILE_CACHE_ID: AtomicU64 = AtomicU64::new(1);

pub(crate) type HdrTileCacheKey = (u32, u32, u32, u32);
