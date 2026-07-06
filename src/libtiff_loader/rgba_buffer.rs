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
//! Clamp that sentinel to `ImageLength` before multiplying by width. libtiff's strip RGBA
//! path also needs one extra image-width row of uint32 slack for the vertical-flip writer.

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
    // gtStripContig + FLIP_VERTICALLY: putRGBcontig8bittile uses toskew = -(w + wmin).
    // For the last row in a strip that can leave the write cursor one row below the
    // nominal width*rps window before the call returns; keep one row of uint32 slack.
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

#[cfg(feature = "preload-debug")]
pub(crate) struct TiffRgbaStripCallDebug {
    pub site: &'static str,
    pub path: std::path::PathBuf,
    pub strip_idx: u32,
    pub read_row: u32,
    pub image_width: u32,
    pub image_height: u32,
    pub stored_rows_per_strip: u32,
    pub tag_present: bool,
    pub tag_rows_per_strip: u32,
    pub defaulted_rows_per_strip: u32,
    pub effective_rows_per_strip: u32,
    pub libtiff_rows_to_read: u32,
    pub strip_buf_u32_len: usize,
    pub strip_buf_bytes: usize,
    pub actual_rows: u32,
    pub rgba_len: usize,
    pub bps: u16,
    pub spp: u16,
    pub photometric: u16,
    pub orientation: u16,
    pub raster_ptr: usize,
}

#[cfg(feature = "preload-debug")]
pub(crate) unsafe fn tiff_rgba_strip_call_debug(
    site: &'static str,
    path: &std::path::Path,
    tif: *mut lib::TIFF,
    image_width: u32,
    image_height: u32,
    stored_rows_per_strip: u32,
    strip_idx: u32,
    read_row: u32,
    strip_buf_u32_len: usize,
    actual_rows: u32,
    rgba_len: usize,
    raster: *const lib::uint32,
) -> TiffRgbaStripCallDebug {
    unsafe {
        let mut tag_rows_per_strip = 0u32;
        let tag_present =
            lib::TIFFGetField(tif, lib::TIFFTAG_ROWSPERSTRIP, &mut tag_rows_per_strip) != 0;
        let mut defaulted_rows_per_strip = TIFF_DEFAULT_ROWSPERSTRIP;
        let _ = lib::TIFFGetFieldDefaulted(
            tif,
            lib::TIFFTAG_ROWSPERSTRIP,
            &mut defaulted_rows_per_strip,
        );
        let effective_rows_per_strip = tiff_effective_rows_per_strip(tif, image_height);
        let libtiff_rows_to_read = if read_row + defaulted_rows_per_strip > image_height {
            image_height.saturating_sub(read_row)
        } else {
            defaulted_rows_per_strip
        };
        let mut bps = 0u16;
        let mut spp = 0u16;
        let mut photometric = 0u16;
        let mut orientation = 0u16;
        lib::TIFFGetField(tif, lib::TIFFTAG_BITSPERSAMPLE, &mut bps);
        lib::TIFFGetField(tif, lib::TIFFTAG_SAMPLESPERPIXEL, &mut spp);
        lib::TIFFGetField(tif, lib::TIFFTAG_PHOTOMETRIC, &mut photometric);
        lib::TIFFGetField(tif, lib::TIFFTAG_ORIENTATION, &mut orientation);
        TiffRgbaStripCallDebug {
            site,
            path: path.to_path_buf(),
            strip_idx,
            read_row,
            image_width,
            image_height,
            stored_rows_per_strip,
            tag_present,
            tag_rows_per_strip,
            defaulted_rows_per_strip,
            effective_rows_per_strip,
            libtiff_rows_to_read,
            strip_buf_u32_len,
            strip_buf_bytes: strip_buf_u32_len.saturating_mul(std::mem::size_of::<lib::uint32>()),
            actual_rows,
            rgba_len,
            bps,
            spp,
            photometric,
            orientation,
            raster_ptr: raster as usize,
        }
    }
}

#[cfg(feature = "preload-debug")]
fn debug_path_file_name(path: &std::path::Path) -> &str {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("?")
}

#[cfg(feature = "preload-debug")]
pub(crate) fn log_tiff_rgba_strip_call(debug: &TiffRgbaStripCallDebug) {
    crate::preload_debugger!(
        "[PreloadDebug][LibTiffRGBAStrip] site={} file={} path={} strip_idx={} read_row={} \
         image={}x{} stored_rps={} tag_present={} tag_rps={} defaulted_rps={} effective_rps={} \
         libtiff_rows_to_read={} strip_buf_u32={} strip_buf_bytes={} actual_rows={} rgba_len={} \
         bps={} spp={} photo={} orient={} raster=0x{:x} \
         check: strip_buf_u32 >= width*effective_rps+width => {}>={}*{}+{}={}",
        debug.site,
        debug_path_file_name(&debug.path),
        debug.path.display(),
        debug.strip_idx,
        debug.read_row,
        debug.image_width,
        debug.image_height,
        debug.stored_rows_per_strip,
        debug.tag_present,
        debug.tag_rows_per_strip,
        debug.defaulted_rows_per_strip,
        debug.effective_rows_per_strip,
        debug.libtiff_rows_to_read,
        debug.strip_buf_u32_len,
        debug.strip_buf_bytes,
        debug.actual_rows,
        debug.rgba_len,
        debug.bps,
        debug.spp,
        debug.photometric,
        debug.orientation,
        debug.raster_ptr,
        debug.strip_buf_u32_len,
        debug.image_width,
        debug.effective_rows_per_strip,
        debug.image_width,
        debug.image_width as usize * debug.effective_rows_per_strip as usize
            + debug.image_width as usize,
    );
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
