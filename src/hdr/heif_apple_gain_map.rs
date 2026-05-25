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

//! Apple HEIC HDR gain-map MakerNote parsing and primary-plane composition.

use crate::hdr::heif_apple_gain_map_compose_simd::compose_apple_gain_map_pixels;
use crate::hdr::types::{
    HdrColorSpace, HdrGainMapMetadata, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat,
};
use std::ffi::CStr;
use std::sync::Arc;

const EXIF_TAG_APPLE_HDR_HEADROOM: u16 = 0x0021;
const EXIF_TAG_APPLE_HDR_GAIN: u16 = 0x0030;
const APPLE_MAKERNOTE_SIGNATURE: &[u8] = b"Apple iOS\0";
const APPLE_MAKERNOTE_TIFF_OFFSET: usize = 12;
const APPLE_HDR_HEADROOM_MIN: f32 = 0.1;
const APPLE_HDR_HEADROOM_MAX: f32 = 10.0;
const APPLE_HDR_DEFAULT_STOPS: f32 = 2.0;

// Piecewise-linear coefficients from Apple's reference implementation:
// <https://developer.apple.com/documentation/appkit/images_and_pdf/applying_apple_hdr_effect_to_your_photos>
const APPLE_HDR_DIM_LOW_GAIN_SLOPE: f32 = -20.0;
const APPLE_HDR_DIM_LOW_GAIN_INTERCEPT: f32 = 1.8;
const APPLE_HDR_DIM_HIGH_GAIN_SLOPE: f32 = -0.101;
const APPLE_HDR_DIM_HIGH_GAIN_INTERCEPT: f32 = 1.601;
const APPLE_HDR_BRIGHT_LOW_GAIN_SLOPE: f32 = -70.0;
const APPLE_HDR_BRIGHT_LOW_GAIN_INTERCEPT: f32 = 3.0;
const APPLE_HDR_BRIGHT_HIGH_GAIN_SLOPE: f32 = -0.303;
const APPLE_HDR_BRIGHT_HIGH_GAIN_INTERCEPT: f32 = 2.303;
const APPLE_HDR_LOW_GAIN_THRESHOLD: f32 = 0.01;
const APPLE_HDR_DIM_HEADROOM_THRESHOLD: f32 = 1.0;

const TIFF_MAGIC_STANDARD: u16 = 0x002A;
const IFD_ENTRY_SIZE: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct AppleHdrHeadroomParams {
    pub hdr_headroom: f32,
    pub hdr_gain: f32,
    pub stops: f32,
    pub linear_headroom: f32,
}

impl AppleHdrHeadroomParams {
    pub(crate) fn default_fallback() -> Self {
        let stops = APPLE_HDR_DEFAULT_STOPS;
        Self {
            hdr_headroom: 0.0,
            hdr_gain: 0.0,
            stops,
            linear_headroom: 2.0_f32.powf(stops),
        }
    }
}

/// Maps display peak linear ratio to Apple gain-map blend weight.
pub(crate) fn apple_gain_map_display_weight(hdr_target_capacity: f32, stops: f32) -> f32 {
    if stops <= 0.0 {
        return 0.0;
    }
    let target_log2 = hdr_target_capacity.max(1.0).log2();
    (target_log2 / stops).clamp(0.0, 1.0)
}

pub(crate) fn resolve_apple_hdr_headroom_from_exif(
    exif_buf: Option<&[u8]>,
) -> AppleHdrHeadroomParams {
    let Some(buf) = exif_buf else {
        return AppleHdrHeadroomParams::default_fallback();
    };
    let Some((hdr_headroom, hdr_gain)) = parse_apple_hdr_metadata_from_exif(buf) else {
        return AppleHdrHeadroomParams::default_fallback();
    };
    let (linear_headroom, stops) = apple_compute_headroom(hdr_headroom, hdr_gain);
    AppleHdrHeadroomParams {
        hdr_headroom,
        hdr_gain,
        stops,
        linear_headroom,
    }
}

/// Returns `true` when decoding the auxiliary gain map and compositing is worth the cost.
pub(crate) fn should_apply_apple_heic_gain_map(
    hdr_target_capacity: f32,
    headroom: &AppleHdrHeadroomParams,
) -> bool {
    apple_gain_map_display_weight(hdr_target_capacity, headroom.stops) > 0.0
}

