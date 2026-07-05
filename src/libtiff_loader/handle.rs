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
use memmap2::Mmap;
use parking_lot::Mutex;
use std::ffi::CString;
use std::os::raw::c_void;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::constants::MAX_TIFF_HANDLE_POOL_SIZE;

use super::mmap::{
    TiffMmapContext, tiff_close_proc, tiff_map_proc, tiff_read_proc, tiff_seek_proc,
    tiff_size_proc, tiff_unmap_proc, tiff_write_proc,
};

/// Build a `CString` for the libtiff `TIFFClientOpen` name parameter from a `Path`.
///
/// The name is metadata (error messages, `TIFFFileName()`) — actual file I/O goes through
/// Rust `memmap2` callbacks that use wide-char APIs on Windows.  `to_string_lossy()` gives
/// the real filename even on Unix paths with non-UTF-8 bytes, without panicking.
pub(crate) fn path_to_tiff_name(path: &Path) -> CString {
    CString::new(path.to_string_lossy().as_bytes())
        .unwrap_or_else(|_| CString::new("image.tif").unwrap())
}

/// RAII handle for a TIFF object — delegates close to [`lib::TiffGuard`] and keeps
/// the memory-map context alive until the handle is dropped.
///
/// Field order is load-bearing: `guard` (→ `TIFFClose`) must drop **before** `_context`
/// (→ mmap free), matching the C lifecycle contract of `TIFFClientOpen`.
pub struct TiffHandle {
    pub(crate) guard: lib::TiffGuard,
    pub(crate) _context: Box<TiffMmapContext>,
}

impl TiffHandle {
    /// Raw pointer for FFI calls. Prefer the typed helpers on [`lib::TiffGuard`] when possible.
    #[inline]
    pub(crate) fn as_ptr(&self) -> *mut lib::TIFF {
        self.guard.as_ptr()
    }
}

// A LibTIFF `TIFF` handle is not documented as safe for concurrent use from multiple threads.
// `LibTiffTiledSource` / `LibTiffScanlineSource` store handles in a Mutex-backed pool so only one
// thread uses each handle at a time.
unsafe impl Send for TiffHandle {}

/// Mutex-backed pool of [`TiffHandle`] with a hard cap on live handles.
pub(crate) struct TiffHandlePool {
    pool: Mutex<Vec<TiffHandle>>,
    live_handles: AtomicUsize,
}

impl TiffHandlePool {
    pub(crate) fn new(initial: TiffHandle) -> Self {
        Self {
            pool: Mutex::new(vec![initial]),
            live_handles: AtomicUsize::new(1),
        }
    }

    pub(crate) fn acquire(&self, mmap: Arc<Mmap>, path: &Path) -> Result<TiffHandle, String> {
        if let Some(handle) = self.pool.lock().pop() {
            return Ok(handle);
        }
        loop {
            let current = self.live_handles.load(Ordering::Acquire);
            if current >= MAX_TIFF_HANDLE_POOL_SIZE {
                return Err(format!(
                    "TIFF handle pool capacity ({MAX_TIFF_HANDLE_POOL_SIZE}) exceeded"
                ));
            }
            if self
                .live_handles
                .compare_exchange(current, current + 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                break;
            }
        }
        match create_tiff_handle(mmap, path) {
            Ok(handle) => Ok(handle),
            Err(e) => {
                self.live_handles.fetch_sub(1, Ordering::AcqRel);
                Err(e)
            }
        }
    }

    pub(crate) fn release(&self, handle: TiffHandle) {
        let mut pool = self.pool.lock();
        if pool.len() < MAX_TIFF_HANDLE_POOL_SIZE {
            pool.push(handle);
        } else {
            drop(handle);
            self.live_handles.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

pub(crate) fn create_tiff_handle(mmap: Arc<Mmap>, path: &Path) -> Result<TiffHandle, String> {
    let mut ctx = Box::new(TiffMmapContext { mmap, offset: 0 });

    unsafe {
        // SAFETY: `ctx` outlives the TIFF handle; I/O callbacks use the mmap-backed context.
        let c_path = path_to_tiff_name(path);
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
            guard: lib::TiffGuard::from_ptr(tif_ptr),
            _context: ctx,
        })
    }
}

// --- Tiled Implementation (Physical Tiles) ---
