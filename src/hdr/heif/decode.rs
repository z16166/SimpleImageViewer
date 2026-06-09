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
use super::session::append_heif_unci_build_hint;
use super::metadata::heif_sample_bit_depth;
use super::ycbcr::hdr_buffer_from_ycbcr;


use crate::hdr::types::HdrImageMetadata;
#[cfg(feature = "heif-native")]
use crate::hdr::types::{HdrImageBuffer, HdrPixelFormat};
#[cfg(feature = "heif-native")]
use std::sync::Arc;

/// Decode the primary HEIF tile to HDR float RGBA. Tries interleaved 16-bit RGBA first, then other
/// interleaved layouts, YCbCr (`4:2:2` / `4:4:4` / `4:2:0`), planar RGB, and 8-bit interleaved fallbacks.
#[cfg(feature = "heif-native")]
pub(crate) fn decode_primary_heif_to_hdr(
    handle: *const libheif_sys::heif_image_handle,
    metadata: HdrImageMetadata,
    decode_options: *const libheif_sys::heif_decoding_options,
) -> Result<HdrImageBuffer, String> {
    let interleaved_aa =
        match decode_primary_interleaved_rrggbbaa_le(handle, &metadata, decode_options) {
            Ok(img) => return Ok(img),
            Err(e) => e,
        };

    let interleaved_rgb16 =
        match decode_primary_interleaved_rrggbbe_le(handle, &metadata, decode_options) {
            Ok(img) => return Ok(img),
            Err(e) => e,
        };

    let y422 = match decode_primary_ycbcr(
        handle,
        &metadata,
        libheif_sys::heif_chroma_422,
        decode_options,
    ) {
        Ok(b) => return Ok(b),
        Err(e) => e,
    };

    let y444 = match decode_primary_ycbcr(
        handle,
        &metadata,
        libheif_sys::heif_chroma_444,
        decode_options,
    ) {
        Ok(b) => return Ok(b),
        Err(e) => e,
    };

    let y420 = match decode_primary_ycbcr(
        handle,
        &metadata,
        libheif_sys::heif_chroma_420,
        decode_options,
    ) {
        Ok(b) => return Ok(b),
        Err(e) => e,
    };

    let planar = match decode_primary_planar_rgb444(handle, &metadata, decode_options) {
        Ok(b) => return Ok(b),
        Err(e) => e,
    };

    let rgba8 = match decode_primary_interleaved_rgba8(handle, &metadata, decode_options) {
        Ok(b) => return Ok(b),
        Err(e) => e,
    };

    let rgb8 = match decode_primary_interleaved_rgb8(handle, &metadata, decode_options) {
        Ok(b) => return Ok(b),
        Err(e) => e,
    };

    Err(append_heif_unci_build_hint(format!(
        "decode HEIF (all targets failed): RGBA16 interleaved: {interleaved_aa}; RGB16 interleaved RRGGBB LE: {interleaved_rgb16}; YCbCr 422: {y422}; YCbCr 444: {y444}; YCbCr 420: {y420}; planar RGB444: {planar}; RGBA8 interleaved: {rgba8}; RGB8 interleaved: {rgb8}"
    )))
}

#[cfg(feature = "heif-native")]
pub(crate) struct RawHeifImage(pub *mut libheif_sys::heif_image);

#[cfg(feature = "heif-native")]
impl Drop for RawHeifImage {
    fn drop(&mut self) {
        unsafe { libheif_sys::heif_image_release(self.0) };
    }
}

