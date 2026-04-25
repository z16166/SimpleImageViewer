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

use std::ffi::{c_char, c_int, c_void};

pub type TIFF = c_void;
#[allow(non_camel_case_types)]
pub type uint32 = u32;
#[allow(non_camel_case_types)]
pub type toff_t = u64;
#[allow(non_camel_case_types)]
pub type tsize_t = i64;

pub const TIFFTAG_IMAGEWIDTH: uint32 = 256;
pub const TIFFTAG_IMAGELENGTH: uint32 = 257;
pub const TIFFTAG_BITSPERSAMPLE: uint32 = 258;
pub const TIFFTAG_COMPRESSION: uint32 = 259;
pub const TIFFTAG_PHOTOMETRIC: uint32 = 262;
pub const TIFFTAG_ORIENTATION: uint32 = 274;
pub const TIFFTAG_SAMPLESPERPIXEL: uint32 = 277;
pub const TIFFTAG_ROWSPERSTRIP: uint32 = 278;
pub const TIFFTAG_PLANARCONFIG: uint32 = 284;
pub const TIFFTAG_COLORMAP: uint32 = 320;
pub const TIFFTAG_TILEWIDTH: uint32 = 322;
pub const TIFFTAG_TILELENGTH: uint32 = 323;
pub const TIFFTAG_SAMPLEFORMAT: uint32 = 339;
pub const TIFFTAG_SMINSAMPLEVALUE: uint32 = 340;
pub const TIFFTAG_SMAXSAMPLEVALUE: uint32 = 341;
pub const TIFFTAG_EXTRASAMPLES: uint32 = 338;

pub const SAMPLEFORMAT_UINT: u16 = 1;
pub const SAMPLEFORMAT_INT: u16 = 2;
pub const SAMPLEFORMAT_IEEEFP: u16 = 3;
pub const ORIENTATION_TOPLEFT: c_int = 1;

pub type TIFFReadWriteProc =
    unsafe extern "C" fn(handle: *mut c_void, buf: *mut c_void, size: tsize_t) -> tsize_t;
pub type TIFFSeekProc =
    unsafe extern "C" fn(handle: *mut c_void, off: toff_t, whence: c_int) -> toff_t;
pub type TIFFCloseProc = unsafe extern "C" fn(handle: *mut c_void) -> c_int;
pub type TIFFSizeProc = unsafe extern "C" fn(handle: *mut c_void) -> toff_t;
pub type TIFFMapFileProc =
    unsafe extern "C" fn(handle: *mut c_void, base: *mut *mut c_void, size: *mut toff_t) -> c_int;
pub type TIFFUnmapFileProc =
    unsafe extern "C" fn(handle: *mut c_void, base: *mut c_void, size: toff_t);
pub type TIFFErrorHandler =
    unsafe extern "C" fn(module: *const c_char, fmt: *const c_char, ap: *mut c_void);

unsafe extern "C" {
    pub fn TIFFSetErrorHandler(handler: Option<TIFFErrorHandler>) -> Option<TIFFErrorHandler>;
    pub fn TIFFSetWarningHandler(handler: Option<TIFFErrorHandler>) -> Option<TIFFErrorHandler>;
    pub fn TIFFOpen(name: *const c_char, mode: *const c_char) -> *mut TIFF;
    pub fn TIFFClientOpen(
        name: *const c_char,
        mode: *const c_char,
        handle: *mut c_void,
        read: TIFFReadWriteProc,
        write: TIFFReadWriteProc,
        seek: TIFFSeekProc,
        close: TIFFCloseProc,
        size: TIFFSizeProc,
        map: TIFFMapFileProc,
        unmap: TIFFUnmapFileProc,
    ) -> *mut TIFF;
    pub fn TIFFClose(tif: *mut TIFF);
    pub fn TIFFGetField(tif: *mut TIFF, tag: uint32, ...) -> c_int;
    pub fn TIFFIsTiled(tif: *mut TIFF) -> c_int;
    pub fn TIFFReadRGBATile(tif: *mut TIFF, x: uint32, y: uint32, raster: *mut uint32) -> c_int;
    pub fn TIFFReadRGBAStrip(tif: *mut TIFF, row: uint32, raster: *mut uint32) -> c_int;
    pub fn TIFFReadRGBAImageOriented(
        tif: *mut TIFF,
        width: uint32,
        height: uint32,
        raster: *mut uint32,
        orientation: c_int,
        stop_on_error: c_int,
    ) -> c_int;
    pub fn TIFFSetDirectory(tif: *mut TIFF, dir: u16) -> c_int;
    pub fn TIFFReadScanline(tif: *mut TIFF, buf: *mut c_void, row: uint32, sample: u16) -> c_int;
    pub fn TIFFScanlineSize(tif: *mut TIFF) -> tsize_t;
    pub fn TIFFDefaultStripSize(tif: *mut TIFF, request: uint32) -> uint32;
    pub fn TIFFIsByteSwapped(tif: *mut TIFF) -> c_int;
}
