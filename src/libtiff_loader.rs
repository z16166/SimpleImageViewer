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
use libtiff_viewer as lib;
use memmap2::Mmap;
use std::ffi::{CString, c_int, c_void};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Context passed to libtiff callbacks
struct TiffMmapContext {
    mmap: Arc<Mmap>,
    offset: u64,
}

// --- libtiff Callbacks over memmap2::Mmap ---

unsafe extern "C" fn tiff_read_proc(
    handle: *mut c_void,
    buf: *mut c_void,
    size: lib::tsize_t,
) -> lib::tsize_t {
    let ctx = unsafe { &mut *(handle as *mut TiffMmapContext) };
    let mmap_len = ctx.mmap.len() as u64;

    if ctx.offset >= mmap_len {
        return 0;
    }

    let rem = mmap_len - ctx.offset;
    let to_read = (size as u64).min(rem);

    if to_read > 0 {
        unsafe {
            std::ptr::copy_nonoverlapping(
                ctx.mmap.as_ptr().add(ctx.offset as usize),
                buf as *mut u8,
                to_read as usize,
            );
        }
        ctx.offset += to_read;
    }
    to_read as lib::tsize_t
}

unsafe extern "C" fn tiff_write_proc(
    _: *mut c_void,
    _: *mut c_void,
    _: lib::tsize_t,
) -> lib::tsize_t {
    0
}

unsafe extern "C" fn tiff_seek_proc(
    handle: *mut c_void,
    off: lib::toff_t,
    whence: c_int,
) -> lib::toff_t {
    let ctx = unsafe { &mut *(handle as *mut TiffMmapContext) };
    match whence {
        0 => ctx.offset = off,                                         // SEEK_SET
        1 => ctx.offset = (ctx.offset as i64 + off as i64) as u64,     // SEEK_CUR
        2 => ctx.offset = (ctx.mmap.len() as i64 + off as i64) as u64, // SEEK_END
        _ => {}
    }
    ctx.offset
}

unsafe extern "C" fn tiff_close_proc(_: *mut c_void) -> c_int {
    0
}

unsafe extern "C" fn tiff_size_proc(handle: *mut c_void) -> lib::toff_t {
    let ctx = unsafe { &*(handle as *const TiffMmapContext) };
    ctx.mmap.len() as u64
}

unsafe extern "C" fn tiff_map_proc(
    handle: *mut c_void,
    base: *mut *mut c_void,
    size: *mut lib::toff_t,
) -> c_int {
    let ctx = unsafe { &*(handle as *const TiffMmapContext) };
    unsafe {
        *base = ctx.mmap.as_ptr() as *mut c_void;
        *size = ctx.mmap.len() as u64;
    }
    1
}

unsafe extern "C" fn tiff_unmap_proc(_: *mut c_void, _: *mut c_void, _: lib::toff_t) {}

/// RAII handle for a TIFF object, ensures the handle is closed and context is kept alive.
pub struct TiffHandle {
    ptr: *mut lib::TIFF,
    _context: Box<TiffMmapContext>,
}

impl Drop for TiffHandle {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe {
                lib::TIFFClose(self.ptr);
            }
        }
    }
}

unsafe impl Send for TiffHandle {}
unsafe impl Sync for TiffHandle {}

fn create_tiff_handle(mmap: Arc<Mmap>, path: &Path) -> Result<TiffHandle, String> {
    let mut ctx = Box::new(TiffMmapContext { mmap, offset: 0 });

    unsafe {
        let c_path = match CString::new(path.to_str().unwrap_or("image.tif")) {
            Ok(c) => c,
            Err(_) => return Err("Invalid path string for C conversion".to_string()),
        };
        let c_mode = match CString::new("r") {
            Ok(c) => c,
            Err(_) => return Err("Invalid mode string for C conversion".to_string()),
        };

        let tif_ptr = lib::TIFFClientOpen(
            c_path.as_ptr(),
            c_mode.as_ptr(),
            ctx.as_mut() as *mut TiffMmapContext as *mut c_void,
            tiff_read_proc,
            tiff_write_proc,
            tiff_seek_proc,
            tiff_close_proc,
            tiff_size_proc,
            tiff_map_proc,
            tiff_unmap_proc,
        );

        if tif_ptr.is_null() {
            return Err("TIFFClientOpen failed".to_string());
        }
        Ok(TiffHandle {
            ptr: tif_ptr,
            _context: ctx,
        })
    }
}

// --- Tiled Implementation (Physical Tiles) ---

pub struct LibTiffTiledSource {
    path: PathBuf,
    mmap: Arc<Mmap>,
    width: u32,
    height: u32,
    tile_width: u32,
    tile_height: u32,
    pool: Mutex<Vec<TiffHandle>>,
}

impl LibTiffTiledSource {
    fn acquire_handle(&self) -> Result<TiffHandle, String> {
        {
            let mut pool = self.pool.lock().map_err(|e| e.to_string())?;
            if let Some(handle) = pool.pop() {
                return Ok(handle);
            }
        }
        create_tiff_handle(self.mmap.clone(), &self.path)
    }

    fn release_handle(&self, handle: TiffHandle) {
        if let Ok(mut pool) = self.pool.lock() {
            pool.push(handle);
        }
    }
}

unsafe impl Send for LibTiffTiledSource {}
unsafe impl Sync for LibTiffTiledSource {}

