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
use super::decode::{
    CameraTiffHdrUpgrade, IeeeSceneLinearDecodeArgs, LogLuvDecodeParams,
    decode_ieee_scene_linear_rgba32f, decode_logl_logluv_scene_linear_rgba32f,
    decode_uint16_rgb_scene_linear_rgba32f, tiff_ieee_scene_linear_eligible,
    tiff_logl_logluv_hdr_eligible, tiff_uint16_rgb_scene_linear_eligible,
    try_camera_tiff_rgb8_hdr_upgrade,
};
use super::scanline::{LibTiffScanlineSource, manual_decode_scanline};
use super::tiled::LibTiffTiledSource;
use crate::hdr::types::{
    HdrColorProfile, HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat, HdrReference,
    HdrToneMapSettings, HdrTransferFunction,
};
use parking_lot::Mutex;
use std::ffi::CString;

use super::constants::*;
use libtiff_viewer as lib;
use std::os::raw::c_void;
use std::path::Path;
use std::sync::Arc;

use super::handle::{TiffHandle, TiffHandlePool, path_to_tiff_name};
use super::mmap::{
    TiffMmapContext, tiff_close_proc, tiff_map_proc, tiff_read_proc, tiff_seek_proc,
    tiff_size_proc, tiff_unmap_proc, tiff_write_proc,
};
use super::orientation::{apply_orientation_buffer, apply_orientation_buffer_f32};
use crate::loader::{DecodedImage, ImageData};

/// Validate that every strip/tile offset+byte_count entry fits within the mmap.
///
/// # libtiff version dependency
///
/// Reads `uint64_t*` arrays via [`TiffGuard::get_field_u64_array`], which relies on
/// libtiff's internal IFD directory layout for `TIFFTAG_STRIPOFFSETS` /
/// `TIFFTAG_TILEOFFSETS` and the corresponding byte-count tags. This convention is
/// stable in libtiff ≥ 4.0 (all modern builds). Alternative: use the public strile API
/// (`TIFFGetStrileOffsetWithErr` / `TIFFGetStrileByteCountWithErr`), which is less
/// coupled to the internal layout but may not be available in older libtiff builds.
///
/// Returns `Err` when an IFD is malformed (missing offset/byte-count arrays) or
/// any entry references data beyond the mapped file — defense-in-depth for corrupted
/// TIFF files that libtiff's own internal guards might not fully catch.
unsafe fn validate_tiff_offsets(
    tif: *mut lib::TIFF,
    guard: &lib::TiffGuard,
    is_tiled: bool,
    mmap_len: usize,
) -> Result<(), String> {
    let (num, offsets_tag, byte_counts_tag, label) = if is_tiled {
        // SAFETY: `tif` is a valid libtiff handle; the call is safe as long as the handle is alive.
        (
            unsafe { lib::TIFFNumberOfTiles(tif) },
            lib::TIFFTAG_TILEOFFSETS,
            lib::TIFFTAG_TILEBYTECOUNTS,
            "tile",
        )
    } else {
        // SAFETY: `tif` is a valid libtiff handle; the call is safe as long as the handle is alive.
        (
            unsafe { lib::TIFFNumberOfStrips(tif) },
            lib::TIFFTAG_STRIPOFFSETS,
            lib::TIFFTAG_STRIPBYTECOUNTS,
            "strip",
        )
    };

    // SAFETY: `get_field_u64_array` reads a pointer from libtiff's directory storage;
    // the handle is alive and we are still in the directory context.
    let Some(offsets) = (unsafe { guard.get_field_u64_array(offsets_tag) }) else {
        return Err(format!(
            "TIFF {label} offset array is missing (corrupted IFD)"
        ));
    };
    let Some(byte_counts) = (unsafe { guard.get_field_u64_array(byte_counts_tag) }) else {
        return Err(format!(
            "TIFF {label} byte-count array is missing (corrupted IFD)"
        ));
    };

    let mmap_len = mmap_len as u64;
    for i in 0..num as usize {
        // SAFETY: libtiff owns these arrays; they live as long as the directory is loaded.
        let off = unsafe { *offsets.add(i) };
        let len = unsafe { *byte_counts.add(i) };
        if len == 0 {
            continue; // empty strip/tile is legal
        }
        let Some(end) = off.checked_add(len) else {
            return Err(format!(
                "TIFF {label}[{i}] offset+byte_count wraps: off={off} len={len}"
            ));
        };
        if end > mmap_len {
            return Err(format!(
                "TIFF {label}[{i}] offset+byte_count {end} exceeds mmap size {mmap_len}"
            ));
        }
    }
    Ok(())
}

