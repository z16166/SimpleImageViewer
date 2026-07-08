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

/// `enum TJPARAM` in turbojpeg.h (TurboJPEG 3).
const TJPARAM_SUBSAMP: c_int = 4;
const TJPARAM_JPEGWIDTH: c_int = 5;
const TJPARAM_JPEGHEIGHT: c_int = 6;

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

#[repr(i32)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[allow(non_camel_case_types)]
pub enum TJSAMP {
    /// `TJSAMP_UNKNOWN` in turbojpeg.h — packed-pixel decode is still supported.
    UNKNOWN = -1,
    SAMP_444 = 0,
    SAMP_422 = 1,
    SAMP_420 = 2,
    SAMP_GRAY = 3,
    SAMP_440 = 4,
    SAMP_411 = 5,
}

unsafe extern "C" {
    fn tjInitDecompress() -> tjhandle;
    fn tj3DecompressHeader(handle: tjhandle, jpegBuf: *const c_uchar, jpegSize: usize) -> c_int;
    fn tj3Get(handle: tjhandle, param: c_int) -> c_int;
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

fn subsamp_from_tj(subsamp: c_int) -> TJSAMP {
    match subsamp {
        -1 => TJSAMP::UNKNOWN,
        0 => TJSAMP::SAMP_444,
        1 => TJSAMP::SAMP_422,
        2 => TJSAMP::SAMP_420,
        3 => TJSAMP::SAMP_GRAY,
        4 => TJSAMP::SAMP_440,
        5 => TJSAMP::SAMP_411,
        _ => TJSAMP::SAMP_444,
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
        let res = unsafe {
            tj3DecompressHeader(self.handle, jpeg_data.as_ptr(), jpeg_data.len())
        };
        turbo_jpeg_ok(self.handle, res)?;

        let width = unsafe { tj3Get(self.handle, TJPARAM_JPEGWIDTH) };
        let height = unsafe { tj3Get(self.handle, TJPARAM_JPEGHEIGHT) };
        let subsamp = unsafe { tj3Get(self.handle, TJPARAM_SUBSAMP) };
        if width <= 0 || height <= 0 {
            return Err(format!("Invalid JPEG dimensions: {width}x{height}"));
        }

        Ok((width, height, subsamp_from_tj(subsamp)))
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
            return Err(format!(
                "JPEG output buffer size overflow: {width}x{height}"
            ));
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

    let mut fallback: Option<(c_int, c_int)> = None;
    let mut best_fit: Option<(c_int, c_int)> = None;

    for f in factors {
        if f.num <= 0 || f.denom <= 0 {
            continue;
        }
        match fallback {
            Some((num, denom)) => {
                if (f.num as i64 * denom as i64) < (num as i64 * f.denom as i64) {
                    fallback = Some((f.num, f.denom));
                }
            }
            None => fallback = Some((f.num, f.denom)),
        }

        let scaled = dct_scaled_dim(dominant, f.num, f.denom);
        if scaled <= max_side as u64 {
            // Prefer the factor with the LEAST reduction (largest ratio) so the
            // decoded output is as close to max_side as possible.
            let Some((best_num, best_denom)) = best_fit else {
                best_fit = Some((f.num, f.denom));
                continue;
            };
            match (f.num as i64 * best_denom as i64).cmp(&(best_num as i64 * f.denom as i64)) {
                Ordering::Greater => {
                    best_fit = Some((f.num, f.denom));
                }
                Ordering::Equal => {
                    // When ratios tie, prefer the smaller denominator (larger absolute dimensions).
                    if f.denom < best_denom {
                        best_fit = Some((f.num, f.denom));
                    }
                }
                Ordering::Less => {}
            }
        }
    }

    let (best_num, best_denom) = best_fit.or(fallback).unwrap_or((1, 1));
    let out_w = dct_scaled_dim(orig_w, best_num, best_denom).max(1) as i32;
    let out_h = dct_scaled_dim(orig_h, best_num, best_denom).max(1) as i32;
    (out_w, out_h)
}

fn dct_scaled_dim(dim: u32, num: c_int, denom: c_int) -> u64 {
    let numerator = dim as u64 * num as u64;
    numerator.div_ceil(denom as u64)
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
        let pixels =
            decompressor.decompress(jpeg_data, orig_w_u as i32, orig_h_u as i32, TJPF::RGBA)?;
        return Ok((orig_w_u, orig_h_u, orig_w_u, orig_h_u, pixels));
    }

    let (out_w, out_h) = best_dct_scaled_dims(orig_w_u, orig_h_u, max_side);
    let pixels = decompressor.decompress(jpeg_data, out_w, out_h, TJPF::RGBA)?;
    Ok((orig_w_u, orig_h_u, out_w as u32, out_h as u32, pixels))
}

#[cfg(test)]
mod tests {
    use super::best_dct_scaled_dims;
    use super::decode_to_rgba;
    use super::decode_to_rgba_with_max_side;
    use super::Decompressor;
    use super::TJPF;
    use std::path::PathBuf;

    fn workspace_asset(path: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join(path)
    }

    #[test]
    fn decode_to_rgba_with_max_side_handles_baseline_jpeg_fixture() {
        let path = workspace_asset("assets/screenshot.jpg");
        if !path.is_file() {
            eprintln!("skip; missing {}", path.display());
            return;
        }
        let bytes = std::fs::read(&path).expect("read fixture JPEG");
        let (orig_w, orig_h, out_w, out_h, pixels) =
            decode_to_rgba_with_max_side(&bytes, 256).expect("DCT strip decode");
        assert!(orig_w > 0 && orig_h > 0);
        assert!(out_w > 0 && out_h > 0);
        assert!(out_w <= orig_w && out_h <= orig_h);
        assert_eq!(pixels.len(), out_w as usize * out_h as usize * 4);
    }

    #[test]
    fn decode_to_rgba_handles_baseline_jpeg_fixture() {
        let path = workspace_asset("assets/screenshot.jpg");
        if !path.is_file() {
            eprintln!("skip; missing {}", path.display());
            return;
        }
        let bytes = std::fs::read(&path).expect("read fixture JPEG");
        let (w, h, pixels) = decode_to_rgba(&bytes).expect("full JPEG decode");
        assert!(w > 0 && h > 0);
        assert_eq!(pixels.len(), w as usize * h as usize * 4);
    }

    #[test]
    fn decompress_rejects_dimensions_that_overflow_buffer_size() {
        let decompressor = Decompressor::new().expect("decompressor");
        let err = decompressor
            .decompress(&[], i32::MAX, i32::MAX, TJPF::RGBA)
            .expect_err("overflow dimensions must fail before allocation");
        assert!(err.contains("overflow"), "unexpected error: {err}");
    }

    #[test]
    fn decompress_i32_overflow_prone_dimensions_do_not_panic() {
        let decompressor = Decompressor::new().expect("decompressor");
        // 23171² × 4 exceeds i32::MAX — previously panicked in debug builds.
        let _ = decompressor.decompress(&[], 23_171, 23_171, TJPF::RGBA);
    }

    #[test]
    fn dct_scaled_dims_never_upscale_large_strip_sources() {
        let (out_w, out_h) = best_dct_scaled_dims(20_000, 15_059, 128);

        assert_eq!((out_w, out_h), (2_500, 1_883));
        assert!(
            out_w <= 20_000 && out_h <= 15_059,
            "DCT strip scaling must not upscale large sources: {out_w}x{out_h}"
        );
    }
}
