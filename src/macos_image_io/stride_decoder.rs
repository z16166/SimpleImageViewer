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

use crate::loader::{DecodedImage, ImageData, TiledImageSource};
use memmap2::Mmap;
use parking_lot::Mutex;
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tiff::decoder::{Decoder, DecodingResult};
use tiff::tags::Tag;

use core_foundation::array::CFArray;
use core_foundation::base::{CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};
use core_graphics::color_space::CGColorSpace;
use core_graphics::context::CGContext;
use core_graphics::image::CGImage;
use foreign_types::ForeignType;

// External link to ImageIO and CoreServices
#[link(name = "ImageIO", kind = "framework")]
#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
struct HugeTiffStrideDecoder;

impl HugeTiffStrideDecoder {
    fn write_stride_preview_pixel_u8(
        dst: &mut [u8],
        dst_offset: usize,
        buf: &[u8],
        src_offset: usize,
        samples: u32,
        is_cmyk: bool,
    ) {
        let end = src_offset.saturating_add(samples as usize);
        if end > buf.len() {
            return;
        }
        match samples {
            3 => {
                dst[dst_offset] = buf[src_offset];
                dst[dst_offset + 1] = buf[src_offset + 1];
                dst[dst_offset + 2] = buf[src_offset + 2];
                dst[dst_offset + 3] = 255;
            }
            4 if is_cmyk => {
                let c = buf[src_offset] as f32 / 255.0;
                let m = buf[src_offset + 1] as f32 / 255.0;
                let y = buf[src_offset + 2] as f32 / 255.0;
                let k = buf[src_offset + 3] as f32 / 255.0;
                dst[dst_offset] = (255.0 * (1.0 - c) * (1.0 - k)) as u8;
                dst[dst_offset + 1] = (255.0 * (1.0 - m) * (1.0 - k)) as u8;
                dst[dst_offset + 2] = (255.0 * (1.0 - y) * (1.0 - k)) as u8;
                dst[dst_offset + 3] = 255;
            }
            4 => {
                dst[dst_offset..dst_offset + 4].copy_from_slice(&buf[src_offset..src_offset + 4]);
            }
            1 => {
                let g = buf[src_offset];
                dst[dst_offset] = g;
                dst[dst_offset + 1] = g;
                dst[dst_offset + 2] = g;
                dst[dst_offset + 3] = 255;
            }
            _ => {}
        }
    }

    fn write_stride_preview_pixel_u16(
        dst: &mut [u8],
        dst_offset: usize,
        buf: &[u16],
        src_offset: usize,
        samples: u32,
        is_cmyk: bool,
    ) {
        let end = src_offset.saturating_add(samples as usize);
        if end > buf.len() {
            return;
        }
        match samples {
            3 => {
                dst[dst_offset] = (buf[src_offset] >> 8) as u8;
                dst[dst_offset + 1] = (buf[src_offset + 1] >> 8) as u8;
                dst[dst_offset + 2] = (buf[src_offset + 2] >> 8) as u8;
                dst[dst_offset + 3] = 255;
            }
            4 if is_cmyk => {
                let c = (buf[src_offset] >> 8) as f32 / 255.0;
                let m = (buf[src_offset + 1] >> 8) as f32 / 255.0;
                let y = (buf[src_offset + 2] >> 8) as f32 / 255.0;
                let k = (buf[src_offset + 3] >> 8) as f32 / 255.0;
                dst[dst_offset] = (255.0 * (1.0 - c) * (1.0 - k)) as u8;
                dst[dst_offset + 1] = (255.0 * (1.0 - m) * (1.0 - k)) as u8;
                dst[dst_offset + 2] = (255.0 * (1.0 - y) * (1.0 - k)) as u8;
                dst[dst_offset + 3] = 255;
            }
            4 => {
                dst[dst_offset] = (buf[src_offset] >> 8) as u8;
                dst[dst_offset + 1] = (buf[src_offset + 1] >> 8) as u8;
                dst[dst_offset + 2] = (buf[src_offset + 2] >> 8) as u8;
                dst[dst_offset + 3] = (buf[src_offset + 3] >> 8) as u8;
            }
            1 => {
                let g = (buf[src_offset] >> 8) as u8;
                dst[dst_offset] = g;
                dst[dst_offset + 1] = g;
                dst[dst_offset + 2] = g;
                dst[dst_offset + 3] = 255;
            }
            _ => {}
        }
    }

