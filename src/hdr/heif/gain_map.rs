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
use super::decode::RawHeifImage;
use super::metadata::{HeifAuxiliaryClassification, list_heif_auxiliary_evidence};
use super::orientation::heif_exif_orientation_from_raw_handle;

#[cfg(feature = "heif-native")]
pub(crate) fn heif_has_apple_hdr_gain_map_auxiliary(
    handle: *const libheif_sys::heif_image_handle,
) -> bool {
    list_heif_auxiliary_evidence(handle)
        .iter()
        .any(|item| item.classification == HeifAuxiliaryClassification::AppleHdrGainMap)
}

#[cfg(feature = "heif-native")]
pub(crate) const EXIF_ORIENTATION_NORMAL: u16 = 1;
#[cfg(feature = "heif-native")]
pub(crate) const EXIF_ORIENTATION_ROTATE_180: u16 = 3;
#[cfg(feature = "heif-native")]
pub(crate) const EXIF_ORIENTATION_ROTATE_90_CW: u16 = 6;
#[cfg(feature = "heif-native")]
pub(crate) const EXIF_ORIENTATION_ROTATE_90_CCW: u16 = 8;

#[cfg(feature = "heif-native")]
pub(crate) fn gain_map_stored_in_sensor_orientation(
    gain_width: u32,
    gain_height: u32,
    primary_ispe_w: u32,
    primary_ispe_h: u32,
) -> bool {
    primary_ispe_w != gain_width || primary_ispe_h != gain_height
}

#[cfg(feature = "heif-native")]
pub(crate) fn rotate_gain_rgba_90_cw(
    width: u32,
    height: u32,
    gain_rgba: Vec<u8>,
) -> (u32, u32, Vec<u8>) {
    let new_w = height;
    let new_h = width;
    let w = width as usize;
    let h = height as usize;
    let nw = new_w as usize;
    let len = gain_rgba.len();
    let expected_len = w
        .checked_mul(h)
        .and_then(|p| p.checked_mul(4))
        .expect("rotate_gain_rgba_90_cw: dimension overflow");
    assert_eq!(len, expected_len);
    let src = gain_rgba;
    let mut out = vec![0; len];
    unsafe {
        let out_ptr: *mut u8 = out.as_mut_ptr();
        for y in 0..h {
            for x in 0..w {
                let src_idx = (y * w + x) * 4;
                let dst_y = x;
                let dst_x = h - 1 - y;
                let dst_idx = (dst_y * nw + dst_x) * 4;
                std::ptr::copy_nonoverlapping(src.as_ptr().add(src_idx), out_ptr.add(dst_idx), 4);
            }
        }
    }
    (new_w, new_h, out)
}

#[cfg(feature = "heif-native")]
pub(crate) fn rotate_gain_rgba_90_ccw(
    width: u32,
    height: u32,
    gain_rgba: Vec<u8>,
) -> (u32, u32, Vec<u8>) {
    let new_w = height;
    let new_h = width;
    let w = width as usize;
    let h = height as usize;
    let nw = new_w as usize;
    let len = gain_rgba.len();
    let expected_len = w
        .checked_mul(h)
        .and_then(|p| p.checked_mul(4))
        .expect("rotate_gain_rgba_90_ccw: dimension overflow");
    assert_eq!(len, expected_len);
    let src = gain_rgba;
    let mut out = vec![0; len];
    unsafe {
        let out_ptr: *mut u8 = out.as_mut_ptr();
        for y in 0..h {
            for x in 0..w {
                let src_idx = (y * w + x) * 4;
                let dst_y = w - 1 - x;
                let dst_x = y;
                let dst_idx = (dst_y * nw + dst_x) * 4;
                std::ptr::copy_nonoverlapping(src.as_ptr().add(src_idx), out_ptr.add(dst_idx), 4);
            }
        }
    }
    (new_w, new_h, out)
}

#[cfg(feature = "heif-native")]
pub(crate) fn rotate_gain_rgba_180(
    width: u32,
    height: u32,
    mut gain_rgba: Vec<u8>,
) -> (u32, u32, Vec<u8>) {
    let w = width as usize;
    let h = height as usize;
    let expected_len = w
        .checked_mul(h)
        .and_then(|p| p.checked_mul(4))
        .expect("rotate_gain_rgba_180: dimension overflow");
    assert_eq!(gain_rgba.len(), expected_len);
    let pixels = w * h;
    for i in 0..pixels / 2 {
        let a = i * 4;
        let b = (pixels - 1 - i) * 4;
        for j in 0..4 {
            gain_rgba.swap(a + j, b + j);
        }
    }
    (width, height, gain_rgba)
}

