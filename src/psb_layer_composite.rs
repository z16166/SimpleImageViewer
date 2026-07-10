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

use std::io::Read;

const PSD_SIGNATURE: &[u8; 4] = b"8BPS";
const PSD_BLEND_SIGNATURE: &[u8; 4] = b"8BIM";
const PSB_BLOCK_SIGNATURE: &[u8; 4] = b"8B64";
const SECTION_DIVIDER_KEY: &[u8; 4] = b"lsct";
const MAX_LAYER_CHANNELS_PER_RECORD: usize = 128;
const PSD_VERSION: u16 = 1;
const PSB_VERSION: u16 = 2;
const LARGE_TAGGED_BLOCK_KEYS: [[u8; 4]; 13] = [
    *b"LMsk", *b"Lr16", *b"Lr32", *b"Layr", *b"Mt16", *b"Mt32", *b"Mtrn", *b"Alph", *b"FMsk",
    *b"lnk2", *b"FEid", *b"FXid", *b"PxSD",
];

#[derive(Debug, Clone)]
pub struct LayerChannel {
    pub id: i16,
    pub data_len: u32,
}

#[derive(Debug, Clone)]
pub struct LayerRecord {
    pub top: i32,
    pub left: i32,
    pub bottom: i32,
    pub right: i32,
    pub channels: Vec<LayerChannel>,
    pub blend: [u8; 4],
    pub opacity: u8,
    pub clipping: u8,
    pub flags: u8,
    pub mask_size: u32,
    pub is_section_divider: bool,
    pub section_type: Option<u32>,
}

impl LayerRecord {
    pub fn is_hidden(&self) -> bool {
        self.flags & 2 != 0
    }

    pub fn is_empty_bounds(&self) -> bool {
        self.left >= self.right || self.top >= self.bottom
    }

    pub fn width(&self) -> u32 {
        if self.is_empty_bounds() {
            0
        } else {
            self.right.saturating_sub(self.left) as u32
        }
    }

    pub fn height(&self) -> u32 {
        if self.is_empty_bounds() {
            0
        } else {
            self.bottom.saturating_sub(self.top) as u32
        }
    }
}

#[derive(Debug)]
pub struct LayerInfo<'a> {
    pub records: Vec<LayerRecord>,
    pub channel_data: &'a [u8],
    pub width: u32,
    pub height: u32,
    pub depth: u16,
    pub color_mode: u16,
    pub is_psb: bool,
}

pub fn parse_layer_records(bytes: &[u8]) -> Result<LayerInfo<'_>, String> {
    let file_len = bytes.len() as u64;
    let mut r = std::io::Cursor::new(bytes);

    let mut sig = [0u8; 4];
    r.read_exact(&mut sig)
        .map_err(|e| format!("Read error: {e}"))?;
    if &sig != PSD_SIGNATURE {
        return Err("Not a PSD/PSB file (invalid signature)".into());
    }

    let version = crate::psb_reader::read_u16(&mut r)?;
    if version != PSD_VERSION && version != PSB_VERSION {
        return Err(format!("Unknown PSD/PSB version: {version}"));
    }
    let is_psb = version == PSB_VERSION;

    crate::psb_reader::seek_forward(&mut r, 6)?;
    let _channels = crate::psb_reader::read_u16(&mut r)?;
    let height = crate::psb_reader::read_u32(&mut r)?;
    let width = crate::psb_reader::read_u32(&mut r)?;
    let depth = crate::psb_reader::read_u16(&mut r)?;
    let color_mode = crate::psb_reader::read_u16(&mut r)?;

    let cm_len = crate::psb_reader::read_u32(&mut r)? as u64;
    skip_section(&mut r, cm_len, file_len, "color mode data")?;

    let ir_len = crate::psb_reader::read_u32(&mut r)? as u64;
    skip_section(&mut r, ir_len, file_len, "image resources")?;

    let lm_len = if is_psb {
        crate::psb_reader::read_u64(&mut r)?
    } else {
        crate::psb_reader::read_u32(&mut r)? as u64
    };
    let lm_start = r.position();
    let lm_end = checked_end(lm_start, lm_len, file_len, "layer and mask info")?;
    if lm_len == 0 {
        let empty = cursor_slice(bytes, lm_start, lm_start)?;
        return Ok(LayerInfo {
            records: Vec::new(),
            channel_data: empty,
            width,
            height,
            depth,
            color_mode,
            is_psb,
        });
    }

    let layer_info_len = if is_psb {
        crate::psb_reader::read_u64(&mut r)?
    } else {
        crate::psb_reader::read_u32(&mut r)? as u64
    };
    let layer_info_start = r.position();
    let layer_info_end = checked_end(
        layer_info_start,
        layer_info_len,
        lm_end,
        "layer info section",
    )?;
    if layer_info_len == 0 {
        let empty = cursor_slice(bytes, layer_info_start, layer_info_start)?;
        return Ok(LayerInfo {
            records: Vec::new(),
            channel_data: empty,
            width,
            height,
            depth,
            color_mode,
            is_psb,
        });
    }

    let layer_count = read_i16(&mut r)?.unsigned_abs() as usize;
    let mut records = Vec::with_capacity(layer_count);
    for _ in 0..layer_count {
        records.push(parse_layer_record(&mut r, layer_info_end, is_psb)?);
    }

    let channel_data_start = r.position();
    let channel_data = cursor_slice(bytes, channel_data_start, layer_info_end)?;

    Ok(LayerInfo {
        records,
        channel_data,
        width,
        height,
        depth,
        color_mode,
        is_psb,
    })
}

