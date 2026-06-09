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
use super::probe::{
    JXL_TRANSFER_FUNCTION_709, JXL_TRANSFER_FUNCTION_GAMMA, JXL_TRANSFER_FUNCTION_HLG,
    JXL_TRANSFER_FUNCTION_LINEAR, JXL_TRANSFER_FUNCTION_PQ, JXL_TRANSFER_FUNCTION_SRGB,
};
use super::decode::{
    decode_jxl_hdr_bytes_with_target_capacity, jxl_tag_display_referred_when_sdr_grade,
};


#[cfg(feature = "jpegxl")]
use crate::hdr::gain_map::GainMapMetadata;
use crate::hdr::types::{
    HdrColorProfile, HdrImageMetadata, HdrLuminanceMetadata, HdrReference, HdrTransferFunction,
};
#[cfg(feature = "jpegxl")]
use crate::hdr::types::{HdrColorSpace, HdrImageBuffer, HdrPixelFormat, HdrToneMapSettings};
#[cfg(feature = "jpegxl")]
use crate::{
    constants::{
        DEFAULT_ANIMATION_DELAY_MS, JXL_PROBE_ITERATION_CAP, MAX_ICC_TAG_COUNT,
        MIN_ANIMATION_DELAY_THRESHOLD_MS,
    },
    loader::{AnimationFrame, DecodedImage, ImageData},
};
#[cfg(feature = "jpegxl")]
use std::sync::Arc;
#[cfg(feature = "jpegxl")]
use std::time::Duration;
#[cfg(feature = "jpegxl")]
pub(crate) fn ensure_jxl_success(status: libjxl_sys::JxlDecoderStatus, action: &str) -> Result<(), String> {
    if status == libjxl_sys::JXL_DEC_SUCCESS {
        Ok(())
    } else {
        Err(format!("Failed to {action}: libjxl status {status}"))
    }
}

#[cfg(feature = "jpegxl")]
pub(crate) fn capture_jxl_box(
    decoder: *mut libjxl_sys::JxlDecoder,
    box_type: [u8; 4],
    buffer: &mut Vec<u8>,
    buffer_pos: usize,
    jhgm_box: &mut Option<Vec<u8>>,
) {
    if buffer.is_empty() || box_type != *b"jhgm" {
        return;
    }
    let remaining = unsafe { libjxl_sys::JxlDecoderReleaseBoxBuffer(decoder) };
    let written = if remaining > 0 {
        buffer.len().saturating_sub(remaining)
    } else {
        buffer.len()
    }
    .max(buffer_pos)
    .min(buffer.len());
    jhgm_box.replace(buffer[..written].to_vec());
}

#[cfg(feature = "jpegxl")]
pub(crate) fn decode_jxl_gain_map_from_bundle(
    bundle: &JxlGainMapBundleRef<'_>,
    metadata: GainMapMetadata,
    target_hdr_capacity: f32,
) -> Result<(GainMapMetadata, u32, u32, Vec<u8>), String> {
    let gain_map = decode_jxl_hdr_bytes_with_target_capacity(bundle.gain_map, target_hdr_capacity)?;
    let gain_rgba = gain_map
        .rgba_f32
        .iter()
        .map(|value| (value * 255.0).round().clamp(0.0, 255.0) as u8)
        .collect();
    Ok((metadata, gain_map.width, gain_map.height, gain_rgba))
}

#[cfg(feature = "jpegxl")]
#[derive(Debug, Clone, Copy)]
pub(crate) struct JxlGainMapBundleRef<'a> {
    #[allow(dead_code)]
    pub(crate) version: u8,
    pub(crate) metadata: &'a [u8],
    pub(crate) gain_map: &'a [u8],
}

#[cfg(feature = "jpegxl")]
pub(crate) fn read_jxl_gain_map_bundle(jhgm_box: &[u8]) -> Result<JxlGainMapBundleRef<'_>, String> {
    let mut reader = JxlBundleReader::new(jhgm_box);
    let version = reader.read_u8()?;
    let metadata_size = reader.read_u16()? as usize;
    let metadata = reader.read_slice(metadata_size)?;
    let compressed_color_encoding_size = reader.read_u8()? as usize;
    let _compressed_color_encoding = reader.read_slice(compressed_color_encoding_size)?;
    let compressed_icc_size = reader.read_u32()? as usize;
    let _compressed_icc = reader.read_slice(compressed_icc_size)?;
    let gain_map = reader.remaining_slice();

    if metadata.is_empty() {
        return Err("JPEG XL jhgm bundle has no ISO gain-map metadata".to_string());
    }
    if gain_map.is_empty() {
        return Err("JPEG XL jhgm bundle has no gain-map codestream".to_string());
    }

    Ok(JxlGainMapBundleRef {
        version,
        metadata,
        gain_map,
    })
}

