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

//! Layer-aware PSD/PSB compositor.
//!
//! Used when the flattened Image Data section cannot be decoded structurally
//! (see `psb_sdr_main::decode_psd_sdr_main_from_bytes_with_cancel`). Decodes
//! each layer's channels (depth 8) and composites them bottom to top with
//! Normal / Screen / Linear Dodge / Multiply blend + opacity + user mask +
//! clipping groups, respecting strict Photoshop layer/group visibility only
//! (no viewer heuristics that open hidden layers).

use std::io::Read;

const PSD_BLEND_SIGNATURE: &[u8; 4] = b"8BIM";
const PSB_BLOCK_SIGNATURE: &[u8; 4] = b"8B64";
const SECTION_DIVIDER_KEY: &[u8; 4] = b"lsct";
const MAX_LAYER_CHANNELS_PER_RECORD: usize = 128;
/// Cap on layer records in the Layer Info section.
///
/// The on-disk count is an `i16` absolute value (at most 65535). Without a
/// tighter cap, a malicious file can force `Vec::with_capacity` + a parse loop
/// over tens of thousands of records and DoS the decoder. Real documents
/// rarely approach this; 8192 leaves headroom for complex comps.
const MAX_LAYER_RECORDS: usize = 8192;
/// Cap on `width * height` for a single layer/mask rect.
///
/// `PSD_MAX_DIMENSION` alone still allows `300_000 x 300_000` (~90GB for one
/// 8-bit channel). This pixel budget keeps malicious/malformed layer bounds
/// from OOM-killing the process while still allowing large legitimate layers
/// (e.g. 32k x 32k, or a long strip up to `PSD_MAX_DIMENSION` on one side).
const MAX_LAYER_PIXELS: u64 = 1024 * 1024 * 1024;
/// Cap on the sum of decoded layer RGBA8 buffers held for one composite pass.
///
/// Per-layer pixel caps alone still allow many large layers to be decoded in
/// parallel and retained until blending finishes. 8 GiB bounds that without
/// rejecting typical multi-layer comps on a desktop viewer.
const MAX_COMPOSITE_DECODED_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const LARGE_TAGGED_BLOCK_KEYS: [[u8; 4]; 13] = [
    *b"LMsk", *b"Lr16", *b"Lr32", *b"Layr", *b"Mt16", *b"Mt32", *b"Mtrn", *b"Alph", *b"FMsk",
    *b"lnk2", *b"FEid", *b"FXid", *b"PxSD",
];

#[derive(Debug, Clone)]
pub struct LayerChannel {
    pub id: i16,
    pub data_len: u32,
}

/// Parsed rectangle + flags from a layer's mask data block (channel id -2).
/// The mask's own rect can differ from the layer's rect (smaller, larger, or
/// offset), so it is decoded and blitted separately -- see `build_layer_sized_mask`.
#[derive(Debug, Clone, Copy)]
pub struct LayerMaskInfo {
    pub top: i32,
    pub left: i32,
    pub bottom: i32,
    pub right: i32,
    /// Value (0 or 255) used for layer pixels outside the mask rect.
    pub default_color: u8,
    /// Mask disabled (bit 1 of the mask flags byte): treat as if no mask.
    pub disabled: bool,
    /// Flags bit 4: user/vector masks have density/feather parameters. When
    /// set, extra parameter bytes sit between the user-mask header and any
    /// real-user-mask rect in the mask data block.
    pub has_parameters_applied: bool,
}

impl LayerMaskInfo {
    fn is_empty_bounds(&self) -> bool {
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
    /// User mask rect/flags parsed from the mask data block, when present and
    /// long enough to contain the standard rect + default color + flags fields.
    pub mask: Option<LayerMaskInfo>,
    /// Real user mask rect (channel id -3), when the mask data block includes
    /// a second rect after the user-mask header (typically `mask_size >= 36`).
    pub real_mask: Option<LayerMaskInfo>,
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

/// Whether `width`/`height` are within the per-side limit used for the
/// document canvas (`psb_reader::PSD_MAX_DIMENSION`) and the total pixel cap
/// (`MAX_LAYER_PIXELS`).
///
/// A malformed or malicious layer record can claim absurd bounds (e.g.
/// `right - left` near `u32::MAX`, or both sides at `PSD_MAX_DIMENSION`) that
/// would make `decode_channel_image`'s allocation try to reserve tens of GB
/// and abort the process. Layer and mask rects are checked before any such
/// allocation.
fn dimensions_within_limit(width: u32, height: u32) -> bool {
    if width > crate::psb_reader::PSD_MAX_DIMENSION || height > crate::psb_reader::PSD_MAX_DIMENSION
    {
        return false;
    }
    match (width as u64).checked_mul(height as u64) {
        Some(pixels) => pixels <= MAX_LAYER_PIXELS,
        None => false,
    }
}

/// `width * height` for a layer/mask buffer, rejecting overflow.
fn checked_layer_pixel_count(width: u32, height: u32) -> Option<usize> {
    (width as u64)
        .checked_mul(height as u64)
        .filter(|&n| n <= MAX_LAYER_PIXELS)
        .and_then(|n| usize::try_from(n).ok())
}

/// Add one layer's RGBA8 footprint to `acc`, enforcing
/// [`MAX_COMPOSITE_DECODED_BYTES`].
fn accumulate_decoded_layer_bytes(acc: u64, width: u32, height: u32) -> Result<u64, String> {
    let pixels = checked_layer_pixel_count(width, height)
        .ok_or_else(|| format!("PSD/PSB layer channel size {width}x{height} exceeds limit"))?;
    let rgba = (pixels as u64)
        .checked_mul(4)
        .ok_or_else(|| "PSD/PSB decoded layer RGBA size overflow".to_string())?;
    let next = acc
        .checked_add(rgba)
        .ok_or_else(|| "PSD/PSB decoded layer byte total overflow".to_string())?;
    if next > MAX_COMPOSITE_DECODED_BYTES {
        return Err(format!(
            "PSD/PSB decoded layer byte budget exceeded ({next} > {MAX_COMPOSITE_DECODED_BYTES})"
        ));
    }
    Ok(next)
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
    /// Resolved CMYK ICC (embedded or default). Empty when not CMYK.
    pub cmyk_icc: Vec<u8>,
}

pub fn parse_layer_records(bytes: &[u8]) -> Result<LayerInfo<'_>, String> {
    let index = crate::psb_section_index::PsdSectionIndex::parse(bytes)?;
    parse_layer_records_from_index(&index, bytes)
}

/// Same as [`parse_layer_records`], but reuses an already-parsed
/// [`crate::psb_section_index::PsdSectionIndex`] instead of re-walking the
/// header, color mode data, and image resources sections.
pub fn parse_layer_records_from_index<'a>(
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
    let cmyk_icc = if color_mode == 4 {
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
    let top = read_i32(r)?;
    let left = read_i32(r)?;
    let bottom = read_i32(r)?;
    let right = read_i32(r)?;

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
        is_section_divider,
        section_type,
    } = parse_layer_extra(r, extra_end, is_psb)?;

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
        mask,
        real_mask,
        is_section_divider,
        section_type,
    })
}

