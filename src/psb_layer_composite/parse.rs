//! Layer record parsing — reads the Layer & Mask Information section of a
//! PSD/PSB file and produces [`LayerRecord`] / [`LayerInfo`] values.

use std::io::Read;

use crate::psb_layer_composite::{
    LayerChannel, LayerInfo, LayerMaskInfo, LayerRecord, VMSK_RECORD_LEN, VectorMaskData,
    VectorMaskFlags,
};
use crate::psb_reader::PSD_COLOR_MODE_CMYK;
use crate::psb_reader_util::SharedSlice;
use std::sync::Arc;

const PSD_BLEND_SIGNATURE: &[u8; 4] = b"8BIM";
const PSB_BLOCK_SIGNATURE: &[u8; 4] = b"8B64";
const SECTION_DIVIDER_KEY: &[u8; 4] = b"lsct";
pub(crate) const MAX_LAYER_CHANNELS_PER_RECORD: usize = 128;

/// Cap on layer records in the Layer Info section.
///
/// The on-disk count is an `i16` absolute value (at most 65535). Without a
/// tighter cap, a malicious file can force `Vec::with_capacity` + a parse loop
/// over tens of thousands of records and DoS the decoder. Real documents
/// rarely approach this; 8192 leaves headroom for complex comps.
pub(crate) const MAX_LAYER_RECORDS: usize = 8192;

const LARGE_TAGGED_BLOCK_KEYS: [[u8; 4]; 13] = [
    *b"LMsk", *b"Lr16", *b"Lr32", *b"Layr", *b"Mt16", *b"Mt32", *b"Mtrn", *b"Alph", *b"FMsk",
    *b"lnk2", *b"FEid", *b"FXid", *b"PxSD",
];

const TAGGED_BLOCK_MIN_HEADER_LEN: u64 = 12;
const SHMD_ENTRY_HEADER_LEN: usize = 16;
pub(crate) const MAX_TAGGED_BLOCK_RESYNCS_PER_LAYER: u32 = 64;

pub(crate) fn parse_layer_records(bytes: &[u8]) -> Result<LayerInfo<'_>, String> {
    let index = crate::psb_section_index::PsdSectionIndex::parse(bytes)?;
    parse_layer_records_from_index(&index, bytes)
}

/// Same as [`parse_layer_records`], but reuses an already-parsed
/// [`crate::psb_section_index::PsdSectionIndex`] instead of re-walking the
/// header, color mode data, and image resources sections.
pub(crate) fn parse_layer_records_from_index<'a>(
    index: &crate::psb_section_index::PsdSectionIndex,
    bytes: &'a [u8],
) -> Result<LayerInfo<'a>, String> {
    let width = index.width;
    let height = index.height;
    let depth = index.depth;
    let color_mode = index.color_mode;
    let is_psb = index.is_psb;

    let embedded_icc =
        crate::psb_reader::extract_icc_profile_from_ir(bytes, index.ir_start, index.ir_end);
    let cmyk_icc = if color_mode == PSD_COLOR_MODE_CMYK {
        crate::psb_cmyk_cms::resolve_cmyk_icc(embedded_icc.as_deref()).to_vec()
    } else {
        Vec::new()
    };

    let lm_start = index.lm_start;
    let lm_end = index.lm_end;
    if lm_end == lm_start {
        let empty = cursor_slice(bytes, lm_start, lm_start)?;
        return Ok(LayerInfo {
            records: Vec::new(),
            channel_data: empty,
            channel_data_shared: None,
            width,
            height,
            depth,
            color_mode,
            is_psb,
            cmyk_icc,
        });
    }

    let mut r = std::io::Cursor::new(bytes);
    r.set_position(lm_start);

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
            channel_data_shared: None,
            width,
            height,
            depth,
            color_mode,
            is_psb,
            cmyk_icc,
        });
    }

    let layer_count = read_i16(&mut r)?.unsigned_abs() as usize;
    if layer_count > MAX_LAYER_RECORDS {
        return Err(format!(
            "PSD/PSB layer count {layer_count} exceeds {MAX_LAYER_RECORDS}"
        ));
    }
    let mut records = Vec::with_capacity(layer_count);
    for _ in 0..layer_count {
        records.push(parse_layer_record(&mut r, layer_info_end, is_psb)?);
    }

    let channel_data_start = r.position();
    let channel_data = cursor_slice(bytes, channel_data_start, layer_info_end)?;

    Ok(LayerInfo {
        records,
        channel_data,
        channel_data_shared: Some(SharedSlice::new(Arc::from(channel_data))),
        width,
        height,
        depth,
        color_mode,
        is_psb,
        cmyk_icc,
    })
}

