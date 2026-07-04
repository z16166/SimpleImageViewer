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

use super::decode::{heif_decode_primary_once, rgba8_from_decoded_heif};
use super::orientation::heif_manual_geometry_decode_options;
use super::session::{HeifCtxGuard, HeifPrimaryGuard, open_heif_primary_from_bytes};

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

/// One libheif open shared by container-thumb probe and primary-SDR strip fallback.
#[cfg(feature = "heif-native")]
struct HeifStripOpened {
    _ctx: HeifCtxGuard,
    primary: HeifPrimaryGuard,
    _decode_geo_holder: Option<super::orientation::HeifDecodeOptionsIgnoredGeometryOwned>,
    decode_opts_ptr: *const libheif_sys::heif_decoding_options,
}

#[cfg(feature = "heif-native")]
fn open_heif_strip_session(bytes: &[u8]) -> Result<HeifStripOpened, ()> {
    let (ctx, primary) = open_heif_primary_from_bytes(bytes).map_err(|_| ())?;
    let (decode_geo_holder, decode_opts_ptr) = heif_manual_geometry_decode_options(bytes);
    Ok(HeifStripOpened {
        _ctx: ctx,
        primary,
        _decode_geo_holder: decode_geo_holder,
        decode_opts_ptr,
    })
}

#[allow(dead_code)] // Used by env-gated corpus tests in this module.
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

#[allow(dead_code)] // Thin wrapper; directory-tree strips use [`try_heif_directory_tree_strip`].
pub(crate) fn probe_heif_strip_thumbnail(bytes: &[u8], max_side: u32) -> HeifStripThumbProbeResult {
    match open_heif_strip_session(bytes) {
        Ok(opened) => probe_heif_strip_thumbnail_on_opened(&opened, max_side),
        Err(()) => (
            None,
            HeifThumbProbe::ContainerUnreadable,
            HeifThumbProbeDetail::default(),
        ),
    }
}

