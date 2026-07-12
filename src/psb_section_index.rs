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

//! One structural walk of a PSD/PSB file: header fields + section offsets.

use std::fmt;
use std::io::{Read, Seek};

use crate::psb_reader::{
    bytes_per_sample, checked_section_end, ensure_supported_color_mode, read_u16, read_u32,
    read_u64, seek_forward_within, validate_psd_dimensions,
};

const IMAGE_DATA_POS_OVERFLOW: &str = "PSD/PSB image_data_pos overflows usize";
const IMAGE_DATA_POS_END_OVERFLOW: &str = "PSD/PSB image_data_pos end overflows usize";
const IMAGE_DATA_COMPRESSION_TRUNCATED: &str = "PSD/PSB Image Data compression truncated";

/// Typed failures from [`PsdSectionIndex::parse`].
///
/// Callers must match on variants (or [`Self::is_structural`]) instead of
/// substring-matching display text -- checklist #30.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SectionParseError {
    /// File shorter than the required header / section-length fields.
    Truncated,
    /// Signature is not `8BPS`.
    BadSignature,
    /// Version is neither 1 (PSD) nor 2 (PSB).
    UnsupportedVersion(u16),
    /// Width / height / channel count failed validation.
    Dimensions(String),
    /// Bit depth is not 8 / 16 / 32.
    UnsupportedDepth(u16),
    /// Color mode is not Gray / RGB / CMYK.
    UnsupportedColorMode(u16),
    /// Section length overflows or exceeds the file size.
    SectionOverflow { label: &'static str, detail: String },
    /// Cursor read / seek failure while walking the header.
    Io(String),
}

impl SectionParseError {
    /// True when P1/P2 cannot proceed and the caller should fall through to
    /// P3-only recovery. Every variant from the structural walk qualifies.
    pub fn is_structural(&self) -> bool {
        matches!(
            self,
            Self::Truncated
                | Self::BadSignature
                | Self::UnsupportedVersion(_)
                | Self::Dimensions(_)
                | Self::UnsupportedDepth(_)
                | Self::UnsupportedColorMode(_)
                | Self::SectionOverflow { .. }
                | Self::Io(_)
        )
    }

    fn section_overflow(label: &'static str, detail: String) -> Self {
        Self::SectionOverflow { label, detail }
    }
}

impl fmt::Display for SectionParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => f.write_str("PSD/PSB header is too short"),
            Self::BadSignature => f.write_str("Not a PSD/PSB file (invalid signature)"),
            Self::UnsupportedVersion(v) => write!(f, "Unknown PSD/PSB version: {v}"),
            Self::Dimensions(msg) | Self::Io(msg) => f.write_str(msg),
            Self::UnsupportedDepth(d) => write!(
                f,
                "Unsupported PSD/PSB bit depth {d} (supported: 8, 16, 32)"
            ),
            Self::UnsupportedColorMode(m) => {
                write!(
                    f,
                    "{}",
                    rust_i18n::t!("error.psd_unsupported_color_mode", mode = m)
                )
            }
            Self::SectionOverflow { detail, .. } => f.write_str(detail),
        }
    }
}

impl std::error::Error for SectionParseError {}

impl From<SectionParseError> for String {
    fn from(err: SectionParseError) -> Self {
        err.to_string()
    }
}

#[derive(Debug, Clone)]
pub struct PsdSectionIndex {
    pub is_psb: bool,
    pub width: u32,
    pub height: u32,
    pub channels: u32,
    pub depth: u16,
    pub color_mode: u16,
    pub ir_start: u64,
    pub ir_end: u64,
    pub lm_start: u64,
    pub lm_end: u64,
    /// Absolute offset of Image Data compression `u16`.
    pub image_data_pos: u64,
}

/// Bytes needed for signature + version before PSD vs PSB minima diverge.
const PSD_SIG_VERSION_LEN: usize = 6;
/// Empty-section minimum: fixed header (26) + cm_len(4) + ir_len(4) + lm_len(4).
const PSD_MIN_STRUCTURAL_LEN: usize = 38;
/// Empty-section minimum: fixed header (26) + cm_len(4) + ir_len(4) + lm_len(8).
const PSB_MIN_STRUCTURAL_LEN: usize = 42;

