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
    bytes_per_sample, checked_section_end, read_u16, read_u32, read_u64, seek_forward_within,
    validate_psd_dimensions,
};

const IMAGE_DATA_POS_OVERFLOW: &str = "PSD/PSB image_data_pos overflows usize";
const IMAGE_DATA_POS_END_OVERFLOW: &str = "PSD/PSB image_data_pos end overflows usize";
const IMAGE_DATA_COMPRESSION_TRUNCATED: &str = "PSD/PSB Image Data compression truncated";

/// Explicit classification for [`PsdSectionIndex::parse`] failures.
///
/// Callers must match on [`SectionParseErrorKind`] (or
/// [`SectionParseError::is_structural`]) instead of substring-matching the
/// display text -- checklist #30.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionParseErrorKind {
    /// Header / section-boundary failure: P1 and P2 cannot proceed.
    Structural,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionParseError {
    pub kind: SectionParseErrorKind,
    message: String,
}

impl SectionParseError {
    pub fn structural(message: impl Into<String>) -> Self {
        Self {
            kind: SectionParseErrorKind::Structural,
            message: message.into(),
        }
    }

    pub fn is_structural(&self) -> bool {
        matches!(self.kind, SectionParseErrorKind::Structural)
    }

    pub fn as_str(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for SectionParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SectionParseError {}

impl From<SectionParseError> for String {
    fn from(err: SectionParseError) -> Self {
        err.message
    }
}

/// True when `kind` should skip P2 and fall through to P3-only recovery.
pub fn is_structural_kind(kind: SectionParseErrorKind) -> bool {
    matches!(kind, SectionParseErrorKind::Structural)
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

impl PsdSectionIndex {
    pub fn parse(bytes: &[u8]) -> Result<Self, SectionParseError> {
        // Full fixed header is 26 bytes (signature through color mode).
        if bytes.len() < 26 {
            return Err(SectionParseError::structural("PSD/PSB header is too short"));
        }

        let file_size = bytes.len() as u64;
        let mut r = std::io::Cursor::new(bytes);

        let mut sig = [0u8; 4];
        r.read_exact(&mut sig)
            .map_err(|e| SectionParseError::structural(format!("Read error: {e}")))?;
        if &sig != b"8BPS" {
            return Err(SectionParseError::structural(
                "Not a PSD/PSB file (invalid signature)",
            ));
        }

        let version = read_u16(&mut r).map_err(SectionParseError::structural)?;
        if version != 1 && version != 2 {
            return Err(SectionParseError::structural(format!(
                "Unknown PSD/PSB version: {version}"
            )));
        }
        let is_psb = version == 2;

        seek_forward_within(&mut r, 6, file_size, "reserved header bytes")
            .map_err(SectionParseError::structural)?;

        let channels = read_u16(&mut r).map_err(SectionParseError::structural)? as u32;
        let height = read_u32(&mut r).map_err(SectionParseError::structural)?;
        let width = read_u32(&mut r).map_err(SectionParseError::structural)?;
        let depth = read_u16(&mut r).map_err(SectionParseError::structural)?;
        let color_mode = read_u16(&mut r).map_err(SectionParseError::structural)?;

        validate_psd_dimensions(width, height, channels).map_err(SectionParseError::structural)?;
        bytes_per_sample(depth).map_err(SectionParseError::structural)?;

        let cm_len = read_u32(&mut r).map_err(SectionParseError::structural)? as u64;
        seek_forward_within(&mut r, cm_len, file_size, "color mode data")
            .map_err(SectionParseError::structural)?;

        let ir_len = read_u32(&mut r).map_err(SectionParseError::structural)? as u64;
        let ir_start = r
            .stream_position()
            .map_err(|e| SectionParseError::structural(format!("Stream position error: {e}")))?;
        let ir_end = checked_section_end(ir_start, ir_len, file_size, "image resources")
            .map_err(SectionParseError::structural)?;
        seek_forward_within(&mut r, ir_len, file_size, "image resources")
            .map_err(SectionParseError::structural)?;

        let lm_len = if is_psb {
            read_u64(&mut r).map_err(SectionParseError::structural)?
        } else {
            read_u32(&mut r).map_err(SectionParseError::structural)? as u64
        };
        let lm_start = r
            .stream_position()
            .map_err(|e| SectionParseError::structural(format!("Stream position error: {e}")))?;
        let lm_end = checked_section_end(lm_start, lm_len, file_size, "layer and mask info")
            .map_err(SectionParseError::structural)?;
        seek_forward_within(&mut r, lm_len, file_size, "layer and mask info")
            .map_err(SectionParseError::structural)?;

        let image_data_pos = r
            .stream_position()
            .map_err(|e| SectionParseError::structural(format!("Stream position error: {e}")))?;

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
    use super::{PsdSectionIndex, SectionParseError, SectionParseErrorKind, is_structural_kind};

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
        assert!(err.is_structural());
        assert!(
            err.as_str().contains("invalid signature") || err.as_str().contains("Not a PSD"),
            "{err}"
        );
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

        assert!(err.is_structural(), "{err}");
        assert!(is_structural_kind(err.kind));
    }

    #[test]
    fn structural_kind_is_explicit() {
        assert!(is_structural_kind(SectionParseErrorKind::Structural));
        assert!(SectionParseError::structural("any message").is_structural());
    }
}
