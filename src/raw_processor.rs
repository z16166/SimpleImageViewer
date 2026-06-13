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

use image::DynamicImage;
use libraw_sys as ffi;
#[cfg(not(target_os = "windows"))]
use std::ffi::CString;
use std::path::Path;

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawDisplayMode {
    SdrDeveloped,
    SceneLinearHdr,
}

#[allow(dead_code)]
pub(crate) fn unpack_libraw_rgb16_rows_to_rgba_f32(
    rgb16_bytes: &[u8],
    width: u32,
    height: u32,
    row_stride: usize,
    bytes_per_pixel: usize,
) -> Result<Vec<f32>, String> {
    let tight_row_bytes = width as usize * bytes_per_pixel;
    let mut rgba_f32 =
        Vec::with_capacity(width as usize * height as usize * crate::constants::RGBA_CHANNELS);
    for y in 0..height as usize {
        let row_off = y * row_stride;
        let row_end = row_off + tight_row_bytes;
        let row = rgb16_bytes
            .get(row_off..row_end)
            .ok_or_else(|| rust_i18n::t!("error.buffer_size_mismatch").to_string())?;
        for px in row.chunks_exact(bytes_per_pixel) {
            if bytes_per_pixel < 6 {
                return Err(rust_i18n::t!("error.buffer_size_mismatch").to_string());
            }
            rgba_f32.push(u16::from_ne_bytes([px[0], px[1]]) as f32 / 65535.0);
            rgba_f32.push(u16::from_ne_bytes([px[2], px[3]]) as f32 / 65535.0);
            rgba_f32.push(u16::from_ne_bytes([px[4], px[5]]) as f32 / 65535.0);
            rgba_f32.push(1.0);
        }
    }
    Ok(rgba_f32)
}

pub fn raw_scene_linear_metadata() -> crate::hdr::types::HdrImageMetadata {
    crate::hdr::types::HdrImageMetadata {
        transfer_function: crate::hdr::types::HdrTransferFunction::Linear,
        reference: crate::hdr::types::HdrReference::SceneLinear,
        color_profile: crate::hdr::types::HdrColorProfile::from_color_space(
            crate::hdr::types::HdrColorSpace::LinearSrgb,
        ),
        luminance: crate::hdr::types::HdrLuminanceMetadata::default(),
        gain_map: None,
        raw_gpu_source: None,
    }
}

pub struct RawProcessor {
    data: *mut ffi::libraw_data_t,
    is_unpacked: bool,
}

/// RAII wrapper for memory allocated by LibRaw (e.g., via libraw_dcraw_make_mem_image).
struct LibRawMemory {
    ptr: *mut ffi::libraw_processed_image_t,
}

impl LibRawMemory {
    fn new(ptr: *mut ffi::libraw_processed_image_t) -> Option<Self> {
        if ptr.is_null() {
            None
        } else {
            Some(Self { ptr })
        }
    }

    fn as_ref(&self) -> &ffi::libraw_processed_image_t {
        unsafe { &*self.ptr }
    }
}

impl Drop for LibRawMemory {
    fn drop(&mut self) {
        unsafe {
            ffi::libraw_dcraw_clear_mem(self.ptr);
        }
    }
}

unsafe impl Send for RawProcessor {}

impl RawProcessor {
    pub fn new() -> Option<Self> {
        unsafe {
            let data = ffi::libraw_init(0);
            if data.is_null() {
                log::error!("{}", rust_i18n::t!("error.libraw_init"));
                None
            } else {
                Some(Self {
                    data,
                    is_unpacked: false,
                })
            }
        }
    }

    pub fn open<P: AsRef<Path>>(&mut self, path: P) -> Result<(), String> {
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::ffi::OsStrExt;
            let mut wide_path: Vec<u16> = path.as_ref().as_os_str().encode_wide().collect();
            wide_path.push(0);
            unsafe {
                let ret = ffi::libraw_open_wfile(self.data, wide_path.as_ptr());
                if ret != 0 {
                    return Err(rust_i18n::t!("error.libraw_open", code = ret).to_string());
                }
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            let path_str = path.as_ref().to_string_lossy();
            let c_path = CString::new(path_str.as_ref()).map_err(|_| "Invalid path")?;
            unsafe {
                let ret = ffi::libraw_open_file(self.data, c_path.as_ptr());
                if ret != 0 {
                    return Err(rust_i18n::t!("error.libraw_open", code = ret).to_string());
                }
            }
        }
        Ok(())
    }