fn parse_layer_record(
    r: &mut std::io::Cursor<&[u8]>,
    layer_info_end: u64,
    is_psb: bool,
) -> Result<LayerRecord, String> {
    read_at(r, 16, layer_info_end, "layer rect")?;
    let mut rect = [0u8; 16];
    r.read_exact(&mut rect)
        .map_err(|e| format!("Read layer rect: {e}"))?;
    let top = i32::from_be_bytes([rect[0], rect[1], rect[2], rect[3]]);
    let left = i32::from_be_bytes([rect[4], rect[5], rect[6], rect[7]]);
    let bottom = i32::from_be_bytes([rect[8], rect[9], rect[10], rect[11]]);
    let right = i32::from_be_bytes([rect[12], rect[13], rect[14], rect[15]]);

    read_at(r, 2, layer_info_end, "layer channel count")?;
    let channel_count = crate::psb_reader::read_u16(r)? as usize;
    if channel_count > MAX_LAYER_CHANNELS_PER_RECORD {
        return Err(format!(
            "PSD/PSB layer channel count {channel_count} exceeds {MAX_LAYER_CHANNELS_PER_RECORD}"
        ));
    }
    // Channel length table: (i16 id + u32/u64 data_len) per channel.
    let channel_entry_size: u64 = if is_psb { 10 } else { 6 };
    let channel_table_len = (channel_count as u64)
        .checked_mul(channel_entry_size)
        .ok_or_else(|| "PSD/PSB layer channel table length overflow".to_string())?;
    read_at(r, channel_table_len, layer_info_end, "layer channel table")?;
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

    read_at(r, 12, layer_info_end, "layer blend header")?;
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

    read_at(r, 4, layer_info_end, "layer extra data length")?;
    let extra_size = crate::psb_reader::read_u32(r)? as u64;
    let extra_start = r.position();
    let extra_end = checked_end(extra_start, extra_size, layer_info_end, "layer extra data")?;
    let ParsedLayerExtra {
        mask_size,
        mask,
        real_mask,
        vector_mask,
        vector_mask_density,
        vector_mask_feather,
        is_section_divider,
        section_type,
        name,
        layer_id,
        cmls_payload,
        fill_opacity,
    } = parse_layer_extra(r, extra_end, is_psb)?;

    r.set_position(extra_end);

    Ok(LayerRecord {
        top,
        left,
        bottom,
        right,
        name,
        layer_id,
        cmls_payload,
        channels,
        blend,
        opacity,
        fill_opacity,
        clipping,
        flags,
        mask_size,
        mask,
        real_mask,
        vector_mask,
        vector_mask_density,
        vector_mask_feather,
        is_section_divider,
        section_type,
    })
}

struct ParsedLayerExtra {
    mask_size: u32,
    mask: Option<LayerMaskInfo>,
    real_mask: Option<LayerMaskInfo>,
    vector_mask: Option<VectorMaskData>,
    pub(crate) is_section_divider: bool,
    pub(crate) section_type: Option<u32>,
    name: String,
    layer_id: Option<u32>,
    cmls_payload: Option<Vec<u8>>,
    pub(crate) fill_opacity: Option<u8>,
    vector_mask_density: u8,
    vector_mask_feather: f64,
}

