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

use std::cmp::Ordering;

use libc::{c_int, c_uchar, c_ulong, c_void};

#[allow(non_camel_case_types)]
type tjhandle = *mut c_void;

/// `tjGetErrorCode` — warning vs fatal (libjpeg-turbo ≥ 1.6). See `turbojpeg.h` `enum TJERR`.
const TJERR_WARNING: c_int = 0;

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
    /// Distinguishes warning (non-fatal) from fatal after `tjDecompress*` / `tjDecompressHeader*` returns -1.
    fn tjGetErrorCode(handle: tjhandle) -> c_int;
    fn tjGetScalingFactors(numscalingfactors: *mut c_int) -> *const tjscalingfactor;
}

/// DCT scaling factor — maps output dimensions to JPEG source dimensions via rational `num / denom`.
#[repr(C)]
#[derive(Debug, Copy, Clone)]
struct tjscalingfactor {
    num: c_int,
    denom: c_int,
}

pub struct Decompressor {
    handle: tjhandle,
}

/// TurboJPEG returns `0` on full success and `-1` on failure **or** on recoverable warning (e.g. unknown
/// marker `0x9d`). In the latter case `tjGetErrorCode` returns `0` (`TJERR_WARNING` in `turbojpeg.h`) and the output is still valid.
/// Do not use `TJFLAG_STOPONWARNING` on decompress flags — that turns warnings into hard failures.
fn turbo_jpeg_ok(handle: tjhandle, res: c_int) -> Result<(), String> {
    if res == 0 {
        return Ok(());
    }
    unsafe {
        let code = tjGetErrorCode(handle);
        if code == TJERR_WARNING {
            return Ok(());
        }
        let err = std::ffi::CStr::from_ptr(tjGetErrorStr2(handle));
        Err(err.to_string_lossy().into_owned())
    }
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

        turbo_jpeg_ok(self.handle, res)?;

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
        if width <= 0 || height <= 0 {
            return Err(format!("Invalid JPEG output dimensions: {width}x{height}"));
        }

        let pixel_size = match pf {
            TJPF::RGB | TJPF::BGR => 3,
            TJPF::GRAY => 1,
            TJPF::CMYK => 4,
            _ => 4,
        };

        let buf_len = (width as u64)
            .checked_mul(height as u64)
            .and_then(|pixels| pixels.checked_mul(pixel_size as u64))
            .ok_or_else(|| format!("JPEG output buffer size overflow: {width}x{height}"))?;
        if buf_len > isize::MAX as u64 {
            return Err(format!("JPEG output buffer size overflow: {width}x{height}"));
        }
        let buf_len = buf_len as usize;

        let mut dst_buf = vec![0u8; buf_len];

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

        turbo_jpeg_ok(self.handle, res)?;

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

/// Read JPEG image dimensions without decoding pixels.
///
/// Calls `tjDecompressHeader3` internally — parses only the SOF marker, no IDCT.
pub fn decode_jpeg_dimensions(jpeg_data: &[u8]) -> Result<(u32, u32), String> {
    let decompressor = Decompressor::new()?;
    let (w, h, _) = decompressor.decompress_header(jpeg_data)?;
    Ok((w as u32, h as u32))
}

/// High-level function to decode a JPEG to RGBA8
pub fn decode_to_rgba(jpeg_data: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let decompressor = Decompressor::new()?;
    let (w, h, _) = decompressor.decompress_header(jpeg_data)?;
    if w <= 0 || h <= 0 {
        return Err(format!("Invalid JPEG dimensions: {w}x{h}"));
    }
    let pixels = decompressor.decompress(jpeg_data, w, h, TJPF::RGBA)?;
    Ok((w as u32, h as u32, pixels))
}

/// Find the best DCT scaling factor such that the output dominant dimension does not exceed
/// `max_side`.
///
/// Among the supported factors that keep the long edge ≤ `max_side`, selects the one with the
/// **largest** ratio (least reduction) so the DCT-scaled output is as close to `max_side` as
/// possible — maximising strip preview fidelity.  When no factor can satisfy `max_side` (e.g. a
/// 4000 px image with `max_side`=256 — even 1/8 yields 500 > 256), falls back to the smallest
/// factor (e.g. 1/8).  The caller subsequently applies a SIMD box-filter downsample from that
/// intermediate, which is still ~64× faster and ~64× less peak memory than full-resolution
/// decode.
fn best_dct_scaled_dims(orig_w: u32, orig_h: u32, max_side: u32) -> (i32, i32) {
    let dominant = orig_w.max(orig_h);
    let mut num_factors: c_int = 0;
    let factors_ptr = unsafe { tjGetScalingFactors(&mut num_factors) };

    if factors_ptr.is_null() || num_factors <= 0 {
        return (orig_w as i32, orig_h as i32);
    }

    let factors = unsafe { std::slice::from_raw_parts(factors_ptr, num_factors as usize) };

    // Factors are sorted ascending by ratio; the first entry is the smallest
    // (e.g. 1/8).  Initialize to the smallest factor as a safe fallback for
    // large images where no factor satisfies max_side.
    let mut best_num = factors[0].num;
    let mut best_denom = factors[0].denom;

    for f in factors {
        let scaled = (dominant as u64 * f.num as u64) / f.denom as u64;
        if scaled <= max_side as u64 {
            // Prefer the factor with the LEAST reduction (largest ratio) so the
            // decoded output is as close to max_side as possible.
            match (f.num as i64 * best_denom as i64).cmp(&(best_num as i64 * f.denom as i64)) {
                Ordering::Greater => {
                    best_num = f.num;
                    best_denom = f.denom;
                }
                Ordering::Equal => {
                    // When ratios tie, prefer the smaller denominator (larger absolute dimensions).
                    if f.denom < best_denom {
                        best_num = f.num;
                        best_denom = f.denom;
                    }
                }
                Ordering::Less => {}
            }
        }
    }

    let out_w = ((orig_w as u64 * best_num as u64) / best_denom as u64).max(1) as i32;
    let out_h = ((orig_h as u64 * best_num as u64) / best_denom as u64).max(1) as i32;
    (out_w, out_h)
}

/// Decode a JPEG to RGBA8 with DCT-domain scaling.
///
/// Returns `(orig_w, orig_h, out_w, out_h, pixels)` — original JPEG dimensions plus the
/// decoded output dimensions and pixel buffer.  The output long edge is chosen via
/// [`best_dct_scaled_dims`] so it does not exceed `max_side`.  When the JPEG is already
/// smaller than `max_side`, no scaling is applied and `out_*` equals `orig_*`.
///
/// Because the scale factor is an exact `tjGetScalingFactors` ratio, `tjDecompress2` performs
/// IDCT at the reduced size — avoiding full-resolution decode, allocations, and the subsequent
/// software downsample.  For a 4000×3000 → 256 px strip preview this is roughly 10× faster and
/// uses ~64× less peak memory than a full decode + `image::imageops::resize`.
pub fn decode_to_rgba_with_max_side(
    jpeg_data: &[u8],
    max_side: u32,
) -> Result<(u32, u32, u32, u32, Vec<u8>), String> {
    // turbojpegʼs tjDecompress2 re-parses the SOF marker internally even when
    // the caller already called decompress_header — the overhead is negligible
    // (a few dozen bytes of fixed-length fields) so we donʼt add API surface for
    // a "decode without re-parse" fast path.
    let decompressor = Decompressor::new()?;
    let (orig_w, orig_h, _) = decompressor.decompress_header(jpeg_data)?;
    if orig_w <= 0 || orig_h <= 0 {
        return Err(format!("Invalid JPEG dimensions: {orig_w}x{orig_h}"));
    }
    let orig_w_u = orig_w as u32;
    let orig_h_u = orig_h as u32;

    if orig_w_u.max(orig_h_u) <= max_side {
        let pixels = decompressor.decompress(
            jpeg_data,
            orig_w_u as i32,
            orig_h_u as i32,
            TJPF::RGBA,
        )?;
        return Ok((orig_w_u, orig_h_u, orig_w_u, orig_h_u, pixels));
    }

    let (out_w, out_h) = best_dct_scaled_dims(orig_w_u, orig_h_u, max_side);
    let pixels = decompressor.decompress(jpeg_data, out_w, out_h, TJPF::RGBA)?;
    Ok((orig_w_u, orig_h_u, out_w as u32, out_h as u32, pixels))
}

#[cfg(test)]
mod tests {
    use super::Decompressor;
    use super::TJPF;

    #[test]
    fn decompress_rejects_dimensions_that_overflow_buffer_size() {
        let decompressor = Decompressor::new().expect("decompressor");
        let err = decompressor
            .decompress(&[], i32::MAX, i32::MAX, TJPF::RGBA)
            .expect_err("overflow dimensions must fail before allocation");
        assert!(
            err.contains("overflow"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn decompress_i32_overflow_prone_dimensions_do_not_panic() {
        let decompressor = Decompressor::new().expect("decompressor");
        // 23171² × 4 exceeds i32::MAX — previously panicked in debug builds.
        let _ = decompressor.decompress(&[], 23_171, 23_171, TJPF::RGBA);
    }
}
