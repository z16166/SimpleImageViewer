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

//! Per-thread decode scratch for libtiff tiled/scanline paths.
//!
//! Tile decode and tile extraction use separate thread-local buffers so
//! `extract_tile` can call `get_or_decode_tile` without `RefCell` reentrancy.
//!
//! ## Buffer initialization policy
//!
//! 1. [`prepare_uninit`] -- caller **must** overwrite every byte/slot in `0..len` before any read.
//! 2. [`prepare_zeroed_u8`] -- sparse or uncertain fill; safe to read any index (gaps are 0).
//! 3. libtiff RGBA strip/tile **u32** scratch -- sized with [`grow_vec_uninit`], then only the
//!    read span is cleared via [`zero_rgba_strip_read_span`] / [`zero_rgba_tile_read_span`]
//!    before libtiff and before conversion (libtiff does not guarantee full-buffer writes).

use std::cell::RefCell;

pub(crate) struct TiledExtractScratch {
    pub(crate) result: Vec<u8>,
}

pub(crate) struct TiledDecodeScratch {
    pub(crate) tile: Vec<u32>,
    pub(crate) rgba: Vec<u8>,
}

pub(crate) struct ScanlineStripScratch {
    pub(crate) strip: Vec<u32>,
    pub(crate) rgba: Vec<u8>,
}

/// Grow `buf` to `len` without initializing elements (internal building block only).
#[inline]
fn grow_vec_uninit<T>(buf: &mut Vec<T>, len: usize) {
    buf.clear();
    if buf.capacity() < len {
        // Regression (nightly UB check, dir-tree strip worker): after `clear()`, `len()` is 0
        // but `capacity()` may still be below the next `len` (e.g. rayon thread reuses TLS
        // scratch: heic0601a strip 90_000 u32 then heic0604a needs 95_000). `Vec::reserve`
        // guarantees `len() + additional <= capacity()`, NOT `capacity() + additional`, so
        // `reserve(len - capacity())` is a no-op when cap already exceeds the delta (90000
        // cap, need 95000: reserve(5000) only requires cap >= 5000) and `set_len(95000)` panics.
        // After clear, use `reserve(len - len())` == `reserve(len)`.
        buf.reserve(len.saturating_sub(buf.len()));
    }
    unsafe {
        debug_assert!(buf.capacity() >= len);
        buf.set_len(len);
    }
}

/// Resize `buf` to `len` **without initializing** its contents.
///
/// # Contract (mandatory)
///
/// - This function does **not** initialize the buffer.
/// - The caller **must** completely overwrite every element in `buf[0..len]` with valid data
///   **before any read** of any element in that range (including implicit reads such as
///   `copy_from_slice`, hashing, or comparisons).
/// - If any slot may remain unfilled -- for example libtiff RGBA APIs, sparse tile compositing,
///   or conditional loops that skip output pixels -- **do not call this function**; use
///   [`prepare_zeroed_u8`], or zero the readable sub-range explicitly after [`grow_vec_uninit`].
#[inline]
fn prepare_uninit<T>(buf: &mut Vec<T>, len: usize) {
    grow_vec_uninit(buf, len);
}

/// Zero-filled u8 buffer for outputs that may contain gaps (e.g. sparse tile extract).
#[inline]
fn prepare_zeroed_u8(buf: &mut Vec<u8>, len: usize) {
    buf.clear();
    buf.resize(len, 0);
}

/// Zero only the u32 strip rows that the RGBA strip converter reads after `TIFFReadRGBAStrip`.
///
/// Call after [`grow_vec_uninit`] and **before** libtiff / conversion. libtiff may return success
/// without filling the whole libtiff-sized buffer on malformed input; clearing this span ensures
/// read slots are defined (0) instead of uninitialized.
#[inline]
pub(crate) fn zero_rgba_strip_read_span(
    strip: &mut [u32],
    width: u32,
    rows_per_strip: u32,
    actual_rows: u32,
) {
    let first_src_row = (rows_per_strip - actual_rows) as usize;
    let read_start = first_src_row.saturating_mul(width as usize);
    let read_end = (rows_per_strip as usize)
        .saturating_mul(width as usize)
        .min(strip.len());
    if read_start < read_end {
        strip[read_start..read_end].fill(0);
    }
}

/// Zero the `tw * th` tile pixels read by the RGBA tile flip loop after `TIFFReadRGBATile`.
///
/// Same contract as [`zero_rgba_strip_read_span`]: required before libtiff when using
/// [`grow_vec_uninit`] rather than [`prepare_uninit`].
#[inline]
pub(crate) fn zero_rgba_tile_read_span(tile: &mut [u32], tile_width: u32, tile_height: u32) {
    let count = (tile_width as usize)
        .saturating_mul(tile_height as usize)
        .min(tile.len());
    tile[..count].fill(0);
}

// Tile workers call extract_tile in parallel; per-thread scratch avoids Mutex contention.
thread_local! {
    static TILED_EXTRACT_SCRATCH: RefCell<TiledExtractScratch> = const {
        RefCell::new(TiledExtractScratch {
            result: Vec::new(),
        })
    };
    static TILED_DECODE_SCRATCH: RefCell<TiledDecodeScratch> = const {
        RefCell::new(TiledDecodeScratch {
            tile: Vec::new(),
            rgba: Vec::new(),
        })
    };
    static SCANLINE_EXTRACT_RESULT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    static SCANLINE_STRIP_SCRATCH: RefCell<ScanlineStripScratch> = const {
        RefCell::new(ScanlineStripScratch {
            strip: Vec::new(),
            rgba: Vec::new(),
        })
    };
}