fn probe_heif_strip_thumbnail_on_opened(
    opened: &HeifStripOpened,
    max_side: u32,
) -> HeifStripThumbProbeResult {
    #[cfg(feature = "preload-debug")]
    let decode_start = std::time::Instant::now();

    let primary_handle = opened.primary.as_ptr();
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

    let decoded = match decode_heif_handle_to_rgba8(thumb_handle.as_ptr(), decode_options.as_ptr())
    {
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

pub(crate) fn primary_logical_size(handle: *const libheif_sys::heif_image_handle) -> (u32, u32) {
    if handle.is_null() {
        return (0, 0);
    }
    let ispe_w = unsafe { libheif_sys::heif_image_handle_get_ispe_width(handle) };
    let ispe_h = unsafe { libheif_sys::heif_image_handle_get_ispe_height(handle) };
    if ispe_w > 0 && ispe_h > 0 {
        return (ispe_w as u32, ispe_h as u32);
    }
    let w = unsafe { libheif_sys::heif_image_handle_get_width(handle) }.max(0) as u32;
    let h = unsafe { libheif_sys::heif_image_handle_get_height(handle) }.max(0) as u32;
    (w, h)
}

/// Primary image logical size from container header only (no pixel decode).
pub(crate) fn libheif_probe_logical_size_from_bytes(bytes: &[u8]) -> Option<(u32, u32)> {
    let (_ctx, primary) = open_heif_primary_from_bytes(bytes).ok()?;
    let logical = primary_logical_size(primary.as_ptr());
    (logical.0 > 0 && logical.1 > 0).then_some(logical)
}

pub(crate) fn decode_heif_handle_to_rgba8(
    handle: *const libheif_sys::heif_image_handle,
    decode_options: *const libheif_sys::heif_decoding_options,
) -> Result<DecodedImage, String> {
    let raw = heif_decode_primary_once(handle, decode_options)?;
    rgba8_from_decoded_heif(handle, raw.as_ptr())
}

type HeifPrimaryStripResult = Option<Result<(DecodedImage, (u32, u32)), String>>;

/// Full-resolution primary HEIF item as 8-bit SDR RGBA (no gain-map auxiliary).
pub(crate) fn decode_heif_primary_sdr_from_handle(
    handle: *const libheif_sys::heif_image_handle,
    decode_opts_ptr: *const libheif_sys::heif_decoding_options,
) -> Result<(DecodedImage, (u32, u32)), String> {
    let logical = primary_logical_size(handle);
    if logical.0 == 0 || logical.1 == 0 {
        return Err("HEIF primary has zero logical size".to_string());
    }

    let decoded = decode_heif_handle_to_rgba8(handle, decode_opts_ptr)?;
    if !preview_aspect_matches_logical(decoded.width, decoded.height, logical.0, logical.1) {
        return Err(format!(
            "HEIF primary SDR aspect mismatch: {}x{} vs logical {}x{}",
            decoded.width, decoded.height, logical.0, logical.1
        ));
    }
    Ok((decoded, logical))
}

#[allow(dead_code)] // Standalone entry; strips use [`try_heif_directory_tree_strip`].
pub(crate) fn decode_heif_primary_sdr_from_bytes(
    bytes: &[u8],
) -> Result<(DecodedImage, (u32, u32)), String> {
    let opened = open_heif_strip_session(bytes).map_err(|_| "Failed to read HEIF".to_string())?;
    decode_heif_primary_sdr_from_handle(opened.primary.as_ptr(), opened.decode_opts_ptr)
}

fn downsample_heif_primary_sdr_strip(
    decoded: DecodedImage,
    logical: (u32, u32),
    max_side: u32,
) -> HeifPrimaryStripResult {
    let strip = match crate::loader::downsample_decoded_for_strip(&decoded, max_side) {
        Ok(image) => image,
        Err(err) => return Some(Err(err.to_string())),
    };
    if !preview_aspect_matches_logical(strip.width, strip.height, logical.0, logical.1) {
        log::debug!(
            "[HEIF] primary SDR strip aspect mismatch after downsample ({}x{} vs logical {}x{}); \
             trying next strip path",
            strip.width,
            strip.height,
            logical.0,
            logical.1
        );
        return None;
    }
    Some(Ok((strip, logical)))
}

fn try_heif_strip_primary_sdr_on_opened(
    opened: &HeifStripOpened,
    max_side: u32,
) -> HeifPrimaryStripResult {
    #[cfg(feature = "preload-debug")]
    let decode_start = std::time::Instant::now();

    let (decoded, logical) = match decode_heif_primary_sdr_from_handle(
        opened.primary.as_ptr(),
        opened.decode_opts_ptr,
    ) {
        Ok(pair) => pair,
        Err(err) => return Some(Err(err)),
    };
    let result = downsample_heif_primary_sdr_strip(decoded, logical, max_side);
    #[cfg(feature = "preload-debug")]
    if let Some(Ok((strip, logical))) = result.as_ref() {
        crate::preload_debug!(
            "[PreloadDebug][Strip] heif_primary_sdr logical={}x{} out={}x{} decode_ms={}",
            logical.0,
            logical.1,
            strip.width,
            strip.height,
            crate::preload_debug::elapsed_ms(decode_start)
        );
    }
    result
}

/// Directory-tree strip: one libheif open, container thumb first, then primary SDR on the same handle.
pub(crate) struct HeifDirectoryTreeStripOutcome {
    pub strip: HeifPrimaryStripResult,
    pub thumb_probe: HeifThumbProbe,
    pub thumb_detail: HeifThumbProbeDetail,
    /// Set when [`Self::strip`] is `Some(Ok(_))`.
    pub decode_path: Option<&'static str>,
}

pub(crate) fn try_heif_directory_tree_strip(
    bytes: &[u8],
    max_side: u32,
    allow_primary_sdr_fallback: bool,
) -> HeifDirectoryTreeStripOutcome {
    let opened = match open_heif_strip_session(bytes) {
        Ok(opened) => opened,
        Err(()) => {
            return HeifDirectoryTreeStripOutcome {
                strip: None,
                thumb_probe: HeifThumbProbe::ContainerUnreadable,
                thumb_detail: HeifThumbProbeDetail::default(),
                decode_path: None,
            };
        }
    };

    let (thumb, probe, detail) = probe_heif_strip_thumbnail_on_opened(&opened, max_side);
    if let Some(preview) = thumb {
        return HeifDirectoryTreeStripOutcome {
            strip: Some(Ok(preview)),
            thumb_probe: probe,
            thumb_detail: detail,
            decode_path: Some("heif_container_thumb"),
        };
    }

    if !allow_primary_sdr_fallback {
        return HeifDirectoryTreeStripOutcome {
            strip: None,
            thumb_probe: probe,
            thumb_detail: detail,
            decode_path: None,
        };
    }

    let strip = try_heif_strip_primary_sdr_on_opened(&opened, max_side);
    HeifDirectoryTreeStripOutcome {
        decode_path: strip
            .as_ref()
            .and_then(|result| result.as_ref().ok())
            .map(|_| "heif_primary_sdr"),
        strip,
        thumb_probe: probe,
        thumb_detail: detail,
    }
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
