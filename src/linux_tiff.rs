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
use std::ffi::{CString, c_void, c_char, c_int};
use std::sync::{Arc, Mutex};
use libloading::{Library, Symbol};
use crate::loader::{ImageData, DecodedImage, TiledImageSource};
use memmap2::Mmap;

// libtiff types
type TIFF = c_void;
#[allow(non_camel_case_types)]
type uint32 = u32;
#[allow(non_camel_case_types)]
type toff_t = u64;
#[allow(non_camel_case_types)]
type tsize_t = i64;

// TIFF tags & constants
const TIFFTAG_IMAGEWIDTH: uint32 = 256;
const TIFFTAG_IMAGELENGTH: uint32 = 257;
const TIFFTAG_TILEWIDTH: uint32 = 322;
const TIFFTAG_TILELENGTH: uint32 = 323;
const TIFFTAG_ROWSPERSTRIP: uint32 = 278;
const ORIENTATION_TOPLEFT: c_int = 1;

// libtiff callback types
type TIFFReadWriteProc = unsafe extern "C" fn(handle: *mut c_void, buf: *mut c_void, size: tsize_t) -> tsize_t;
type TIFFSeekProc = unsafe extern "C" fn(handle: *mut c_void, off: toff_t, whence: c_int) -> toff_t;
type TIFFCloseProc = unsafe extern "C" fn(handle: *mut c_void) -> c_int;
type TIFFSizeProc = unsafe extern "C" fn(handle: *mut c_void) -> toff_t;
type TIFFMapFileProc = unsafe extern "C" fn(handle: *mut c_void, base: *mut *mut c_void, size: *mut toff_t) -> c_int;
type TIFFUnmapFileProc = unsafe extern "C" fn(handle: *mut c_void, base: *mut c_void, size: toff_t);

// Function signatures
type TIFFClientOpenFn = unsafe extern "C" fn(
    name: *const c_char, mode: *const c_char, 
    handle: *mut c_void,
    read: TIFFReadWriteProc, write: TIFFReadWriteProc,
    seek: TIFFSeekProc, close: TIFFCloseProc, size: TIFFSizeProc,
    map: TIFFMapFileProc, unmap: TIFFUnmapFileProc
) -> *mut TIFF;
type TIFFCloseFn = unsafe extern "C" fn(tif: *mut TIFF);
type TIFFGetFieldFn = unsafe extern "C" fn(tif: *mut TIFF, tag: uint32, ...) -> c_int;
type TIFFIsTiledFn = unsafe extern "C" fn(tif: *mut TIFF) -> c_int;
type TIFFReadRGBATileFn = unsafe extern "C" fn(tif: *mut TIFF, x: uint32, y: uint32, raster: *mut uint32) -> c_int;
type TIFFReadRGBAStripFn = unsafe extern "C" fn(tif: *mut TIFF, row: uint32, raster: *mut uint32) -> c_int;
type TIFFReadRGBAImageOrientedFn = unsafe extern "C" fn(
    tif: *mut TIFF, width: uint32, height: uint32, raster: *mut uint32, 
    orientation: c_int, stop_on_error: c_int
) -> c_int;
type TIFFSetDirectoryFn = unsafe extern "C" fn(tif: *mut TIFF, dir: u16) -> c_int;

struct LibTiff {
    _lib: &'static Library,
    client_open: Symbol<'static, TIFFClientOpenFn>,
    close: Symbol<'static, TIFFCloseFn>,
    get_field: Symbol<'static, TIFFGetFieldFn>,
    is_tiled: Symbol<'static, TIFFIsTiledFn>,
    read_rgba_tile: Symbol<'static, TIFFReadRGBATileFn>,
    read_rgba_strip: Symbol<'static, TIFFReadRGBAStripFn>,
    read_rgba_image_oriented: Symbol<'static, TIFFReadRGBAImageOrientedFn>,
    set_directory: Symbol<'static, TIFFSetDirectoryFn>,
}