/// Compose Apple HDR gain map into a scene-linear sRGB [`HdrImageBuffer`].
pub(crate) fn apply_apple_gain_map_composition(
    hdr: HdrImageBuffer,
    gain_w: u32,
    gain_h: u32,
    gain_rgba: &[u8],
    headroom: &AppleHdrHeadroomParams,
    hdr_target_capacity: f32,
) -> HdrImageBuffer {
    let weight = apple_gain_map_display_weight(hdr_target_capacity, headroom.stops);
    let base_pixels = &hdr.rgba_f32;
    let pixel_count = hdr.width as usize * hdr.height as usize * 4;
    let mut composed_pixels = vec![0.0_f32; pixel_count];

    let color_space = hdr.color_space;
    let tf = hdr.metadata.transfer_function;
    let linear_headroom = headroom.linear_headroom;
    let headroom_span = linear_headroom - 1.0;

    compose_apple_gain_map_pixels(
        base_pixels,
        &mut composed_pixels,
        hdr.width,
        hdr.height,
        gain_rgba,
        gain_w,
        gain_h,
        color_space,
        tf,
        &hdr.metadata,
        headroom_span,
        weight,
    );

    let mut final_metadata = HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb);
    final_metadata.luminance = hdr.metadata.luminance;
    final_metadata.gain_map = Some(HdrGainMapMetadata {
        source: "HEIF",
        target_hdr_capacity: Some(hdr_target_capacity),
        diagnostic: format!(
            "Apple HDR Gain Map ({}x{} pixels, stops: {:.2}, weight: {:.2})",
            gain_w, gain_h, headroom.stops, weight
        ),
        capped_display_referred: false,
        apple_heic_deferred: None,
    });

    HdrImageBuffer {
        width: hdr.width,
        height: hdr.height,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::LinearSrgb,
        metadata: final_metadata,
        rgba_f32: Arc::new(composed_pixels),
    }
}

pub(crate) fn read_heif_exif_block(
    handle: *const libheif_sys::heif_image_handle,
) -> Option<Vec<u8>> {
    unsafe {
        let total =
            libheif_sys::heif_image_handle_get_number_of_metadata_blocks(handle, std::ptr::null());
        if total <= 0 {
            return None;
        }
        let total = total as usize;
        let mut ids = vec![0_u32; total];
        let n = libheif_sys::heif_image_handle_get_list_of_metadata_block_IDs(
            handle,
            std::ptr::null(),
            ids.as_mut_ptr(),
            total as i32,
        );
        let n = n.max(0) as usize;
        for &id in ids.iter().take(n) {
            let typ = libheif_sys::heif_image_handle_get_metadata_type(handle, id);
            if typ.is_null() {
                continue;
            }
            let typ_bytes = CStr::from_ptr(typ).to_bytes();
            if typ_bytes == b"Exif" {
                let sz = libheif_sys::heif_image_handle_get_metadata_size(handle, id);
                if sz > 0 {
                    let mut buf = vec![0_u8; sz];
                    let err = libheif_sys::heif_image_handle_get_metadata(
                        handle,
                        id,
                        buf.as_mut_ptr().cast(),
                    );
                    if err.code == libheif_sys::heif_error_Ok {
                        return Some(buf);
                    }
                }
            }
        }
        None
    }
}

/// Compute linear headroom and stops from Apple MakerNote tag values.
///
/// `hdr_headroom` is tag `0x0021`; `hdr_gain` is tag `0x0030`.
pub(crate) fn apple_compute_headroom(hdr_headroom: f32, hdr_gain: f32) -> (f32, f32) {
    let stops = if hdr_headroom < APPLE_HDR_DIM_HEADROOM_THRESHOLD {
        if hdr_gain <= APPLE_HDR_LOW_GAIN_THRESHOLD {
            APPLE_HDR_DIM_LOW_GAIN_SLOPE * hdr_gain + APPLE_HDR_DIM_LOW_GAIN_INTERCEPT
        } else {
            APPLE_HDR_DIM_HIGH_GAIN_SLOPE * hdr_gain + APPLE_HDR_DIM_HIGH_GAIN_INTERCEPT
        }
    } else if hdr_gain <= APPLE_HDR_LOW_GAIN_THRESHOLD {
        APPLE_HDR_BRIGHT_LOW_GAIN_SLOPE * hdr_gain + APPLE_HDR_BRIGHT_LOW_GAIN_INTERCEPT
    } else {
        APPLE_HDR_BRIGHT_HIGH_GAIN_SLOPE * hdr_gain + APPLE_HDR_BRIGHT_HIGH_GAIN_INTERCEPT
    };
    let stops = stops.max(0.0);
    (2.0_f32.powf(stops), stops)
}

