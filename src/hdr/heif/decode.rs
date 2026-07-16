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
use super::metadata::heif_sample_bit_depth;
use super::session::append_heif_unci_build_hint;
use super::ycbcr::hdr_buffer_from_ycbcr;

use crate::hdr::types::{HdrImageBuffer, HdrImageMetadata, HdrPixelFormat};
#[cfg(feature = "heif-native")]
use crate::loader::DecodedImage;
#[cfg(feature = "heif-native")]
use rayon::prelude::*;
#[cfg(feature = "heif-native")]
use std::sync::Arc;

/// Minimum image height before HEIF pixel conversion uses rayon row parallelism.
#[cfg(feature = "heif-native")]
pub(crate) const PARALLEL_ROW_THRESHOLD: usize = 8;

/// Read-only libheif plane pointer safe to share across rayon row workers for one decode.
#[cfg(feature = "heif-native")]
#[derive(Copy, Clone)]
pub(crate) struct SendReadonlyPtr(*const u8);

#[cfg(feature = "heif-native")]
impl SendReadonlyPtr {
    #[inline]
    pub(crate) fn new(ptr: *const u8) -> Self {
        Self(ptr)
    }

    #[inline]
    pub(crate) fn get(self) -> *const u8 {
        self.0
    }
}

// SAFETY: libheif exposes immutable plane bytes for the decode lifetime; row workers read disjoint rows.
#[cfg(feature = "heif-native")]
unsafe impl Send for SendReadonlyPtr {}

#[cfg(feature = "heif-native")]
unsafe impl Sync for SendReadonlyPtr {}

#[cfg(feature = "heif-native")]
pub(crate) fn parallel_row_chunks_mut<T>(
    rows: usize,
    row_len: usize,
    buf: &mut [T],
    decode_row: impl Fn(usize, &mut [T]) -> Result<(), String> + Send + Sync,
) -> Result<(), String>
where
    T: Send,
{
    debug_assert_eq!(buf.len(), rows * row_len);
    if rows >= PARALLEL_ROW_THRESHOLD {
        buf.par_chunks_mut(row_len)
            .enumerate()
            .map(|(y, row)| decode_row(y, row))
            .collect()
    } else {
        for y in 0..rows {
            let start = y * row_len;
            decode_row(y, &mut buf[start..start + row_len])?;
        }
        Ok(())
    }
}

/// Decode the primary HEIF tile to HDR float RGBA using libheif's preferred native layout (one decode).
#[cfg(feature = "heif-native")]
pub(crate) fn decode_primary_heif_to_hdr(
    handle: *const libheif_sys::heif_image_handle,
    metadata: HdrImageMetadata,
    decode_options: *const libheif_sys::heif_decoding_options,
) -> Result<HdrImageBuffer, String> {
    let img =
        heif_decode_primary_once(handle, decode_options).map_err(append_heif_unci_build_hint)?;
    hdr_buffer_from_decoded_heif(handle, metadata, img.as_ptr())
        .map_err(append_heif_unci_build_hint)
}

/// Resolve the single `(colorspace, chroma)` pair libheif proposes for native decode.
#[cfg(feature = "heif-native")]
pub(crate) fn heif_preferred_decode_target(
    handle: *const libheif_sys::heif_image_handle,
) -> Result<(libheif_sys::heif_colorspace, libheif_sys::heif_chroma), String> {
    let mut colorspace = libheif_sys::heif_colorspace_undefined;
    let mut chroma = libheif_sys::heif_chroma_undefined;
    let err = unsafe {
        libheif_sys::heif_image_handle_get_preferred_decoding_colorspace(
            handle,
            &mut colorspace,
            &mut chroma,
        )
    };
    if err.code != libheif_sys::heif_error_Ok {
        return Err(format!(
            "libheif preferred decode layout query failed: {}",
            heif_err_to_plain(err)
        ));
    }

    if colorspace == libheif_sys::heif_colorspace_undefined
        || chroma == libheif_sys::heif_chroma_undefined
    {
        Ok((
            libheif_sys::heif_colorspace_undefined,
            libheif_sys::heif_chroma_undefined,
        ))
    } else {
        Ok((colorspace, chroma))
    }
}

/// Decode the primary image exactly once using libheif's preferred (or native) layout.
#[cfg(feature = "heif-native")]
pub(crate) fn heif_decode_primary_once(
    handle: *const libheif_sys::heif_image_handle,
    decode_options: *const libheif_sys::heif_decoding_options,
) -> Result<RawHeifImage, String> {
    let (colorspace, chroma) = heif_preferred_decode_target(handle)?;
    heif_decode_into(handle, colorspace, chroma, decode_options).map_err(|err| {
        format!(
            "HEIF decode failed for {}: {}",
            heif_decode_target_label(colorspace, chroma),
            heif_err_to_plain(err)
        )
    })
}

