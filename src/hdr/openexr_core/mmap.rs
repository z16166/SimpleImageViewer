// Simple Image Viewer - A high-performance, cross-platform image viewer
// Copyright (C) 2024-2026 Simple Image Viewer Contributors
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

#![allow(dead_code)]

use std::ffi::{c_int, c_void};
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
    cookie: Arc<ExrMmapReadCookie>,
    c_ref: *const ExrMmapReadCookie,
    context_alive: bool,
}

impl ExrMmapCookieGuard {
    pub(crate) fn new(mmap: Mmap) -> Self {
        let cookie = Arc::new(ExrMmapReadCookie {
            mmap: Arc::new(mmap),
            destroy_called: AtomicBool::new(false),
        });
        let c_ref = Arc::into_raw(Arc::clone(&cookie));
        Self {
            cookie,
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
    /// The Rust guard keeps its own `Arc`, so the callback can signal whether cleanup happened. If
    /// `exr_start_read` fails before creating a context and never calls `destroy_fn`, the guard drops
    /// the C-held reference itself.
    pub(crate) fn mark_context_alive(&mut self) {
        self.context_alive = true;
    }
}

impl Drop for ExrMmapCookieGuard {
    fn drop(&mut self) {
        if self.context_alive || self.cookie.destroy_called.load(Ordering::Acquire) {
            return;
        }
        unsafe {
            drop(Arc::from_raw(self.c_ref));
        }
    }
}

pub(crate) fn openexr_memory_map_initializer(cookie: *mut c_void) -> sys::ExrContextInitializer {
    sys::ExrContextInitializer {
        size: std::mem::size_of::<sys::ExrContextInitializer>(),
        error_handler_fn: None,
        alloc_fn: None,
        free_fn: None,
        user_data: cookie,
        read_fn: Some(openexr_read_mmap),
        size_fn: Some(openexr_query_mmap_size),
        write_fn: None,
        destroy_fn: Some(openexr_destroy_mmap_cookie),
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
