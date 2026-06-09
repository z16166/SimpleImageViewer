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
pub struct TiffStripCachingSource {
    path: PathBuf,
    _mmap: Arc<Mmap>,
    cached_image: CGImage,
    color_space: CGColorSpace,
    physical_width: u32,
    physical_height: u32,
    logical_width: u32,
    logical_height: u32,
    chunk_w: u32,
    chunk_h: u32,
    orientation: u32,
    // Key: chunk_idx, Value: Normalized RGBA8 buffer for that chunk
    strip_cache: Mutex<HashMap<u32, Arc<Vec<u8>>>>,
    cache_order: Mutex<Vec<u32>>,
}

unsafe impl Send for TiffStripCachingSource {}
unsafe impl Sync for TiffStripCachingSource {}

impl crate::loader::TiledImageSource for TiffStripCachingSource {
    fn width(&self) -> u32 {
        self.logical_width
    }
    fn height(&self) -> u32 {
        self.logical_height
    }

    fn extract_tile(&self, x: u32, y: u32, w: u32, h: u32) -> Arc<Vec<u8>> {
        let mut rgba = vec![255u8; (w * h * 4) as usize];

        if self.orientation <= 1 {
            self.copy_physical_rect_from_strips(x, y, w, h, &mut rgba);
            return Arc::new(rgba);
        }

        // Logical (display) tile: map each pixel to physical storage and sample strips.
        // Cache the current horizontal strip's decoded buffer; only touch strip_cache when py
        // crosses a strip boundary (avoids ~w*h mutex acquisitions per tile).
        let pw = self.physical_width;
        let ph = self.physical_height;
        let mut cached_strip_idx: Option<u32> = None;
        let mut cached_strip_data: Option<Arc<Vec<u8>>> = None;

        for ty in 0..h {
            for tx in 0..w {
                let lx = x.saturating_add(tx);
                let ly = y.saturating_add(ty);
                let Some((px, py)) =
                    exif_display_to_physical_pixel(lx, ly, self.orientation, pw, ph)
                else {
                    continue;
                };
                let dst_off = ((ty * w + tx) as usize) * 4;
                if px >= pw || py >= ph {
                    continue;
                }
                let strip_idx = py / self.chunk_h;
                if cached_strip_idx != Some(strip_idx) {
                    cached_strip_idx = Some(strip_idx);
                    cached_strip_data = self.get_or_decode_chunk(strip_idx);
                }
                let Some(data) = cached_strip_data.as_deref() else {
                    continue;
                };
                self.copy_physical_rgba_from_strip_buffer(
                    data,
                    strip_idx,
                    px,
                    py,
                    &mut rgba[dst_off..dst_off + 4],
                );
            }
        }

        Arc::new(rgba)
    }

    fn generate_preview(&self, max_w: u32, max_h: u32) -> (u32, u32, Vec<u8>) {
        HugeTiffStrideDecoder::decode_preview(&self.path, max_w.max(max_h), self.orientation)
            .unwrap_or((0, 0, vec![]))
    }

    fn full_pixels(&self) -> Option<Arc<Vec<u8>>> {
        None
    }
}

impl TiffStripCachingSource {
    /// Copy a rectangle from horizontal strips when logical coordinates match physical
    /// (EXIF orientation 1). `x,y,w,h` are in **physical** space (= logical for orientation 1).
    fn copy_physical_rect_from_strips(&self, x: u32, y: u32, w: u32, h: u32, rgba: &mut [u8]) {
        let tiles_across = (self.physical_width + self.chunk_w - 1) / self.chunk_w;

        let start_chunk_row = y / self.chunk_h;
        let end_chunk_row = (y + h - 1) / self.chunk_h;
        let start_chunk_col = x / self.chunk_w;
        let end_chunk_col = (x + w - 1) / self.chunk_w;

        for crow in start_chunk_row..=end_chunk_row {
            for ccol in start_chunk_col..=end_chunk_col {
                let chunk_idx = crow * tiles_across + ccol;

                if let Some(data) = self.get_or_decode_chunk(chunk_idx) {
                    let chunk_y_start = crow * self.chunk_h;
                    let chunk_x_start = ccol * self.chunk_w;

                    let intersect_y_start = y.max(chunk_y_start);
                    let intersect_y_end = (y + h)
                        .min(chunk_y_start + self.chunk_h)
                        .min(self.logical_height);
                    let intersect_x_start = x.max(chunk_x_start);
                    let intersect_x_end = (x + w)
                        .min(chunk_x_start + self.chunk_w)
                        .min(self.logical_width);

                    if intersect_y_start >= intersect_y_end || intersect_x_start >= intersect_x_end
                    {
                        continue;
                    }

                    for py in intersect_y_start..intersect_y_end {
                        let y_in_chunk = py - chunk_y_start;
                        let ty = py - y;

                        let src_row_start = (y_in_chunk * self.chunk_w
                            + (intersect_x_start - chunk_x_start))
                            as usize
                            * 4;
                        let dst_row_start = (ty * w + (intersect_x_start - x)) as usize * 4;
                        let copy_px_count = intersect_x_end - intersect_x_start;
                        let copy_bytes = copy_px_count as usize * 4;

                        if src_row_start + copy_bytes <= data.len()
                            && dst_row_start + copy_bytes <= rgba.len()
                        {
                            rgba[dst_row_start..dst_row_start + copy_bytes]
                                .copy_from_slice(&data[src_row_start..src_row_start + copy_bytes]);
                        }
                    }
                } else {
                    log::warn!(
                        "TiffStripCachingSource: Failed to decode chunk {}. Returning partial white tile.",
                        chunk_idx
                    );
                }
            }
        }
    }

