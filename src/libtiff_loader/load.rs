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
    CameraTiffHdrUpgrade, LogLuvDecodeParams, decode_ieee_scene_linear_rgba32f,
    decode_logl_logluv_scene_linear_rgba32f, decode_uint16_rgb_scene_linear_rgba32f,
    tiff_ieee_scene_linear_eligible, tiff_logl_logluv_hdr_eligible,
    tiff_uint16_rgb_scene_linear_eligible, try_camera_tiff_rgb8_hdr_upgrade,
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

use super::handle::{TiffHandle, path_to_tiff_name};
use super::mmap::{
    TiffMmapContext, tiff_close_proc, tiff_map_proc, tiff_read_proc, tiff_seek_proc,
    tiff_size_proc, tiff_unmap_proc, tiff_write_proc,
};
use super::orientation::{apply_orientation_buffer, apply_orientation_buffer_f32};
use crate::loader::{DecodedImage, ImageData};

/// IFD0 tags for diagnostics (tests / support).
#[cfg(test)]
pub fn peek_tiff_tags(path: &Path) -> Result<String, String> {
    let mmap = Arc::new(crate::mmap_util::map_file(path)?);
    let mut ctx = Box::new(TiffMmapContext { mmap, offset: 0 });
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

pub fn load_via_libtiff(
    path: &Path,
    hdr_target_capacity: f32,
    tone_map: HdrToneMapSettings,
) -> Result<ImageData, String> {
    let mmap = Arc::new(crate::mmap_util::map_file(path)?);
    load_via_libtiff_from_mmap(path, mmap, hdr_target_capacity, tone_map)
}

pub(crate) fn load_via_libtiff_from_mmap(
    path: &Path,
    mmap: Arc<memmap2::Mmap>,
    hdr_target_capacity: f32,
    tone_map: HdrToneMapSettings,
) -> Result<ImageData, String> {
    let mut ctx = Box::new(TiffMmapContext {
        mmap: mmap.clone(),
        offset: 0,
    });

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
            && pixel_count_pre <= 256 * 1024 * 1024
        {
            let spp_use = if spp == 0 { 1 } else { spp };
            match decode_ieee_scene_linear_rgba32f(
                handle.as_ptr(),
                width,
                height,
                bps,
                spp_use,
                photo,
                planar_config,
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
            && pixel_count_pre <= 256 * 1024 * 1024
            && !crate::loader::hdr_display_requests_sdr_preview(hdr_target_capacity)
        {
            let spp_use = if spp == 0 { 3 } else { spp };
            match decode_uint16_rgb_scene_linear_rgba32f(handle.as_ptr(), width, height, spp_use) {
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
            && pixel_count_pre <= 256 * 1024 * 1024
        {
            match decode_logl_logluv_scene_linear_rgba32f(LogLuvDecodeParams {
                tif: handle.as_ptr(),
                width,
                height,
                photo,
                compression,
                bps,
                spp,
                sample_format,
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
        if orientation > 1 && pixel_count <= 256 * 1024 * 1024 {
            force_static = true;
        }

        let pixel_count = width as u64 * height as u64;
        let limit = crate::tile_cache::get_max_texture_side();
        let tiled_threshold =
            crate::tile_cache::TILED_THRESHOLD.load(std::sync::atomic::Ordering::Relaxed);
        let is_large = pixel_count >= tiled_threshold || width > limit || height > limit;

        if !force_static && is_large {
            if lib::TIFFIsTiled(handle.as_ptr()) != 0 {
                let mut tile_width: lib::uint32 = 0;
                let mut tile_height: lib::uint32 = 0;
                lib::TIFFGetField(handle.as_ptr(), lib::TIFFTAG_TILEWIDTH, &mut tile_width);
                lib::TIFFGetField(handle.as_ptr(), lib::TIFFTAG_TILELENGTH, &mut tile_height);

                if tile_width == 0 || tile_height == 0 {
                    return Err("TIFF is tiled but tile dimensions are zero".to_string());
                }

                return Ok(ImageData::Tiled(Arc::new(LibTiffTiledSource {
                    path: path.to_path_buf(),
                    mmap: mmap.clone(),
                    width,
                    height,
                    tile_width,
                    tile_height,
                    pool: Mutex::new(vec![handle]),
                })));
            } else {
                let mut rps: lib::uint32 = 0;
                if lib::TIFFGetField(handle.as_ptr(), lib::TIFFTAG_ROWSPERSTRIP, &mut rps) == 0
                    || rps == 0
                {
                    rps = height;
                }

                let strip_bytes = width as usize * rps as usize * 4;
                let max_cached = if let Some(strip_bytes) = std::num::NonZeroUsize::new(strip_bytes)
                {
                    (256 * 1024 * 1024 / strip_bytes.get()).max(16)
                } else {
                    64
                };

                return Ok(ImageData::Tiled(Arc::new(LibTiffScanlineSource {
                    path: path.to_path_buf(),
                    mmap: mmap.clone(),
                    width,
                    height,
                    rows_per_strip: rps,
                    pool: Mutex::new(vec![handle]),
                    strip_cache: Mutex::new(std::collections::HashMap::new()),
                    cache_order: Mutex::new(Vec::new()),
                    max_cached_strips: max_cached,
                })));
            }
        }

        let total_pixels = (width as usize) * (height as usize);
        if total_pixels > 256 * 1024 * 1024 {
            return Err("Static TIFF TOO LARGE for single pass decode".to_string());
        }

        // Try RGBA interface first (fast, handles color spaces)
        let mut bps: u16 = 0;
        lib::TIFFGetField(handle.as_ptr(), lib::TIFFTAG_BITSPERSAMPLE, &mut bps);

        let mut success = false;
        let mut pixels = Vec::new();

        // Try RGBA interface first ONLY if not forced static
        if !force_static {
            let mut raster: Vec<lib::uint32> = vec![0; total_pixels];
            if lib::TIFFReadRGBAImageOriented(
                handle.as_ptr(),
                width,
                height,
                raster.as_mut_ptr(),
                1,
                0,
            ) != 0
            {
                pixels = vec![0u8; total_pixels * 4];
                std::ptr::copy_nonoverlapping(
                    raster.as_ptr() as *const u8,
                    pixels.as_mut_ptr(),
                    pixels.len(),
                );
                success = true;
            }
        }

        if !success {
            // Fallback to manual scanline decode
            pixels = manual_decode_scanline(handle.as_ptr(), width, height)?;
        }

        if orientation > 1 {
            let (out_w, out_h, out_pixels) =
                apply_orientation_buffer(pixels, width, height, orientation);
            width = out_w;
            height = out_h;
            pixels = out_pixels;
        }

        if let Some(hdr) = try_camera_tiff_rgb8_hdr_upgrade(CameraTiffHdrUpgrade {
            path,
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