/// Convert a decoded libheif image (already rasterized once) into HDR float RGBA.
#[cfg(feature = "heif-native")]
pub(crate) fn hdr_buffer_from_decoded_heif(
    handle: *const libheif_sys::heif_image_handle,
    metadata: HdrImageMetadata,
    image: *const libheif_sys::heif_image,
) -> Result<HdrImageBuffer, String> {
    let colorspace = unsafe { libheif_sys::heif_image_get_colorspace(image) };
    let chroma = unsafe { libheif_sys::heif_image_get_chroma_format(image) };

    match colorspace {
        cs if cs == libheif_sys::heif_colorspace_YCbCr => {
            hdr_buffer_from_ycbcr(handle, metadata, image, chroma)
        }
        cs if cs == libheif_sys::heif_colorspace_RGB => match chroma {
            c if c == libheif_sys::heif_chroma_444 => {
                hdr_buffer_from_planar_rgb444(handle, metadata, image)
            }
            c if c == libheif_sys::heif_chroma_interleaved_RRGGBBAA_LE => {
                hdr_buffer_from_interleaved_rgb16(handle, metadata, image, 4, false)
            }
            c if c == libheif_sys::heif_chroma_interleaved_RRGGBB_LE => {
                hdr_buffer_from_interleaved_rgb16(handle, metadata, image, 3, false)
            }
            c if c == libheif_sys::heif_chroma_interleaved_RRGGBBAA_BE => {
                hdr_buffer_from_interleaved_rgb16(handle, metadata, image, 4, true)
            }
            c if c == libheif_sys::heif_chroma_interleaved_RRGGBB_BE => {
                hdr_buffer_from_interleaved_rgb16(handle, metadata, image, 3, true)
            }
            c if c == libheif_sys::heif_chroma_interleaved_RGBA => {
                hdr_buffer_from_interleaved_rgb8_packed(handle, metadata, image, 4)
            }
            c if c == libheif_sys::heif_chroma_interleaved_RGB => {
                hdr_buffer_from_interleaved_rgb8_packed(handle, metadata, image, 3)
            }
            _ => Err(format!(
                "unsupported HEIF RGB chroma layout ({})",
                heif_chroma_label(chroma)
            )),
        },
        cs if cs == libheif_sys::heif_colorspace_monochrome => {
            hdr_buffer_from_monochrome(handle, metadata, image)
        }
        cs if cs == libheif_sys::heif_colorspace_nonvisual => {
            Err("HEIF primary uses non-visual colorspace (no displayable raster)".to_string())
        }
        _ => Err(format!(
            "unsupported HEIF colorspace ({colorspace}); decoded chroma={}",
            heif_chroma_label(chroma)
        )),
    }
}

/// Convert a decoded libheif image (already rasterized once) into 8-bit SDR RGBA.
#[cfg(feature = "heif-native")]
pub(crate) fn rgba8_from_decoded_heif(
    handle: *const libheif_sys::heif_image_handle,
    image: *const libheif_sys::heif_image,
) -> Result<DecodedImage, String> {
    let colorspace = unsafe { libheif_sys::heif_image_get_colorspace(image) };
    let chroma = unsafe { libheif_sys::heif_image_get_chroma_format(image) };

    match colorspace {
        cs if cs == libheif_sys::heif_colorspace_YCbCr => {
            let convert = super::ycbcr::ycbcr_matrix_from_heif_handle(handle);
            super::ycbcr::ycbcr_image_to_rgba8(image, chroma, convert)
        }
        cs if cs == libheif_sys::heif_colorspace_RGB => match chroma {
            c if c == libheif_sys::heif_chroma_444 => rgba8_from_planar_rgb444(handle, image),
            c if c == libheif_sys::heif_chroma_interleaved_RGBA => {
                rgba8_from_interleaved_rgb8(image, 4)
            }
            c if c == libheif_sys::heif_chroma_interleaved_RGB => {
                rgba8_from_interleaved_rgb8(image, 3)
            }
            c if c == libheif_sys::heif_chroma_interleaved_RRGGBBAA_LE => {
                rgba8_from_interleaved_rgb16(handle, image, 4, false)
            }
            c if c == libheif_sys::heif_chroma_interleaved_RRGGBB_LE => {
                rgba8_from_interleaved_rgb16(handle, image, 3, false)
            }
            c if c == libheif_sys::heif_chroma_interleaved_RRGGBBAA_BE => {
                rgba8_from_interleaved_rgb16(handle, image, 4, true)
            }
            c if c == libheif_sys::heif_chroma_interleaved_RRGGBB_BE => {
                rgba8_from_interleaved_rgb16(handle, image, 3, true)
            }
            _ => Err(format!(
                "unsupported HEIF RGB chroma layout for SDR preview ({})",
                heif_chroma_label(chroma)
            )),
        },
        cs if cs == libheif_sys::heif_colorspace_monochrome => rgba8_from_monochrome(image),
        cs if cs == libheif_sys::heif_colorspace_nonvisual => {
            Err("HEIF item uses non-visual colorspace (no SDR preview raster)".to_string())
        }
        _ => Err(format!(
            "unsupported HEIF colorspace for SDR preview ({colorspace}); chroma={}",
            heif_chroma_label(chroma)
        )),
    }
}

#[cfg(feature = "heif-native")]
pub(crate) fn heif_decode_target_label(
    colorspace: libheif_sys::heif_colorspace,
    chroma: libheif_sys::heif_chroma,
) -> String {
    format!(
        "{} / {}",
        heif_colorspace_label(colorspace),
        heif_chroma_label(chroma)
    )
}

#[cfg(feature = "heif-native")]
pub(crate) fn heif_colorspace_label(colorspace: libheif_sys::heif_colorspace) -> &'static str {
    match colorspace {
        cs if cs == libheif_sys::heif_colorspace_YCbCr => "YCbCr",
        cs if cs == libheif_sys::heif_colorspace_RGB => "RGB",
        cs if cs == libheif_sys::heif_colorspace_monochrome => "monochrome",
        cs if cs == libheif_sys::heif_colorspace_nonvisual => "non-visual",
        cs if cs == libheif_sys::heif_colorspace_undefined => "native (undefined)",
        _ => "unknown colorspace",
    }
}