struct ParsedLayerExtra {
    mask_size: u32,
    mask: Option<LayerMaskInfo>,
    real_mask: Option<LayerMaskInfo>,
    is_section_divider: bool,
    section_type: Option<u32>,
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
            is_section_divider: false,
            section_type: None,
        });
    }

    let mask_size = read_extra_u32(r, extra_end, "layer mask data length")?;
    let mask_start = r.position();
    let mask_end = checked_end(mask_start, mask_size as u64, extra_end, "layer mask data")?;
    // Standard layer mask data (when >= 20 bytes): user-mask rect + default
    // color + flags (+ 2-byte pad). When >= 36 bytes, a real user mask rect
    // follows (after optional density/feather parameters when flags bit 4 is
    // set). Shorter blocks leave both masks as `None`.
    let (mask, real_mask) = if mask_size >= LAYER_MASK_USER_HEADER_LEN {
        parse_layer_mask_data(r, mask_end)?
    } else {
        (None, None)
    };
    r.set_position(mask_end);

    let blending_ranges_len = read_extra_u32(r, extra_end, "layer blending ranges length")?;
    skip_extra_bytes(
        r,
        blending_ranges_len as u64,
        extra_end,
        "layer blending ranges",
    )?;

    skip_pascal_name(r, extra_end)?;
    let (is_section_divider, section_type) = scan_extra_tagged_blocks(r, extra_end, is_psb)?;
    Ok(ParsedLayerExtra {
        mask_size,
        mask,
        real_mask,
        is_section_divider,
        section_type,
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
) -> Result<(Option<LayerMaskInfo>, Option<LayerMaskInfo>), String> {
    let Some(mask) = parse_layer_mask_rect(r, mask_end)? else {
        return Ok((None, None));
    };
    // Spec pads the user-mask header to 20 bytes (2 bytes after flags).
    const USER_MASK_PAD: u64 = 2;
    if checked_end(r.position(), USER_MASK_PAD, mask_end, "layer mask pad").is_ok() {
        crate::psb_reader::seek_forward(r, USER_MASK_PAD)?;
    }

    let real_mask = parse_real_user_mask_rect(r, mask_end, &mask)?;
    Ok((Some(mask), real_mask))
}

/// When the mask data block is long enough, parse the real user mask rect
/// used by channel id -3. Density/feather parameter bytes (flags bit 4) are
/// skipped when present; if their layout cannot be trusted, real mask is
/// left as `None` and channel -3 falls back to the user-mask rect.
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
    if user_mask.has_parameters_applied && !skip_mask_parameters_prefix(r, mask_end)? {
        return Ok(None);
    }
    let remaining = mask_end.saturating_sub(r.position());
    if remaining >= REAL_MASK_FULL_LEN {
        return parse_layer_mask_rect(r, mask_end);
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
    }))
}

/// Skip the density/feather parameter prefix that follows the user-mask
/// header when flags bit 4 is set. Returns `true` when the cursor is left
/// at a plausible real-mask header (or section end with nothing left).
fn skip_mask_parameters_prefix(
    r: &mut std::io::Cursor<&[u8]>,
    mask_end: u64,
) -> Result<bool, String> {
    let remaining_before = mask_end.saturating_sub(r.position());
    if remaining_before < 1 {
        return Ok(false);
    }
    let mut present = [0u8; 1];
    r.read_exact(&mut present)
        .map_err(|e| format!("Read layer mask parameters flags: {e}"))?;
    // Bit 0: user density, bit 1: user feather, bit 2: vector density,
    // bit 3: vector feather.
    let mut need = 0u64;
    if present[0] & 0x01 != 0 {
        need = need.saturating_add(1);
    }
    if present[0] & 0x02 != 0 {
        need = need.saturating_add(8);
    }
    if present[0] & 0x04 != 0 {
        need = need.saturating_add(1);
    }
    if present[0] & 0x08 != 0 {
        need = need.saturating_add(8);
    }
    if checked_end(r.position(), need, mask_end, "layer mask parameters").is_err() {
        return Ok(false);
    }
    crate::psb_reader::seek_forward(r, need)?;
    let remaining = mask_end.saturating_sub(r.position());
    // Real mask may be rect-only (16) or rect+color+flags (18).
    Ok(remaining == 0 || remaining >= 4 * 4)
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
    }))
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
            // Defensive: checked_end already bounds data_end, but saturating_add
            // on overflow could leave data_start past the buffer.
            if let Some(section_bytes) = bytes.get(data_start..data_start.saturating_add(4))
                && section_bytes.len() == 4
            {
                section_type = Some(u32::from_be_bytes([
                    section_bytes[0],
                    section_bytes[1],
                    section_bytes[2],
                    section_bytes[3],
                ]));
                is_section_divider = true;
            }
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

// -- Layer channel decode ---------------------------------------------

/// How often (in rows) to poll `cancel` inside a single channel's RLE decode.
const RLE_ROW_CANCEL_POLL_INTERVAL: usize = 64;

/// Decode one channel's image data (compression header + rows) into 8-bit samples.
/// `data` must be exactly the channel's declared byte range (depth 8 only, v1).
fn decode_channel_image(
    data: &[u8],
    width: u32,
    height: u32,
    is_psb: bool,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<Vec<u8>, crate::loader::DecodeError> {
    let mut r = std::io::Cursor::new(data);
    let compression = crate::psb_reader::read_u16(&mut r)?;
    let pixel_count = checked_layer_pixel_count(width, height)
        .ok_or_else(|| format!("PSD/PSB layer channel size {width}x{height} exceeds limit"))?;

    match compression {
        0 => {
            // Avoid zero-filling the copied prefix: grow from the raw slice.
            let avail = data.len().saturating_sub(2);
            let copy = avail.min(pixel_count);
            let mut out = data[2..2 + copy].to_vec();
            out.resize(pixel_count, 0);
            Ok(out)
        }
        1 => {
            let mut row_counts = Vec::with_capacity(height as usize);
            for row in 0..height as usize {
                if row % RLE_ROW_CANCEL_POLL_INTERVAL == 0 {
                    crate::psb_reader::check_decode_cancel(cancel)?;
                }
                let count = if is_psb {
                    crate::psb_reader::read_u32(&mut r)? as usize
                } else {
                    crate::psb_reader::read_u16(&mut r)? as usize
                };
                row_counts.push(count);
            }

            let mut out = vec![0u8; pixel_count];
            let mut row_buf = Vec::with_capacity(width as usize);
            let width_usize = width as usize;
            for (row, &count) in row_counts.iter().enumerate() {
                if row % RLE_ROW_CANCEL_POLL_INTERVAL == 0 {
                    crate::psb_reader::check_decode_cancel(cancel)?;
                }
                let start = r.position() as usize;
                let end = start
                    .checked_add(count)
                    .ok_or_else(|| "PSD/PSB layer channel RLE row length overflow".to_string())?;
                let compressed = data
                    .get(start..end)
                    .ok_or_else(|| "PSD/PSB layer channel RLE row out of bounds".to_string())?;
                crate::psb_reader::unpack_bits_into(&mut row_buf, compressed, width_usize);
                let dst_start = row
                    .checked_mul(width_usize)
                    .ok_or_else(|| "PSD/PSB layer channel row offset overflow".to_string())?;
                let dst_end = dst_start
                    .checked_add(width_usize)
                    .ok_or_else(|| "PSD/PSB layer channel row end overflow".to_string())?;
                out.get_mut(dst_start..dst_end)
                    .ok_or_else(|| "PSD/PSB layer channel row out of bounds".to_string())?
                    .copy_from_slice(&row_buf[..width_usize]);
                r.set_position(end as u64);
            }
            Ok(out)
        }
        2 | 3 => {
            crate::psb_reader::check_decode_cancel(cancel)?;
            let compressed = data
                .get(2..)
                .ok_or_else(|| "PSD/PSB layer channel ZIP payload missing".to_string())?;
            crate::psb_zip::decode_zip_channel_bytes(
                compressed,
                width as usize,
                height as usize,
                8,
                compression == 3,
            )
            .map_err(Into::into)
        }
        _ => Err(format!("Unsupported layer channel compression: {compression}").into()),
    }
}

// -- Layer RGBA assembly -------------------------------------------------

/// Build a layer's straight-alpha RGBA8 rect from its decoded channels.
/// `color[0..3]` map to C/M/Y/K (mode 4) or R/G/B (mode 3, and fallback).
/// `color[0]` alone is used as gray for mode 1. Opacity and the optional
/// user mask are folded into alpha.
struct LayerRgbaArgs<'a> {
    color_mode: u16,
    width: u32,
    height: u32,
    color: &'a [Option<Vec<u8>>; 4],
    alpha: Option<&'a [u8]>,
    mask: Option<&'a [u8]>,
    opacity: u8,
    cmyk_icc: &'a [u8],
}

