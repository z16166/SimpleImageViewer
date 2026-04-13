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

use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use memmap2::Mmap;

/// Decoded PSB composite image (Full in-memory).
#[allow(dead_code)]
pub struct PsbComposite {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>, // RGBA8
}

/// A tiled source for PSD/PSB files that decodes regions on demand from a memory-mapped file.
pub struct PsbTiledSource {
    #[allow(dead_code)]
    path: PathBuf,
    mmap: Arc<Mmap>,
    width: u32,
    height: u32,
    channels: u32,
    color_mode: u16,
    #[allow(dead_code)]
    is_psb: bool,
    compression: u16,
    /// Absolute file offsets for the start of each row's data.
    /// Index: ch_idx * height + row_idx
    row_offsets: Vec<u64>,
}

/// Read the flattened composite image from a PSD/PSB file.
/// Works for both PSD (v1) and PSB (v2).
#[allow(dead_code)]
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

    // Interleave channels directly into RGBA
    let pixel_count = width as usize * height as usize;
    let mut rgba = vec![255u8; pixel_count * 4]; // Default alpha = 255

    // First: row byte counts for RLE (if applicable)
    let total_rows = height as usize * channels as usize;
    let mut row_counts = Vec::new();
    if compression == 1 {
        row_counts.reserve(total_rows);
        for _ in 0..total_rows {
            let count = if is_psb {
                read_u32(&mut r)? as usize
            } else {
                read_u16(&mut r)? as usize
            };
            row_counts.push(count);
        }
    }

    // Decoding and interleaving
    for ch_idx in 0..channels {
        // Map channel index to RGBA component
        // Mode 1 (Grayscale): 0=Gray, 1=Alpha
        // Mode 3 (RGB): 0=R, 1=G, 2=B, 3=Alpha
        let ch_target = match (color_mode, ch_idx) {
            (1, 0) => vec![0, 1, 2], // Gray maps to R, G, B
            (1, 1) => vec![3],       // Alpha
            (3, 0) => vec![0],
            (3, 1) => vec![1],
            (3, 2) => vec![2],
            (3, 3) => vec![3],
            (_, 0..=2) => vec![ch_idx as usize],
            _ => vec![], // Skip extra channels
        };

        if ch_target.is_empty() {
             // Skip this channel in the file
             match compression {
                 0 => { r.seek(SeekFrom::Current(pixel_count as i64)).map_err(|e| format!("Skip raw: {e}"))?; }
                 1 => {
                     for row in 0..height {
                         let len = row_counts[ch_idx as usize * height as usize + row as usize];
                         r.seek(SeekFrom::Current(len as i64)).map_err(|e| format!("Skip RLE: {e}"))?;
                     }
                 }
                 _ => {}
             }
             continue;
        }

        match compression {
            0 => {
                // Raw data
                for row in 0..height {
                    let mut row_buf = vec![0u8; width as usize];
                    r.read_exact(&mut row_buf).map_err(|e| format!("Read raw row: {e}"))?;
                    for (col, &val) in row_buf.iter().enumerate() {
                        let base = (row as usize * width as usize + col) * 4;
                        for &target_offset in &ch_target {
                            rgba[base + target_offset] = val;
                        }
                    }
                }
            }
            1 => {
                // RLE data
                for row in 0..height {
                    let compressed_len = row_counts[ch_idx as usize * height as usize + row as usize];
                    let mut compressed = vec![0u8; compressed_len];
                    r.read_exact(&mut compressed).map_err(|e| format!("Read RLE: {e}"))?;
                    let decompressed = unpack_bits(&compressed, width as usize);
                    
                    for (col, &val) in decompressed.iter().enumerate() {
                        let base = (row as usize * width as usize + col) * 4;
                        for &target_offset in &ch_target {
                            rgba[base + target_offset] = val;
                        }
                    }
                }
            }
            _ => return Err(format!("Unsupported compression: {compression}")),
        }
    }

    Ok(PsbComposite { width, height, pixels: rgba })
}

/// Initialize a tiled source for a PSB file.
pub fn open_tiled_source(path: &Path) -> Result<PsbTiledSource, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("Cannot open file: {e}"))?;
    let mmap = unsafe { Mmap::map(&file).map_err(|e| format!("Mmap failed: {e}"))? };
    let mut cursor = std::io::Cursor::new(&mmap[..]);

    // 1. Parse Headers (same as read_composite but on cursor)
    let mut sig = [0u8; 4];
    cursor.read_exact(&mut sig).map_err(|e| e.to_string())?;
    let version = read_u16(&mut cursor)?;
    let is_psb = version == 2;
    cursor.seek(SeekFrom::Current(6)).map_err(|e| e.to_string())?;
    let channels = read_u16(&mut cursor)? as u32;
    let height = read_u32(&mut cursor)?;
    let width = read_u32(&mut cursor)?;
    let depth = read_u16(&mut cursor)?;
    let color_mode = read_u16(&mut cursor)?;

    if depth != 8 {
        return Err("Only 8-bit depth supported for tiled mode".into());
    }

    // Skip Sections 2, 3, 4
    let cm_len = read_u32(&mut cursor)?;
    cursor.seek(SeekFrom::Current(cm_len as i64)).map_err(|e| e.to_string())?;
    let ir_len = read_u32(&mut cursor)?;
    cursor.seek(SeekFrom::Current(ir_len as i64)).map_err(|e| e.to_string())?;
    let lm_len = if is_psb { read_u64(&mut cursor)? } else { read_u32(&mut cursor)? as u64 };
    cursor.seek(SeekFrom::Current(lm_len as i64)).map_err(|e| e.to_string())?;

    // Image Data Section start
    let compression = read_u16(&mut cursor)?;
    let row_counts_start = cursor.position();

    let mut row_offsets = Vec::with_capacity(channels as usize * height as usize);

    match compression {
        0 => {
            // Raw: data follows immediately, each row is simply 'width' bytes
            let pixel_count = width as u64 * height as u64;
            for ch in 0..channels {
                for row in 0..height {
                    row_offsets.push(row_counts_start + (ch as u64 * pixel_count) + (row as u64 * width as u64));
                }
            }
        }
        1 => {
            // RLE: row counts follow, then the data
            let total_rows = channels as usize * height as usize;
            let mut counts = Vec::with_capacity(total_rows);
            for _ in 0..total_rows {
                let cnt = if is_psb { read_u32(&mut cursor)? as u64 } else { read_u16(&mut cursor)? as u64 };
                counts.push(cnt);
            }
            let data_start = cursor.position();
            let mut running_offset = data_start;
            for cnt in counts {
                row_offsets.push(running_offset);
                running_offset += cnt;
            }
        }
        _ => return Err(format!("Unsupported compression: {compression}")),
    }

    Ok(PsbTiledSource {
        path: path.to_path_buf(),
        mmap: Arc::new(mmap),
        width, height, channels, color_mode, is_psb, compression,
        row_offsets,
    })
}

