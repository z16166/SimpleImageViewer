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

#![cfg(feature = "avif-native")]

pub(crate) const AVIF_TRANSFORM_IROT_FLAG: libavif_sys::avifTransformFlags = 1 << 2;
#[cfg(feature = "avif-native")]
pub(crate) const AVIF_TRANSFORM_IMIR_FLAG: libavif_sys::avifTransformFlags = 1 << 3;

pub(crate) fn avif_irot_imir_to_exif_orientation(
    transform_flags: libavif_sys::avifTransformFlags,
    irot_angle: u8,
    imir_axis: u8,
) -> u16 {
    let flags = transform_flags;
    let angle = irot_angle & 3;
    let axis = imir_axis & 1;

    if flags & AVIF_TRANSFORM_IROT_FLAG == 0 || angle == 0 {
        if flags & AVIF_TRANSFORM_IMIR_FLAG == 0 {
            return 1;
        }
        return if axis == 0 { 4 } else { 2 };
    }

    if angle == 1 {
        if flags & AVIF_TRANSFORM_IMIR_FLAG == 0 {
            return 8;
        }
        return if axis == 0 { 5 } else { 7 };
    }

    if angle == 2 {
        if flags & AVIF_TRANSFORM_IMIR_FLAG == 0 {
            return 3;
        }
        return if axis == 0 { 2 } else { 4 };
    }

    if flags & AVIF_TRANSFORM_IMIR_FLAG == 0 {
        return 6;
    }
    if axis == 0 {
        return 7;
    }
    5
}

#[cfg(feature = "avif-native")]
pub(crate) fn avif_transforms_to_exif_orientation(image: &libavif_sys::avifImage) -> u16 {
    avif_irot_imir_to_exif_orientation(image.transformFlags, image.irot.angle, image.imir.axis)
}

/// After [`libavif_sys::avifDecoderParse`], `decoder->image` is filled from the container (incl. `irot` /
/// `imir`) before bitstream decode — no need for full read.
#[cfg(feature = "avif-native")]
pub(crate) fn libavif_probe_exif_orientation_from_bytes(bytes: &[u8]) -> Option<u16> {
    let decoder = libavif_sys::AvifDecoderOwned::new()?;
    unsafe {
        libavif_sys::siv_avif_decoder_set_strict_flags(
            decoder.as_ptr(),
            libavif_sys::AVIF_STRICT_DISABLED,
        );
        libavif_sys::siv_avif_decoder_set_image_content_flags(
            decoder.as_ptr(),
            libavif_sys::AVIF_IMAGE_CONTENT_COLOR_AND_ALPHA,
        );
    }
    let r = unsafe {
        libavif_sys::avifDecoderSetIOMemory(decoder.as_ptr(), bytes.as_ptr(), bytes.len())
    };
    if r != libavif_sys::AVIF_RESULT_OK {
        return None;
    }
    let r = unsafe { libavif_sys::avifDecoderParse(decoder.as_ptr()) };
    if r != libavif_sys::AVIF_RESULT_OK {
        return None;
    }
    let img = unsafe { libavif_sys::siv_avif_decoder_get_image(decoder.as_ptr()) };
    if img.is_null() {
        return None;
    }
    let image = unsafe { &*img };
    let o = avif_transforms_to_exif_orientation(image);
    ((1..=8).contains(&o)).then_some(o)
}

#[cfg(feature = "avif-native")]
pub(crate) fn libavif_probe_exif_orientation_from_path(path: &std::path::Path) -> Option<u16> {
    let mmap = crate::mmap_util::map_file(path).ok()?;
    libavif_probe_exif_orientation_from_bytes(&mmap[..])
}