fn parse_layer_extra(
    r: &mut std::io::Cursor<&[u8]>,
    extra_end: u64,
    is_psb: bool,
) -> Result<ParsedLayerExtra, String> {
    if r.position() >= extra_end {
        return Ok(ParsedLayerExtra {
            mask_size: 0,
            mask: None,
            real_mask: None,
            vector_mask: None,
            vector_mask_density: 255,
            vector_mask_feather: 0.0,
            is_section_divider: false,
            section_type: None,
            name: String::new(),
            layer_id: None,
            cmls_payload: None,
            fill_opacity: None,
        });
    }

    let mask_size = read_extra_u32(r, extra_end, "layer mask data length")?;
    let mask_start = r.position();
    let mask_end = checked_end(mask_start, mask_size as u64, extra_end, "layer mask data")?;
    // Standard layer mask data (when >= 20 bytes): user-mask rect + default
    // color + flags (+ 2-byte pad). When >= 36 bytes, a real user mask rect
    // follows (after optional density/feather parameters when flags bit 4 is
    // set). Shorter blocks leave both masks as `None`.
    // parse_layer_mask_data now returns vector density/feather too.
    let (mask, real_mask, vector_mask_density, vector_mask_feather) =
        if mask_size >= LAYER_MASK_USER_HEADER_LEN {
            parse_layer_mask_data(r, mask_end)?
        } else {
            (None, None, 255, 0.0)
        };
    r.set_position(mask_end);

    let blending_ranges_len = read_extra_u32(r, extra_end, "layer blending ranges length")?;
    skip_extra_bytes(
        r,
        blending_ranges_len as u64,
        extra_end,
        "layer blending ranges",
    )?;

    let pascal_name = read_pascal_name(r, extra_end)?;
    let TaggedBlockScan {
        is_section_divider,
        section_type,
        layer_id,
        unicode_name,
        cmls_payload,
        fill_opacity,
        vector_mask,
    } = scan_extra_tagged_blocks(r, extra_end, is_psb)?;
    let name = unicode_name.unwrap_or(pascal_name);
    Ok(ParsedLayerExtra {
        mask_size,
        mask,
        real_mask,
        vector_mask,
        is_section_divider,
        section_type,
        name,
        layer_id,
        cmls_payload,
        fill_opacity,
        vector_mask_density,
        vector_mask_feather,
    })
}

/// Minimum mask-data size that can hold a user-mask header (rect + color +
/// flags + 2-byte pad).
const LAYER_MASK_USER_HEADER_LEN: u32 = 20;
/// Flags bit 4: density/feather parameters follow the user-mask header.
const LAYER_MASK_FLAGS_HAS_PARAMETERS: u8 = 0x10;

/// Parse user-mask and optional real-user-mask fields from a layer mask data
/// block. Cursor must be at the start of the mask payload; caller seeks to
/// `mask_end` afterward.
fn parse_layer_mask_data(
    r: &mut std::io::Cursor<&[u8]>,
    mask_end: u64,
) -> Result<(Option<LayerMaskInfo>, Option<LayerMaskInfo>, u8, f64), String> {
    let Some(mut mask) = parse_layer_mask_rect(r, mask_end)? else {
        return Ok((None, None, 255, 0.0));
    };

    // Read density/feather parameters when present.
    let mut vector_density = 255u8;
    let mut vector_feather = 0.0f64;
    if mask.has_parameters_applied {
        let (ud, uf, vd, vf, ok) = read_mask_parameters_prefix(r, mask_end)?;
        mask.density = ud;
        mask.feather = uf;
        vector_density = vd;
        vector_feather = vf;
        if !ok {
            return Ok((Some(mask), None, vector_density, vector_feather));
        }
    }

    // Spec pads the user-mask header to 20 bytes (2 bytes after flags).
    const USER_MASK_PAD: u64 = 2;
    if checked_end(r.position(), USER_MASK_PAD, mask_end, "layer mask pad").is_ok() {
        crate::psb_reader::seek_forward(r, USER_MASK_PAD)?;
    }

    let real_mask = parse_real_user_mask_rect(r, mask_end, &mask)?;
    Ok((Some(mask), real_mask, vector_density, vector_feather))
}