    pub fn width(&self) -> u32 {
        unsafe { ffi::libraw_get_iwidth(self.data) as u32 }
    }

    pub fn height(&self) -> u32 {
        unsafe { ffi::libraw_get_iheight(self.data) as u32 }
    }

    pub fn flip(&self) -> i32 {
        unsafe { ffi::siv_libraw_get_flip(self.data) }
    }

    pub fn set_user_flip(&mut self, flip: i32) {
        unsafe { ffi::siv_libraw_set_user_flip(self.data, flip) }
    }

    /// CFA / sensor width from LibRaw (`raw_width`). May exceed [`Self::width`] when margins exist.
    pub fn raw_width(&self) -> u32 {
        unsafe { ffi::libraw_get_raw_width(self.data) as u32 }
    }

    /// CFA / sensor height from LibRaw (`raw_height`). May exceed [`Self::height`] when margins exist.
    pub fn raw_height(&self) -> u32 {
        unsafe { ffi::libraw_get_raw_height(self.data) as u32 }
    }

    pub fn is_supported_bayer(&self) -> bool {
        let filters = unsafe { ffi::siv_libraw_get_filters(self.data) };
        let colors = unsafe { ffi::siv_libraw_get_colors(self.data) };
        // filters == 0 means not a CFA image (e.g. Foveon, linear RGB)
        // filters == 1 means X-Trans (unsupported by our simple compute shader)
        // colors != 3 means not RGB Bayer
        filters > 1 && colors == 3
    }

    /// Best-effort developed output dimensions for tiling and HQ size checks.
    ///
    /// Some bodies (e.g. Epson ERF) report `iwidth`/`iheight` equal to the tiny embedded JPEG
    /// until demosaic; prefer CFA bounds when output size clearly matches the thumb only.
    pub fn developed_output_dimensions(
        &self,
        embedded: Option<&crate::loader::DecodedImage>,
    ) -> (u32, u32) {
        let iw = self.width();
        let ih = self.height();
        let rw = self.raw_width();
        let rh = self.raw_height();

        if let Some(p) = embedded {
            if p.width == iw && p.height == ih && ((rw > iw && rw > 0) || (rh > ih && rh > 0)) {
                return (rw.max(iw), rh.max(ih));
            }
        }

        if iw > 0 && ih > 0 { (iw, ih) } else { (rw, rh) }
    }

    pub fn unpack(&mut self) -> Result<(), String> {
        unsafe {
            let ret = ffi::libraw_unpack(self.data);
            if ret != 0 {
                return Err(rust_i18n::t!("error.libraw_unpack", code = ret).to_string());
            }
            self.is_unpacked = true;
        }
        Ok(())
    }

