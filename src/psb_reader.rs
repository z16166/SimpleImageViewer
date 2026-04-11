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

//! Minimal PSB (Photoshop Large Document) composite image reader.
//!
//! This module ONLY extracts the flattened composite image from a PSB file.
//! It does NOT parse layers, masks, or image resources — those are skipped
//! entirely to minimise memory usage and complexity.
//!
//! The PSB format is nearly identical to PSD, differing only in:
//! - Version field: 2 instead of 1
//! - Several length fields are u64 instead of u32
//! - Channel row byte counts in RLE are u32 instead of u16
//!
//! Reference: Adobe Photoshop File Formats Specification (March 2013)

use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

/// Decoded PSB composite image.
pub struct PsbComposite {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>, // RGBA8
}

/// Read the flattened composite image from a PSD/PSB file.
/// Works for both PSD (v1) and PSB (v2).
pub fn read_composite(path: &Path) -> Result<PsbComposite, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("Cannot open file: {e}"))?;
    let mut r = BufReader::new(file);

    // ── Section 1: File Header ─────────────────────────────────────
    let mut sig = [0u8; 4];
    r.read_exact(&mut sig).map_err(|e| format!("Read error: {e}"))?;
    if &sig != b"8BPS" {
        return Err("Not a PSD/PSB file (invalid signature)".into());
    }

    let version = read_u16(&mut r)?;
    if version != 1 && version != 2 {
        return Err(format!("Unknown PSD/PSB version: {version}"));
    }
    let is_psb = version == 2;

    // 6 bytes reserved
    r.seek(SeekFrom::Current(6)).map_err(|e| format!("Seek error: {e}"))?;

    let channels = read_u16(&mut r)? as u32;
    let height = read_u32(&mut r)?;
    let width = read_u32(&mut r)?;
    let depth = read_u16(&mut r)?; // bits per channel (8, 16, 32)
    let color_mode = read_u16(&mut r)?;

    if depth != 8 {
        return Err(format!(
            "Only 8-bit depth is supported for direct viewing (this file is {depth}-bit). \
             Please convert to 8-bit TIFF first."
        ));
    }

    log::info!(
        "PSB header: {}x{}, {} channels, {}-bit, color_mode={}, version={}",
        width, height, channels, depth, color_mode, version
    );

    // ── Section 2: Color Mode Data ─────────────────────────────────
    let cm_len = read_u32(&mut r)?;
    r.seek(SeekFrom::Current(cm_len as i64)).map_err(|e| format!("Seek error: {e}"))?;

    // ── Section 3: Image Resources ─────────────────────────────────
    let ir_len = read_u32(&mut r)?;
    r.seek(SeekFrom::Current(ir_len as i64)).map_err(|e| format!("Seek error: {e}"))?;

    // ── Section 4: Layer and Mask Information ───────────────────────
    // PSD uses u32 length, PSB uses u64 length
    let lm_len = if is_psb {
        read_u64(&mut r)?
    } else {
        read_u32(&mut r)? as u64
    };
    r.seek(SeekFrom::Current(lm_len as i64)).map_err(|e| format!("Seek error: {e}"))?;

    // ── Section 5: Image Data (the flattened composite) ────────────
    let compression = read_u16(&mut r)?;

    let num_channels = channels.min(4); // We only care about up to 4 channels (RGBA)
    let total_rows = height as usize * channels as usize;

    let mut channel_data: Vec<Vec<u8>> = Vec::new();

    match compression {
        0 => {
            // Raw data — channels stored sequentially, each is width*height bytes
            let row_bytes = width as usize;
            for _ch in 0..channels {
                let mut buf = vec![0u8; row_bytes * height as usize];
                r.read_exact(&mut buf).map_err(|e| format!("Read raw data: {e}"))?;
                channel_data.push(buf);
            }
        }
        1 => {
            // RLE compressed
            // First: row byte counts for ALL channels (total_rows entries)
            // PSD: u16 each, PSB: u32 each
            let mut row_counts = Vec::with_capacity(total_rows);
            for _ in 0..total_rows {
                let count = if is_psb {
                    read_u32(&mut r)? as usize
                } else {
                    read_u16(&mut r)? as usize
                };
                row_counts.push(count);
            }

            // Then: compressed data for each channel, row by row
            let mut row_idx = 0;
            for _ch in 0..channels {
                let mut channel_buf = Vec::with_capacity(width as usize * height as usize);
                for _row in 0..height {
                    let compressed_len = row_counts[row_idx];
                    let mut compressed = vec![0u8; compressed_len];
                    r.read_exact(&mut compressed).map_err(|e| format!("Read RLE data: {e}"))?;
                    let decompressed = unpack_bits(&compressed, width as usize);
                    channel_buf.extend_from_slice(&decompressed);
                    row_idx += 1;
                }
                channel_data.push(channel_buf);
            }
        }
        _ => {
            return Err(format!(
                "Unsupported compression method: {compression} (only raw and RLE are supported)"
            ));
        }
    }

    // ── Interleave channels into RGBA ──────────────────────────────
    let pixel_count = width as usize * height as usize;
    let mut rgba = vec![255u8; pixel_count * 4]; // Default alpha = 255

    // Color mode mapping:
    // 1 = Grayscale, 3 = RGB, 4 = CMYK (simplified)
    match color_mode {
        1 => {
            // Grayscale → RGB (duplicate to all channels)
            if let Some(gray) = channel_data.first() {
                for i in 0..pixel_count {
                    let v = gray.get(i).copied().unwrap_or(0);
                    rgba[i * 4] = v;
                    rgba[i * 4 + 1] = v;
                    rgba[i * 4 + 2] = v;
                }
            }
            // Alpha channel if present
            if channels >= 2 {
                if let Some(alpha) = channel_data.get(1) {
                    for i in 0..pixel_count {
                        rgba[i * 4 + 3] = alpha.get(i).copied().unwrap_or(255);
                    }
                }
            }
        }
        3 => {
            // RGB
            for ch in 0..3usize {
                if let Some(data) = channel_data.get(ch) {
                    for i in 0..pixel_count {
                        rgba[i * 4 + ch] = data.get(i).copied().unwrap_or(0);
                    }
                }
            }
            // Alpha
            if channels >= 4 {
                if let Some(alpha) = channel_data.get(3) {
                    for i in 0..pixel_count {
                        rgba[i * 4 + 3] = alpha.get(i).copied().unwrap_or(255);
                    }
                }
            }
        }
        _ => {
            // Best-effort: treat first 3 channels as RGB
            for ch in 0..3usize.min(channels as usize) {
                if let Some(data) = channel_data.get(ch) {
                    for i in 0..pixel_count {
                        rgba[i * 4 + ch] = data.get(i).copied().unwrap_or(0);
                    }
                }
            }
        }
    }

    Ok(PsbComposite { width, height, pixels: rgba })
}

