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
use std::os::raw::c_void;
use std::sync::Arc;

use super::handle::TiffHandle;
use crate::loader::TiledImageSource;

use memmap2::Mmap;
use parking_lot::Mutex;
use std::path::PathBuf;

use super::decode::{get_raw_value, process_scanline_contig, process_scanline_separate};
use super::handle::create_tiff_handle;
use super::thumbnail::extract_embedded_thumbnail;

// --- Scanline Implementation (Mock Tiles from Strips) ---

pub struct LibTiffScanlineSource {
    pub(crate) path: PathBuf,
    pub(crate) mmap: Arc<Mmap>,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) rows_per_strip: u32,
    pub(crate) pool: Mutex<Vec<TiffHandle>>,
    pub(crate) strip_cache: Mutex<std::collections::HashMap<u32, Arc<Vec<u8>>>>,
    pub(crate) cache_order: Mutex<Vec<u32>>,
    pub(crate) max_cached_strips: usize,
}

impl LibTiffScanlineSource {
    fn acquire_handle(&self) -> Result<TiffHandle, String> {
        {
            let mut pool = self.pool.lock();
            if let Some(handle) = pool.pop() {
                return Ok(handle);
            }
        }
        create_tiff_handle(self.mmap.clone(), &self.path)
    }

    fn release_handle(&self, handle: TiffHandle) {
        self.pool.lock().push(handle);
    }

    fn get_or_decode_strip(&self, strip_idx: u32, handle: &TiffHandle) -> Option<Arc<Vec<u8>>> {
        {
            let cache = self.strip_cache.lock();
            if let Some(data) = cache.get(&strip_idx) {
                let mut order = self.cache_order.lock();
                if let Some(pos) = order.iter().position(|&k| k == strip_idx) {
                    order.remove(pos);
                }
                order.push(strip_idx);
                return Some(Arc::clone(data));
            }
        }

        let rps = self.rows_per_strip;
        let mut strip_buf = vec![0u32; (self.width as usize) * (rps as usize)];

        let decoded = unsafe {
            lib::TIFFReadRGBAStrip(handle.as_ptr(), strip_idx * rps, strip_buf.as_mut_ptr()) != 0
        };

        if !decoded {
            return None;
        }

        let actual_rows = if (strip_idx + 1) * rps > self.height {
            self.height - strip_idx * rps
        } else {
            rps
        };
        let mut rgba = vec![0u8; (self.width as usize) * (actual_rows as usize) * 4];
        for row in 0..actual_rows {
            let src_row = (rps - 1 - row) as usize;
            let src_offset = src_row * self.width as usize;
            let dst_offset = row as usize * self.width as usize * 4;
            for col in 0..self.width as usize {
                let src_idx = src_offset + col;
                if src_idx < strip_buf.len() {
                    let pixel = strip_buf[src_idx].to_ne_bytes();
                    let dst_idx = dst_offset + col * 4;
                    rgba[dst_idx..dst_idx + 4].copy_from_slice(&pixel);
                }
            }
        }
        let data = Arc::new(rgba);

        {
            let mut cache = self.strip_cache.lock();
            let mut order = self.cache_order.lock();

            while order.len() >= self.max_cached_strips {
                if let Some(oldest) = order.first().copied() {
                    order.remove(0);
                    cache.remove(&oldest);
                }
            }

            cache.insert(strip_idx, Arc::clone(&data));
            order.push(strip_idx);
        }

        Some(data)
    }
}

impl TiledImageSource for LibTiffScanlineSource {
    fn width(&self) -> u32 {
        self.width
    }
    fn height(&self) -> u32 {
        self.height
    }

    fn extract_tile(&self, x: u32, y: u32, w: u32, h: u32) -> std::sync::Arc<Vec<u8>> {
        let mut result = vec![0u8; (w as usize) * (h as usize) * 4];
        let handle = match self.acquire_handle() {
            Ok(h) => h,
            Err(e) => {
                log::error!(
                    "[{}] libtiff: Failed to acquire handle for scanline: {}",
                    self.path.display(),
                    e
                );
                return std::sync::Arc::new(result);
            }
        };

        let rps = self.rows_per_strip;
        let start_strip = y / rps;
        let end_strip = (y + h - 1) / rps;

        for strip_idx in start_strip..=end_strip {
            let strip_data = match self.get_or_decode_strip(strip_idx, &handle) {
                Some(d) => d,
                None => continue,
            };

            let strip_y_start = strip_idx * rps;
            let actual_rows = if (strip_idx + 1) * rps > self.height {
                self.height - strip_y_start
            } else {
                rps
            };

            let intersect_y_start = y.max(strip_y_start);
            let intersect_y_end = (y + h).min(strip_y_start + actual_rows).min(self.height);
            let intersect_x_start = x;
            let intersect_x_end = (x + w).min(self.width);

            if intersect_y_start >= intersect_y_end || intersect_x_start >= intersect_x_end {
                continue;
            }

            let copy_bytes = (intersect_x_end - intersect_x_start) as usize * 4;

            for py in intersect_y_start..intersect_y_end {
                let row_in_strip = (py - strip_y_start) as usize;
                let src_offset =
                    (row_in_strip * self.width as usize + intersect_x_start as usize) * 4;
                let dst_y = (py - y) as usize;
                let dst_offset = (dst_y * w as usize + (intersect_x_start - x) as usize) * 4;

                if src_offset + copy_bytes <= strip_data.len()
                    && dst_offset + copy_bytes <= result.len()
                {
                    result[dst_offset..dst_offset + copy_bytes]
                        .copy_from_slice(&strip_data[src_offset..src_offset + copy_bytes]);
                }
            }
        }

        self.release_handle(handle);
        std::sync::Arc::new(result)
    }