    pub fn extract_raw_gpu_source(
        &mut self,
        demosaic_method: crate::settings::RawDemosaicMethod,
    ) -> Result<crate::hdr::types::RawGpuSource, String> {
        if !self.is_unpacked {
            self.unpack()?;
        }

        let raw_pixels_ptr = unsafe { ffi::siv_libraw_get_raw_image(self.data) };
        if raw_pixels_ptr.is_null() {
            return Err("Raw sensor pixel buffer is null".to_string());
        }

        let rw = self.raw_width();
        let rh = self.raw_height();
        let w = self.width();
        let h = self.height();

        let mut left_margin = 0;
        let mut top_margin = 0;
        unsafe {
            ffi::siv_libraw_get_margins(self.data, &mut left_margin, &mut top_margin);
        }

        // Validate bounds
        if left_margin < 0
            || top_margin < 0
            || (left_margin as u32 + w) > rw
            || (top_margin as u32 + h) > rh
        {
            return Err("Margins and active area exceed raw image boundaries".to_string());
        }

        // Copy and crop raw pixels row by row
        let total_pixels = w as usize * h as usize;
        let mut cropped_pixels = Vec::with_capacity(total_pixels);
        unsafe {
            let slice = std::slice::from_raw_parts(raw_pixels_ptr, (rw * rh) as usize);
            for r in 0..h {
                let src_row_offset = ((top_margin as u32 + r) * rw + left_margin as u32) as usize;
                cropped_pixels
                    .extend_from_slice(&slice[src_row_offset..src_row_offset + w as usize]);
            }
        }

        // Query colors, filters, and params
        let p00 =
            unsafe { ffi::siv_libraw_get_color_at(self.data, top_margin, left_margin) } as u32;
        let p01 =
            unsafe { ffi::siv_libraw_get_color_at(self.data, top_margin, left_margin + 1) } as u32;
        let p10 =
            unsafe { ffi::siv_libraw_get_color_at(self.data, top_margin + 1, left_margin) } as u32;
        let p11 =
            unsafe { ffi::siv_libraw_get_color_at(self.data, top_margin + 1, left_margin + 1) }
                as u32;
        let bayer_pattern = [p00, p01, p10, p11];

        let mut cam_mul = [0.0f32; 4];
        let mut cblack = [0.0f32; 4];
        let mut black = 0;
        let mut maximum = 0;
        unsafe {
            ffi::siv_libraw_get_color_params(
                self.data,
                cam_mul.as_mut_ptr(),
                cblack.as_mut_ptr(),
                &mut black,
                &mut maximum,
            );
        }

        let mut black_level = [0.0f32; 4];
        for i in 0..4 {
            black_level[i] = if cblack[i] > 0.0 {
                cblack[i]
            } else {
                black as f32
            };
        }

        // Normalize cam_mul by green channel (cam_mul[1]) to prevent excessive scaling.
        let green_gain = cam_mul[1].max(1e-6);
        for i in 0..4 {
            cam_mul[i] /= green_gain;
        }

        log::debug!(
            "[Loader] RAW GPU source extraction parameters: maximum={}, black_level={:?}, cam_mul={:?}, bayer_pattern={:?}",
            maximum,
            black_level,
            cam_mul,
            bayer_pattern
        );

        Ok(crate::hdr::types::RawGpuSource {
            raw_width: rw,
            raw_height: rh,
            width: w,
            height: h,
            raw_pixels: std::sync::Arc::new(cropped_pixels),
            black_level,
            cam_mul,
            maximum: maximum as f32,
            bayer_pattern,
            demosaic_method,
            bootstrap_preview: None,
        })
    }

    #[allow(dead_code)]
    pub fn set_use_camera_matrix(&mut self, value: i32) {
        unsafe { ffi::siv_libraw_set_use_camera_matrix(self.data, value) }
    }

    #[allow(dead_code)]
    pub fn set_auto_bright_thr(&mut self, value: f32) {
        unsafe { ffi::siv_libraw_set_auto_bright_thr(self.data, value) }
    }

