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
    let rem = ctx.mmap.len() as u64 - ctx.offset;
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

// --- Tiled Implementation (Physical Tiles) ---

pub struct LibTiffTiledSource {
    _path: PathBuf,
    width: u32,
    height: u32,
    tile_width: u32,
    tile_height: u32,
    handle: Mutex<TiffHandle>,
}

unsafe impl Send for LibTiffTiledSource {}
unsafe impl Sync for LibTiffTiledSource {}

fn extract_embedded_thumbnail(lib: &LibTiff, tif: *mut TIFF, main_width: u32) -> Option<(u32, u32, Vec<u8>)> {
    unsafe {
        // Try to see if there's a second directory (IFD 1) which often contains a thumbnail
        if (lib.set_directory)(tif, 1) != 0 {
            let mut tw: uint32 = 0;
            let mut th: uint32 = 0;
            (lib.get_field)(tif, TIFFTAG_IMAGEWIDTH, &mut tw);
            (lib.get_field)(tif, TIFFTAG_IMAGELENGTH, &mut th);
            
            let res = if tw > 0 && th > 0 && (tw as u32) < main_width / 2 {
                let mut raster = vec![0u32; (tw * th) as usize];
                // Use orientation 1 (Top-Left) to avoid manual flipping
                if (lib.read_rgba_image_oriented)(tif, tw, th, raster.as_mut_ptr(), ORIENTATION_TOPLEFT, 0) != 0 {
                    let mut pixels = Vec::with_capacity((tw * th * 4) as usize);
                    for p in raster { pixels.extend_from_slice(&p.to_ne_bytes()); }
                    log::info!("Linux LibTiff: Using embedded IFD1 thumbnail ({}x{})", tw, th);
                    Some((tw as u32, th as u32, pixels))
                } else {
                    None
                }
            } else {
                None
            };
            
            // Restore back to main directory Regardless of success
            (lib.set_directory)(tif, 0);
            return res;
        }
        None
    }
}

impl TiledImageSource for LibTiffTiledSource {
    fn width(&self) -> u32 { self.width }
    fn height(&self) -> u32 { self.height }

    fn extract_tile(&self, x: u32, y: u32, w: u32, h: u32) -> Vec<u8> {
        let mut result = vec![0u8; (w as usize) * (h as usize) * 4];
        LIB.with(|l| {
            let lib = match l.as_ref() { 
                Ok(l) => l, 
                Err(e) => {
                    log::error!("[{}] Linux libtiff: Failed to access library for tile: {}", self._path.display(), e);
                    return; 
                }
            };
            if let Ok(handle_lock) = self.handle.lock() {
                let tif_ptr = handle_lock.ptr;
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
            }
        });
        result
    }

    fn generate_preview(&self, max_w: u32, max_h: u32) -> (u32, u32, Vec<u8>) {
        let embedded = LIB.with(|l| {
            let lib = match l.as_ref() { Ok(l) => l, Err(_) => return None };
            let handle_lock = match self.handle.lock() { Ok(lock) => lock, Err(_) => return None };
            extract_embedded_thumbnail(lib, handle_lock.ptr, self.width)
        });

        embedded.unwrap_or_else(|| {
            // High-speed fallback for preview: subsample rows from main IFD
            let scale = (max_w as f64 / self.width as f64).min(max_h as f64 / self.height as f64).min(1.0);
            let pw = (self.width as f64 * scale) as u32;
            let ph = (self.height as f64 * scale) as u32;
            log::info!("Linux LibTiff: Generating high-speed fallback preview ({}x{})", pw, ph);
            (pw, ph, self.extract_tile(0, 0, pw, ph))
        })
    }

    fn full_pixels(&self) -> Option<Arc<Vec<u8>>> { None }
}

// --- Scanline Implementation (Mock Tiles from Strips) ---

pub struct LibTiffScanlineSource {
    path: PathBuf,
    width: u32,
    height: u32,
    rows_per_strip: u32,
    handle: Mutex<TiffHandle>,
}

unsafe impl Send for LibTiffScanlineSource {}
unsafe impl Sync for LibTiffScanlineSource {}

impl TiledImageSource for LibTiffScanlineSource {
    fn width(&self) -> u32 { self.width }
    fn height(&self) -> u32 { self.height }