impl LibTiff {
    fn load() -> Result<Self, String> {
        let lib_names = ["libtiff.so.6", "libtiff.so.5", "libtiff.so"];
        for name in lib_names {
            if let Ok(lib) = unsafe { Library::new(name) } {
                let lib: &'static Library = Box::leak(Box::new(lib));
                unsafe {
                    let client_open = lib.get(b"TIFFClientOpen").map_err(|e| e.to_string())?;
                    let close = lib.get(b"TIFFClose").map_err(|e| e.to_string())?;
                    let get_field = lib.get(b"TIFFGetField").map_err(|e| e.to_string())?;
                    let is_tiled = lib.get(b"TIFFIsTiled").map_err(|e| e.to_string())?;
                    let read_rgba_tile = lib.get(b"TIFFReadRGBATile").map_err(|e| e.to_string())?;
                    let read_rgba_strip = lib.get(b"TIFFReadRGBAStrip").map_err(|e| e.to_string())?;
                    let read_rgba_image_oriented = lib.get(b"TIFFReadRGBAImageOriented").map_err(|e| e.to_string())?;
                    let set_directory = lib.get(b"TIFFSetDirectory").map_err(|e| e.to_string())?;
                    
                    return Ok(Self {
                        _lib: lib, client_open, close, get_field, 
                        is_tiled, read_rgba_tile, read_rgba_strip, read_rgba_image_oriented,
                        set_directory,
                    });
                }
            }
        }
        Err("Could not find libtiff.so".to_string())
    }
}

thread_local! {
    static LIB: Result<LibTiff, String> = LibTiff::load();
}

/// Context passed to libtiff callbacks
struct TiffMmapContext {
    mmap: Arc<Mmap>,
    offset: u64,
}

// --- libtiff Callbacks over memmap2::Mmap ---

unsafe extern "C" fn tiff_read_proc(handle: *mut c_void, buf: *mut c_void, size: tsize_t) -> tsize_t {
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
                to_read as usize
            );
        }
        ctx.offset += to_read;
    }
    to_read as tsize_t
}

unsafe extern "C" fn tiff_write_proc(_: *mut c_void, _: *mut c_void, _: tsize_t) -> tsize_t { 0 }

unsafe extern "C" fn tiff_seek_proc(handle: *mut c_void, off: toff_t, whence: c_int) -> toff_t {
    let ctx = unsafe { &mut *(handle as *mut TiffMmapContext) };
    match whence {
        0 => ctx.offset = off, // SEEK_SET
        1 => ctx.offset = (ctx.offset as i64 + off as i64) as u64, // SEEK_CUR
        2 => ctx.offset = (ctx.mmap.len() as i64 + off as i64) as u64, // SEEK_END
        _ => {}
    }
    ctx.offset
}

unsafe extern "C" fn tiff_close_proc(_: *mut c_void) -> c_int { 0 }

unsafe extern "C" fn tiff_size_proc(handle: *mut c_void) -> toff_t {
    let ctx = unsafe { &*(handle as *const TiffMmapContext) };
    ctx.mmap.len() as u64
}

unsafe extern "C" fn tiff_map_proc(handle: *mut c_void, base: *mut *mut c_void, size: *mut toff_t) -> c_int {
    let ctx = unsafe { &*(handle as *const TiffMmapContext) };
    unsafe {
        *base = ctx.mmap.as_ptr() as *mut c_void;
        *size = ctx.mmap.len() as u64;
    }
    1
}

unsafe extern "C" fn tiff_unmap_proc(_: *mut c_void, _: *mut c_void, _: toff_t) {}

/// RAII handle for a TIFF object, ensures the handle is closed and context is kept alive.
pub struct TiffHandle {
    ptr: *mut TIFF,
    _context: Box<TiffMmapContext>,
}

impl Drop for TiffHandle {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            LIB.with(|l| {
                if let Ok(lib) = l.as_ref() {
                    unsafe { (lib.close)(self.ptr); }
                }
            });
        }
    }
}

unsafe impl Send for TiffHandle {}
unsafe impl Sync for TiffHandle {}

fn create_tiff_handle(mmap: Arc<Mmap>, path: &Path) -> Result<TiffHandle, String> {
    LIB.with(|lib_res| {
        let lib = lib_res.as_ref().map_err(|e| e.clone())?;
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

            let tif_ptr = (lib.client_open)(
                c_path.as_ptr(), c_mode.as_ptr(),
                ctx.as_mut() as *mut TiffMmapContext as *mut c_void,
                tiff_read_proc, tiff_write_proc, tiff_seek_proc,
                tiff_close_proc, tiff_size_proc, tiff_map_proc, tiff_unmap_proc
            );

            if tif_ptr.is_null() { return Err("TIFFClientOpen failed".to_string()); }
            Ok(TiffHandle { ptr: tif_ptr, _context: ctx })
        }
    })
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