fn layer_to_rgba8(args: LayerRgbaArgs<'_>) -> Vec<u8> {
    let Some(pixel_count) = checked_layer_pixel_count(args.width, args.height) else {
        return Vec::new();
    };
    let Some(rgba_len) = pixel_count.checked_mul(4) else {
        return Vec::new();
    };
    let opacity = args.opacity as u32;

    if args.color_mode == 4
        && let (Some(c), Some(m), Some(y), Some(k)) = (
            args.color[0].as_deref(),
            args.color[1].as_deref(),
            args.color[2].as_deref(),
            args.color[3].as_deref(),
        )
    {
        let icc = crate::psb_cmyk_cms::resolve_cmyk_icc(if args.cmyk_icc.is_empty() {
            None
        } else {
            Some(args.cmyk_icc)
        });
        let span = crate::psb_cmyk_cms::AdobeCmykSpan {
            c,
            m,
            y,
            k,
            alpha: args.alpha,
        };
        if let Some(mut rgba) = crate::psb_cmyk_cms::planar_cmyk_adobe_to_rgba8(&span, icc) {
            fold_opacity_mask_into_alpha(&mut rgba, opacity, args.mask);
            return rgba;
        }
    }

    // Gray fast path: broadcast G->RGB via SIMD, then fold opacity/mask into alpha.
    if args.color_mode == 1
        && let Some(gray) = args.color[0].as_deref()
        && gray.len() >= pixel_count
    {
        let mut rgba = vec![0u8; rgba_len];
        let g = &gray[..pixel_count];
        if let Some(a) = args.alpha.filter(|a| a.len() >= pixel_count) {
            simple_image_viewer::simd_swizzle::interleave_rgba(
                g,
                g,
                g,
                &a[..pixel_count],
                &mut rgba,
            );
        } else {
            simple_image_viewer::simd_swizzle::interleave_rgb_with_alpha(g, g, g, 255, &mut rgba);
        }
        fold_opacity_mask_into_alpha(&mut rgba, opacity, args.mask);
        return rgba;
    }

    let mut rgba = vec![0u8; rgba_len];
    let sample =
        |ch: &Option<Vec<u8>>, i: usize| ch.as_deref().and_then(|d| d.get(i)).copied().unwrap_or(0);

    for i in 0..pixel_count {
        let (r, g, b) = match args.color_mode {
            4 => crate::psb_reader::cmyk_to_rgb(
                sample(&args.color[0], i),
                sample(&args.color[1], i),
                sample(&args.color[2], i),
                sample(&args.color[3], i),
            ),
            1 => {
                let v = sample(&args.color[0], i);
                (v, v, v)
            }
            _ => (
                sample(&args.color[0], i),
                sample(&args.color[1], i),
                sample(&args.color[2], i),
            ),
        };

        let base_alpha = args.alpha.and_then(|a| a.get(i)).copied().unwrap_or(255) as u32;
        let mut a = base_alpha * opacity / 255;
        if let Some(m) = args.mask {
            let mv = m.get(i).copied().unwrap_or(255) as u32;
            a = a * mv / 255;
        }

        let off = i * 4;
        rgba[off] = r;
        rgba[off + 1] = g;
        rgba[off + 2] = b;
        rgba[off + 3] = a as u8;
    }

    rgba
}

fn fold_opacity_mask_into_alpha(rgba: &mut [u8], opacity: u32, mask: Option<&[u8]>) {
    let pixel_count = rgba.len() / 4;
    for i in 0..pixel_count {
        let off = i * 4 + 3;
        let mut a = rgba[off] as u32 * opacity / 255;
        if let Some(m) = mask {
            let mv = m.get(i).copied().unwrap_or(255) as u32;
            a = a * mv / 255;
        }
        rgba[off] = a as u8;
    }
}