#[cfg(feature = "jpegxl")]
struct JxlBundleReader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

#[cfg(feature = "jpegxl")]
impl<'a> JxlBundleReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn read_u8(&mut self) -> Result<u8, String> {
        let slice = self.read_slice(1)?;
        Ok(slice[0])
    }

    fn read_u16(&mut self) -> Result<u16, String> {
        let slice = self.read_slice(2)?;
        Ok(u16::from_be_bytes([slice[0], slice[1]]))
    }

    fn read_u32(&mut self) -> Result<u32, String> {
        let slice = self.read_slice(4)?;
        Ok(u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
    }

    fn read_slice(&mut self, len: usize) -> Result<&'a [u8], String> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or_else(|| "JPEG XL jhgm bundle length overflow".to_string())?;
        if end > self.bytes.len() {
            return Err("truncated JPEG XL jhgm gain-map bundle".to_string());
        }
        let slice = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(slice)
    }

    fn remaining_slice(&mut self) -> &'a [u8] {
        let slice = &self.bytes[self.offset..];
        self.offset = self.bytes.len();
        slice
    }
}

#[cfg(feature = "jpegxl")]
pub(crate) fn linear_to_srgb_u8(value: f32) -> u8 {
    let value = value.max(0.0);
    let encoded = if value <= 0.0031308 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    };
    (encoded * 255.0).round().clamp(0.0, 255.0) as u8
}

#[cfg(feature = "jpegxl")]
fn icc_find_tag_element_offset(icc: &[u8], tag: &[u8; 4]) -> Option<usize> {
    for (sig, offset, _size) in icc_tag_entries(icc)? {
        if sig == *tag {
            return Some(offset as usize);
        }
    }
    None
}

#[cfg(feature = "jpegxl")]
fn icc_read_s15fixed16(bytes: &[u8], offset: usize) -> Option<f32> {
    let v = i32::from_be_bytes(bytes.get(offset..offset + 4)?.try_into().ok()?);
    Some(v as f32 / 65536.0)
}

/// Classify the `rTRC` (red Tone Reproduction Curve) tag of an ICC profile so
/// we can decide whether libjxl's float buffer for an embedded-ICC source is
/// already in encoded form (`Srgb` / `Gamma`) or truly linear (`Linear`). The
/// classification is a heuristic — it only inspects `rTRC` and assumes the
/// per-channel TRCs are uniform — but it's enough for the JXL conformance
/// corpus we care about (sRGB ICC, Display P3 linear ICC, etc.).
///
/// ICC v4 §10.5: `curveType` is `'curv'` followed by reserved (4) and a u32
/// `count`:
///   - count == 0 → identity (linear)
///   - count == 1 → single u8.8 fixed-point gamma value (`0x0100` = 1.0)
///   - count >= 2 → a `count`-entry u16 LUT (e.g. ICC v4 sRGB has count == 1024)
///
/// Returns `None` if the tag is missing or malformed (caller falls back).
#[cfg(feature = "jpegxl")]
fn icc_trc_kind(icc: &[u8]) -> Option<HdrTransferFunction> {
    let off = icc_find_tag_element_offset(icc, b"rTRC")?;
    if off + 12 > icc.len() {
        return None;
    }
    if &icc[off..off + 4] != b"curv" {
        // Could be `parametricCurveType` (`para`) — ICC v4 §10.18. We only
        // bother with the linear/non-linear distinction.
        if &icc[off..off + 4] == b"para" {
            // ICC v4 §10.18: function type at offset+8 (u16). Type 0 = simple
            // power gamma `Y = X^g`. Type 1+ are sRGB-style piecewise.
            let function_type = u16::from_be_bytes(icc[off + 8..off + 10].try_into().ok()?);
            if function_type == 0 {
                let gamma = icc_read_s15fixed16(icc, off + 12)?;
                if (gamma - 1.0).abs() < 1e-3 {
                    return Some(HdrTransferFunction::Linear);
                }
                return Some(HdrTransferFunction::Gamma);
            }
            return Some(HdrTransferFunction::Srgb);
        }
        return None;
    }
    let count = u32::from_be_bytes(icc[off + 8..off + 12].try_into().ok()?) as usize;
    if count == 0 {
        return Some(HdrTransferFunction::Linear);
    }
    if count == 1 {
        if off + 14 > icc.len() {
            return None;
        }
        let raw = u16::from_be_bytes(icc[off + 12..off + 14].try_into().ok()?);
        let gamma = raw as f32 / 256.0; // u8.8 fixed point
        if (gamma - 1.0).abs() < 1e-2 {
            return Some(HdrTransferFunction::Linear);
        }
        return Some(HdrTransferFunction::Gamma);
    }
    // Multi-entry LUT: assume sRGB-style encoding curve. We could detect a
    // pure-linear LUT here (identity ramp) but real-world ICCs that ship a
    // LUT are non-linear, and the SDR fallback's direct-quantize path is the
    // safe choice for any non-linear curve we encounter on the JXL conformance
    // corpus.
    Some(HdrTransferFunction::Srgb)
}