#[cfg(feature = "heif-native")]
pub(crate) fn heif_chroma_label(chroma: libheif_sys::heif_chroma) -> &'static str {
    match chroma {
        c if c == libheif_sys::heif_chroma_undefined => "native (undefined)",
        c if c == libheif_sys::heif_chroma_monochrome => "monochrome",
        c if c == libheif_sys::heif_chroma_420 => "4:2:0",
        c if c == libheif_sys::heif_chroma_422 => "4:2:2",
        c if c == libheif_sys::heif_chroma_444 => "4:4:4",
        c if c == libheif_sys::heif_chroma_interleaved_RGB => "interleaved RGB8",
        c if c == libheif_sys::heif_chroma_interleaved_RGBA => "interleaved RGBA8",
        c if c == libheif_sys::heif_chroma_interleaved_RRGGBB_LE => "interleaved RRGGBB16 LE",
        c if c == libheif_sys::heif_chroma_interleaved_RRGGBBAA_LE => "interleaved RRGGBBAA16 LE",
        c if c == libheif_sys::heif_chroma_interleaved_RRGGBB_BE => "interleaved RRGGBB16 BE",
        c if c == libheif_sys::heif_chroma_interleaved_RRGGBBAA_BE => "interleaved RRGGBBAA16 BE",
        _ => "unknown chroma",
    }
}

#[cfg(feature = "heif-native")]
pub(crate) struct RawHeifImage(pub libheif_sys::HeifImageGuard);

#[cfg(feature = "heif-native")]
impl RawHeifImage {
    #[inline]
    pub(crate) fn as_ptr(&self) -> *const libheif_sys::heif_image {
        self.0.as_ptr()
    }
}

#[cfg(feature = "heif-native")]
pub(crate) fn heif_decode_into(
    handle: *const libheif_sys::heif_image_handle,
    cs: libheif_sys::heif_colorspace,
    chroma: libheif_sys::heif_chroma,
    decode_options: *const libheif_sys::heif_decoding_options,
) -> Result<RawHeifImage, libheif_sys::heif_error> {
    let mut image_ptr = std::ptr::null_mut();
    let err = unsafe {
        libheif_sys::heif_decode_image(handle, &mut image_ptr, cs, chroma, decode_options)
    };
    if err.code != libheif_sys::heif_error_Ok {
        return Err(err);
    }
    if image_ptr.is_null() {
        return Err(libheif_sys::heif_error {
            code: -1,
            subcode: 0,
            message: std::ptr::null(),
        });
    }
    Ok(RawHeifImage(unsafe {
        libheif_sys::HeifImageGuard::from_ptr(image_ptr)
    }))
}

#[cfg(feature = "heif-native")]
pub(crate) fn heif_err_to_plain(err: libheif_sys::heif_error) -> String {
    use std::ffi::CStr;
    if err.message.is_null() {
        return format!("libheif error code {} subcode {}", err.code, err.subcode);
    }
    unsafe { CStr::from_ptr(err.message) }
        .to_string_lossy()
        .into_owned()
}

#[cfg(feature = "heif-native")]
pub(crate) fn hdr_buffer_from_monochrome(
    handle: *const libheif_sys::heif_image_handle,
    metadata: HdrImageMetadata,
    image: *const libheif_sys::heif_image,
) -> Result<HdrImageBuffer, String> {
    use libheif_sys::heif_channel_Y;

    let width_i = unsafe { libheif_sys::heif_image_get_width(image, heif_channel_Y) };
    let height_i = unsafe { libheif_sys::heif_image_get_height(image, heif_channel_Y) };
    if width_i <= 0 || height_i <= 0 {
        return Err("libheif monochrome image has zero size".to_string());
    }
    crate::constants::validate_static_decode_dimensions(width_i as u32, height_i as u32)?;

    let mut stride = 0usize;
    let plane =
        unsafe { libheif_sys::heif_image_get_plane_readonly2(image, heif_channel_Y, &mut stride) };
    if plane.is_null() {
        return Err("libheif monochrome image missing Y plane".to_string());
    }

    let width = width_i as u32;
    let height = height_i as u32;
    let span = planar_storage_span_bytes(image, heif_channel_Y);
    let scale = planar_scale_from_depth(planar_semantic_depth_bits(image, handle, heif_channel_Y)?);
    let w = width as usize;
    let h = height as usize;
    if stride < span.saturating_mul(w.max(1)) {
        return Err("libheif monochrome stride too small".to_string());
    }

    let rgba_f32_len = super::heif_checked_rgba_buffer_len(w, h)?;
    let row_len = super::heif_checked_rgba_row_len(w)?;
    let mut rgba_f32 = vec![0.0_f32; rgba_f32_len];
    let plane = SendReadonlyPtr::new(plane);
    parallel_row_chunks_mut(h, row_len, &mut rgba_f32, |y, row_dst| {
        let row_base = unsafe { plane.get().add(y * stride) };
        for x in 0..w {
            let yn = planar_read_sample(row_base, x, stride, span)?;
            let v = yn as f32 / scale.max(1.0);
            let dst = x * 4;
            row_dst[dst] = v;
            row_dst[dst + 1] = v;
            row_dst[dst + 2] = v;
            row_dst[dst + 3] = 1.0;
        }
        Ok(())
    })?;

    let color_space = metadata.color_space_hint();
    Ok(HdrImageBuffer {
        width,
        height,
        format: HdrPixelFormat::Rgba32Float,
        color_space,
        metadata,
        rgba_f32: Arc::new(rgba_f32),
    })
}

