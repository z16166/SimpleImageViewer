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
// use std::ptr;
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
                return Err(format!("LibRaw failed to open file: {}", ret));
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
                return Err(format!("LibRaw unpack failed: {}", ret));
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
            // Standard development
            let ret = ffi::libraw_dcraw_process(self.data);
            if ret != 0 {
                return Err(format!("LibRaw process failed: {}", ret));
            }

            let mut err = 0;
            let processed = ffi::libraw_dcraw_make_mem_image(self.data, &mut err);
            if processed.is_null() {
                return Err(format!("LibRaw make_mem_image failed: {}", err));
            }

            let img = &*processed;
            if img.image_type != ffi::LibRaw_image_formats::LIBRAW_IMAGE_BITMAP as u32 {
                ffi::libraw_dcraw_clear_mem(processed);
                return Err(format!("Unsupported processed image type: {} (expected BITMAP={})", img.image_type, ffi::LibRaw_image_formats::LIBRAW_IMAGE_BITMAP as u32));
            }

            if img.colors != 3 || img.bits != 8 {
                ffi::libraw_dcraw_clear_mem(processed);
                return Err(format!("Unsupported image format: colors={}, bits={}", img.colors, img.bits));
            }

            // Create RgbImage from the data
            let width = img.width as u32;
            let height = img.height as u32;
            let data_ptr = &img.data as *const u8;
            let data_len = (width * height * 3) as usize;
            let data = std::slice::from_raw_parts(data_ptr, data_len).to_vec();

            ffi::libraw_dcraw_clear_mem(processed);

            let rgb = RgbImage::from_raw(width, height, data)
                .ok_or("Failed to create RgbImage from buffer")?;
            
            Ok(DynamicImage::ImageRgb8(rgb))
        }
    }

    pub fn unpack_thumb(&mut self) -> Result<crate::loader::DecodedImage, String> {
        let mut err = 0;
        unsafe {
            let res = ffi::libraw_unpack_thumb(self.data);
            if res != 0 {
                return Err(format!("libraw_unpack_thumb failed: {}", res));
            }

            let processed = ffi::libraw_dcraw_make_mem_thumb(self.data, &mut err);
            if processed.is_null() {
                return Err(format!("libraw_dcraw_make_mem_thumb failed: {}", err));
            }

            let img = &*processed;
            let data_ptr = &img.data as *const u8;
            let data_size = img.data_size as usize;
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
                    Err(e) => Err(format!("Failed to decode JPEG thumbnail: {}", e)),
                }
            } else if img.image_type == ffi::LibRaw_image_formats::LIBRAW_IMAGE_BITMAP as u32 {
                // Bitmap thumbnail (RGB)
                if img.colors == 3 && img.bits == 8 {
                    let mut rgba = Vec::with_capacity(img.width as usize * img.height as usize * 4);
                    for i in 0..(img.width as usize * img.height as usize) {
                        rgba.push(slice[i * 3]);
                        rgba.push(slice[i * 3 + 1]);
                        rgba.push(slice[i * 3 + 2]);
                        rgba.push(255);
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
                    if slice.len() > 3 && slice[0] == 0xFF && slice[1] == 0xD8 && slice[2] == 0xFF {
                        match image::load_from_memory(slice) {
                            Ok(decoded) => {
                                let rgba = decoded.to_rgba8();
                                Ok(crate::loader::DecodedImage {
                                    width: rgba.width(),
                                    height: rgba.height(),
                                    pixels: rgba.into_raw(),
                                })
                            }
                            Err(e) => Err(format!("Heuristic JPEG detection failed: {}", e)),
                        }
                    } else {
                        Err(format!("Unsupported bitmap thumbnail format: colors={}, bits={}, image_type={}", img.colors, img.bits, img.image_type))
                    }
                }
            } else {
                Err(format!("Unknown thumbnail type: {}", img.image_type))
            };

            ffi::libraw_dcraw_clear_mem(processed);
            result
        }
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