/// Read an `XYZType` payload (`XYZ ` + reserved + three s15Fixed16) and convert to CIE xy.
#[cfg(feature = "jpegxl")]
fn icc_xyz_type_to_xy(icc: &[u8], tag_element_offset: usize) -> Option<(f64, f64)> {
    if tag_element_offset + 20 > icc.len() {
        return None;
    }
    if &icc[tag_element_offset..tag_element_offset + 4] != b"XYZ " {
        return None;
    }
    let x = icc_read_s15fixed16(icc, tag_element_offset + 8)? as f64;
    let y = icc_read_s15fixed16(icc, tag_element_offset + 12)? as f64;
    let z = icc_read_s15fixed16(icc, tag_element_offset + 16)? as f64;
    let sum = x + y + z;
    if !sum.is_finite() || sum.abs() < 1e-20 {
        return None;
    }
    Some((x / sum, y / sum))
}

/// Derive CICP-style primaries from ICC `rXYZ`/`gXYZ`/`bXYZ` when no `cicp` tag is present
/// (common for libjxl-generated PQ profiles).
///
/// ICC tags are named after **file** channel order (e.g. JPEG XL `brg` / `bgr`), not necessarily
/// RGB semantics, so we match the multiset of three xy points to BT.2020 / Display P3 primaries.
///
/// ICC `rXYZ`/`gXYZ`/`bXYZ` often encodes **BT.709** primaries for PQ/HDR JPEG XL while libjxl
/// still outputs **linear light in that same narrow gamut** (see conformance `bench_oriented_brg`).
/// Do **not** assume Rec.2020 unless the chromaticities actually match BT.2020 / P3.
#[cfg(feature = "jpegxl")]
fn hdr_metadata_from_icc_rgb_xyz_primaries_for_jxl_float(icc: &[u8]) -> Option<HdrImageMetadata> {
    let r_off = icc_find_tag_element_offset(icc, b"rXYZ")?;
    let g_off = icc_find_tag_element_offset(icc, b"gXYZ")?;
    let b_off = icc_find_tag_element_offset(icc, b"bXYZ")?;
    let xy0 = icc_xyz_type_to_xy(icc, r_off)?;
    let xy1 = icc_xyz_type_to_xy(icc, g_off)?;
    let xy2 = icc_xyz_type_to_xy(icc, b_off)?;
    let observed = [xy0, xy1, xy2];

    const BT2020: [(f64, f64); 3] = [(0.708, 0.292), (0.17, 0.797), (0.131, 0.046)];
    const DISPLAY_P3: [(f64, f64); 3] = [(0.68, 0.32), (0.265, 0.69), (0.15, 0.06)];
    const BT709: [(f64, f64); 3] = [(0.64, 0.33), (0.3, 0.6), (0.15, 0.06)];
    const PERMS: [[usize; 3]; 6] = [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ];

    let multiset_close = |obs: [(f64, f64); 3], tgt: [(f64, f64); 3], eps: f64| {
        PERMS.iter().any(|perm| {
            (0..3).all(|i| {
                let p = obs[perm[i]];
                let t = tgt[i];
                (p.0 - t.0).hypot(p.1 - t.1) <= eps
            })
        })
    };

    let color_primaries = if multiset_close(observed, BT2020, 0.08) {
        9u16
    } else if multiset_close(observed, DISPLAY_P3, 0.1) {
        11u16
    } else if multiset_close(observed, BT709, 0.06) {
        1u16
    } else {
        return None;
    };

    // The function only fires when libjxl can't expose the codestream encoding
    // as a JxlColorEncoding enum (empty `JxlDecoderGetColorAsEncodedProfile`),
    // i.e. the ICC profile is the only ground truth. Parse the rTRC tag to
    // distinguish actually-linear ICCs (e.g. conformance `patches_lossless`
    // with a Display P3 linear profile) from sRGB-curve ICCs (e.g.
    // `bench_oriented_brg` JPEG-recompressed sRGB) — the float buffer libjxl
    // emits matches the ICC's TRC, so the SDR-grade fallback needs to know.
    let trc = icc_trc_kind(icc).unwrap_or(HdrTransferFunction::Srgb);
    let (cicp_transfer, internal_tf, reference) = match trc {
        HdrTransferFunction::Linear => (8_u16, HdrTransferFunction::Linear, HdrReference::Unknown),
        HdrTransferFunction::Gamma => (4_u16, HdrTransferFunction::Gamma, HdrReference::Unknown),
        // Fallback: encoded-curve ICC — match `bench_oriented_brg` behavior.
        // (BT.2020 / Display P3 with non-linear curves still go through the
        // SDR-grade direct-quantize path, intensity_target gates HDR.)
        _ => (13_u16, HdrTransferFunction::Srgb, HdrReference::Unknown),
    };

    Some(HdrImageMetadata {
        transfer_function: internal_tf,
        reference,
        color_profile: HdrColorProfile::Cicp {
            color_primaries,
            transfer_characteristics: cicp_transfer,
            matrix_coefficients: 0,
            full_range: true,
        },
        luminance: HdrLuminanceMetadata::default(),
        gain_map: None,
    })
}

