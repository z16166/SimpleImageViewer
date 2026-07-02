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
use crate::constants::JXL_PROBE_ITERATION_CAP;
use crate::hdr::types::HdrImageMetadata;

pub(crate) fn is_jxl_header(header: &[u8]) -> bool {
    header.starts_with(&[0xff, 0x0a])
        || header.starts_with(&[0x00, 0x00, 0x00, 0x0c, b'J', b'X', b'L', b' '])
}

/// Peek [`JxlBasicInfo.orientation`] with [`libjxl_sys::JxlDecoderSetKeepOrientation`] enabled so
/// libjxl reports the codestream value (defaults would fold it to [`libjxl_sys::JXL_ORIENT_IDENTITY`]
/// once re-orientation is applied). Values match EXIF Orientation 1–8 (`jxl/codestream_header.h`).
#[cfg(feature = "jpegxl")]
pub(crate) fn libjxl_probe_orientation_from_bytes(bytes: &[u8]) -> Option<u16> {
    let probe_len = bytes.len().clamp(2, 16);
    if bytes.len() < 2 || !is_jxl_header(&bytes[..probe_len]) {
        return None;
    }
    struct DecoderPtr(*mut libjxl_sys::JxlDecoder);
    impl Drop for DecoderPtr {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { libjxl_sys::JxlDecoderDestroy(self.0) };
                self.0 = std::ptr::null_mut();
            }
        }
    }
    unsafe {
        let raw = libjxl_sys::JxlDecoderCreate(std::ptr::null());
        if raw.is_null() {
            return None;
        }
        let decoder = DecoderPtr(raw);
        if libjxl_sys::JxlDecoderSetKeepOrientation(decoder.0, libjxl_sys::JXL_TRUE)
            != libjxl_sys::JXL_DEC_SUCCESS
        {
            return None;
        }
        if libjxl_sys::JxlDecoderSubscribeEvents(
            decoder.0,
            libjxl_sys::JXL_DEC_BASIC_INFO as std::os::raw::c_int,
        ) != libjxl_sys::JXL_DEC_SUCCESS
        {
            return None;
        }
        if libjxl_sys::JxlDecoderSetInput(decoder.0, bytes.as_ptr(), bytes.len())
            != libjxl_sys::JXL_DEC_SUCCESS
        {
            return None;
        }
        libjxl_sys::JxlDecoderCloseInput(decoder.0);

        // Subscribed-only basic-info probes should terminate quickly; cap iterations on bad input.
        for _ in 0..JXL_PROBE_ITERATION_CAP {
            match libjxl_sys::JxlDecoderProcessInput(decoder.0) {
                libjxl_sys::JXL_DEC_BASIC_INFO => {
                    let mut info = std::mem::MaybeUninit::<libjxl_sys::JxlBasicInfo>::uninit();
                    if libjxl_sys::JxlDecoderGetBasicInfo(decoder.0.cast_const(), info.as_mut_ptr())
                        != libjxl_sys::JXL_DEC_SUCCESS
                    {
                        return None;
                    }
                    let info = info.assume_init();
                    let o_ok = info.orientation;
                    return ((1..=8).contains(&o_ok)).then_some(o_ok as u16);
                }
                libjxl_sys::JXL_DEC_SUCCESS
                | libjxl_sys::JXL_DEC_ERROR
                | libjxl_sys::JXL_DEC_NEED_MORE_INPUT => {
                    return None;
                }
                _ => {}
            }
        }
        None
    }
}

#[cfg(feature = "jpegxl")]
pub(crate) fn libjxl_probe_orientation_from_path(path: &std::path::Path) -> Option<u16> {
    let mmap = crate::mmap_util::map_file(path).ok()?;
    libjxl_probe_orientation_from_bytes(&mmap[..])
}

#[cfg(feature = "jpegxl")]
fn jxl_display_dimensions(info: &libjxl_sys::JxlBasicInfo) -> (u32, u32) {
    let w = info.xsize;
    let h = info.ysize;
    if w == 0 || h == 0 {
        return (0, 0);
    }
    match info.orientation {
        5..=8 => (h, w),
        _ => (w, h),
    }
}

/// Logical display size from codestream header only (no full-frame decode).
#[cfg(feature = "jpegxl")]
pub(crate) fn libjxl_probe_logical_size_from_bytes(bytes: &[u8]) -> Option<(u32, u32)> {
    let probe_len = bytes.len().clamp(2, 16);
    if bytes.len() < 2 || !is_jxl_header(&bytes[..probe_len]) {
        return None;
    }
    struct DecoderPtr(*mut libjxl_sys::JxlDecoder);
    impl Drop for DecoderPtr {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { libjxl_sys::JxlDecoderDestroy(self.0) };
                self.0 = std::ptr::null_mut();
            }
        }
    }
    unsafe {
        let raw = libjxl_sys::JxlDecoderCreate(std::ptr::null());
        if raw.is_null() {
            return None;
        }
        let decoder = DecoderPtr(raw);
        if libjxl_sys::JxlDecoderSubscribeEvents(
            decoder.0,
            libjxl_sys::JXL_DEC_BASIC_INFO as std::os::raw::c_int,
        ) != libjxl_sys::JXL_DEC_SUCCESS
        {
            return None;
        }
        if libjxl_sys::JxlDecoderSetInput(decoder.0, bytes.as_ptr(), bytes.len())
            != libjxl_sys::JXL_DEC_SUCCESS
        {
            return None;
        }
        libjxl_sys::JxlDecoderCloseInput(decoder.0);

        for _ in 0..JXL_PROBE_ITERATION_CAP {
            match libjxl_sys::JxlDecoderProcessInput(decoder.0) {
                libjxl_sys::JXL_DEC_BASIC_INFO => {
                    let mut info = std::mem::MaybeUninit::<libjxl_sys::JxlBasicInfo>::uninit();
                    if libjxl_sys::JxlDecoderGetBasicInfo(decoder.0.cast_const(), info.as_mut_ptr())
                        != libjxl_sys::JXL_DEC_SUCCESS
                    {
                        return None;
                    }
                    let info = info.assume_init();
                    let (w, h) = jxl_display_dimensions(&info);
                    return (w > 0 && h > 0).then_some((w, h));
                }
                libjxl_sys::JXL_DEC_SUCCESS
                | libjxl_sys::JXL_DEC_ERROR
                | libjxl_sys::JXL_DEC_NEED_MORE_INPUT => {
                    return None;
                }
                _ => {}
            }
        }
        None
    }
}