/// When the mask data block is long enough, parse the real user mask rect
/// used by channel id -3. Density/feather parameters (flags bit 4) have
/// already been consumed by `parse_layer_mask_data` before calling this
/// function; if their layout was untrustworthy, the real mask is left as
/// `None` and channel -3 falls back to the user-mask rect.
///
/// Common on-disk sizes after the 20-byte user-mask header:
/// - +16 bytes: real-mask rect only (total 36)
/// - +18 bytes: real-mask rect + default color + flags
fn parse_real_user_mask_rect(
    r: &mut std::io::Cursor<&[u8]>,
    mask_end: u64,
    user_mask: &LayerMaskInfo,
) -> Result<Option<LayerMaskInfo>, String> {
    const REAL_MASK_RECT_LEN: u64 = 4 * 4;
    const REAL_MASK_FULL_LEN: u64 = REAL_MASK_RECT_LEN + 1 + 1;
    // Density/feather parameters were already consumed by
    // `parse_layer_mask_data` if `has_parameters_applied` was set.
    let remaining = mask_end.saturating_sub(r.position());
    if remaining >= REAL_MASK_FULL_LEN {
        let mut real = parse_layer_mask_rect(r, mask_end)?;
        if let Some(ref mut r) = real {
            r.density = user_mask.density;
            r.feather = user_mask.feather;
        }
        return Ok(real);
    }
    if remaining < REAL_MASK_RECT_LEN {
        return Ok(None);
    }
    let top = read_i32(r)?;
    let left = read_i32(r)?;
    let bottom = read_i32(r)?;
    let right = read_i32(r)?;
    Ok(Some(LayerMaskInfo {
        top,
        left,
        bottom,
        right,
        default_color: 0,
        disabled: false,
        has_parameters_applied: false,
        density: user_mask.density,
        feather: user_mask.feather,
    }))
}

/// Read the density/feather parameter prefix that follows the user-mask
/// header when flags bit 4 is set, and return the parsed values.
/// Returns `(user_density, user_feather, vector_density, vector_feather, plausible)`.
/// When a flag bit is absent, density defaults to 255 and feather to 0.0.
fn read_mask_parameters_prefix(
    r: &mut std::io::Cursor<&[u8]>,
    mask_end: u64,
) -> Result<(u8, f64, u8, f64, bool), String> {
    let remaining_before = mask_end.saturating_sub(r.position());
    if remaining_before < 1 {
        return Ok((255, 0.0, 255, 0.0, false));
    }
    let mut present = [0u8; 1];
    r.read_exact(&mut present)
        .map_err(|e| format!("Read layer mask parameters flags: {e}"))?;
    // Bit 0: user density (1 byte), bit 1: user feather (8 bytes),
    // bit 2: vector density (1 byte), bit 3: vector feather (8 bytes).
    let mut density: u8 = 255;
    let mut feather: f64 = 0.0;
    let mut vector_density: u8 = 255;
    let mut vector_feather: f64 = 0.0;

    if present[0] & 0x01 != 0 {
        let mut buf = [0u8; 1];
        r.read_exact(&mut buf)
            .map_err(|e| format!("Read layer mask density: {e}"))?;
        density = buf[0];
    }
    if present[0] & 0x02 != 0 {
        let mut buf = [0u8; 8];
        r.read_exact(&mut buf)
            .map_err(|e| format!("Read layer mask feather: {e}"))?;
        feather = f64::from_be_bytes(buf).max(0.0);
    }
    if present[0] & 0x04 != 0 {
        let mut buf = [0u8; 1];
        r.read_exact(&mut buf)
            .map_err(|e| format!("Read vector mask density: {e}"))?;
        vector_density = buf[0];
    }
    if present[0] & 0x08 != 0 {
        let mut buf = [0u8; 8];
        r.read_exact(&mut buf)
            .map_err(|e| format!("Read vector mask feather: {e}"))?;
        vector_feather = f64::from_be_bytes(buf).max(0.0);
    }
    let remaining = mask_end.saturating_sub(r.position());
    let ok = remaining == 0 || remaining >= 4 * 4;
    Ok((density, feather, vector_density, vector_feather, ok))
}