#[cfg(feature = "jpegxl")]
fn icc_scan_cicp_tag(icc: &[u8]) -> Option<(u16, u16, u16, bool)> {
    for (sig, offset, _size) in icc_tag_entries(icc)? {
        if sig == *b"cicp" {
            let offset = offset as usize;
            // Tag data: signature (4) + reserved (4) + payload
            if offset + 12 > icc.len() {
                return None;
            }
            let p = u16::from(icc[offset + 8]);
            let t = u16::from(icc[offset + 9]);
            let m = u16::from(icc[offset + 10]);
            let fr = icc[offset + 11] != 0;
            return Some((p, t, m, fr));
        }
    }
    None
}

#[cfg(feature = "jpegxl")]
fn icc_tag_entries(icc: &[u8]) -> Option<Vec<([u8; 4], u32, u32)>> {
    const HEADER: usize = 128;
    if icc.len() < HEADER + 4 {
        return None;
    }
    let tag_count = u32::from_be_bytes(icc[128..132].try_into().ok()?) as usize;
    if tag_count > MAX_ICC_TAG_COUNT {
        return None;
    }
    let mut out = Vec::with_capacity(tag_count.min(128));
    let mut entry = 132usize;
    for _ in 0..tag_count {
        if entry + 12 > icc.len() {
            break;
        }
        let sig = icc[entry..entry + 4].try_into().ok()?;
        let offset = u32::from_be_bytes(icc[entry + 4..entry + 8].try_into().ok()?);
        let size = u32::from_be_bytes(icc[entry + 8..entry + 12].try_into().ok()?);
        out.push((sig, offset, size));
        entry += 12;
    }
    Some(out)
}

