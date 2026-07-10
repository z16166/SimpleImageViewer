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

//! Layer-aware PSD/PSB compositor and SDR main-image fallback.
//!
//! Used when the flattened Image Data section cannot be decoded structurally
//! (see `decode_psd_sdr_main_from_bytes_with_cancel`). Decodes each layer's
//! channels (depth 8) and composites them bottom to top with Normal / Screen /
//! Linear Dodge / Multiply blend + opacity + user mask, respecting strict
//! Photoshop layer/group visibility only (no viewer heuristics that open
//! hidden layers).

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
    /// Mask rect/flags parsed from the mask data block, when present and long
    /// enough to contain the standard rect + default color + flags fields.
    pub mask: Option<LayerMaskInfo>,
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

/// Whether `width`/`height` are within the same per-side limit used for the
/// document's own canvas (`psb_reader::PSD_MAX_DIMENSION`).
///
/// A malformed or malicious layer record can claim absurd bounds (e.g.
/// `right - left` near `u32::MAX`) that would make `decode_channel_image`'s
/// `vec![0u8; width * height]` try to allocate an enormous buffer, aborting
/// the whole process on allocation failure. Layer and mask rects are checked
/// against this same limit before any such allocation.
fn dimensions_within_limit(width: u32, height: u32) -> bool {
    width <= crate::psb_reader::PSD_MAX_DIMENSION && height <= crate::psb_reader::PSD_MAX_DIMENSION
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
    let ir_start = r.position();
    let ir_end = checked_end(ir_start, ir_len, file_len, "image resources")?;
    let embedded_icc = crate::psb_reader::extract_icc_profile_from_ir(bytes, ir_start, ir_end);
    skip_section(&mut r, ir_len, file_len, "image resources")?;

    let cmyk_icc = if color_mode == 4 {
        crate::psb_cmyk_cms::resolve_cmyk_icc(embedded_icc.as_deref()).to_vec()
    } else {
        Vec::new()
    };

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
            cmyk_icc,
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
            cmyk_icc,
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
        cmyk_icc,
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
    let (mask_size, mask, is_section_divider, section_type) =
        parse_layer_extra(r, extra_end, is_psb)?;

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
        is_section_divider,
        section_type,
    })
}

