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

/// LRU cache for decompressed PSB rows.
/// Key: (channel_index, global_row) -> decompressed row bytes (exactly `width` bytes)
struct PsbRowCache {
    entries: std::collections::HashMap<(u32, u32), Arc<Vec<u8>>>,
    order: std::collections::VecDeque<(u32, u32)>,
    capacity: usize,
}

impl PsbRowCache {
    fn new(capacity: usize) -> Self {
        Self {
            entries: std::collections::HashMap::new(),
            order: std::collections::VecDeque::new(),
            capacity,
        }
    }

    fn get(&mut self, key: (u32, u32)) -> Option<Arc<Vec<u8>>> {
        if let Some(data) = self.entries.get(&key) {
            // Move to back (most recent)
            if let Some(pos) = self.order.iter().position(|k| *k == key) {
                self.order.remove(pos);
            }
            self.order.push_back(key);
            Some(Arc::clone(data))
        } else {
            None
        }
    }

    fn insert(&mut self, key: (u32, u32), data: Arc<Vec<u8>>) {
        if self.entries.contains_key(&key) {
            return;
        }
        while self.entries.len() >= self.capacity {
            if let Some(evicted) = self.order.pop_front() {
                self.entries.remove(&evicted);
            } else {
                break;
            }
        }
        self.entries.insert(key, data);
        self.order.push_back(key);
    }
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
    /// Cache of recently decompressed rows to avoid re-decoding for adjacent tiles.
    row_cache: std::sync::Mutex<PsbRowCache>,
}

impl PsbTiledSource {
    /// Decode a single row without touching the cache. Pure computation.
    fn decode_row_unlocked(&self, ch_idx: u32, global_row: u32) -> Vec<u8> {
        let idx = ch_idx as usize * self.height as usize + global_row as usize;
        match self.compression {
            0 => {
                let offset = match self.row_offsets.get(idx) {
                    Some(&o) => o as usize,
                    None => return vec![0u8; self.width as usize],
                };
                let end = offset + self.width as usize;
                if end <= self.mmap.len() {
                    self.mmap[offset..end].to_vec()
                } else {
                    vec![0u8; self.width as usize]
                }
            }
            1 => {
                let offset = match self.row_offsets.get(idx) {
                    Some(&o) => o as usize,
                    None => return vec![0u8; self.width as usize],
                };
                let next_offset = if (idx + 1) < self.row_offsets.len() {
                    self.row_offsets[idx + 1] as usize
                } else {
                    self.mmap.len()
                };
                if offset < self.mmap.len() && next_offset <= self.mmap.len() && next_offset > offset {
                    let compressed = &self.mmap[offset..next_offset];
                    let mut decompressed = unpack_bits(compressed, self.width as usize);
                    decompressed.resize(self.width as usize, 0);
                    decompressed
                } else {
                    vec![0u8; self.width as usize]
                }
            }
            _ => vec![0u8; self.width as usize],
        }
    }

    /// Get a decompressed row for a given channel and global row index.
    /// Used by generate_preview which processes one row at a time.
    fn get_row(&self, ch_idx: u32, global_row: u32) -> Arc<Vec<u8>> {
        let key = (ch_idx, global_row);
        {
            let mut cache = self.row_cache.lock().unwrap();
            if let Some(data) = cache.get(key) {
                return data;
            }
        }
        let row_data = self.decode_row_unlocked(ch_idx, global_row);
        let data = Arc::new(row_data);
        {
            let mut cache = self.row_cache.lock().unwrap();
            cache.insert(key, Arc::clone(&data));
        }
        data
    }