/// PackBits RLE decompression (Macintosh PackBits variant).
fn unpack_bits(data: &[u8], expected_len: usize) -> Vec<u8> {
    let mut result = Vec::with_capacity(expected_len);
    let mut i = 0;
    while i < data.len() && result.len() < expected_len {
        let n = data[i] as i8;
        i += 1;
        if n >= 0 {
            // Copy next (n+1) bytes literally
            let count = n as usize + 1;
            let end = (i + count).min(data.len());
            result.extend_from_slice(&data[i..end]);
            i = end;
        } else if n > -128 {
            // Repeat next byte (1-n) times
            let count = (1 - n as i16) as usize;
            if i < data.len() {
                let val = data[i];
                i += 1;
                for _ in 0..count {
                    if result.len() >= expected_len { break; }
                    result.push(val);
                }
            }
        }
        // n == -128: no-op
    }
    result
}

// ── Helpers ────────────────────────────────────────────────────────

fn read_u16(r: &mut impl Read) -> Result<u16, String> {
    let mut buf = [0u8; 2];
    r.read_exact(&mut buf).map_err(|e| format!("Read u16: {e}"))?;
    Ok(u16::from_be_bytes(buf))
}

fn read_u32(r: &mut impl Read) -> Result<u32, String> {
    let mut buf = [0u8; 4];
    r.read_exact(&mut buf).map_err(|e| format!("Read u32: {e}"))?;
    Ok(u32::from_be_bytes(buf))
}

fn read_u64(r: &mut impl Read) -> Result<u64, String> {
    let mut buf = [0u8; 8];
    r.read_exact(&mut buf).map_err(|e| format!("Read u64: {e}"))?;
    Ok(u64::from_be_bytes(buf))
}

/// Estimate the memory required to decode a PSD/PSB composite (in bytes).
/// Returns (width, height, channels, estimated_bytes) or an error.
pub fn estimate_memory(path: &Path) -> Result<(u32, u32, u32, u64), String> {
    let file = std::fs::File::open(path).map_err(|e| format!("Cannot open file: {e}"))?;
    let mut r = BufReader::new(file);

    let mut sig = [0u8; 4];
    r.read_exact(&mut sig).map_err(|e| format!("Read error: {e}"))?;
    if &sig != b"8BPS" {
        return Err("Not a PSD/PSB file".into());
    }
    let _version = read_u16(&mut r)?;
    r.seek(SeekFrom::Current(6)).map_err(|e| format!("Seek error: {e}"))?;
    let channels = read_u16(&mut r)? as u32;
    let height = read_u32(&mut r)?;
    let width = read_u32(&mut r)?;
    let _depth = read_u16(&mut r)?;

    // Estimated memory: channels * width * height (for planar) + width * height * 4 (for RGBA output)
    let planar = width as u64 * height as u64 * channels as u64;
    let rgba = width as u64 * height as u64 * 4;
    Ok((width, height, channels, planar + rgba))
}