/// IFD0 tags for diagnostics (tests / support).
#[cfg(test)]
pub fn peek_tiff_tags(path: &Path) -> Result<String, String> {
    let mmap = Arc::new(crate::mmap_util::map_file(path)?.0);
    let mut ctx = Box::new(TiffMmapContext::new(mmap));
    unsafe {
        let c_path = path_to_tiff_name(path);
        let c_mode = CString::new("r").map_err(|_| "Invalid mode".to_string())?;
        let tif_ptr = lib::TIFFClientOpen(
            c_path.as_ptr(),
            c_mode.as_ptr(),
            ctx.as_mut() as *mut TiffMmapContext as *mut c_void,
            tiff_read_proc,
            tiff_write_proc,
            tiff_seek_proc,
            tiff_close_proc,
            tiff_size_proc,
            tiff_map_proc,
            tiff_unmap_proc,
        );
        if tif_ptr.is_null() {
            return Err("TIFFClientOpen failed".to_string());
        }
        let mut width: lib::uint32 = 0;
        let mut height: lib::uint32 = 0;
        let mut bps: u16 = 0;
        let mut photo: u16 = 0;
        let mut sample_format: u16 = lib::SAMPLEFORMAT_UINT;
        let mut spp: u16 = 0;
        let mut planar_config: u16 = CONFIG_CONTIG;
        lib::TIFFGetField(tif_ptr, lib::TIFFTAG_IMAGEWIDTH, &mut width);
        lib::TIFFGetField(tif_ptr, lib::TIFFTAG_IMAGELENGTH, &mut height);
        lib::TIFFGetField(tif_ptr, lib::TIFFTAG_BITSPERSAMPLE, &mut bps);
        lib::TIFFGetField(tif_ptr, lib::TIFFTAG_PHOTOMETRIC, &mut photo);
        lib::TIFFGetField(tif_ptr, lib::TIFFTAG_SAMPLEFORMAT, &mut sample_format);
        lib::TIFFGetField(tif_ptr, lib::TIFFTAG_SAMPLESPERPIXEL, &mut spp);
        lib::TIFFGetField(tif_ptr, lib::TIFFTAG_PLANARCONFIG, &mut planar_config);
        lib::TIFFClose(tif_ptr);
        drop(ctx);
        Ok(format!(
            "tiff tags: {width}x{height} bps={bps} photo={photo} sample_format={sample_format} spp={spp} planar={planar_config}"
        ))
    }
}

#[cfg(test)]
pub fn load_via_libtiff(
    path: &Path,
    hdr_target_capacity: f32,
    tone_map: HdrToneMapSettings,
) -> Result<ImageData, String> {
    let mmap = Arc::new(crate::mmap_util::map_file(path)?.0);
    load_via_libtiff_from_mmap(path, mmap, hdr_target_capacity, tone_map, None)
}

