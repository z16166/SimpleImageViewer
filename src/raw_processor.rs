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

use libraw_sys_msvc as ffi;
use std::path::Path;
use std::ffi::CString;
use image::{DynamicImage, RgbImage};

pub struct RawProcessor {
    data: *mut ffi::libraw_data_t,
    is_unpacked: bool,
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
                Some(Self { data, is_unpacked: false })
            }
        }
    }

    pub fn open<P: AsRef<Path>>(&mut self, path: P) -> Result<(), String> {
        let path_str = path.as_ref().to_string_lossy();
        let c_path = CString::new(path_str.as_ref()).map_err(|_| "Invalid path")?;
        
        unsafe {
            let ret = ffi::libraw_open_file(self.data, c_path.as_ptr());
            if ret != 0 {
                return Err(rust_i18n::t!("error.libraw_open", code = ret).to_string());
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

    #[allow(dead_code)]
    pub fn raw_width(&self) -> u32 {
        unsafe { ffi::libraw_get_raw_width(self.data) as u32 }
    }

    #[allow(dead_code)]
    pub fn raw_height(&self) -> u32 {
        unsafe { ffi::libraw_get_raw_height(self.data) as u32 }
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

    pub fn develop(&mut self) -> Result<DynamicImage, String> {
        if !self.is_unpacked {
            self.unpack()?;
        }

        unsafe {
            // Set output parameters for better colors
            ffi::libraw_set_output_bps(self.data, 8);
            ffi::libraw_set_use_camera_wb(self.data, 1);
            ffi::libraw_set_auto_bright(self.data, 1);
            
            // Standard development
            let ret = ffi::libraw_dcraw_process(self.data);
            if ret != 0 {
                return Err(rust_i18n::t!("error.libraw_process", code = ret).to_string());
            }

            let mut err = 0;
            let processed = ffi::libraw_dcraw_make_mem_image(self.data, &mut err);
            if processed.is_null() {
                return Err(rust_i18n::t!("error.libraw_mem_image", code = err).to_string());
            }

            let img = &*processed;
            if img.image_type != ffi::LibRaw_image_formats::LIBRAW_IMAGE_BITMAP as u32 {
                ffi::libraw_dcraw_clear_mem(processed);
                return Err(rust_i18n::t!("error.unsupported_raw_type", img_type = img.image_type, expected = ffi::LibRaw_image_formats::LIBRAW_IMAGE_BITMAP as u32).to_string());
            }

            if img.colors != crate::constants::RGB_CHANNELS as u16 || img.bits != crate::constants::BIT_DEPTH_8 as u16 {
                ffi::libraw_dcraw_clear_mem(processed);
                return Err(rust_i18n::t!("error.unsupported_raw_format", colors = img.colors, bits = img.bits).to_string());
            }

            // Create RgbImage from the data
            let width = img.width as u32;
            let height = img.height as u32;
            let data_ptr = &img.data as *const u8;
            
            // CRITICAL FIX: Use the actual data_size returned by LibRaw.
            // Manually calculating (width * height * 3) can fail if LibRaw includes 
            // padding or alignment bytes at the end of the buffer.
            let data_len = img.data_size as usize;
            
            log::debug!(
                "[RawProcessor] FFI develop: ptr={:p}, size={}, dim={}x{}, colors={}, bits={}",
                data_ptr, data_len, width, height, img.colors, img.bits
            );

            if data_ptr.is_null() || data_len == 0 {
                ffi::libraw_dcraw_clear_mem(processed);
                return Err(rust_i18n::t!("error.libraw_mem_image", code = -1).to_string());
            }

            // Verify that the data size is at least enough for the expected RGB8 data.
            // If it's smaller, we have a memory corruption risk.
            let expected_min = (width * height * crate::constants::RGB_CHANNELS as u32) as usize;
            if data_len < expected_min {
                ffi::libraw_dcraw_clear_mem(processed);
                log::error!("[RawProcessor] Buffer size mismatch: expected at least {}, got {}", expected_min, data_len);
                return Err(rust_i18n::t!("error.buffer_size_mismatch").to_string());
            }

            let data = std::slice::from_raw_parts(data_ptr, data_len).to_vec();

            ffi::libraw_dcraw_clear_mem(processed);

            // If LibRaw included padding, we need to truncate the Vec to what RgbImage expects,
            // or use the stride if we were to support it. RgbImage::from_raw expects exactly W*H*3.
            let mut final_data = data;
            if final_data.len() > expected_min {
                final_data.truncate(expected_min);
            }

            let rgb = RgbImage::from_raw(width, height, final_data)
                .ok_or_else(|| rust_i18n::t!("error.rgb_image_create_failed").to_string())?;
            
            Ok(DynamicImage::ImageRgb8(rgb))
        }
    }

    pub fn unpack_thumb(&mut self) -> Result<crate::loader::DecodedImage, String> {
        let mut err = 0;
        unsafe {
            let res = ffi::libraw_unpack_thumb(self.data);
            if res != 0 {
                return Err(rust_i18n::t!("error.libraw_unpack", code = res).to_string());
            }

            let processed = ffi::libraw_dcraw_make_mem_thumb(self.data, &mut err);
            if processed.is_null() {
                return Err(rust_i18n::t!("error.libraw_mem_image", code = err).to_string());
            }

            let img = &*processed;
            let data_ptr = &img.data as *const u8;
            let data_size = img.data_size as usize;

            if data_ptr.is_null() || data_size == 0 {
                ffi::libraw_dcraw_clear_mem(processed);
                return Err(rust_i18n::t!("error.libraw_mem_image", code = -2).to_string());
            }

            let slice = std::slice::from_raw_parts(data_ptr, data_size);

            let result = if img.image_type == ffi::LibRaw_image_formats::LIBRAW_IMAGE_JPEG as u32 {
                // JPEG thumbnail
                match image::load_from_memory(slice) {
                    Ok(decoded) => {
                        let rgba = decoded.to_rgba8();
                        Ok(crate::loader::DecodedImage {
                            width: rgba.width(),
                            height: rgba.height(),
                            pixels: rgba.into_raw(),
                        })
                    }
                    Err(e) => Err(rust_i18n::t!("error.decode_thumb_failed", err = e).to_string()),
                }
            } else if img.image_type == ffi::LibRaw_image_formats::LIBRAW_IMAGE_BITMAP as u32 {
                // Bitmap thumbnail (RGB)
                if img.colors == crate::constants::RGB_CHANNELS as u16 && img.bits == crate::constants::BIT_DEPTH_8 as u16 {
                    let mut rgba = Vec::with_capacity(img.width as usize * img.height as usize * crate::constants::RGBA_CHANNELS);
                    for i in 0..(img.width as usize * img.height as usize) {
                        rgba.push(slice[i * crate::constants::RGB_CHANNELS]);
                        rgba.push(slice[i * crate::constants::RGB_CHANNELS + 1]);
                        rgba.push(slice[i * crate::constants::RGB_CHANNELS + 2]);
                        rgba.push(crate::constants::MAX_CHANNEL_VALUE);
                    }
                    Ok(crate::loader::DecodedImage {
                        width: img.width as u32,
                        height: img.height as u32,
                        pixels: rgba,
                    })
                } else {
                    // Heuristic fallback: Some cameras (like Fuji) might report a thumbnail as 
                    // a bitmap type but actually embed a JPEG, or report bits/colors as 0.
                    // We check for the JPEG magic bytes (FF D8 FF).
                    if slice.len() > crate::constants::RGB_CHANNELS && slice[0] == 0xFF && slice[1] == 0xD8 && slice[2] == 0xFF {
                        match image::load_from_memory(slice) {
                            Ok(decoded) => {
                                let rgba = decoded.to_rgba8();
                                Ok(crate::loader::DecodedImage {
                                    width: rgba.width(),
                                    height: rgba.height(),
                                    pixels: rgba.into_raw(),
                                })
                            }
                            Err(e) => Err(rust_i18n::t!("error.heuristic_jpeg_failed", err = e).to_string()),
                        }
                    } else {
                        Err(rust_i18n::t!("error.unsupported_thumb_format", colors = img.colors, bits = img.bits, img_type = img.image_type).to_string())
                    }
                }
            } else {
                Err(rust_i18n::t!("error.unknown_thumb_type", img_type = img.image_type).to_string())
            };

            ffi::libraw_dcraw_clear_mem(processed);
            result
        }
    }

    pub fn process_warnings(&self) -> u32 {
        unsafe { ffi::libraw_get_process_warnings(self.data) }
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
    "arw", "srf", "sr2", "sr1", "sr", // Sony
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
    "dng", "rwz", "cxi", "fpix", "rdc", "qtk" // Generic / Other (rawzor, foveon, etc)
];

pub fn is_raw_extension(ext: &str) -> bool {
    let lower = ext.to_lowercase();
    RAW_EXTENSIONS.contains(&lower.as_str())
}

pub fn get_supported_extensions() -> Vec<String> {
    // According to LibRaw's design, identification is based on Magic Numbers, 
    // not file extensions. For UI filtering purposes, we use this comprehensive 
    // list of common professional RAW formats.
    RAW_EXTENSIONS.iter().map(|s| s.to_string()).collect()
}