fn extract_embedded_thumbnail(
    tif: *mut lib::TIFF,
    main_width: u32,
    target_size: u32,
) -> Option<(u32, u32, Vec<u8>)> {
    unsafe {
        let mut best_index = 0;
        let mut best_dim = 0;
        let mut best_pixels = None;

        // Iterate through IFDs to find the best-fitting thumbnail
        let mut dir_idx = 1;
        while lib::TIFFSetDirectory(tif, dir_idx) != 0 {
            let mut tw: lib::uint32 = 0;
            let mut th: lib::uint32 = 0;
            lib::TIFFGetField(tif, lib::TIFFTAG_IMAGEWIDTH, &mut tw);
            lib::TIFFGetField(tif, lib::TIFFTAG_IMAGELENGTH, &mut th);

            let dim = tw.max(th);
            let total_pixels = tw as u64 * th as u64;
            if total_pixels > 64 * 1024 * 1024 {
                // 64MP Limit
                dir_idx += 1;
                continue;
            }

            if tw > 0 && th > 0 && tw < main_width {
                if dim >= target_size && (best_pixels.is_none() || dim < best_dim) {
                    best_dim = dim;
                    best_index = dir_idx;

                    let mut raster = vec![0u32; (tw * th) as usize];
                    if lib::TIFFReadRGBAImageOriented(
                        tif,
                        tw,
                        th,
                        raster.as_mut_ptr(),
                        lib::ORIENTATION_TOPLEFT,
                        0,
                    ) != 0
                    {
                        let mut pixels = vec![0u8; (tw * th * 4) as usize];
                        std::ptr::copy_nonoverlapping(
                            raster.as_ptr() as *const u8,
                            pixels.as_mut_ptr(),
                            pixels.len(),
                        );
                        best_pixels = Some((tw as u32, th as u32, pixels));
                    }
                } else if best_pixels.is_none() && dim > best_dim {
                    best_dim = dim;
                    best_index = dir_idx;

                    let mut raster = vec![0u32; (tw * th) as usize];
                    if lib::TIFFReadRGBAImageOriented(
                        tif,
                        tw,
                        th,
                        raster.as_mut_ptr(),
                        lib::ORIENTATION_TOPLEFT,
                        0,
                    ) != 0
                    {
                        let mut pixels = vec![0u8; (tw * th * 4) as usize];
                        std::ptr::copy_nonoverlapping(
                            raster.as_ptr() as *const u8,
                            pixels.as_mut_ptr(),
                            pixels.len(),
                        );
                        best_pixels = Some((tw as u32, th as u32, pixels));
                    }
                }
            }
            dir_idx += 1;
        }

        lib::TIFFSetDirectory(tif, 0);
        if let Some(res) = best_pixels {
            log::info!(
                "LibTiff: Using embedded IFD{} thumbnail ({}x{}) for target size {}",
                best_index,
                res.0,
                res.1,
                target_size
            );
            return Some(res);
        }
        None
    }
}

impl TiledImageSource for LibTiffTiledSource {
    fn width(&self) -> u32 {
        self.width
    }
    fn height(&self) -> u32 {
        self.height
    }