    pub fn develop(&mut self) -> Result<DynamicImage, String> {
        if !self.is_unpacked {
            self.unpack()?;
        }

        unsafe {
            ffi::libraw_set_output_bps(self.data, 8);
            ffi::siv_libraw_set_use_camera_wb(self.data, 1);
            ffi::siv_libraw_set_use_camera_matrix(self.data, 1);
            ffi::libraw_set_no_auto_bright(self.data, 0);
            ffi::siv_libraw_set_auto_bright_thr(self.data, crate::constants::RAW_AUTO_BRIGHT_THR);

            let ret = ffi::libraw_dcraw_process(self.data);
            if ret != 0 {
                return Err(rust_i18n::t!("error.libraw_process", code = ret).to_string());
            }

            let mut err = 0;
            let processed =
                LibRawMemory::new(ffi::libraw_dcraw_make_mem_image(self.data, &mut err))
                    .ok_or_else(|| {
                        rust_i18n::t!("error.libraw_mem_image", code = err).to_string()
                    })?;

            let img = processed.as_ref();
            if img.image_type != ffi::LibRaw_image_formats::LIBRAW_IMAGE_BITMAP as u32 {
                return Err(rust_i18n::t!(
                    "error.unsupported_raw_type",
                    img_type = img.image_type,
                    expected = ffi::LibRaw_image_formats::LIBRAW_IMAGE_BITMAP as u32
                )
                .to_string());
            }

            if img.colors != crate::constants::RGB_CHANNELS as u16
                || img.bits != crate::constants::BIT_DEPTH_8 as u16
            {
                return Err(rust_i18n::t!(
                    "error.unsupported_raw_format",
                    colors = img.colors,
                    bits = img.bits
                )
                .to_string());
            }

            let width = img.width as u32;
            let height = img.height as u32;
            let data_ptr = img.data.as_ptr();
            let data_len = img.data_size as usize;

            if data_ptr.is_null() || data_len == 0 {
                return Err(rust_i18n::t!("error.libraw_mem_image", code = -1).to_string());
            }

            let expected_min = width as usize * height as usize * crate::constants::RGB_CHANNELS;
            if data_len < expected_min {
                return Err(rust_i18n::t!("error.buffer_size_mismatch").to_string());
            }

            // SINGLE-PASS PACKING OPTIMIZATION:
            let mut rgba = vec![
                crate::constants::MAX_CHANNEL_VALUE;
                width as usize * height as usize * crate::constants::RGBA_CHANNELS
            ];
            let slice = std::slice::from_raw_parts(data_ptr, expected_min);

            simple_image_viewer::simd_swizzle::interleave_rgb_packed_to_rgba_packed(
                slice, &mut rgba,
            );

            let rgba_img = image::RgbaImage::from_raw(width, height, rgba)
                .ok_or_else(|| rust_i18n::t!("error.rgb_image_create_failed").to_string())?;

            Ok(DynamicImage::ImageRgba8(rgba_img))
        }
    }

    pub fn develop_scene_linear_hdr(
        &mut self,
    ) -> Result<crate::hdr::types::HdrImageBuffer, String> {
        if !self.is_unpacked {
            self.unpack()?;
        }

        unsafe {
            ffi::libraw_set_output_bps(self.data, 16);
            ffi::siv_libraw_set_use_camera_wb(self.data, 1);
            ffi::siv_libraw_set_use_camera_matrix(self.data, 1);
            ffi::siv_libraw_set_output_color(self.data, 1); // sRGB primaries, with linear gamma below.
            ffi::libraw_set_no_auto_bright(self.data, 0);
            ffi::siv_libraw_set_auto_bright_thr(self.data, crate::constants::RAW_AUTO_BRIGHT_THR);
            ffi::siv_libraw_set_gamma(self.data, 1.0, 1.0);

            let ret = ffi::libraw_dcraw_process(self.data);
            if ret != 0 {
                return Err(rust_i18n::t!("error.libraw_process", code = ret).to_string());
            }

            let mut err = 0;
            let processed =
                LibRawMemory::new(ffi::libraw_dcraw_make_mem_image(self.data, &mut err))
                    .ok_or_else(|| {
                        rust_i18n::t!("error.libraw_mem_image", code = err).to_string()
                    })?;

            let img = processed.as_ref();
            if img.image_type != ffi::LibRaw_image_formats::LIBRAW_IMAGE_BITMAP as u32 {
                return Err(rust_i18n::t!(
                    "error.unsupported_raw_type",
                    img_type = img.image_type,
                    expected = ffi::LibRaw_image_formats::LIBRAW_IMAGE_BITMAP as u32
                )
                .to_string());
            }
            if img.colors != crate::constants::RGB_CHANNELS as u16 || img.bits != 16 {
                return Err(rust_i18n::t!(
                    "error.unsupported_raw_format",
                    colors = img.colors,
                    bits = img.bits
                )
                .to_string());
            }

            let width = img.width as u32;
            let height = img.height as u32;
            let data_ptr = img.data.as_ptr();
            let data_len = img.data_size as usize;
            let colors = img.colors as usize;
            let bytes_per_sample = (img.bits as usize) / 8;
            let bytes_per_pixel = colors * bytes_per_sample;
            let tight_row_bytes = width as usize * bytes_per_pixel;
            let tight_size = tight_row_bytes * height as usize;
            if data_ptr.is_null() || data_len < tight_size || bytes_per_pixel == 0 {
                return Err(rust_i18n::t!("error.buffer_size_mismatch").to_string());
            }

            let row_stride = if height > 0 {
                data_len / height as usize
            } else {
                tight_row_bytes
            };
            if row_stride < tight_row_bytes {
                return Err(rust_i18n::t!("error.buffer_size_mismatch").to_string());
            }

            let rgb16_bytes = std::slice::from_raw_parts(data_ptr, data_len);
            let rgba_f32 = unpack_libraw_rgb16_rows_to_rgba_f32(
                rgb16_bytes,
                width,
                height,
                row_stride,
                bytes_per_pixel,
            )?;

            let metadata = raw_scene_linear_metadata();
            Ok(crate::hdr::types::HdrImageBuffer {
                width,
                height,
                format: crate::hdr::types::HdrPixelFormat::Rgba32Float,
                color_space: metadata.color_space_hint(),
                metadata,
                rgba_f32: std::sync::Arc::new(rgba_f32),
            })
        }
    }