fn extract_embedded_thumbnail(lib: &LibTiff, tif: *mut TIFF, main_width: u32, target_size: u32) -> Option<(u32, u32, Vec<u8>)> {
    unsafe {
        let mut best_index = 0;
        let mut best_dim = 0;
        let mut best_pixels = None;

        // Iterate through IFDs to find the best-fitting thumbnail
        let mut dir_idx = 1;
        while (lib.set_directory)(tif, dir_idx) != 0 {
            let mut tw: uint32 = 0;
            let mut th: uint32 = 0;
            (lib.get_field)(tif, TIFFTAG_IMAGEWIDTH, &mut tw);
            (lib.get_field)(tif, TIFFTAG_IMAGELENGTH, &mut th);
            
            let dim = tw.max(th);
            // Safety: Protect against OOM from malicious files by limiting thumbnail size
            let total_pixels = tw as u64 * th as u64;
            if total_pixels > 64 * 1024 * 1024 { // 64MP Limit
                log::warn!("Linux LibTiff: Embedded thumbnail too large ({}x{}), skipping to avoid OOM", tw, th);
                dir_idx += 1;
                continue;
            }

            if tw > 0 && th > 0 && tw < main_width {
                if dim >= target_size && (best_pixels.is_none() || dim < best_dim) {
                    best_dim = dim;
                    best_index = dir_idx;
                    
                    let mut raster = vec![0u32; (tw * th) as usize];
                    if (lib.read_rgba_image_oriented)(tif, tw, th, raster.as_mut_ptr(), ORIENTATION_TOPLEFT, 0) != 0 {
                        // Performance: Fast bulk copy instead of extend_from_slice in loop
                        let mut pixels = vec![0u8; (tw * th * 4) as usize];
                        std::ptr::copy_nonoverlapping(raster.as_ptr() as *const u8, pixels.as_mut_ptr(), pixels.len());
                        best_pixels = Some((tw as u32, th as u32, pixels));
                    }
                } else if best_pixels.is_none() && dim > best_dim {
                    best_dim = dim;
                    best_index = dir_idx;
                    
                    let mut raster = vec![0u32; (tw * th) as usize];
                    if (lib.read_rgba_image_oriented)(tif, tw, th, raster.as_mut_ptr(), ORIENTATION_TOPLEFT, 0) != 0 {
                        let mut pixels = vec![0u8; (tw * th * 4) as usize];
                        std::ptr::copy_nonoverlapping(raster.as_ptr() as *const u8, pixels.as_mut_ptr(), pixels.len());
                        best_pixels = Some((tw as u32, th as u32, pixels));
                    }
                }
            }
            dir_idx += 1;
        }

        (lib.set_directory)(tif, 0);
        if let Some(res) = best_pixels {
            log::info!("Linux LibTiff: Using embedded IFD{} thumbnail ({}x{}) for target size {}", best_index, res.0, res.1, target_size);
            return Some(res);
        }
        None
    }
}

impl TiledImageSource for LibTiffTiledSource {
    fn width(&self) -> u32 { self.width }
    fn height(&self) -> u32 { self.height }

