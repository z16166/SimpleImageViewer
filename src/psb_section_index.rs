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

use std::io::{Read, Seek};

use crate::psb_reader::{
    bytes_per_sample, checked_section_end, read_u16, read_u32, read_u64, seek_forward_within,
    validate_psd_dimensions,
};

const IMAGE_DATA_POS_OVERFLOW: &str = "PSD/PSB image_data_pos overflows usize";
const IMAGE_DATA_POS_END_OVERFLOW: &str = "PSD/PSB image_data_pos end overflows usize";
const IMAGE_DATA_COMPRESSION_TRUNCATED: &str = "PSD/PSB Image Data compression truncated";

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
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        // Full fixed header is 26 bytes (signature through color mode).
        if bytes.len() < 26 {
            return Err("PSD/PSB header is too short".into());
        }

        let file_size = bytes.len() as u64;
        let mut r = std::io::Cursor::new(bytes);

        let mut sig = [0u8; 4];
        r.read_exact(&mut sig)
            .map_err(|e| format!("Read error: {e}"))?;
        if &sig != b"8BPS" {
            return Err("Not a PSD/PSB file (invalid signature)".into());
        }

        let version = read_u16(&mut r)?;
        if version != 1 && version != 2 {
            return Err(format!("Unknown PSD/PSB version: {version}"));
        }
        let is_psb = version == 2;

        seek_forward_within(&mut r, 6, file_size, "reserved header bytes")?;

        let channels = read_u16(&mut r)? as u32;
        let height = read_u32(&mut r)?;
        let width = read_u32(&mut r)?;
        let depth = read_u16(&mut r)?;
        let color_mode = read_u16(&mut r)?;

        validate_psd_dimensions(width, height, channels)?;
        bytes_per_sample(depth)?;

        let cm_len = read_u32(&mut r)? as u64;
        seek_forward_within(&mut r, cm_len, file_size, "color mode data")?;

        let ir_len = read_u32(&mut r)? as u64;
        let ir_start = r
            .stream_position()
            .map_err(|e| format!("Stream position error: {e}"))?;
        let ir_end = checked_section_end(ir_start, ir_len, file_size, "image resources")?;
        seek_forward_within(&mut r, ir_len, file_size, "image resources")?;

        let lm_len = if is_psb {
            read_u64(&mut r)?
        } else {
            read_u32(&mut r)? as u64
        };
        let lm_start = r
            .stream_position()
            .map_err(|e| format!("Stream position error: {e}"))?;
        let lm_end = checked_section_end(lm_start, lm_len, file_size, "layer and mask info")?;
        seek_forward_within(&mut r, lm_len, file_size, "layer and mask info")?;

        let image_data_pos = r
            .stream_position()
            .map_err(|e| format!("Stream position error: {e}"))?;

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

    pub fn is_structural_error(err: &str) -> bool {
        let is_image_data_compression_error = err.contains("Image Data compression");
        err.contains("invalid signature")
            || err.starts_with("Unknown PSD/PSB version:")
            || err.starts_with("PSD/PSB dimensions")
            || err.contains("channel count")
            || err.starts_with("Unsupported PSD/PSB bit depth")
            || err.starts_with("PSD/PSB header is too short")
            || err.starts_with("Not a PSD/PSB file")
            || err.contains("exceeds section boundary")
            || err.contains("color mode data")
            || err.contains("image resources")
            || err.contains("layer and mask")
            || (!is_image_data_compression_error
                && (err.contains("truncated") || err.contains("failed to fill whole buffer")))
    }
}

#[cfg(test)]
mod tests {
    use super::PsdSectionIndex;

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
        assert!(
            err.contains("invalid signature") || err.contains("Not a PSD"),
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

        assert!(PsdSectionIndex::is_structural_error(&err), "{err}");
    }

    #[test]
    fn structural_error_matches_header_failures() {
        assert!(PsdSectionIndex::is_structural_error(
            "Not a PSD/PSB file (invalid signature)"
        ));
        assert!(PsdSectionIndex::is_structural_error(
            "Unknown PSD/PSB version: 99"
        ));
        assert!(PsdSectionIndex::is_structural_error(
            "PSD/PSB dimensions must be non-zero"
        ));
        assert!(PsdSectionIndex::is_structural_error(
            "PSD/PSB channel count 0 is out of range (1..=56)"
        ));
        assert!(PsdSectionIndex::is_structural_error(
            "Unsupported PSD/PSB bit depth 12 (supported: 8, 16, 32)"
        ));
        assert!(PsdSectionIndex::is_structural_error(
            "PSD/PSB header is too short"
        ));
        assert!(!PsdSectionIndex::is_structural_error(
            "Invalid PSD/PSB Image Data compression: 9"
        ));
        assert!(!PsdSectionIndex::is_structural_error(
            "PSD/PSB Image Data compression truncated"
        ));
    }
}