// JPEG XL colour / container behaviour (normative references for this module):
//
// - **ISO/IEC 18181-1** — JPEG XL codestream (image data, colour description in bitstream).
// - **ISO/IEC 18181-2** — JPEG XL file format (BMFF boxes, optional ICC, orientation, etc.).
// - **ISO/IEC 18181-4** — Reference software; **libjxl** is the de-facto normative decoder API
//   used here (`jxl/decode.h`). Decoder colour queries are defined in that API, not guessed.
// - **JPEG XL orientation** (`JxlDecoderSetKeepOrientation`, `JxlBasicInfo`): default libjxl applies
//   codestream orientation during decode **and folds** [`JxlBasicInfo.orientation`] back to identity
//   (`jxl/decode.h`). We enable **keep coded orientation** on the main decoder so pixels stay in codestream
//   layout while [`crate::metadata_utils::get_exif_orientation`] reads container EXIF when present or
//   else [`libjxl_probe_orientation_from_bytes`]/`path` parity for [`crate::loader::orientation`].
// - **`JxlColorProfileTarget`** (libjxl): `JXL_COLOR_PROFILE_TARGET_DATA` is the profile of the
//   **decoded pixels** written to the image out buffer; `JXL_COLOR_PROFILE_TARGET_ORIGINAL` is
//   the profile carried in metadata / codestream before decode. For `JXL_TYPE_FLOAT` output,
//   interpret samples against **TARGET_DATA ICC** when present, else `JxlColorEncoding` for DATA
//   (ICC wins over a generic encoded enum for XYB+ICC streams such as `bench_oriented_brg`).
// - **Associated alpha** (`JxlDecoderSetUnpremultiplyAlpha`): default decode is **premultiplied**
//   RGB when alpha is associated; we enable unpremultiply before decode so tone mapping sees
//   straight RGB (`jxl/decode.h`).
// - **XYB without ICC** (`JxlDecoderSetPreferredColorProfile`): when `TARGET_DATA` has **no** ICC,
//   steer XYB→float RGB with primaries inferred from any codestream ICC hint. If `TARGET_DATA`
//   already has an ICC, libjxl follows it for pixels — calling `SetPreferredColorProfile` then can
//   fight that path (washed highlights on conformance `bench_oriented_brg`).
// - **`JxlDecoderSetDesiredIntensityTarget`**: after `JXL_DEC_BASIC_INFO`, pass the codestream
//   `intensity_target` so float output luminance is scaled for that peak (e.g. 255 nits tests).
// - **ICC v4 `cicp` tag** (optional in profiles): carries ITU-T **H.273** codes; we map those
//   when present. Otherwise we derive primaries from ICC `rXYZ`/`gXYZ`/`bXYZ` per ICC.1.
//
// libjxl `JxlTransferFunction` values (`jxl/color_encoding.h`). **Linear / sRGB / PQ / HLG**
// discriminants intentionally match ITU-T H.273 `transfer_characteristics` — reuse `hdr::cicp`.
/// BT.709 / BT.601 OETF family (see `JXL_TRANSFER_FUNCTION_*` in libjxl headers).
pub(crate) const JXL_TRANSFER_FUNCTION_709: u16 = 1;
/// LibjXL “gamma”; not a fixed H.273 code.
pub(crate) const JXL_TRANSFER_FUNCTION_GAMMA: u16 = 65535;
pub(crate) const JXL_TRANSFER_FUNCTION_LINEAR: u16 = crate::hdr::cicp::H273_TRANSFER_LINEAR;
pub(crate) const JXL_TRANSFER_FUNCTION_SRGB: u16 =
    crate::hdr::cicp::H273_TRANSFER_IEC61966_2_1_SRGB;
pub(crate) const JXL_TRANSFER_FUNCTION_PQ: u16 =
    crate::hdr::cicp::H273_TRANSFER_SMPTE_ST2084_FOR_PQ;
pub(crate) const JXL_TRANSFER_FUNCTION_HLG: u16 =
    crate::hdr::cicp::H273_TRANSFER_ARIB_STD_B67_FOR_HLG;

#[allow(dead_code)]
pub(crate) fn jxl_color_encoding_to_metadata(
    color_primaries: u16,
    transfer_characteristics: u16,
    intensity_target_nits: Option<f32>,
) -> HdrImageMetadata {
    crate::hdr::cicp::cicp_to_metadata(
        color_primaries,
        transfer_characteristics,
        0,
        true,
        intensity_target_nits,
    )
}
