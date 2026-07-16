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

//! Low-resolution directory-tree strip decode via libjxl embedded preview (no full-frame float).

use super::decode::{jxl_sanitize_straight_alpha, srgb_unit_to_u8};
use super::metadata::ensure_jxl_success;
use super::probe::is_jxl_header;
use super::runner::JxlResizableRunnerPtr;

#[cfg(feature = "jpegxl")]
type JxlStripPreviewResult = Option<Result<(Vec<u8>, u32, u32, u32, u32), String>>;

/// Decode the libjxl DC / preview image only (typically a few hundred px per side).
///
/// Returns `None` when the codestream has no preview or the file is not JPEG XL.
#[cfg(feature = "jpegxl")]
pub(crate) fn decode_jxl_strip_preview_rgba8(bytes: &[u8]) -> JxlStripPreviewResult {
    if bytes.len() < 2 {
        return None;
    }
    let probe_len = bytes.len().clamp(2, 16);
    if !is_jxl_header(&bytes[..probe_len]) {
        return None;
    }

    struct JxlDecoder(*mut libjxl_sys::JxlDecoder);
    impl Drop for JxlDecoder {
        fn drop(&mut self) {
            unsafe { libjxl_sys::JxlDecoderDestroy(self.0) };
        }
    }

    let parallel_runner = JxlResizableRunnerPtr::try_new();
    let decoder = JxlDecoder(unsafe { libjxl_sys::JxlDecoderCreate(std::ptr::null()) });
    if decoder.0.is_null() {
        return Some(Err("Failed to create libjxl decoder".to_string()));
    }

    let unpremul_st =
        unsafe { libjxl_sys::JxlDecoderSetUnpremultiplyAlpha(decoder.0, libjxl_sys::JXL_TRUE) };
    if unpremul_st != libjxl_sys::JXL_DEC_SUCCESS {
        log::debug!(
            "JxlDecoderSetUnpremultiplyAlpha failed (libjxl status {unpremul_st}) on strip preview path"
        );
    }

    if let Some(runner) = parallel_runner.as_ref() {
        let _ = unsafe {
            libjxl_sys::JxlDecoderSetParallelRunner(
                decoder.0,
                Some(libjxl_sys::JxlResizableParallelRunner),
                runner.as_ptr(),
            )
        };
    }

    let subscribed = libjxl_sys::JXL_DEC_BASIC_INFO | libjxl_sys::JXL_DEC_PREVIEW_IMAGE;
    ensure_jxl_success(
        unsafe { libjxl_sys::JxlDecoderSubscribeEvents(decoder.0, subscribed) },
        "subscribe JPEG XL strip preview events",
    )
    .ok()?;
    ensure_jxl_success(
        unsafe { libjxl_sys::JxlDecoderSetInput(decoder.0, bytes.as_ptr(), bytes.len()) },
        "set JPEG XL strip preview input",
    )
    .ok()?;
    unsafe { libjxl_sys::JxlDecoderCloseInput(decoder.0) };

    let pixel_format = libjxl_sys::JxlPixelFormat {
        num_channels: 4,
        data_type: libjxl_sys::JXL_TYPE_FLOAT,
        endianness: libjxl_sys::JXL_NATIVE_ENDIAN,
        align: 0,
    };

    let mut basic_info: Option<libjxl_sys::JxlBasicInfo> = None;
    let mut preview_scratch = Vec::<u8>::new();
    let mut preview_ready = false;

    loop {
        match unsafe { libjxl_sys::JxlDecoderProcessInput(decoder.0) } {
            libjxl_sys::JXL_DEC_SUCCESS => {
                if !preview_ready {
                    return None;
                }
                let info = basic_info?;
                let preview = info.preview;
                if preview.xsize == 0 || preview.ysize == 0 {
                    return Some(Err("libjxl preview has zero dimensions".to_string()));
                }
                let expected = match (preview.xsize as usize)
                    .checked_mul(preview.ysize as usize)
                    .and_then(|p| p.checked_mul(4))
                {
                    Some(v) => v,
                    None => {
                        return Some(Err(format!(
                            "libjxl strip preview size overflow for {}x{}",
                            preview.xsize, preview.ysize
                        )));
                    }
                };
                let floats: &[f32] = bytemuck::cast_slice(&preview_scratch);
                if floats.len() < expected {
                    return Some(Err(format!(
                        "libjxl preview buffer too short: {} floats, expected {}",
                        floats.len(),
                        expected
                    )));
                }
                let mut rgba_f32 = floats[..expected].to_vec();
                jxl_sanitize_straight_alpha(&mut rgba_f32);
                let rgba8 = jxl_preview_float_to_display_rgba8(&rgba_f32);
                return Some(Ok((
                    rgba8,
                    preview.xsize,
                    preview.ysize,
                    info.xsize,
                    info.ysize,
                )));
            }
            libjxl_sys::JXL_DEC_ERROR => {
                return Some(Err("libjxl strip preview decode failed".to_string()));
            }
            libjxl_sys::JXL_DEC_NEED_MORE_INPUT => {
                return Some(Err("libjxl strip preview requested more input".to_string()));
            }
            libjxl_sys::JXL_DEC_BASIC_INFO => {
                let mut info = std::mem::MaybeUninit::<libjxl_sys::JxlBasicInfo>::zeroed();
                ensure_jxl_success(
                    unsafe { libjxl_sys::JxlDecoderGetBasicInfo(decoder.0, info.as_mut_ptr()) },
                    "read JPEG XL basic info for strip preview",
                )
                .ok()?;
                let info = unsafe { info.assume_init() };
                if info.have_preview == 0 {
                    return None;
                }
                if info.xsize == 0 || info.ysize == 0 {
                    return Some(Err("libjxl decoded zero-sized image".to_string()));
                }
                crate::constants::validate_static_decode_dimensions(info.xsize, info.ysize).ok()?;
                basic_info = Some(info);
            }
            libjxl_sys::JXL_DEC_NEED_PREVIEW_OUT_BUFFER => {
                let mut size = 0_usize;
                ensure_jxl_success(
                    unsafe {
                        libjxl_sys::JxlDecoderPreviewOutBufferSize(
                            decoder.0,
                            &pixel_format,
                            &mut size,
                        )
                    },
                    "size JPEG XL strip preview output buffer",
                )
                .ok()?;
                if !size.is_multiple_of(std::mem::size_of::<f32>()) {
                    return Some(Err(
                        "libjxl strip preview buffer size is not float-aligned".to_string()
                    ));
                }
                // Validate preview buffer size before allocation.
                let info = match basic_info.as_ref() {
                    Some(info) => info,
                    None => {
                        return Some(Err(
                            "JXL_NEED_PREVIEW_OUT_BUFFER before basic info".to_string()
                        ));
                    }
                };
                crate::constants::validate_static_decode_dimensions(
                    info.preview.xsize,
                    info.preview.ysize,
                )
                .ok()?;
                let expected_preview = match (info.preview.xsize as usize)
                    .checked_mul(info.preview.ysize as usize)
                    .and_then(|p| p.checked_mul(4))
                    .and_then(|p| p.checked_mul(std::mem::size_of::<f32>()))
                {
                    Some(v) => v,
                    None => {
                        return Some(Err(format!(
                            "JPEG XL strip preview buffer size overflow for {}x{}",
                            info.preview.xsize, info.preview.ysize
                        )));
                    }
                };
                if size < expected_preview {
                    return Some(Err(format!(
                        "JPEG XL strip preview buffer size {size} is smaller than expected {expected_preview}"
                    )));
                }
                preview_scratch.resize(size, 0);
                ensure_jxl_success(
                    unsafe {
                        libjxl_sys::JxlDecoderSetPreviewOutBuffer(
                            decoder.0,
                            &pixel_format,
                            preview_scratch.as_mut_ptr().cast(),
                            size,
                        )
                    },
                    "set JPEG XL strip preview output buffer",
                )
                .ok()?;
            }
            libjxl_sys::JXL_DEC_PREVIEW_IMAGE => {
                preview_ready = true;
            }
            _ => {}
        }
    }
}

#[cfg(feature = "jpegxl")]
fn jxl_preview_float_to_display_rgba8(rgba_f32: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgba_f32.len());
    for px in rgba_f32.chunks_exact(4) {
        out.push(srgb_unit_to_u8(px[0]));
        out.push(srgb_unit_to_u8(px[1]));
        out.push(srgb_unit_to_u8(px[2]));
        let a = if px[3].is_finite() {
            (px[3].clamp(0.0, 1.0) * 255.0).round() as u8
        } else {
            255
        };
        out.push(a);
    }
    out
}

#[cfg(all(test, feature = "jpegxl"))]
mod tests {
    #[test]
    fn jxl_strip_preview_short_input_returns_none() {
        assert!(super::decode_jxl_strip_preview_rgba8(&[]).is_none());
        assert!(super::decode_jxl_strip_preview_rgba8(&[0xff]).is_none());
    }
}