#[cfg(feature = "jpegxl")]
fn hdr_metadata_from_h273_cicp_for_jxl_float_buffer(
    color_primaries: u16,
    transfer_characteristics: u16,
    matrix_coefficients: u16,
    full_range: bool,
) -> HdrImageMetadata {
    // CICP transfer characteristics carry ground-truth source TF when present
    // (ITU-T H.273 §8.2). Map them to our internal flag so the SDR-grade
    // fallback knows whether libjxl's float buffer is linear (needs OETF) or
    // already encoded (direct quantize). Previously this hard-coded `Linear`
    // and the rest of the pipeline papered over it — that broke true-linear
    // sources like conformance `patches/input.jxl`.
    let (internal_tf, reference) = match transfer_characteristics {
        8 => (HdrTransferFunction::Linear, HdrReference::Unknown),
        16 => (HdrTransferFunction::Pq, HdrReference::DisplayReferred),
        18 => (HdrTransferFunction::Hlg, HdrReference::SceneLinear),
        4 => (HdrTransferFunction::Gamma, HdrReference::Unknown),
        // 1 (BT.709), 6 / 14 / 15 (BT.601 / BT.2020 ish), 13 (sRGB IEC 61966-2-1):
        // all encoded with sRGB-equivalent OETF for SDR, the float buffer is
        // already in encoded form for libjxl's Modular mode output.
        _ => (HdrTransferFunction::Srgb, HdrReference::Unknown),
    };
    HdrImageMetadata {
        transfer_function: internal_tf,
        reference,
        color_profile: HdrColorProfile::Cicp {
            color_primaries,
            transfer_characteristics,
            matrix_coefficients,
            full_range,
        },
        luminance: HdrLuminanceMetadata::default(),
        gain_map: None,
    }
}

#[cfg(feature = "jpegxl")]
fn jxl_xy_dist(a: [f64; 2], b: [f64; 2]) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

#[cfg(feature = "jpegxl")]
fn jxl_xy_close(a: [f64; 2], b: [f64; 2], eps: f64) -> bool {
    jxl_xy_dist(a, b) <= eps
}

/// Map `JxlColorEncoding` primaries to an H.273-style `color_primaries` code for our
/// `HdrColorProfile::Cicp` hint. `JXL_PRIMARIES_CUSTOM` is resolved from `primaries_*_xy`.
#[cfg(feature = "jpegxl")]
fn jxl_cicp_color_primaries_from_encoding(color: &libjxl_sys::JxlColorEncoding) -> u16 {
    if color.color_space != libjxl_sys::JXL_COLOR_SPACE_RGB {
        return 2;
    }
    if color.primaries == libjxl_sys::JXL_PRIMARIES_2100 {
        return 9;
    }
    if color.primaries == libjxl_sys::JXL_PRIMARIES_P3 {
        return 11;
    }
    if color.primaries == libjxl_sys::JXL_PRIMARIES_SRGB {
        return 1;
    }
    if color.primaries == libjxl_sys::JXL_PRIMARIES_CUSTOM {
        if chromaticities_close_to_bt2020(color) {
            return 9;
        }
        if chromaticities_close_to_display_p3(color) {
            return 11;
        }
        if chromaticities_close_to_bt709_srgb(color) {
            return 1;
        }
    }
    2
}

#[cfg(feature = "jpegxl")]
fn chromaticities_close_to_bt2020(color: &libjxl_sys::JxlColorEncoding) -> bool {
    const R: [f64; 2] = [0.708, 0.292];
    const G: [f64; 2] = [0.17, 0.797];
    const B: [f64; 2] = [0.131, 0.046];
    const EPS: f64 = 0.06;
    jxl_xy_close(color.primaries_red_xy, R, EPS)
        && jxl_xy_close(color.primaries_green_xy, G, EPS)
        && jxl_xy_close(color.primaries_blue_xy, B, EPS)
}

#[cfg(feature = "jpegxl")]
fn chromaticities_close_to_display_p3(color: &libjxl_sys::JxlColorEncoding) -> bool {
    const R: [f64; 2] = [0.68, 0.32];
    const G: [f64; 2] = [0.265, 0.69];
    const B: [f64; 2] = [0.15, 0.06];
    const EPS: f64 = 0.05;
    jxl_xy_close(color.primaries_red_xy, R, EPS)
        && jxl_xy_close(color.primaries_green_xy, G, EPS)
        && jxl_xy_close(color.primaries_blue_xy, B, EPS)
}

#[cfg(feature = "jpegxl")]
fn chromaticities_close_to_bt709_srgb(color: &libjxl_sys::JxlColorEncoding) -> bool {
    const R: [f64; 2] = [0.64, 0.33];
    const G: [f64; 2] = [0.3, 0.6];
    const B: [f64; 2] = [0.15, 0.06];
    const EPS: f64 = 0.04;
    jxl_xy_close(color.primaries_red_xy, R, EPS)
        && jxl_xy_close(color.primaries_green_xy, G, EPS)
        && jxl_xy_close(color.primaries_blue_xy, B, EPS)
}