#[cfg(feature = "heif-native")]
pub(crate) fn hdr_buffer_from_interleaved_rgb16(
    handle: *const libheif_sys::heif_image_handle,
    metadata: HdrImageMetadata,
    image: *const libheif_sys::heif_image,
    components: u8,
    big_endian: bool,
) -> Result<HdrImageBuffer, String> {
    if components != 3 && components != 4 {
        return Err(format!(
            "unsupported interleaved 16-bit component count ({components}); expected 3 (RGB) or 4 (RGBA)"
        ));
    }

    let width_i = unsafe { libheif_sys::heif_image_get_primary_width(image) };
    let height_i = unsafe { libheif_sys::heif_image_get_primary_height(image) };
    if width_i <= 0 || height_i <= 0 {
        return Err("libheif decoded zero-sized image".to_string());
    }
    crate::constants::validate_static_decode_dimensions(width_i as u32, height_i as u32)?;
    let mut stride = 0_usize;
    let plane = unsafe {
        libheif_sys::heif_image_get_plane_readonly2(
            image,
            libheif_sys::heif_channel_interleaved,
            &mut stride,
        )
    };
    if plane.is_null() {
        return Err("libheif did not expose an interleaved RGB/RGBA plane".to_string());
    }

    let width = width_i as u32;
    let height = height_i as u32;
    let bytes_per_pixel = (components as usize) * std::mem::size_of::<u16>();
    let row_bytes = (width as usize)
        .checked_mul(bytes_per_pixel)
        .ok_or_else(|| {
            format!("HEIF row byte size overflow: width={width} bpp={bytes_per_pixel}")
        })?;
    if stride < row_bytes {
        return Err(format!(
            "libheif row stride too small: got {stride}, expected at least {row_bytes}",
        ));
    }

    let bit_depth = heif_sample_bit_depth(image, handle)?;
    let scale = ((1_u32 << bit_depth.min(16)) - 1) as f32;
    let read_u16 = |px: &[u8], offset: usize| -> u16 {
        if big_endian {
            u16::from_be_bytes([px[offset], px[offset + 1]])
        } else {
            u16::from_le_bytes([px[offset], px[offset + 1]])
        }
    };
    let w = width as usize;
    let h = height as usize;
    let rgba_f32_len = super::heif_checked_rgba_buffer_len(w, h)?;
    let row_len = super::heif_checked_rgba_row_len(w)?;
    let mut rgba_f32 = vec![0.0_f32; rgba_f32_len];
    let plane = SendReadonlyPtr::new(plane);
    parallel_row_chunks_mut(h, row_len, &mut rgba_f32, |y, row_dst| {
        let row = unsafe { std::slice::from_raw_parts(plane.get().add(y * stride), row_bytes) };
        for (x, px) in row.chunks_exact(bytes_per_pixel).enumerate() {
            let dst = x * 4;
            row_dst[dst] = read_u16(px, 0) as f32 / scale;
            row_dst[dst + 1] = read_u16(px, 2) as f32 / scale;
            row_dst[dst + 2] = read_u16(px, 4) as f32 / scale;
            row_dst[dst + 3] = if components == 4 {
                read_u16(px, 6) as f32 / scale
            } else {
                1.0
            };
        }
        Ok(())
    })?;

    let color_space = metadata.color_space_hint();
    Ok(HdrImageBuffer {
        width,
        height,
        format: HdrPixelFormat::Rgba32Float,
        color_space,
        metadata,
        rgba_f32: Arc::new(rgba_f32),
    })
}

#[cfg(feature = "heif-native")]
fn rgba8_from_interleaved_rgb8(
    image: *const libheif_sys::heif_image,
    components: u8,
) -> Result<DecodedImage, String> {
    if components != 3 && components != 4 {
        return Err(format!(
            "unsupported interleaved 8-bit component count ({components})"
        ));
    }

    let width_i = unsafe { libheif_sys::heif_image_get_primary_width(image) };
    let height_i = unsafe { libheif_sys::heif_image_get_primary_height(image) };
    if width_i <= 0 || height_i <= 0 {
        return Err("libheif decoded zero-sized image".to_string());
    }
    crate::constants::validate_static_decode_dimensions(width_i as u32, height_i as u32)?;
    let width = width_i as u32;
    let height = height_i as u32;

    let mut stride = 0_usize;
    let plane = unsafe {
        libheif_sys::heif_image_get_plane_readonly2(
            image,
            libheif_sys::heif_channel_interleaved,
            &mut stride,
        )
    };
    if plane.is_null() {
        return Err("libheif missing interleaved plane".to_string());
    }

    let bytes_per_pixel = components as usize;
    let row_bytes = (width as usize)
        .checked_mul(bytes_per_pixel)
        .ok_or_else(|| {
            format!("HEIF row byte size overflow: width={width} bpp={bytes_per_pixel}")
        })?;
    if stride < row_bytes {
        return Err(format!(
            "libheif row stride too small: got {stride}, need {row_bytes}"
        ));
    }

    let w = width as usize;
    let h = height as usize;
    let rgba_len = super::heif_checked_rgba_buffer_len(w, h)?;
    let row_len = super::heif_checked_rgba_row_len(w)?;
    let mut rgba = vec![0_u8; rgba_len];
    let plane = SendReadonlyPtr::new(plane);
    parallel_row_chunks_mut(h, row_len, &mut rgba, |y, row_dst| {
        let row = unsafe { std::slice::from_raw_parts(plane.get().add(y * stride), row_bytes) };
        for (x, px) in row.chunks_exact(bytes_per_pixel).enumerate() {
            let dst = x * 4;
            row_dst[dst] = px[0];
            row_dst[dst + 1] = px[1];
            row_dst[dst + 2] = px[2];
            row_dst[dst + 3] = if components == 4 { px[3] } else { 255 };
        }
        Ok(())
    })?;
    Ok(DecodedImage::new(width, height, rgba))
}