/// Decode a user/real mask channel into a layer-sized alpha matte, or `None`
/// when the mask is disabled, empty, or oversized (caller keeps no-mask).
#[allow(clippy::too_many_arguments)]
fn decode_mask_channel_to_layer(
    slice: &[u8],
    mask_info: &LayerMaskInfo,
    layer_left: i32,
    layer_top: i32,
    layer_w: u32,
    layer_h: u32,
    is_psb: bool,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<Option<Vec<u8>>, crate::loader::DecodeError> {
    let mask_w = mask_info.width();
    let mask_h = mask_info.height();
    let mask_has_bounds = !mask_info.disabled && mask_w > 0 && mask_h > 0;
    if !mask_has_bounds {
        return Ok(None);
    }
    // Same oversized-rect guard as the layer rect: skip just this mask
    // (fall back to no mask) rather than erroring out the whole composite.
    if !dimensions_within_limit(mask_w, mask_h) {
        log::debug!(
            "PSD/PSB layer mask rect {mask_w}x{mask_h} exceeds dimension/pixel \
             limit (max side {}, max pixels {MAX_LAYER_PIXELS}), skipping mask",
            crate::psb_reader::PSD_MAX_DIMENSION
        );
        return Ok(None);
    }
    let mask_pixels = decode_channel_image(slice, mask_w, mask_h, is_psb, cancel)?;
    Ok(Some(build_layer_sized_mask(
        mask_info,
        &mask_pixels,
        layer_left,
        layer_top,
        layer_w,
        layer_h,
    )))
}

/// Blit a decoded mask (its own `mask_info` rect, which may differ from the
/// layer's rect in size and/or offset) into a layer-sized alpha-multiplier
/// buffer. Layer pixels outside the mask's rect use `mask_info.default_color`
/// (the standard PSD convention for "area not covered by the mask").
fn build_layer_sized_mask(
    mask_info: &LayerMaskInfo,
    mask_pixels: &[u8],
    layer_left: i32,
    layer_top: i32,
    layer_w: u32,
    layer_h: u32,
) -> Vec<u8> {
    let Some(pixel_count) = checked_layer_pixel_count(layer_w, layer_h) else {
        return Vec::new();
    };
    let mut out = vec![mask_info.default_color; pixel_count];
    let mask_w = mask_info.width() as i64;
    let mask_h = mask_info.height() as i64;
    if mask_w == 0 || mask_h == 0 {
        return out;
    }

    let off_x = mask_info.left as i64 - layer_left as i64;
    let off_y = mask_info.top as i64 - layer_top as i64;
    let dst_x0 = off_x.max(0);
    let dst_y0 = off_y.max(0);
    let dst_x1 = (off_x + mask_w).min(layer_w as i64);
    let dst_y1 = (off_y + mask_h).min(layer_h as i64);
    if dst_x0 >= dst_x1 || dst_y0 >= dst_y1 {
        return out;
    }

    for dy in dst_y0..dst_y1 {
        let sy = (dy - off_y) as usize;
        let dst_row_start = dy as usize * layer_w as usize;
        let src_row_start = sy * mask_w as usize;
        for dx in dst_x0..dst_x1 {
            let sx = (dx - off_x) as usize;
            out[dst_row_start + dx as usize] = mask_pixels[src_row_start + sx];
        }
    }

    out
}

// -- Blend modes -----------------------------------------------------------

fn separable_blend_kind(
    blend: &[u8; 4],
) -> Option<crate::psb_layer_blend_simd::SeparableBlendKind> {
    use crate::psb_layer_blend_simd::SeparableBlendKind;
    match blend {
        b"norm" => Some(SeparableBlendKind::Normal),
        b"scrn" => Some(SeparableBlendKind::Screen),
        b"lddg" => Some(SeparableBlendKind::LinearDodge),
        b"mul " => Some(SeparableBlendKind::Multiply),
        _ => None,
    }
}

fn blend_mode_supported(blend: &[u8; 4]) -> bool {
    separable_blend_kind(blend).is_some()
}

/// Straight-alpha separable blend of `layer_rgba` onto `canvas` (PDF formula).
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn blend_separable_onto(
    canvas: &mut [u8],
    canvas_w: u32,
    canvas_h: u32,
    layer_rgba: &[u8],
    left: i32,
    top: i32,
    lw: u32,
    lh: u32,
    kind: crate::psb_layer_blend_simd::SeparableBlendKind,
) {
    if lw == 0 || lh == 0 || canvas_w == 0 || canvas_h == 0 {
        return;
    }

    let canvas_w_i = canvas_w as i64;
    let canvas_h_i = canvas_h as i64;
    let left = left as i64;
    let top = top as i64;
    let lw_i = lw as i64;
    let lh_i = lh as i64;

    let src_x0 = (-left).max(0);
    let src_y0 = (-top).max(0);
    let src_x1 = (canvas_w_i - left).min(lw_i);
    let src_y1 = (canvas_h_i - top).min(lh_i);
    if src_x0 >= src_x1 || src_y0 >= src_y1 {
        return;
    }

    let span_w = (src_x1 - src_x0) as usize;
    let span_bytes = span_w * 4;
    for sy in src_y0..src_y1 {
        let dy = (top + sy) as usize;
        let dx0 = (left + src_x0) as usize;
        let d_off = dy * canvas_w as usize * 4 + dx0 * 4;
        let s_off = sy as usize * lw as usize * 4 + src_x0 as usize * 4;
        crate::psb_layer_blend_simd::blend_separable_span(
            &mut canvas[d_off..d_off + span_bytes],
            &layer_rgba[s_off..s_off + span_bytes],
            kind,
        );
    }
}

/// Straight-alpha src-over convenience used by unit tests (Normal = B(Cb,Cs)=Cs).
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn blend_normal_onto(
    canvas: &mut [u8],
    canvas_w: u32,
    canvas_h: u32,
    layer_rgba: &[u8],
    left: i32,
    top: i32,
    lw: u32,
    lh: u32,
) {
    blend_separable_onto(
        canvas,
        canvas_w,
        canvas_h,
        layer_rgba,
        left,
        top,
        lw,
        lh,
        crate::psb_layer_blend_simd::SeparableBlendKind::Normal,
    );
}

/// Dispatch by PSD blend-mode key; unknown modes fall back to Normal (logged once).
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn blend_layer_onto(
    canvas: &mut [u8],
    canvas_w: u32,
    canvas_h: u32,
    layer_rgba: &[u8],
    left: i32,
    top: i32,
    lw: u32,
    lh: u32,
    blend: &[u8; 4],
) {
    let kind = match separable_blend_kind(blend) {
        Some(k) => k,
        None => {
            log_unsupported_blend_once(blend);
            crate::psb_layer_blend_simd::SeparableBlendKind::Normal
        }
    };
    blend_separable_onto(
        canvas, canvas_w, canvas_h, layer_rgba, left, top, lw, lh, kind,
    );
}

// -- Group visibility ------------------------------------------------------

/// Compute per-record effective visibility: a layer is visible only if it
/// and every ancestor group is visible.
///
/// Photoshop stores layer records **bottom to top** in the file (index 0 is
/// the bottommost layer, the last index is the topmost). A group's lsct
/// bounding section divider (type 3, hidden in the UI) is therefore its
/// *first* record in file order (the bottom of the group), while the actual
/// folder record (type 1 open / type 2 closed, carrying the group's own
/// hidden flag) is its *last* record in file order (the top of the group).
///
/// So this walks records in **reverse** (top to bottom, visually): the
/// folder record is seen first and pushes a nested visibility scope (using
/// the group's own hidden flag, which is only known at that point), and the
/// bounding divider is seen last and pops it.
fn compute_effective_visibility(records: &[LayerRecord]) -> Vec<bool> {
    let mut visible = vec![false; records.len()];
    let mut stack: Vec<bool> = vec![true];

    for (i, layer) in records.iter().enumerate().rev() {
        let self_visible = !layer.is_hidden();
        let current = *stack.last().unwrap_or(&true) && self_visible;
        visible[i] = current;

        if layer.is_section_divider {
            match layer.section_type {
                Some(1) | Some(2) => stack.push(current),
                Some(3) if stack.len() > 1 => {
                    stack.pop();
                }
                _ => {}
            }
        }
    }

    visible
}

/// True when strict visibility yields at least one pixel layer that can affect
/// the canvas (flag + geometry only; no pixel sampling).
fn strict_visibility_has_drawable_output(
    canvas_w: u32,
    canvas_h: u32,
    records: &[LayerRecord],
    visible: &[bool],
) -> bool {
    if visible.len() != records.len() || canvas_w == 0 || canvas_h == 0 {
        return false;
    }
    let canvas_l = 0i64;
    let canvas_t = 0i64;
    let canvas_r = i64::from(canvas_w);
    let canvas_b = i64::from(canvas_h);

    for (i, record) in records.iter().enumerate() {
        if !visible[i] || record.is_section_divider || record.opacity == 0 {
            continue;
        }
        if record.is_empty_bounds() {
            continue;
        }
        // Present mask with empty bounds produces no output.
        if let Some(mask) = &record.mask
            && !mask.disabled
            && mask.is_empty_bounds()
        {
            continue;
        }
        let l = i64::from(record.left).max(canvas_l);
        let t = i64::from(record.top).max(canvas_t);
        let r = i64::from(record.right).min(canvas_r);
        let b = i64::from(record.bottom).min(canvas_b);
        if r > l && b > t {
            return true;
        }
    }
    false
}

/// Log an unsupported blend-mode key once (unsupported modes fall back to Normal).
fn log_unsupported_blend_once(blend: &[u8; 4]) {
    static SEEN: std::sync::OnceLock<parking_lot::Mutex<std::collections::HashSet<[u8; 4]>>> =
        std::sync::OnceLock::new();
    let seen = SEEN.get_or_init(|| parking_lot::Mutex::new(std::collections::HashSet::new()));
    let mut seen = seen.lock();
    if seen.insert(*blend) {
        let key = String::from_utf8_lossy(blend).into_owned();
        log::debug!("PSD/PSB layer composite: unsupported blend mode '{key}', treating as Normal");
    }
}

// -- Full composite ---------------------------------------------------------

struct DecodedLayer {
    left: i32,
    top: i32,
    width: u32,
    height: u32,
    blend: [u8; 4],
    /// 0 = base / unclipped; non-zero = clipped to nearest base below.
    clipping: u8,
    rgba: Vec<u8>,
}

struct LayerDecodeParams<'a> {
    color_mode: u16,
    is_psb: bool,
    should_decode: bool,
    cancel: Option<&'a std::sync::atomic::AtomicBool>,
    cmyk_icc: &'a [u8],
}

/// Decode one layer's channels from `channel_data[*cursor..]`, advancing `*cursor`
/// past every channel regardless of `should_decode` so later layers stay aligned.
fn decode_one_layer(
    channel_data: &[u8],
    cursor: &mut usize,
    record: &LayerRecord,
    params: &LayerDecodeParams<'_>,
) -> Result<Option<DecodedLayer>, crate::loader::DecodeError> {
    let width = record.width();
    let height = record.height();
    let has_bounds = params.should_decode && width > 0 && height > 0;
    // Treat an oversized layer rect as corrupt *for that layer only*: skip
    // decoding it (still advancing `cursor` past its channel bytes below so
    // later layers stay aligned) rather than erroring out the whole
    // composite, since every other layer in the file may well be fine.
    if has_bounds && !dimensions_within_limit(width, height) {
        log::debug!(
            "PSD/PSB layer rect {width}x{height} exceeds dimension/pixel limit \
             (max side {}, max pixels {MAX_LAYER_PIXELS}), skipping layer",
            crate::psb_reader::PSD_MAX_DIMENSION
        );
    }
    let can_decode = has_bounds && dimensions_within_limit(width, height);

    let mut color: [Option<Vec<u8>>; 4] = [None, None, None, None];
    let mut alpha: Option<Vec<u8>> = None;
    let mut mask: Option<Vec<u8>> = None;

    for ch in &record.channels {
        let data_len = ch.data_len as usize;
        let start = *cursor;
        let end = start
            .checked_add(data_len)
            .ok_or_else(|| "PSD/PSB layer channel data length overflow".to_string())?;
        let slice = channel_data
            .get(start..end)
            .ok_or_else(|| "PSD/PSB layer channel data out of bounds".to_string())?;
        *cursor = end;

        if !can_decode {
            continue;
        }

        match ch.id {
            -1 => match decode_channel_image(slice, width, height, params.is_psb, params.cancel) {
                Ok(data) => alpha = Some(data),
                Err(e) if e.is_cancelled() => return Err(e),
                Err(e) => log::debug!("PSD/PSB layer alpha channel decode failed: {e}"),
            },
            -2 | -3 => {
                // Channel -2 = user mask; -3 = real user mask (combined
                // vector+user). Prefer -3 when both are present: it is the
                // authoritative rendered mask. Geometry comes from
                // `real_mask` for -3 when parsed, otherwise the user-mask
                // rect. Missing/disabled/oversized rects skip that channel.
                let mask_info = if ch.id == -3 {
                    record.real_mask.as_ref().or(record.mask.as_ref())
                } else if record.real_mask.is_some() && record.channels.iter().any(|c| c.id == -3) {
                    // User mask is superseded by a real user mask channel.
                    None
                } else {
                    record.mask.as_ref()
                };
                if let Some(mask_info) = mask_info {
                    match decode_mask_channel_to_layer(
                        slice,
                        mask_info,
                        record.left,
                        record.top,
                        width,
                        height,
                        params.is_psb,
                        params.cancel,
                    ) {
                        Ok(Some(layer_mask)) => mask = Some(layer_mask),
                        Ok(None) => {}
                        Err(e) if e.is_cancelled() => return Err(e),
                        Err(e) => {
                            log::debug!("PSD/PSB layer mask channel {} decode failed: {e}", ch.id);
                        }
                    }
                }
            }
            0..=3 => {
                let idx = ch.id as usize;
                match decode_channel_image(slice, width, height, params.is_psb, params.cancel) {
                    Ok(data) => color[idx] = Some(data),
                    Err(e) if e.is_cancelled() => return Err(e),
                    Err(e) => log::debug!("PSD/PSB layer color channel {idx} decode failed: {e}"),
                }
            }
            _ => {}
        }
    }

    if !can_decode {
        return Ok(None);
    }

    if !blend_mode_supported(&record.blend) {
        log_unsupported_blend_once(&record.blend);
    }

    let rgba = layer_to_rgba8(LayerRgbaArgs {
        color_mode: params.color_mode,
        width,
        height,
        color: &color,
        alpha: alpha.as_deref(),
        mask: mask.as_deref(),
        opacity: record.opacity,
        cmyk_icc: params.cmyk_icc,
    });

    Ok(Some(DecodedLayer {
        left: record.left,
        top: record.top,
        width,
        height,
        blend: record.blend,
        clipping: record.clipping,
        rgba,
    }))
}