    fn extract_tile(&self, x: u32, y: u32, w: u32, h: u32) -> Vec<u8> {
        let mut result = vec![0u8; (w as usize) * (h as usize) * 4];
        let handle = match self.acquire_handle() {
            Ok(h) => h,
            Err(e) => {
                log::error!(
                    "[{}] libtiff: Failed to acquire handle for tile: {}",
                    self.path.display(),
                    e
                );
                return result;
            }
        };

        let tif_ptr = handle.ptr;
        let tw = self.tile_width;
        let th = self.tile_height;
        let mut tile_buf = vec![0u32; (tw as usize) * (th as usize)];
        let start_tx = (x / tw) * tw;
        let start_ty = (y / th) * th;

        for curr_ty in (start_ty..(y + h)).step_by(th as usize) {
            for curr_tx in (start_tx..(x + w)).step_by(tw as usize) {
                unsafe {
                    if lib::TIFFReadRGBATile(tif_ptr, curr_tx, curr_ty, tile_buf.as_mut_ptr()) != 0
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
                                let dest_idx = (dest_y as usize * w as usize + dest_x as usize) * 4;
                                let src_idx =
                                    (th - 1 - ty_in_p) as usize * tw as usize + tx_in_p as usize;

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
        result
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

        let embedded = extract_embedded_thumbnail(handle.ptr, self.width, max_dim);

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

        let tif_ptr = handle.ptr;
        let tw = self.tile_width;
        let th = self.tile_height;
        let mut tile_buf = vec![0u32; (tw * th) as usize];
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
                let tiles_across = (self.width + tw - 1) / tw;
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

// --- Scanline Implementation (Mock Tiles from Strips) ---

pub struct LibTiffScanlineSource {
    path: PathBuf,
    mmap: Arc<Mmap>,
    width: u32,
    height: u32,
    rows_per_strip: u32,
    pool: Mutex<Vec<TiffHandle>>,
    strip_cache: Mutex<std::collections::HashMap<u32, Arc<Vec<u8>>>>,
    cache_order: Mutex<Vec<u32>>,
    max_cached_strips: usize,
}

impl LibTiffScanlineSource {
    fn acquire_handle(&self) -> Result<TiffHandle, String> {
        {
            let mut pool = self.pool.lock().map_err(|e| e.to_string())?;
            if let Some(handle) = pool.pop() {
                return Ok(handle);
            }
        }
        create_tiff_handle(self.mmap.clone(), &self.path)
    }

    fn release_handle(&self, handle: TiffHandle) {
        if let Ok(mut pool) = self.pool.lock() {
            pool.push(handle);
        }
    }

    fn get_or_decode_strip(&self, strip_idx: u32, handle: &TiffHandle) -> Option<Arc<Vec<u8>>> {
        {
            let cache = self.strip_cache.lock().unwrap();
            if let Some(data) = cache.get(&strip_idx) {
                let mut order = self.cache_order.lock().unwrap();
                if let Some(pos) = order.iter().position(|&k| k == strip_idx) {
                    order.remove(pos);
                }
                order.push(strip_idx);
                return Some(Arc::clone(data));
            }
        }

        let rps = self.rows_per_strip;
        let mut strip_buf = vec![0u32; (self.width as usize) * (rps as usize)];

        let decoded = unsafe {
            lib::TIFFReadRGBAStrip(handle.ptr, strip_idx * rps, strip_buf.as_mut_ptr()) != 0
        };

        if !decoded {
            return None;
        }

        let actual_rows = if (strip_idx + 1) * rps > self.height {
            self.height - strip_idx * rps
        } else {
            rps
        };
        let mut rgba = vec![0u8; (self.width as usize) * (actual_rows as usize) * 4];
        for row in 0..actual_rows {
            let src_row = (rps - 1 - row) as usize;
            let src_offset = src_row * self.width as usize;
            let dst_offset = row as usize * self.width as usize * 4;
            for col in 0..self.width as usize {
                let src_idx = src_offset + col;
                if src_idx < strip_buf.len() {
                    let pixel = strip_buf[src_idx].to_ne_bytes();
                    let dst_idx = dst_offset + col * 4;
                    rgba[dst_idx..dst_idx + 4].copy_from_slice(&pixel);
                }
            }
        }
        let data = Arc::new(rgba);

        {
            let mut cache = self.strip_cache.lock().unwrap();
            let mut order = self.cache_order.lock().unwrap();

            while order.len() >= self.max_cached_strips {
                if let Some(oldest) = order.first().copied() {
                    order.remove(0);
                    cache.remove(&oldest);
                }
            }

            cache.insert(strip_idx, Arc::clone(&data));
            order.push(strip_idx);
        }

        Some(data)
    }
}

unsafe impl Send for LibTiffScanlineSource {}
unsafe impl Sync for LibTiffScanlineSource {}

impl TiledImageSource for LibTiffScanlineSource {
    fn width(&self) -> u32 {
        self.width
    }
    fn height(&self) -> u32 {
        self.height
    }

    fn extract_tile(&self, x: u32, y: u32, w: u32, h: u32) -> Vec<u8> {
        let mut result = vec![0u8; (w as usize) * (h as usize) * 4];
        let handle = match self.acquire_handle() {
            Ok(h) => h,
            Err(e) => {
                log::error!(
                    "[{}] libtiff: Failed to acquire handle for scanline: {}",
                    self.path.display(),
                    e
                );
                return result;
            }
        };

        let rps = self.rows_per_strip;
        let start_strip = y / rps;
        let end_strip = (y + h - 1) / rps;

        for strip_idx in start_strip..=end_strip {
            let strip_data = match self.get_or_decode_strip(strip_idx, &handle) {
                Some(d) => d,
                None => continue,
            };

            let strip_y_start = strip_idx * rps;
            let actual_rows = if (strip_idx + 1) * rps > self.height {
                self.height - strip_y_start
            } else {
                rps
            };

            let intersect_y_start = y.max(strip_y_start);
            let intersect_y_end = (y + h).min(strip_y_start + actual_rows).min(self.height);
            let intersect_x_start = x;
            let intersect_x_end = (x + w).min(self.width);

            if intersect_y_start >= intersect_y_end || intersect_x_start >= intersect_x_end {
                continue;
            }

            let copy_bytes = (intersect_x_end - intersect_x_start) as usize * 4;

            for py in intersect_y_start..intersect_y_end {
                let row_in_strip = (py - strip_y_start) as usize;
                let src_offset =
                    (row_in_strip * self.width as usize + intersect_x_start as usize) * 4;
                let dst_y = (py - y) as usize;
                let dst_offset = (dst_y * w as usize + (intersect_x_start - x) as usize) * 4;

                if src_offset + copy_bytes <= strip_data.len()
                    && dst_offset + copy_bytes <= result.len()
                {
                    result[dst_offset..dst_offset + copy_bytes]
                        .copy_from_slice(&strip_data[src_offset..src_offset + copy_bytes]);
                }
            }
        }

        self.release_handle(handle);
        result
    }

    fn generate_preview(&self, max_w: u32, max_h: u32) -> (u32, u32, Vec<u8>) {
        let max_dim = max_w.max(max_h);
        let handle = match self.acquire_handle() {
            Ok(h) => h,
            Err(e) => {
                log::error!(
                    "[{}] libtiff: Failed to acquire handle for scanline preview: {}",
                    self.path.display(),
                    e
                );
                return (0, 0, vec![]);
            }
        };

        let embedded = extract_embedded_thumbnail(handle.ptr, self.width, max_dim);

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
            "libtiff: Generating stride-based fallback preview from strips ({}x{})",
            pw,
            ph
        );

        let tif_ptr = handle.ptr;
        let rps = self.rows_per_strip;
        let mut strip_buf = vec![0u32; (self.width as usize) * (rps as usize)];
        let mut last_strip_idx = u32::MAX;

        let stride_x_fp = ((self.width as u64) << 16) / pw as u64;
        let stride_y_fp = ((self.height as u64) << 16) / ph as u64;

        for ty in 0..ph {
            let y = ((ty as u64 * stride_y_fp) >> 16) as u32;
            let strip_idx = y / rps;
            let y_in_strip = y % rps;
            let dst_y_offset = (ty * pw) as usize * 4;

            unsafe {
                if strip_idx != last_strip_idx {
                    if lib::TIFFReadRGBAStrip(tif_ptr, strip_idx * rps, strip_buf.as_mut_ptr()) != 0
                    {
                        last_strip_idx = strip_idx;
                    } else {
                        continue;
                    }
                }

                for tx in 0..pw {
                    let x = ((tx as u64 * stride_x_fp) >> 16) as u32;
                    let src_idx =
                        (rps - 1 - y_in_strip) as usize * self.width as usize + x as usize;
                    if src_idx < strip_buf.len() {
                        let pixel = strip_buf[src_idx].to_ne_bytes();
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

// TIFF Photometric Interpretations
const PHOTO_MINISWHITE: u16 = 0; // 0 is pure white, max is pure black (common in legacy faxes/scans)
const PHOTO_MINISBLACK: u16 = 1; // 0 is pure black, max is pure white (modern standard grayscale)
const PHOTO_RGB: u16 = 2;
const PHOTO_PALETTE: u16 = 3;
const PHOTO_SEPARATED: u16 = 5;
const PHOTO_LOGL: u16 = 32844;
const PHOTO_LOGLUV: u16 = 32845;

// TIFF Sample Formats
const FORMAT_UINT: u16 = 1;
const FORMAT_INT: u16 = 2;
const FORMAT_IEEEFP: u16 = 3;

// TIFF Planar Configurations
const CONFIG_CONTIG: u16 = 1; // Contiguous / Chunky format (e.g., RGBRGBRGB...)
const CONFIG_SEPARATE: u16 = 2; // Planar format (e.g., RRR... GGG... BBB...)

// TIFF Compressions
const COMPRESSION_THUNDERSCAN: u16 = 32809;

unsafe fn manual_decode_scanline(
    tif: *mut lib::TIFF,
    width: u32,
    height: u32,
) -> Result<Vec<u8>, String> {
    let mut bps: u16 = 0;
    let mut spp: u16 = 0;
    let mut photo: u16 = 0;
    let mut config: u16 = CONFIG_CONTIG;
    let mut format: u16 = FORMAT_UINT; // SampleFormat
    let mut compression: u16 = 0;

    let swapped: bool;
    let mut smin: f64 = 0.0;
    let mut smax: f64 = 1.0;
    unsafe {
        lib::TIFFGetField(tif, lib::TIFFTAG_BITSPERSAMPLE, &mut bps);
        lib::TIFFGetField(tif, lib::TIFFTAG_SAMPLESPERPIXEL, &mut spp);
        lib::TIFFGetField(tif, lib::TIFFTAG_PHOTOMETRIC, &mut photo);
        lib::TIFFGetField(tif, lib::TIFFTAG_PLANARCONFIG, &mut config);
        lib::TIFFGetField(tif, lib::TIFFTAG_SAMPLEFORMAT, &mut format);
        lib::TIFFGetField(tif, lib::TIFFTAG_COMPRESSION, &mut compression);
        lib::TIFFGetField(tif, lib::TIFFTAG_COMPRESSION, &mut compression);

        swapped = lib::TIFFIsByteSwapped(tif) != 0;
    }

    let scanline_size = unsafe { lib::TIFFScanlineSize(tif) };
    if scanline_size <= 0 {
        return Err("Invalid scanline size".to_string());
    }

    let mut buf = vec![0u8; scanline_size as usize];
    let mut rgba = vec![255u8; width as usize * height as usize * 4];

    // Palette handling
    let mut r_map: *mut u16 = std::ptr::null_mut();
    let mut g_map: *mut u16 = std::ptr::null_mut();
    let mut b_map: *mut u16 = std::ptr::null_mut();
    if photo == PHOTO_PALETTE {
        if unsafe {
            lib::TIFFGetField(
                tif,
                lib::TIFFTAG_COLORMAP,
                &mut r_map,
                &mut g_map,
                &mut b_map,
            )
        } == 0
        {
            return Err("Palette image missing colormap".to_string());
        }
    }
    let samples_to_process = (spp as usize).min(match photo {
        PHOTO_RGB | PHOTO_SEPARATED => 4, // RGB(A) and CMYK
        _ => 1,
    });

    // Determine if we need a two-pass normalization for floats or large integers
    let mut smax_provided = false;
    let mut smin_provided = false;
    unsafe {
        let mut smin_ptr: *mut f64 = std::ptr::null_mut();
        let mut smax_ptr: *mut f64 = std::ptr::null_mut();
        if lib::TIFFGetField(tif, lib::TIFFTAG_SMINSAMPLEVALUE, &mut smin_ptr) != 0
            && !smin_ptr.is_null()
        {
            smin = *smin_ptr;
            smin_provided = true;
        }
        if lib::TIFFGetField(tif, lib::TIFFTAG_SMAXSAMPLEVALUE, &mut smax_ptr) != 0
            && !smax_ptr.is_null()
        {
            smax = *smax_ptr;
            smax_provided = true;
        }
    }

    // Auto-scale HDR formats if SMax is missing, excluding CMYK which has absolute values
    let use_auto_scale = !smax_provided
        && photo != PHOTO_SEPARATED
        && (format == FORMAT_IEEEFP || bps == 16 || bps == 32 || bps == 64);

    if use_auto_scale {
        let mut actual_min = f64::MAX;
        let mut actual_max = f64::MIN;

        let scans_per_row = if config == CONFIG_SEPARATE {
            samples_to_process
        } else {
            1
        };

        for s in 0..scans_per_row {
            for y in 0..height {
                if unsafe {
                    lib::TIFFReadScanline(tif, buf.as_mut_ptr() as *mut c_void, y, s as u16)
                } > 0
                {
                    let num_samples = if config == CONFIG_SEPARATE {
                        width as usize
                    } else {
                        width as usize * spp as usize
                    };
                    for idx in 0..num_samples {
                        let val = get_raw_value(&buf, idx, bps, format);
                        if val.is_finite() {
                            if val < actual_min {
                                actual_min = val;
                            }
                            if val > actual_max {
                                actual_max = val;
                            }
                        }
                    }
                }
            }
        }

        if actual_max > actual_min {
            if !smin_provided {
                smin = actual_min;
            }
            smax = actual_max;
        }
    }

    if config == CONFIG_CONTIG {
        // Contig
        for y in 0..height {
            if unsafe { lib::TIFFReadScanline(tif, buf.as_mut_ptr() as *mut c_void, y, 0) } <= 0 {
                buf.fill(0);
            }
            let row_offset = y as usize * width as usize * 4;
            process_scanline_contig(
                &buf,
                &mut rgba[row_offset..],
                width,
                bps,
                spp,
                photo,
                format,
                swapped,
                smin,
                smax,
                r_map,
                g_map,
                b_map,
            );
        }
    } else {
        // Separate
        for s in 0..samples_to_process {
            for y in 0..height {
                if unsafe {
                    lib::TIFFReadScanline(tif, buf.as_mut_ptr() as *mut c_void, y, s as u16)
                } <= 0
                {
                    buf.fill(0);
                }
                let row_offset = y as usize * width as usize * 4;
                process_scanline_separate(
                    &buf,
                    &mut rgba[row_offset..],
                    width,
                    bps,
                    s as usize,
                    photo,
                    format,
                    swapped,
                    smin,
                    smax,
                );
            }
        }
    }
    Ok(rgba)
}

fn get_raw_value(buf: &[u8], idx: usize, bps: u16, format: u16) -> f64 {
    match (bps, format) {
        (16, _) => unsafe {
            std::ptr::read_unaligned(buf.as_ptr().add(idx * 2) as *const u16) as f64
        },
        (32, FORMAT_UINT) => unsafe {
            std::ptr::read_unaligned(buf.as_ptr().add(idx * 4) as *const u32) as f64
        },
        (32, FORMAT_INT) => unsafe {
            std::ptr::read_unaligned(buf.as_ptr().add(idx * 4) as *const i32) as f64
        },
        (32, FORMAT_IEEEFP) => unsafe {
            f32::from_bits(std::ptr::read_unaligned(
                buf.as_ptr().add(idx * 4) as *const u32
            )) as f64
        },
        (64, FORMAT_UINT) => unsafe {
            std::ptr::read_unaligned(buf.as_ptr().add(idx * 8) as *const u64) as f64
        },
        (64, FORMAT_IEEEFP) => unsafe {
            f64::from_bits(std::ptr::read_unaligned(
                buf.as_ptr().add(idx * 8) as *const u64
            ))
        },
        _ => 0.0,
    }
}

fn process_scanline_contig(
    buf: &[u8],
    rgba_row: &mut [u8],
    width: u32,
    bps: u16,
    spp: u16,
    photo: u16,
    format: u16,
    swapped: bool,
    smin: f64,
    smax: f64,
    r_map: *mut u16,
    g_map: *mut u16,
    b_map: *mut u16,
) {
    let is_palette = photo == PHOTO_PALETTE;
    for x in 0..width as usize {
        let dst_idx = x * 4;
        let src_sample_offset = x * spp as usize;

        let mut samples = [0u32; 4];
        for s in 0..(spp as usize).min(4) {
            samples[s] = get_sample_value(
                buf,
                src_sample_offset + s,
                bps,
                format,
                swapped,
                smin,
                smax,
                is_palette,
                photo,
            );
        }

        match photo {
            PHOTO_MINISWHITE | PHOTO_MINISBLACK => {
                let v = (if photo == PHOTO_MINISWHITE {
                    255 - samples[0].min(255)
                } else {
                    samples[0].min(255)
                }) as u8;
                rgba_row[dst_idx] = v;
                rgba_row[dst_idx + 1] = v;
                rgba_row[dst_idx + 2] = v;
            }
            PHOTO_RGB => {
                rgba_row[dst_idx] = samples[0] as u8;
                rgba_row[dst_idx + 1] = samples[1] as u8;
                rgba_row[dst_idx + 2] = samples[2] as u8;
                if spp >= 4 {
                    rgba_row[dst_idx + 3] = samples[3] as u8;
                }
            }
            PHOTO_SEPARATED => {
                // Separated (CMYK)
                let c = samples[0] as u8;
                let m = if spp > 1 { samples[1] as u8 } else { 0 };
                let y = if spp > 2 { samples[2] as u8 } else { 0 };
                let k = if spp > 3 { samples[3] as u8 } else { 0 };
                rgba_row[dst_idx] = ((255 - c) as u16 * (255 - k) as u16 / 255) as u8;
                rgba_row[dst_idx + 1] = ((255 - m) as u16 * (255 - k) as u16 / 255) as u8;
                rgba_row[dst_idx + 2] = ((255 - y) as u16 * (255 - k) as u16 / 255) as u8;
                rgba_row[dst_idx + 3] = 255;
            }
            PHOTO_PALETTE => {
                let idx = get_sample_value(buf, x, bps, format, swapped, smin, smax, true, photo)
                    as usize;
                unsafe {
                    let r_ptr = r_map.add(idx);
                    let g_ptr = g_map.add(idx);
                    let b_ptr = b_map.add(idx);
                    rgba_row[dst_idx] = (*r_ptr >> 8) as u8;
                    rgba_row[dst_idx + 1] = (*g_ptr >> 8) as u8;
                    rgba_row[dst_idx + 2] = (*b_ptr >> 8) as u8;
                    rgba_row[dst_idx + 3] = 255;
                }
            }
            _ => {}
        }
    }
}

fn process_scanline_separate(
    buf: &[u8],
    rgba_row: &mut [u8],
    width: u32,
    bps: u16,
    sample_idx: usize,
    photo: u16,
    format: u16,
    swapped: bool,
    smin: f64,
    smax: f64,
) {
    let is_palette = photo == PHOTO_PALETTE;
    for x in 0..width as usize {
        let dst_idx = x * 4;
        let val = get_sample_value(buf, x, bps, format, swapped, smin, smax, is_palette, photo);

        match photo {
            PHOTO_MINISWHITE | PHOTO_MINISBLACK => {
                let v = if photo == PHOTO_MINISWHITE {
                    255 - val
                } else {
                    val
                } as u8;
                rgba_row[dst_idx] = v;
                rgba_row[dst_idx + 1] = v;
                rgba_row[dst_idx + 2] = v;
            }
            PHOTO_RGB => {
                if sample_idx < 4 {
                    rgba_row[dst_idx + sample_idx] = val as u8;
                }
            }
            PHOTO_SEPARATED => {
                // Separated (CMYK)
                // In planar mode, we need a way to store samples 0,1,2,3 without overwriting RGBA yet.
                // We'll use a trick: since we know we are in manual_decode_scanline, we can assume
                // the caller might have a temporary buffer, but here we only have rgba_row.
                // Let's use the actual samples from get_sample_value.
                // To avoid corruption, we only convert to RGB on the LAST sample.
                // But where to store C, M, Y while waiting for K?
                // Actually, the current manual_decode_scanline for Separate (planar) reads ONE channel
                // for the WHOLE image before moving to the next channel.
                // So when s=0, we write ALL Cyan values to rgba_row[0].
                // This means rgba_row is actually shared across all pixels.
                // Wait! rgba_row is just for ONE scanline.
                // So s=0: writes C to all rgba_row[x*4+0].
                // s=1: writes M to all rgba_row[x*4+1].
                // s=3: reads C,M,Y from rgba_row and converts.
                // THIS WORKS FINE as long as val is stored as u8.
                rgba_row[dst_idx + sample_idx] = val as u8;
                if sample_idx == 3 {
                    let c = rgba_row[dst_idx];
                    let m = rgba_row[dst_idx + 1];
                    let y = rgba_row[dst_idx + 2];
                    let k = rgba_row[dst_idx + 3];
                    rgba_row[dst_idx] = ((255 - c) as u16 * (255 - k) as u16 / 255) as u8;
                    rgba_row[dst_idx + 1] = ((255 - m) as u16 * (255 - k) as u16 / 255) as u8;
                    rgba_row[dst_idx + 2] = ((255 - y) as u16 * (255 - k) as u16 / 255) as u8;
                    rgba_row[dst_idx + 3] = 255;
                }
            }
            PHOTO_LOGL | PHOTO_LOGLUV => {
                // LogL / LogLuv
                let v = val as u8;
                rgba_row[dst_idx] = v;
                rgba_row[dst_idx + 1] = v;
                rgba_row[dst_idx + 2] = v;
                rgba_row[dst_idx + 3] = 255;
            }
            _ => {}
        }
    }
}

fn get_sample_value(
    buf: &[u8],
    idx: usize,
    bps: u16,
    format: u16,
    _swapped: bool,
    smin: f64,
    smax: f64,
    is_palette: bool,
    photo: u16,
) -> u32 {
    let range = smax - smin;

    // Handle packed bitstreams (1, 2, 4, 6, 10, 12, 14)
    if bps < 16 && bps != 8 && format == FORMAT_UINT {
        let bit_offset = idx * bps as usize;
        let byte_idx = bit_offset / 8;
        let bit_in_byte = bit_offset % 8;

        let mut val: u32 = (buf[byte_idx] as u32) << 16;
        if byte_idx + 1 < buf.len() {
            val |= (buf[byte_idx + 1] as u32) << 8;
        }
        if byte_idx + 2 < buf.len() {
            val |= buf[byte_idx + 2] as u32;
        }

        let shift = 24 - bit_in_byte - bps as usize;
        let mask = (1u32 << bps) - 1;
        let res = (val >> shift) & mask;

        if is_palette {
            return res;
        }

        let linear = if smax > smin && (smin != 0.0 || smax != 1.0) {
            ((res as f64 - smin) / range).clamp(0.0, 1.0) as f32
        } else {
            (res as f32) / ((1u32 << bps) - 1) as f32
        };

        return if photo != PHOTO_SEPARATED {
            to_srgb_8(linear) as u32
        } else {
            (linear * 255.0) as u32
        };
    }

    // High bit depth or floats.
    // CRITICAL: libtiff ALWAYS returns 16, 32, and 64-bit samples in the host's NATIVE byte order!
    // We MUST NOT perform manual swap_bytes on them.
    let f_val: f64 = match (bps, format) {
        (8, _) => buf[idx] as f64,
        (16, _) => unsafe {
            std::ptr::read_unaligned(buf.as_ptr().add(idx * 2) as *const u16) as f64
        },
        (24, _) => {
            // libtiff doesn't natively swap 24-bit arrays. We must swap based on _swapped flag.
            let b0 = buf[idx * 3] as u32;
            let b1 = buf[idx * 3 + 1] as u32;
            let b2 = buf[idx * 3 + 2] as u32;
            let val = if _swapped {
                (b0 << 16) | (b1 << 8) | b2
            } else {
                (b2 << 16) | (b1 << 8) | b0
            };
            val as f64
        }
        (32, 1) => unsafe {
            std::ptr::read_unaligned(buf.as_ptr().add(idx * 4) as *const u32) as f64
        },
        (32, 2) => unsafe {
            std::ptr::read_unaligned(buf.as_ptr().add(idx * 4) as *const i32) as f64
        },
        (32, 3) => unsafe {
            f32::from_bits(std::ptr::read_unaligned(
                buf.as_ptr().add(idx * 4) as *const u32
            )) as f64
        },
        (64, 1) => unsafe {
            std::ptr::read_unaligned(buf.as_ptr().add(idx * 8) as *const u64) as f64
        },
        (64, 3) => unsafe {
            f64::from_bits(std::ptr::read_unaligned(
                buf.as_ptr().add(idx * 8) as *const u64
            ))
        },
        _ => 0.0,
    };

    if (bps == 8 || bps == 16) && is_palette {
        return f_val as u32;
    }

    // Default max for integers if smax is 1.0
    let effective_max = if smax <= 1.0 && format != FORMAT_IEEEFP {
        match bps {
            64 => 18446744073709551615.0,
            32 => 4294967295.0,
            24 => 16777215.0,
            16 => 65535.0,
            8 => 255.0,
            _ => 1.0,
        }
    } else {
        smax
    };

    let effective_range = effective_max - smin;
    let linear = if effective_range > 0.0 {
        ((f_val - smin) / effective_range).clamp(0.0, 1.0) as f32
    } else {
        (f_val - smin).clamp(0.0, 1.0) as f32
    };

    if photo == PHOTO_SEPARATED {
        (linear * 255.0) as u32
    } else {
        to_srgb_8(linear) as u32
    }
}

fn to_srgb_8(linear: f32) -> u8 {
    // Simple sRGB / Gamma 2.2 mapping
    let l = linear.clamp(0.0, 1.0);
    let s = if l <= 0.0031308 {
        12.92 * l
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    };
    (s * 255.0) as u8
}

pub fn load_via_libtiff(path: &Path) -> Result<ImageData, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mmap = Arc::new(unsafe { Mmap::map(&file).map_err(|e| e.to_string())? });

    let mut ctx = Box::new(TiffMmapContext {
        mmap: mmap.clone(),
        offset: 0,
    });

    unsafe {
        let c_path = match CString::new(path.to_str().unwrap_or("image.tif")) {
            Ok(c) => c,
            Err(_) => return Err("Invalid path string for C conversion".to_string()),
        };
        let c_mode = match CString::new("r") {
            Ok(c) => c,
            Err(_) => return Err("Invalid mode string for C conversion".to_string()),
        };

        let tif_ptr = lib::TIFFClientOpen(
            c_path.as_ptr(),
            c_mode.as_ptr(),
            ctx.as_mut() as *mut TiffMmapContext as *mut c_void,
            tiff_read_proc,
            tiff_write_proc,
            tiff_seek_proc,
            tiff_close_proc,
            tiff_size_proc,
            tiff_map_proc,
            tiff_unmap_proc,
        );

        if tif_ptr.is_null() {
            return Err("TIFFClientOpen failed".to_string());
        }

        let handle = TiffHandle {
            ptr: tif_ptr,
            _context: ctx,
        };

        let mut width: lib::uint32 = 0;
        let mut height: lib::uint32 = 0;
        lib::TIFFGetField(handle.ptr, lib::TIFFTAG_IMAGEWIDTH, &mut width);
        lib::TIFFGetField(handle.ptr, lib::TIFFTAG_IMAGELENGTH, &mut height);

        let mut bps: u16 = 0;
        lib::TIFFGetField(handle.ptr, lib::TIFFTAG_BITSPERSAMPLE, &mut bps);

        if width == 0 || height == 0 {
            return Err("TIFF has zero width or height".to_string());
        }

        let mut photo: u16 = 0;
        let mut compression: u16 = 0;
        let mut orientation: u16 = 1;
        lib::TIFFGetField(handle.ptr, lib::TIFFTAG_PHOTOMETRIC, &mut photo);
        lib::TIFFGetField(handle.ptr, lib::TIFFTAG_COMPRESSION, &mut compression);
        lib::TIFFGetField(handle.ptr, lib::TIFFTAG_ORIENTATION, &mut orientation);

        // Intercept formats that libtiff's RGBA interface fails to handle natively
        // 24/32/64-bit, 16-bit Grayscale, ThunderScan
        // Note: We leave CMYK (photo=PHOTO_SEPARATED) and LogL/LogLuv (PHOTO_LOGL/PHOTO_LOGLUV) to libtiff natively!
        let mut force_static = (bps != 8 && bps != 16)
            || (bps == 16 && (photo == PHOTO_MINISWHITE || photo == PHOTO_MINISBLACK))
            || (compression == COMPRESSION_THUNDERSCAN);

        let pixel_count = width as u64 * height as u64;

        // If orientation is complex (e.g. 90deg), force static decoding so we can rotate the full buffer,
        // UNLESS the image is huge (to prevent OOM). Most rotated images are from cameras and easily fit in static.
        // If it's a huge rotated image, it will fail static allocation limit and correctly fall back to WIC/ImageIO.
        if orientation > 1 && pixel_count <= 256 * 1024 * 1024 {
            force_static = true;
        }

        let pixel_count = width as u64 * height as u64;
        let limit = crate::tile_cache::get_max_texture_side();
        let tiled_threshold =
            crate::tile_cache::TILED_THRESHOLD.load(std::sync::atomic::Ordering::Relaxed);
        let is_large = pixel_count >= tiled_threshold || width > limit || height > limit;

        if !force_static && is_large {
            if lib::TIFFIsTiled(handle.ptr) != 0 {
                let mut tile_width: lib::uint32 = 0;
                let mut tile_height: lib::uint32 = 0;
                lib::TIFFGetField(handle.ptr, lib::TIFFTAG_TILEWIDTH, &mut tile_width);
                lib::TIFFGetField(handle.ptr, lib::TIFFTAG_TILELENGTH, &mut tile_height);

                if tile_width == 0 || tile_height == 0 {
                    return Err("TIFF is tiled but tile dimensions are zero".to_string());
                }

                return Ok(ImageData::Tiled(Arc::new(LibTiffTiledSource {
                    path: path.to_path_buf(),
                    mmap: mmap.clone(),
                    width,
                    height,
                    tile_width,
                    tile_height,
                    pool: Mutex::new(vec![handle]),
                })));
            } else {
                let mut rps: lib::uint32 = 0;
                if lib::TIFFGetField(handle.ptr, lib::TIFFTAG_ROWSPERSTRIP, &mut rps) == 0
                    || rps == 0
                {
                    rps = height;
                }

                let strip_bytes = width as usize * rps as usize * 4;
                let max_cached = if strip_bytes > 0 {
                    (256 * 1024 * 1024 / strip_bytes).max(16)
                } else {
                    64
                };

                return Ok(ImageData::Tiled(Arc::new(LibTiffScanlineSource {
                    path: path.to_path_buf(),
                    mmap: mmap.clone(),
                    width,
                    height,
                    rows_per_strip: rps,
                    pool: Mutex::new(vec![handle]),
                    strip_cache: Mutex::new(std::collections::HashMap::new()),
                    cache_order: Mutex::new(Vec::new()),
                    max_cached_strips: max_cached,
                })));
            }
        }

        let total_pixels = (width as usize) * (height as usize);
        if total_pixels > 256 * 1024 * 1024 {
            return Err("Static TIFF TOO LARGE for single pass decode".to_string());
        }

        // Try RGBA interface first (fast, handles color spaces)
        let mut bps: u16 = 0;
        lib::TIFFGetField(handle.ptr, lib::TIFFTAG_BITSPERSAMPLE, &mut bps);

        let mut success = false;
        let mut pixels = Vec::new();

        // Try RGBA interface first ONLY if not forced static
        if !force_static {
            let mut raster: Vec<lib::uint32> = vec![0; total_pixels];
            if lib::TIFFReadRGBAImageOriented(handle.ptr, width, height, raster.as_mut_ptr(), 1, 0)
                != 0
            {
                pixels = vec![0u8; total_pixels * 4];
                std::ptr::copy_nonoverlapping(
                    raster.as_ptr() as *const u8,
                    pixels.as_mut_ptr(),
                    pixels.len(),
                );
                success = true;
            }
        }

        if !success {
            // Fallback to manual scanline decode
            pixels = manual_decode_scanline(handle.ptr, width, height)?;
        }

        if orientation > 1 {
            let (out_w, out_h, out_pixels) =
                apply_orientation_buffer(pixels, width, height, orientation);
            width = out_w;
            height = out_h;
            pixels = out_pixels;
        }

        Ok(ImageData::Static(DecodedImage {
            width,
            height,
            pixels,
        }))
    }
}

pub(crate) fn apply_orientation_buffer(
    pixels: Vec<u8>,
    w: u32,
    h: u32,
    orientation: u16,
) -> (u32, u32, Vec<u8>) {
    if orientation <= 1 {
        return (w, h, pixels);
    }

    let (out_w, out_h) = if orientation >= 5 && orientation <= 8 {
        (h, w)
    } else {
        (w, h)
    };
    let mut out = vec![0u8; (out_w * out_h * 4) as usize];

    for y in 0..h {
        for x in 0..w {
            let (nx, ny) = match orientation {
                2 => (w - 1 - x, y),
                3 => (w - 1 - x, h - 1 - y),
                4 => (x, h - 1 - y),
                5 => (y, x),
                6 => (h - 1 - y, x),
                7 => (h - 1 - y, w - 1 - x),
                8 => (y, w - 1 - x),
                _ => (x, y),
            };
            let src_idx = (y * w + x) as usize * 4;
            let dst_idx = (ny * out_w + nx) as usize * 4;
            if dst_idx + 4 <= out.len() && src_idx + 4 <= pixels.len() {
                out[dst_idx..dst_idx + 4].copy_from_slice(&pixels[src_idx..src_idx + 4]);
            }
        }
    }
    (out_w, out_h, out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CStr;
    use std::path::Path;
    use walkdir::WalkDir;

    unsafe extern "C" fn tiff_error_handler(
        module: *const std::ffi::c_char,
        fmt: *const std::ffi::c_char,
        _ap: *mut std::ffi::c_void,
    ) {
        let module = if module.is_null() {
            "Unknown"
        } else {
            unsafe { CStr::from_ptr(module) }
                .to_str()
                .unwrap_or("Unknown")
        };
        let fmt = if fmt.is_null() {
            ""
        } else {
            unsafe { CStr::from_ptr(fmt) }.to_str().unwrap_or("")
        };
        println!("[TIFF Error] Module: {}, Message: {}", module, fmt);
    }

    unsafe extern "C" fn tiff_warning_handler(
        module: *const std::ffi::c_char,
        fmt: *const std::ffi::c_char,
        _ap: *mut std::ffi::c_void,
    ) {
        let module = if module.is_null() {
            "Unknown"
        } else {
            unsafe { CStr::from_ptr(module) }
                .to_str()
                .unwrap_or("Unknown")
        };
        let fmt = if fmt.is_null() {
            ""
        } else {
            unsafe { CStr::from_ptr(fmt) }.to_str().unwrap_or("")
        };
        println!("[TIFF Warning] Module: {}, Message: {}", module, fmt);
    }

    #[test]
    fn tiff_stress_test() {
        unsafe {
            lib::TIFFSetErrorHandler(Some(tiff_error_handler));
            lib::TIFFSetWarningHandler(Some(tiff_warning_handler));
        }

        let root = Path::new(r"F:\win7\libtiffpic\");
        if !root.exists() {
            println!(
                "Root path {} does not exist, skipping stress test.",
                root.display()
            );
            return;
        }

        let mut total = 0;
        let mut failed = 0;

        for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_file() {
                let ext = path
                    .extension()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                if ext == "tif" || ext == "tiff" {
                    total += 1;
                    match load_via_libtiff(path) {
                        Ok(_) => {
                            // println!("OK: {}", path.display());
                        }
                        Err(e) => {
                            failed += 1;
                            println!("FAILED: {} - Reason: {}", path.display(), e);

                            // Debug tags
                            unsafe {
                                let c_path =
                                    std::ffi::CString::new(path.to_str().unwrap()).unwrap();
                                let tif = lib::TIFFOpen(
                                    c_path.as_ptr(),
                                    b"r\0".as_ptr() as *const std::ffi::c_char,
                                );
                                if !tif.is_null() {
                                    let mut w: u32 = 0;
                                    let mut h: u32 = 0;
                                    let mut bps: u16 = 0;
                                    let mut spp: u16 = 0;
                                    let mut comp: u16 = 0;
                                    let mut photo: u16 = 0;
                                    lib::TIFFSetDirectory(tif, 0);
                                    let r1 =
                                        lib::TIFFGetField(tif, lib::TIFFTAG_IMAGEWIDTH, &mut w);
                                    let r2 =
                                        lib::TIFFGetField(tif, lib::TIFFTAG_IMAGELENGTH, &mut h);
                                    let r3 = lib::TIFFGetField(
                                        tif,
                                        lib::TIFFTAG_BITSPERSAMPLE,
                                        &mut bps,
                                    );
                                    let r4 = lib::TIFFGetField(
                                        tif,
                                        lib::TIFFTAG_SAMPLESPERPIXEL,
                                        &mut spp,
                                    );
                                    let r5 =
                                        lib::TIFFGetField(tif, lib::TIFFTAG_COMPRESSION, &mut comp);
                                    let r6 = lib::TIFFGetField(
                                        tif,
                                        lib::TIFFTAG_PHOTOMETRIC,
                                        &mut photo,
                                    );
                                    println!(
                                        "  TAGS: Res={}{}{}{}{}{}, Size={}x{}, BPS={}, SPP={}, Comp={}, Photo={}",
                                        r1, r2, r3, r4, r5, r6, w, h, bps, spp, comp, photo
                                    );
                                    lib::TIFFClose(tif);
                                }
                            }
                        }
                    }
                }
            }
        }

        println!("Summary: Total: {}, Failed: {}", total, failed);
    }
}
