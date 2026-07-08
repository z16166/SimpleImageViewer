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
use std::ffi::{c_int, c_void};
use std::sync::Arc;

/// Context passed to libtiff callbacks
pub(crate) struct TiffMmapContext {
    /// Keeps the mmap mapping alive for `data_ptr`.
    #[allow(dead_code)]
    pub(crate) mmap: Arc<Mmap>,
    pub(crate) data_ptr: *const u8,
    pub(crate) mmap_len: u64,
    pub(crate) offset: u64,
}

impl TiffMmapContext {
    pub(crate) fn new(mmap: Arc<Mmap>) -> Self {
        Self {
            data_ptr: mmap.as_ptr(),
            mmap_len: mmap.len() as u64,
            offset: 0,
            mmap,
        }
    }
}

// --- libtiff Callbacks over memmap2::Mmap ---
//
// Read-only policy: every `TIFFClientOpen` uses mode `"r"`, and `tiff_write_proc` always
// returns 0 (libtiff requires a non-null writeproc). Explicit writes fail; `tiff_map_proc`
// still exposes the mmap for zero-copy reads.
//
// Use raw pointer field access in every callback so libtiff never holds overlapping
// `&mut TiffMmapContext` and `&TiffMmapContext` references at once (Rust noalias).

pub(crate) unsafe extern "C" fn tiff_read_proc(
    handle: *mut c_void,
    buf: *mut c_void,
    size: lib::tsize_t,
) -> lib::tsize_t {
    let ctx = handle.cast::<TiffMmapContext>();
    let mmap_len = unsafe { (*ctx).mmap_len };

    if unsafe { (*ctx).offset } >= mmap_len {
        return 0;
    }

    let rem = mmap_len - unsafe { (*ctx).offset };
    let to_read = (size as u64).min(rem);

    if to_read > 0 {
        unsafe {
            std::ptr::copy_nonoverlapping(
                (*ctx).data_ptr.add((*ctx).offset as usize),
                buf.cast::<u8>(),
                to_read as usize,
            );
            (*ctx).offset += to_read;
        }
    }
    to_read as lib::tsize_t
}

/// libtiff requires a non-null writeproc even in `"r"` mode; returning 0 rejects all writes.
#[cold]
pub(crate) unsafe extern "C" fn tiff_write_proc(
    _: *mut c_void,
    _: *mut c_void,
    _: lib::tsize_t,
) -> lib::tsize_t {
    0
}

pub(crate) unsafe extern "C" fn tiff_seek_proc(
    handle: *mut c_void,
    off: lib::toff_t,
    whence: c_int,
) -> lib::toff_t {
    let ctx = handle.cast::<TiffMmapContext>();
    let mmap_len = unsafe { (*ctx).mmap_len };
    let len_i64 = mmap_len as i64;
    match whence {
        0 => unsafe {
            (*ctx).offset = off.min(mmap_len);
        }, // SEEK_SET
        1 => unsafe {
            // SEEK_CUR: interpret `off` as signed per libtiff conventions (see `toff_t as i64`).
            let next = ((*ctx).offset as i64)
                .saturating_add(off as i64)
                .clamp(0, len_i64);
            (*ctx).offset = next as u64;
        },
        2 => unsafe {
            // SEEK_END
            let next = len_i64.saturating_add(off as i64).clamp(0, len_i64);
            (*ctx).offset = next as u64;
        },
        _ => {}
    }
    unsafe { (*ctx).offset }
}

pub(crate) unsafe extern "C" fn tiff_close_proc(_: *mut c_void) -> c_int {
    0
}

pub(crate) unsafe extern "C" fn tiff_size_proc(handle: *mut c_void) -> lib::toff_t {
    let ctx = handle.cast::<TiffMmapContext>();
    unsafe { (*ctx).mmap_len }
}

pub(crate) unsafe extern "C" fn tiff_map_proc(
    handle: *mut c_void,
    base: *mut *mut c_void,
    size: *mut lib::toff_t,
) -> c_int {
    let ctx = handle.cast::<TiffMmapContext>();
    unsafe {
        *base = (*ctx).data_ptr.cast::<c_void>().cast_mut();
        *size = (*ctx).mmap_len;
    }
    1
}

pub(crate) unsafe extern "C" fn tiff_unmap_proc(_: *mut c_void, _: *mut c_void, _: lib::toff_t) {}