    /// Copy one RGBA pixel from a decoded strip buffer (`strip_idx` must be `py / chunk_h`).
    /// No-op if out of bounds (tile pixel stays at its initialized fill).
    fn copy_physical_rgba_from_strip_buffer(
        &self,
        data: &[u8],
        strip_idx: u32,
        px: u32,
        py: u32,
        dst: &mut [u8],
    ) {
        if dst.len() < 4 {
            return;
        }
        let pw = self.physical_width;
        let ph = self.physical_height;
        if px >= pw || py >= ph {
            return;
        }
        let start_y = strip_idx * self.chunk_h;
        let strip_h = self.chunk_h.min(ph.saturating_sub(start_y));
        let y_in_chunk = py - start_y;
        if y_in_chunk >= strip_h {
            return;
        }
        let row_stride = pw as usize * 4;
        let idx = y_in_chunk as usize * row_stride + px as usize * 4;
        if idx + 4 > data.len() {
            return;
        }
        dst[..4].copy_from_slice(&data[idx..idx + 4]);
    }

    fn get_or_decode_chunk(&self, chunk_idx: u32) -> Option<Arc<Vec<u8>>> {
        {
            let cache = self.strip_cache.lock();
            if let Some(chunk) = cache.get(&chunk_idx) {
                return Some(Arc::clone(chunk));
            }
        }

        // Decode
        let data = self.decode_chunk_to_rgba8(chunk_idx)?;
        let data_arc = Arc::new(data);

        {
            let mut cache = self.strip_cache.lock();
            let mut order = self.cache_order.lock();

            cache.insert(chunk_idx, Arc::clone(&data_arc));
            order.push(chunk_idx);

            // Evict if too many strips (Max 32 strips ~ 1.5GB for very giant images)
            if order.len() > 32 {
                let to_remove = order.remove(0);
                cache.remove(&to_remove);
            }
        }

        Some(data_arc)
    }

    fn decode_chunk_to_rgba8(&self, chunk_idx: u32) -> Option<Vec<u8>> {
        let pw = self.physical_width;
        let ph = self.physical_height;
        let start_y = chunk_idx * self.chunk_h;
        if start_y >= ph {
            return None;
        }

        let h = self.chunk_h.min(ph - start_y);

        // Render the strip by drawing the full CGImage into a small bitmap context
        // with a translation offset. This is the same proven approach used by
        // ImageIoTiledSource::extract_tile, which correctly handles CoreGraphics'
        // bottom-up coordinate system.
        let mut context = CGContext::create_bitmap_context(
            None,
            pw as usize,
            h as usize,
            8,
            pw as usize * 4,
            &self.color_space,
            core_graphics::base::kCGImageAlphaPremultipliedLast,
        );

        // CoreGraphics has Y=0 at bottom. To extract strip at top-down row `start_y`,
        // we need to translate so that the correct portion of the image lands in
        // our h-pixel-tall bitmap context.
        //
        // The full image drawn at origin would place its bottom-left at (0,0).
        // We want the rows [start_y .. start_y+h] (top-down) to appear in our context.
        // In CG coords, those rows are at y = (ph - start_y - h) .. (ph - start_y).
        // We translate by -(ph - start_y - h) to shift them into view.
        let cg_y_offset = ph as f64 - start_y as f64 - h as f64;
        context.translate(0.0, -cg_y_offset);

        let full_rect = core_graphics::geometry::CGRect::new(
            &core_graphics::geometry::CGPoint::new(0.0, 0.0),
            &core_graphics::geometry::CGSize::new(pw as f64, ph as f64),
        );
        context.draw_image(full_rect, &self.cached_image);
        Some(context.data().to_vec())
    }
}