impl PsdSectionIndex {
    pub fn parse(bytes: &[u8]) -> Result<Self, SectionParseError> {
        // Need signature + version first so we can apply the PSD/PSB-specific
        // minimum that covers cm_len + ir_len + lm_len (not just the 26-byte
        // fixed header through color mode).
        if bytes.len() < PSD_SIG_VERSION_LEN {
            return Err(SectionParseError::Truncated);
        }

        let file_size = bytes.len() as u64;
        let mut r = std::io::Cursor::new(bytes);

        let mut sig = [0u8; 4];
        r.read_exact(&mut sig)
            .map_err(|e| SectionParseError::Io(format!("Read error: {e}")))?;
        if &sig != b"8BPS" {
            return Err(SectionParseError::BadSignature);
        }

        let version = read_u16(&mut r).map_err(SectionParseError::Io)?;
        if version != 1 && version != 2 {
            return Err(SectionParseError::UnsupportedVersion(version));
        }
        let is_psb = version == 2;
        let min_len = if is_psb {
            PSB_MIN_STRUCTURAL_LEN
        } else {
            PSD_MIN_STRUCTURAL_LEN
        };
        if bytes.len() < min_len {
            return Err(SectionParseError::Truncated);
        }

        seek_forward_within(&mut r, 6, file_size, "reserved header bytes").map_err(|detail| {
            SectionParseError::section_overflow("reserved header bytes", detail)
        })?;

        let channels = read_u16(&mut r).map_err(SectionParseError::Io)? as u32;
        let height = read_u32(&mut r).map_err(SectionParseError::Io)?;
        let width = read_u32(&mut r).map_err(SectionParseError::Io)?;
        let depth = read_u16(&mut r).map_err(SectionParseError::Io)?;
        let color_mode = read_u16(&mut r).map_err(SectionParseError::Io)?;

        validate_psd_dimensions(width, height, channels).map_err(SectionParseError::Dimensions)?;
        match bytes_per_sample(depth) {
            Ok(_) => {}
            Err(_) => return Err(SectionParseError::UnsupportedDepth(depth)),
        }
        ensure_supported_color_mode(color_mode)
            .map_err(|_| SectionParseError::UnsupportedColorMode(color_mode))?;

        let cm_len = read_u32(&mut r).map_err(SectionParseError::Io)? as u64;
        seek_forward_within(&mut r, cm_len, file_size, "color mode data")
            .map_err(|detail| SectionParseError::section_overflow("color mode data", detail))?;

        let ir_len = read_u32(&mut r).map_err(SectionParseError::Io)? as u64;
        let ir_start = r
            .stream_position()
            .map_err(|e| SectionParseError::Io(format!("Stream position error: {e}")))?;
        let ir_end = checked_section_end(ir_start, ir_len, file_size, "image resources")
            .map_err(|detail| SectionParseError::section_overflow("image resources", detail))?;
        seek_forward_within(&mut r, ir_len, file_size, "image resources")
            .map_err(|detail| SectionParseError::section_overflow("image resources", detail))?;

        let lm_len = if is_psb {
            read_u64(&mut r).map_err(SectionParseError::Io)?
        } else {
            read_u32(&mut r).map_err(SectionParseError::Io)? as u64
        };
        let lm_start = r
            .stream_position()
            .map_err(|e| SectionParseError::Io(format!("Stream position error: {e}")))?;
        let lm_end = checked_section_end(lm_start, lm_len, file_size, "layer and mask info")
            .map_err(|detail| SectionParseError::section_overflow("layer and mask info", detail))?;
        seek_forward_within(&mut r, lm_len, file_size, "layer and mask info")
            .map_err(|detail| SectionParseError::section_overflow("layer and mask info", detail))?;

        let image_data_pos = r
            .stream_position()
            .map_err(|e| SectionParseError::Io(format!("Stream position error: {e}")))?;

        Ok(Self {
            is_psb,
            width,
            height,
            channels,
            depth,
            color_mode,
            ir_start,
            ir_end,
            lm_start,
            lm_end,
            image_data_pos,
        })
    }

