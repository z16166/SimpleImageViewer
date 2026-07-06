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

#![allow(dead_code)]

use std::ffi::{CStr, c_char, c_int, c_void};
use std::ptr;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use memmap2::Mmap;
use openexr_core_sys as sys;

pub(crate) struct ExrMmapReadCookie {
    mmap: Arc<Mmap>,
    destroy_called: AtomicBool,
}

/// OpenEXRCore expects `pread`-like threading semantics — backing store is immutable mapped bytes.
unsafe extern "C" fn openexr_read_mmap(
    _ctxt: sys::ExrConstContext,
    userdata: *mut c_void,
    buffer: *mut c_void,
    sz: u64,
    offset: u64,
    _error_cb: *mut c_void,
) -> i64 {
    if userdata.is_null() || buffer.is_null() {
        return -1;
    }

    let cookie = unsafe { &*userdata.cast::<ExrMmapReadCookie>() };
    let slice = cookie.mmap.as_ref();
    let len = slice.len();
    let Ok(off) = usize::try_from(offset) else {
        return -1;
    };
    let Ok(n) = usize::try_from(sz) else {
        return -1;
    };
    let Some(end) = off.checked_add(n) else {
        return -1;
    };
    if end > len {
        return -1;
    }
    unsafe {
        ptr::copy_nonoverlapping(slice.as_ptr().add(off), buffer.cast::<u8>(), n);
    }
    sz as i64
}

unsafe extern "C" fn openexr_query_mmap_size(
    _ctxt: sys::ExrConstContext,
    userdata: *mut c_void,
) -> i64 {
    if userdata.is_null() {
        return -1;
    }
    let cookie = unsafe { &*userdata.cast::<ExrMmapReadCookie>() };
    cookie.mmap.len() as i64
}

unsafe extern "C" fn openexr_destroy_mmap_cookie(
    _ctxt: sys::ExrConstContext,
    userdata: *mut c_void,
    _failed: c_int,
) {
    if userdata.is_null() {
        return;
    }
    unsafe {
        let cookie = Arc::from_raw(userdata.cast::<ExrMmapReadCookie>());
        cookie.destroy_called.store(true, Ordering::Release);
    }
}

pub(crate) struct ExrMmapCookieGuard {
    c_ref: *const ExrMmapReadCookie,
    context_alive: bool,
}

impl ExrMmapCookieGuard {
    pub(crate) fn new(mmap: Mmap) -> Self {
        Self::from_shared(Arc::new(mmap))
    }

    pub(crate) fn from_shared(mmap: Arc<Mmap>) -> Self {
        let c_ref = Arc::into_raw(Arc::new(ExrMmapReadCookie {
            mmap,
            destroy_called: AtomicBool::new(false),
        }));
        Self {
            c_ref,
            context_alive: false,
        }
    }

    pub(crate) fn as_mut_ptr(&self) -> *mut c_void {
        self.c_ref.cast_mut().cast::<c_void>()
    }

    /// Mark the C-held cookie reference as owned by a live OpenEXRCore context.
    ///
    /// OpenEXRCore's public docs say `user_data` is caller-managed custom stream data. The
    /// implementation copies that pointer into a context. If header parsing later fails,
    /// `exr_start_read` calls `exr_finish(&ret)`, which invokes our `destroy_fn` and drops the
    /// C-held `Arc` reference.
    ///
    /// The Rust guard keeps a single `Arc` handed to OpenEXRCore via `c_ref`. If header parsing
    /// fails before creating a context and never calls `destroy_fn`, the guard drops the C-held
    /// reference itself. When a context is alive, only `destroy_fn` may reclaim that reference.
    pub(crate) fn mark_context_alive(&mut self) {
        self.context_alive = true;
    }
}

impl Drop for ExrMmapCookieGuard {
    fn drop(&mut self) {
        if self.context_alive {
            return;
        }
        unsafe {
            let cookie = &*self.c_ref;
            if cookie.destroy_called.load(Ordering::Acquire) {
                return;
            }
            drop(Arc::from_raw(self.c_ref));
        }
    }
}

/// OpenEXRCore prints to stderr when `error_handler_fn` is null. Mip/ripmap probing calls
/// `exr_get_tile_counts` on invalid levels and expects failure via return codes instead.
unsafe extern "C" fn openexr_error_handler_silent(
    _ctxt: sys::ExrConstContext,
    code: sys::ExrResult,
    text: *const c_char,
) {
    if log::log_enabled!(log::Level::Trace) {
        let message = if text.is_null() {
            "unknown OpenEXRCore error".to_string()
        } else {
            unsafe { CStr::from_ptr(text) }
                .to_string_lossy()
                .into_owned()
        };
        log::trace!("OpenEXRCore error {code}: {message}");
    }
}

fn openexr_base_initializer() -> sys::ExrContextInitializer {
    sys::ExrContextInitializer {
        size: std::mem::size_of::<sys::ExrContextInitializer>(),
        error_handler_fn: Some(openexr_error_handler_silent),
        alloc_fn: None,
        free_fn: None,
        user_data: ptr::null_mut(),
        read_fn: None,
        size_fn: None,
        write_fn: None,
        destroy_fn: None,
        max_image_width: -2,
        max_image_height: -2,
        max_tile_width: -2,
        max_tile_height: -2,
        zip_level: -2,
        dwa_quality: -1.0,
        flags: 0,
        pad: [0u8; 4],
    }
}

/// Default initializer for OpenEXRCore's built-in file reader (no custom mmap I/O).
pub(crate) fn openexr_file_initializer() -> sys::ExrContextInitializer {
    openexr_base_initializer()
}

pub(crate) fn openexr_memory_map_initializer(cookie: *mut c_void) -> sys::ExrContextInitializer {
    let mut init = openexr_base_initializer();
    init.user_data = cookie;
    init.read_fn = Some(openexr_read_mmap);
    init.size_fn = Some(openexr_query_mmap_size);
    init.destroy_fn = Some(openexr_destroy_mmap_cookie);
    init
}