    fn extract_tile(&self, x: u32, y: u32, w: u32, h: u32) -> Vec<u8> {
        let mut result = vec![0u8; (w as usize) * (h as usize) * 4];
        let handle = match self.acquire_handle() {
            Ok(h) => h,
            Err(e) => {
                log::error!("[{}] Linux libtiff: Failed to acquire handle for tile: {}", self.path.display(), e);
                return result;
            }
        };

        LIB.with(|l| {
            let lib = match l.as_ref() { 
                Ok(l) => l, 
                Err(e) => {
                    log::error!("[{}] Linux libtiff: Failed to access library for tile: {}", self.path.display(), e);
                    return; 
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
                        if (lib.read_rgba_tile)(tif_ptr, curr_tx, curr_ty, tile_buf.as_mut_ptr()) != 0 {
                            for ty_in_p in 0..th {
                                let py = curr_ty + ty_in_p;
                                if py < y || py >= y + h { continue; }
                                for tx_in_p in 0..tw {
                                    let px = curr_tx + tx_in_p;
                                    if px < x || px >= x + w { continue; }
                                    let dest_x = px - x;
                                    let dest_y = py - y;
                                    let dest_idx = (dest_y as usize * w as usize + dest_x as usize) * 4;
                                    let src_idx = (th - 1 - ty_in_p) as usize * tw as usize + tx_in_p as usize;
                                    
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
        });

        self.release_handle(handle);
        result
    }

    fn generate_preview(&self, max_w: u32, max_h: u32) -> (u32, u32, Vec<u8>) {
        let max_dim = max_w.max(max_h);
        let handle = match self.acquire_handle() {
            Ok(h) => h,
            Err(e) => {
                log::error!("[{}] Linux libtiff: Failed to acquire handle for preview: {}", self.path.display(), e);
                return (0, 0, vec![]);
            }
        };

        let embedded = LIB.with(|l| {
            let lib = match l.as_ref() { Ok(l) => l, Err(_) => return None };
            extract_embedded_thumbnail(lib, handle.ptr, self.width, max_dim)
        });

        if let Some(res) = embedded {
            let thumb_max = res.0.max(res.1);
            if max_w.max(max_h) <= 512 || thumb_max >= 2048 || thumb_max >= max_w.max(max_h) {
                self.release_handle(handle);
                return res;
            }
        }

        let scale = (max_w as f64 / self.width as f64).min(max_h as f64 / self.height as f64).min(1.0);
        let pw = (self.width as f64 * scale) as u32;
        let ph = (self.height as f64 * scale) as u32;
        if pw == 0 || ph == 0 { 
            self.release_handle(handle);
            return (0, 0, vec![]); 
        }
        
        let mut result = vec![0u8; (pw * ph * 4) as usize];
        log::info!("Linux LibTiff: Generating stride-based fallback preview ({}x{})", pw, ph);

        LIB.with(|l| {
            let lib = match l.as_ref() { Ok(l) => l, Err(_) => return };
            let tif_ptr = handle.ptr;
            let tw = self.tile_width;
            let th = self.tile_height;
            let mut tile_buf = vec![0u32; (tw * th) as usize];
            let mut last_tile_idx = u32::MAX;

            // Performance: Fixed-point arithmetic for stride calculations
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
                            if (lib.read_rgba_tile)(tif_ptr, tile_col * tw, tile_row * th, tile_buf.as_mut_ptr()) != 0 {
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
                            result[dst_idx..dst_idx+4].copy_from_slice(&pixel);
                        }
                    }
                }
            }
        });

        self.release_handle(handle);
        (pw, ph, result)
    }

    fn full_pixels(&self) -> Option<Arc<Vec<u8>>> { None }
}

// --- Scanline Implementation (Mock Tiles from Strips) ---

pub struct LibTiffScanlineSource {
    path: PathBuf,
    mmap: Arc<Mmap>,
    width: u32,
    height: u32,
    rows_per_strip: u32,
    pool: Mutex<Vec<TiffHandle>>,
    /// Cache of decoded strips: key=strip_idx, value=RGBA pixels for that strip.
    /// Each strip is width * rows_per_strip * 4 bytes.
    strip_cache: Mutex<std::collections::HashMap<u32, Arc<Vec<u8>>>>,
    /// LRU order for cache eviction (oldest first).
    cache_order: Mutex<Vec<u32>>,
    /// Maximum number of strips to keep in cache.
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

    /// Get a decoded strip from cache, or decode it and insert into cache.
    fn get_or_decode_strip(&self, strip_idx: u32, handle: &TiffHandle) -> Option<Arc<Vec<u8>>> {
        // Phase 1: Check cache
        {
            let mut cache = self.strip_cache.lock().unwrap();
            if let Some(data) = cache.get(&strip_idx) {
                // Move to back of LRU
                let mut order = self.cache_order.lock().unwrap();
                if let Some(pos) = order.iter().position(|&k| k == strip_idx) {
                    order.remove(pos);
                }
                order.push(strip_idx);
                return Some(Arc::clone(data));
            }
        }

        // Phase 2: Decode strip (no lock held)
        let rps = self.rows_per_strip;
        let mut strip_buf = vec![0u32; (self.width as usize) * (rps as usize)];
        
        let decoded = LIB.with(|l| {
            let lib = match l.as_ref() { Ok(l) => l, Err(_) => return false };
            unsafe {
                (lib.read_rgba_strip)(handle.ptr, strip_idx * rps, strip_buf.as_mut_ptr()) != 0
            }
        });
        
        if !decoded {
            return None;
        }
        
        // Convert ABGR u32 to RGBA u8 and flip rows (libtiff returns bottom-up within strip)
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

        // Phase 3: Insert into cache with LRU eviction
        {
            let mut cache = self.strip_cache.lock().unwrap();
            let mut order = self.cache_order.lock().unwrap();
            
            // Evict oldest strips if over capacity
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
    fn width(&self) -> u32 { self.width }
    fn height(&self) -> u32 { self.height }

    fn extract_tile(&self, x: u32, y: u32, w: u32, h: u32) -> Vec<u8> {
        let mut result = vec![0u8; (w as usize) * (h as usize) * 4];
        let handle = match self.acquire_handle() {
            Ok(h) => h,
            Err(e) => {
                log::error!("[{}] Linux libtiff: Failed to acquire handle for scanline: {}", self.path.display(), e);
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

            // Calculate intersection between tile and this strip
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
                let src_offset = (row_in_strip * self.width as usize + intersect_x_start as usize) * 4;
                let dst_y = (py - y) as usize;
                let dst_offset = (dst_y * w as usize + (intersect_x_start - x) as usize) * 4;

                if src_offset + copy_bytes <= strip_data.len() && dst_offset + copy_bytes <= result.len() {
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
                log::error!("[{}] Linux libtiff: Failed to acquire handle for scanline preview: {}", self.path.display(), e);
                return (0, 0, vec![]);
            }
        };

        let embedded = LIB.with(|l| {
            let lib = match l.as_ref() { Ok(l) => l, Err(_) => return None };
            extract_embedded_thumbnail(lib, handle.ptr, self.width, max_dim)
        });

        if let Some(res) = embedded {
            let thumb_max = res.0.max(res.1);
            if max_w.max(max_h) <= 512 || thumb_max >= 2048 || thumb_max >= max_w.max(max_h) {
                self.release_handle(handle);
                return res;
            }
        }

        let scale = (max_w as f64 / self.width as f64).min(max_h as f64 / self.height as f64).min(1.0);
        let pw = (self.width as f64 * scale) as u32;
        let ph = (self.height as f64 * scale) as u32;
        if pw == 0 || ph == 0 { 
            self.release_handle(handle);
            return (0, 0, vec![]); 
        }
        
        let mut result = vec![0u8; (pw * ph * 4) as usize];
        log::info!("Linux LibTiff: Generating stride-based fallback preview from strips ({}x{})", pw, ph);

        LIB.with(|l| {
            let lib = match l.as_ref() { Ok(l) => l, Err(_) => return };
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
                        if (lib.read_rgba_strip)(tif_ptr, strip_idx * rps, strip_buf.as_mut_ptr()) != 0 {
                            last_strip_idx = strip_idx;
                        } else {
                            continue;
                        }
                    }

                    for tx in 0..pw {
                        let x = ((tx as u64 * stride_x_fp) >> 16) as u32;
                        let src_idx = (rps - 1 - y_in_strip) as usize * self.width as usize + x as usize;
                        if src_idx < strip_buf.len() {
                            let pixel = strip_buf[src_idx].to_ne_bytes();
                            let dst_idx = dst_y_offset + (tx as usize) * 4;
                            result[dst_idx..dst_idx+4].copy_from_slice(&pixel);
                        }
                    }
                }
            }
        });

        self.release_handle(handle);
        (pw, ph, result)
    }

    fn full_pixels(&self) -> Option<Arc<Vec<u8>>> { None }
}

pub fn load_via_libtiff(path: &Path) -> Result<ImageData, String> {
    LIB.with(|lib_res| {
        let lib = lib_res.as_ref().map_err(|e| e.clone())?;

        let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
        let mmap = Arc::new(unsafe { Mmap::map(&file).map_err(|e| e.to_string())? });

        let mut ctx = Box::new(TiffMmapContext { mmap: mmap.clone(), offset: 0 });

        unsafe {
            let c_path = match CString::new(path.to_str().unwrap_or("image.tif")) {
                Ok(c) => c,
                Err(_) => return Err("Invalid path string for C conversion".to_string()),
            };
            let c_mode = match CString::new("r") {
                Ok(c) => c,
                Err(_) => return Err("Invalid mode string for C conversion".to_string()),
            };

            let tif_ptr = (lib.client_open)(
                c_path.as_ptr(), c_mode.as_ptr(),
                ctx.as_mut() as *mut TiffMmapContext as *mut c_void,
                tiff_read_proc, tiff_write_proc, tiff_seek_proc,
                tiff_close_proc, tiff_size_proc, tiff_map_proc, tiff_unmap_proc
            );

            if tif_ptr.is_null() { return Err("TIFFClientOpen failed".to_string()); }
            
            let handle = TiffHandle { ptr: tif_ptr, _context: ctx };

            let mut width: uint32 = 0;
            let mut height: uint32 = 0;
            (lib.get_field)(handle.ptr, TIFFTAG_IMAGEWIDTH, &mut width);
            (lib.get_field)(handle.ptr, TIFFTAG_IMAGELENGTH, &mut height);

            if width == 0 || height == 0 {
                return Err("TIFF has zero width or height".to_string());
            }

            let pixel_count = width as u64 * height as u64;
            let limit = crate::tile_cache::get_max_texture_side();
            let tiled_threshold = crate::tile_cache::TILED_THRESHOLD.load(std::sync::atomic::Ordering::Relaxed);
            let is_large = pixel_count >= tiled_threshold || width > limit || height > limit;

            if is_large {
                if (lib.is_tiled)(handle.ptr) != 0 {
                    let mut tile_width: uint32 = 0;
                    let mut tile_height: uint32 = 0;
                    (lib.get_field)(handle.ptr, TIFFTAG_TILEWIDTH, &mut tile_width);
                    (lib.get_field)(handle.ptr, TIFFTAG_TILELENGTH, &mut tile_height);

                    if tile_width == 0 || tile_height == 0 {
                        return Err("TIFF is tiled but tile dimensions are zero".to_string());
                    }

                    return Ok(ImageData::Tiled(Arc::new(LibTiffTiledSource {
                        path: path.to_path_buf(), mmap: mmap.clone(), 
                        width, height, tile_width, tile_height,
                        pool: Mutex::new(vec![handle]),
                    })));
                } else {
                    let mut rps: uint32 = 0;
                    if (lib.get_field)(handle.ptr, TIFFTAG_ROWSPERSTRIP, &mut rps) == 0 || rps == 0 {
                        rps = height; 
                    }
                    if rps == 0 {
                        return Err("TIFF scanline height is zero".to_string());
                    }
                    
                    // Calculate cache capacity: ~256MB budget
                    let strip_bytes = width as usize * rps as usize * 4;
                    let max_cached = if strip_bytes > 0 {
                        (256 * 1024 * 1024 / strip_bytes).max(16)
                    } else {
                        64
                    };
                    log::info!("Linux LibTiff: Stripped TIFF {}x{}, rps={}, cache capacity={} strips ({} MB budget)",
                        width, height, rps, max_cached, max_cached * strip_bytes / (1024 * 1024));
                    
                    return Ok(ImageData::Tiled(Arc::new(LibTiffScanlineSource {
                        path: path.to_path_buf(), mmap: mmap.clone(),
                        width, height, rows_per_strip: rps,
                        pool: Mutex::new(vec![handle]),
                        strip_cache: Mutex::new(std::collections::HashMap::new()),
                        cache_order: Mutex::new(Vec::new()),
                        max_cached_strips: max_cached,
                    })));
                }
            }

            // Fallback for regular small images
            let total_pixels = (width as usize) * (height as usize);
            if total_pixels > 256 * 1024 * 1024 { // 256MP limit for static decode
                return Err("Static TIFF TOO LARGE for single pass decode".to_string());
            }
            let mut raster: Vec<uint32> = vec![0; total_pixels];
            if (lib.read_rgba_image_oriented)(handle.ptr, width, height, raster.as_mut_ptr(), 1, 0) == 0 {
                return Err("TIFFReadRGBAImageOriented failed".to_string());
            }

            // Performance: Fast bulk copy
            let mut pixels = vec![0u8; total_pixels * 4];
            std::ptr::copy_nonoverlapping(raster.as_ptr() as *const u8, pixels.as_mut_ptr(), pixels.len());
            Ok(ImageData::Static(DecodedImage { width, height, pixels }))
        }
    })
}
