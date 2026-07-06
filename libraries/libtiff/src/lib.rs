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
use std::fmt;

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

// ── RAII guards ────────────────────────────────────────────────────────

/// Owns a [`TIFF`] handle and calls [`TIFFClose`] on drop.
///
/// A `TIFF` handle is **not** `Send`: LibTIFF is not thread-safe, and the caller must
/// either guard access with a `Mutex` or ensure exclusive ownership on a single thread.
#[must_use = "TiffGuard will close the TIFF handle on drop"]
pub struct TiffGuard {
    ptr: *mut TIFF,
}

// SAFETY: The caller is responsible for ensuring the TIFF handle is only used from one
// thread at a time (the common `Mutex<TiffGuard>` pattern). We intentionally do not
// auto-derive `Send` — the caller must explicitly opt in with `unsafe impl Send`.
unsafe impl Send for TiffGuard {}

impl TiffGuard {
    /// Wrap an already-open TIFF handle. The caller must ensure `ptr` is a valid,
    /// non-null handle from [`TIFFOpen`] or [`TIFFClientOpen`].
    ///
    /// # Safety
    ///
    /// `ptr` must be a valid `*mut TIFF` that has not been passed to another guard.
    #[inline]
    pub unsafe fn from_ptr(ptr: *mut TIFF) -> Self {
        debug_assert!(!ptr.is_null(), "TiffGuard constructed with null TIFF*");
        Self { ptr }
    }

    /// Raw pointer to the underlying `TIFF` handle (for FFI calls).
    #[inline]
    pub fn as_ptr(&self) -> *mut TIFF {
        self.ptr
    }

    /// Consume the guard and return the raw pointer without closing.
    ///
    /// The caller becomes responsible for calling [`TIFFClose`].
    #[inline]
    pub fn into_raw(mut self) -> *mut TIFF {
        let ptr = self.ptr;
        self.ptr = std::ptr::null_mut();
        ptr
    }

    // ── Typed `TIFFGetField` wrappers ──────────────────────────────────

    /// Read a `u16` tag (e.g. `BITSPERSAMPLE`, `ORIENTATION`, `PHOTOMETRIC`).
    ///
    /// Returns `None` when the tag is not present or the field read fails.
    #[inline]
    pub unsafe fn get_field_u16(&self, tag: u32) -> Option<u16> {
        let mut val: u16 = 0;
        if unsafe { TIFFGetField(self.ptr, tag, &mut val) } != 0 {
            Some(val)
        } else {
            None
        }
    }

    /// Read a `u32` tag (e.g. `IMAGEWIDTH`, `IMAGELENGTH`, `TILEWIDTH`, `ROWSPERSTRIP`).
    #[inline]
    pub unsafe fn get_field_u32(&self, tag: u32) -> Option<u32> {
        let mut val: u32 = 0;
        if unsafe { TIFFGetField(self.ptr, tag, &mut val) } != 0 {
            Some(val)
        } else {
            None
        }
    }

    /// Read a `f64` tag (e.g. `SMINSAMPLEVALUE`, `SMAXSAMPLEVALUE`).
    #[inline]
    pub unsafe fn get_field_f64(&self, tag: u32) -> Option<f64> {
        let mut val: f64 = 0.0;
        if unsafe { TIFFGetField(self.ptr, tag, &mut val) } != 0 {
            Some(val)
        } else {
            None
        }
    }

    /// Read the colormap tag (three `*mut u16` pointers: R, G, B).
    #[inline]
    pub unsafe fn get_field_colormap(
        &self,
        tag: u32,
    ) -> Option<(*mut u16, *mut u16, *mut u16)> {
        let (mut r, mut g, mut b) = (
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        if unsafe { TIFFGetField(self.ptr, tag, &mut r, &mut g, &mut b) } != 0 {
            Some((r, g, b))
        } else {
            None
        }
    }
}

impl Drop for TiffGuard {
    #[inline]
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe {
                TIFFClose(self.ptr);
            }
        }
    }
}

impl fmt::Debug for TiffGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TiffGuard")
            .field("ptr", &self.ptr)
            .finish()
    }
}