/// Parse the rect + default color + flags fields at the start of a layer's
/// mask data block (see [`LayerMaskInfo`]). Returns `None` if the block is
/// too short to hold them (defensive; callers check size before calling).
fn parse_layer_mask_rect(
    r: &mut std::io::Cursor<&[u8]>,
    mask_end: u64,
) -> Result<Option<LayerMaskInfo>, String> {
    const RECT_PLUS_COLOR_AND_FLAGS_LEN: u64 = 4 * 4 + 1 + 1;
    if checked_end(
        r.position(),
        RECT_PLUS_COLOR_AND_FLAGS_LEN,
        mask_end,
        "layer mask rect",
    )
    .is_err()
    {
        return Ok(None);
    }

    let top = read_i32(r)?;
    let left = read_i32(r)?;
    let bottom = read_i32(r)?;
    let right = read_i32(r)?;

    let mut default_color = [0u8; 1];
    r.read_exact(&mut default_color)
        .map_err(|e| format!("Read layer mask default color: {e}"))?;
    let mut mask_flags = [0u8; 1];
    r.read_exact(&mut mask_flags)
        .map_err(|e| format!("Read layer mask flags: {e}"))?;
    let disabled = mask_flags[0] & 0x02 != 0;
    let has_parameters_applied = mask_flags[0] & LAYER_MASK_FLAGS_HAS_PARAMETERS != 0;

    Ok(Some(LayerMaskInfo {
        top,
        left,
        bottom,
        right,
        default_color: default_color[0],
        disabled,
        has_parameters_applied,
        density: 255,
        feather: 0.0,
    }))
}

pub(crate) fn scan_extra_tagged_blocks(
    r: &mut std::io::Cursor<&[u8]>,
    extra_end: u64,
    is_psb: bool,
) -> Result<TaggedBlockScan, String> {
    let mut is_section_divider = false;
    let mut section_type = None;
    let mut layer_id = None;
    let mut unicode_name = None;
    let mut cmls_payload = None;
    let mut fill_opacity = None;
    let mut vector_mask: Option<VectorMaskData> = None;
    let mut resyncs = 0u32;
    let extra_end_usize = usize::try_from(extra_end)
        .unwrap_or(usize::MAX)
        .min(r.get_ref().len());

    while r.position().saturating_add(TAGGED_BLOCK_MIN_HEADER_LEN) <= extra_end {
        let search_start = match usize::try_from(r.position()) {
            Ok(pos) => pos,
            Err(_) => break,
        };
        let Some(block_start_usize) =
            find_next_tagged_block_signature(r.get_ref(), search_start, extra_end_usize)
        else {
            r.set_position(extra_end);
            break;
        };
        let block_start = block_start_usize as u64;
        if block_start.saturating_add(TAGGED_BLOCK_MIN_HEADER_LEN) > extra_end {
            r.set_position(extra_end);
            break;
        }

        r.set_position(block_start);
        let mut signature = [0u8; 4];
        r.read_exact(&mut signature)
            .map_err(|e| format!("Read layer tagged block signature: {e}"))?;

        let mut key = [0u8; 4];
        r.read_exact(&mut key)
            .map_err(|e| format!("Read layer tagged block key: {e}"))?;
        let uses_u64_len = tagged_block_uses_u64_len(&signature, &key, is_psb);
        if uses_u64_len
            && checked_end(r.position(), 8, extra_end, "layer tagged block length").is_err()
        {
            if abandon_tagged_block_candidate(r, block_start, extra_end, &mut resyncs) {
                break;
            }
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
                if abandon_tagged_block_candidate(r, block_start, extra_end, &mut resyncs) {
                    break;
                }
                continue;
            }
        };

        let bytes = r.get_ref();
        let data_start_usize = usize::try_from(data_start).unwrap_or(usize::MAX);
        let data_end_usize = usize::try_from(data_end).unwrap_or(usize::MAX);
        let payload = bytes.get(data_start_usize..data_end_usize).unwrap_or(&[]);

        if &key == SECTION_DIVIDER_KEY && data_len >= 4 {
            let data_start = usize::try_from(data_start)
                .map_err(|_| "PSD/PSB lsct data_start overflows usize".to_string())?;
            let section_end = data_start
                .checked_add(4)
                .ok_or_else(|| "PSD/PSB lsct section_type end overflows".to_string())?;
            let section_bytes = bytes.get(data_start..section_end).ok_or_else(|| {
                "PSD/PSB lsct section_type truncated (expected 4 bytes)".to_string()
            })?;
            section_type = Some(u32::from_be_bytes([
                section_bytes[0],
                section_bytes[1],
                section_bytes[2],
                section_bytes[3],
            ]));
            is_section_divider = true;
        } else if &key == b"lyid" && payload.len() >= 4 {
            layer_id = Some(u32::from_be_bytes([
                payload[0], payload[1], payload[2], payload[3],
            ]));
        } else if &key == b"luni" {
            if let Some(name) = parse_luni_name(payload) {
                unicode_name = Some(name);
            }
        } else if &key == b"shmd"
            && let Some(payload) = parse_shmd_cmls_payload(payload)
        {
            cmls_payload = Some(payload);
        } else if &key == b"iOpa" && !payload.is_empty() {
            // Photoshop Fill Opacity (single byte). Present even when 255.
            fill_opacity = Some(payload[0]);
        } else if (&key == b"vmsk" || &key == b"vsms") && payload.len() >= 30 {
            let version_bytes: [u8; 4] = payload
                .get(..4)
                .and_then(|b| b.try_into().ok())
                .unwrap_or([0u8; 4]);
            let version = i32::from_be_bytes(version_bytes);
            if version == 3 {
                // Structure: Version(4) + Flags(4) + path records.
                let flags_raw = u32::from_be_bytes(
                    payload
                        .get(4..8)
                        .and_then(|b| b.try_into().ok())
                        .unwrap_or([0u8; 4]),
                );
                let flags = VectorMaskFlags {
                    invert: flags_raw & 0x01 != 0,
                    not_linked: flags_raw & 0x02 != 0,
                    disabled: flags_raw & 0x04 != 0,
                };
                let path_bytes = &payload[8..];
                let mut records: Vec<[u8; VMSK_RECORD_LEN]> = Vec::new();
                for chunk in path_bytes.chunks_exact(VMSK_RECORD_LEN) {
                    let mut rec = [0u8; VMSK_RECORD_LEN];
                    rec.copy_from_slice(chunk);
                    let selector = i16::from_be_bytes([rec[0], rec[1]]);
                    // 0xFFFF = end of path; stop collecting.
                    if selector == -1 {
                        break;
                    }
                    records.push(rec);
                }
                if !records.is_empty() {
                    vector_mask = Some(VectorMaskData { records, flags });
                }
            } else {
                // Adobe only writes version 3 (PS 6.0+); v2 or unknown
                // versions from legacy files should not appear in practice.
                log::warn!(
                    "vmsk/vsms unexpected version {version} (expected 3), skipping vector mask"
                );
            }
        }

        let padded_end = data_end.saturating_add(data_len % 2);
        r.set_position(padded_end.min(extra_end));
    }

    Ok(TaggedBlockScan {
        is_section_divider,
        section_type,
        layer_id,
        unicode_name,
        cmls_payload,
        fill_opacity,
        vector_mask,
    })
}