    pub fn image_data_compression(&self, bytes: &[u8]) -> Result<u16, String> {
        let pos = usize::try_from(self.image_data_pos)
            .map_err(|_| IMAGE_DATA_POS_OVERFLOW.to_string())?;
        let end = pos
            .checked_add(2)
            .ok_or_else(|| IMAGE_DATA_POS_END_OVERFLOW.to_string())?;
        let slice = bytes
            .get(pos..end)
            .ok_or_else(|| IMAGE_DATA_COMPRESSION_TRUNCATED.to_string())?;
        Ok(u16::from_be_bytes([slice[0], slice[1]]))
    }
}

#[cfg(test)]
mod tests {
    use super::{PsdSectionIndex, SectionParseError};

    fn minimal_psd_bytes() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"8BPS");
        bytes.extend_from_slice(&1u16.to_be_bytes());
        bytes.extend_from_slice(&[0; 6]);
        bytes.extend_from_slice(&3u16.to_be_bytes());
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.extend_from_slice(&8u16.to_be_bytes());
        bytes.extend_from_slice(&3u16.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&0u16.to_be_bytes());
        bytes
    }

    fn minimal_psb_bytes() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"8BPS");
        bytes.extend_from_slice(&2u16.to_be_bytes());
        bytes.extend_from_slice(&[0; 6]);
        bytes.extend_from_slice(&3u16.to_be_bytes());
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.extend_from_slice(&8u16.to_be_bytes());
        bytes.extend_from_slice(&3u16.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&0u64.to_be_bytes());
        bytes.extend_from_slice(&1u16.to_be_bytes());
        bytes
    }

    fn psd_with_truncated_layer_mask() -> Vec<u8> {
        let mut bytes = minimal_psd_bytes();
        bytes.truncate(34);
        bytes.extend_from_slice(&4u32.to_be_bytes());
        bytes.push(0);
        bytes
    }

    #[test]
    fn parse_rejects_bad_signature() {
        let mut bytes = vec![0u8; 26];
        bytes[..4].copy_from_slice(b"XXXX");
        let err = PsdSectionIndex::parse(&bytes).unwrap_err();
        assert!(matches!(err, SectionParseError::BadSignature));
        assert!(err.is_structural());
    }

    #[test]
    fn parse_rejects_unsupported_version() {
        let mut bytes = vec![0u8; 26];
        bytes[..4].copy_from_slice(b"8BPS");
        bytes[4..6].copy_from_slice(&99u16.to_be_bytes());
        let err = PsdSectionIndex::parse(&bytes).unwrap_err();
        assert_eq!(err, SectionParseError::UnsupportedVersion(99));
        assert!(err.is_structural());
    }

    #[test]
    fn parse_rejects_truncated_before_section_lengths() {
        // Valid sig+version+fixed fields (26 bytes) but missing cm_len/ir_len/lm_len.
        let mut bytes = vec![0u8; 26];
        bytes[..4].copy_from_slice(b"8BPS");
        bytes[4..6].copy_from_slice(&1u16.to_be_bytes());
        let err = PsdSectionIndex::parse(&bytes).unwrap_err();
        assert!(matches!(err, SectionParseError::Truncated));
        assert!(err.is_structural());

        let mut psb = bytes;
        psb[4..6].copy_from_slice(&2u16.to_be_bytes());
        // 38 bytes is enough for PSD but not PSB (needs 42).
        psb.resize(38, 0);
        let err = PsdSectionIndex::parse(&psb).unwrap_err();
        assert!(matches!(err, SectionParseError::Truncated));
        assert!(err.is_structural());
    }

    #[test]
    fn image_data_compression_rejects_pos_overflowing_usize() {
        let index = PsdSectionIndex {
            is_psb: false,
            width: 1,
            height: 1,
            channels: 3,
            depth: 8,
            color_mode: 3,
            ir_start: 0,
            ir_end: 0,
            lm_start: 0,
            lm_end: 0,
            image_data_pos: u64::MAX,
        };
        let err = index.image_data_compression(&[]).unwrap_err();
        assert!(
            err.contains("image_data_pos") && err.contains("overflow"),
            "{err}"
        );
    }

    #[test]
    fn parse_minimal_header_offsets_are_ordered() {
        let bytes = minimal_psd_bytes();
        let index = PsdSectionIndex::parse(&bytes).unwrap();

        assert!(!index.is_psb);
        assert_eq!(index.width, 1);
        assert_eq!(index.height, 1);
        assert_eq!(index.channels, 3);
        assert_eq!(index.depth, 8);
        assert_eq!(index.color_mode, 3);
        assert!(index.ir_start <= index.ir_end);
        assert!(index.ir_end <= index.lm_start);
        assert!(index.lm_start <= index.lm_end);
        assert!(index.lm_end <= index.image_data_pos);
        assert!(index.image_data_pos + 2 <= bytes.len() as u64);
        assert_eq!(index.image_data_compression(&bytes).unwrap(), 0);
    }

    #[test]
    fn parse_minimal_psb_uses_u64_layer_mask_length() {
        let bytes = minimal_psb_bytes();
        let index = PsdSectionIndex::parse(&bytes).unwrap();

        assert!(index.is_psb);
        assert!(index.ir_start <= index.ir_end);
        assert!(index.ir_end <= index.lm_start);
        assert!(index.lm_start <= index.lm_end);
        assert!(index.lm_end <= index.image_data_pos);
        assert!(index.image_data_pos + 2 <= bytes.len() as u64);
        assert_eq!(index.image_data_compression(&bytes).unwrap(), 1);
    }

    #[test]
    fn parse_truncated_mid_section_is_structural_error() {
        let err = PsdSectionIndex::parse(&psd_with_truncated_layer_mask()).unwrap_err();

        assert!(
            matches!(
                err,
                SectionParseError::SectionOverflow {
                    label: "layer and mask info",
                    ..
                }
            ),
            "{err:?}"
        );
        assert!(err.is_structural());
    }

    #[test]
    fn structural_classification_matches_variants() {
        assert!(SectionParseError::Truncated.is_structural());
        assert!(SectionParseError::BadSignature.is_structural());
        assert!(SectionParseError::UnsupportedVersion(3).is_structural());
        assert!(SectionParseError::Dimensions("x".into()).is_structural());
        assert!(SectionParseError::UnsupportedDepth(7).is_structural());
        assert!(SectionParseError::UnsupportedColorMode(9).is_structural());
        assert!(
            SectionParseError::SectionOverflow {
                label: "image resources",
                detail: "overflow".into(),
            }
            .is_structural()
        );
        assert!(SectionParseError::Io("read".into()).is_structural());
    }

    #[test]
    fn unsupported_color_mode_is_rejected_at_parse() {
        let mut bytes = vec![0u8; 50];
        bytes[0..4].copy_from_slice(b"8BPS");
        bytes[4..6].copy_from_slice(&1u16.to_be_bytes());
        bytes[12..14].copy_from_slice(&3u16.to_be_bytes()); // channels
        bytes[14..18].copy_from_slice(&8u32.to_be_bytes()); // height
        bytes[18..22].copy_from_slice(&8u32.to_be_bytes()); // width
        bytes[22..24].copy_from_slice(&8u16.to_be_bytes()); // depth
        bytes[24..26].copy_from_slice(&9u16.to_be_bytes()); // Lab -- unsupported
        // color mode data len + ir len + layer len = 0
        let err = PsdSectionIndex::parse(&bytes).unwrap_err();
        assert_eq!(err, SectionParseError::UnsupportedColorMode(9));
        assert!(err.is_structural());
    }
}