    /// Multi-threaded, Zero-Allocation Stride-Reader using Rayon + Mmap.
    fn decode_preview(
        path: &Path,
        max_size: u32,
        orientation: u32,
    ) -> Result<(u32, u32, Vec<u8>), String> {
        let mmap = crate::mmap_util::map_file(path)?;

        let cursor = Cursor::new(&mmap[..]);
        let mut decoder = Decoder::new(cursor).map_err(|e| e.to_string())?;

        let (width, height) = decoder.dimensions().map_err(|e| e.to_string())?;
        let max_dim = width.max(height);
        let stride = (max_dim / max_size).max(1);

        let target_w = width / stride;
        let target_h = height / stride;

        let chunk_w = decoder.get_tag_u32(Tag::TileWidth).unwrap_or(width);
        let chunk_h = decoder
            .get_tag_u32(Tag::TileLength)
            .unwrap_or_else(|_| decoder.get_tag_u32(Tag::RowsPerStrip).unwrap_or(height));
        let is_tiled = decoder.get_tag_u32(Tag::TileWidth).is_ok();

        let color_type = decoder.colortype().map_err(|e| e.to_string())?;
        let samples = match color_type {
            tiff::ColorType::RGB(_) => 3,
            tiff::ColorType::RGBA(_) | tiff::ColorType::CMYK(_) => 4,
            tiff::ColorType::Gray(_) => 1,
            _ => return Err("Unsupported color type".into()),
        };

        let mut preview_pixels = vec![0u8; (target_w * target_h * 4) as usize];

        let tiles_across = (width + chunk_w - 1) / chunk_w;
        let tiles_down = (height + chunk_h - 1) / chunk_h;
        let total_chunks = tiles_across * tiles_down;

        let comp = decoder.get_tag_u32(Tag::Compression).unwrap_or(1);
        let planar = decoder.get_tag_u32(Tag::PlanarConfiguration).unwrap_or(1);
        let is_8bit = matches!(
            color_type,
            tiff::ColorType::RGB(8)
                | tiff::ColorType::RGBA(8)
                | tiff::ColorType::CMYK(8)
                | tiff::ColorType::Gray(8)
        );
        let is_cmyk = matches!(color_type, tiff::ColorType::CMYK(_));

        let offsets_tag = if is_tiled {
            Tag::TileOffsets
        } else {
            Tag::StripOffsets
        };
        let offsets = if comp == 1 && planar == 1 && is_8bit {
            decoder.get_tag_u64_vec(offsets_tag).ok().or_else(|| {
                decoder
                    .get_tag_u32_vec(offsets_tag)
                    .ok()
                    .map(|v| v.into_iter().map(|x| x as u64).collect())
            })
        } else {
            None
        };

        if let Some(offsets) = offsets {
            // Engine A: Fast Zero-Copy Mmap
            log::info!("MacOS ImageIO: Engine A - Unified Zero-Copy Mmap Stride-Reader");
            for ty in 0..target_h {
                let y = ty * stride;
                let chunk_row = y / chunk_h;
                let y_in_chunk = y % chunk_h;
                let dst_y_offset = (ty * target_w) as usize * 4;

                for tx in 0..target_w {
                    let x = tx * stride;
                    let chunk_col = x / chunk_w;
                    let chunk_idx = (chunk_row * tiles_across + chunk_col) as usize;

                    if chunk_idx >= offsets.len() {
                        continue;
                    }

                    let offset_in_chunk = (y_in_chunk * chunk_w + (x % chunk_w)) * samples;
                    let src_offset = (offsets[chunk_idx] + offset_in_chunk as u64) as usize;
                    let dst_offset = dst_y_offset + (tx as usize) * 4;

                    if src_offset + (samples as usize) > mmap.len() {
                        continue;
                    }

                    Self::write_stride_preview_pixel_u8(
                        &mut preview_pixels,
                        dst_offset,
                        &mmap[..],
                        src_offset,
                        samples,
                        is_cmyk,
                    );
                }
            }
        } else {
            // ========================================================
            // Engine B: Rayon Multi-threaded Decompression + Zero Allocation
            // ========================================================
            log::info!("MacOS ImageIO: Engine B - Rayon Parallel Decompression (Zero Alloc)");

            // 1. Identify which chunks are ACTUALLY required
            let mut required_chunks = HashSet::new();
            for ty in 0..target_h {
                let y = ty * stride;
                for tx in 0..target_w {
                    let x = tx * stride;
                    let chunk_row = y / chunk_h;
                    let chunk_col = x / chunk_w;
                    let chunk_idx = chunk_row * tiles_across + chunk_col;
                    if chunk_idx < total_chunks {
                        required_chunks.insert(chunk_idx);
                    }
                }
            }

            let required_chunks: Vec<u32> = required_chunks.into_iter().collect();
            let mmap_slice = &mmap[..];

            // 2. Parallel decode across all cores!
            // Each thread builds a lightweight Decoder pointing to the same mmap memory.
            let chunk_cache: std::collections::HashMap<u32, DecodingResult> = required_chunks
                .into_par_iter()
                .filter_map(|chunk_idx| {
                    let local_cursor = Cursor::new(mmap_slice);
                    if let Ok(mut local_decoder) = Decoder::new(local_cursor) {
                        if let Ok(res) = local_decoder.read_chunk(chunk_idx) {
                            return Some((chunk_idx, res));
                        }
                    }
                    None
                })
                .collect();

            // 3. Extract pixels with On-The-Fly 16-bit shift (NO intermediate memory allocations)
            for ty in 0..target_h {
                let y = ty * stride;
                let dst_y_offset = (ty * target_w) as usize * 4;

                for tx in 0..target_w {
                    let x = tx * stride;
                    let chunk_row = y / chunk_h;
                    let chunk_col = x / chunk_w;
                    let chunk_idx = chunk_row * tiles_across + chunk_col;

                    let y_in_chunk = y % chunk_h;
                    let x_in_chunk = x % chunk_w;
                    let src_offset = ((y_in_chunk * chunk_w + x_in_chunk) * samples) as usize;
                    let dst_offset = dst_y_offset + (tx as usize) * 4;

                    let Some(chunk_data) = chunk_cache.get(&chunk_idx) else {
                        continue;
                    };
                    match chunk_data {
                        DecodingResult::U8(v) => Self::write_stride_preview_pixel_u8(
                            &mut preview_pixels,
                            dst_offset,
                            v,
                            src_offset,
                            samples,
                            is_cmyk,
                        ),
                        DecodingResult::U16(v) => Self::write_stride_preview_pixel_u16(
                            &mut preview_pixels,
                            dst_offset,
                            v,
                            src_offset,
                            samples,
                            is_cmyk,
                        ),
                        _ => {}
                    }
                }
            }
        }

        Ok(apply_orientation_buffer(
            preview_pixels,
            target_w,
            target_h,
            orientation,
        ))
    }
}

