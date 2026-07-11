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

//! Probe PSD/PSB embedded ICC (IR 1039) for explicit HDR markings.
//!
//! Used by the 16-bit content gate: only PQ/HLG `cicp`, high `lumi`, or
//! description substrings pull the HDR path. No pixel scanning.

use crate::hdr::cicp::{H273_TRANSFER_ARIB_STD_B67_FOR_HLG, H273_TRANSFER_SMPTE_ST2084_FOR_PQ};
use crate::hdr::types::HdrTransferFunction;

/// Absolute white (cd/m^2) above this marks HDR via the `lumi` tag.
pub const LUMI_HDR_NITS_THRESHOLD: f32 = 100.0;

/// Result of probing an ICC profile for HDR intent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IccHdrProbe {
    pub marks_hdr: bool,
    pub transfer: HdrTransferFunction,
    pub peak_nits: Option<f32>,
}

impl Default for IccHdrProbe {
    fn default() -> Self {
        Self {
            marks_hdr: false,
            transfer: HdrTransferFunction::Unknown,
            peak_nits: None,
        }
    }
}

/// Whether this ICC explicitly marks an HDR encoding (16-bit content gate).
pub fn icc_profile_marks_hdr(icc: &[u8]) -> bool {
    probe_icc_hdr(icc).marks_hdr
}

/// Full probe: transfer hint + optional peak nits from `lumi`.
pub fn probe_icc_hdr(icc: &[u8]) -> IccHdrProbe {
    let Some(tags) = parse_icc_tag_table(icc) else {
        return IccHdrProbe::default();
    };

    // 1) cicp transfer 16/18
    if let Some(cicp) = tags.iter().find(|t| t.sig == *b"cicp")
        && let Some((tf, marks)) = parse_cicp_tag(icc, cicp)
        && marks
    {
        let peak = tags
            .iter()
            .find(|t| t.sig == *b"lumi")
            .and_then(|t| parse_lumi_y_nits(icc, t));
        return IccHdrProbe {
            marks_hdr: true,
            transfer: tf,
            peak_nits: peak,
        };
    }

    // 2) lumi >> 100 nits
    if let Some(lumi) = tags.iter().find(|t| t.sig == *b"lumi")
        && let Some(nits) = parse_lumi_y_nits(icc, lumi)
        && nits > LUMI_HDR_NITS_THRESHOLD
    {
        return IccHdrProbe {
            marks_hdr: true,
            transfer: HdrTransferFunction::Linear,
            peak_nits: Some(nits),
        };
    }

    // 3) desc / mluc substring fallback
    for tag in &tags {
        if (tag.sig == *b"desc" || tag.sig == *b"dmnd" || tag.sig == *b"dscm")
            && let Some(text) = read_icc_description(icc, tag)
            && description_marks_hdr(&text)
        {
            let tf = transfer_from_description(&text);
            let peak = tags
                .iter()
                .find(|t| t.sig == *b"lumi")
                .and_then(|t| parse_lumi_y_nits(icc, t));
            return IccHdrProbe {
                marks_hdr: true,
                transfer: tf,
                peak_nits: peak,
            };
        }
    }

    IccHdrProbe::default()
}

/// Content gate: should P1/P2 attempt the HDR path (ignoring environment)?
pub fn psd_content_wants_hdr(depth: u16, embedded_icc: Option<&[u8]>) -> bool {
    match depth {
        32 => true,
        16 => embedded_icc.map(icc_profile_marks_hdr).unwrap_or(false),
        _ => false,
    }
}

/// Environment gate: same capacity boundary as other HDR formats.
pub fn psd_env_wants_hdr(hdr_target_capacity: f32) -> bool {
    hdr_target_capacity > 1.0 + crate::loader::HDR_CAPACITY_MATCH_EPSILON
}

struct IccTag {
    sig: [u8; 4],
    offset: u32,
    size: u32,
}

fn parse_icc_tag_table(icc: &[u8]) -> Option<Vec<IccTag>> {
    if icc.len() < 132 {
        return None;
    }
    // ICC magic "acsp" at offset 36.
    if &icc[36..40] != b"acsp" {
        return None;
    }
    let tag_count = u32::from_be_bytes(icc[128..132].try_into().ok()?);
    let table_end = 132u64
        .checked_add(u64::from(tag_count).checked_mul(12)?)?
        .min(icc.len() as u64) as usize;
    if table_end < 132 {
        return None;
    }
    let mut tags = Vec::with_capacity(tag_count as usize);
    let mut pos = 132usize;
    for _ in 0..tag_count {
        if pos + 12 > icc.len() {
            break;
        }
        let mut sig = [0u8; 4];
        sig.copy_from_slice(&icc[pos..pos + 4]);
        let offset = u32::from_be_bytes(icc[pos + 4..pos + 8].try_into().ok()?);
        let size = u32::from_be_bytes(icc[pos + 8..pos + 12].try_into().ok()?);
        tags.push(IccTag { sig, offset, size });
        pos += 12;
    }
    Some(tags)
}