fn parse_layer_record(
    r: &mut std::io::Cursor<&[u8]>,
    layer_info_end: u64,
    is_psb: bool,
) -> Result<LayerRecord, String> {
    let top = read_i32(r)?;
    let left = read_i32(r)?;
    let bottom = read_i32(r)?;
    let right = read_i32(r)?;

    let channel_count = crate::psb_reader::read_u16(r)? as usize;
    if channel_count > MAX_LAYER_CHANNELS_PER_RECORD {
        return Err(format!(
            "PSD/PSB layer channel count {channel_count} exceeds {MAX_LAYER_CHANNELS_PER_RECORD}"
        ));
    }
    let mut channels = Vec::with_capacity(channel_count);
    for _ in 0..channel_count {
        let id = read_i16(r)?;
        let data_len = if is_psb {
            let len = crate::psb_reader::read_u64(r)?;
            u32::try_from(len).map_err(|_| {
                format!("PSD/PSB layer channel length {len} exceeds supported range")
            })?
        } else {
            crate::psb_reader::read_u32(r)?
        };
        channels.push(LayerChannel { id, data_len });
    }

    let mut blend_signature = [0u8; 4];
    r.read_exact(&mut blend_signature)
        .map_err(|e| format!("Read layer blend signature: {e}"))?;
    if &blend_signature != PSD_BLEND_SIGNATURE {
        return Err(format!(
            "Invalid PSD/PSB layer blend signature: {blend_signature:?}"
        ));
    }

    let mut blend = [0u8; 4];
    r.read_exact(&mut blend)
        .map_err(|e| format!("Read layer blend mode: {e}"))?;

    let mut attrs = [0u8; 4];
    r.read_exact(&mut attrs)
        .map_err(|e| format!("Read layer attributes: {e}"))?;
    let opacity = attrs[0];
    let clipping = attrs[1];
    let flags = attrs[2];

    let extra_size = crate::psb_reader::read_u32(r)? as u64;
    let extra_start = r.position();
    let extra_end = checked_end(extra_start, extra_size, layer_info_end, "layer extra data")?;
    let (mask_size, is_section_divider, section_type) = parse_layer_extra(r, extra_end, is_psb)?;

    r.set_position(extra_end);

    Ok(LayerRecord {
        top,
        left,
        bottom,
        right,
        channels,
        blend,
        opacity,
        clipping,
        flags,
        mask_size,
        is_section_divider,
        section_type,
    })
}

fn parse_layer_extra(
    r: &mut std::io::Cursor<&[u8]>,
    extra_end: u64,
    is_psb: bool,
) -> Result<(u32, bool, Option<u32>), String> {
    if r.position() >= extra_end {
        return Ok((0, false, None));
    }

    let mask_size = read_extra_u32(r, extra_end, "layer mask data length")?;
    skip_extra_bytes(r, mask_size as u64, extra_end, "layer mask data")?;

    let blending_ranges_len = read_extra_u32(r, extra_end, "layer blending ranges length")?;
    skip_extra_bytes(
        r,
        blending_ranges_len as u64,
        extra_end,
        "layer blending ranges",
    )?;

    skip_pascal_name(r, extra_end)?;
    let (is_section_divider, section_type) = scan_extra_tagged_blocks(r, extra_end, is_psb)?;
    Ok((mask_size, is_section_divider, section_type))
}

fn scan_extra_tagged_blocks(
    r: &mut std::io::Cursor<&[u8]>,
    extra_end: u64,
    is_psb: bool,
) -> Result<(bool, Option<u32>), String> {
    let mut is_section_divider = false;
    let mut section_type = None;

    while r.position().saturating_add(TAGGED_BLOCK_MIN_HEADER_LEN) <= extra_end {
        let block_start = r.position();
        let mut signature = [0u8; 4];
        r.read_exact(&mut signature)
            .map_err(|e| format!("Read layer tagged block signature: {e}"))?;
        if &signature != PSD_BLEND_SIGNATURE && &signature != PSB_BLOCK_SIGNATURE {
            r.set_position(block_start.saturating_add(1));
            continue;
        }

        let mut key = [0u8; 4];
        r.read_exact(&mut key)
            .map_err(|e| format!("Read layer tagged block key: {e}"))?;
        let uses_u64_len = tagged_block_uses_u64_len(&signature, &key, is_psb);
        if uses_u64_len
            && checked_end(r.position(), 8, extra_end, "layer tagged block length").is_err()
        {
            r.set_position(block_start.saturating_add(1));
            continue;
        }
        let data_len = if uses_u64_len {
            crate::psb_reader::read_u64(r)?
        } else {
            crate::psb_reader::read_u32(r)? as u64
        };
        let data_start = r.position();
        let data_end = match checked_end(data_start, data_len, extra_end, "layer tagged block") {
            Ok(end) => end,
            Err(_) => {
                r.set_position(block_start.saturating_add(1));
                continue;
            }
        };

        if &key == SECTION_DIVIDER_KEY && data_len >= 4 {
            let data_start = data_start as usize;
            let bytes = r.get_ref();
            section_type = Some(u32::from_be_bytes([
                bytes[data_start],
                bytes[data_start + 1],
                bytes[data_start + 2],
                bytes[data_start + 3],
            ]));
            is_section_divider = true;
        }

        let padded_end = data_end.saturating_add(data_len % 2);
        r.set_position(padded_end.min(extra_end));
    }

    Ok((is_section_divider, section_type))
}