impl crate::loader::TiledImageSource for PsbTiledSource {
    fn width(&self) -> u32 { self.width }
    fn height(&self) -> u32 { self.height }

    fn extract_tile(&self, x: u32, y: u32, w: u32, h: u32) -> Vec<u8> {
        let mut rgba = vec![255u8; (w * h * 4) as usize];
        
        for ch_idx in 0..self.channels {
            let ch_target = match (self.color_mode, ch_idx) {
                (1, 0) => vec![0, 1, 2],
                (1, 1) => vec![3],
                (3, 0) => vec![0],
                (3, 1) => vec![1],
                (3, 2) => vec![2],
                (3, 3) => vec![3],
                (_, 0..=2) => vec![ch_idx as usize],
                _ => vec![],
            };
            if ch_target.is_empty() { continue; }

            for row_in_tile in 0..h {
                let global_row = y + row_in_tile;
                if global_row >= self.height { continue; }
                
                let idx = ch_idx as usize * self.height as usize + global_row as usize;
                let offset = self.row_offsets[idx] as usize;
                
                match self.compression {
                    0 => {
                        // Raw: just take the slice for the full line and then sub-slice it
                        let line_start = offset + x as usize;
                        let line_end = line_start + w as usize;
                        if line_end <= self.mmap.len() {
                            let data = &self.mmap[line_start..line_end];
                            for (col, &val) in data.iter().enumerate() {
                                let base = (row_in_tile as usize * w as usize + col) * 4;
                                for &target_offset in &ch_target {
                                    if base + target_offset < rgba.len() {
                                        rgba[base + target_offset] = val;
                                    }
                                }
                            }
                        }
                    }
                    1 => {
                        // RLE: decompress the Row
                        let next_offset = if (idx + 1) < self.row_offsets.len() {
                            self.row_offsets[idx + 1] as usize
                        } else { self.mmap.len() };
                        
                        if offset < self.mmap.len() && next_offset <= self.mmap.len() {
                            let compressed = &self.mmap[offset..next_offset];
                            let decompressed = unpack_bits(compressed, self.width as usize);
                            
                            let start = x as usize;
                            let end = (x + w) as usize;
                            if start < decompressed.len() {
                                let data = &decompressed[start..end.min(decompressed.len())];
                                for (col, &val) in data.iter().enumerate() {
                                    let base = (row_in_tile as usize * w as usize + col) * 4;
                                    for &target_offset in &ch_target {
                                        if base + target_offset < rgba.len() {
                                            rgba[base + target_offset] = val;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                };
            }
        }
        rgba
    }

    fn generate_preview(&self, max_w: u32, max_h: u32) -> (u32, u32, Vec<u8>) {
        // For a preview, we can just grab even-spaced rows/columns.
        // Even simpler: just use our existing extract_tile logic but specialized for downsampling if needed.
        // For now, let's do a basic but correct downsampling by skipping rows.
        let scale = (max_w as f64 / self.width as f64).min(max_h as f64 / self.height as f64).min(1.0);
        let out_w = (self.width as f64 * scale).round().max(1.0) as u32;
        let out_h = (self.height as f64 * scale).round().max(1.0) as u32;
        
        let mut pixels = vec![255u8; (out_w * out_h * 4) as usize];
        
        // This is a naive downsampler, but it's FAST because it only decodes required rows
        for y in 0..out_h {
            let src_y = (y as f64 / scale) as u32;
            // Decode JUST this one row at full width, then pick pixels
            let row_rgba = self.extract_tile(0, src_y, self.width, 1);
            for x in 0..out_w {
                let src_x = (x as f64 / scale) as usize;
                let dst_off = (y as usize * out_w as usize + x as usize) * 4;
                let src_off = src_x * 4;
                if src_off + 4 <= row_rgba.len() {
                    pixels[dst_off..dst_off+4].copy_from_slice(&row_rgba[src_off..src_off+4]);
                }
            }
        }
        
        (out_w, out_h, pixels)
    }

    fn full_pixels(&self) -> Option<Arc<Vec<u8>>> {
        None
    }
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

    // Optimized memory: width * height * 4 (the final RGBA output is the main consumer)
    let rgba = width as u64 * height as u64 * 4;
    Ok((width, height, channels, rgba))
}
