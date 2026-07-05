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

use libtiff_viewer as lib;
use std::sync::Arc;

use super::handle::TiffHandle;
use crate::loader::TiledImageSource;

use memmap2::Mmap;
use parking_lot::Mutex;
use std::path::PathBuf;

use super::handle::create_tiff_handle;
use super::scratch::with_tiled_extract_scratch;
use super::thumbnail::extract_embedded_thumbnail;

fn checked_tile_pixel_count(tile_width: u32, tile_height: u32) -> Option<usize> {
    (tile_width as usize).checked_mul(tile_height as usize)
}

pub struct LibTiffTiledSource {
    pub(crate) path: PathBuf,
    pub(crate) mmap: Arc<Mmap>,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) tile_width: u32,
    pub(crate) tile_height: u32,
    pub(crate) pool: Mutex<Vec<TiffHandle>>,
}

impl LibTiffTiledSource {
    fn acquire_handle(&self) -> Result<TiffHandle, String> {
        {
            let mut pool = self.pool.lock();
            if let Some(handle) = pool.pop() {
                return Ok(handle);
            }
        }
        create_tiff_handle(self.mmap.clone(), &self.path)
    }

    fn release_handle(&self, handle: TiffHandle) {
        self.pool.lock().push(handle);
    }
}

impl TiledImageSource for LibTiffTiledSource {
    fn width(&self) -> u32 {
        self.width
    }
    fn height(&self) -> u32 {
        self.height
    }

    fn extract_tile(&self, x: u32, y: u32, w: u32, h: u32) -> std::sync::Arc<Vec<u8>> {
        let result_len = (w as usize)
            .checked_mul(h as usize)
            .and_then(|p| p.checked_mul(crate::constants::RGBA_CHANNELS))
            .unwrap_or(0);
        let tile_len = checked_tile_pixel_count(self.tile_width, self.tile_height).unwrap_or(0);

        let ((), result) = with_tiled_extract_scratch(result_len, tile_len, |scratch| {
            let result = &mut scratch.result;
            let tile_buf = &mut scratch.tile;
            let handle = match self.acquire_handle() {
                Ok(h) => h,
                Err(e) => {
                    log::error!(
                        "[{}] libtiff: Failed to acquire handle for tile: {}",
                        self.path.display(),
                        e
                    );
                    return;
                }
            };

            let tif_ptr = handle.as_ptr();
            let tw = self.tile_width;
            let th = self.tile_height;
            let start_tx = (x / tw) * tw;
            let start_ty = (y / th) * th;

            for curr_ty in (start_ty..(y + h)).step_by(th as usize) {
                for curr_tx in (start_tx..(x + w)).step_by(tw as usize) {
                    unsafe {
                        if lib::TIFFReadRGBATile(tif_ptr, curr_tx, curr_ty, tile_buf.as_mut_ptr())
                            != 0
                        {
                            for ty_in_p in 0..th {
                                let py = curr_ty + ty_in_p;
                                if py < y || py >= y + h {
                                    continue;
                                }
                                for tx_in_p in 0..tw {
                                    let px = curr_tx + tx_in_p;
                                    if px < x || px >= x + w {
                                        continue;
                                    }
                                    let dest_x = px - x;
                                    let dest_y = py - y;
                                    let dest_idx =
                                        (dest_y as usize * w as usize + dest_x as usize) * 4;
                                    let src_idx = (th - 1 - ty_in_p) as usize * tw as usize
                                        + tx_in_p as usize;

                                    if src_idx < tile_buf.len() && dest_idx + 4 <= result.len() {
                                        let pixel = tile_buf[src_idx].to_ne_bytes();
                                        result[dest_idx..dest_idx + 4].copy_from_slice(&pixel);
                                    }
                                }
                            }
                        }
                    }
                }
            }

            self.release_handle(handle);
        });

        std::sync::Arc::new(result)
    }

    fn generate_preview(&self, max_w: u32, max_h: u32) -> (u32, u32, Vec<u8>) {
        let max_dim = max_w.max(max_h);
        let handle = match self.acquire_handle() {
            Ok(h) => h,
            Err(e) => {
                log::error!(
                    "[{}] libtiff: Failed to acquire handle for preview: {}",
                    self.path.display(),
                    e
                );
                return (0, 0, vec![]);
            }
        };

        let embedded = extract_embedded_thumbnail(handle.as_ptr(), self.width, max_dim);

        if let Some(res) = embedded {
            let thumb_max = res.0.max(res.1);
            if max_w.max(max_h) <= 512 || thumb_max >= 2048 || thumb_max >= max_w.max(max_h) {
                self.release_handle(handle);
                return res;
            }
        }

        let scale = (max_w as f64 / self.width as f64)
            .min(max_h as f64 / self.height as f64)
            .min(1.0);
        let pw = (self.width as f64 * scale) as u32;
        let ph = (self.height as f64 * scale) as u32;
        if pw == 0 || ph == 0 {
            self.release_handle(handle);
            return (0, 0, vec![]);
        }

        let mut result = vec![0u8; (pw * ph * 4) as usize];
        log::info!(
            "libtiff: Generating stride-based fallback preview ({}x{})",
            pw,
            ph
        );

        let tif_ptr = handle.as_ptr();
        let tw = self.tile_width;
        let th = self.tile_height;
        let tile_len = match checked_tile_pixel_count(tw, th) {
            Some(len) => len,
            None => {
                log::error!(
                    "[{}] libtiff: tile buffer size overflow ({}x{})",
                    self.path.display(),
                    tw,
                    th
                );
                self.release_handle(handle);
                return (0, 0, vec![]);
            }
        };
        let mut tile_buf = vec![0u32; tile_len];
        let mut last_tile_idx = u32::MAX;

        let stride_x_fp = ((self.width as u64) << 16) / pw as u64;
        let stride_y_fp = ((self.height as u64) << 16) / ph as u64;

        for ty in 0..ph {
            let y = ((ty as u64 * stride_y_fp) >> 16) as u32;
            let tile_row = y / th;
            let y_in_tile = y % th;
            let dst_y_offset = (ty * pw) as usize * 4;

            for tx in 0..pw {
                let x = ((tx as u64 * stride_x_fp) >> 16) as u32;
                let tile_col = x / tw;
                let tiles_across = self.width.div_ceil(tw);
                let tile_idx = tile_row * tiles_across + tile_col;

                unsafe {
                    if tile_idx != last_tile_idx {
                        if lib::TIFFReadRGBATile(
                            tif_ptr,
                            tile_col * tw,
                            tile_row * th,
                            tile_buf.as_mut_ptr(),
                        ) != 0
                        {
                            last_tile_idx = tile_idx;
                        } else {
                            continue;
                        }
                    }
                    let x_in_tile = x % tw;
                    let src_idx = (th - 1 - y_in_tile) as usize * tw as usize + x_in_tile as usize;
                    if src_idx < tile_buf.len() {
                        let pixel = tile_buf[src_idx].to_ne_bytes();
                        let dst_idx = dst_y_offset + (tx as usize) * 4;
                        result[dst_idx..dst_idx + 4].copy_from_slice(&pixel);
                    }
                }
            }
        }

        self.release_handle(handle);
        (pw, ph, result)
    }

    fn full_pixels(&self) -> Option<Arc<Vec<u8>>> {
        None
    }
}