    pub fn unpack_thumb(&mut self) -> Result<crate::loader::DecodedImage, String> {
        let mut err = 0;
        unsafe {
            let res = ffi::libraw_unpack_thumb(self.data);
            if res != 0 {
                return Err(rust_i18n::t!("error.libraw_unpack", code = res).to_string());
            }

            let processed =
                LibRawMemory::new(ffi::libraw_dcraw_make_mem_thumb(self.data, &mut err))
                    .ok_or_else(|| {
                        rust_i18n::t!("error.libraw_mem_image", code = err).to_string()
                    })?;

            let img = processed.as_ref();
            let data_ptr = img.data.as_ptr();
            let data_size = img.data_size as usize;

            if data_ptr.is_null() || data_size == 0 {
                return Err(rust_i18n::t!("error.libraw_mem_image", code = -2).to_string());
            }

            let slice = std::slice::from_raw_parts(data_ptr, data_size);

            if img.image_type == ffi::LibRaw_image_formats::LIBRAW_IMAGE_JPEG as u32 {
                // JPEG thumbnail
                match image::load_from_memory(slice) {
                    Ok(decoded) => {
                        let rgba = decoded.into_rgba8();
                        Ok(crate::loader::DecodedImage::new(
                            rgba.width(),
                            rgba.height(),
                            rgba.into_raw(),
                        ))
                    }
                    Err(e) => Err(rust_i18n::t!("error.decode_thumb_failed", err = e).to_string()),
                }
            } else if img.image_type == ffi::LibRaw_image_formats::LIBRAW_IMAGE_BITMAP as u32 {
                // Bitmap thumbnail (RGB)
                if img.colors == crate::constants::RGB_CHANNELS as u16
                    && img.bits == crate::constants::BIT_DEPTH_8 as u16
                {
                    let count = img.width as usize * img.height as usize;
                    let mut rgba = vec![
                        crate::constants::MAX_CHANNEL_VALUE;
                        count * crate::constants::RGBA_CHANNELS
                    ];

                    if let Some(rgb) = image::RgbImage::from_raw(
                        img.width as u32,
                        img.height as u32,
                        slice.to_vec(),
                    ) {
                        let rgba_img = image::DynamicImage::ImageRgb8(rgb).into_rgba8();
                        Ok(crate::loader::DecodedImage::new(
                            img.width as u32,
                            img.height as u32,
                            rgba_img.into_raw(),
                        ))
                    } else {
                        // Fallback to manual if RgbImage::from_raw fails (shouldn't happen)
                        for i in 0..count {
                            rgba[i * crate::constants::RGBA_CHANNELS] =
                                slice[i * crate::constants::RGB_CHANNELS];
                            rgba[i * crate::constants::RGBA_CHANNELS + 1] =
                                slice[i * crate::constants::RGB_CHANNELS + 1];
                            rgba[i * crate::constants::RGBA_CHANNELS + 2] =
                                slice[i * crate::constants::RGB_CHANNELS + 2];
                        }
                        Ok(crate::loader::DecodedImage::new(
                            img.width as u32,
                            img.height as u32,
                            rgba,
                        ))
                    }
                } else {
                    // Heuristic fallback: Some cameras (like Fuji) might report a thumbnail as
                    // a bitmap type but actually embed a JPEG, or report bits/colors as 0.
                    if slice.len() > crate::constants::RGB_CHANNELS
                        && slice[0] == 0xFF
                        && slice[1] == 0xD8
                        && slice[2] == 0xFF
                    {
                        match image::load_from_memory(slice) {
                            Ok(decoded) => {
                                let rgba = decoded.into_rgba8();
                                Ok(crate::loader::DecodedImage::new(
                                    rgba.width(),
                                    rgba.height(),
                                    rgba.into_raw(),
                                ))
                            }
                            Err(e) => {
                                Err(rust_i18n::t!("error.heuristic_jpeg_failed", err = e)
                                    .to_string())
                            }
                        }
                    } else {
                        Err(rust_i18n::t!(
                            "error.unsupported_thumb_format",
                            colors = img.colors,
                            bits = img.bits,
                            img_type = img.image_type
                        )
                        .to_string())
                    }
                }
            } else {
                Err(
                    rust_i18n::t!("error.unknown_thumb_type", img_type = img.image_type)
                        .to_string(),
                )
            }
        }
    }

