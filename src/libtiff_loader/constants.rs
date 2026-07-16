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

// TIFF Photometric Interpretations
pub(crate) const PHOTO_MINISWHITE: u16 = 0;
pub(crate) const PHOTO_MINISBLACK: u16 = 1;
pub(crate) const PHOTO_RGB: u16 = 2;
pub(crate) const PHOTO_PALETTE: u16 = 3;
pub(crate) const PHOTO_SEPARATED: u16 = 5;
pub(crate) const PHOTO_LOGL: u16 = 32844;
pub(crate) const PHOTO_LOGLUV: u16 = 32845;
pub(crate) const FORMAT_UINT: u16 = 1;
pub(crate) const FORMAT_INT: u16 = 2;
pub(crate) const FORMAT_IEEEFP: u16 = 3;
pub(crate) const CONFIG_CONTIG: u16 = 1;
pub(crate) const CONFIG_SEPARATE: u16 = 2;
#[allow(dead_code)]
pub(crate) const COMPRESSION_THUNDERSCAN: u16 = 32809;

/// Upper bound on TIFF tile width/height from tags; rejects absurd values before allocation.
pub(crate) const MAX_TIFF_TILE_DIMENSION: u32 = crate::constants::ABSOLUTE_MAX_TEXTURE_SIDE;

/// Budget (bytes) for strip/tile cache sizing in large TIFFs.
pub(crate) const STRIP_CACHE_BUDGET_BYTES: usize = 256 * 1024 * 1024;
pub(crate) const TILE_CACHE_BUDGET_BYTES: usize = STRIP_CACHE_BUDGET_BYTES;

/// Maximum pixel count for static full-image HDR decode paths (256 megapixels).
pub(crate) const MAX_STATIC_HDR_DECODE_PIXELS: u64 =
    crate::constants::MAX_STATIC_FULL_DECODE_PIXELS;

/// Poll cooperative cancel every N scanlines in owned TIFF row loops.
pub(crate) const TIFF_SCANLINE_CANCEL_POLL_INTERVAL: u32 = 64;

#[inline]
pub(crate) fn poll_tiff_scanline_cancel(
    cancel: Option<&std::sync::atomic::AtomicBool>,
    y: u32,
) -> Result<(), String> {
    if y & (TIFF_SCANLINE_CANCEL_POLL_INTERVAL - 1) == 0 {
        crate::loader::check_decode_cancel_str(cancel)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod cancel_poll_tests {
    use super::poll_tiff_scanline_cancel;
    use crate::loader::{DECODE_CANCELLED, DecodeCancelFlag};

    #[test]
    fn poll_tiff_scanline_cancel_respects_flag_on_interval() {
        let flag = DecodeCancelFlag::new();
        assert!(poll_tiff_scanline_cancel(Some(flag.as_atomic()), 0).is_ok());
        assert!(poll_tiff_scanline_cancel(Some(flag.as_atomic()), 63).is_ok());
        flag.cancel();
        assert_eq!(
            poll_tiff_scanline_cancel(Some(flag.as_atomic()), 0).unwrap_err(),
            DECODE_CANCELLED
        );
        // Off-interval rows skip the atomic load when already cancelled mid-loop;
        // next interval row will still observe cancel.
        assert!(poll_tiff_scanline_cancel(Some(flag.as_atomic()), 1).is_ok());
        assert_eq!(
            poll_tiff_scanline_cancel(Some(flag.as_atomic()), 64).unwrap_err(),
            DECODE_CANCELLED
        );
    }
}

/// Upper bound on concurrent libtiff handles per tiled/scanline source (2x img-loader threads).
pub(crate) const MAX_TIFF_HANDLE_POOL_SIZE: usize = crate::loader::MAX_IMG_LOADER_THREADS * 2;

/// RGBA byte length for `width` x `height` pixels; rejects dimension overflow.
pub(crate) fn checked_rgba_byte_len(width: u32, height: u32) -> Option<usize> {
    (width as u64)
        .checked_mul(height as u64)?
        .checked_mul(4)?
        .try_into()
        .ok()
}

/// Byte offset of RGBA pixel at row `y`, column `x` in a row-major buffer with `row_stride` pixels per row.
pub(crate) fn checked_rgba_byte_index(y: u32, x: u32, row_stride: u32) -> Option<usize> {
    let row = (y as u64).checked_mul(row_stride as u64)?;
    let pixel = row.checked_add(x as u64)?;
    pixel.checked_mul(4)?.try_into().ok()
}