#[cfg(feature = "heif-native")]
pub(crate) fn heif_try_decode_into(
    handle: *const libheif_sys::heif_image_handle,
    cs: libheif_sys::heif_colorspace,
    chroma: libheif_sys::heif_chroma,
    decode_options: *const libheif_sys::heif_decoding_options,
    _detail: &'static str,
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
    Ok(RawHeifImage(image_ptr))
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
pub(crate) fn decode_primary_interleaved_rrggbbaa_le(
    handle: *const libheif_sys::heif_image_handle,
    metadata: &HdrImageMetadata,
    decode_options: *const libheif_sys::heif_decoding_options,
) -> Result<HdrImageBuffer, String> {
    let img = match heif_try_decode_into(
        handle,
        libheif_sys::heif_colorspace_RGB,
        libheif_sys::heif_chroma_interleaved_RRGGBBAA_LE,
        decode_options,
        "RGBA16",
    ) {
        Ok(i) => i,
        Err(e) => {
            return Err(format!(
                "Failed to decode HEIF image as interleaved 16-bit RGBA ({})",
                heif_err_to_plain(e),
            ));
        }
    };

    hdr_buffer_from_interleaved_rgb16_le(handle, metadata, img.0, 4)
}

#[cfg(feature = "heif-native")]
pub(crate) fn decode_primary_interleaved_rrggbbe_le(
    handle: *const libheif_sys::heif_image_handle,
    metadata: &HdrImageMetadata,
    decode_options: *const libheif_sys::heif_decoding_options,
) -> Result<HdrImageBuffer, String> {
    let img = match heif_try_decode_into(
        handle,
        libheif_sys::heif_colorspace_RGB,
        libheif_sys::heif_chroma_interleaved_RRGGBB_LE,
        decode_options,
        "RGB16 triple",
    ) {
        Ok(i) => i,
        Err(e) => {
            return Err(format!(
                "Failed to decode HEIF image as interleaved 16-bit RRGGBB LE ({})",
                heif_err_to_plain(e),
            ));
        }
    };

    hdr_buffer_from_interleaved_rgb16_le(handle, metadata, img.0, 3)
}

#[cfg(feature = "heif-native")]
pub(crate) fn decode_primary_interleaved_rgba8(
    handle: *const libheif_sys::heif_image_handle,
    metadata: &HdrImageMetadata,
    decode_options: *const libheif_sys::heif_decoding_options,
) -> Result<HdrImageBuffer, String> {
    let img = match heif_try_decode_into(
        handle,
        libheif_sys::heif_colorspace_RGB,
        libheif_sys::heif_chroma_interleaved_RGBA,
        decode_options,
        "RGBA8",
    ) {
        Ok(i) => i,
        Err(e) => {
            return Err(format!(
                "Failed to decode HEIF image as interleaved RGBA8 ({})",
                heif_err_to_plain(e),
            ));
        }
    };

    hdr_buffer_from_interleaved_rgb8_packed(handle, metadata, img.0, 4)
}

#[cfg(feature = "heif-native")]
pub(crate) fn decode_primary_interleaved_rgb8(
    handle: *const libheif_sys::heif_image_handle,
    metadata: &HdrImageMetadata,
    decode_options: *const libheif_sys::heif_decoding_options,
) -> Result<HdrImageBuffer, String> {
    let img = match heif_try_decode_into(
        handle,
        libheif_sys::heif_colorspace_RGB,
        libheif_sys::heif_chroma_interleaved_RGB,
        decode_options,
        "RGB8",
    ) {
        Ok(i) => i,
        Err(e) => {
            return Err(format!(
                "Failed to decode HEIF image as interleaved RGB8 ({})",
                heif_err_to_plain(e),
            ));
        }
    };

    hdr_buffer_from_interleaved_rgb8_packed(handle, metadata, img.0, 3)
}

#[cfg(feature = "heif-native")]
pub(crate) fn decode_primary_planar_rgb444(
    handle: *const libheif_sys::heif_image_handle,
    metadata: &HdrImageMetadata,
    decode_options: *const libheif_sys::heif_decoding_options,
) -> Result<HdrImageBuffer, String> {
    let img = match heif_try_decode_into(
        handle,
        libheif_sys::heif_colorspace_RGB,
        libheif_sys::heif_chroma_444,
        decode_options,
        "RGB444 planar",
    ) {
        Ok(i) => i,
        Err(e) => {
            return Err(format!(
                "Failed to decode HEIF image as planar RGB444 ({})",
                heif_err_to_plain(e),
            ));
        }
    };

    hdr_buffer_from_planar_rgb444(handle, metadata, img.0)
}

#[cfg(feature = "heif-native")]
pub(crate) fn decode_primary_ycbcr(
    handle: *const libheif_sys::heif_image_handle,
    metadata: &HdrImageMetadata,
    chroma: libheif_sys::heif_chroma,
    decode_options: *const libheif_sys::heif_decoding_options,
) -> Result<HdrImageBuffer, String> {
    let chroma_detail = chroma_plane_label(chroma);
    let img = match heif_try_decode_into(
        handle,
        libheif_sys::heif_colorspace_YCbCr,
        chroma,
        decode_options,
        chroma_detail,
    ) {
        Ok(i) => i,
        Err(e) => {
            return Err(format!(
                "Failed to decode HEIF image as YCbCr ({chroma_detail}) ({})",
                heif_err_to_plain(e),
            ));
        }
    };

    hdr_buffer_from_ycbcr(handle, metadata, img.0, chroma)
}

#[cfg(feature = "heif-native")]
pub(crate) fn chroma_plane_label(chroma: libheif_sys::heif_chroma) -> &'static str {
    match chroma {
        c if c == libheif_sys::heif_chroma_420 => "420",
        c if c == libheif_sys::heif_chroma_422 => "422",
        c if c == libheif_sys::heif_chroma_444 => "444",
        _ => "YCbCr",
    }
}

