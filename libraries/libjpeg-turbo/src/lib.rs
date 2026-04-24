// Simple Image Viewer - A high-performance, cross-platform image viewer
// Copyright (C) 2024-2026 Simple Image Viewer Contributors
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

use libc::{c_int, c_uchar, c_ulong, c_void};

#[allow(non_camel_case_types)]
type tjhandle = *mut c_void;

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum TJPF {
    RGB = 0,
    BGR = 1,
    RGBX = 2,
    BGRX = 3,
    XBGR = 4,
    XRGB = 5,
    GRAY = 6,
    RGBA = 7,
    BGRA = 8,
    ABGR = 9,
    ARGB = 10,
    CMYK = 11,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[allow(non_camel_case_types)]
pub enum TJSAMP {
    SAMP_444 = 0,
    SAMP_422 = 1,
    SAMP_420 = 2,
    SAMP_GRAY = 3,
    SAMP_440 = 4,
    SAMP_411 = 5,
}

unsafe extern "C" {
    fn tjInitDecompress() -> tjhandle;
    fn tjDecompressHeader3(
        handle: tjhandle,
        jpegBuf: *const c_uchar,
        jpegSize: c_ulong,
        width: *mut c_int,
        height: *mut c_int,
        jpegSubsamp: *mut c_int,
        jpegColorspace: *mut c_int,
    ) -> c_int;
    fn tjDecompress2(
        handle: tjhandle,
        jpegBuf: *const c_uchar,
        jpegSize: c_ulong,
        dstBuf: *mut c_uchar,
        width: c_int,
        pitch: c_int,
        height: c_int,
        pixelFormat: c_int,
        flags: c_int,
    ) -> c_int;
    fn tjDestroy(handle: tjhandle) -> c_int;
    fn tjGetErrorStr2(handle: tjhandle) -> *const libc::c_char;
}

pub struct Decompressor {
    handle: tjhandle,
}

impl Decompressor {
    pub fn new() -> Result<Self, String> {
        let handle = unsafe { tjInitDecompress() };
        if handle.is_null() {
            return Err("Failed to initialize TurboJPEG decompressor".to_string());
        }
        Ok(Self { handle })
    }

    pub fn decompress_header(&self, jpeg_data: &[u8]) -> Result<(i32, i32, TJSAMP), String> {
        let mut width: c_int = 0;
        let mut height: c_int = 0;
        let mut subsamp: c_int = 0;
        let mut colorspace: c_int = 0;

        let res = unsafe {
            tjDecompressHeader3(
                self.handle,
                jpeg_data.as_ptr(),
                jpeg_data.len() as c_ulong,
                &mut width,
                &mut height,
                &mut subsamp,
                &mut colorspace,
            )
        };

        if res != 0 {
            let err = unsafe { std::ffi::CStr::from_ptr(tjGetErrorStr2(self.handle)) };
            return Err(err.to_string_lossy().into_owned());
        }

        let subsamp_enum = match subsamp {
            0 => TJSAMP::SAMP_444,
            1 => TJSAMP::SAMP_422,
            2 => TJSAMP::SAMP_420,
            3 => TJSAMP::SAMP_GRAY,
            4 => TJSAMP::SAMP_440,
            5 => TJSAMP::SAMP_411,
            _ => TJSAMP::SAMP_444, // Fallback
        };

        Ok((width, height, subsamp_enum))
    }

    pub fn decompress(
        &self,
        jpeg_data: &[u8],
        width: i32,
        height: i32,
        pf: TJPF,
    ) -> Result<Vec<u8>, String> {
        let pixel_size = match pf {
            TJPF::RGB | TJPF::BGR => 3,
            TJPF::GRAY => 1,
            TJPF::CMYK => 4,
            _ => 4,
        };

        let mut dst_buf = vec![0u8; (width * height * pixel_size) as usize];

        let res = unsafe {
            tjDecompress2(
                self.handle,
                jpeg_data.as_ptr(),
                jpeg_data.len() as c_ulong,
                dst_buf.as_mut_ptr(),
                width,
                0, // pitch (0 means width * pixel_size)
                height,
                pf as c_int,
                0, // flags
            )
        };

        if res != 0 {
            let err = unsafe { std::ffi::CStr::from_ptr(tjGetErrorStr2(self.handle)) };
            return Err(err.to_string_lossy().into_owned());
        }

        Ok(dst_buf)
    }
}

impl Drop for Decompressor {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe {
                tjDestroy(self.handle);
            }
        }
    }
}

unsafe impl Send for Decompressor {}
unsafe impl Sync for Decompressor {}

/// High-level function to decode a JPEG to RGBA8
pub fn decode_to_rgba(jpeg_data: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let decompressor = Decompressor::new()?;
    let (w, h, _) = decompressor.decompress_header(jpeg_data)?;
    let pixels = decompressor.decompress(jpeg_data, w, h, TJPF::RGBA)?;
    Ok((w as u32, h as u32, pixels))
}