/// Display text for [`crate::loader::DecodeError::NoDrawableVisibleLayers`].
pub use crate::loader::STRICT_LAYER_COMPOSITE_BLANK;

/// Decode a PSD/PSB layer stack and composite it into a single RGBA8 canvas
/// (depth 8 only: Normal / Screen / Linear Dodge / Multiply + opacity + user
/// mask + clipping groups + strict group/leaf visibility).
///
/// When `gpu` is provided, the canvas is large enough, every decoded layer
/// uses Normal blend, and none are clipped, blending may run on an offscreen
/// wgpu compute path; failures, non-Normal stacks, or clipping fall back to CPU.
///
/// Returns [`crate::loader::DecodeError::NoDrawableVisibleLayers`] when no
/// visible layer intersects the canvas (no pixel work is performed).
pub fn composite_layers_from_bytes_with_cancel(
    bytes: &[u8],
    cancel: Option<&std::sync::atomic::AtomicBool>,
    gpu: Option<&crate::psb_layer_blend_gpu::PsdGpuContext>,
) -> Result<crate::psb_reader::PsbComposite, crate::loader::DecodeError> {
    let total_t0 = std::time::Instant::now();
    crate::psb_reader::check_decode_cancel(cancel)?;
    let parse_t0 = std::time::Instant::now();
    let info = parse_layer_records(bytes)?;
    let parse_ms = parse_t0.elapsed().as_secs_f64() * 1000.0;
    composite_layers_from_info(info, parse_ms, total_t0, cancel, gpu)
}

/// Same as [`composite_layers_from_bytes_with_cancel`], but reuses an
/// already-parsed [`crate::psb_section_index::PsdSectionIndex`] instead of
/// re-walking the header/color-mode/image-resources/layer-mask sections.
pub fn composite_layers_from_index(
    index: &crate::psb_section_index::PsdSectionIndex,
    bytes: &[u8],
    cancel: Option<&std::sync::atomic::AtomicBool>,
    gpu: Option<&crate::psb_layer_blend_gpu::PsdGpuContext>,
) -> Result<crate::psb_reader::PsbComposite, crate::loader::DecodeError> {
    let total_t0 = std::time::Instant::now();
    crate::psb_reader::check_decode_cancel(cancel)?;
    let parse_t0 = std::time::Instant::now();
    let info = parse_layer_records_from_index(index, bytes)?;
    let parse_ms = parse_t0.elapsed().as_secs_f64() * 1000.0;
    composite_layers_from_info(info, parse_ms, total_t0, cancel, gpu)
}

fn composite_layers_from_info(
    info: LayerInfo<'_>,
    parse_ms: f64,
    total_t0: std::time::Instant,
    cancel: Option<&std::sync::atomic::AtomicBool>,
    gpu: Option<&crate::psb_layer_blend_gpu::PsdGpuContext>,
) -> Result<crate::psb_reader::PsbComposite, crate::loader::DecodeError> {
    if info.depth != 8 {
        return Err(format!(
            "PSD/PSB layer composite requires 8-bit depth (found {}-bit)",
            info.depth
        )
        .into());
    }

    let canvas_w = info.width;
    let canvas_h = info.height;
    let visible = compute_effective_visibility(&info.records);
    if !strict_visibility_has_drawable_output(canvas_w, canvas_h, &info.records, &visible) {
        return Err(crate::loader::DecodeError::NoDrawableVisibleLayers);
    }

    let canvas_len = (canvas_w as usize)
        .checked_mul(canvas_h as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| "PSD/PSB layer composite canvas size overflow".to_string())?;
    // CMYK documents composite over white paper in Photoshop; starting from
    // transparent black leaves unpainted holes looking like a dark/black page.
    let mut canvas = allocate_composite_canvas(canvas_len, info.color_mode);

    let mut timing = CompositeTiming {
        parse_ms,
        unpack_ms: 0.0,
        cmyk_ms: 0.0,
        blend_ms: 0.0,
        readback_ms: 0.0,
        mode: "cpu",
        layers: 0,
    };

    run_composite_pass(
        &info,
        &visible,
        &mut canvas,
        canvas_w,
        canvas_h,
        cancel,
        gpu,
        &mut timing,
    )?;

    let total_ms = total_t0.elapsed().as_secs_f64() * 1000.0;
    #[cfg(feature = "preload-debug")]
    crate::preload_debug!(
        "[PreloadDebug][PsdComposite] mode={} parse_ms={:.1} unpack_ms={:.1} cmyk_ms={:.1} \
         blend_ms={:.1} readback_ms={:.1} total_ms={:.1} layers={} {}x{}",
        timing.mode,
        timing.parse_ms,
        timing.unpack_ms,
        timing.cmyk_ms,
        timing.blend_ms,
        timing.readback_ms,
        total_ms,
        timing.layers,
        canvas_w,
        canvas_h
    );
    #[cfg(not(feature = "preload-debug"))]
    let _ = (total_ms, &timing);

    Ok(crate::psb_reader::PsbComposite {
        width: canvas_w,
        height: canvas_h,
        pixels: canvas,
    })
}

struct CompositeTiming {
    #[cfg_attr(not(feature = "preload-debug"), allow(dead_code))]
    parse_ms: f64,
    #[cfg_attr(not(feature = "preload-debug"), allow(dead_code))]
    unpack_ms: f64,
    #[cfg_attr(not(feature = "preload-debug"), allow(dead_code))]
    cmyk_ms: f64,
    #[cfg_attr(not(feature = "preload-debug"), allow(dead_code))]
    blend_ms: f64,
    #[cfg_attr(not(feature = "preload-debug"), allow(dead_code))]
    readback_ms: f64,
    #[cfg_attr(not(feature = "preload-debug"), allow(dead_code))]
    mode: &'static str,
    #[cfg_attr(not(feature = "preload-debug"), allow(dead_code))]
    layers: usize,
}

fn allocate_composite_canvas(len: usize, color_mode: u16) -> Vec<u8> {
    let mut canvas = vec![0u8; len];
    clear_composite_canvas(&mut canvas, color_mode);
    canvas
}

fn clear_composite_canvas(canvas: &mut [u8], color_mode: u16) {
    if color_mode == 4 {
        // CMYK paper white is 255 per channel; SIMD fill beats per-pixel scalar stores.
        crate::psb_packbits_simd::fill_bytes(canvas, 255);
    } else {
        canvas.fill(0);
    }
}