pub(crate) struct TaggedBlockScan {
    pub(crate) is_section_divider: bool,
    pub(crate) section_type: Option<u32>,
    layer_id: Option<u32>,
    unicode_name: Option<String>,
    cmls_payload: Option<Vec<u8>>,
    pub(crate) fill_opacity: Option<u8>,
    vector_mask: Option<VectorMaskData>,
}

fn parse_luni_name(payload: &[u8]) -> Option<String> {
    let len_bytes: [u8; 4] = payload.get(0..4)?.try_into().ok()?;
    let unit_count = u32::from_be_bytes(len_bytes) as usize;
    let byte_len = unit_count.checked_mul(2)?;
    let string_bytes = payload.get(4..4usize.checked_add(byte_len)?)?;
    let mut units = Vec::with_capacity(unit_count);
    for chunk in string_bytes.chunks_exact(2) {
        units.push(u16::from_be_bytes([chunk[0], chunk[1]]));
    }
    String::from_utf16(&units).ok()
}

fn parse_shmd_cmls_payload(payload: &[u8]) -> Option<Vec<u8>> {
    let count_bytes: [u8; 4] = payload.get(0..4)?.try_into().ok()?;
    let entry_count = u32::from_be_bytes(count_bytes) as usize;
    let mut pos = 4usize;
    for _ in 0..entry_count {
        let header_end = pos.checked_add(SHMD_ENTRY_HEADER_LEN)?;
        let header = payload.get(pos..header_end)?;
        let signature = header.get(0..4)?;
        let key = header.get(4..8)?;
        let len_bytes: [u8; 4] = header.get(12..16)?.try_into().ok()?;
        let data_len = u32::from_be_bytes(len_bytes) as usize;
        let data_start = header_end;
        let data_end = data_start.checked_add(data_len)?;
        let data = payload.get(data_start..data_end)?;
        if signature == PSD_BLEND_SIGNATURE && key == b"cmls" {
            return Some(data.to_vec());
        }
        pos = data_end.checked_add(data_len % 2)?;
        if pos > payload.len() {
            return None;
        }
    }
    None
}