fn skip_pascal_name(r: &mut std::io::Cursor<&[u8]>, extra_end: u64) -> Result<(), String> {
    if r.position() >= extra_end {
        return Ok(());
    }

    let mut len = [0u8; 1];
    r.read_exact(&mut len)
        .map_err(|e| format!("Read layer name length: {e}"))?;
    let raw_len = 1u64 + len[0] as u64;
    let padded_len = raw_len.next_multiple_of(4);
    skip_extra_bytes(
        r,
        padded_len.saturating_sub(1),
        extra_end,
        "layer name data",
    )
}

fn read_extra_u32(
    r: &mut std::io::Cursor<&[u8]>,
    extra_end: u64,
    label: &str,
) -> Result<u32, String> {
    checked_end(r.position(), 4, extra_end, label)?;
    crate::psb_reader::read_u32(r)
}

fn skip_extra_bytes(
    r: &mut std::io::Cursor<&[u8]>,
    len: u64,
    extra_end: u64,
    label: &str,
) -> Result<(), String> {
    checked_end(r.position(), len, extra_end, label)?;
    crate::psb_reader::seek_forward(r, len)
}

fn skip_section(
    r: &mut std::io::Cursor<&[u8]>,
    len: u64,
    file_len: u64,
    label: &str,
) -> Result<(), String> {
    checked_end(r.position(), len, file_len, label)?;
    crate::psb_reader::seek_forward(r, len)
}

fn checked_end(start: u64, len: u64, limit: u64, label: &str) -> Result<u64, String> {
    let end = start
        .checked_add(len)
        .ok_or_else(|| format!("PSD/PSB {label} length overflow"))?;
    if end > limit {
        return Err(format!(
            "PSD/PSB {label} exceeds section boundary ({end} > {limit})"
        ));
    }
    Ok(end)
}

fn cursor_slice(bytes: &[u8], start: u64, end: u64) -> Result<&[u8], String> {
    let start = usize::try_from(start).map_err(|_| "PSD/PSB slice start overflow".to_string())?;
    let end = usize::try_from(end).map_err(|_| "PSD/PSB slice end overflow".to_string())?;
    bytes
        .get(start..end)
        .ok_or_else(|| "PSD/PSB slice is out of bounds".to_string())
}

const TAGGED_BLOCK_MIN_HEADER_LEN: u64 = 12;

fn tagged_block_uses_u64_len(signature: &[u8; 4], key: &[u8; 4], is_psb: bool) -> bool {
    signature == PSB_BLOCK_SIGNATURE
        || (is_psb
            && LARGE_TAGGED_BLOCK_KEYS
                .iter()
                .any(|large_key| large_key == key))
}

fn read_i16(r: &mut impl Read) -> Result<i16, String> {
    Ok(crate::psb_reader::read_u16(r)? as i16)
}

fn read_i32(r: &mut impl Read) -> Result<i32, String> {
    Ok(crate::psb_reader::read_u32(r)? as i32)
}

#[cfg(test)]
mod tests {
    use super::{parse_layer_records, scan_extra_tagged_blocks};
    use std::path::Path;

    #[test]
    fn psb_8bim_lsct_uses_u32_length() {
        let mut block = Vec::new();
        block.extend_from_slice(b"8BIM");
        block.extend_from_slice(b"lsct");
        block.extend_from_slice(&4u32.to_be_bytes());
        block.extend_from_slice(&2u32.to_be_bytes());
        let mut cursor = std::io::Cursor::new(block.as_slice());

        let (is_section_divider, section_type) =
            scan_extra_tagged_blocks(&mut cursor, block.len() as u64, true).unwrap();

        assert!(is_section_divider);
        assert_eq!(section_type, Some(2));
    }

    #[test]
    fn parse_layer_records_11_psd_corpus() {
        let path = Path::new(r"F:\BaiduNetdiskDownload\素材库\45套 psd企业画册模板\11\11.psd");
        if !path.is_file() {
            return;
        }
        let bytes = std::fs::read(path).unwrap();
        let layers = parse_layer_records(&bytes).unwrap();
        assert!(layers.records.len() >= 300);
        assert!(
            layers
                .records
                .iter()
                .any(|l| !l.is_empty_bounds() && !l.is_hidden())
        );
        assert!(layers.records.iter().any(|l| l.is_section_divider));
        assert!(!layers.channel_data.is_empty());
    }
}
