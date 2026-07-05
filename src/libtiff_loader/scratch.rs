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

/// Grow `buf` to `len` without zero-filling. Safe when the caller overwrites every
/// element before the slice is read (e.g. libtiff strip decode, full RGBA conversion).
#[inline]
fn prepare_uninit<T>(buf: &mut Vec<T>, len: usize) {
    buf.clear();
    if buf.capacity() < len {
        buf.reserve(len - buf.capacity());
    }
    unsafe {
        buf.set_len(len);
    }
}

/// Zero-fill is required for sparse tile extraction where missing tiles leave gaps.
#[inline]
fn prepare_zeroed_u8(buf: &mut Vec<u8>, len: usize) {
    buf.clear();
    buf.resize(len, 0);
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
pub(crate) fn with_tiled_decode_scratch<R>(
    tile_len: usize,
    rgba_len: usize,
    f: impl FnOnce(&mut TiledDecodeScratch) -> Option<R>,
) -> Option<(R, Vec<u8>)> {
    TILED_DECODE_SCRATCH.with(|scratch| {
        let mut scratch = scratch.borrow_mut();
        prepare_uninit(&mut scratch.tile, tile_len);
        prepare_uninit(&mut scratch.rgba, rgba_len);
        let r = f(&mut scratch)?;
        let output = std::mem::replace(&mut scratch.rgba, Vec::with_capacity(rgba_len));
        Some((r, output))
    })
}

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

pub(crate) fn with_scanline_strip_scratch<R>(
    strip_len: usize,
    rgba_len: usize,
    f: impl FnOnce(&mut ScanlineStripScratch) -> R,
) -> (R, Vec<u8>) {
    SCANLINE_STRIP_SCRATCH.with(|scratch| {
        let mut scratch = scratch.borrow_mut();
        prepare_uninit(&mut scratch.strip, strip_len);
        prepare_uninit(&mut scratch.rgba, rgba_len);
        let r = f(&mut scratch);
        let output = std::mem::replace(&mut scratch.rgba, Vec::with_capacity(rgba_len));
        (r, output)
    })
}

pub(crate) fn with_scanline_strip_buf<R>(strip_len: usize, f: impl FnOnce(&mut [u32]) -> R) -> R {
    SCANLINE_STRIP_SCRATCH.with(|scratch| {
        let mut scratch = scratch.borrow_mut();
        prepare_uninit(&mut scratch.strip, strip_len);
        f(&mut scratch.strip)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

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