    pub fn process_warnings(&self) -> u32 {
        unsafe { ffi::siv_libraw_get_process_warnings(self.data) }
    }
}

impl Drop for RawProcessor {
    fn drop(&mut self) {
        unsafe {
            ffi::libraw_close(self.data);
        }
    }
}

pub fn version() -> String {
    ffi::version()
}

pub const RAW_EXTENSIONS: &[&str] = &[
    "crw", "cr2", "cr3", // Canon
    "nef", "nrw", "nrv", // Nikon
    "arw", "srf", "sr2", "sr1", "sr",  // Sony
    "raf", // Fujifilm
    "orf", "ori", "obm", // Olympus
    "rw2", "raw", // Panasonic
    "pef", "ptx", "pkx", // Pentax
    "3fr", "fff", // Hasselblad
    "iiq", "cap", "eip", // Phase One
    "dcr", "dcs", "drf", "k25", "kdc", "kqc", "kc2", // Kodak
    "rwl", "dng", // Leica (dng is shared, listed generically below too)
    "srw", // Samsung
    "x3f", // Sigma
    "mos", "mef", "mfw", // Leaf / Mamiya
    "erf", // Epson
    "gpr", // GoPro
    "rw1", "j6i", // Ricoh
    "bay", "cam", // Casio
    "ari", // ARRI
    "r3d", // RED
    "stx", "sti", // Sinar
    "pxn", // Logitech
    "mrw", "mdc", // Minolta
    "dng", "rwz", "cxi", "fpix", "rdc", "qtk", // Generic / Other (rawzor, foveon, etc)
];

pub fn is_raw_extension(ext: &str) -> bool {
    RAW_EXTENSIONS
        .iter()
        .any(|raw_ext| raw_ext.eq_ignore_ascii_case(ext))
}

/// LibRaw identifies camera RAW by file content, not extension. Some vendors (e.g. Kodak DCS)
/// store RAW in `.tif` containers; probe before the generic TIFF decoder so we demosaic IFD0
/// instead of showing a tiny embedded RGB preview IFD.
pub fn probe_libraw_can_open(path: &Path) -> bool {
    let mut processor = match RawProcessor::new() {
        Some(p) => p,
        None => return false,
    };
    if processor.open(path).is_err() {
        return false;
    }
    let w = processor.width();
    let h = processor.height();
    w > 0 && h > 0
}