#[cfg(feature = "heif-native")]
fn rgba8_from_interleaved_rgb16(
    handle: *const libheif_sys::heif_image_handle,
    image: *const libheif_sys::heif_image,
    components: u8,
    big_endian: bool,
) -> Result<DecodedImage, String> {
    if components != 3 && components != 4 {
        return Err(format!(
            "unsupported interleaved 16-bit component count ({components})"
        ));
    }

    let width_i = unsafe { libheif_sys::heif_image_get_primary_width(image) };
    let height_i = unsafe { libheif_sys::heif_image_get_primary_height(image) };
    if width_i <= 0 || height_i <= 0 {
        return Err("libheif decoded zero-sized image".to_string());
    }
    crate::constants::validate_static_decode_dimensions(width_i as u32, height_i as u32)?;
    let width = width_i as u32;
    let height = height_i as u32;

    let mut stride = 0_usize;
    let plane = unsafe {
        libheif_sys::heif_image_get_plane_readonly2(
            image,
            libheif_sys::heif_channel_interleaved,
            &mut stride,
        )
    };
    if plane.is_null() {
        return Err("libheif missing interleaved plane".to_string());
    }

    let bytes_per_pixel = (components as usize) * std::mem::size_of::<u16>();
    let row_bytes = (width as usize)
        .checked_mul(bytes_per_pixel)
        .ok_or_else(|| {
            format!("HEIF row byte size overflow: width={width} bpp={bytes_per_pixel}")
        })?;
    if stride < row_bytes {
        return Err(format!(
            "libheif row stride too small: got {stride}, need {row_bytes}"
        ));
    }

    let bit_depth = heif_sample_bit_depth(image, handle)?.clamp(1, 16);
    let maxv = ((1_u32 << bit_depth.min(16)) - 1).max(1) as f32;
    let read_u16 = |px: &[u8], offset: usize| -> u16 {
        if big_endian {
            u16::from_be_bytes([px[offset], px[offset + 1]])
        } else {
            u16::from_le_bytes([px[offset], px[offset + 1]])
        }
    };

    let w = width as usize;
    let h = height as usize;
    let rgba_len = super::heif_checked_rgba_buffer_len(w, h)?;
    let row_len = super::heif_checked_rgba_row_len(w)?;
    let mut rgba = vec![0_u8; rgba_len];
    let plane = SendReadonlyPtr::new(plane);
    parallel_row_chunks_mut(h, row_len, &mut rgba, |y, row_dst| {
        let row = unsafe { std::slice::from_raw_parts(plane.get().add(y * stride), row_bytes) };
        for (x, px) in row.chunks_exact(bytes_per_pixel).enumerate() {
            let dst = x * 4;
            row_dst[dst] = (read_u16(px, 0) as f32 / maxv * 255.0)
                .round()
                .clamp(0.0, 255.0) as u8;
            row_dst[dst + 1] = (read_u16(px, 2) as f32 / maxv * 255.0)
                .round()
                .clamp(0.0, 255.0) as u8;
            row_dst[dst + 2] = (read_u16(px, 4) as f32 / maxv * 255.0)
                .round()
                .clamp(0.0, 255.0) as u8;
            row_dst[dst + 3] = if components == 4 {
                (read_u16(px, 6) as f32 / maxv * 255.0)
                    .round()
                    .clamp(0.0, 255.0) as u8
            } else {
                255
            };
        }
        Ok(())
    })?;
    Ok(DecodedImage::new(width, height, rgba))
}

#[cfg(feature = "heif-native")]
fn rgba8_from_monochrome(image: *const libheif_sys::heif_image) -> Result<DecodedImage, String> {
    let width_i = unsafe { libheif_sys::heif_image_get_width(image, libheif_sys::heif_channel_Y) };
    let height_i =
        unsafe { libheif_sys::heif_image_get_height(image, libheif_sys::heif_channel_Y) };
    if width_i <= 0 || height_i <= 0 {
        return Err("libheif monochrome image has zero size".to_string());
    }
    crate::constants::validate_static_decode_dimensions(width_i as u32, height_i as u32)?;
    let width = width_i as u32;
    let height = height_i as u32;

    let mut stride = 0_usize;
    let plane = unsafe {
        libheif_sys::heif_image_get_plane_readonly2(image, libheif_sys::heif_channel_Y, &mut stride)
    };
    if plane.is_null() || stride < width as usize {
        return Err("libheif monochrome image missing Y plane".to_string());
    }

    let w = width as usize;
    let h = height as usize;
    let rgba_len = super::heif_checked_rgba_buffer_len(w, h)?;
    let row_len = super::heif_checked_rgba_row_len(w)?;
    let mut rgba = vec![0_u8; rgba_len];
    let plane = SendReadonlyPtr::new(plane);
    parallel_row_chunks_mut(h, row_len, &mut rgba, |y, row_dst| {
        let row = unsafe { std::slice::from_raw_parts(plane.get().add(y * stride), w) };
        for (x, &lum) in row.iter().enumerate() {
            let dst = x * 4;
            row_dst[dst] = lum;
            row_dst[dst + 1] = lum;
            row_dst[dst + 2] = lum;
            row_dst[dst + 3] = 255;
        }
        Ok(())
    })?;
    Ok(DecodedImage::new(width, height, rgba))
}