/// Rotate an Apple HDR gain map from sensor (ispe) orientation into primary display orientation.
#[cfg(feature = "heif-native")]
#[derive(Clone, Copy)]
pub(crate) struct AppleGainMapAlignment {
    pub(crate) primary_ispe_w: u32,
    pub(crate) primary_ispe_h: u32,
    pub(crate) primary_disp_w: u32,
    pub(crate) primary_disp_h: u32,
    pub(crate) orientation: Option<u16>,
}

/// Rotate an Apple HDR gain map from sensor (ispe) orientation into primary display orientation.
#[cfg(feature = "heif-native")]
pub(crate) fn align_apple_gain_map_to_primary_display_orientation(
    gain_rgba: Vec<u8>,
    gain_width: u32,
    gain_height: u32,
    alignment: AppleGainMapAlignment,
) -> (u32, u32, Vec<u8>) {
    let AppleGainMapAlignment {
        primary_ispe_w,
        primary_ispe_h,
        primary_disp_w,
        primary_disp_h,
        orientation,
    } = alignment;
    let ispe_swapped = primary_ispe_w == primary_disp_h && primary_ispe_h == primary_disp_w;
    let ispe_rotated = ispe_swapped && primary_ispe_w != primary_ispe_h;
    let gain_in_sensor = gain_map_stored_in_sensor_orientation(
        gain_width,
        gain_height,
        primary_ispe_w,
        primary_ispe_h,
    );

    let rotation = match orientation {
        Some(EXIF_ORIENTATION_ROTATE_90_CW) => Some(RotateGainMap::Cw90),
        Some(EXIF_ORIENTATION_ROTATE_90_CCW) => Some(RotateGainMap::Ccw90),
        Some(EXIF_ORIENTATION_ROTATE_180) => Some(RotateGainMap::Rotate180),
        Some(EXIF_ORIENTATION_NORMAL) | None => {
            if ispe_rotated && gain_in_sensor {
                Some(RotateGainMap::from_ispe_aspect(
                    primary_ispe_w,
                    primary_ispe_h,
                ))
            } else {
                None
            }
        }
        Some(_) => {
            if ispe_rotated && gain_in_sensor {
                Some(RotateGainMap::from_ispe_aspect(
                    primary_ispe_w,
                    primary_ispe_h,
                ))
            } else {
                None
            }
        }
    };

    match rotation {
        Some(RotateGainMap::Cw90) => rotate_gain_rgba_90_cw(gain_width, gain_height, gain_rgba),
        Some(RotateGainMap::Ccw90) => rotate_gain_rgba_90_ccw(gain_width, gain_height, gain_rgba),
        Some(RotateGainMap::Rotate180) => rotate_gain_rgba_180(gain_width, gain_height, gain_rgba),
        None => (gain_width, gain_height, gain_rgba),
    }
}

#[cfg(feature = "heif-native")]
pub(crate) enum RotateGainMap {
    Cw90,
    Ccw90,
    Rotate180,
}

#[cfg(feature = "heif-native")]
impl RotateGainMap {
    fn from_ispe_aspect(primary_ispe_w: u32, primary_ispe_h: u32) -> Self {
        if primary_ispe_w > primary_ispe_h {
            RotateGainMap::Cw90
        } else {
            RotateGainMap::Ccw90
        }
    }
}