/// Build metadata from `JxlColorEncoding` for **`JXL_COLOR_PROFILE_TARGET_DATA`** (decoded pixels).
///
/// With `JXL_TYPE_FLOAT` + default bit depth, libjxl returns **linear light** in the profile's
/// RGB primaries; the encoding's `transfer_function` describes the **coded** image, not raw
/// nonlinear samples in the float buffer (see libjxl decoder API / examples).
#[cfg(feature = "jpegxl")]
fn hdr_metadata_from_jxl_float_decode(color: &libjxl_sys::JxlColorEncoding) -> HdrImageMetadata {
    let cicp_primaries = jxl_cicp_color_primaries_from_encoding(color);
    // libjxl's `JxlTransferFunction` is a signed `c_int` enum but the values
    // we care about (1, 4, 8, 13, 16, 18, 65535=GAMMA) all fit unsigned u16.
    let jxl_tf_code = color.transfer_function as i64;
    let cicp_transfer = jxl_cicp_transfer_code_from_jxl(jxl_tf_code);
    let internal_tf = jxl_internal_transfer_for_jxl_float_buffer(jxl_tf_code);
    let reference = match internal_tf {
        HdrTransferFunction::Pq => HdrReference::DisplayReferred,
        HdrTransferFunction::Hlg => HdrReference::SceneLinear,
        _ => HdrReference::Unknown,
    };
    HdrImageMetadata {
        transfer_function: internal_tf,
        reference,
        color_profile: HdrColorProfile::Cicp {
            color_primaries: cicp_primaries,
            transfer_characteristics: cicp_transfer,
            matrix_coefficients: 0,
            full_range: true,
        },
        luminance: HdrLuminanceMetadata::default(),
        gain_map: None,
    }
}

/// Map libjxl's `JxlTransferFunction` enum (codestream value) to the
/// `HdrTransferFunction` we use internally to decide how to quantize the float
/// buffer for SDR fallback. Per empirical sampling of conformance files,
/// libjxl preserves the codestream's encoding in the float buffer for
/// Modular-mode files: TF=Linear → linear floats,
/// TF=IEC sRGB (**13**) / BT.709 codestream (**1**, [`HdrTransferFunction::Bt709`]) / Gamma (**4**) / Unknown →
/// preserve libjxl’s nonlinear floats; PQ / HLG (**16** / **18**) signal HDR.
#[cfg(feature = "jpegxl")]
fn jxl_internal_transfer_for_jxl_float_buffer(jxl_tf: i64) -> HdrTransferFunction {
    match jxl_tf {
        x if x == JXL_TRANSFER_FUNCTION_LINEAR as i64 => HdrTransferFunction::Linear,
        x if x == JXL_TRANSFER_FUNCTION_SRGB as i64 => HdrTransferFunction::Srgb,
        x if x == JXL_TRANSFER_FUNCTION_709 as i64 => HdrTransferFunction::Bt709,
        x if x == JXL_TRANSFER_FUNCTION_PQ as i64 => HdrTransferFunction::Pq,
        x if x == JXL_TRANSFER_FUNCTION_HLG as i64 => HdrTransferFunction::Hlg,
        x if x == JXL_TRANSFER_FUNCTION_GAMMA as i64 => HdrTransferFunction::Gamma,
        _ => HdrTransferFunction::Unknown,
    }
}

/// Convert libjxl's `JxlTransferFunction` enum into the matching CICP transfer
/// characteristics code (ITU-T H.273), so downstream components see the same
/// numeric values the JXL bitstream specified instead of always reporting
/// "linear" (which used to be the previous hard-coded fallback).
#[cfg(feature = "jpegxl")]
fn jxl_cicp_transfer_code_from_jxl(jxl_tf: i64) -> u16 {
    match jxl_tf {
        x if x == JXL_TRANSFER_FUNCTION_709 as i64 => 1,
        x if x == JXL_TRANSFER_FUNCTION_LINEAR as i64 => 8,
        x if x == JXL_TRANSFER_FUNCTION_SRGB as i64 => 13,
        x if x == JXL_TRANSFER_FUNCTION_PQ as i64 => 16,
        x if x == JXL_TRANSFER_FUNCTION_HLG as i64 => 18,
        x if x == JXL_TRANSFER_FUNCTION_GAMMA as i64 => 4,
        _ => 2,
    }
}