#[cfg(feature = "heif-native")]
fn rgba8_from_planar_rgb444(
    handle: *const libheif_sys::heif_image_handle,
    image: *const libheif_sys::heif_image,
) -> Result<DecodedImage, String> {
    use libheif_sys::{heif_channel_Alpha, heif_channel_B, heif_channel_G, heif_channel_R};

    for ch in [heif_channel_R, heif_channel_G, heif_channel_B] {
        if unsafe { libheif_sys::heif_image_has_channel(image, ch) } == 0 {
            return Err("planar RGB444: missing R/G/B channel".to_string());
        }
    }

    let width_i = unsafe { libheif_sys::heif_image_get_width(image, heif_channel_R) };
    let height_i = unsafe { libheif_sys::heif_image_get_height(image, heif_channel_R) };
    if width_i <= 0 || height_i <= 0 {
        return Err("planar RGB: zero-sized plane".to_string());
    }
    crate::constants::validate_static_decode_dimensions(width_i as u32, height_i as u32)?;
    let w = width_i as usize;
    let h = height_i as usize;
    let has_alpha = unsafe { libheif_sys::heif_image_has_channel(image, heif_channel_Alpha) != 0 };

    let mut stride_r = 0usize;
    let ptr_r = unsafe {
        libheif_sys::heif_image_get_plane_readonly2(image, heif_channel_R, &mut stride_r)
    };
    let mut stride_g = 0usize;
    let ptr_g = unsafe {
        libheif_sys::heif_image_get_plane_readonly2(image, heif_channel_G, &mut stride_g)
    };
    let mut stride_b = 0usize;
    let ptr_b = unsafe {
        libheif_sys::heif_image_get_plane_readonly2(image, heif_channel_B, &mut stride_b)
    };
    if ptr_r.is_null() || ptr_g.is_null() || ptr_b.is_null() {
        return Err("planar RGB: null plane pointer".to_string());
    }

    let span_r = planar_storage_span_bytes(image, heif_channel_R);
    let span_g = planar_storage_span_bytes(image, heif_channel_G);
    let span_b = planar_storage_span_bytes(image, heif_channel_B);
    let scale_r =
        planar_scale_from_depth(planar_semantic_depth_bits(image, handle, heif_channel_R)?);
    let scale_g =
        planar_scale_from_depth(planar_semantic_depth_bits(image, handle, heif_channel_G)?);
    let scale_b =
        planar_scale_from_depth(planar_semantic_depth_bits(image, handle, heif_channel_B)?);

    let alpha_pack = if has_alpha {
        let mut stride_a = 0usize;
        let ptr_a = unsafe {
            libheif_sys::heif_image_get_plane_readonly2(image, heif_channel_Alpha, &mut stride_a)
        };
        if ptr_a.is_null() || stride_a == 0 {
            None
        } else {
            let span_a = planar_storage_span_bytes(image, heif_channel_Alpha);
            let scale_a = planar_scale_from_depth(planar_semantic_depth_bits(
                image,
                handle,
                heif_channel_Alpha,
            )?);
            Some((SendReadonlyPtr::new(ptr_a), stride_a, span_a, scale_a))
        }
    } else {
        None
    };

    let sample_to_u8 = |value: u32, scale: f32| -> u8 {
        (value as f32 / scale.max(1.0) * 255.0)
            .round()
            .clamp(0.0, 255.0) as u8
    };

    let ptr_r = SendReadonlyPtr::new(ptr_r);
    let ptr_g = SendReadonlyPtr::new(ptr_g);
    let ptr_b = SendReadonlyPtr::new(ptr_b);
    let rgba_len = super::heif_checked_rgba_buffer_len(w, h)?;
    let row_len = super::heif_checked_rgba_row_len(w)?;
    let mut rgba = vec![0_u8; rgba_len];
    parallel_row_chunks_mut(h, row_len, &mut rgba, |y, row_dst| {
        let row_r = unsafe { ptr_r.get().byte_add(y * stride_r) };
        let row_g = unsafe { ptr_g.get().byte_add(y * stride_g) };
        let row_b = unsafe { ptr_b.get().byte_add(y * stride_b) };
        for x in 0..w {
            let rn = planar_read_sample(row_r, x, stride_r, span_r)?;
            let gn = planar_read_sample(row_g, x, stride_g, span_g)?;
            let bn = planar_read_sample(row_b, x, stride_b, span_b)?;
            let dst = x * 4;
            row_dst[dst] = sample_to_u8(rn, scale_r);
            row_dst[dst + 1] = sample_to_u8(gn, scale_g);
            row_dst[dst + 2] = sample_to_u8(bn, scale_b);
            row_dst[dst + 3] = if let Some((ap_base, sar, span_a, scale_a)) = alpha_pack {
                let row_a = unsafe { ap_base.get().byte_add(y * sar) };
                let an = planar_read_sample(row_a, x, sar, span_a)?;
                sample_to_u8(an, scale_a)
            } else {
                255
            };
        }
        Ok(())
    })?;
    Ok(DecodedImage::new(width_i as u32, height_i as u32, rgba))
}