    /// Batch-fetch rows for a tile: single lock for lookup, decode misses without lock,
    /// single lock for insertion. Reduces lock ops from ~2048 to 2 per channel.
    fn get_rows_batch(&self, ch_idx: u32, y: u32, h: u32) -> Vec<(u32, Arc<Vec<u8>>)> {
        let mut result: Vec<(u32, Arc<Vec<u8>>)> = Vec::with_capacity(h as usize);
        let mut missing: Vec<u32> = Vec::new();

        // Phase 1: Single lock — batch cache lookup
        {
            let mut cache = self.row_cache.lock().unwrap();
            for row_in_tile in 0..h {
                let global_row = y + row_in_tile;
                if global_row >= self.height { continue; }
                let key = (ch_idx, global_row);
                if let Some(data) = cache.get(key) {
                    result.push((row_in_tile, data));
                } else {
                    missing.push(row_in_tile);
                }
            }
        }

        if missing.is_empty() {
            return result;
        }

        // Phase 2: Decode all missing rows WITHOUT holding the lock
        let decoded: Vec<(u32, Arc<Vec<u8>>)> = missing.iter().map(|&row_in_tile| {
            let global_row = y + row_in_tile;
            let data = Arc::new(self.decode_row_unlocked(ch_idx, global_row));
            (row_in_tile, data)
        }).collect();

        // Phase 3: Single lock — batch insert
        {
            let mut cache = self.row_cache.lock().unwrap();
            for (row_in_tile, data) in &decoded {
                let global_row = y + row_in_tile;
                cache.insert((ch_idx, global_row), Arc::clone(data));
            }
        }

        result.extend(decoded);
        result
    }
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
                         let idx = ch_idx as usize * height as usize + row as usize;
                         let len = *row_counts.get(idx).ok_or_else(|| format!("Row count index {idx} out of range"))?;
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
                    let idx = ch_idx as usize * height as usize + row as usize;
                    let compressed_len = *row_counts.get(idx).ok_or_else(|| format!("Row count index {idx} out of range"))?;
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
        _ => {
            log::error!("[{}] PSB: Unsupported compression method {}", path.display(), compression);
            return Err(format!("Unsupported compression: {compression}"));
        }
    }

    // Cache capacity: memory-budget-based to avoid blowing up RAM on ultra-wide images.
    // Budget: 128MB max. Each cached row is `width` bytes.
    // Minimum: channels * 512 rows (needed to process one tile without cache thrashing).
    const ROW_CACHE_BUDGET: usize = 128 * 1024 * 1024; // 128MB
    let row_bytes = width as usize;
    let budget_rows = ROW_CACHE_BUDGET / row_bytes.max(1);
    let min_rows = channels as usize * 512; // Must cover one full tile height per channel
    let cache_capacity = budget_rows.max(min_rows).min(channels as usize * 2048);

    Ok(PsbTiledSource {
        path: path.to_path_buf(),
        mmap: Arc::new(mmap),
        width, height, channels, color_mode, is_psb, compression,
        row_offsets,
        row_cache: std::sync::Mutex::new(PsbRowCache::new(cache_capacity)),
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

            // Batch: fetch all rows for this channel at once (2 lock ops instead of 512*2)
            let rows = self.get_rows_batch(ch_idx, y, h);
            
            let start = x as usize;
            let end = (x + w) as usize;
            
            for (row_in_tile, row_data) in &rows {
                let row_len = row_data.len();
                if start >= row_len { continue; }
                let actual_end = end.min(row_len);
                let data = &row_data[start..actual_end];
                let actual_w = data.len();
                
                let dst_row_start = *row_in_tile as usize * w as usize;
                
                if ch_target.len() == 1 {
                    let target = ch_target[0];
                    for col in 0..actual_w {
                        rgba[(dst_row_start + col) * 4 + target] = data[col];
                    }
                } else {
                    for col in 0..actual_w {
                        let base = (dst_row_start + col) * 4;
                        for &target in &ch_target {
                            rgba[base + target] = data[col];
                        }
                    }
                }
            }
        }
        rgba
    }

    fn generate_preview(&self, max_w: u32, max_h: u32) -> (u32, u32, Vec<u8>) {
        let scale = (max_w as f64 / self.width as f64).min(max_h as f64 / self.height as f64).min(1.0);
        let out_w = (self.width as f64 * scale).round().max(1.0) as u32;
        let out_h = (self.height as f64 * scale).round().max(1.0) as u32;
        
        let mut pixels = vec![255u8; (out_w * out_h * 4) as usize];
        
        // For each sampled row, decode once per channel and pick pixels
        for out_y in 0..out_h {
            let src_y = ((out_y as f64 / scale) as u32).min(self.height - 1);
            
            for ch_idx in 0..self.channels {
                let ch_target = match (self.color_mode, ch_idx) {
                    (1, 0) => vec![0usize, 1, 2],
                    (1, 1) => vec![3],
                    (3, 0) => vec![0],
                    (3, 1) => vec![1],
                    (3, 2) => vec![2],
                    (3, 3) => vec![3],
                    (_, 0..=2) => vec![ch_idx as usize],
                    _ => vec![],
                };
                if ch_target.is_empty() { continue; }
                
                let row_data = self.get_row(ch_idx, src_y);
                
                for out_x in 0..out_w {
                    let src_x = ((out_x as f64 / scale) as usize).min(row_data.len().saturating_sub(1));
                    let dst_off = (out_y as usize * out_w as usize + out_x as usize) * 4;
                    if src_x < row_data.len() {
                        let val = row_data[src_x];
                        for &target in &ch_target {
                            if dst_off + target < pixels.len() {
                                pixels[dst_off + target] = val;
                            }
                        }
                    }
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