#[cfg(test)]
mod tests {
    use super::{
        RawDisplayMode, RawProcessor, is_raw_extension, probe_libraw_can_open,
        raw_scene_linear_metadata, unpack_libraw_rgb16_rows_to_rgba_f32,
    };
    use crate::hdr::types::{HdrReference, HdrTransferFunction};
    use std::path::Path;

    #[test]
    fn unpack_libraw_rgb16_respects_row_stride_padding() {
        // Two RGB pixels per row; LibRaw pads each row to 16 bytes (12 + 4).
        let mut data = vec![0_u8; 32];
        for row in 0..2 {
            let base = row * 16;
            data[base] = 0xFF;
            data[base + 1] = 0xFF; // R
            data[base + 8] = 0xFF;
            data[base + 9] = 0xFF; // G of pixel 2
        }
        let rgba = unpack_libraw_rgb16_rows_to_rgba_f32(&data, 2, 2, 16, 6).expect("unpack");
        assert_eq!(rgba.len(), 2 * 2 * 4);
        assert!((rgba[0] - 1.0).abs() < 0.01); // row0 px0 R
        assert!((rgba[5] - 1.0).abs() < 0.01); // row0 px1 G
        assert!((rgba[8] - 1.0).abs() < 0.01); // row1 px0 R (after stride skip)
        assert!((rgba[13] - 1.0).abs() < 0.01); // row1 px1 G
    }

    #[test]
    fn raw_scene_linear_metadata_enters_hdr_pipeline_as_linear_scene_data() {
        let metadata = raw_scene_linear_metadata();

        assert_eq!(metadata.transfer_function, HdrTransferFunction::Linear);
        assert_eq!(metadata.reference, HdrReference::SceneLinear);
    }

    #[test]
    fn raw_display_mode_defaults_to_existing_sdr_developed_behavior() {
        let mode = RawDisplayMode::SdrDeveloped;

        assert_eq!(mode, RawDisplayMode::SdrDeveloped);
    }

    #[test]
    fn tif_extension_is_not_treated_as_raw_by_extension_alone() {
        assert!(!is_raw_extension("tif"));
        assert!(!is_raw_extension("tiff"));
    }

    #[test]
    fn probe_libraw_can_open_false_for_missing_file() {
        assert!(!probe_libraw_can_open(Path::new(
            "definitely_missing_kodak_dcs460d.tif"
        )));
    }

    fn luminance_stats_rgba8(pixels: &[u8]) -> (f64, f64, f64, u8) {
        let mut r_sum = 0u64;
        let mut g_sum = 0u64;
        let mut b_sum = 0u64;
        let mut max = 0u8;
        let mut n = 0u64;
        for chunk in pixels.chunks_exact(4) {
            r_sum += chunk[0] as u64;
            g_sum += chunk[1] as u64;
            b_sum += chunk[2] as u64;
            max = max.max(chunk[0]).max(chunk[1]).max(chunk[2]);
            n += 1;
        }
        if n == 0 {
            return (0.0, 0.0, 0.0, 0);
        }
        (
            r_sum as f64 / n as f64,
            g_sum as f64 / n as f64,
            b_sum as f64 / n as f64,
            max,
        )
    }