#[cfg(feature = "heif-native")]
pub(crate) fn hdr_buffer_from_interleaved_rgb8_packed(
    handle: *const libheif_sys::heif_image_handle,
    metadata: HdrImageMetadata,
    image: *const libheif_sys::heif_image,
    components: u8,
) -> Result<HdrImageBuffer, String> {
    if components != 3 && components != 4 {
        return Err(format!(
            "unsupported interleaved 8-bit component count ({components}); expected 3 (RGB) or 4 (RGBA)"
        ));
    }

    let width_i = unsafe { libheif_sys::heif_image_get_primary_width(image) };
    let height_i = unsafe { libheif_sys::heif_image_get_primary_height(image) };
    if width_i <= 0 || height_i <= 0 {
        return Err("libheif decoded zero-sized image".to_string());
    }
    crate::constants::validate_static_decode_dimensions(width_i as u32, height_i as u32)?;
    let mut stride = 0_usize;
    let plane = unsafe {
        libheif_sys::heif_image_get_plane_readonly2(
            image,
            libheif_sys::heif_channel_interleaved,
            &mut stride,
        )
    };
    if plane.is_null() {
        return Err("libheif did not expose an interleaved RGB/RGBA plane".to_string());
    }

    let width = width_i as u32;
    let height = height_i as u32;
    let bytes_per_pixel = components as usize;
    let row_bytes = (width as usize)
        .checked_mul(bytes_per_pixel)
        .ok_or_else(|| {
            format!("HEIF row byte size overflow: width={width} bpp={bytes_per_pixel}")
        })?;
    if stride < row_bytes {
        return Err(format!(
            "libheif row stride too small: got {stride}, expected at least {row_bytes}",
        ));
    }

    let bit_depth = heif_sample_bit_depth(image, handle)?.clamp(1, 8);
    let scale = ((1_u32 << bit_depth) - 1) as f32;
    let w = width as usize;
    let h = height as usize;
    let rgba_f32_len = super::heif_checked_rgba_buffer_len(w, h)?;
    let row_len = super::heif_checked_rgba_row_len(w)?;
    let mut rgba_f32 = vec![0.0_f32; rgba_f32_len];
    let plane = SendReadonlyPtr::new(plane);
    parallel_row_chunks_mut(h, row_len, &mut rgba_f32, |y, row_dst| {
        let row = unsafe { std::slice::from_raw_parts(plane.get().add(y * stride), row_bytes) };
        for (x, px) in row.chunks_exact(bytes_per_pixel).enumerate() {
            let dst = x * 4;
            row_dst[dst] = px[0] as f32 / scale;
            row_dst[dst + 1] = px[1] as f32 / scale;
            row_dst[dst + 2] = px[2] as f32 / scale;
            row_dst[dst + 3] = if components == 4 {
                px[3] as f32 / scale
            } else {
                1.0
            };
        }
        Ok(())
    })?;

    let color_space = metadata.color_space_hint();
    Ok(HdrImageBuffer {
        width,
        height,
        format: HdrPixelFormat::Rgba32Float,
        color_space,
        metadata,
        rgba_f32: Arc::new(rgba_f32),
    })
}

#[cfg(feature = "heif-native")]
pub(crate) fn planar_storage_span_bytes(
    image: *const libheif_sys::heif_image,
    channel: libheif_sys::heif_channel,
) -> usize {
    let bpp = unsafe { libheif_sys::heif_image_get_bits_per_pixel(image, channel).max(8) };
    ((bpp + 7) / 8) as usize
}

#[cfg(feature = "heif-native")]
pub(crate) fn planar_semantic_depth_bits(
    image: *const libheif_sys::heif_image,
    handle: *const libheif_sys::heif_image_handle,
    channel: libheif_sys::heif_channel,
) -> Result<i32, String> {
    let decoded_range = unsafe { libheif_sys::heif_image_get_bits_per_pixel_range(image, channel) };
    let luma = unsafe { libheif_sys::heif_image_handle_get_luma_bits_per_pixel(handle) };
    let chroma = unsafe { libheif_sys::heif_image_handle_get_chroma_bits_per_pixel(handle) };
    let per_ch = decoded_range.max(luma).max(chroma).max(8);
    Ok(per_ch.min(32))
}

#[cfg(feature = "heif-native")]
pub(crate) fn planar_scale_from_depth(semantic_bits: i32) -> f32 {
    let d = semantic_bits.clamp(1, 32);
    let maxv = (1_u64 << d as u32).saturating_sub(1).max(1);
    maxv as f32
}

#[cfg(feature = "heif-native")]
pub(crate) fn planar_read_sample(
    row_base: *const u8,
    x: usize,
    stride_bytes: usize,
    storage_span: usize,
) -> Result<u32, String> {
    let offset = x
        .checked_mul(storage_span)
        .ok_or_else(|| "planar sample offset overflow".to_string())?;
    if offset + storage_span > stride_bytes {
        return Err("planar sample read past row stride".to_string());
    }
    unsafe {
        match storage_span {
            1 => Ok(*row_base.add(offset) as u32),
            2 => {
                let bytes = std::slice::from_raw_parts(row_base.add(offset), 2);
                Ok(u16::from_le_bytes([bytes[0], bytes[1]]) as u32)
            }
            4 => {
                let bytes = std::slice::from_raw_parts(row_base.add(offset), 4);
                Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
            }
            n => Err(format!(
                "unsupported planar sample storage width ({n}); extend reader for this HEIF variant"
            )),
        }
    }
}