fn parse_heif_exif_raw(buf: &[u8]) -> Option<exif::Exif> {
    if buf.len() >= 6 {
        if let Ok(offset_bytes) = <[u8; 4]>::try_from(&buf[0..4]) {
            let offset = u32::from_be_bytes(offset_bytes) as usize;
            if buf.len() >= 4 + offset {
                if let Some(tiff_tail) = buf.get(4 + offset..) {
                    if let Ok(e) = exif::Reader::new().read_raw(tiff_tail.to_vec()) {
                        return Some(e);
                    }
                }
            }
        }
    }
    exif::Reader::new().read_raw(buf.to_vec()).ok()
}

/// Parse Apple HDR Headroom and Gain from the MakerNote inside the raw HEIF Exif block.
pub(crate) fn parse_apple_hdr_metadata_from_exif(buf: &[u8]) -> Option<(f32, f32)> {
    let exif = parse_heif_exif_raw(buf)?;
    let maker_note = exif.get_field(exif::Tag::MakerNote, exif::In::PRIMARY)?;
    let maker_bytes = match &maker_note.value {
        exif::Value::Undefined(v, _) => v.as_slice(),
        _ => return None,
    };

    let sig_index = maker_bytes
        .windows(APPLE_MAKERNOTE_SIGNATURE.len())
        .position(|w| w == APPLE_MAKERNOTE_SIGNATURE)?;

    let tiff_start = sig_index + APPLE_MAKERNOTE_TIFF_OFFSET;
    if tiff_start >= maker_bytes.len() {
        return None;
    }
    parse_apple_embedded_manual(&maker_bytes[tiff_start..], tiff_start)
}

/// Manual parser for Apple's embedded MakerNote TIFF block.
pub(crate) fn parse_apple_embedded_manual(tiff: &[u8], tiff_offset: usize) -> Option<(f32, f32)> {
    if tiff.len() < 4 {
        return None;
    }

    let is_be = match &tiff[0..2] {
        b"MM" => true,
        b"II" => false,
        _ => return None,
    };

    let magic = read_u16(&tiff[2..4], is_be)?;
    let (entries_start, count) = if magic == TIFF_MAGIC_STANDARD {
        let ifd_offset = read_u32(&tiff[4..8], is_be)? as usize;
        if ifd_offset + 2 > tiff.len() {
            return None;
        }
        let count = read_u16(&tiff[ifd_offset..], is_be)? as usize;
        (ifd_offset + 2, count)
    } else {
        (4, magic as usize)
    };

    let mut headroom = None;
    let mut gain = None;

    for i in 0..count {
        let entry_offset = entries_start + i * IFD_ENTRY_SIZE;
        if entry_offset + IFD_ENTRY_SIZE > tiff.len() {
            break;
        }
        let entry = &tiff[entry_offset..entry_offset + IFD_ENTRY_SIZE];
        let tag = read_u16(&entry[0..2], is_be)?;
        if tag != EXIF_TAG_APPLE_HDR_HEADROOM && tag != EXIF_TAG_APPLE_HDR_GAIN {
            continue;
        }

        let val_off = read_u32(&entry[8..12], is_be)? as usize;
        let adj_off = val_off.saturating_sub(tiff_offset);
        if adj_off + 8 > tiff.len() {
            continue;
        }
        let val_bytes = &tiff[adj_off..adj_off + 8];

        let try_rational = |bytes: &[u8], be: bool| -> Option<f32> {
            let num = read_u32(&bytes[0..4], be)? as f32;
            let den = read_u32(&bytes[4..8], be)? as f32;
            if den != 0.0 { Some(num / den) } else { None }
        };

        let val = try_rational(val_bytes, is_be)
            .filter(|v| (APPLE_HDR_HEADROOM_MIN..=APPLE_HDR_HEADROOM_MAX).contains(v))
            .or_else(|| {
                try_rational(val_bytes, !is_be)
                    .filter(|v| (APPLE_HDR_HEADROOM_MIN..=APPLE_HDR_HEADROOM_MAX).contains(v))
            });

        if let Some(v) = val {
            if tag == EXIF_TAG_APPLE_HDR_HEADROOM {
                headroom = Some(v);
            } else {
                gain = Some(v);
            }
        }
    }

    let headroom = headroom?;
    Some((headroom, gain.unwrap_or(headroom)))
}

fn read_u16(buf: &[u8], be: bool) -> Option<u16> {
    let bytes = buf.get(0..2)?.try_into().ok()?;
    Some(if be {
        u16::from_be_bytes(bytes)
    } else {
        u16::from_le_bytes(bytes)
    })
}