#[cfg(feature = "heif-native")]
pub(crate) fn hdr_buffer_from_interleaved_rgb16_le(
    handle: *const libheif_sys::heif_image_handle,
    metadata: &HdrImageMetadata,
    image: *const libheif_sys::heif_image,
    components: u8,
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
    let row_bytes = width as usize * bytes_per_pixel;
    if stride < row_bytes {
        return Err(format!(
            "libheif row stride too small: got {stride}, expected at least {row_bytes}",
        ));
    }

    let bit_depth = heif_sample_bit_depth(image, handle)?;
    let scale = ((1_u32 << bit_depth.min(16)) - 1) as f32;
    let mut rgba_f32 = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height as usize {
        let row = unsafe { std::slice::from_raw_parts(plane.add(y * stride), row_bytes) };
        for px in row.chunks_exact(bytes_per_pixel) {
            rgba_f32.push(u16::from_le_bytes([px[0], px[1]]) as f32 / scale);
            rgba_f32.push(u16::from_le_bytes([px[2], px[3]]) as f32 / scale);
            rgba_f32.push(u16::from_le_bytes([px[4], px[5]]) as f32 / scale);
            if components == 4 {
                rgba_f32.push(u16::from_le_bytes([px[6], px[7]]) as f32 / scale);
            } else {
                rgba_f32.push(1.0);
            }
        }
    }

    let color_space = metadata.color_space_hint();
    Ok(HdrImageBuffer {
        width,
        height,
        format: HdrPixelFormat::Rgba32Float,
        color_space,
        metadata: metadata.clone(),
        rgba_f32: Arc::new(rgba_f32),
    })
}

#[cfg(feature = "heif-native")]
pub(crate) fn hdr_buffer_from_interleaved_rgb8_packed(
    handle: *const libheif_sys::heif_image_handle,
    metadata: &HdrImageMetadata,
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
    let row_bytes = width as usize * bytes_per_pixel;
    if stride < row_bytes {
        return Err(format!(
            "libheif row stride too small: got {stride}, expected at least {row_bytes}",
        ));
    }

    let bit_depth = heif_sample_bit_depth(image, handle)?.min(8).max(1);
    let scale = ((1_u32 << bit_depth as u32) - 1) as f32;
    let mut rgba_f32 = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height as usize {
        let row = unsafe { std::slice::from_raw_parts(plane.add(y * stride), row_bytes) };
        for px in row.chunks_exact(bytes_per_pixel) {
            rgba_f32.push(px[0] as f32 / scale);
            rgba_f32.push(px[1] as f32 / scale);
            rgba_f32.push(px[2] as f32 / scale);
            if components == 4 {
                rgba_f32.push(px[3] as f32 / scale);
            } else {
                rgba_f32.push(1.0);
            }
        }
    }

    let color_space = metadata.color_space_hint();
    Ok(HdrImageBuffer {
        width,
        height,
        format: HdrPixelFormat::Rgba32Float,
        color_space,
        metadata: metadata.clone(),
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
    metadata: &HdrImageMetadata,
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
            Some((ptr_a, stride_a, span_a_val, scale_a_val))
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

    let mut rgba_f32 = Vec::with_capacity(w * h * 4);

    for y in 0..h {
        let row_r = unsafe { ptr_r.byte_add(y * stride_r) };
        let row_g = unsafe { ptr_g.byte_add(y * stride_g) };
        let row_b = unsafe { ptr_b.byte_add(y * stride_b) };

        let min_stride_need_r = span_r * w.max(1);
        let min_stride_need_g = span_g * w.max(1);
        let min_stride_need_b = span_b * w.max(1);
        if stride_r < min_stride_need_r
            || stride_g < min_stride_need_g
            || stride_b < min_stride_need_b
        {
            return Err("planar RGB: stride inconsistent with dimensions".to_string());
        }

        if let Some((_, alpha_stride_px, alpha_span_px, _)) = alpha_pack
            && alpha_stride_px < alpha_span_px * w.max(1)
        {
            return Err("planar RGB: alpha stride inconsistent".to_string());
        }

        for x_px in 0..w {
            let rn = planar_read_sample(row_r, x_px, stride_r, span_r)?;
            let gn = planar_read_sample(row_g, x_px, stride_g, span_g)?;
            let bn = planar_read_sample(row_b, x_px, stride_b, span_b)?;

            rgba_f32.push(rn as f32 / scale_r.max(1.0));
            rgba_f32.push(gn as f32 / scale_g.max(1.0));
            rgba_f32.push(bn as f32 / scale_b.max(1.0));

            if let Some((ap_base, sar, spam_a_px, scl_a)) = alpha_pack {
                let row_a = unsafe { ap_base.byte_add(y * sar) };
                let an = planar_read_sample(row_a, x_px, sar, spam_a_px)?;
                rgba_f32.push((an as f32 / scl_a.max(1.0)).clamp(0.0, 1.0));
            } else {
                rgba_f32.push(1.0);
            }
        }
    }

    let color_space = metadata.color_space_hint();
    Ok(HdrImageBuffer {
        width: width_i as u32,
        height: height_i as u32,
        format: HdrPixelFormat::Rgba32Float,
        color_space,
        metadata: metadata.clone(),
        rgba_f32: Arc::new(rgba_f32),
    })
}