#[cfg(feature = "heif-native")]
pub(crate) fn hdr_buffer_from_planar_rgb444(
    handle: *const libheif_sys::heif_image_handle,
    metadata: HdrImageMetadata,
    image: *const libheif_sys::heif_image,
) -> Result<HdrImageBuffer, String> {
    use libheif_sys::{heif_channel_Alpha, heif_channel_B, heif_channel_G, heif_channel_R};

    for ch in [heif_channel_R, heif_channel_G, heif_channel_B] {
        if unsafe { libheif_sys::heif_image_has_channel(image, ch) } == 0 {
            return Err("planar RGB444: missing R/G/B channel".to_string());
        }
    }

    let width_i = unsafe { libheif_sys::heif_image_get_width(image, heif_channel_R) };
    let height_i = unsafe { libheif_sys::heif_image_get_height(image, heif_channel_R) };
    if width_i <= 0 || height_i <= 0 {
        return Err("planar RGB: zero-sized plane".to_string());
    }
    crate::constants::validate_static_decode_dimensions(width_i as u32, height_i as u32)?;
    let w = width_i as usize;
    let h = height_i as usize;

    let has_alpha = unsafe { libheif_sys::heif_image_has_channel(image, heif_channel_Alpha) != 0 };

    let mut stride_r = 0usize;
    let ptr_r = unsafe {
        libheif_sys::heif_image_get_plane_readonly2(image, heif_channel_R, &mut stride_r)
    };
    let mut stride_g = 0usize;
    let ptr_g = unsafe {
        libheif_sys::heif_image_get_plane_readonly2(image, heif_channel_G, &mut stride_g)
    };
    let mut stride_b = 0usize;
    let ptr_b = unsafe {
        libheif_sys::heif_image_get_plane_readonly2(image, heif_channel_B, &mut stride_b)
    };
    let alpha_pack = if has_alpha {
        let mut stride_a = 0usize;
        let ptr_a = unsafe {
            libheif_sys::heif_image_get_plane_readonly2(image, heif_channel_Alpha, &mut stride_a)
        };
        if ptr_a.is_null() || stride_a == 0 {
            None
        } else {
            let span_a_val = planar_storage_span_bytes(image, heif_channel_Alpha);
            let scale_a_val = planar_scale_from_depth(planar_semantic_depth_bits(
                image,
                handle,
                heif_channel_Alpha,
            )?);
            Some((
                SendReadonlyPtr::new(ptr_a),
                stride_a,
                span_a_val,
                scale_a_val,
            ))
        }
    } else {
        None
    };

    if ptr_r.is_null() || ptr_g.is_null() || ptr_b.is_null() {
        return Err("planar RGB: null plane pointer".to_string());
    }

    let span_r = planar_storage_span_bytes(image, heif_channel_R);
    let span_g = planar_storage_span_bytes(image, heif_channel_G);
    let span_b = planar_storage_span_bytes(image, heif_channel_B);

    let scale_r =
        planar_scale_from_depth(planar_semantic_depth_bits(image, handle, heif_channel_R)?);
    let scale_g =
        planar_scale_from_depth(planar_semantic_depth_bits(image, handle, heif_channel_G)?);
    let scale_b =
        planar_scale_from_depth(planar_semantic_depth_bits(image, handle, heif_channel_B)?);

    let min_stride_need_r = span_r * w.max(1);
    let min_stride_need_g = span_g * w.max(1);
    let min_stride_need_b = span_b * w.max(1);
    if stride_r < min_stride_need_r || stride_g < min_stride_need_g || stride_b < min_stride_need_b
    {
        return Err("planar RGB: stride inconsistent with dimensions".to_string());
    }
    if let Some((_, alpha_stride_px, alpha_span_px, _)) = alpha_pack
        && alpha_stride_px < alpha_span_px * w.max(1)
    {
        return Err("planar RGB: alpha stride inconsistent".to_string());
    }

    let ptr_r = SendReadonlyPtr::new(ptr_r);
    let ptr_g = SendReadonlyPtr::new(ptr_g);
    let ptr_b = SendReadonlyPtr::new(ptr_b);
    let rgba_f32_len = super::heif_checked_rgba_buffer_len(w, h)?;
    let row_len = super::heif_checked_rgba_row_len(w)?;
    let mut rgba_f32 = vec![0.0_f32; rgba_f32_len];

    parallel_row_chunks_mut(h, row_len, &mut rgba_f32, |y, row_dst| {
        let row_r = unsafe { ptr_r.get().byte_add(y * stride_r) };
        let row_g = unsafe { ptr_g.get().byte_add(y * stride_g) };
        let row_b = unsafe { ptr_b.get().byte_add(y * stride_b) };

        for x_px in 0..w {
            let rn = planar_read_sample(row_r, x_px, stride_r, span_r)?;
            let gn = planar_read_sample(row_g, x_px, stride_g, span_g)?;
            let bn = planar_read_sample(row_b, x_px, stride_b, span_b)?;

            let dst = x_px * 4;
            row_dst[dst] = rn as f32 / scale_r.max(1.0);
            row_dst[dst + 1] = gn as f32 / scale_g.max(1.0);
            row_dst[dst + 2] = bn as f32 / scale_b.max(1.0);

            row_dst[dst + 3] = if let Some((ap_base, sar, spam_a_px, scl_a)) = alpha_pack {
                let row_a = unsafe { ap_base.get().byte_add(y * sar) };
                let an = planar_read_sample(row_a, x_px, sar, spam_a_px)?;
                (an as f32 / scl_a.max(1.0)).clamp(0.0, 1.0)
            } else {
                1.0
            };
        }
        Ok(())
    })?;

    let color_space = metadata.color_space_hint();
    Ok(HdrImageBuffer {
        width: width_i as u32,
        height: height_i as u32,
        format: HdrPixelFormat::Rgba32Float,
        color_space,
        metadata,
        rgba_f32: Arc::new(rgba_f32),
    })
}