#[cfg(feature = "heif-native")]
pub(crate) fn decode_heif_gain_map(
    main_handle: *const libheif_sys::heif_image_handle,
    decode_options: *const libheif_sys::heif_decoding_options,
) -> Option<(u32, u32, Vec<u8>)> {
    let evidence = list_heif_auxiliary_evidence(main_handle);
    let apple_gain_map_item = evidence
        .into_iter()
        .find(|item| item.classification == HeifAuxiliaryClassification::AppleHdrGainMap);

    let apple_gain_map_item = match apple_gain_map_item {
        Some(item) => item,
        None => {
            log::debug!("[HDR] No Apple HDR Gain Map auxiliary image found in evidence.");
            return None;
        }
    };

    let mut aux_handle_ptr = std::ptr::null_mut();
    let status = unsafe {
        libheif_sys::heif_image_handle_get_auxiliary_image_handle(
            main_handle,
            apple_gain_map_item.item_id,
            &mut aux_handle_ptr,
        )
    };
    if status.code != libheif_sys::heif_error_Ok || aux_handle_ptr.is_null() {
        log::warn!(
            "[HDR] Failed to get auxiliary image handle for item #{}, code: {}",
            apple_gain_map_item.item_id,
            status.code
        );
        return None;
    }
    let aux_handle = HeifAuxiliaryImageHandle(unsafe {
        libheif_sys::HeifImageHandleGuard::from_ptr(aux_handle_ptr)
    });

    let mut image_ptr = std::ptr::null_mut();
    let err = unsafe {
        libheif_sys::heif_decode_image(
            aux_handle.as_ptr(),
            &mut image_ptr,
            libheif_sys::heif_colorspace_RGB,
            libheif_sys::heif_chroma_interleaved_RGBA,
            decode_options,
        )
    };
    if err.code != libheif_sys::heif_error_Ok || image_ptr.is_null() {
        log::warn!(
            "[HDR] Failed to decode auxiliary gain map image, code: {}",
            err.code
        );
        return None;
    }
    let gain_image = RawHeifImage(unsafe { libheif_sys::HeifImageGuard::from_ptr(image_ptr) });

    let width_i = unsafe { libheif_sys::heif_image_get_primary_width(gain_image.as_ptr()) };
    let height_i = unsafe { libheif_sys::heif_image_get_primary_height(gain_image.as_ptr()) };
    if width_i <= 0 || height_i <= 0 {
        log::warn!(
            "[HDR] Invalid auxiliary gain map dimensions: {}x{}",
            width_i,
            height_i
        );
        return None;
    }
    let width = width_i as u32;
    let height = height_i as u32;

    if let Err(e) = crate::constants::validate_static_decode_dimensions(width, height) {
        log::warn!("[HDR] HEIF gain map dimensions rejected: {width}x{height} — {e}",);
        return None;
    }

    let mut stride = 0_usize;
    let plane = unsafe {
        libheif_sys::heif_image_get_plane_readonly2(
            gain_image.as_ptr(),
            libheif_sys::heif_channel_interleaved,
            &mut stride,
        )
    };
    if plane.is_null() {
        log::warn!("[HDR] Failed to get plane pointer for auxiliary gain map image");
        return None;
    }

    let gain_len = match crate::constants::checked_rgba_buffer_len(width as usize, height as usize)
    {
        Some(n) => n,
        None => {
            log::warn!("[HDR] HEIF gain map buffer size overflow for {width}x{height}");
            return None;
        }
    };
    let mut gain_rgba = Vec::with_capacity(gain_len);
    let Some(row_bytes) = (width as usize).checked_mul(4) else {
        log::warn!("[HDR] HEIF gain map row_bytes overflow for {width}x{height}");
        return None;
    };
    if stride < row_bytes {
        log::warn!(
            "[HDR] Auxiliary gain map stride {} is less than row bytes {}",
            stride,
            row_bytes
        );
        return None;
    }

    // Verify that y * stride does not overflow (defense-in-depth for unsafe slice construction).
    if (height as usize).checked_mul(stride).is_none() {
        log::warn!("[HDR] HEIF gain map stride * height overflow: stride={stride} height={height}");
        return None;
    }

    for y in 0..height as usize {
        let row = unsafe { std::slice::from_raw_parts(plane.add(y * stride), row_bytes) };
        gain_rgba.extend_from_slice(row);
    }

    // The gain map auxiliary image is stored in the sensor (ispe) orientation.
    // The primary may have been decoded with HEIF/EXIF rotation applied (display
    // orientation). If the primary's display dimensions are swapped relative to
    // its stored ispe dimensions, apply the same rotation to the gain map so both
    // are in the same orientation for composition.
    let primary_ispe_w =
        unsafe { libheif_sys::heif_image_handle_get_ispe_width(main_handle) } as u32;
    let primary_ispe_h =
        unsafe { libheif_sys::heif_image_handle_get_ispe_height(main_handle) } as u32;
    let primary_disp_w = unsafe { libheif_sys::heif_image_handle_get_width(main_handle) } as u32;
    let primary_disp_h = unsafe { libheif_sys::heif_image_handle_get_height(main_handle) } as u32;

    let orientation = heif_exif_orientation_from_raw_handle(main_handle);
    let (out_w, out_h, out_rgba) = align_apple_gain_map_to_primary_display_orientation(
        gain_rgba,
        width,
        height,
        AppleGainMapAlignment {
            primary_ispe_w,
            primary_ispe_h,
            primary_disp_w,
            primary_disp_h,
            orientation,
        },
    );
    if (out_w, out_h) != (width, height) {
        log::debug!(
            "[HDR] Gain map rotated to display orientation: {}×{} → {}×{} (primary ispe {}×{} → display {}×{})",
            width,
            height,
            out_w,
            out_h,
            primary_ispe_w,
            primary_ispe_h,
            primary_disp_w,
            primary_disp_h,
        );
    }
    Some((out_w, out_h, out_rgba))
}

#[cfg(feature = "heif-native")]
pub(crate) struct HeifAuxiliaryImageHandle(pub(crate) libheif_sys::HeifImageHandleGuard);

#[cfg(feature = "heif-native")]
impl HeifAuxiliaryImageHandle {
    #[inline]
    pub(crate) fn as_ptr(&self) -> *const libheif_sys::heif_image_handle {
        self.0.as_ptr()
    }
}
