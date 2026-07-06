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

//! Buffer sizing for libtiff `TIFFReadRGBAStrip`.
//!
//! TIFF 6.0: missing `ROWSPERSTRIP` defaults to `(uint32_t)-1` (single strip = full image).
//! Clamp that sentinel to `ImageLength` before multiplying by width.

use libtiff_viewer as lib;

/// `ROWSPERSTRIP` sentinel when the tag is missing (`td_rowsperstrip` in libtiff).
pub(crate) const TIFF_DEFAULT_ROWSPERSTRIP: u32 = u32::MAX;

/// Normalize tag/default `ROWSPERSTRIP` for buffer sizing and strip indexing.
#[inline]
pub(crate) fn effective_rows_per_strip(tag_rows_per_strip: u32, image_height: u32) -> u32 {
    if image_height == 0 {
        return 0;
    }
    if tag_rows_per_strip == 0
        || tag_rows_per_strip == TIFF_DEFAULT_ROWSPERSTRIP
        || tag_rows_per_strip > image_height
    {
        image_height
    } else {
        tag_rows_per_strip
    }
}

/// Read `ROWSPERSTRIP` from an open TIFF and return the clamped strip height.
pub(crate) unsafe fn tiff_effective_rows_per_strip(tif: *mut lib::TIFF, image_height: u32) -> u32 {
    unsafe {
        let mut rows_per_strip = 0u32;
        let has_tag = lib::TIFFGetField(tif, lib::TIFFTAG_ROWSPERSTRIP, &mut rows_per_strip) != 0;
        if !has_tag || rows_per_strip == 0 || rows_per_strip == TIFF_DEFAULT_ROWSPERSTRIP {
            effective_rows_per_strip(TIFF_DEFAULT_ROWSPERSTRIP, image_height)
        } else {
            effective_rows_per_strip(rows_per_strip, image_height)
        }
    }
}

/// `TIFFReadRGBAStrip` raster length in 32-bit pixels (`width * effective_rows_per_strip + width`).
pub(crate) unsafe fn tiff_rgba_strip_buffer_u32_count(
    tif: *mut lib::TIFF,
    image_width: u32,
    image_height: u32,
) -> Option<usize> {
    if image_width == 0 {
        return None;
    }
    let rps = unsafe { tiff_effective_rows_per_strip(tif, image_height) };
    if rps == 0 {
        return None;
    }
    let base = (image_width as usize).checked_mul(rps as usize)?;
    // Regression (Page Heap / WinDbg, F:\win7\top100): `width * rps` uint32 pixels is not
    // enough for `TIFFReadRGBAStrip`. In gtStripContig + FLIP_VERTICALLY, putRGBcontig8bittile
    // uses `toskew = -(w + wmin)`; the last row of a strip can write one full image-width row
    // past the nominal window (e.g. heic0601a 18000x18000 rps=4: AV at putRGBcontig8bittile
    // with a 72000-u32 buffer; needs 90000 = 18000*4 + 18000). Always add `width` slack.
    base.checked_add(image_width as usize)
}

/// `TIFFReadRGBATile` raster length in 32-bit pixels (`tile_width * tile_length`).
pub(crate) unsafe fn tiff_rgba_tile_buffer_u32_count(tif: *mut lib::TIFF) -> Option<usize> {
    unsafe {
        let mut tile_width: u32 = 0;
        let mut tile_length: u32 = 0;
        if lib::TIFFGetField(tif, lib::TIFFTAG_TILEWIDTH, &mut tile_width) == 0 || tile_width == 0 {
            return None;
        }
        if lib::TIFFGetField(tif, lib::TIFFTAG_TILELENGTH, &mut tile_length) == 0
            || tile_length == 0
        {
            return None;
        }
        tile_width.checked_mul(tile_length).map(|n| n as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_tag_is_full_image_height() {
        assert_eq!(
            effective_rows_per_strip(TIFF_DEFAULT_ROWSPERSTRIP, 10_000),
            10_000
        );
    }

    #[test]
    fn oversized_tag_clamps_to_image_height() {
        assert_eq!(effective_rows_per_strip(8192, 500), 500);
    }

    #[test]
    fn normal_tag_is_preserved() {
        assert_eq!(effective_rows_per_strip(8192, 10_000), 8192);
    }

    #[test]
    fn rgba_strip_buffer_adds_one_row_slack() {
        let width = 18_000usize;
        let rps = 4usize;
        let with_slack = width
            .checked_mul(rps)
            .and_then(|base| base.checked_add(width));
        assert_eq!(with_slack, Some(90_000));
    }
}