    fn extract_tile(&self, x: u32, y: u32, w: u32, h: u32) -> Vec<u8> {
        let mut result = vec![0u8; (w as usize) * (h as usize) * 4];
        LIB.with(|l| {
            let lib = match l.as_ref() { 
                Ok(l) => l, 
                Err(e) => {
                    log::error!("[{}] Linux libtiff: Failed to access library for scanline: {}", self.path.display(), e);
                    return; 
                }
            };
            if let Ok(handle_lock) = self.handle.lock() {
                let tif_ptr = handle_lock.ptr;
                let rps = self.rows_per_strip;
                let mut strip_buf = vec![0u32; (self.width as usize) * (rps as usize)];
                let mut last_strip_idx = u32::MAX;

                for py in y..(y + h) {
                    if py >= self.height { break; }
                    let strip_idx = py / rps;
                    
                    unsafe {
                        if strip_idx != last_strip_idx {
                            // Read the strip containing row 'py'
                            if (lib.read_rgba_strip)(tif_ptr, strip_idx * rps, strip_buf.as_mut_ptr()) == 0 {
                                continue;
                            }
                            last_strip_idx = strip_idx;
                        }

                        let row_in_strip = py % rps;
                        // Note: TIFFReadRGBAStrip raster is orientation-aware but usually bottom-up within the strip for RGBA
                        // Actually, libtiff's RGBA interface follows specific rules. For strips, row 0 is BOTTOM of strip.
                        let src_row = (rps - 1 - row_in_strip) as usize;
                        let src_offset = src_row * self.width as usize;

                        for px in x..(x + w) {
                            if px >= self.width { break; }
                            let dest_x = px - x;
                            let dest_y = py - y;
                            let dest_idx = (dest_y as usize * w as usize + dest_x as usize) * 4;
                            let src_idx = src_offset + px as usize;
                            if src_idx < strip_buf.len() && dest_idx + 4 <= result.len() {
                                let pixel = strip_buf[src_idx].to_ne_bytes();
                                result[dest_idx..dest_idx + 4].copy_from_slice(&pixel);
                            }
                        }
                    }
                }
            }
        });
        result
    }

    fn generate_preview(&self, max_w: u32, max_h: u32) -> (u32, u32, Vec<u8>) {
        let embedded = LIB.with(|l| {
            let lib = match l.as_ref() { Ok(l) => l, Err(_) => return None };
            let handle_lock = match self.handle.lock() { Ok(lock) => lock, Err(_) => return None };
            extract_embedded_thumbnail(lib, handle_lock.ptr, self.width)
        });

        embedded.unwrap_or_else(|| {
            // High-speed fallback for preview: subsample rows from main IFD
            let scale = (max_w as f64 / self.width as f64).min(max_h as f64 / self.height as f64).min(1.0);
            let pw = (self.width as f64 * scale) as u32;
            let ph = (self.height as f64 * scale) as u32;
            log::info!("Linux LibTiff: Generating high-speed fallback preview ({}x{})", pw, ph);
            (pw, ph, self.extract_tile(0, 0, pw, ph))
        })
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

            let pixel_count = width as u64 * height as u64;
            let limit = crate::tile_cache::get_max_texture_side();
            let is_large = pixel_count >= crate::tile_cache::TILED_THRESHOLD || width > limit || height > limit;

            if is_large {
                if (lib.is_tiled)(handle.ptr) != 0 {
                    let mut tile_width: uint32 = 0;
                    let mut tile_height: uint32 = 0;
                    (lib.get_field)(handle.ptr, TIFFTAG_TILEWIDTH, &mut tile_width);
                    (lib.get_field)(handle.ptr, TIFFTAG_TILELENGTH, &mut tile_height);

                    return Ok(ImageData::Tiled(Arc::new(LibTiffTiledSource {
                        _path: path.to_path_buf(), width, height, tile_width, tile_height,
                        handle: Mutex::new(handle),
                    })));
                } else {
                    let mut rps: uint32 = 0;
                    if (lib.get_field)(handle.ptr, TIFFTAG_ROWSPERSTRIP, &mut rps) == 0 {
                        rps = height; // Fallback to whole image if tag missing
                    }
                    
                    return Ok(ImageData::Tiled(Arc::new(LibTiffScanlineSource {
                        path: path.to_path_buf(),
                        width, height, rows_per_strip: rps,
                        handle: Mutex::new(handle),
                    })));
                }
            }

            // Fallback for regular small images
            let total_pixels = (width as usize) * (height as usize);
            let mut raster: Vec<uint32> = vec![0; total_pixels];
            if (lib.read_rgba_image_oriented)(handle.ptr, width, height, raster.as_mut_ptr(), 1, 0) == 0 {
                return Err("TIFFReadRGBAImageOriented failed".to_string());
            }

            let mut pixels = Vec::with_capacity(total_pixels * 4);
            for p in raster { pixels.extend_from_slice(&p.to_ne_bytes()); }
            Ok(ImageData::Static(DecodedImage { width, height, pixels }))
        }
    })
}