fn read_u32(buf: &[u8], be: bool) -> Option<u32> {
    let bytes = buf.get(0..4)?.try_into().ok()?;
    Some(if be {
        u32::from_be_bytes(bytes)
    } else {
        u32::from_le_bytes(bytes)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apple_compute_headroom_matches_reference_piecewise_branches() {
        let (linear, stops) = apple_compute_headroom(0.5, 0.005);
        assert!((stops - 1.7).abs() < 1e-4);
        assert!((linear - 2.0_f32.powf(1.7)).abs() < 1e-3);

        let (linear_bright, stops_bright) = apple_compute_headroom(1.5, 0.005);
        assert!((stops_bright - 2.65).abs() < 1e-4);
        assert!((linear_bright - 2.0_f32.powf(2.65)).abs() < 1e-3);

        let (_, stops_high_gain) = apple_compute_headroom(1.5, 0.5);
        assert!((stops_high_gain - 2.1515).abs() < 1e-3);
    }

    #[test]
    fn apple_gain_map_display_weight_scales_with_target_capacity() {
        assert_eq!(apple_gain_map_display_weight(1.0, 2.0), 0.0);
        assert!((apple_gain_map_display_weight(4.0, 2.0) - 1.0).abs() < 1e-5);
        assert_eq!(apple_gain_map_display_weight(8.0, 0.0), 0.0);
    }

    #[test]
    fn should_skip_gain_map_on_sdr_target_capacity() {
        let headroom = AppleHdrHeadroomParams::default_fallback();
        assert!(!should_apply_apple_heic_gain_map(1.0, &headroom));
        assert!(should_apply_apple_heic_gain_map(4.0, &headroom));
    }

    #[test]
    fn parse_apple_embedded_manual_reads_nonstandard_ifd() {
        const EXIF_TYPE_RATIONAL: u16 = 5;
        // MakerNote: "Apple iOS\0" + 2-byte version + II IFD (count=1) + one rational headroom tag.
        let mut maker = Vec::new();
        maker.extend_from_slice(APPLE_MAKERNOTE_SIGNATURE);
        maker.extend_from_slice(&[0, 0]); // version
        maker.extend_from_slice(b"II");
        maker.extend_from_slice(&1_u16.to_le_bytes()); // entry count
        // IFD entry: tag 0x0021, type RATIONAL, count 1, value offset 28 (maker-relative)
        maker.extend_from_slice(&EXIF_TAG_APPLE_HDR_HEADROOM.to_le_bytes());
        maker.extend_from_slice(&EXIF_TYPE_RATIONAL.to_le_bytes());
        maker.extend_from_slice(&1_u32.to_le_bytes());
        maker.extend_from_slice(&28_u32.to_le_bytes());
        // Rational 178/100 = 1.78 at maker offset 28
        maker.extend_from_slice(&178_u32.to_le_bytes());
        maker.extend_from_slice(&100_u32.to_le_bytes());

        let tiff_start = APPLE_MAKERNOTE_TIFF_OFFSET;
        let parsed = parse_apple_embedded_manual(&maker[tiff_start..], tiff_start).expect("parse");
        assert!((parsed.0 - 1.78).abs() < 0.01);
        assert!((parsed.1 - 1.78).abs() < 0.01);
    }

    #[test]
    fn compose_gain_map_pixels_writes_expected_alpha_passthrough() {
        const W: u32 = 4;
        const H: u32 = 2;
        let pixel_count = W as usize * H as usize * 4;
        let mut base_pixels = vec![0.5_f32; pixel_count];
        base_pixels[3] = 0.25;
        base_pixels[pixel_count - 1] = 0.75;
        let gain_rgba = vec![128_u8; W as usize * H as usize * 4];
        let metadata = HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb);
        let headroom = AppleHdrHeadroomParams::default_fallback();
        let headroom_span = headroom.linear_headroom - 1.0;
        let weight = apple_gain_map_display_weight(4.0, headroom.stops);
        let mut out = vec![0.0_f32; pixel_count];
        compose_apple_gain_map_pixels(
            &base_pixels,
            &mut out,
            W,
            H,
            &gain_rgba,
            W,
            H,
            HdrColorSpace::LinearSrgb,
            metadata.transfer_function,
            &metadata,
            headroom_span,
            weight,
        );
        assert_eq!(out[3], 0.25);
        assert_eq!(out[pixel_count - 1], 0.75);
        assert!(out[0] > 0.5);
    }
}