#[cfg(feature = "jpegxl")]
pub(crate) fn jxl_decoder_copy_target_data_icc(decoder: *const libjxl_sys::JxlDecoder) -> Option<Vec<u8>> {
    jxl_decoder_copy_icc_for_target(decoder, libjxl_sys::JXL_COLOR_PROFILE_TARGET_DATA)
}

/// Read the **original** color profile of the JXL bitstream (i.e. before any
/// CMS applied by libjxl). This is the "source" profile used by external
/// color management — for CMYK-style sources it's a CMYK ICC profile that we
/// feed into lcms2 to compose CMYK→sRGB.
#[cfg(feature = "jpegxl")]
pub(crate) fn jxl_decoder_copy_target_original_icc(decoder: *const libjxl_sys::JxlDecoder) -> Option<Vec<u8>> {
    jxl_decoder_copy_icc_for_target(decoder, libjxl_sys::JXL_COLOR_PROFILE_TARGET_ORIGINAL)
}

#[cfg(feature = "jpegxl")]
fn jxl_decoder_copy_icc_for_target(
    decoder: *const libjxl_sys::JxlDecoder,
    target: libjxl_sys::JxlColorProfileTarget,
) -> Option<Vec<u8>> {
    let mut icc_size = 0_usize;
    let st = unsafe { libjxl_sys::JxlDecoderGetICCProfileSize(decoder, target, &mut icc_size) };
    if st != libjxl_sys::JXL_DEC_SUCCESS || icc_size == 0 {
        return None;
    }
    let mut icc = vec![0_u8; icc_size];
    let st2 = unsafe {
        libjxl_sys::JxlDecoderGetColorAsICCProfile(decoder, target, icc.as_mut_ptr(), icc.len())
    };
    (st2 == libjxl_sys::JXL_DEC_SUCCESS).then_some(icc)
}

/// VarDCT (XYB) + ICC: steer libjxl's XYB→float-RGB path toward primaries inferred from the
/// embedded `TARGET_DATA` ICC (`rXYZ`/`gXYZ`/`bXYZ`), instead of relying on the decoder's generic
/// fallback that can disagree with narrow-gamut PQ ICCs (e.g. conformance `bench_oriented_brg`).
#[cfg(feature = "jpegxl")]
pub(crate) fn jxl_apply_preferred_profile_from_target_data_icc(decoder: *mut libjxl_sys::JxlDecoder) {
    let Some(icc) = jxl_decoder_copy_target_data_icc(decoder.cast_const()) else {
        return;
    };
    let Some(meta) = hdr_metadata_from_icc_rgb_xyz_primaries_for_jxl_float(&icc) else {
        return;
    };
    let HdrColorProfile::Cicp {
        color_primaries, ..
    } = meta.color_profile
    else {
        return;
    };
    let primaries = match color_primaries {
        1 => libjxl_sys::JXL_PRIMARIES_SRGB,
        9 => libjxl_sys::JXL_PRIMARIES_2100,
        11 => libjxl_sys::JXL_PRIMARIES_P3,
        _ => return,
    };

    let enc = libjxl_sys::JxlColorEncoding {
        color_space: libjxl_sys::JXL_COLOR_SPACE_RGB,
        white_point: libjxl_sys::JXL_WHITE_POINT_D65,
        white_point_xy: [0.0, 0.0],
        primaries,
        primaries_red_xy: [0.0, 0.0],
        primaries_green_xy: [0.0, 0.0],
        primaries_blue_xy: [0.0, 0.0],
        transfer_function: libjxl_sys::JXL_TRANSFER_FUNCTION_LINEAR,
        gamma: 0.0,
        rendering_intent: libjxl_sys::JXL_RENDERING_INTENT_RELATIVE,
    };

    let st = unsafe { libjxl_sys::JxlDecoderSetPreferredColorProfile(decoder, &enc) };
    if st != libjxl_sys::JXL_DEC_SUCCESS {
        log::debug!(
            "JxlDecoderSetPreferredColorProfile returned {st} (decoder may use its default XYB output)"
        );
    }
}