    fn generate_preview(&self, max_w: u32, max_h: u32) -> (u32, u32, Vec<u8>) {
        let max_dim = max_w.max(max_h);
        let handle = match self.acquire_handle() {
            Ok(h) => h,
            Err(e) => {
                log::error!(
                    "[{}] libtiff: Failed to acquire handle for scanline preview: {}",
                    self.path.display(),
                    e
                );
                return (0, 0, vec![]);
            }
        };

        let embedded = extract_embedded_thumbnail(handle.as_ptr(), self.width, max_dim);

        if let Some(res) = embedded {
            let thumb_max = res.0.max(res.1);
            if max_w.max(max_h) <= 512 || thumb_max >= 2048 || thumb_max >= max_w.max(max_h) {
                self.release_handle(handle);
                return res;
            }
        }

        let scale = (max_w as f64 / self.width as f64)
            .min(max_h as f64 / self.height as f64)
            .min(1.0);
        let pw = (self.width as f64 * scale) as u32;
        let ph = (self.height as f64 * scale) as u32;
        if pw == 0 || ph == 0 {
            self.release_handle(handle);
            return (0, 0, vec![]);
        }

        let mut result = vec![0u8; (pw * ph * 4) as usize];
        log::info!(
            "libtiff: Generating stride-based fallback preview from strips ({}x{})",
            pw,
            ph
        );

        let tif_ptr = handle.as_ptr();
        let rps = self.rows_per_strip;
        let mut strip_buf = vec![0u32; (self.width as usize) * (rps as usize)];
        let mut last_strip_idx = u32::MAX;

        let stride_x_fp = ((self.width as u64) << 16) / pw as u64;
        let stride_y_fp = ((self.height as u64) << 16) / ph as u64;

        for ty in 0..ph {
            let y = ((ty as u64 * stride_y_fp) >> 16) as u32;
            let strip_idx = y / rps;
            let y_in_strip = y % rps;
            let dst_y_offset = (ty * pw) as usize * 4;

            unsafe {
                if strip_idx != last_strip_idx {
                    if lib::TIFFReadRGBAStrip(tif_ptr, strip_idx * rps, strip_buf.as_mut_ptr()) != 0
                    {
                        last_strip_idx = strip_idx;
                    } else {
                        continue;
                    }
                }

                for tx in 0..pw {
                    let x = ((tx as u64 * stride_x_fp) >> 16) as u32;
                    let src_idx =
                        (rps - 1 - y_in_strip) as usize * self.width as usize + x as usize;
                    if src_idx < strip_buf.len() {
                        let pixel = strip_buf[src_idx].to_ne_bytes();
                        let dst_idx = dst_y_offset + (tx as usize) * 4;
                        result[dst_idx..dst_idx + 4].copy_from_slice(&pixel);
                    }
                }
            }
        }

        self.release_handle(handle);
        (pw, ph, result)
    }

    fn full_pixels(&self) -> Option<Arc<Vec<u8>>> {
        None
    }
}

// TIFF Photometric Interpretations
const PHOTO_RGB: u16 = 2;
const PHOTO_PALETTE: u16 = 3;
const PHOTO_SEPARATED: u16 = 5;

// TIFF Sample Formats
const FORMAT_UINT: u16 = 1;
const FORMAT_IEEEFP: u16 = 3;

// TIFF Planar Configurations
const CONFIG_CONTIG: u16 = 1; // Contiguous / Chunky format (e.g., RGBRGBRGB...)
const CONFIG_SEPARATE: u16 = 2; // Planar format (e.g., RRR... GGG... BBB...)