pub(crate) fn load_via_libtiff_from_mmap(
    path: &Path,
    mmap: Arc<memmap2::Mmap>,
    hdr_target_capacity: f32,
    tone_map: HdrToneMapSettings,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<ImageData, String> {
    crate::loader::check_decode_cancel_str(cancel)?;
    let mut ctx = Box::new(TiffMmapContext::new(mmap.clone()));

    unsafe {
        let c_path = path_to_tiff_name(path);
        let c_mode = match CString::new("r") {
            Ok(c) => c,
            Err(_) => return Err("Invalid mode string for C conversion".to_string()),
        };

        let tif_ptr = lib::TIFFClientOpen(
            c_path.as_ptr(),
            c_mode.as_ptr(),
            ctx.as_mut() as *mut TiffMmapContext as *mut c_void,
            tiff_read_proc,
            tiff_write_proc,
            tiff_seek_proc,
            tiff_close_proc,
            tiff_size_proc,
            tiff_map_proc,
            tiff_unmap_proc,
        );

        if tif_ptr.is_null() {
            return Err("TIFFClientOpen failed".to_string());
        }

        let handle = TiffHandle {
            guard: lib::TiffGuard::from_ptr(tif_ptr),
            _context: ctx,
        };

        let mut width: lib::uint32 = 0;
        let mut height: lib::uint32 = 0;
        lib::TIFFGetField(handle.as_ptr(), lib::TIFFTAG_IMAGEWIDTH, &mut width);
        lib::TIFFGetField(handle.as_ptr(), lib::TIFFTAG_IMAGELENGTH, &mut height);

        let mut bps: u16 = 0;
        lib::TIFFGetField(handle.as_ptr(), lib::TIFFTAG_BITSPERSAMPLE, &mut bps);

        if width == 0 || height == 0 {
            return Err("TIFF has zero width or height".to_string());
        }

        let mut photo: u16 = 0;
        let mut compression: u16 = 0;
        let mut orientation: u16 = 1;
        lib::TIFFGetField(handle.as_ptr(), lib::TIFFTAG_PHOTOMETRIC, &mut photo);
        lib::TIFFGetField(handle.as_ptr(), lib::TIFFTAG_COMPRESSION, &mut compression);
        lib::TIFFGetField(handle.as_ptr(), lib::TIFFTAG_ORIENTATION, &mut orientation);
        // TIFF orientation is defined as 1..=8; treat other values as identity.
        if !(1..=8).contains(&orientation) {
            orientation = 1;
        }

        let mut sample_format: u16 = lib::SAMPLEFORMAT_UINT;
        lib::TIFFGetField(
            handle.as_ptr(),
            lib::TIFFTAG_SAMPLEFORMAT,
            &mut sample_format,
        );
        let mut spp: u16 = 0;
        lib::TIFFGetField(handle.as_ptr(), lib::TIFFTAG_SAMPLESPERPIXEL, &mut spp);
        let mut planar_config: u16 = CONFIG_CONTIG;
        lib::TIFFGetField(
            handle.as_ptr(),
            lib::TIFFTAG_PLANARCONFIG,
            &mut planar_config,
        );

        let pixel_count_pre = width as u64 * height as u64;
        if tiff_ieee_scene_linear_eligible(sample_format, bps, photo, spp)
            && pixel_count_pre <= MAX_STATIC_HDR_DECODE_PIXELS
        {
            let spp_use = if spp == 0 { 1 } else { spp };
            match decode_ieee_scene_linear_rgba32f(IeeeSceneLinearDecodeArgs {
                tif: handle.as_ptr(),
                width,
                height,
                bps,
                spp: spp_use,
                photo,
                config: planar_config,
                cancel,
            }) {
                Ok(mut rgba_f32) => {
                    let mut w = width;
                    let mut h = height;
                    if orientation > 1 {
                        let (ow, oh, opix) =
                            apply_orientation_buffer_f32(rgba_f32, w, h, orientation);
                        w = ow;
                        h = oh;
                        rgba_f32 = opix;
                    }
                    let hdr = HdrImageBuffer {
                        width: w,
                        height: h,
                        format: HdrPixelFormat::Rgba32Float,
                        color_space: HdrColorSpace::LinearSrgb,
                        metadata: HdrImageMetadata {
                            transfer_function: HdrTransferFunction::Linear,
                            reference: HdrReference::SceneLinear,
                            color_profile: HdrColorProfile::LinearSrgb,
                            ..Default::default()
                        },
                        rgba_f32: Arc::new(rgba_f32),
                    };
                    let fallback = DecodedImage::from_hdr_sdr_fallback(
                        hdr.width,
                        hdr.height,
                        crate::loader::hdr_sdr_fallback_rgba8_or_placeholder(&hdr)?,
                    );
                    return Ok(ImageData::Hdr {
                        hdr: Box::new(hdr),
                        fallback,
                    });
                }
                Err(err) => {
                    log::debug!(
                        "[libtiff_loader] IEEE float HDR path skipped ({}), using standard decode",
                        err
                    );
                }
            }
        } else if tiff_ieee_scene_linear_eligible(sample_format, bps, photo, spp) {
            log::warn!(
                "[libtiff_loader] IEEE float TIFF exceeds static decode cap — falling back to 8-bit / tiled path"
            );
        }

        if tiff_uint16_rgb_scene_linear_eligible(sample_format, bps, photo, spp, planar_config)
            && pixel_count_pre <= MAX_STATIC_HDR_DECODE_PIXELS
            && !crate::loader::hdr_display_requests_sdr_preview(hdr_target_capacity)
        {
            let spp_use = if spp == 0 { 3 } else { spp };
            match decode_uint16_rgb_scene_linear_rgba32f(
                handle.as_ptr(),
                width,
                height,
                spp_use,
                cancel,
            ) {
                Ok(mut rgba_f32) => {
                    let mut w = width;
                    let mut h = height;
                    if orientation > 1 {
                        let (ow, oh, opix) =
                            apply_orientation_buffer_f32(rgba_f32, w, h, orientation);
                        w = ow;
                        h = oh;
                        rgba_f32 = opix;
                    }
                    let hdr = HdrImageBuffer {
                        width: w,
                        height: h,
                        format: HdrPixelFormat::Rgba32Float,
                        color_space: HdrColorSpace::LinearSrgb,
                        metadata: HdrImageMetadata {
                            transfer_function: HdrTransferFunction::Linear,
                            reference: HdrReference::SceneLinear,
                            color_profile: HdrColorProfile::LinearSrgb,
                            ..Default::default()
                        },
                        rgba_f32: Arc::new(rgba_f32),
                    };
                    let fallback = DecodedImage::from_hdr_sdr_fallback(
                        hdr.width,
                        hdr.height,
                        crate::loader::hdr_sdr_fallback_rgba8_or_placeholder(&hdr)?,
                    );
                    return Ok(ImageData::Hdr {
                        hdr: Box::new(hdr),
                        fallback,
                    });
                }
                Err(err) => {
                    log::debug!(
                        "[libtiff_loader] 16-bit RGB scene-linear HDR path skipped ({}), using standard decode",
                        err
                    );
                }
            }
        }

        if tiff_logl_logluv_hdr_eligible(photo, planar_config)
            && pixel_count_pre <= MAX_STATIC_HDR_DECODE_PIXELS
        {
            match decode_logl_logluv_scene_linear_rgba32f(
                LogLuvDecodeParams {
                    tif: handle.as_ptr(),
                    width,
                    height,
                    photo,
                    compression,
                    bps,
                    spp,
                    sample_format,
                },
                cancel,
            ) {
                Ok(mut rgba_f32) => {
                    let mut w = width;
                    let mut h = height;
                    if orientation > 1 {
                        let (ow, oh, opix) =
                            apply_orientation_buffer_f32(rgba_f32, w, h, orientation);
                        w = ow;
                        h = oh;
                        rgba_f32 = opix;
                    }
                    let hdr = HdrImageBuffer {
                        width: w,
                        height: h,
                        format: HdrPixelFormat::Rgba32Float,
                        color_space: HdrColorSpace::LinearSrgb,
                        metadata: HdrImageMetadata {
                            transfer_function: HdrTransferFunction::Linear,
                            reference: HdrReference::SceneLinear,
                            color_profile: HdrColorProfile::LinearSrgb,
                            ..Default::default()
                        },
                        rgba_f32: Arc::new(rgba_f32),
                    };
                    let fallback = DecodedImage::from_hdr_sdr_fallback(
                        hdr.width,
                        hdr.height,
                        crate::loader::hdr_sdr_fallback_rgba8_or_placeholder(&hdr)?,
                    );
                    return Ok(ImageData::Hdr {
                        hdr: Box::new(hdr),
                        fallback,
                    });
                }
                Err(err) => {
                    log::debug!(
                        "[libtiff_loader] LogL/LogLuv HDR path skipped ({}), using standard decode",
                        err
                    );
                }
            }
        }

        // Intercept formats that libtiff's RGBA interface fails to handle natively
        // 24/32/64-bit, 16-bit Grayscale, ThunderScan
        // LogL / LogLuv: scene-linear HDR path above when contiguous.
        let mut force_static = (bps != 8 && bps != 16)
            || (bps == 16 && (photo == PHOTO_MINISWHITE || photo == PHOTO_MINISBLACK))
            || (compression == COMPRESSION_THUNDERSCAN);

        let pixel_count = width as u64 * height as u64;

        // If orientation is complex (e.g. 90deg), force static decoding so we can rotate the full buffer,
        // UNLESS the image is huge (to prevent OOM). Most rotated images are from cameras and easily fit in static.
        // If it's a huge rotated image, it will fail static allocation limit and correctly fall back to WIC/ImageIO.
        if orientation > 1 && pixel_count <= MAX_STATIC_HDR_DECODE_PIXELS {
            force_static = true;
        }

        let is_large = crate::tile_cache::image_requires_tiled_plane(width, height);

        if !force_static && is_large {
            if lib::TIFFIsTiled(handle.as_ptr()) != 0 {
                let mut tile_width: lib::uint32 = 0;
                let mut tile_height: lib::uint32 = 0;
                lib::TIFFGetField(handle.as_ptr(), lib::TIFFTAG_TILEWIDTH, &mut tile_width);
                lib::TIFFGetField(handle.as_ptr(), lib::TIFFTAG_TILELENGTH, &mut tile_height);

                if tile_width == 0 || tile_height == 0 {
                    return Err("TIFF is tiled but tile dimensions are zero".to_string());
                }
                if tile_width > MAX_TIFF_TILE_DIMENSION || tile_height > MAX_TIFF_TILE_DIMENSION {
                    return Err(format!(
                        "TIFF tile dimensions {tile_width}x{tile_height} exceed maximum {MAX_TIFF_TILE_DIMENSION}"
                    ));
                }
                // TIFF allows edge tiles to extend past the image (padding). That remains legal.
                // Reject only clearly corrupt cases where a single tile exceeds the canvas on both
                // axes by more than the hard MAX_TIFF_TILE_DIMENSION bound already checked above
                // would not catch (e.g. 1x1 image with 8192x8192 tiles is still allowed by MAX).
                // Extra defense: if either tile side is larger than the image *and* the tile pixel
                // count exceeds the static decode budget, refuse tiled mode rather than OOM on one tile.
                let tile_pixels = (tile_width as u64).saturating_mul(tile_height as u64);
                if (tile_width > width || tile_height > height)
                    && tile_pixels > MAX_STATIC_HDR_DECODE_PIXELS
                {
                    return Err(format!(
                        "TIFF tile dimensions {tile_width}x{tile_height} exceed image {width}x{height} and pixel budget"
                    ));
                }

                // Validate tile offsets fit within the mmap (defense-in-depth).
                validate_tiff_offsets(handle.as_ptr(), &handle.guard, true, mmap.len())?;

                let tile_bytes = (tile_width as usize)
                    .checked_mul(tile_height as usize)
                    .and_then(|v| v.checked_mul(crate::constants::RGBA_CHANNELS));
                let max_cached =
                    if let Some(tile_bytes) = tile_bytes.and_then(std::num::NonZeroUsize::new) {
                        (TILE_CACHE_BUDGET_BYTES / tile_bytes.get()).max(16)
                    } else {
                        64
                    };

                return Ok(ImageData::Tiled(Arc::new(LibTiffTiledSource {
                    path: path.to_path_buf(),
                    mmap: mmap.clone(),
                    width,
                    height,
                    tile_width,
                    tile_height,
                    handle_pool: TiffHandlePool::new(handle),
                    tile_cache: Mutex::new(std::collections::HashMap::new()),
                    tile_lru: Mutex::new(crate::lru_order::LruOrder::default()),
                    max_cached_tiles: max_cached,
                })));
            } else {
                let rps =
                    super::rgba_buffer::tiff_effective_rows_per_strip(handle.as_ptr(), height);
                if rps == 0 {
                    return Err("TIFF strip height is zero".to_string());
                }

                // Validate strip offsets fit within the mmap (defense-in-depth).
                validate_tiff_offsets(handle.as_ptr(), &handle.guard, false, mmap.len())?;

                let strip_bytes = (width as usize)
                    .checked_mul(rps as usize)
                    // Same +width slack as [`tiff_rgba_strip_buffer_u32_count`]: libtiff strip
                    // RGBA decode can write one row past width*rps (see rgba_buffer.rs).
                    .and_then(|base| base.checked_add(width as usize))
                    .and_then(|pixels| pixels.checked_mul(std::mem::size_of::<lib::uint32>()));
                let max_cached =
                    if let Some(strip_bytes) = strip_bytes.and_then(std::num::NonZeroUsize::new) {
                        (STRIP_CACHE_BUDGET_BYTES / strip_bytes.get()).max(16)
                    } else {
                        64
                    };

                return Ok(ImageData::Tiled(Arc::new(LibTiffScanlineSource {
                    path: path.to_path_buf(),
                    mmap: mmap.clone(),
                    width,
                    height,
                    rows_per_strip: rps,
                    handle_pool: TiffHandlePool::new(handle),
                    cache: Mutex::new((
                        std::collections::HashMap::new(),
                        crate::lru_order::LruOrder::default(),
                    )),
                    max_cached_strips: max_cached,
                })));
            }
        }

        let Some(total_pixels) = (width as usize).checked_mul(height as usize) else {
            return Err(format!("Static TIFF dimension overflow ({width}x{height})"));
        };
        if total_pixels > MAX_STATIC_HDR_DECODE_PIXELS as usize {
            return Err("Static TIFF TOO LARGE for single pass decode".to_string());
        }
        let Some(rgba_byte_len) = total_pixels.checked_mul(4) else {
            return Err(format!(
                "Static TIFF RGBA buffer overflow ({width}x{height})"
            ));
        };

        // Try RGBA interface first (fast, handles color spaces)
        let mut bps: u16 = 0;
        lib::TIFFGetField(handle.as_ptr(), lib::TIFFTAG_BITSPERSAMPLE, &mut bps);

        let mut success = false;
        let mut pixels = Vec::new();

        // Try RGBA interface first ONLY if not forced static
        if !force_static {
            crate::loader::check_decode_cancel_str(cancel)?;
            pixels = vec![0u8; rgba_byte_len];
            // SAFETY: libtiff RGBA raster is native-endian u32 pixels; layout matches `Vec<u8>`.
            if lib::TIFFReadRGBAImageOriented(
                handle.as_ptr(),
                width,
                height,
                pixels.as_mut_ptr() as *mut lib::uint32,
                1,
                0,
            ) != 0
            {
                crate::loader::check_decode_cancel_str(cancel)?;
                success = true;
            } else {
                pixels.clear();
            }
        }

        if !success {
            // Fallback to manual scanline decode
            pixels = manual_decode_scanline(handle.as_ptr(), width, height, cancel)?;
        }

        if orientation > 1 {
            let (out_w, out_h, out_pixels) =
                apply_orientation_buffer(pixels, width, height, orientation);
            width = out_w;
            height = out_h;
            pixels = out_pixels;
        }

        if let Some(hdr) = try_camera_tiff_rgb8_hdr_upgrade(CameraTiffHdrUpgrade {
            file_bytes: mmap.as_ref(),
            hdr_target_capacity,
            tone_map: &tone_map,
            photo,
            bps,
            width,
            height,
            pixels: &pixels,
        })? {
            return Ok(hdr);
        }

        Ok(ImageData::Static(DecodedImage::new(width, height, pixels)))
    }
}