#[cfg(feature = "jpegxl")]
pub(crate) fn read_jxl_metadata(
    decoder: *const libjxl_sys::JxlDecoder,
    mut metadata: HdrImageMetadata,
) -> HdrImageMetadata {
    let saved_luminance = metadata.luminance;

    // 1) ENUM profile of **decoded pixels** (`JXL_COLOR_PROFILE_TARGET_DATA`) — when libjxl
    // can express the float buffer's encoding as a `JxlColorEncoding`, this is the most
    // accurate signal of what's actually in the buffer. For Modular-mode files libjxl
    // preserves the codestream encoding (TF=Linear → linear floats; TF=sRGB → already-
    // encoded floats), so trusting the enum here makes `jxl_sdr_grade_fallback_rgba8`
    // pick the right quantizer instead of always assuming "encoded floats" (which used to
    // break conformance `patches/input.jxl` to ~22 codes too dark across every pixel).
    let mut color_data = std::mem::MaybeUninit::<libjxl_sys::JxlColorEncoding>::zeroed();
    let encoded_data_status = unsafe {
        libjxl_sys::JxlDecoderGetColorAsEncodedProfile(
            decoder,
            libjxl_sys::JXL_COLOR_PROFILE_TARGET_DATA,
            color_data.as_mut_ptr(),
        )
    };
    if encoded_data_status == libjxl_sys::JXL_DEC_SUCCESS {
        let color = unsafe { color_data.assume_init() };
        let mut out = hdr_metadata_from_jxl_float_decode(&color);
        out.luminance = saved_luminance;
        jxl_tag_display_referred_when_sdr_grade(&mut out);
        return out;
    }

    // 2) ICC profile of decoded pixels (e.g. conformance `bench_oriented_brg` whose JPEG
    // reconstruction yields an sRGB ICC that libjxl can't express as enum, or
    // `patches_lossless` whose Display P3 linear ICC the same). Walk CICP first, then
    // RGB primary tags (parses `rTRC` to distinguish linear vs encoded ICCs), finally
    // fall back to a minimal "trust the ICC blob" path — that path itself parses `rTRC`
    // so the SDR-grade fallback applies (or skips) the sRGB OETF correctly.
    if let Some(icc) = jxl_decoder_copy_target_data_icc(decoder) {
        if let Some((p, t, m, fr)) = icc_scan_cicp_tag(&icc) {
            let mut out = hdr_metadata_from_h273_cicp_for_jxl_float_buffer(p, t, m, fr);
            out.luminance = saved_luminance;
            jxl_tag_display_referred_when_sdr_grade(&mut out);
            return out;
        }
        if let Some(mut out) = hdr_metadata_from_icc_rgb_xyz_primaries_for_jxl_float(&icc) {
            out.luminance = saved_luminance;
            jxl_tag_display_referred_when_sdr_grade(&mut out);
            return out;
        }
        let trc = icc_trc_kind(&icc).unwrap_or(HdrTransferFunction::Srgb);
        metadata.color_profile = HdrColorProfile::Icc(Arc::new(icc));
        metadata.transfer_function = trc;
        metadata.reference = HdrReference::Unknown;
        metadata.luminance = saved_luminance;
        crate::hdr::types::log_unrecognized_embedded_icc_after_decode(&metadata);
        jxl_tag_display_referred_when_sdr_grade(&mut metadata);
        return metadata;
    }

    // 3) ENUM profile of the **original** codestream — last resort when neither the
    // decoded enum nor a DATA ICC was exposed. Not strictly interchangeable with DATA but
    // libjxl's Modular path preserves the source encoding.
    let mut color_orig = std::mem::MaybeUninit::<libjxl_sys::JxlColorEncoding>::zeroed();
    let orig_status = unsafe {
        libjxl_sys::JxlDecoderGetColorAsEncodedProfile(
            decoder,
            libjxl_sys::JXL_COLOR_PROFILE_TARGET_ORIGINAL,
            color_orig.as_mut_ptr(),
        )
    };
    if orig_status == libjxl_sys::JXL_DEC_SUCCESS {
        let o = unsafe { color_orig.assume_init() };
        let mut out = hdr_metadata_from_jxl_float_decode(&o);
        out.luminance = saved_luminance;
        jxl_tag_display_referred_when_sdr_grade(&mut out);
        return out;
    }

    metadata.luminance = saved_luminance;
    jxl_tag_display_referred_when_sdr_grade(&mut metadata);
    metadata
}