fn tag_bytes<'a>(icc: &'a [u8], tag: &IccTag) -> Option<&'a [u8]> {
    let start = tag.offset as usize;
    let end = start.checked_add(tag.size as usize)?;
    icc.get(start..end)
}

fn parse_cicp_tag(icc: &[u8], tag: &IccTag) -> Option<(HdrTransferFunction, bool)> {
    let data = tag_bytes(icc, tag)?;
    // cicp type: 'cicp' (4) + reserved (4) + primaries, transfer, matrix, full_range
    if data.len() < 12 || &data[0..4] != b"cicp" {
        return None;
    }
    let transfer = data[9] as u16;
    let tf = match transfer {
        t if t == H273_TRANSFER_SMPTE_ST2084_FOR_PQ => HdrTransferFunction::Pq,
        t if t == H273_TRANSFER_ARIB_STD_B67_FOR_HLG => HdrTransferFunction::Hlg,
        _ => HdrTransferFunction::Unknown,
    };
    let marks = matches!(tf, HdrTransferFunction::Pq | HdrTransferFunction::Hlg);
    Some((tf, marks))
}

fn parse_lumi_y_nits(icc: &[u8], tag: &IccTag) -> Option<f32> {
    let data = tag_bytes(icc, tag)?;
    // XYZType: 'XYZ ' (4) + reserved (4) + X,Y,Z as s15Fixed16Number
    if data.len() < 20 || &data[0..4] != b"XYZ " {
        return None;
    }
    let y_fixed = i32::from_be_bytes(data[12..16].try_into().ok()?);
    Some(y_fixed as f32 / 65536.0)
}

fn read_icc_description(icc: &[u8], tag: &IccTag) -> Option<String> {
    let data = tag_bytes(icc, tag)?;
    if data.len() < 8 {
        return None;
    }
    let type_sig = &data[0..4];
    if type_sig == b"desc" {
        // textDescriptionType: ascii count at 8, then ascii
        if data.len() < 12 {
            return None;
        }
        let count = u32::from_be_bytes(data[8..12].try_into().ok()?) as usize;
        let start = 12usize;
        let end = start.saturating_add(count).min(data.len());
        if start >= end {
            return None;
        }
        let raw = &data[start..end];
        // Trim trailing NUL.
        let end_trim = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        return Some(String::from_utf8_lossy(&raw[..end_trim]).into_owned());
    }
    if type_sig == b"mluc" {
        // multiLocalizedUnicodeType: record count at 8, then records
        if data.len() < 16 {
            return None;
        }
        let count = u32::from_be_bytes(data[8..12].try_into().ok()?) as usize;
        let mut pos = 16usize;
        for _ in 0..count {
            if pos + 12 > data.len() {
                break;
            }
            let _lang = &data[pos..pos + 4];
            let len = u32::from_be_bytes(data[pos + 4..pos + 8].try_into().ok()?) as usize;
            let off = u32::from_be_bytes(data[pos + 8..pos + 12].try_into().ok()?) as usize;
            pos += 12;
            let s = off;
            let e = s.saturating_add(len).min(data.len());
            if s < e && (e - s) >= 2 {
                // UTF-16BE
                let mut chars = Vec::new();
                let mut i = s;
                while i + 1 < e {
                    let cu = u16::from_be_bytes([data[i], data[i + 1]]);
                    if cu == 0 {
                        break;
                    }
                    if let Some(c) = char::from_u32(cu as u32) {
                        chars.push(c);
                    }
                    i += 2;
                }
                if !chars.is_empty() {
                    return Some(chars.into_iter().collect());
                }
            }
        }
    }
    None
}

fn description_marks_hdr(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    const NEEDLES: &[&str] = &[
        "pq",
        "hlg",
        "hdr10",
        "rec. 2100",
        "rec.2100",
        "rec. 2020 pq",
        "rec.2020 pq",
        "display p3 hl",
        "high luminance",
        "smpte st 2084",
        "st2084",
    ];
    NEEDLES.iter().any(|n| lower.contains(n))
}

fn transfer_from_description(text: &str) -> HdrTransferFunction {
    let lower = text.to_ascii_lowercase();
    if lower.contains("hlg") {
        HdrTransferFunction::Hlg
    } else if lower.contains("pq")
        || lower.contains("hdr10")
        || lower.contains("2084")
        || lower.contains("2100")
    {
        HdrTransferFunction::Pq
    } else {
        HdrTransferFunction::Linear
    }
}

/// Build a minimal synthetic ICC with a `cicp` tag (for tests / fixtures).
#[cfg(test)]
pub fn synthetic_icc_with_cicp_transfer(transfer: u8) -> Vec<u8> {
    // Minimal profile: 128-byte header + 1 tag + cicp data.
    let mut icc = vec![0u8; 128];
    icc[36..40].copy_from_slice(b"acsp");
    // tag count = 1
    let mut out = icc;
    out.extend_from_slice(&1u32.to_be_bytes());
    // tag: cicp at offset 128+4+12=144, size 12
    out.extend_from_slice(b"cicp");
    out.extend_from_slice(&144u32.to_be_bytes());
    out.extend_from_slice(&12u32.to_be_bytes());
    // pad to offset 144
    while out.len() < 144 {
        out.push(0);
    }
    out.extend_from_slice(b"cicp");
    out.extend_from_slice(&[0, 0, 0, 0]); // reserved
    out.push(1); // primaries BT.709
    out.push(transfer);
    out.push(0); // matrix
    out.push(1); // full range
    // profile size field
    let size = out.len() as u32;
    out[0..4].copy_from_slice(&size.to_be_bytes());
    out
}

