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

use crate::psb_layer_decode::{
    StreamingPeakTracker, gpu_batch_eligible_decoded_bytes, run_composite_pass_cpu_streaming,
    run_composite_pass_gpu_batch,
};

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
pub(crate) const MAX_LAYER_PIXELS: u64 = 1024 * 1024 * 1024;
/// Cap on the sum of decoded layer RGBA8 buffers held for one composite pass.
///
/// Per-layer pixel caps alone still allow many large layers to be decoded in
/// parallel and retained until blending finishes. 8 GiB bounds that without
/// rejecting typical multi-layer comps on a desktop viewer.
const MAX_COMPOSITE_DECODED_BYTES: u64 = 8 * 1024 * 1024 * 1024;
/// Max [`DecodedLayer`]s resident at once on the CPU streaming composite
/// path: the layer currently being blended, plus the next one prefetched in
/// parallel on [`crate::psb_layer_decode_pool::PSD_LAYER_DECODE_POOL`].
const LAYER_PREFETCH_WINDOW: usize = 2;
// `run_composite_pass_cpu_streaming` overlaps exactly one prefetch with the
// current layer's blend; the design does not support a wider window.
const _: () = assert!(LAYER_PREFETCH_WINDOW == 2);
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
    /// Layer name, preferring the Unicode `luni` tagged block over the Pascal
    /// layer-name fallback in the extra data.
    pub name: String,
    pub layer_id: Option<u32>,
    /// Raw `cmls` descriptor payload found inside `shmd`, retained for the
    /// later Layer Comp descriptor pass.
    pub cmls_payload: Option<Vec<u8>>,
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
pub(crate) fn dimensions_within_limit(width: u32, height: u32) -> bool {
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
pub(crate) fn checked_layer_pixel_count(width: u32, height: u32) -> Option<usize> {
    (width as u64)
        .checked_mul(height as u64)
        .filter(|&n| n <= MAX_LAYER_PIXELS)
        .and_then(|n| usize::try_from(n).ok())
}

/// Byte footprint of a layer's decoded straight-alpha RGBA8 buffer.
fn layer_rgba_byte_len(width: u32, height: u32) -> Option<u64> {
    checked_layer_pixel_count(width, height).map(|pixels| pixels as u64 * 4)
}

/// Add one layer's RGBA8 footprint to `acc`, enforcing
/// [`MAX_COMPOSITE_DECODED_BYTES`].
pub(crate) fn accumulate_decoded_layer_bytes(
    acc: u64,
    width: u32,
    height: u32,
) -> Result<u64, String> {
    let rgba = layer_rgba_byte_len(width, height)
        .ok_or_else(|| format!("PSD/PSB layer channel size {width}x{height} exceeds limit"))?;
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

/// CPU streaming budget check: the layer currently held plus the next one
/// about to be prefetched (the only [`LAYER_PREFETCH_WINDOW`] `DecodedLayer`s
/// ever resident at once) must fit [`MAX_COMPOSITE_DECODED_BYTES`]. Unlike the
/// GPU batch path, this never sums the whole layer stack up front.
pub(crate) fn check_streaming_pair_budget(
    current: &crate::psb_layer_decode::DecodedLayer,
    next_width: u32,
    next_height: u32,
) -> Result<(), crate::loader::DecodeError> {
    let current_bytes = layer_rgba_byte_len(current.width, current.height).unwrap_or(0);
    let Some(next_bytes) = layer_rgba_byte_len(next_width, next_height) else {
        return Ok(());
    };
    let total = current_bytes
        .checked_add(next_bytes)
        .ok_or_else(|| "PSD/PSB decoded layer byte total overflow".to_string())?;
    if total > MAX_COMPOSITE_DECODED_BYTES {
        return Err(format!(
            "PSD/PSB decoded layer byte budget exceeded ({total} > {MAX_COMPOSITE_DECODED_BYTES})"
        )
        .into());
    }
    Ok(())
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
        name,
        layer_id,
        cmls_payload,
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
    name: String,
    layer_id: Option<u32>,
    cmls_payload: Option<Vec<u8>>,
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
            name: String::new(),
            layer_id: None,
            cmls_payload: None,
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

    let pascal_name = read_pascal_name(r, extra_end)?;
    let TaggedBlockScan {
        is_section_divider,
        section_type,
        layer_id,
        unicode_name,
        cmls_payload,
    } = scan_extra_tagged_blocks(r, extra_end, is_psb)?;
    let name = unicode_name.unwrap_or(pascal_name);
    Ok(ParsedLayerExtra {
        mask_size,
        mask,
        real_mask,
        is_section_divider,
        section_type,
        name,
        layer_id,
        cmls_payload,
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
) -> Result<TaggedBlockScan, String> {
    let mut is_section_divider = false;
    let mut section_type = None;
    let mut layer_id = None;
    let mut unicode_name = None;
    let mut cmls_payload = None;
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
    })
}

struct TaggedBlockScan {
    is_section_divider: bool,
    section_type: Option<u32>,
    layer_id: Option<u32>,
    unicode_name: Option<String>,
    cmls_payload: Option<Vec<u8>>,
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

fn find_next_tagged_block_signature(bytes: &[u8], start: usize, limit: usize) -> Option<usize> {
    let mut cursor = start.min(limit);
    while cursor.saturating_add(4) <= limit {
        let offset = memchr::memchr(PSD_BLEND_SIGNATURE[0], &bytes[cursor..limit])?;
        let candidate = cursor + offset;
        if candidate.saturating_add(4) > limit {
            return None;
        }
        let signature = &bytes[candidate..candidate + 4];
        if signature == PSD_BLEND_SIGNATURE || signature == PSB_BLOCK_SIGNATURE {
            return Some(candidate);
        }
        cursor = candidate.saturating_add(1);
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

const TAGGED_BLOCK_MIN_HEADER_LEN: u64 = 12;
const SHMD_ENTRY_HEADER_LEN: usize = 16;
const MAX_TAGGED_BLOCK_RESYNCS_PER_LAYER: u32 = 64;

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
pub(crate) fn compute_effective_visibility(records: &[LayerRecord]) -> Vec<bool> {
    compute_effective_visibility_with_flags(records, None)
}

/// Same as [`compute_effective_visibility`], but optionally overrides each
/// layer's `flags` byte (used by Layer Comp `cmls` without cloning records).
pub(crate) fn compute_effective_visibility_with_flags(
    records: &[LayerRecord],
    flags_override: Option<&[u8]>,
) -> Vec<bool> {
    if let Some(flags) = flags_override {
        debug_assert_eq!(records.len(), flags.len());
    }
    let mut visible = vec![false; records.len()];
    let mut stack: Vec<bool> = vec![true];

    for (i, layer) in records.iter().enumerate().rev() {
        let self_visible = match flags_override {
            Some(flags) => flags.get(i).is_some_and(|f| f & 2 == 0),
            None => !layer.is_hidden(),
        };
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
pub(crate) fn strict_visibility_has_drawable_output(
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

// -- Full composite ---------------------------------------------------------

/// Display text for [`crate::loader::DecodeError::NoDrawableVisibleLayers`].
pub use crate::loader::STRICT_LAYER_COMPOSITE_BLANK;

/// Decode a PSD/PSB layer stack and composite it into a single RGBA8 canvas
/// (depth 8 only: Normal / Screen / Linear Dodge / Multiply + opacity + user
/// mask + clipping groups + strict group/leaf visibility).
///
/// When `gpu` is provided, the canvas is large enough, every decoded layer
/// uses a GPU-separable blend mode, blending may run on an offscreen wgpu
/// compute path; failures or non-separable stacks fall back to CPU.
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
    let visible = compute_effective_visibility(&info.records);
    composite_layers_with_visibility_from_info(&info, &visible, parse_ms, total_t0, cancel, gpu)
}

/// Same as [`composite_layers_from_info`], but takes an explicit per-record
/// `visible` mask instead of deriving it from strict Photoshop layer/group
/// flags via [`compute_effective_visibility`].
///
/// Used by callers that need to override strict flag-based visibility (e.g.
/// a future Layer Comp or max-bounding-box "reveal" pass); ordinary decode
/// paths should go through [`composite_layers_from_info`] instead.
pub(crate) fn composite_layers_with_visibility_from_info(
    info: &LayerInfo<'_>,
    visible: &[bool],
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
    if visible.len() != info.records.len() {
        return Err("PSD/PSB visibility mask length mismatch".into());
    }

    let canvas_w = info.width;
    let canvas_h = info.height;
    if !strict_visibility_has_drawable_output(canvas_w, canvas_h, &info.records, visible) {
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
        info,
        visible,
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

pub(crate) struct CompositeTiming {
    #[cfg_attr(not(feature = "preload-debug"), allow(dead_code))]
    pub(crate) parse_ms: f64,
    #[cfg_attr(not(feature = "preload-debug"), allow(dead_code))]
    pub(crate) unpack_ms: f64,
    #[cfg_attr(not(feature = "preload-debug"), allow(dead_code))]
    pub(crate) cmyk_ms: f64,
    #[cfg_attr(not(feature = "preload-debug"), allow(dead_code))]
    pub(crate) blend_ms: f64,
    #[cfg_attr(not(feature = "preload-debug"), allow(dead_code))]
    pub(crate) readback_ms: f64,
    #[cfg_attr(not(feature = "preload-debug"), allow(dead_code))]
    pub(crate) mode: &'static str,
    #[cfg_attr(not(feature = "preload-debug"), allow(dead_code))]
    pub(crate) layers: usize,
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

/// Whether `record` will actually decode into a
/// [`crate::psb_layer_decode::DecodedLayer`] (mirrors the skip conditions
/// applied in `decode_one_layer`/`decode_at`): visible, not a section
/// divider, non-empty bounds, non-zero opacity, and within the per-layer
/// dimension/pixel cap. Metadata-only -- never touches channel data.
pub(crate) fn layer_will_decode(record: &LayerRecord, visible: bool) -> bool {
    let should_decode =
        visible && !record.is_section_divider && !record.is_empty_bounds() && record.opacity > 0;
    should_decode && dimensions_within_limit(record.width(), record.height())
}

/// Decode and blend every eligible visible layer bottom to top, returning how
/// many were actually composited.
///
/// Dispatches to one of two strategies:
/// - GPU all-at-once batch ([`run_composite_pass_gpu_batch`]): only when a GPU
///   context is available AND [`gpu_batch_eligible_decoded_bytes`] finds every
///   composited layer GPU-separable and within budget, including clipping
///   groups made only from separable blend modes.
/// - CPU streaming ([`run_composite_pass_cpu_streaming`]): the default, and
///   the fallback whenever the GPU batch is not eligible.
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
    let gpu_batch_ctx = gpu.filter(|_| gpu_batch_eligible_decoded_bytes(info, visible).is_some());
    if let Some(gpu_ctx) = gpu_batch_ctx {
        return run_composite_pass_gpu_batch(
            info, visible, canvas, canvas_w, canvas_h, cancel, gpu_ctx, timing,
        );
    }
    let peak_tracker = StreamingPeakTracker::default();
    run_composite_pass_cpu_streaming(
        info,
        visible,
        canvas,
        canvas_w,
        canvas_h,
        cancel,
        timing,
        &peak_tracker,
    )
}

#[cfg(test)]
mod tests {
    use super::{
        CompositeTiming, LAYER_PREFETCH_WINDOW, LayerChannel, LayerInfo, LayerRecord,
        STRICT_LAYER_COMPOSITE_BLANK, StreamingPeakTracker, checked_layer_pixel_count,
        composite_layers_from_bytes_with_cancel, composite_layers_with_visibility_from_info,
        compute_effective_visibility, dimensions_within_limit, gpu_batch_eligible_decoded_bytes,
        layer_will_decode, parse_layer_records, run_composite_pass_cpu_streaming,
        scan_extra_tagged_blocks, strict_visibility_has_drawable_output,
    };
    use std::path::Path;

    fn raw_channel_bytes(pixel: u8, pixel_count: usize) -> Vec<u8> {
        let mut data = vec![0u8, 0u8]; // compression = 0 (raw)
        data.extend(std::iter::repeat_n(pixel, pixel_count));
        data
    }

    /// Minimal spec for a synthetic composite test layer: full RGB channels
    /// (raw/uncompressed) covering `[left, right) x [top, bottom)`, no alpha
    /// or mask channel (alpha defaults to fully opaque).
    struct TestLayerSpec {
        top: i32,
        left: i32,
        bottom: i32,
        right: i32,
        rgb: (u8, u8, u8),
        blend: [u8; 4],
        clipping: u8,
        opacity: u8,
    }

    /// Build `LayerRecord`s + a matching contiguous `channel_data` blob for
    /// [`super::run_composite_pass_cpu_streaming`] /
    /// [`crate::psb_layer_decode::decode_layers_for_composite`] tests, bypassing
    /// the full on-disk PSD byte format.
    fn build_test_layers(specs: &[TestLayerSpec]) -> (Vec<LayerRecord>, Vec<u8>) {
        let mut records = Vec::with_capacity(specs.len());
        let mut channel_data = Vec::new();
        for spec in specs {
            let width = (spec.right - spec.left) as u32;
            let height = (spec.bottom - spec.top) as u32;
            let pixel_count = (width * height) as usize;
            let mut channels = Vec::with_capacity(3);
            for (id, value) in [(0i16, spec.rgb.0), (1, spec.rgb.1), (2, spec.rgb.2)] {
                let bytes = raw_channel_bytes(value, pixel_count);
                channels.push(LayerChannel {
                    id,
                    data_len: bytes.len() as u32,
                });
                channel_data.extend_from_slice(&bytes);
            }
            records.push(LayerRecord {
                top: spec.top,
                left: spec.left,
                bottom: spec.bottom,
                right: spec.right,
                name: String::new(),
                layer_id: None,
                cmls_payload: None,
                channels,
                blend: spec.blend,
                opacity: spec.opacity,
                clipping: spec.clipping,
                flags: 0,
                mask_size: 0,
                mask: None,
                real_mask: None,
                is_section_divider: false,
                section_type: None,
            });
        }
        (records, channel_data)
    }

    fn mk_layer_info(
        width: u32,
        height: u32,
        records: Vec<LayerRecord>,
        channel_data: &[u8],
    ) -> LayerInfo<'_> {
        LayerInfo {
            records,
            channel_data,
            width,
            height,
            depth: 8,
            color_mode: 3,
            is_psb: false,
            cmyk_icc: Vec::new(),
        }
    }

    fn px(canvas: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
        let o = ((y * w + x) * 4) as usize;
        [canvas[o], canvas[o + 1], canvas[o + 2], canvas[o + 3]]
    }

    fn empty_timing() -> CompositeTiming {
        CompositeTiming {
            parse_ms: 0.0,
            unpack_ms: 0.0,
            cmyk_ms: 0.0,
            blend_ms: 0.0,
            readback_ms: 0.0,
            mode: "cpu",
            layers: 0,
        }
    }

    fn push_tagged_block(bytes: &mut Vec<u8>, key: &[u8; 4], payload: &[u8]) {
        bytes.extend_from_slice(b"8BIM");
        bytes.extend_from_slice(key);
        bytes.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        bytes.extend_from_slice(payload);
        if !payload.len().is_multiple_of(2) {
            bytes.push(0);
        }
    }

    fn minimal_psd_with_layer_extra(extra: Vec<u8>) -> Vec<u8> {
        let mut layer_record = Vec::new();
        layer_record.extend_from_slice(&0i32.to_be_bytes()); // top
        layer_record.extend_from_slice(&0i32.to_be_bytes()); // left
        layer_record.extend_from_slice(&1i32.to_be_bytes()); // bottom
        layer_record.extend_from_slice(&1i32.to_be_bytes()); // right
        layer_record.extend_from_slice(&0u16.to_be_bytes()); // channel count
        layer_record.extend_from_slice(b"8BIM");
        layer_record.extend_from_slice(b"norm");
        layer_record.extend_from_slice(&[255, 0, 0, 0]); // opacity, clipping, flags, filler
        layer_record.extend_from_slice(&(extra.len() as u32).to_be_bytes());
        layer_record.extend_from_slice(&extra);

        let mut layer_info = Vec::new();
        layer_info.extend_from_slice(&1i16.to_be_bytes());
        layer_info.extend_from_slice(&layer_record);

        let mut layer_mask_info = Vec::new();
        layer_mask_info.extend_from_slice(&(layer_info.len() as u32).to_be_bytes());
        layer_mask_info.extend_from_slice(&layer_info);

        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"8BPS");
        bytes.extend_from_slice(&1u16.to_be_bytes());
        bytes.extend_from_slice(&[0; 6]);
        bytes.extend_from_slice(&3u16.to_be_bytes());
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.extend_from_slice(&8u16.to_be_bytes());
        bytes.extend_from_slice(&3u16.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes()); // color mode data
        bytes.extend_from_slice(&0u32.to_be_bytes()); // image resources
        bytes.extend_from_slice(&(layer_mask_info.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&layer_mask_info);
        bytes.extend_from_slice(&0u16.to_be_bytes()); // image data compression
        bytes
    }

    fn layer_extra_with_pascal_name(name: &[u8]) -> Vec<u8> {
        let mut extra = Vec::new();
        extra.extend_from_slice(&0u32.to_be_bytes()); // mask data length
        extra.extend_from_slice(&0u32.to_be_bytes()); // blending ranges length
        extra.push(name.len() as u8);
        extra.extend_from_slice(name);
        while extra.len() % 4 != 0 {
            extra.push(0);
        }
        extra
    }

    #[test]
    fn parse_lyid_and_luni_from_extra_block() {
        let mut extra = layer_extra_with_pascal_name(b"A");
        push_tagged_block(&mut extra, b"lyid", &42u32.to_be_bytes());
        let mut luni = Vec::new();
        luni.extend_from_slice(&5u32.to_be_bytes());
        for unit in "Hello".encode_utf16() {
            luni.extend_from_slice(&unit.to_be_bytes());
        }
        push_tagged_block(&mut extra, b"luni", &luni);

        let bytes = minimal_psd_with_layer_extra(extra);
        let info = parse_layer_records(&bytes).expect("parse layers");

        assert_eq!(info.records.len(), 1);
        assert_eq!(info.records[0].layer_id, Some(42));
        assert_eq!(info.records[0].name, "Hello");
    }

    #[test]
    fn parse_shmd_stores_cmls_payload() {
        let cmls_payload = [0, 0, 0, 16, b'c', b'm', b'l', b's'];
        let mut shmd = Vec::new();
        shmd.extend_from_slice(&1u32.to_be_bytes());
        shmd.extend_from_slice(b"8BIM");
        shmd.extend_from_slice(b"cmls");
        shmd.push(1); // copy flag
        shmd.extend_from_slice(&[0; 3]);
        shmd.extend_from_slice(&(cmls_payload.len() as u32).to_be_bytes());
        shmd.extend_from_slice(&cmls_payload);

        let mut extra = layer_extra_with_pascal_name(b"A");
        push_tagged_block(&mut extra, b"shmd", &shmd);

        let bytes = minimal_psd_with_layer_extra(extra);
        let info = parse_layer_records(&bytes).expect("parse layers");

        assert_eq!(
            info.records[0].cmls_payload.as_deref(),
            Some(&cmls_payload[..])
        );
    }

    /// Build a minimal `LayerRecord` for `compute_effective_visibility` tests;
    /// only `flags` (hidden bit), `is_section_divider`, and `section_type`
    /// matter for that function.
    fn mk_layer(hidden: bool, is_section_divider: bool, section_type: Option<u32>) -> LayerRecord {
        LayerRecord {
            top: 0,
            left: 0,
            bottom: 1,
            right: 1,
            name: String::new(),
            layer_id: None,
            cmls_payload: None,
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
    fn composite_with_visibility_forces_hidden_layer_when_mask_says_so() {
        // Both layers are hidden per their on-disk flags, so the default
        // strict-visibility path (`compute_effective_visibility`) would find
        // nothing drawable and return `NoDrawableVisibleLayers`. An explicit
        // `visible` override (as a future Layer Comp / max-bbox reveal pass
        // will supply) must be able to force them on regardless of flags.
        let (width, height) = (2u32, 2u32);
        let specs = [
            TestLayerSpec {
                top: 0,
                left: 0,
                bottom: 2,
                right: 2,
                rgb: (10, 20, 30),
                blend: *b"norm",
                clipping: 0,
                opacity: 255,
            },
            TestLayerSpec {
                top: 0,
                left: 0,
                bottom: 2,
                right: 2,
                rgb: (40, 50, 60),
                blend: *b"norm",
                clipping: 0,
                opacity: 255,
            },
        ];
        let (mut records, channel_data) = build_test_layers(&specs);
        for record in &mut records {
            record.flags = 2; // hidden bit set on every record
        }
        let default_visible = compute_effective_visibility(&records);
        assert!(
            default_visible.iter().all(|v| !v),
            "sanity: default strict visibility must hide every record here"
        );

        let info = mk_layer_info(width, height, records, &channel_data);
        let visible = vec![true, true];

        let composite = composite_layers_with_visibility_from_info(
            &info,
            &visible,
            0.0,
            std::time::Instant::now(),
            None,
            None,
        )
        .expect("explicit visibility override should produce a drawable composite");

        assert_eq!(composite.width, width);
        assert_eq!(composite.height, height);
        // Top (last) opaque layer wins under Normal blend.
        assert_eq!(px(&composite.pixels, width, 0, 0), [40, 50, 60, 255]);
    }

    #[test]
    fn composite_with_visibility_length_mismatch_is_an_error() {
        let (width, height) = (2u32, 2u32);
        let specs = [TestLayerSpec {
            top: 0,
            left: 0,
            bottom: 2,
            right: 2,
            rgb: (1, 2, 3),
            blend: *b"norm",
            clipping: 0,
            opacity: 255,
        }];
        let (records, channel_data) = build_test_layers(&specs);
        let info = mk_layer_info(width, height, records, &channel_data);
        let visible = vec![true, true]; // wrong length: 2 vs 1 record

        let err = composite_layers_with_visibility_from_info(
            &info,
            &visible,
            0.0,
            std::time::Instant::now(),
            None,
            None,
        )
        .expect_err("mismatched visibility length must be rejected");
        assert!(!err.is_no_drawable_visible_layers());
        assert!(err.as_str().contains("visibility"));
    }

    #[test]
    fn psb_8bim_lsct_uses_u32_length() {
        let mut block = Vec::new();
        block.extend_from_slice(b"8BIM");
        block.extend_from_slice(b"lsct");
        block.extend_from_slice(&4u32.to_be_bytes());
        block.extend_from_slice(&2u32.to_be_bytes());
        let mut cursor = std::io::Cursor::new(block.as_slice());

        let scan = scan_extra_tagged_blocks(&mut cursor, block.len() as u64, true).unwrap();

        assert!(scan.is_section_divider);
        assert_eq!(scan.section_type, Some(2));
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

        let scan = scan_extra_tagged_blocks(&mut cursor, block.len() as u64, false).unwrap();

        assert!(!scan.is_section_divider);
        assert_eq!(scan.section_type, None);
    }

    #[test]
    fn scan_extra_finds_lsct_after_garbage() {
        let mut block = Vec::new();
        block.extend_from_slice(b"garbage before signature");
        block.extend_from_slice(b"8BIM");
        block.extend_from_slice(b"lsct");
        block.extend_from_slice(&4u32.to_be_bytes());
        block.extend_from_slice(&3u32.to_be_bytes());
        let mut cursor = std::io::Cursor::new(block.as_slice());

        let scan = scan_extra_tagged_blocks(&mut cursor, block.len() as u64, false).unwrap();

        assert!(scan.is_section_divider);
        assert_eq!(scan.section_type, Some(3));
    }

    #[test]
    fn scan_extra_resync_budget_terminates() {
        let block = vec![0u8; 32 * 1024 * 1024];
        let mut cursor = std::io::Cursor::new(block.as_slice());
        let started = std::time::Instant::now();

        let scan = scan_extra_tagged_blocks(&mut cursor, block.len() as u64, false).unwrap();

        assert!(!scan.is_section_divider);
        assert_eq!(scan.section_type, None);
        assert!(
            started.elapsed() < std::time::Duration::from_millis(500),
            "signature-free scan should finish quickly"
        );
    }

    #[test]
    fn scan_extra_resync_budget_stops_before_late_lsct() {
        let mut block = Vec::new();
        for _ in 0..=super::MAX_TAGGED_BLOCK_RESYNCS_PER_LAYER {
            block.extend_from_slice(b"8BIM");
            block.extend_from_slice(b"junk");
            block.extend_from_slice(&(u32::MAX).to_be_bytes());
        }
        block.extend_from_slice(b"8BIM");
        block.extend_from_slice(b"lsct");
        block.extend_from_slice(&4u32.to_be_bytes());
        block.extend_from_slice(&2u32.to_be_bytes());
        let mut cursor = std::io::Cursor::new(block.as_slice());

        let scan = scan_extra_tagged_blocks(&mut cursor, block.len() as u64, false).unwrap();

        assert!(!scan.is_section_divider);
        assert_eq!(scan.section_type, None);
    }

    #[test]
    fn scan_extra_resync_budget_keeps_existing_lsct() {
        let mut block = Vec::new();
        block.extend_from_slice(b"8BIM");
        block.extend_from_slice(b"lsct");
        block.extend_from_slice(&4u32.to_be_bytes());
        block.extend_from_slice(&1u32.to_be_bytes());
        for _ in 0..=super::MAX_TAGGED_BLOCK_RESYNCS_PER_LAYER {
            block.extend_from_slice(b"8BIM");
            block.extend_from_slice(b"junk");
            block.extend_from_slice(&(u32::MAX).to_be_bytes());
        }
        let mut cursor = std::io::Cursor::new(block.as_slice());

        let scan = scan_extra_tagged_blocks(&mut cursor, block.len() as u64, false).unwrap();

        assert!(scan.is_section_divider);
        assert_eq!(scan.section_type, Some(1));
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

    #[test]
    fn streaming_composite_peak_live_layers_at_most_prefetch_window() {
        let (width, height) = (4u32, 4u32);
        let specs = [
            TestLayerSpec {
                top: 0,
                left: 0,
                bottom: 4,
                right: 4,
                rgb: (255, 0, 0),
                blend: *b"norm",
                clipping: 0,
                opacity: 255,
            },
            TestLayerSpec {
                top: 0,
                left: 0,
                bottom: 4,
                right: 4,
                rgb: (0, 255, 0),
                blend: *b"norm",
                clipping: 0,
                opacity: 255,
            },
            TestLayerSpec {
                top: 0,
                left: 0,
                bottom: 4,
                right: 4,
                rgb: (0, 0, 255),
                blend: *b"norm",
                clipping: 0,
                opacity: 255,
            },
        ];
        let (records, channel_data) = build_test_layers(&specs);
        let visible = vec![true; records.len()];
        let info = mk_layer_info(width, height, records, &channel_data);
        let mut canvas = vec![0u8; (width * height * 4) as usize];
        let mut timing = empty_timing();
        let tracker = StreamingPeakTracker::default();

        let composited = run_composite_pass_cpu_streaming(
            &info,
            &visible,
            &mut canvas,
            width,
            height,
            None,
            &mut timing,
            &tracker,
        )
        .expect("stream composite");

        assert_eq!(composited, 3);
        // Top (last) opaque layer wins under Normal blend.
        assert_eq!(px(&canvas, width, 0, 0), [0, 0, 255, 255]);
        let peak = tracker.peak();
        assert!(
            peak <= LAYER_PREFETCH_WINDOW,
            "peak live decoded layers {peak} exceeded window {LAYER_PREFETCH_WINDOW}"
        );
        assert!(peak >= 1, "expected at least one live layer to be observed");
    }

    #[test]
    fn streaming_composite_skips_zero_opacity_and_invisible_layers() {
        let (width, height) = (2u32, 2u32);
        let full_rect = |rgb: (u8, u8, u8), opacity: u8| TestLayerSpec {
            top: 0,
            left: 0,
            bottom: 2,
            right: 2,
            rgb,
            blend: *b"norm",
            clipping: 0,
            opacity,
        };
        let specs = [
            full_rect((10, 20, 30), 255),
            // Zero opacity -- must be skipped, not painted white.
            full_rect((255, 255, 255), 0),
            // Not visible (e.g. hidden layer/ancestor group, as computed
            // upstream by `compute_effective_visibility`) -- also skipped.
            full_rect((0, 255, 0), 255),
        ];
        let (records, channel_data) = build_test_layers(&specs);
        let visible = vec![true, true, false];
        let info = mk_layer_info(width, height, records, &channel_data);
        let mut canvas = vec![0u8; (width * height * 4) as usize];
        let mut timing = empty_timing();
        let tracker = StreamingPeakTracker::default();

        let composited = run_composite_pass_cpu_streaming(
            &info,
            &visible,
            &mut canvas,
            width,
            height,
            None,
            &mut timing,
            &tracker,
        )
        .expect("stream composite");

        assert_eq!(composited, 1, "only the base layer should composite");
        assert_eq!(px(&canvas, width, 0, 0), [10, 20, 30, 255]);
    }

    #[test]
    fn streaming_composite_matches_batch_with_clipping_and_screen_blend() {
        // Bottom red base, a Screen-blended clip on top of it (clipped to the
        // base's silhouette), and an unclipped green base above both. This
        // exercises both blend dispatch and clipping-group handling on the
        // streaming path, and must match the pre-existing batch API exactly.
        let (width, height) = (4u32, 4u32);
        let specs = [
            TestLayerSpec {
                top: 0,
                left: 0,
                bottom: 4,
                right: 4,
                rgb: (200, 0, 0),
                blend: *b"norm",
                clipping: 0,
                opacity: 255,
            },
            TestLayerSpec {
                top: 1,
                left: 1,
                bottom: 3,
                right: 3,
                rgb: (0, 0, 255),
                blend: *b"scrn",
                clipping: 1,
                opacity: 255,
            },
            TestLayerSpec {
                top: 0,
                left: 2,
                bottom: 2,
                right: 4,
                rgb: (0, 128, 0),
                blend: *b"norm",
                clipping: 0,
                opacity: 128,
            },
        ];
        let (records, channel_data) = build_test_layers(&specs);
        let visible = vec![true; records.len()];
        let info = mk_layer_info(width, height, records, &channel_data);

        let mut streamed = vec![0u8; (width * height * 4) as usize];
        let mut timing = empty_timing();
        let tracker = StreamingPeakTracker::default();
        run_composite_pass_cpu_streaming(
            &info,
            &visible,
            &mut streamed,
            width,
            height,
            None,
            &mut timing,
            &tracker,
        )
        .expect("stream composite");

        let decoded = crate::psb_layer_decode::decode_layers_for_composite(&info, &visible, None)
            .expect("decode");
        let clip_refs: Vec<crate::psb_layer_clip::ClipLayerRef<'_>> = decoded
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
        let mut batch = vec![0u8; (width * height * 4) as usize];
        crate::psb_layer_clip::blend_layers_with_clipping(
            &mut batch, width, height, &clip_refs, None,
        )
        .expect("batch blend");

        assert_eq!(streamed, batch);
    }

    #[test]
    fn gpu_batch_eligible_allows_clipping_with_separable_modes() {
        let (width, height) = (4u32, 4u32);
        let channel_data_owned = Vec::new();
        let base_spec = |blend: [u8; 4], clipping: u8| TestLayerSpec {
            top: 0,
            left: 0,
            bottom: 4,
            right: 4,
            rgb: (10, 10, 10),
            blend,
            clipping,
            opacity: 255,
        };

        let (records, _) = build_test_layers(&[base_spec(*b"scrn", 0)]);
        let visible = vec![true; records.len()];
        let info = mk_layer_info(width, height, records, &channel_data_owned);
        assert!(
            gpu_batch_eligible_decoded_bytes(&info, &visible).is_some(),
            "Screen without clipping should be GPU batch eligible after P0"
        );

        for key in [*b"norm", *b"mul ", *b"lddg"] {
            let (records, _) = build_test_layers(&[base_spec(key, 0)]);
            let visible = vec![true; records.len()];
            let info = mk_layer_info(width, height, records, &channel_data_owned);
            assert!(
                gpu_batch_eligible_decoded_bytes(&info, &visible).is_some(),
                "separable mode {:?} should be eligible",
                key
            );
        }

        let (clipped_records, _) =
            build_test_layers(&[base_spec(*b"norm", 0), base_spec(*b"scrn", 1)]);
        let visible3 = vec![true; clipped_records.len()];
        let clipped_info = mk_layer_info(width, height, clipped_records, &channel_data_owned);
        assert!(
            gpu_batch_eligible_decoded_bytes(&clipped_info, &visible3).is_some(),
            "clipping with separable modes should be GPU batch eligible"
        );
    }

    #[test]
    fn layer_will_decode_matches_should_decode_conditions() {
        // `layer_will_decode` trusts the caller-supplied `visible` flag (that
        // is where `is_hidden()`/group visibility is already folded in by
        // `compute_effective_visibility`); it only re-checks the remaining
        // decode-eligibility conditions.
        let mut normal = mk_layer(false, false, None);
        normal.right = 2;
        normal.bottom = 2;
        assert!(layer_will_decode(&normal, true));
        assert!(!layer_will_decode(&normal, false), "not visible");

        let mut zero_opacity = mk_layer(false, false, None);
        zero_opacity.right = 2;
        zero_opacity.bottom = 2;
        zero_opacity.opacity = 0;
        assert!(!layer_will_decode(&zero_opacity, true));

        let divider = mk_layer(false, true, Some(1));
        assert!(!layer_will_decode(&divider, true));

        let mut oversized = mk_layer(false, false, None);
        oversized.right = crate::psb_reader::PSD_MAX_DIMENSION as i32 + 1;
        oversized.bottom = 1;
        assert!(!layer_will_decode(&oversized, true));
    }
}