    #[test]
    #[ignore]
    fn probe_legacy_raw_hdr_paths() {
        let samples = [
            ("aptus75", Path::new(r"F:\win7\raws\leaf\RAW_APTUS_75.MOS")),
            (
                "aptus22",
                Path::new(r"F:\win7\raws\leaf\aptus22\RAW_LEAF_APTUS_22.MOS"),
            ),
            (
                "mamiya_zd",
                Path::new(r"F:\win7\raws\mamiya\zd\RAW_MAMIYA_ZD.MEF"),
            ),
            (
                "nikon1_v1",
                Path::new(r"F:\win7\raws\nikon\RAW_NIKON1_V1.NEF"),
            ),
        ];
        for (label, path) in samples {
            if !path.is_file() {
                eprintln!("skip {label}: {}", path.display());
                continue;
            }
            let mut processor = RawProcessor::new().expect("libraw init");
            processor.open(path).expect("libraw open");
            let w = processor.width();
            let h = processor.height();
            eprintln!(
                "{label}: libraw {w}x{h} ({:.1} MP)",
                (w as f64 * h as f64) / 1e6
            );

            let mut thumb_processor = RawProcessor::new().expect("libraw init");
            thumb_processor.open(path).expect("libraw open");
            if let Ok(thumb) = thumb_processor.unpack_thumb() {
                let (r, g, b, max) = luminance_stats_rgba8(thumb.rgba());
                eprintln!(
                    "{label}: unpack_thumb {}x{} avg=({r:.1},{g:.1},{b:.1}) max={max}",
                    thumb.width, thumb.height
                );
            } else {
                eprintln!("{label}: unpack_thumb failed");
            }

            let sdr = processor.develop().expect("develop");
            let rgba = sdr.to_rgba8();
            let (r, g, b, max) = luminance_stats_rgba8(rgba.as_raw());
            eprintln!(
                "{label}: develop avg=({r:.1},{g:.1},{b:.1}) max={max} size={}x{}",
                rgba.width(),
                rgba.height()
            );
            assert!(max > 0, "{label}: develop produced all-black image");

            let mut hdr_processor = RawProcessor::new().expect("libraw init");
            hdr_processor.open(path).expect("libraw open");
            let hdr = hdr_processor
                .develop_scene_linear_hdr()
                .expect("develop_scene_linear_hdr");
            let mut max_l = 0.0f32;
            for px in hdr.rgba_f32.chunks_exact(4) {
                let l = 0.2126 * px[0] + 0.7152 * px[1] + 0.0722 * px[2];
                max_l = max_l.max(l);
            }
            eprintln!("{label}: scene_linear max_l={max_l:.6}");

            for cap in [1.0_f32, 4.0_f32] {
                let mut tone_processor = RawProcessor::new().expect("libraw init");
                tone_processor.open(path).expect("libraw open");
                let hdr = tone_processor
                    .develop_scene_linear_hdr()
                    .expect("develop_scene_linear_hdr");
                let fallback = crate::loader::hdr_sdr_fallback_rgba8_eager_or_placeholder(
                    &hdr,
                    cap,
                    &crate::hdr::types::HdrToneMapSettings::default(),
                )
                .expect("sdr fallback");
                let (r, g, b, max) = luminance_stats_rgba8(fallback.as_ref());
                eprintln!("{label}: sdr_fallback cap={cap} avg=({r:.1},{g:.1},{b:.1}) max={max}");
                assert!(max > 0, "{label}: sdr_fallback cap={cap} must not be black");
            }
            assert!(max_l > 0.0, "{label}: scene linear HDR is all zero");
        }
    }

    /// Requires `F:\win7\raws\kodak\RAW_KODAK_DCS460D_FILEVERSION_3.TIF` on the test machine.
    #[test]
    #[ignore]
    fn probe_libraw_can_open_kodak_dcs460d_tif() {
        let path = Path::new(r"F:\win7\raws\kodak\RAW_KODAK_DCS460D_FILEVERSION_3.TIF");
        if !path.is_file() {
            eprintln!(
                "skip: Kodak DCS460D sample not present at {}",
                path.display()
            );
            return;
        }
        assert!(
            probe_libraw_can_open(path),
            "LibRaw should recognize Kodak DCS460D TIFF container as camera RAW"
        );
        let mut processor = RawProcessor::new().expect("libraw init");
        processor.open(path).expect("libraw open");
        assert!(
            processor.width() > 256 && processor.height() > 256,
            "expected full sensor dimensions, got {}x{}",
            processor.width(),
            processor.height()
        );
    }
}

pub fn get_supported_extensions() -> Vec<String> {
    // According to LibRaw's design, identification is based on Magic Numbers,
    // not file extensions. For UI filtering purposes, we use this comprehensive
    // list of common professional RAW formats.
    RAW_EXTENSIONS.iter().map(|s| s.to_string()).collect()
}