/// Scan for the next `8BIM` / `8B64` block signature using memchr.
///
/// O(n) single-byte-find scan with quadratic-match rejection; replaces a
/// prior O(n²) `bytes.windows(4).position()` that re-scanned rejected
/// candidates from scratch on every false match.
fn find_next_tagged_block_signature(bytes: &[u8], start: usize, limit: usize) -> Option<usize> {
    let start = start.min(limit);
    let haystack = &bytes[start..limit];
    for offset in memchr::Memchr::new(PSD_BLEND_SIGNATURE[0], haystack) {
        let candidate = start + offset;
        if candidate.saturating_add(4) > limit {
            return None;
        }
        let signature = &bytes[candidate..candidate + 4];
        if signature == PSD_BLEND_SIGNATURE || signature == PSB_BLOCK_SIGNATURE {
            return Some(candidate);
        }
    }
    None
}

fn abandon_tagged_block_candidate(
    r: &mut std::io::Cursor<&[u8]>,
    block_start: u64,
    extra_end: u64,
    resyncs: &mut u32,
) -> bool {
    *resyncs = resyncs.saturating_add(1);
    if *resyncs > MAX_TAGGED_BLOCK_RESYNCS_PER_LAYER {
        r.set_position(extra_end);
        return true;
    }
    r.set_position(block_start.saturating_add(1));
    false
}

fn read_pascal_name(r: &mut std::io::Cursor<&[u8]>, extra_end: u64) -> Result<String, String> {
    if r.position() >= extra_end {
        return Ok(String::new());
    }

    let mut len = [0u8; 1];
    r.read_exact(&mut len)
        .map_err(|e| format!("Read layer name length: {e}"))?;
    let raw_len = 1u64 + len[0] as u64;
    let padded_len = raw_len.next_multiple_of(4);
    read_at(
        r,
        padded_len.saturating_sub(1),
        extra_end,
        "layer name data",
    )?;
    let name_len = len[0] as usize;
    let name_start = r.position() as usize;
    let name_end = name_start
        .checked_add(name_len)
        .ok_or_else(|| "PSD/PSB layer name length overflow".to_string())?;
    let name_bytes = r
        .get_ref()
        .get(name_start..name_end)
        .ok_or_else(|| "PSD/PSB layer name exceeds section boundary".to_string())?;
    let name = String::from_utf8_lossy(name_bytes).into_owned();
    crate::psb_reader::seek_forward(r, padded_len.saturating_sub(1))?;
    Ok(name)
}

fn read_extra_u32(
    r: &mut std::io::Cursor<&[u8]>,
    extra_end: u64,
    label: &str,
) -> Result<u32, String> {
    read_at(r, 4, extra_end, label)?;
    crate::psb_reader::read_u32(r)
}

fn skip_extra_bytes(
    r: &mut std::io::Cursor<&[u8]>,
    len: u64,
    extra_end: u64,
    label: &str,
) -> Result<(), String> {
    read_at(r, len, extra_end, label)?;
    crate::psb_reader::seek_forward(r, len)
}

/// Ensure `len` bytes remain before `limit` at the current cursor position.
#[inline]
fn read_at(
    r: &mut std::io::Cursor<&[u8]>,
    len: u64,
    limit: u64,
    label: &str,
) -> Result<(), String> {
    checked_end(r.position(), len, limit, label)?;
    Ok(())
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
