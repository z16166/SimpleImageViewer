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

//! CIPA DC-007 Multi-Picture Format (MPF) helpers for JPEG gain-map location.
//!
//! Ultra HDR and Adobe HDR Gain Map JPEGs may embed the gain-map image via MPF APP2
//! metadata instead of (or in addition to) GContainer `Item:Length` trailers.
//! Offsets in MP entries are relative to the MPF TIFF header (the byte after `MPF\0`).

const MPF_SIGNATURE: &[u8] = b"MPF\x00";
const MPF_VERSION: &[u8] = b"0100";

const MPF_TAG_VERSION: u16 = 0xB000;
const MPF_TAG_NUM_IMAGES: u16 = 0xB001;
const MPF_TAG_MP_ENTRY: u16 = 0xB002;

const MP_ENTRY_SIZE: usize = 16;

/// Baseline MP primary image (`kMPEntryAttributeTypePrimary` in libultrahdr).
const MP_IMAGE_TYPE_PRIMARY: u32 = 0x0003_0000;
/// Gain-map image type registered in CIPA DC-007 / ExifTool MPF tag tables.
const MP_IMAGE_TYPE_GAIN_MAP: u32 = 0x0005_0000;

const MP_FORMAT_JPEG: u32 = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MpEntry {
    image_type: u32,
    format: u32,
    size: u32,
    data_offset: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MpfGainMapSlice {
    start: u32,
    length: u32,
}

pub(crate) fn mpf_app2_payload_has_gain_map_image(payload: &[u8]) -> bool {
    locate_mpf_gain_map_in_app2_payload(payload).is_some()
}

pub(crate) fn extract_mpf_gain_map_jpeg_from_bytes(
    bytes: &[u8],
    mpf_app2_payload: &[u8],
) -> Result<Vec<u8>, String> {
    let slice = locate_mpf_gain_map_in_app2_payload(mpf_app2_payload)
        .ok_or_else(|| "MPF metadata does not describe a gain-map image".to_string())?;

    let tiff_base = bytes
        .windows(MPF_SIGNATURE.len())
        .position(|window| window == MPF_SIGNATURE)
        .map(|mpf| mpf + MPF_SIGNATURE.len())
        .ok_or_else(|| "MPF signature missing from JPEG stream".to_string())?;

    let start = tiff_base
        .checked_add(slice.start as usize)
        .ok_or_else(|| "MPF gain-map offset overflow".to_string())?;
    let end = start
        .checked_add(slice.length as usize)
        .ok_or_else(|| "MPF gain-map length overflow".to_string())?;

    if end > bytes.len() {
        return Err("MPF gain-map slice exceeds JPEG file size".to_string());
    }

    let mut gain_map = bytes[start..end].to_vec();
    if !gain_map.starts_with(&[0xFF, 0xD8]) {
        // libultrahdr stores the secondary image without the SOI marker.
        let mut with_soi = Vec::with_capacity(gain_map.len() + 2);
        with_soi.extend_from_slice(&[0xFF, 0xD8]);
        with_soi.extend_from_slice(&gain_map);
        gain_map = with_soi;
    }

    if !gain_map.starts_with(&[0xFF, 0xD8]) || !gain_map.ends_with(&[0xFF, 0xD9]) {
        return Err("MPF gain-map payload is not a JPEG stream".to_string());
    }

    Ok(gain_map)
}

fn locate_mpf_gain_map_in_app2_payload(payload: &[u8]) -> Option<MpfGainMapSlice> {
    let mpf = parse_mpf(payload)?;
    mpf.entries
        .iter()
        .find(|entry| entry.image_type == MP_IMAGE_TYPE_GAIN_MAP)
        .or_else(|| mpf.entries.iter().find(|entry| is_gain_map_entry(**entry)))
        .map(|entry| MpfGainMapSlice {
            start: entry.data_offset,
            length: entry.size,
        })
}

fn is_gain_map_entry(entry: MpEntry) -> bool {
    if entry.format != MP_FORMAT_JPEG || entry.size == 0 {
        return false;
    }
    match entry.image_type {
        MP_IMAGE_TYPE_GAIN_MAP => true,
        MP_IMAGE_TYPE_PRIMARY => false,
        0x0001_0001..=0x0001_0005 | 0x0002_0001..=0x0002_0003 | 0x0004_0000 => false,
        // libultrahdr stores the secondary gain-map image with a zero type field.
        0 => true,
        _ => false,
    }
}

#[derive(Debug)]
struct ParsedMpf {
    entries: Vec<MpEntry>,
}

fn parse_mpf(payload: &[u8]) -> Option<ParsedMpf> {
    let tiff = payload.strip_prefix(MPF_SIGNATURE)?;
    let (big_endian, ifd_offset) = parse_tiff_header(tiff)?;
    let (num_images, mp_entry_bytes) = parse_mpf_index_ifd(tiff, ifd_offset, big_endian)?;
    if num_images < 2 {
        return None;
    }

    let expected_len = num_images as usize * MP_ENTRY_SIZE;
    if mp_entry_bytes.len() < expected_len {
        return None;
    }

    let mut entries = Vec::with_capacity(num_images as usize);
    for index in 0..num_images as usize {
        let offset = index * MP_ENTRY_SIZE;
        entries.push(parse_mp_entry(
            &mp_entry_bytes[offset..offset + MP_ENTRY_SIZE],
            big_endian,
        )?);
    }

    Some(ParsedMpf { entries })
}

fn parse_tiff_header(tiff: &[u8]) -> Option<(bool, u32)> {
    if tiff.len() < 8 {
        return None;
    }
    let big_endian = match &tiff[0..2] {
        b"MM" => true,
        b"II" => false,
        _ => return None,
    };
    if read_u16(&tiff[2..], big_endian)? != 42 {
        return None;
    }
    Some((big_endian, read_u32(&tiff[4..], big_endian)?))
}

fn parse_mpf_index_ifd(
    tiff: &[u8],
    ifd_offset: u32,
    big_endian: bool,
) -> Option<(u32, Vec<u8>)> {
    let ifd_start = ifd_offset as usize;
    if ifd_start + 2 > tiff.len() {
        return None;
    }
    let tag_count = read_u16(&tiff[ifd_start..], big_endian)? as usize;
    let mut num_images = None;
    let mut mp_entry_bytes = None;
    let mut saw_version = false;

    for index in 0..tag_count {
        let entry_start = ifd_start + 2 + index * 12;
        if entry_start + 12 > tiff.len() {
            return None;
        }
        let tag = read_u16(&tiff[entry_start..], big_endian)?;
        let value_type = read_u16(&tiff[entry_start + 2..], big_endian)?;
        let count = read_u32(&tiff[entry_start + 4..], big_endian)?;
        let value = read_u32(&tiff[entry_start + 8..], big_endian)?;

        match tag {
            MPF_TAG_VERSION if value_type == 7 && count == 4 => {
                let version = read_ifd_value_bytes(tiff, value_type, count, value, big_endian)?;
                saw_version = version.as_slice() == MPF_VERSION;
            }
            MPF_TAG_NUM_IMAGES if value_type == 4 && count == 1 => {
                num_images = Some(value);
            }
            MPF_TAG_MP_ENTRY if value_type == 7 => {
                mp_entry_bytes = Some(read_ifd_value_bytes(
                    tiff, value_type, count, value, big_endian,
                )?);
            }
            _ => {}
        }
    }

    if !saw_version {
        return None;
    }
    Some((num_images?, mp_entry_bytes?))
}

fn parse_mp_entry(bytes: &[u8], big_endian: bool) -> Option<MpEntry> {
    if bytes.len() < MP_ENTRY_SIZE {
        return None;
    }
    let attribute = read_u32(bytes, big_endian)?;
    Some(MpEntry {
        format: (attribute >> 24) & 0x7,
        image_type: attribute & 0x00FF_FFFF,
        size: read_u32(&bytes[4..], big_endian)?,
        data_offset: read_u32(&bytes[8..], big_endian)?,
    })
}

fn read_ifd_value_bytes(
    tiff: &[u8],
    value_type: u16,
    count: u32,
    value: u32,
    big_endian: bool,
) -> Option<Vec<u8>> {
    let type_size = match value_type {
        1 | 2 | 6 | 7 => 1,
        3 | 8 => 2,
        4 | 9 | 11 => 4,
        5 | 10 | 12 => 8,
        _ => return None,
    };
    let total = count as usize * type_size;
    if total <= 4 {
        let raw = value.to_be_bytes();
        let len = total.min(4);
        let start = if big_endian { 0 } else { 4 - len };
        return Some(raw[start..start + len].to_vec());
    }
    let offset = value as usize;
    if offset + total > tiff.len() {
        return None;
    }
    Some(tiff[offset..offset + total].to_vec())
}

fn read_u16(bytes: &[u8], big_endian: bool) -> Option<u16> {
    let chunk: [u8; 2] = bytes.get(0..2)?.try_into().ok()?;
    Some(if big_endian {
        u16::from_be_bytes(chunk)
    } else {
        u16::from_le_bytes(chunk)
    })
}

fn read_u32(bytes: &[u8], big_endian: bool) -> Option<u32> {
    let chunk: [u8; 4] = bytes.get(0..4)?.try_into().ok()?;
    Some(if big_endian {
        u32::from_be_bytes(chunk)
    } else {
        u32::from_le_bytes(chunk)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn gain_map_samples_root() -> Option<PathBuf> {
        std::env::var_os("SIV_GAIN_MAP_SAMPLES_DIR")
            .map(PathBuf::from)
            .or_else(|| Some(PathBuf::from(r"F:\HDR\GainMap")))
            .filter(|path| path.is_dir())
    }

    fn first_mpf_app2_payload(bytes: &[u8]) -> Option<Vec<u8>> {
        bytes
            .windows(MPF_SIGNATURE.len())
            .position(|window| window == MPF_SIGNATURE)
            .map(|start| bytes[start..].to_vec())
    }

    #[test]
    fn mpf_detects_gain_map_entry_in_camera_raw_exports() {
        let Some(root) = gain_map_samples_root() else {
            eprintln!("skipping MPF corpus test; set SIV_GAIN_MAP_SAMPLES_DIR");
            return;
        };
        let path = root.join("DSC2306-Edit_1000x667_100_3x2__benz8GainMap.jpg");
        if !path.is_file() {
            eprintln!("skipping MPF corpus test; sample missing");
            return;
        }

        let bytes = std::fs::read(&path).expect("read sample");
        let payload = first_mpf_app2_payload(&bytes).expect("locate MPF segment");

        assert!(mpf_app2_payload_has_gain_map_image(&payload));

        let gain_map = extract_mpf_gain_map_jpeg_from_bytes(&bytes, &payload).expect("extract MPF gain map");
        assert!(gain_map.starts_with(&[0xFF, 0xD8]));
        assert!(gain_map.ends_with(&[0xFF, 0xD9]));
        assert!(gain_map
            .windows(b"hdrgm:GainMapMax".len())
            .any(|window| window == b"hdrgm:GainMapMax"));
    }

    #[test]
    fn mpf_primary_type_constant_matches_libultrahdr() {
        assert_eq!(MP_IMAGE_TYPE_PRIMARY, 0x030000);
    }
}