pub(crate) unsafe fn manual_decode_scanline(
    tif: *mut lib::TIFF,
    width: u32,
    height: u32,
) -> Result<Vec<u8>, String> {
    let mut bps: u16 = 0;
    let mut spp: u16 = 0;
    let mut photo: u16 = 0;
    let mut config: u16 = CONFIG_CONTIG;
    let mut format: u16 = FORMAT_UINT; // SampleFormat
    let mut compression: u16 = 0;

    let swapped: bool;
    let mut smin: f64 = 0.0;
    let mut smax: f64 = 1.0;
    unsafe {
        lib::TIFFGetField(tif, lib::TIFFTAG_BITSPERSAMPLE, &mut bps);
        lib::TIFFGetField(tif, lib::TIFFTAG_SAMPLESPERPIXEL, &mut spp);
        lib::TIFFGetField(tif, lib::TIFFTAG_PHOTOMETRIC, &mut photo);
        lib::TIFFGetField(tif, lib::TIFFTAG_PLANARCONFIG, &mut config);
        lib::TIFFGetField(tif, lib::TIFFTAG_SAMPLEFORMAT, &mut format);
        lib::TIFFGetField(tif, lib::TIFFTAG_COMPRESSION, &mut compression);
        lib::TIFFGetField(tif, lib::TIFFTAG_COMPRESSION, &mut compression);

        swapped = lib::TIFFIsByteSwapped(tif) != 0;
    }

    let scanline_size = unsafe { lib::TIFFScanlineSize(tif) };
    if scanline_size <= 0 {
        return Err("Invalid scanline size".to_string());
    }

    let mut buf = vec![0u8; scanline_size as usize];
    let mut rgba = vec![255u8; width as usize * height as usize * 4];

    // Palette handling
    let mut r_map: *mut u16 = std::ptr::null_mut();
    let mut g_map: *mut u16 = std::ptr::null_mut();
    let mut b_map: *mut u16 = std::ptr::null_mut();
    if photo == PHOTO_PALETTE
        && unsafe {
            lib::TIFFGetField(
                tif,
                lib::TIFFTAG_COLORMAP,
                &mut r_map,
                &mut g_map,
                &mut b_map,
            )
        } == 0
    {
        return Err("Palette image missing colormap".to_string());
    }
    let samples_to_process = (spp as usize).min(match photo {
        PHOTO_RGB | PHOTO_SEPARATED => 4, // RGB(A) and CMYK
        _ => 1,
    });

    // Determine if we need a two-pass normalization for floats or large integers
    let mut smax_provided = false;
    let mut smin_provided = false;
    unsafe {
        let mut smin_v: f64 = 0.0;
        let mut smax_v: f64 = 0.0;
        if lib::TIFFGetField(tif, lib::TIFFTAG_SMINSAMPLEVALUE, &mut smin_v) != 0 {
            smin = smin_v;
            smin_provided = true;
        }
        if lib::TIFFGetField(tif, lib::TIFFTAG_SMAXSAMPLEVALUE, &mut smax_v) != 0 {
            smax = smax_v;
            smax_provided = true;
        }
    }

    // Auto-scale HDR formats if SMax is missing, excluding CMYK which has absolute values
    let use_auto_scale = !smax_provided
        && photo != PHOTO_SEPARATED
        && (format == FORMAT_IEEEFP || bps == 16 || bps == 32 || bps == 64);

    if use_auto_scale {
        let mut actual_min = f64::MAX;
        let mut actual_max = f64::MIN;

        let scans_per_row = if config == CONFIG_SEPARATE {
            samples_to_process
        } else {
            1
        };

        for s in 0..scans_per_row {
            for y in 0..height {
                if unsafe {
                    lib::TIFFReadScanline(tif, buf.as_mut_ptr() as *mut c_void, y, s as u16)
                } > 0
                {
                    let num_samples = if config == CONFIG_SEPARATE {
                        width as usize
                    } else {
                        width as usize * spp as usize
                    };
                    for idx in 0..num_samples {
                        let val = get_raw_value(&buf, idx, bps, format);
                        if val.is_finite() {
                            if val < actual_min {
                                actual_min = val;
                            }
                            if val > actual_max {
                                actual_max = val;
                            }
                        }
                    }
                }
            }
        }

        if actual_max > actual_min {
            if !smin_provided {
                smin = actual_min;
            }
            smax = actual_max;
        }
    }

    if config == CONFIG_CONTIG {
        // Contig
        for y in 0..height {
            if unsafe { lib::TIFFReadScanline(tif, buf.as_mut_ptr() as *mut c_void, y, 0) } <= 0 {
                buf.fill(0);
            }
            let row_offset = y as usize * width as usize * 4;
            process_scanline_contig(
                &buf,
                &mut rgba[row_offset..],
                width,
                bps,
                spp,
                photo,
                format,
                swapped,
                smin,
                smax,
                r_map,
                g_map,
                b_map,
            );
        }
    } else {
        // Separate
        for s in 0..samples_to_process {
            for y in 0..height {
                if unsafe {
                    lib::TIFFReadScanline(tif, buf.as_mut_ptr() as *mut c_void, y, s as u16)
                } <= 0
                {
                    buf.fill(0);
                }
                let row_offset = y as usize * width as usize * 4;
                process_scanline_separate(
                    &buf,
                    &mut rgba[row_offset..],
                    width,
                    bps,
                    s,
                    photo,
                    format,
                    swapped,
                    smin,
                    smax,
                );
            }
        }
    }
    Ok(rgba)
}