/// Decode one libtiff RGBA tile into reusable thread-local buffers.
///
/// Returns `None` when `f` fails (e.g. `TIFFReadRGBATile` error). On success the
/// RGBA bytes are moved out via `mem::replace` to avoid an extra copy.
///
/// - `scratch.tile`: sized with [`grow_vec_uninit`]; closure **must** call
///   [`zero_rgba_tile_read_span`] before `TIFFReadRGBATile` and must only read inside that span.
/// - `scratch.rgba`: [`prepare_uninit`]; closure **must** write all `rgba_len` bytes (full tile
///   RGBA conversion loop).
pub(crate) fn with_tiled_decode_scratch<R>(
    tile_len: usize,
    rgba_len: usize,
    f: impl FnOnce(&mut TiledDecodeScratch) -> Option<R>,
) -> Option<(R, Vec<u8>)> {
    TILED_DECODE_SCRATCH.with(|scratch| {
        let mut scratch = scratch.borrow_mut();
        grow_vec_uninit(&mut scratch.tile, tile_len);
        prepare_uninit(&mut scratch.rgba, rgba_len);
        let r = f(&mut scratch)?;
        let output = std::mem::replace(&mut scratch.rgba, Vec::with_capacity(rgba_len));
        Some((r, output))
    })
}

/// Sparse tile extract: missing tiles leave transparent gaps -- fully zeroed output.
pub(crate) fn with_tiled_extract_scratch<R>(
    result_len: usize,
    f: impl FnOnce(&mut TiledExtractScratch) -> R,
) -> (R, Vec<u8>) {
    TILED_EXTRACT_SCRATCH.with(|scratch| {
        let mut scratch = scratch.borrow_mut();
        prepare_zeroed_u8(&mut scratch.result, result_len);
        let r = f(&mut scratch);
        let output = std::mem::replace(&mut scratch.result, Vec::with_capacity(result_len));
        (r, output)
    })
}

/// Scanline mock-tile extract: same sparse semantics as [`with_tiled_extract_scratch`].
pub(crate) fn with_scanline_extract_result<R>(
    result_len: usize,
    f: impl FnOnce(&mut [u8]) -> R,
) -> (R, Vec<u8>) {
    SCANLINE_EXTRACT_RESULT.with(|result| {
        let mut result = result.borrow_mut();
        prepare_zeroed_u8(&mut result, result_len);
        let r = f(&mut result);
        let output = std::mem::replace(&mut *result, Vec::with_capacity(result_len));
        (r, output)
    })
}

/// Decode one libtiff RGBA strip plus conversion into reusable thread-local buffers.
///
/// - `scratch.strip`: [`grow_vec_uninit`]; closure **must** call [`zero_rgba_strip_read_span`]
///   before `TIFFReadRGBAStrip` and must only read inside that span.
/// - `scratch.rgba`: [`prepare_uninit`]; closure **must** write all `rgba_len` bytes.
pub(crate) fn with_scanline_strip_scratch<R>(
    strip_len: usize,
    rgba_len: usize,
    f: impl FnOnce(&mut ScanlineStripScratch) -> R,
) -> (R, Vec<u8>) {
    SCANLINE_STRIP_SCRATCH.with(|scratch| {
        let mut scratch = scratch.borrow_mut();
        grow_vec_uninit(&mut scratch.strip, strip_len);
        prepare_uninit(&mut scratch.rgba, rgba_len);
        let r = f(&mut scratch);
        let output = std::mem::replace(&mut scratch.rgba, Vec::with_capacity(rgba_len));
        (r, output)
    })
}

/// Reusable libtiff RGBA strip u32 buffer (preview stride sampling).
///
/// Sized with [`grow_vec_uninit`]; closure **must** call [`zero_rgba_strip_read_span`] before
/// each `TIFFReadRGBAStrip` for the strip being read and must only read inside that span.
pub(crate) fn with_scanline_strip_buf<R>(strip_len: usize, f: impl FnOnce(&mut [u32]) -> R) -> R {
    SCANLINE_STRIP_SCRATCH.with(|scratch| {
        let mut scratch = scratch.borrow_mut();
        grow_vec_uninit(&mut scratch.strip, strip_len);
        f(&mut scratch.strip)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn prepare_uninit_grows_when_new_len_exceeds_reused_capacity() {
        let mut buf: Vec<u32> = Vec::new();
        prepare_uninit(&mut buf, 90_000);
        assert_eq!(buf.len(), 90_000);
        assert!(buf.capacity() >= 90_000);
        // Caller contract: overwrite before read.
        buf.fill(0);

        // heic0604a-style strip on same thread-local scratch: need 95_000 after 90_000 cap.
        prepare_uninit(&mut buf, 95_000);
        assert_eq!(buf.len(), 95_000);
        assert!(buf.capacity() >= 95_000);
        buf.fill(0);
    }

    #[test]
    fn tiled_decode_and_extract_scratch_do_not_reenter() {
        thread_local! {
            static REENTRANT: Cell<bool> = const { Cell::new(false) };
        }

        let nested = with_tiled_extract_scratch(16, |extract| {
            REENTRANT.with(|flag| flag.set(true));
            let nested = with_tiled_decode_scratch(4, 16, |decode| {
                assert!(REENTRANT.with(|flag| flag.get()));
                decode.tile.fill(0xAABBCCDD);
                decode.rgba.fill(0xFF);
                Some(())
            });
            assert!(nested.is_some());
            extract.result.fill(0x11);
        });
        assert_eq!(nested.1.len(), 16);
        assert!(nested.1.iter().all(|&b| b == 0x11));
    }
}