#[cfg(test)]
pub fn synthetic_icc_with_lumi_nits(nits: f32) -> Vec<u8> {
    let mut out = vec![0u8; 128];
    out[36..40].copy_from_slice(b"acsp");
    out.extend_from_slice(&1u32.to_be_bytes());
    out.extend_from_slice(b"lumi");
    out.extend_from_slice(&144u32.to_be_bytes());
    out.extend_from_slice(&20u32.to_be_bytes());
    while out.len() < 144 {
        out.push(0);
    }
    out.extend_from_slice(b"XYZ ");
    out.extend_from_slice(&[0, 0, 0, 0]);
    let y = (nits * 65536.0) as i32;
    out.extend_from_slice(&0i32.to_be_bytes()); // X
    out.extend_from_slice(&y.to_be_bytes()); // Y
    out.extend_from_slice(&0i32.to_be_bytes()); // Z
    let size = out.len() as u32;
    out[0..4].copy_from_slice(&size.to_be_bytes());
    out
}

#[cfg(test)]
pub fn synthetic_icc_with_desc(desc: &str) -> Vec<u8> {
    let ascii = desc.as_bytes();
    let desc_size = 12 + ascii.len() + 1;
    let data_offset = 144u32;
    let mut out = vec![0u8; 128];
    out[36..40].copy_from_slice(b"acsp");
    out.extend_from_slice(&1u32.to_be_bytes());
    out.extend_from_slice(b"desc");
    out.extend_from_slice(&data_offset.to_be_bytes());
    out.extend_from_slice(&(desc_size as u32).to_be_bytes());
    while out.len() < data_offset as usize {
        out.push(0);
    }
    out.extend_from_slice(b"desc");
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&(ascii.len() as u32 + 1).to_be_bytes());
    out.extend_from_slice(ascii);
    out.push(0);
    let size = out.len() as u32;
    out[0..4].copy_from_slice(&size.to_be_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hdr::cicp::{
        H273_TRANSFER_ARIB_STD_B67_FOR_HLG, H273_TRANSFER_IEC61966_2_1_SRGB,
        H273_TRANSFER_SMPTE_ST2084_FOR_PQ,
    };

    #[test]
    fn cicp_pq_marks_hdr() {
        let icc = synthetic_icc_with_cicp_transfer(H273_TRANSFER_SMPTE_ST2084_FOR_PQ as u8);
        let p = probe_icc_hdr(&icc);
        assert!(p.marks_hdr);
        assert_eq!(p.transfer, HdrTransferFunction::Pq);
    }

    #[test]
    fn cicp_hlg_marks_hdr() {
        let icc = synthetic_icc_with_cicp_transfer(H273_TRANSFER_ARIB_STD_B67_FOR_HLG as u8);
        let p = probe_icc_hdr(&icc);
        assert!(p.marks_hdr);
        assert_eq!(p.transfer, HdrTransferFunction::Hlg);
    }

    #[test]
    fn cicp_srgb_does_not_mark_hdr() {
        let icc = synthetic_icc_with_cicp_transfer(H273_TRANSFER_IEC61966_2_1_SRGB as u8);
        assert!(!icc_profile_marks_hdr(&icc));
    }

    #[test]
    fn lumi_high_marks_hdr() {
        let icc = synthetic_icc_with_lumi_nits(1000.0);
        let p = probe_icc_hdr(&icc);
        assert!(p.marks_hdr);
        assert!(p.peak_nits.unwrap() > 100.0);
    }

    #[test]
    fn lumi_sdr_does_not_mark_hdr() {
        let icc = synthetic_icc_with_lumi_nits(80.0);
        assert!(!icc_profile_marks_hdr(&icc));
    }

    #[test]
    fn desc_hdr10_marks_hdr() {
        let icc = synthetic_icc_with_desc("Rec. 2100 PQ / HDR10");
        let p = probe_icc_hdr(&icc);
        assert!(p.marks_hdr);
        assert_eq!(p.transfer, HdrTransferFunction::Pq);
    }

    #[test]
    fn content_gate_depth() {
        assert!(!psd_content_wants_hdr(8, None));
        assert!(!psd_content_wants_hdr(16, None));
        assert!(psd_content_wants_hdr(32, None));
        let pq = synthetic_icc_with_cicp_transfer(H273_TRANSFER_SMPTE_ST2084_FOR_PQ as u8);
        assert!(psd_content_wants_hdr(16, Some(&pq)));
    }

    #[test]
    fn env_gate_capacity() {
        assert!(!psd_env_wants_hdr(1.0));
        assert!(psd_env_wants_hdr(2.0));
    }
}
