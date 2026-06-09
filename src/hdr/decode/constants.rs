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

use std::path::Path;
use std::sync::Arc;

#[cfg(test)]
use std::io::{BufRead, Cursor};

use image::{ImageReader, Limits};

use crate::hdr::types::{
    HdrColorProfile, HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat, HdrReference,
    HdrToneMapSettings, HdrTransferFunction,
};

pub(crate) const HDR_RGBA32F_BYTES_PER_PIXEL: u64 = 4 * std::mem::size_of::<f32>() as u64;
pub(crate) const SDR_RGBA8_BYTES_PER_PIXEL: u64 = 4;
pub(crate) const HDR_FALLBACK_BYTES_PER_PIXEL_WITH_SDR: u64 =
    HDR_RGBA32F_BYTES_PER_PIXEL + SDR_RGBA8_BYTES_PER_PIXEL;
pub(crate) const MAX_HDR_FALLBACK_PIXELS: u64 = 8192 * 8192;
pub(crate) const MAX_HDR_FALLBACK_DECODE_BYTES: u64 = MAX_HDR_FALLBACK_PIXELS * HDR_RGBA32F_BYTES_PER_PIXEL;
pub(crate) const MAX_HDR_FALLBACK_TOTAL_BYTES: u64 =
    MAX_HDR_FALLBACK_PIXELS * HDR_FALLBACK_BYTES_PER_PIXEL_WITH_SDR;
pub(crate) const MAX_HDR_TONE_MAP_INPUT: f32 = f32::MAX;
pub(crate) const INVERSE_DISPLAY_GAMMA: f32 = 1.0 / 2.2;