fn parse_layer_extra(
    r: &mut std::io::Cursor<&[u8]>,
    extra_end: u64,
    is_psb: bool,
) -> Result<(u32, Option<LayerMaskInfo>, bool, Option<u32>), String> {
    if r.position() >= extra_end {
        return Ok((0, None, false, None));
    }

    let mask_size = read_extra_u32(r, extra_end, "layer mask data length")?;
    let mask_start = r.position();
    let mask_end = checked_end(mask_start, mask_size as u64, extra_end, "layer mask data")?;
    // Standard layer mask data (when >= 20 bytes): rect (4 x i32) + default
    // color (1 byte) + flags (1 byte), possibly followed by mask parameters
    // and/or a "real" user mask rect that v1 does not need. Shorter blocks
    // (rare, malformed, or absent) leave `mask` as `None`.
    let mask = if mask_size >= 20 {
        parse_layer_mask_rect(r, mask_end)?
    } else {
        None
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
    Ok((mask_size, mask, is_section_divider, section_type))
}

/// Parse the rect + default color + flags fields at the start of a layer's
/// mask data block (see [`LayerMaskInfo`]). Returns `None` if the block is
/// too short to hold them (defensive; `parse_layer_extra` already checks
/// `mask_size >= 20` before calling this).
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

    Ok(Some(LayerMaskInfo {
        top,
        left,
        bottom,
        right,
        default_color: default_color[0],
        disabled,
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
) -> Result<Vec<u8>, String> {
    let mut r = std::io::Cursor::new(data);
    let compression = crate::psb_reader::read_u16(&mut r)?;
    let pixel_count = width as usize * height as usize;

    match compression {
        0 => {
            let mut out = vec![0u8; pixel_count];
            let avail = data.len().saturating_sub(2);
            let copy = avail.min(pixel_count);
            out[..copy].copy_from_slice(&data[2..2 + copy]);
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
                crate::psb_reader::unpack_bits_into(&mut row_buf, compressed, width as usize);
                let dst_start = row * width as usize;
                out[dst_start..dst_start + width as usize]
                    .copy_from_slice(&row_buf[..width as usize]);
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
        }
        _ => Err(format!(
            "Unsupported layer channel compression: {compression}"
        )),
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
    let pixel_count = args.width as usize * args.height as usize;
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

    let mut rgba = vec![0u8; pixel_count * 4];
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
    let mut out = vec![mask_info.default_color; layer_w as usize * layer_h as usize];
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

/// Separable blend function B(Cb, Cs) in [0, 1] (Photoshop / PDF).
type SeparableBlendFn = fn(f32, f32) -> f32;

fn blend_fn_normal(_cb: f32, cs: f32) -> f32 {
    cs
}

fn blend_fn_screen(cb: f32, cs: f32) -> f32 {
    1.0 - (1.0 - cb) * (1.0 - cs)
}

fn blend_fn_linear_dodge(cb: f32, cs: f32) -> f32 {
    (cb + cs).min(1.0)
}

fn blend_fn_multiply(cb: f32, cs: f32) -> f32 {
    cb * cs
}

fn separable_blend_fn(blend: &[u8; 4]) -> Option<SeparableBlendFn> {
    match blend {
        b"norm" => Some(blend_fn_normal),
        b"scrn" => Some(blend_fn_screen),
        b"lddg" => Some(blend_fn_linear_dodge),
        b"mul " => Some(blend_fn_multiply),
        _ => None,
    }
}

fn blend_mode_supported(blend: &[u8; 4]) -> bool {
    separable_blend_fn(blend).is_some()
}

/// Straight-alpha separable blend of `layer_rgba` onto `canvas` (PDF formula).
/// `blend_fn` is Photoshop B(Cb, Cs); Normal is B = Cs (src-over).
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
    blend_fn: SeparableBlendFn,
    is_normal: bool,
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

    for sy in src_y0..src_y1 {
        let dy = (top + sy) as usize;
        let dst_row_start = dy * canvas_w as usize * 4;
        let src_row_start = sy as usize * lw as usize * 4;
        for sx in src_x0..src_x1 {
            let dx = (left + sx) as usize;
            let d_off = dst_row_start + dx * 4;
            let s_off = src_row_start + sx as usize * 4;

            let sa = layer_rgba[s_off + 3];
            if sa == 0 {
                continue;
            }
            // Opaque Normal is a straight copy; Screen/Add still need B(Cb, Cs).
            if is_normal && sa == 255 {
                canvas[d_off..d_off + 4].copy_from_slice(&layer_rgba[s_off..s_off + 4]);
                continue;
            }

            let sa_f = sa as f32 / 255.0;
            let da_f = canvas[d_off + 3] as f32 / 255.0;
            let out_a_f = sa_f + da_f * (1.0 - sa_f);
            if out_a_f <= 0.0 {
                canvas[d_off..d_off + 4].fill(0);
                continue;
            }

            for c in 0..3 {
                let sc = layer_rgba[s_off + c] as f32 / 255.0;
                let dc = canvas[d_off + c] as f32 / 255.0;
                let b = blend_fn(dc, sc);
                // Premultiplied channel, then un-premultiply to straight alpha.
                let co = sa_f * (1.0 - da_f) * sc + sa_f * da_f * b + da_f * (1.0 - sa_f) * dc;
                canvas[d_off + c] = ((co / out_a_f).clamp(0.0, 1.0) * 255.0).round() as u8;
            }
            canvas[d_off + 3] = (out_a_f.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
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
        blend_fn_normal,
        true,
    );
}

/// Dispatch by PSD blend-mode key; unknown modes fall back to Normal (logged once).
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
    let (blend_fn, is_normal) = match separable_blend_fn(blend) {
        Some(f) if blend == b"norm" => (f, true),
        Some(f) => (f, false),
        None => {
            log_unsupported_blend_once(blend);
            (blend_fn_normal as SeparableBlendFn, true)
        }
    };
    blend_separable_onto(
        canvas, canvas_w, canvas_h, layer_rgba, left, top, lw, lh, blend_fn, is_normal,
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
) -> Result<Option<DecodedLayer>, String> {
    let width = record.width();
    let height = record.height();
    let has_bounds = params.should_decode && width > 0 && height > 0;
    // Treat an oversized layer rect as corrupt *for that layer only*: skip
    // decoding it (still advancing `cursor` past its channel bytes below so
    // later layers stay aligned) rather than erroring out the whole
    // composite, since every other layer in the file may well be fine.
    if has_bounds && !dimensions_within_limit(width, height) {
        log::debug!(
            "PSD/PSB layer rect {width}x{height} exceeds max dimension {}, skipping layer",
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
                Err(e) if crate::loader::is_decode_cancelled_error(&e) => return Err(e),
                Err(e) => log::debug!("PSD/PSB layer alpha channel decode failed: {e}"),
            },
            -2 => {
                // The mask channel's own rect (from the layer's mask data
                // block) can differ in size and/or offset from the layer's
                // rect -- decode at the mask's own dimensions, then blit into
                // a layer-sized buffer. With no parsed rect (`mask_size == 0`
                // or too short to contain one), the mask's true geometry is
                // unknown, so it is skipped entirely (no mask applied).
                if let Some(mask_info) = &record.mask {
                    let mask_w = mask_info.width();
                    let mask_h = mask_info.height();
                    let mask_has_bounds = !mask_info.disabled && mask_w > 0 && mask_h > 0;
                    // Same oversized-rect guard as the layer rect below: skip
                    // just this mask (fall back to no mask) rather than
                    // erroring out the whole layer/composite.
                    if mask_has_bounds && !dimensions_within_limit(mask_w, mask_h) {
                        log::debug!(
                            "PSD/PSB layer mask rect {mask_w}x{mask_h} exceeds max dimension \
                             {}, skipping mask",
                            crate::psb_reader::PSD_MAX_DIMENSION
                        );
                    } else if mask_has_bounds {
                        match decode_channel_image(
                            slice,
                            mask_w,
                            mask_h,
                            params.is_psb,
                            params.cancel,
                        ) {
                            Ok(mask_pixels) => {
                                mask = Some(build_layer_sized_mask(
                                    mask_info,
                                    &mask_pixels,
                                    record.left,
                                    record.top,
                                    width,
                                    height,
                                ));
                            }
                            Err(e) if crate::loader::is_decode_cancelled_error(&e) => {
                                return Err(e);
                            }
                            Err(e) => log::debug!("PSD/PSB layer mask channel decode failed: {e}"),
                        }
                    }
                }
            }
            -3 => {
                // Real user mask (rendered from a combined vector + user
                // mask). Not supported in v1; the channel's bytes are still
                // consumed above via `cursor`, so later layers stay aligned.
            }
            0..=3 => {
                let idx = ch.id as usize;
                match decode_channel_image(slice, width, height, params.is_psb, params.cancel) {
                    Ok(data) => color[idx] = Some(data),
                    Err(e) if crate::loader::is_decode_cancelled_error(&e) => return Err(e),
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
        rgba,
    }))
}

/// Returned when strict visibility has no drawable layers (geometry/flags).
pub const STRICT_LAYER_COMPOSITE_BLANK: &str = "PSD layer composite has no drawable visible layers";

/// Decode a PSD/PSB layer stack and composite it into a single RGBA8 canvas
/// (depth 8 only: Normal / Screen / Linear Dodge / Multiply + opacity + user
/// mask + strict group/leaf visibility).
///
/// When `gpu` is provided, the canvas is large enough, and every decoded layer
/// uses Normal blend, blending may run on an offscreen wgpu compute path;
/// failures or non-Normal stacks fall back to CPU.
///
/// Returns [`STRICT_LAYER_COMPOSITE_BLANK`] when no visible layer intersects
/// the canvas (no pixel work is performed).
pub fn composite_layers_from_bytes_with_cancel(
    bytes: &[u8],
    cancel: Option<&std::sync::atomic::AtomicBool>,
    gpu: Option<&crate::psb_layer_blend_gpu::PsdGpuContext>,
) -> Result<crate::psb_reader::PsbComposite, String> {
    let total_t0 = std::time::Instant::now();
    crate::psb_reader::check_decode_cancel(cancel)?;
    let parse_t0 = std::time::Instant::now();
    let info = parse_layer_records(bytes)?;
    let parse_ms = parse_t0.elapsed().as_secs_f64() * 1000.0;
    if info.depth != 8 {
        return Err(format!(
            "PSD/PSB layer composite requires 8-bit depth (found {}-bit)",
            info.depth
        ));
    }

    let canvas_w = info.width;
    let canvas_h = info.height;
    let visible = compute_effective_visibility(&info.records);
    if !strict_visibility_has_drawable_output(canvas_w, canvas_h, &info.records, &visible) {
        return Err(STRICT_LAYER_COMPOSITE_BLANK.to_string());
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
        true,
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

/// SDR main-image state machine: flattened composite -> strict layer composite
/// -> IR thumbnail -> explicit failure. Hidden layers are never opened.
///
/// P1 accepts a structurally valid flattened buffer only when it is not an
/// absolute blank (all-alpha-0 or all-RGB-0). P2 accepts a strict-visibility
/// composite only when it is not zero-information (all-alpha-0 or solid RGB
/// with variance 0). P3 accepts an IR thumbnail under the same zero-information
/// barrier as P2. All barriers are full-buffer SIMD scans.
pub fn decode_psd_sdr_main_from_bytes_with_cancel(
    bytes: &[u8],
    cancel: Option<&std::sync::atomic::AtomicBool>,
    gpu: Option<&crate::psb_layer_blend_gpu::PsdGpuContext>,
) -> Result<crate::psb_reader::PsbComposite, String> {
    // P1: structurally valid flattened Image Data, then absolute blank barrier.
    match crate::psb_reader::read_composite_from_bytes_with_cancel(bytes, cancel) {
        Ok(composite) => {
            let absolutely_blank = crate::psb_reader::rgba8_is_absolutely_blank_with_cancel(
                &composite.pixels,
                cancel,
            )?;
            if absolutely_blank {
                crate::preload_debug!(
                    "[PreloadDebug][PsdSdrMain] stage=P1_absolute_blank {}x{} \
                     pixels={} -> degrade_P2",
                    composite.width,
                    composite.height,
                    composite.pixels.len()
                );
                log::info!(
                    "PSD SDR main: P1 flattened {}x{} is absolute blank \
                     (all-transparent or all-RGB-0); degrading to P2",
                    composite.width,
                    composite.height
                );
            } else {
                crate::preload_debug!(
                    "[PreloadDebug][PsdSdrMain] stage=P1_flattened {}x{} pixels={}",
                    composite.width,
                    composite.height,
                    composite.pixels.len()
                );
                log::info!(
                    "PSD SDR main: P1 flattened composite {}x{}",
                    composite.width,
                    composite.height
                );
                return Ok(composite);
            }
        }
        Err(e) if crate::loader::is_decode_cancelled_error(&e) => return Err(e),
        Err(e) => {
            crate::preload_debug!("[PreloadDebug][PsdSdrMain] stage=P1_fail err={e}");
            log::debug!("PSD SDR main P1 flattened decode failed: {e}");
        }
    }

    // P2: strict visibility layer composite, then zero-information barrier.
    let mut p2_no_drawable_visible = false;
    match composite_layers_from_bytes_with_cancel(bytes, cancel, gpu) {
        Ok(composite) => {
            let zero_info = crate::psb_reader::rgba8_is_zero_information_with_cancel(
                &composite.pixels,
                cancel,
            )?;
            if zero_info {
                crate::preload_debug!(
                    "[PreloadDebug][PsdSdrMain] stage=P2_zero_information {}x{} \
                     pixels={} -> degrade_P3",
                    composite.width,
                    composite.height,
                    composite.pixels.len()
                );
                log::info!(
                    "PSD SDR main: P2 strict composite {}x{} is zero-information \
                     (all-transparent or solid RGB); degrading to P3",
                    composite.width,
                    composite.height
                );
            } else {
                crate::preload_debug!(
                    "[PreloadDebug][PsdSdrMain] stage=P2_strict_layers {}x{} pixels={}",
                    composite.width,
                    composite.height,
                    composite.pixels.len()
                );
                log::info!(
                    "PSD SDR main: P2 strict layer composite {}x{}",
                    composite.width,
                    composite.height
                );
                return Ok(composite);
            }
        }
        Err(e) if crate::loader::is_decode_cancelled_error(&e) => return Err(e),
        Err(e) => {
            p2_no_drawable_visible = e == STRICT_LAYER_COMPOSITE_BLANK;
            crate::preload_debug!("[PreloadDebug][PsdSdrMain] stage=P2_fail err={e}");
            log::debug!("PSD SDR main P2 layer composite unavailable: {e}");
        }
    }

    // P3: embedded Photoshop IR thumbnail, then zero-information barrier.
    match crate::psb_reader::try_extract_photoshop_thumbnail(bytes) {
        Some(thumb) => {
            let zero_info =
                crate::psb_reader::rgba8_is_zero_information_with_cancel(&thumb.pixels, cancel)?;
            if zero_info {
                crate::preload_debug!(
                    "[PreloadDebug][PsdSdrMain] stage=P3_zero_information {}x{} \
                     pixels={} -> fail",
                    thumb.width,
                    thumb.height,
                    thumb.pixels.len()
                );
                log::info!(
                    "PSD SDR main: P3 IR thumbnail {}x{} is zero-information \
                     (all-transparent or solid RGB); no displayable image",
                    thumb.width,
                    thumb.height
                );
            } else {
                crate::preload_debug!(
                    "[PreloadDebug][PsdSdrMain] stage=P3_ir_thumbnail {}x{} pixels={}",
                    thumb.width,
                    thumb.height,
                    thumb.pixels.len()
                );
                log::info!(
                    "PSD SDR main: P3 IR thumbnail {}x{}",
                    thumb.width,
                    thumb.height
                );
                return Ok(thumb);
            }
        }
        None => {
            crate::preload_debug!("[PreloadDebug][PsdSdrMain] stage=P3_fail no_ir_thumbnail");
            log::debug!("PSD SDR main P3: no embedded IR thumbnail");
        }
    }

    crate::preload_debug!("[PreloadDebug][PsdSdrMain] stage=fail no_p1_p2_p3");
    if p2_no_drawable_visible {
        return Err(rust_i18n::t!("error.psd_all_layers_hidden").to_string());
    }
    Err(rust_i18n::t!("error.psd_no_displayable_image").to_string())
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
        for px in canvas.chunks_exact_mut(4) {
            px[0] = 255;
            px[1] = 255;
            px[2] = 255;
            px[3] = 255;
        }
    } else {
        canvas.fill(0);
    }
}

/// Layer pixel-area threshold above which `run_composite_pass` polls `cancel`
/// immediately before and after `blend_normal_onto`, in addition to the
/// per-layer poll at the top of the loop (blending a very large layer can
/// take a while, so a cancellation should not have to wait for the next one).
const LARGE_LAYER_BLEND_CANCEL_POLL_PIXELS: u64 = 1_000_000;

/// Decode and blend every eligible layer bottom to top, returning how many were
/// actually composited. When `respect_visibility` is false, `visible` is ignored
/// (every non-divider, non-empty, non-fully-transparent layer is composited).
#[allow(clippy::too_many_arguments)]
fn run_composite_pass(
    info: &LayerInfo<'_>,
    visible: &[bool],
    respect_visibility: bool,
    canvas: &mut [u8],
    canvas_w: u32,
    canvas_h: u32,
    cancel: Option<&std::sync::atomic::AtomicBool>,
    gpu: Option<&crate::psb_layer_blend_gpu::PsdGpuContext>,
    timing: &mut CompositeTiming,
) -> Result<usize, String> {
    // Layer records and channel_data are both stored bottom to top (index 0
    // is the bottommost layer). Decoding must walk that same order to stay
    // aligned with `channel_data`; blending happens to want the identical
    // order (draw the bottommost layer first, then successively higher ones
    // on top), so a single forward pass does both. Skipped layers still
    // advance `cursor` past their channel bytes so later layers stay aligned,
    // even though they are not decoded or blended.
    let mut cursor: usize = 0;
    let mut layers: Vec<DecodedLayer> = Vec::new();
    let decode_t0 = std::time::Instant::now();
    for (i, record) in info.records.iter().enumerate() {
        // Poll every layer (not just periodically): with hundreds of layers,
        // each potentially decoding a full-canvas image, waiting many layers
        // between polls makes cancellation feel unresponsive.
        crate::psb_reader::check_decode_cancel(cancel)?;
        let should_decode = (!respect_visibility || visible[i])
            && !record.is_section_divider
            && !record.is_empty_bounds()
            && record.opacity > 0;
        let layer = decode_one_layer(
            info.channel_data,
            &mut cursor,
            record,
            &LayerDecodeParams {
                color_mode: info.color_mode,
                is_psb: info.is_psb,
                should_decode,
                cancel,
                cmyk_icc: info.cmyk_icc.as_slice(),
            },
        )?;
        if let Some(layer) = layer {
            layers.push(layer);
        }
    }
    // Decode includes PackBits + planar convert + CMYK/ICC; split CMS later if needed.
    timing.unpack_ms += decode_t0.elapsed().as_secs_f64() * 1000.0;
    timing.layers = layers.len();

    if layers.is_empty() {
        return Ok(0);
    }

    let blend_t0 = std::time::Instant::now();
    let all_normal = layers.iter().all(|l| l.blend == *b"norm");
    let used_gpu = if let Some(gpu_ctx) = gpu {
        if !all_normal {
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
                canvas.copy_from_slice(&gpu_pixels);
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
        for layer in &layers {
            let is_large =
                layer.width as u64 * layer.height as u64 > LARGE_LAYER_BLEND_CANCEL_POLL_PIXELS;
            if is_large {
                crate::psb_reader::check_decode_cancel(cancel)?;
            }
            blend_layer_onto(
                canvas,
                canvas_w,
                canvas_h,
                &layer.rgba,
                layer.left,
                layer.top,
                layer.width,
                layer.height,
                &layer.blend,
            );
            if is_large {
                crate::psb_reader::check_decode_cancel(cancel)?;
            }
        }
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
        STRICT_LAYER_COMPOSITE_BLANK, blend_fn_screen, blend_layer_onto, blend_normal_onto,
        blend_separable_onto, build_layer_sized_mask, composite_layers_from_bytes_with_cancel,
        compute_effective_visibility, decode_one_layer, decode_psd_sdr_main_from_bytes_with_cancel,
        dimensions_within_limit, layer_to_rgba8, parse_layer_records, scan_extra_tagged_blocks,
        strict_visibility_has_drawable_output,
    };
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
            is_section_divider,
            section_type,
        }
    }

    #[test]
    fn dimensions_within_limit_rejects_oversized_dimensions() {
        assert!(dimensions_within_limit(1, 1));
        assert!(dimensions_within_limit(
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
            blend_fn_screen,
            false,
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
    fn decode_01_02_psd_sdr_main_returns_structurally_valid_image() {
        // Flattened Image Data may be a solid-ish placeholder; under the SDR
        // state machine that is still a valid P1 result (no pixel heuristics).
        let path = Path::new(r"F:\BaiduNetdiskDownload\素材库\45套 psd企业画册模板\12\01-02.psd");
        if !path.is_file() {
            eprintln!("skipping decode_01_02_psd_sdr_main...; sample missing");
            return;
        }
        let bytes = std::fs::read(path).unwrap();
        let main = decode_psd_sdr_main_from_bytes_with_cancel(&bytes, None, None).expect("main");
        assert_eq!((main.width, main.height), (5031, 3437));
        assert_eq!(main.pixels.len(), 5031 * 3437 * 4);
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
        assert_eq!(err, STRICT_LAYER_COMPOSITE_BLANK);
    }

    #[test]
    fn decode_psd_sdr_main_all_hidden_reports_photoshop_hint() {
        let path = Path::new(r"F:\BaiduNetdiskDownload\素材库\45套 psd企业画册模板\18\18\1-2.psd");
        if !path.is_file() {
            eprintln!("skipping decode_psd_sdr_main_all_hidden...; sample missing");
            return;
        }
        let bytes = std::fs::read(path).expect("read");
        let err = decode_psd_sdr_main_from_bytes_with_cancel(&bytes, None, None)
            .expect_err("expected fail when all layers hidden and P3 is blank");
        let expected = rust_i18n::t!("error.psd_all_layers_hidden").to_string();
        assert_eq!(err, expected);
        assert!(
            err.contains("designer") || err.contains("设计师") || err.contains("設計師"),
            "error should attribute hidden layers to the designer: {err}"
        );
        assert!(
            err.contains("Photoshop"),
            "error should point users to Photoshop: {err}"
        );
    }

    #[test]
    fn decode_psd_sdr_main_prefers_structurally_valid_flattened() {
        // 10.psd has a usable flattened composite -- P1 must win even if layers exist.
        let path = Path::new(r"F:\BaiduNetdiskDownload\素材库\45套 psd企业画册模板\10\10.psd");
        if !path.is_file() {
            eprintln!(
                "skipping decode_psd_sdr_main_prefers_structurally_valid_flattened; sample missing"
            );
            return;
        }
        let bytes = std::fs::read(path).expect("read");
        let flat = crate::psb_reader::read_composite_from_bytes(&bytes).expect("flat");
        let main = decode_psd_sdr_main_from_bytes_with_cancel(&bytes, None, None).expect("main");
        assert_eq!((main.width, main.height), (flat.width, flat.height));
        assert_eq!(main.pixels, flat.pixels);
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
