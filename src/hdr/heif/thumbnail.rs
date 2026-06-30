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

//! libheif container thumbnail probe + SDR RGBA decode for directory-tree strip previews.

use std::path::Path;

use crate::loader::{DecodedImage, preview_aspect_matches_logical};

use super::decode::heif_try_decode_into;
use super::session::open_heif_primary_from_bytes;
use super::ycbcr::{HeifYcbcrMatrix, ycbcr_linear_to_rgb};

/// Outcome of a libheif container-thumbnail probe ([`preload-debug`] logs use [`label`](Self::label)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeifThumbProbe {
    ContainerUnreadable,
    NoThumbnail,
    ThumbnailHandleFailed,
    ThumbDimensionsInvalid,
    DecodeFailed,
    AspectRejected,
    DownsampleFailed,
    Found,
}

impl HeifThumbProbe {
    #[cfg_attr(not(feature = "preload-debug"), allow(dead_code))]
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::ContainerUnreadable => "container_unreadable",
            Self::NoThumbnail => "no_thumbnail",
            Self::ThumbnailHandleFailed => "thumbnail_handle_failed",
            Self::ThumbDimensionsInvalid => "thumb_dimensions_invalid",
            Self::DecodeFailed => "decode_failed",
            Self::AspectRejected => "aspect_rejected",
            Self::DownsampleFailed => "downsample_failed",
            Self::Found => "found",
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct HeifThumbProbeDetail {
    #[cfg_attr(not(feature = "preload-debug"), allow(dead_code))]
    pub thumb_count: Option<i32>,
    #[cfg_attr(not(feature = "preload-debug"), allow(dead_code))]
    pub thumb_id: Option<u32>,
    pub thumb_w: Option<u32>,
    pub thumb_h: Option<u32>,
    #[cfg_attr(not(feature = "preload-debug"), allow(dead_code))]
    pub primary_w: Option<u32>,
    #[cfg_attr(not(feature = "preload-debug"), allow(dead_code))]
    pub primary_h: Option<u32>,
    #[cfg_attr(not(feature = "preload-debug"), allow(dead_code))]
    pub decode_ms: Option<u32>,
}

type HeifStripThumbProbeResult = (
    Option<(DecodedImage, (u32, u32))>,
    HeifThumbProbe,
    HeifThumbProbeDetail,
);

type HeifInterleavedDecodeFn =
    fn(*const libheif_sys::heif_image, u8) -> Result<DecodedImage, String>;

pub(crate) fn probe_heif_strip_thumbnail_from_path(
    path: &Path,
    max_side: u32,
) -> HeifStripThumbProbeResult {
    let mmap = match crate::mmap_util::map_file(path) {
        Ok(data) => data,
        Err(_) => {
            return (
                None,
                HeifThumbProbe::ContainerUnreadable,
                HeifThumbProbeDetail::default(),
            );
        }
    };
    probe_heif_strip_thumbnail(mmap.as_ref(), max_side)
}

pub(crate) fn probe_heif_strip_thumbnail(bytes: &[u8], max_side: u32) -> HeifStripThumbProbeResult {
    #[cfg(feature = "preload-debug")]
    let decode_start = std::time::Instant::now();

    let (_ctx, primary) = match open_heif_primary_from_bytes(bytes) {
        Ok(opened) => opened,
        Err(_) => {
            return (
                None,
                HeifThumbProbe::ContainerUnreadable,
                HeifThumbProbeDetail::default(),
            );
        }
    };

    let primary_handle = primary.as_ptr();
    let logical = primary_logical_size(primary_handle);
    let mut detail = HeifThumbProbeDetail {
        primary_w: Some(logical.0),
        primary_h: Some(logical.1),
        ..HeifThumbProbeDetail::default()
    };

    if logical.0 == 0 || logical.1 == 0 {
        return (None, HeifThumbProbe::ThumbDimensionsInvalid, detail);
    }

    let thumb_count =
        unsafe { libheif_sys::heif_image_handle_get_number_of_thumbnails(primary_handle) };
    detail.thumb_count = Some(thumb_count);
    if thumb_count <= 0 {
        return (None, HeifThumbProbe::NoThumbnail, detail);
    }

    let mut thumb_id = 0_u32;
    let listed = unsafe {
        libheif_sys::heif_image_handle_get_list_of_thumbnail_IDs(primary_handle, &mut thumb_id, 1)
    };
    if listed <= 0 {
        return (None, HeifThumbProbe::NoThumbnail, detail);
    }
    detail.thumb_id = Some(thumb_id);

    let mut thumb_handle_ptr = std::ptr::null_mut();
    let thumb_err = unsafe {
        libheif_sys::heif_image_handle_get_thumbnail(
            primary_handle,
            thumb_id,
            &mut thumb_handle_ptr,
        )
    };
    if thumb_err.code != libheif_sys::heif_error_Ok || thumb_handle_ptr.is_null() {
        return (None, HeifThumbProbe::ThumbnailHandleFailed, detail);
    }
    let thumb_handle = unsafe { libheif_sys::HeifImageHandleGuard::from_ptr(thumb_handle_ptr) };

    let thumb_w =
        unsafe { libheif_sys::heif_image_handle_get_width(thumb_handle.as_ptr()) }.max(0) as u32;
    let thumb_h =
        unsafe { libheif_sys::heif_image_handle_get_height(thumb_handle.as_ptr()) }.max(0) as u32;
    detail.thumb_w = Some(thumb_w);
    detail.thumb_h = Some(thumb_h);
    if thumb_w == 0 || thumb_h == 0 {
        return (None, HeifThumbProbe::ThumbDimensionsInvalid, detail);
    }

    let decode_options = match libheif_sys::HeifDecodingOptionsGuard::new() {
        Some(options) => options,
        None => return (None, HeifThumbProbe::DecodeFailed, detail),
    };

    let decoded =
        match decode_thumbnail_handle_to_rgba8(thumb_handle.as_ptr(), decode_options.as_ptr()) {
            Ok(image) => image,
            Err(_) => return (None, HeifThumbProbe::DecodeFailed, detail),
        };

    if !preview_aspect_matches_logical(decoded.width, decoded.height, logical.0, logical.1) {
        return (None, HeifThumbProbe::AspectRejected, detail);
    }

    let preview = match crate::loader::downsample_decoded_for_strip(&decoded, max_side) {
        Ok(strip) => strip,
        Err(_) => return (None, HeifThumbProbe::DownsampleFailed, detail),
    };

    #[cfg(feature = "preload-debug")]
    {
        detail.decode_ms = Some(crate::loader::elapsed_ms_u32(decode_start));
    }

    (Some((preview, logical)), HeifThumbProbe::Found, detail)
}

fn primary_logical_size(handle: *const libheif_sys::heif_image_handle) -> (u32, u32) {
    let ispe_w = unsafe { libheif_sys::heif_image_handle_get_ispe_width(handle) };
    let ispe_h = unsafe { libheif_sys::heif_image_handle_get_ispe_height(handle) };
    if ispe_w > 0 && ispe_h > 0 {
        return (ispe_w as u32, ispe_h as u32);
    }
    let w = unsafe { libheif_sys::heif_image_handle_get_width(handle) }.max(0) as u32;
    let h = unsafe { libheif_sys::heif_image_handle_get_height(handle) }.max(0) as u32;
    (w, h)
}

fn decode_thumbnail_handle_to_rgba8(
    handle: *const libheif_sys::heif_image_handle,
    decode_options: *const libheif_sys::heif_decoding_options,
) -> Result<DecodedImage, String> {
    let attempts: &[(i32, i32, u8, HeifInterleavedDecodeFn)] = &[
        (
            libheif_sys::heif_colorspace_RGB,
            libheif_sys::heif_chroma_interleaved_RGBA,
            4,
            interleaved_image_to_rgba8,
        ),
        (
            libheif_sys::heif_colorspace_RGB,
            libheif_sys::heif_chroma_interleaved_RGB,
            3,
            interleaved_image_to_rgba8,
        ),
    ];

    let mut last_err = String::from("no decode attempts");
    for &(cs, chroma, components, convert) in attempts {
        match heif_try_decode_into(handle, cs, chroma, decode_options, "strip-thumb") {
            Ok(raw) => match convert(raw.as_ptr(), components) {
                Ok(decoded) => return Ok(decoded),
                Err(err) => last_err = err,
            },
            Err(err) => last_err = super::decode::heif_err_to_plain(err),
        }
    }

    match heif_try_decode_into(
        handle,
        libheif_sys::heif_colorspace_YCbCr,
        libheif_sys::heif_chroma_420,
        decode_options,
        "strip-thumb-ycbcr",
    ) {
        Ok(raw) => ycbcr420_image_to_rgba8(raw.as_ptr()),
        Err(err) => Err(super::decode::heif_err_to_plain(err)),
    }
    .or(Err(last_err))
}

fn interleaved_image_to_rgba8(
    image: *const libheif_sys::heif_image,
    components: u8,
) -> Result<DecodedImage, String> {
    let width_i = unsafe { libheif_sys::heif_image_get_primary_width(image) };
    let height_i = unsafe { libheif_sys::heif_image_get_primary_height(image) };
    if width_i <= 0 || height_i <= 0 {
        return Err("libheif thumbnail has zero size".to_string());
    }
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
        return Err("libheif thumbnail missing interleaved plane".to_string());
    }

