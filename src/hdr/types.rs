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

use std::collections::hash_map::DefaultHasher;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::sync::{LazyLock, Mutex};

pub const DEFAULT_SDR_WHITE_NITS: f32 = 203.0;
pub const DEFAULT_MAX_DISPLAY_NITS: f32 = 1000.0;

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HdrPixelFormat {
    Rgba16Float,
    Rgba32Float,
}

#[allow(dead_code)]
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HdrColorSpace {
    LinearSrgb = 0,
    LinearScRgb = 1,
    Rec2020Linear = 2,
    Aces2065_1 = 3,
    Xyz = 4,
    Unknown = 5,
    /// Display P3 primaries, D65 white, linear light. CICP colour primaries **11** (SMPTE 431 /
    /// DCI‑P3 family) and **12** (SMPTE EG 432‑1 / **Display P3**, common in AV1/AVIF e.g. libavif
    /// `*p3pq*` test assets).
    DisplayP3Linear = 6,
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HdrColorProfile {
    LinearSrgb,
    ColorSpace(HdrColorSpace),
    Cicp {
        color_primaries: u16,
        transfer_characteristics: u16,
        matrix_coefficients: u16,
        full_range: bool,
    },
    Icc(std::sync::Arc<Vec<u8>>),
    Unknown,
}

impl HdrColorProfile {
    pub fn from_color_space(color_space: HdrColorSpace) -> Self {
        match color_space {
            HdrColorSpace::LinearSrgb => Self::LinearSrgb,
            _ => Self::ColorSpace(color_space),
        }
    }
}

#[allow(dead_code)]
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HdrTransferFunction {
    Linear = 0,
    Srgb = 1,
    Pq = 2,
    Hlg = 3,
    Gamma = 4,
    Unknown = 5,
}

#[allow(dead_code)]
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HdrReference {
    SceneLinear = 0,
    DisplayReferred = 1,
    SdrGainMapBase = 2,
    Unknown = 3,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct HdrLuminanceMetadata {
    pub max_cll_nits: Option<f32>,
    pub max_fall_nits: Option<f32>,
    pub mastering_min_nits: Option<f32>,
    pub mastering_max_nits: Option<f32>,
    pub sdr_white_nits: Option<f32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HdrGainMapMetadata {
    pub source: &'static str,
    pub target_hdr_capacity: Option<f32>,
    pub diagnostic: String,
    /// True when RGB is **display-referred linear** in about 0–1 (e.g. libavif `avifImageApplyGainMap`).
    /// False for scene-linear / extended recovery (`append_hdr_pixel_from_sdr_and_gain`, JXL jhgm, …).
    pub capped_display_referred: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HdrImageMetadata {
    pub transfer_function: HdrTransferFunction,
    pub reference: HdrReference,
    pub color_profile: HdrColorProfile,
    pub luminance: HdrLuminanceMetadata,
    pub gain_map: Option<HdrGainMapMetadata>,
}

impl HdrImageMetadata {
    pub fn from_color_space(color_space: HdrColorSpace) -> Self {
        Self {
            color_profile: HdrColorProfile::from_color_space(color_space),
            ..Self::default()
        }
    }

    pub fn color_space_hint(&self) -> HdrColorSpace {
        match self.color_profile {
            HdrColorProfile::LinearSrgb => HdrColorSpace::LinearSrgb,
            HdrColorProfile::ColorSpace(color_space) => color_space,
            // **Matrix coefficients** 9/10/12 are BT.2020 NCL/CL variants: libavif/YUV→RGB produces
            // **Rec.2020** display-referred RGB. Some AVIF (incl. bad conformance tags) declare **colour_primaries 1**
            // with matrix 10; matching `primaries 1` first would skip WGSL Rec.2020→linear-sRGB → blue.
            HdrColorProfile::Cicp {
                matrix_coefficients: 9 | 10 | 12,
                ..
            } => HdrColorSpace::Rec2020Linear,
            HdrColorProfile::Cicp {
                color_primaries: 9, ..
            } => HdrColorSpace::Rec2020Linear,
            HdrColorProfile::Cicp {
                color_primaries: 11 | 12, ..
            } => HdrColorSpace::DisplayP3Linear,
            HdrColorProfile::Cicp {
                color_primaries: 1, ..
            } => HdrColorSpace::LinearSrgb,
            HdrColorProfile::Icc(ref data) => embedded_icc_profile_color_space_hint(data.as_slice()),
            _ => HdrColorSpace::Unknown,
        }
    }

    #[allow(dead_code)]
    pub fn transfer_short_label(&self) -> &'static str {
        match self.transfer_function {
            HdrTransferFunction::Linear => "Linear",
            HdrTransferFunction::Srgb => "sRGB",
            HdrTransferFunction::Pq => "PQ",
            HdrTransferFunction::Hlg => "HLG",
            HdrTransferFunction::Gamma => "Gamma",
            HdrTransferFunction::Unknown => "Unknown",
        }
    }
}

/// Emit **decode-time** diagnostics for embedded ICC that `embedded_icc_profile_color_space_hint`
/// could not classify. Call from loader / decode paths only (e.g. background decode thread), not from
/// `color_space_hint()` which also runs on UI/HDR draw hot paths.
pub(crate) fn log_unrecognized_embedded_icc_after_decode(metadata: &HdrImageMetadata) {
    let HdrColorProfile::Icc(ref data) = metadata.color_profile else {
        return;
    };
    let icc = data.as_slice();
    if embedded_icc_profile_color_space_hint(icc) == HdrColorSpace::Unknown {
        log_unrecognized_embedded_icc_profile_once(icc);
    }
}

impl Default for HdrImageMetadata {
    fn default() -> Self {
        Self {
            transfer_function: HdrTransferFunction::Linear,
            reference: HdrReference::Unknown,
            color_profile: HdrColorProfile::LinearSrgb,
            luminance: HdrLuminanceMetadata::default(),
            gain_map: None,
        }
    }
}

/// Embedded ICC: **only** ISO 15076–based classification via **Little CMS 2** (read ICC `rXYZ` /
/// `gXYZ` / `bXYZ` and compare chromaticities to **ITU/SMPTE** tabulated primaries). **CICP** is
/// handled in [`Self::color_space_hint`] for [`HdrColorProfile::Cicp`] (ITU-T H.273). There is **no**
/// substring / `mluc` text matching. Builds **without** `jpegxl` cannot link `lcms2` here —
/// [`HdrColorProfile::Icc`] yields [`HdrColorSpace::Unknown`] (honest, not guessed).
fn embedded_icc_profile_color_space_hint(icc: &[u8]) -> HdrColorSpace {
    #[cfg(feature = "jpegxl")]
    {
        use crate::hdr::icc_primaries_lcms::{classify_embedded_icc_primaries, EmbeddedIccHint};
        match classify_embedded_icc_primaries(icc) {
            EmbeddedIccHint::Classified(cs) => cs,
            EmbeddedIccHint::RgbPrimariesUnmatched | EmbeddedIccHint::IccPrimariesNotReadable => {
                HdrColorSpace::Unknown
            }
        }
    }
    #[cfg(not(feature = "jpegxl"))]
    {
        let _ = icc;
        HdrColorSpace::Unknown
    }
}

/// Deduplicate ICC blobs so opening the same file twice does not repeat huge hex lines; not for UI throttling.
const ICC_UNRECOGNIZED_LOG_HEX_BYTES: usize = 256;

static ICC_UNRECOGNIZED_LOG_DEDUPE: LazyLock<Mutex<HashSet<u64>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

fn icc_profile_fingerprint_for_dedup(icc: &[u8]) -> u64 {
    let mut h = DefaultHasher::new();
    icc.len().hash(&mut h);
    for b in icc.iter().take(512) {
        b.hash(&mut h);
    }
    h.finish()
}

fn icc_bytes_to_lower_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len().saturating_mul(2));
    for b in bytes {
        let _ = write!(s, "{:02x}", b);
    }
    s
}

/// Raw hex only — embedded text may be UTF-16 `mluc` / vendor-specific; encoding is not assumed.
fn log_unrecognized_embedded_icc_profile_once(icc: &[u8]) {
    let fp = icc_profile_fingerprint_for_dedup(icc);
    let mut seen = ICC_UNRECOGNIZED_LOG_DEDUPE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if !seen.insert(fp) {
        return;
    }

    let len = icc.len();
    let preview_n = len.min(ICC_UNRECOGNIZED_LOG_HEX_BYTES);
    let head_hex = icc_bytes_to_lower_hex(&icc[..preview_n]);
    let tail_note = if len > ICC_UNRECOGNIZED_LOG_HEX_BYTES {
        format!(" [log truncated: first {} of {} bytes as hex]", preview_n, len)
    } else {
        String::new()
    };
    log::debug!(
        "[HDR] embedded ICC: primaries not classified (ICC.1 + lcms: invalid/non-RGB/missing XYZ tags or xy outside BT.709 | P3 | BT.2020); len={} hex_preview={}{}",
        len,
        head_hex,
        tail_note
    );
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HdrOutputMode {
    SdrToneMapped,
    WindowsScRgb,
    MacOsEdr,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub struct HdrToneMapSettings {
    pub exposure_ev: f32,
    pub sdr_white_nits: f32,
    pub max_display_nits: f32,
}

impl Default for HdrToneMapSettings {
    fn default() -> Self {
        Self {
            exposure_ev: 0.0,
            sdr_white_nits: DEFAULT_SDR_WHITE_NITS,
            max_display_nits: DEFAULT_MAX_DISPLAY_NITS,
        }
    }
}

impl HdrToneMapSettings {
    pub fn target_hdr_capacity(self) -> f32 {
        self.max_display_nits.max(self.sdr_white_nits.max(1.0)) / self.sdr_white_nits.max(1.0)
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct HdrImageBuffer {
    pub width: u32,
    pub height: u32,
    pub format: HdrPixelFormat,
    pub color_space: HdrColorSpace,
    pub metadata: HdrImageMetadata,
    pub rgba_f32: std::sync::Arc<Vec<f32>>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TilePixelFormat {
    SdrRgba8,
    HdrRgba32F,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum TilePixelBuffer {
    SdrRgba8(std::sync::Arc<Vec<u8>>),
    HdrRgba32F(std::sync::Arc<Vec<f32>>),
}

#[allow(dead_code)]
impl TilePixelBuffer {
    pub fn pixel_format(&self) -> TilePixelFormat {
        match self {
            Self::SdrRgba8(_) => TilePixelFormat::SdrRgba8,
            Self::HdrRgba32F(_) => TilePixelFormat::HdrRgba32F,
        }
    }

    pub fn bytes_per_pixel(&self) -> usize {
        match self {
            Self::SdrRgba8(_) => 4,
            Self::HdrRgba32F(_) => 4 * std::mem::size_of::<f32>(),
        }
    }

    pub fn len_bytes(&self) -> usize {
        match self {
            Self::SdrRgba8(pixels) => pixels.len(),
            Self::HdrRgba32F(pixels) => pixels.len() * std::mem::size_of::<f32>(),
        }
    }

    pub fn is_hdr(&self) -> bool {
        matches!(self, Self::HdrRgba32F(_))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        HdrColorProfile, HdrColorSpace, HdrGainMapMetadata, HdrImageMetadata, HdrReference,
        HdrToneMapSettings, HdrTransferFunction, TilePixelBuffer, TilePixelFormat,
    };
    use std::sync::Arc;

    #[test]
    fn sdr_tile_buffer_reports_rgba8_accounting() {
        let pixels = Arc::new(vec![0_u8; 2 * 3 * 4]);
        let buffer = TilePixelBuffer::SdrRgba8(Arc::clone(&pixels));

        assert_eq!(buffer.pixel_format(), TilePixelFormat::SdrRgba8);
        assert_eq!(buffer.bytes_per_pixel(), 4);
        assert_eq!(buffer.len_bytes(), pixels.len());
        assert!(!buffer.is_hdr());
    }

    #[test]
    fn hdr_tile_buffer_reports_rgba32f_accounting() {
        let pixels = Arc::new(vec![0.0_f32; 2 * 3 * 4]);
        let buffer = TilePixelBuffer::HdrRgba32F(Arc::clone(&pixels));

        assert_eq!(buffer.pixel_format(), TilePixelFormat::HdrRgba32F);
        assert_eq!(buffer.bytes_per_pixel(), 16);
        assert_eq!(
            buffer.len_bytes(),
            pixels.len() * std::mem::size_of::<f32>()
        );
        assert!(buffer.is_hdr());
    }

    #[test]
    fn tone_map_settings_report_target_hdr_capacity() {
        let settings = HdrToneMapSettings {
            exposure_ev: 0.0,
            sdr_white_nits: 200.0,
            max_display_nits: 1000.0,
        };

        assert_eq!(settings.target_hdr_capacity(), 5.0);
    }

    #[test]
    fn hdr_image_metadata_defaults_match_existing_linear_srgb_behavior() {
        let metadata = HdrImageMetadata::default();

        assert_eq!(metadata.transfer_function, HdrTransferFunction::Linear);
        assert_eq!(metadata.reference, HdrReference::Unknown);
        assert_eq!(metadata.color_profile, HdrColorProfile::LinearSrgb);
        assert!(metadata.luminance.max_cll_nits.is_none());
        assert!(metadata.luminance.max_fall_nits.is_none());
        assert!(metadata.luminance.mastering_min_nits.is_none());
        assert!(metadata.luminance.mastering_max_nits.is_none());
        assert!(metadata.luminance.sdr_white_nits.is_none());
        assert!(metadata.gain_map.is_none());
    }

    #[test]
    fn hdr_metadata_reports_short_transfer_labels_for_osd() {
        let metadata = HdrImageMetadata {
            transfer_function: HdrTransferFunction::Pq,
            ..HdrImageMetadata::default()
        };

        assert_eq!(metadata.transfer_short_label(), "PQ");
    }

    #[test]
    fn embedded_icc_invalid_or_opaque_blob_yields_unknown() {
        let icc = vec![0xFF_u8; 512];
        let metadata = HdrImageMetadata {
            color_profile: HdrColorProfile::Icc(Arc::new(icc)),
            ..HdrImageMetadata::default()
        };
        assert_eq!(metadata.color_space_hint(), HdrColorSpace::Unknown);
    }

    #[test]
    fn cicp_color_primaries_1_maps_to_linear_srgb_hint() {
        let metadata = HdrImageMetadata {
            color_profile: HdrColorProfile::Cicp {
                color_primaries: 1,
                transfer_characteristics: 8,
                matrix_coefficients: 0,
                full_range: true,
            },
            ..HdrImageMetadata::default()
        };

        assert_eq!(metadata.color_space_hint(), HdrColorSpace::LinearSrgb);
    }

    #[test]
    fn cicp_unspecified_primaries_bt2020_matrix_maps_to_rec2020_linear_hint() {
        let metadata = HdrImageMetadata {
            color_profile: HdrColorProfile::Cicp {
                color_primaries: 2,
                transfer_characteristics: 16,
                matrix_coefficients: 9,
                full_range: true,
            },
            ..HdrImageMetadata::default()
        };

        assert_eq!(metadata.color_space_hint(), HdrColorSpace::Rec2020Linear);
    }

    #[test]
    fn cicp_bt709_primaries_bt2020_matrix_prefers_rec2020_hint() {
        let metadata = HdrImageMetadata {
            color_profile: HdrColorProfile::Cicp {
                color_primaries: 1,
                transfer_characteristics: 16,
                matrix_coefficients: 10,
                full_range: true,
            },
            ..HdrImageMetadata::default()
        };

        assert_eq!(metadata.color_space_hint(), HdrColorSpace::Rec2020Linear);
    }

    #[test]
    fn cicp_color_primaries_12_display_p3_maps_to_display_p3_linear_hint() {
        let metadata = HdrImageMetadata {
            color_profile: HdrColorProfile::Cicp {
                color_primaries: 12,
                transfer_characteristics: 16,
                matrix_coefficients: 0,
                full_range: true,
            },
            ..HdrImageMetadata::default()
        };

        assert_eq!(
            metadata.color_space_hint(),
            HdrColorSpace::DisplayP3Linear,
            "AV1/AVIF Display P3 is primaries=12 (SMPTE EG 432-1), not 11"
        );
    }

    #[test]
    fn cicp_color_primaries_11_still_maps_to_display_p3_linear_hint() {
        let metadata = HdrImageMetadata {
            color_profile: HdrColorProfile::Cicp {
                color_primaries: 11,
                transfer_characteristics: 16,
                matrix_coefficients: 0,
                full_range: true,
            },
            ..HdrImageMetadata::default()
        };

        assert_eq!(metadata.color_space_hint(), HdrColorSpace::DisplayP3Linear);
    }

    #[test]
    fn hdr_metadata_can_carry_gain_map_diagnostics() {
        let metadata = HdrImageMetadata {
            gain_map: Some(HdrGainMapMetadata {
                source: "AVIF",
                target_hdr_capacity: Some(4.0),
                diagnostic: "GainMapMax=[2.000,2.000,2.000]".to_string(),
                capped_display_referred: false,
            }),
            ..HdrImageMetadata::default()
        };

        let gain_map = metadata.gain_map.as_ref().expect("gain-map marker");
        assert_eq!(gain_map.source, "AVIF");
        assert_eq!(gain_map.target_hdr_capacity, Some(4.0));
        assert!(gain_map.diagnostic.contains("GainMapMax"));
    }
}
