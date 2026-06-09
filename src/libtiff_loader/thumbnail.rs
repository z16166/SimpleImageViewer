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

pub(crate) fn extract_embedded_thumbnail(
    tif: *mut lib::TIFF,
    main_width: u32,
    target_size: u32,
) -> Option<(u32, u32, Vec<u8>)> {
    unsafe {
        let mut best_index = 0;
        let mut best_dim = 0;
        let mut best_pixels = None;

        // Iterate through IFDs to find the best-fitting thumbnail
        let mut dir_idx = 1;
        while lib::TIFFSetDirectory(tif, dir_idx) != 0 {
            let mut tw: lib::uint32 = 0;
            let mut th: lib::uint32 = 0;
            lib::TIFFGetField(tif, lib::TIFFTAG_IMAGEWIDTH, &mut tw);
            lib::TIFFGetField(tif, lib::TIFFTAG_IMAGELENGTH, &mut th);

            let dim = tw.max(th);
            let total_pixels = tw as u64 * th as u64;
            if total_pixels > 64 * 1024 * 1024 {
                // 64MP Limit
                dir_idx += 1;
                continue;
            }

            if tw > 0 && th > 0 && tw < main_width {
                if dim >= target_size && (best_pixels.is_none() || dim < best_dim) {
                    best_dim = dim;
                    best_index = dir_idx;

                    let mut raster = vec![0u32; (tw * th) as usize];
                    if lib::TIFFReadRGBAImageOriented(
                        tif,
                        tw,
                        th,
                        raster.as_mut_ptr(),
                        lib::ORIENTATION_TOPLEFT,
                        0,
                    ) != 0
                    {
                        let mut pixels = vec![0u8; (tw * th * 4) as usize];
                        std::ptr::copy_nonoverlapping(
                            raster.as_ptr() as *const u8,
                            pixels.as_mut_ptr(),
                            pixels.len(),
                        );
                        best_pixels = Some((tw as u32, th as u32, pixels));
                    }
                } else if best_pixels.is_none() && dim > best_dim {
                    best_dim = dim;
                    best_index = dir_idx;

                    let mut raster = vec![0u32; (tw * th) as usize];
                    if lib::TIFFReadRGBAImageOriented(
                        tif,
                        tw,
                        th,
                        raster.as_mut_ptr(),
                        lib::ORIENTATION_TOPLEFT,
                        0,
                    ) != 0
                    {
                        let mut pixels = vec![0u8; (tw * th * 4) as usize];
                        std::ptr::copy_nonoverlapping(
                            raster.as_ptr() as *const u8,
                            pixels.as_mut_ptr(),
                            pixels.len(),
                        );
                        best_pixels = Some((tw as u32, th as u32, pixels));
                    }
                }
            }
            dir_idx += 1;
        }

        lib::TIFFSetDirectory(tif, 0);
        if let Some(res) = best_pixels {
            log::info!(
                "LibTiff: Using embedded IFD{} thumbnail ({}x{}) for target size {}",
                best_index,
                res.0,
                res.1,
                target_size
            );
            return Some(res);
        }
        None
    }
}