/// Precompute each layer's `[start, end)` byte range in the contiguous channel
/// image data block. Validates bounds once so parallel workers can slice
/// independently without a shared cursor.
fn layer_channel_byte_ranges(
    records: &[LayerRecord],
    channel_data_len: usize,
) -> Result<Vec<(usize, usize)>, crate::loader::DecodeError> {
    let mut ranges = Vec::with_capacity(records.len());
    let mut cursor = 0usize;
    for record in records {
        let start = cursor;
        for ch in &record.channels {
            cursor = cursor
                .checked_add(ch.data_len as usize)
                .ok_or_else(|| "PSD/PSB layer channel data length overflow".to_string())?;
        }
        if cursor > channel_data_len {
            return Err("PSD/PSB layer channel data out of bounds".into());
        }
        ranges.push((start, cursor));
    }
    Ok(ranges)
}

/// Decode every eligible visible layer (optionally in parallel). Blend order
/// is preserved: results are collected in record order, skipping layers that
/// decode to `None`.
fn decode_layers_for_composite(
    info: &LayerInfo<'_>,
    visible: &[bool],
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<Vec<DecodedLayer>, crate::loader::DecodeError> {
    let ranges = layer_channel_byte_ranges(&info.records, info.channel_data.len())?;
    // Fail fast on total decoded RGBA footprint before parallel alloc.
    let mut decoded_bytes = 0u64;
    for (i, record) in info.records.iter().enumerate() {
        let should_decode = visible.get(i).copied().unwrap_or(false)
            && !record.is_section_divider
            && !record.is_empty_bounds()
            && record.opacity > 0;
        if !should_decode {
            continue;
        }
        let width = record.width();
        let height = record.height();
        if !dimensions_within_limit(width, height) {
            continue;
        }
        decoded_bytes = accumulate_decoded_layer_bytes(decoded_bytes, width, height)?;
    }
    let decode_at = |i: usize,
                     record: &LayerRecord|
     -> Result<Option<DecodedLayer>, crate::loader::DecodeError> {
        crate::psb_reader::check_decode_cancel(cancel)?;
        let should_decode = visible[i]
            && !record.is_section_divider
            && !record.is_empty_bounds()
            && record.opacity > 0;
        let (start, end) = ranges[i];
        let mut cursor = 0usize;
        decode_one_layer(
            &info.channel_data[start..end],
            &mut cursor,
            record,
            &LayerDecodeParams {
                color_mode: info.color_mode,
                is_psb: info.is_psb,
                should_decode,
                cancel,
                cmyk_icc: info.cmyk_icc.as_slice(),
            },
        )
    };

    if info.records.len() >= crate::psb_layer_decode_pool::PARALLEL_LAYER_DECODE_MIN {
        // Dedicated pool (capped at 2-4 workers): do not nest into img-loader /
        // refinement / strip pools via bare `par_iter`.
        use rayon::prelude::*;
        let results: Vec<Result<Option<DecodedLayer>, crate::loader::DecodeError>> =
            crate::psb_layer_decode_pool::PSD_LAYER_DECODE_POOL.install(|| {
                info.records
                    .par_iter()
                    .enumerate()
                    .map(|(i, record)| decode_at(i, record))
                    .collect()
            });
        let mut layers = Vec::new();
        for result in results {
            if let Some(layer) = result? {
                layers.push(layer);
            }
        }
        return Ok(layers);
    }

    let mut layers = Vec::new();
    for (i, record) in info.records.iter().enumerate() {
        if let Some(layer) = decode_at(i, record)? {
            layers.push(layer);
        }
    }
    Ok(layers)
}

/// Decode and blend every eligible visible layer bottom to top, returning how
/// many were actually composited.
#[allow(clippy::too_many_arguments)]
fn run_composite_pass(
    info: &LayerInfo<'_>,
    visible: &[bool],
    canvas: &mut Vec<u8>,
    canvas_w: u32,
    canvas_h: u32,
    cancel: Option<&std::sync::atomic::AtomicBool>,
    gpu: Option<&crate::psb_layer_blend_gpu::PsdGpuContext>,
    timing: &mut CompositeTiming,
) -> Result<usize, crate::loader::DecodeError> {
    // Layer records and channel_data are both stored bottom to top (index 0
    // is the bottommost layer). Decoding may run in parallel once per-layer
    // byte ranges are known; blending still walks decoded layers bottom to top.
    let decode_t0 = std::time::Instant::now();
    let layers = decode_layers_for_composite(info, visible, cancel)?;
    // Decode includes PackBits + planar convert + CMYK/ICC; split CMS later if needed.
    timing.unpack_ms += decode_t0.elapsed().as_secs_f64() * 1000.0;
    timing.layers = layers.len();

    if layers.is_empty() {
        return Ok(0);
    }

    let blend_t0 = std::time::Instant::now();
    let clip_refs: Vec<crate::psb_layer_clip::ClipLayerRef<'_>> = layers
        .iter()
        .map(|l| crate::psb_layer_clip::ClipLayerRef {
            left: l.left,
            top: l.top,
            width: l.width,
            height: l.height,
            blend: l.blend,
            clipping: l.clipping,
            rgba: &l.rgba,
        })
        .collect();
    let all_normal = layers.iter().all(|l| l.blend == *b"norm");
    let has_clipping = crate::psb_layer_clip::any_layer_clipped(&clip_refs);
    let used_gpu = if let Some(gpu_ctx) = gpu {
        if !all_normal || has_clipping {
            false
        } else {
            let layer_refs: Vec<crate::psb_layer_blend_gpu::DecodedLayerRef<'_>> = layers
                .iter()
                .map(|l| crate::psb_layer_blend_gpu::DecodedLayerRef {
                    left: l.left,
                    top: l.top,
                    width: l.width,
                    height: l.height,
                    rgba: &l.rgba,
                })
                .collect();
            let readback_t0 = std::time::Instant::now();
            if let Some(gpu_pixels) = crate::psb_layer_blend_gpu::try_blend_layers_gpu(
                gpu_ctx,
                canvas_w,
                canvas_h,
                canvas,
                &layer_refs,
                cancel,
            ) {
                timing.readback_ms += readback_t0.elapsed().as_secs_f64() * 1000.0;
                // Take ownership of the GPU readback buffer (no full-canvas copy).
                *canvas = gpu_pixels;
                timing.mode = "gpu";
                true
            } else {
                false
            }
        }
    } else {
        false
    };

    if !used_gpu {
        crate::psb_layer_clip::blend_layers_with_clipping(
            canvas, canvas_w, canvas_h, &clip_refs, cancel,
        )?;
        timing.mode = "cpu";
    }
    timing.blend_ms += blend_t0.elapsed().as_secs_f64() * 1000.0;
    // For GPU, blend_ms includes upload+dispatch+readback; readback_ms is nested.
    // Prefer reporting GPU wall as blend_ms and keep readback as a subset hint.
    Ok(layers.len())
}

#[cfg(test)]
mod tests {
    use super::{
        LayerChannel, LayerDecodeParams, LayerMaskInfo, LayerRecord, LayerRgbaArgs,
        STRICT_LAYER_COMPOSITE_BLANK, blend_layer_onto, blend_normal_onto, blend_separable_onto,
        build_layer_sized_mask, checked_layer_pixel_count, composite_layers_from_bytes_with_cancel,
        compute_effective_visibility, decode_channel_image, decode_one_layer,
        dimensions_within_limit, layer_to_rgba8, parse_layer_records, scan_extra_tagged_blocks,
        strict_visibility_has_drawable_output,
    };
    use crate::psb_layer_blend_simd::SeparableBlendKind;
    use std::path::Path;

    /// Build a minimal `LayerRecord` for `compute_effective_visibility` tests;
    /// only `flags` (hidden bit), `is_section_divider`, and `section_type`
    /// matter for that function.
    fn mk_layer(hidden: bool, is_section_divider: bool, section_type: Option<u32>) -> LayerRecord {
        LayerRecord {
            top: 0,
            left: 0,
            bottom: 1,
            right: 1,
            channels: Vec::new(),
            blend: *b"norm",
            opacity: 255,
            clipping: 0,
            flags: if hidden { 2 } else { 0 },
            mask_size: 0,
            mask: None,
            real_mask: None,
            is_section_divider,
            section_type,
        }
    }

    #[test]
    fn dimensions_within_limit_rejects_oversized_dimensions() {
        assert!(dimensions_within_limit(1, 1));
        assert!(dimensions_within_limit(
            crate::psb_reader::PSD_MAX_DIMENSION,
            1
        ));
        assert!(dimensions_within_limit(
            1,
            crate::psb_reader::PSD_MAX_DIMENSION
        ));
        // Per-side max alone would allow 300k x 300k (~90GB); pixel cap must reject it.
        assert!(!dimensions_within_limit(
            crate::psb_reader::PSD_MAX_DIMENSION,
            crate::psb_reader::PSD_MAX_DIMENSION
        ));
        assert!(!dimensions_within_limit(
            crate::psb_reader::PSD_MAX_DIMENSION + 1,
            1
        ));
        assert!(!dimensions_within_limit(
            1,
            crate::psb_reader::PSD_MAX_DIMENSION + 1
        ));
        assert!(!dimensions_within_limit(u32::MAX, u32::MAX));
        // Pixel budget is exactly 32768^2 (= 1G); one pixel over must fail.
        assert!(dimensions_within_limit(32_768, 32_768));
        assert!(!dimensions_within_limit(32_769, 32_769));
    }

    #[test]
    fn max_layer_records_constant_is_sane() {
        // i16::unsigned_abs max is 65535; our DoS cap must be tighter and
        // still allow complex legitimate comps.
        const {
            assert!(super::MAX_LAYER_RECORDS < 65_535);
            assert!(super::MAX_LAYER_RECORDS >= 1024);
        }
    }

    #[test]
    fn checked_layer_pixel_count_uses_checked_mul() {
        // Defense in depth for decode_channel_image: do not rely solely on
        // upstream dimensions_within_limit.
        assert_eq!(checked_layer_pixel_count(2, 3), Some(6));
        assert!(checked_layer_pixel_count(u32::MAX, u32::MAX).is_none());
        assert!(checked_layer_pixel_count(32_769, 32_769).is_none());
    }

    #[test]
    fn decode_channel_image_rejects_oversized_dims() {
        let data = [0u8, 0u8]; // compression = Raw
        let err =
            decode_channel_image(&data, u32::MAX, u32::MAX, false, None).expect_err("oversized");
        assert!(
            err.as_str().contains("exceeds limit"),
            "unexpected err: {err}"
        );
    }

    // RED until accumulate_decoded_layer_bytes + MAX_COMPOSITE_DECODED_BYTES land.
    #[test]
    fn composite_decoded_byte_budget_rejects_many_large_layers() {
        // Three 32k^2 RGBA layers are 12 GiB; budget must reject before alloc.
        let mut total = 0u64;
        for _ in 0..3 {
            match super::accumulate_decoded_layer_bytes(total, 32_768, 32_768) {
                Ok(next) => total = next,
                Err(e) => {
                    assert!(e.contains("decoded layer byte budget"), "err: {e}");
                    return;
                }
            }
        }
        panic!("expected decoded-layer byte budget to reject");
    }

    #[test]
    fn decode_one_layer_oversized_layer_rect_is_skipped() {
        // A malicious/malformed layer record claiming an absurd width would
        // otherwise make `decode_channel_image` try to `vec![0u8; w * h]`,
        // risking an allocation-failure abort. It must be skipped instead.
        let mut record = mk_layer(false, false, None);
        record.top = 0;
        record.left = 0;
        record.bottom = 1_000_000_000;
        record.right = 1_000_000_000;
        record.channels = vec![LayerChannel {
            id: -1,
            data_len: 0,
        }];

        let channel_data: [u8; 0] = [];
        let mut cursor = 0usize;
        let result = decode_one_layer(
            &channel_data,
            &mut cursor,
            &record,
            &LayerDecodeParams {
                color_mode: 3,
                is_psb: false,
                should_decode: true,
                cancel: None,
                cmyk_icc: &[],
            },
        );

        assert!(result.is_ok(), "oversized layer must not error out");
        assert!(
            result.unwrap().is_none(),
            "oversized layer must be skipped rather than decoded"
        );
        assert_eq!(cursor, 0, "cursor still advances past the channel bytes");
    }

    #[test]
    fn decode_one_layer_oversized_mask_rect_is_skipped() {
        // Same guard, but for the mask channel's own (potentially
        // independently-sized) rect rather than the layer's rect.
        let mut record = mk_layer(false, false, None);
        record.top = 0;
        record.left = 0;
        record.bottom = 2;
        record.right = 2;
        record.mask = Some(LayerMaskInfo {
            top: 0,
            left: 0,
            bottom: 1_000_000_000,
            right: 1_000_000_000,
            default_color: 0,
            disabled: false,
            has_parameters_applied: false,
        });
        record.channels = vec![LayerChannel {
            id: -2,
            data_len: 2,
        }];

        // Compression = 0 (raw), no pixel bytes follow -- irrelevant since
        // the oversized mask rect must be rejected before any read/alloc.
        let channel_data = [0u8, 0u8];
        let mut cursor = 0usize;
        let result = decode_one_layer(
            &channel_data,
            &mut cursor,
            &record,
            &LayerDecodeParams {
                color_mode: 3,
                is_psb: false,
                should_decode: true,
                cancel: None,
                cmyk_icc: &[],
            },
        );

        assert!(result.is_ok());
        let layer = result.unwrap().expect("layer rect itself is valid");
        assert_eq!(layer.width, 2);
        assert_eq!(layer.height, 2);
        // No mask could be decoded, so alpha defaults to fully opaque (255)
        // via `layer_to_rgba8`'s `unwrap_or(255)` fallback for a missing mask.
        assert!(layer.rgba.chunks_exact(4).all(|px| px[3] == 255));
    }

    #[test]
    fn blend_normal_onto_2x2_straight_alpha() {
        // Opaque red covers the top-left pixel; 50% green partially covers top-right;
        // the bottom row of the layer is fully transparent and must not touch the canvas.
        let mut canvas = vec![
            10, 10, 10, 255, // (0,0)
            20, 20, 20, 255, // (1,0)
            30, 30, 30, 255, // (0,1)
            40, 40, 40, 255, // (1,1)
        ];
        let layer = vec![
            255, 0, 0, 255, // (0,0) opaque red
            0, 255, 0, 128, // (1,0) 50% green
            0, 0, 0, 0, // (0,1) transparent
            0, 0, 0, 0, // (1,1) transparent
        ];

        blend_normal_onto(&mut canvas, 2, 2, &layer, 0, 0, 2, 2);

        assert_eq!(&canvas[0..4], &[255, 0, 0, 255]);
        assert_eq!(&canvas[8..12], &[30, 30, 30, 255]);
        assert_eq!(&canvas[12..16], &[40, 40, 40, 255]);

        // (1,0): green over gray20 at 50% alpha, straight-alpha src-over.
        let blended = &canvas[4..8];
        assert_eq!(blended[3], 255);
        assert_eq!(blended[0], 10); // (0*128 + 20*255*127/255) / 255 ~= 10
        assert_eq!(blended[1], 138); // (255*128 + 20*127) / 255 ~= 138
        assert_eq!(blended[2], 10);
    }

    #[test]
    fn blend_normal_onto_clips_to_canvas() {
        // A 3x3 opaque white layer at (1,1) only overlaps the canvas's bottom-right pixel.
        let mut canvas = vec![0u8; 2 * 2 * 4];
        let layer = vec![255u8; 3 * 3 * 4];
        blend_normal_onto(&mut canvas, 2, 2, &layer, 1, 1, 3, 3);
        assert_eq!(&canvas[0..12], &[0u8; 12]);
        assert_eq!(&canvas[12..16], &[255, 255, 255, 255]);
    }

    #[test]
    fn blend_screen_opaque_black_preserves_backdrop() {
        // Screen light-effect layers are often black + bright flare; black must
        // not paint an opaque rectangle (the Normal-fallback bug).
        let mut canvas = vec![40u8, 80, 120, 255, 40, 80, 120, 255];
        let layer = [0u8, 0, 0, 255, 255, 255, 255, 255];
        blend_separable_onto(
            &mut canvas,
            2,
            1,
            &layer,
            0,
            0,
            2,
            1,
            SeparableBlendKind::Screen,
        );
        assert_eq!(&canvas[0..4], &[40, 80, 120, 255]);
        assert_eq!(&canvas[4..8], &[255, 255, 255, 255]);
    }

    #[test]
    fn blend_layer_onto_dispatches_screen_key() {
        let mut canvas = vec![100u8, 100, 100, 255];
        let layer = [0u8, 0, 0, 255];
        blend_layer_onto(&mut canvas, 1, 1, &layer, 0, 0, 1, 1, b"scrn");
        assert_eq!(&canvas, &[100, 100, 100, 255]);
    }

    #[test]
    fn compute_effective_visibility_leaf_hidden_inside_visible_group() {
        // Records in file (bottom-to-top) order:
        //   0: outer bottom divider (type 3)
        //   1: inner bottom divider (type 3)
        //   2: leaf, hidden, inside inner group
        //   3: inner folder header (type 1), visible
        //   4: leaf, visible, inside outer group but outside inner group
        //   5: outer folder header (type 2), visible
        let records = vec![
            mk_layer(false, true, Some(3)),
            mk_layer(false, true, Some(3)),
            mk_layer(true, false, None),
            mk_layer(false, true, Some(1)),
            mk_layer(false, false, None),
            mk_layer(false, true, Some(2)),
        ];

        let visible = compute_effective_visibility(&records);

        assert!(!visible[2], "leaf's own hidden flag must hide it");
        assert!(
            visible[4],
            "sibling leaf in the same visible group stays visible"
        );
        assert!(visible[3], "inner group header itself is visible");
        assert!(visible[5], "outer group header itself is visible");
    }

    #[test]
    fn compute_effective_visibility_group_hidden_hides_descendants() {
        // Same nesting as above, but the *outer* group is hidden while every
        // leaf/inner-group flag is visible.
        let records = vec![
            mk_layer(false, true, Some(3)),
            mk_layer(false, true, Some(3)),
            mk_layer(false, false, None),
            mk_layer(false, true, Some(1)),
            mk_layer(false, false, None),
            mk_layer(true, true, Some(2)),
        ];

        let strict = compute_effective_visibility(&records);
        assert!(
            !strict[2],
            "strict visibility: leaf inside a hidden ancestor group must be hidden"
        );
        assert!(!strict[4], "strict visibility: sibling leaf also hidden");
        assert!(
            !strict[5],
            "strict visibility: the hidden group header itself is hidden"
        );
    }

    #[test]
    fn strict_visibility_has_drawable_output_rejects_hidden_and_offcanvas() {
        let mut on_canvas = mk_layer(false, false, None);
        on_canvas.left = 0;
        on_canvas.top = 0;
        on_canvas.right = 10;
        on_canvas.bottom = 10;
        let mut off_canvas = mk_layer(false, false, None);
        off_canvas.left = 100;
        off_canvas.top = 100;
        off_canvas.right = 110;
        off_canvas.bottom = 110;
        let mut hidden = mk_layer(true, false, None);
        hidden.left = 0;
        hidden.top = 0;
        hidden.right = 10;
        hidden.bottom = 10;

        let records = vec![on_canvas.clone()];
        let visible = compute_effective_visibility(&records);
        assert!(strict_visibility_has_drawable_output(
            50, 50, &records, &visible
        ));

        let records = vec![off_canvas];
        let visible = compute_effective_visibility(&records);
        assert!(!strict_visibility_has_drawable_output(
            50, 50, &records, &visible
        ));

        let records = vec![hidden];
        let visible = compute_effective_visibility(&records);
        assert!(!strict_visibility_has_drawable_output(
            50, 50, &records, &visible
        ));
    }

    #[test]
    fn compute_effective_visibility_unpaired_divider_does_not_panic() {
        // A lone bounding divider (type 3) with no matching folder header
        // above it must not underflow the visibility stack.
        let records = vec![mk_layer(false, true, Some(3)), mk_layer(false, false, None)];

        let visible = compute_effective_visibility(&records);

        assert_eq!(visible.len(), 2);
        assert!(visible[1], "leaf above the unpaired divider stays visible");
    }

    #[test]
    fn build_layer_sized_mask_smaller_mask_with_offset() {
        // 2x2 mask offset by (1,1) inside a 4x4 layer.
        let mask_info = LayerMaskInfo {
            top: 1,
            left: 1,
            bottom: 3,
            right: 3,
            default_color: 0,
            disabled: false,
            has_parameters_applied: false,
        };
        let mask_pixels = vec![10, 20, 30, 40];

        let out = build_layer_sized_mask(&mask_info, &mask_pixels, 0, 0, 4, 4);

        assert_eq!(out.len(), 16);
        let mut expected = vec![0u8; 16];
        expected[4 + 1] = 10;
        expected[4 + 2] = 20;
        expected[2 * 4 + 1] = 30;
        expected[2 * 4 + 2] = 40;
        assert_eq!(out, expected);
    }

    #[test]
    fn build_layer_sized_mask_default_color_outside_mask_rect() {
        // Mask rect falls entirely outside the layer's bounds, so every
        // output pixel must fall back to `default_color`.
        let mask_info = LayerMaskInfo {
            top: 10,
            left: 10,
            bottom: 11,
            right: 11,
            default_color: 255,
            disabled: false,
            has_parameters_applied: false,
        };
        let mask_pixels = vec![99];

        let out = build_layer_sized_mask(&mask_info, &mask_pixels, 0, 0, 3, 3);

        assert_eq!(out, vec![255u8; 9]);
    }

    #[test]
    fn layer_to_rgba8_cmyk_opacity_mask_numeric() {
        // 1x1 CMYK pixel (Adobe polarity 0=100% ink): c=204, m=153, y=102, k=204.
        // r = 204*204/255 = 163; g = 153*204/255 = 122; b = 102*204/255 = 81.
        let color: [Option<Vec<u8>>; 4] = [
            Some(vec![204]),
            Some(vec![153]),
            Some(vec![102]),
            Some(vec![204]),
        ];
        let alpha = vec![200u8];
        let mask = vec![128u8];

        // Force naive path: invalid ICC makes lcms fail closed to cmyk_to_rgb.
        let rgba = layer_to_rgba8(LayerRgbaArgs {
            color_mode: 4,
            width: 1,
            height: 1,
            color: &color,
            alpha: Some(&alpha),
            mask: Some(&mask),
            opacity: 200,
            cmyk_icc: b"not-icc",
        });

        // a = 200 * 200 / 255 = 156, then 156 * 128 / 255 = 78.
        assert_eq!(rgba, vec![163, 122, 81, 78]);
    }

    #[test]
    fn composite_layers_all_hidden_returns_blank_error() {
        // Two top-level groups, both eye-off: strict composite must not invent
        // visibility and must report blank without pixel work.
        let path = Path::new(r"F:\BaiduNetdiskDownload\素材库\45套 psd企业画册模板\11\11.psd");
        if !path.is_file() {
            eprintln!("skipping composite_layers_all_hidden_returns_blank_error; sample missing");
            return;
        }
        let bytes = std::fs::read(path).expect("read");
        let err = composite_layers_from_bytes_with_cancel(&bytes, None, None)
            .expect_err("expected blank under strict visibility");
        assert!(err.is_no_drawable_visible_layers());
        assert_eq!(err.as_str(), STRICT_LAYER_COMPOSITE_BLANK);
    }

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
    fn scan_lsct_skips_when_payload_truncated() {
        // data_len claims 4 bytes but only 2 remain after the length field.
        let mut block = Vec::new();
        block.extend_from_slice(b"8BIM");
        block.extend_from_slice(b"lsct");
        block.extend_from_slice(&4u32.to_be_bytes());
        block.extend_from_slice(&[0x00, 0x01]); // truncated
        let mut cursor = std::io::Cursor::new(block.as_slice());

        let (is_section_divider, section_type) =
            scan_extra_tagged_blocks(&mut cursor, block.len() as u64, false).unwrap();

        assert!(!is_section_divider);
        assert_eq!(section_type, None);
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