    if components != 3 && components != 4 {
        return Err(format!(
            "unsupported interleaved component count ({components})"
        ));
    }
    let bytes_per_pixel = components as usize;
    let row_bytes = width as usize * bytes_per_pixel;
    if stride < row_bytes {
        return Err(format!(
            "libheif thumbnail stride too small: got {stride}, need {row_bytes}"
        ));
    }

    let mut rgba = vec![0_u8; width as usize * height as usize * 4];
    for y in 0..height as usize {
        let row = unsafe { std::slice::from_raw_parts(plane.add(y * stride), row_bytes) };
        for (x, px) in row.chunks_exact(bytes_per_pixel).enumerate() {
            let dst = (y * width as usize + x) * 4;
            rgba[dst] = px[0];
            rgba[dst + 1] = px[1];
            rgba[dst + 2] = px[2];
            rgba[dst + 3] = if components == 4 { px[3] } else { 255 };
        }
    }
    Ok(DecodedImage::new(width, height, rgba))
}

fn ycbcr420_image_to_rgba8(image: *const libheif_sys::heif_image) -> Result<DecodedImage, String> {
    let y_w = unsafe { libheif_sys::heif_image_get_width(image, libheif_sys::heif_channel_Y) };
    let y_h = unsafe { libheif_sys::heif_image_get_height(image, libheif_sys::heif_channel_Y) };
    if y_w <= 0 || y_h <= 0 {
        return Err("libheif YCbCr thumbnail has zero luma size".to_string());
    }
    let width = y_w as u32;
    let height = y_h as u32;

    let mut y_stride = 0_usize;
    let y_plane = unsafe {
        libheif_sys::heif_image_get_plane_readonly2(
            image,
            libheif_sys::heif_channel_Y,
            &mut y_stride,
        )
    };
    let mut cb_stride = 0_usize;
    let cb_plane = unsafe {
        libheif_sys::heif_image_get_plane_readonly2(
            image,
            libheif_sys::heif_channel_Cb,
            &mut cb_stride,
        )
    };
    let mut cr_stride = 0_usize;
    let cr_plane = unsafe {
        libheif_sys::heif_image_get_plane_readonly2(
            image,
            libheif_sys::heif_channel_Cr,
            &mut cr_stride,
        )
    };
    if y_plane.is_null() || cb_plane.is_null() || cr_plane.is_null() {
        return Err("libheif YCbCr thumbnail missing planes".to_string());
    }

    let matrix = HeifYcbcrMatrix::Bt709;
    let chroma_row_len = (width as usize).div_ceil(2);
    let mut rgba = vec![0_u8; width as usize * height as usize * 4];
    for y in 0..height as usize {
        let y_row =
            unsafe { std::slice::from_raw_parts(y_plane.add(y * y_stride), width as usize) };
        let cb_row = unsafe {
            std::slice::from_raw_parts(cb_plane.add((y / 2) * cb_stride), chroma_row_len)
        };
        let cr_row = unsafe {
            std::slice::from_raw_parts(cr_plane.add((y / 2) * cr_stride), chroma_row_len)
        };
        for x in 0..width as usize {
            let yy = y_row[x] as f32 / 255.0;
            let cb = cb_row[x / 2] as f32 / 255.0 - 0.5;
            let cr = cr_row[x / 2] as f32 / 255.0 - 0.5;
            let [r, g, b] = ycbcr_linear_to_rgb(yy, cb, cr, matrix);
            let dst = (y * width as usize + x) * 4;
            rgba[dst] = (r.clamp(0.0, 1.0) * 255.0).round() as u8;
            rgba[dst + 1] = (g.clamp(0.0, 1.0) * 255.0).round() as u8;
            rgba[dst + 2] = (b.clamp(0.0, 1.0) * 255.0).round() as u8;
            rgba[dst + 3] = 255;
        }
    }
    Ok(DecodedImage::new(width, height, rgba))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn heif_thumb_probe_labels_are_stable() {
        assert_eq!(HeifThumbProbe::NoThumbnail.label(), "no_thumbnail");
        assert_eq!(HeifThumbProbe::Found.label(), "found");
    }

    #[test]
    fn heif_strip_thumbnail_smoke_with_env_corpus() {
        let Some(dir) = std::env::var("SIV_HEIF_HDR_SAMPLES_DIR")
            .ok()
            .filter(|value| Path::new(value).is_dir())
        else {
            return;
        };
        let path = match std::fs::read_dir(&dir)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .find(|path| {
                path.extension().is_some_and(|ext| {
                    ext.eq_ignore_ascii_case("heic") || ext.eq_ignore_ascii_case("heif")
                })
            }) {
            Some(path) => path,
            None => return,
        };
        let (thumb, probe, detail) = probe_heif_strip_thumbnail_from_path(&path, 128);
        eprintln!(
            "probe={} path={} detail={detail:?} out={:?}",
            probe.label(),
            path.display(),
            thumb
                .as_ref()
                .map(|(decoded, logical)| (decoded.width, decoded.height, logical))
        );
        assert_eq!(
            probe,
            HeifThumbProbe::Found,
            "expected libheif container thumbnail in {}",
            path.display()
        );
        let (decoded, logical) = thumb.expect("thumbnail decode");
        assert!(logical.0 > 0 && logical.1 > 0);
        assert!(decoded.width > 0 && decoded.height > 0);
        assert!(decoded.width <= 128 || decoded.height <= 128);
    }
}
