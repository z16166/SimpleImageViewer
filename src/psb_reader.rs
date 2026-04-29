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

use memmap2::Mmap;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

// SIMD architecture-specific imports are handled within submodules



use crate::simd_swizzle;

/// Decoded PSB composite image (Full in-memory).
#[allow(dead_code)]
pub struct PsbComposite {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>, // RGBA8
}

/// Row cache is now handled by moka::sync::Cache in PsbTiledSource.
/// Provides concurrent access without explicit locking, built-in LRU eviction,
/// and automatic coalescing of concurrent requests for the same key.

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
    /// Concurrent LRU cache for decompressed rows. Uses moka for lock-free
    /// access and automatic coalescing of concurrent requests for the same key.
    row_cache: moka::sync::Cache<(u32, u32), Arc<Vec<u8>>>,
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
                if offset < self.mmap.len()
                    && next_offset <= self.mmap.len()
                    && next_offset > offset
                {
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
    /// moka's get_with automatically coalesces concurrent requests: if two
    /// workers request the same row, only one decode runs.
    fn get_row(&self, ch_idx: u32, global_row: u32) -> Arc<Vec<u8>> {
        let key = (ch_idx, global_row);
        self.row_cache.get_with(key, || {
            Arc::new(self.decode_row_unlocked(ch_idx, global_row))
        })
    }

    /// Batch-fetch rows for a tile. Each row is fetched through the moka cache
    /// which handles concurrent access, LRU eviction, and request coalescing.
    fn get_rows_batch(&self, ch_idx: u32, y: u32, h: u32) -> Vec<(u32, Arc<Vec<u8>>)> {
        let mut result: Vec<(u32, Arc<Vec<u8>>)> = Vec::with_capacity(h as usize);
        for row_in_tile in 0..h {
            let global_row = y + row_in_tile;
            if global_row >= self.height {
                continue;
            }
            let data = self.get_row(ch_idx, global_row);
            result.push((row_in_tile, data));
        }
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
    r.read_exact(&mut sig)
        .map_err(|e| format!("Read error: {e}"))?;
    if &sig != b"8BPS" {
        return Err("Not a PSD/PSB file (invalid signature)".into());
    }

    let version = read_u16(&mut r)?;
    if version != 1 && version != 2 {
        return Err(format!("Unknown PSD/PSB version: {version}"));
    }
    let is_psb = version == 2;

    // 6 bytes reserved
    r.seek(SeekFrom::Current(6))
        .map_err(|e| format!("Seek error: {e}"))?;

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
        width,
        height,
        channels,
        depth,
        color_mode,
        version
    );

    // ── Section 2: Color Mode Data ─────────────────────────────────
    let cm_len = read_u32(&mut r)?;
    r.seek(SeekFrom::Current(cm_len as i64))
        .map_err(|e| format!("Seek error: {e}"))?;

    // ── Section 3: Image Resources ─────────────────────────────────
    let ir_len = read_u32(&mut r)?;
    r.seek(SeekFrom::Current(ir_len as i64))
        .map_err(|e| format!("Seek error: {e}"))?;

    // ── Section 4: Layer and Mask Information ───────────────────────
    // PSD uses u32 length, PSB uses u64 length
    let lm_len = if is_psb {
        read_u64(&mut r)?
    } else {
        read_u32(&mut r)? as u64
    };
    r.seek(SeekFrom::Current(lm_len as i64))
        .map_err(|e| format!("Seek error: {e}"))?;

    // ── Section 5: Image Data (the flattened composite) ────────────
    let compression = read_u16(&mut r)?;

    // Interleave channels directly into RGBA
    let pixel_count = width as usize * height as usize;

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

    // Step 1: Read all channel data into planar buffers (Sequential I/O)
    let mut planar_channels: Vec<Option<Vec<u8>>> = vec![None; channels as usize];
    for ch_idx in 0..channels {
        let is_used = match (color_mode, ch_idx) {
            (1, 0..=1) => true, // Gray, Alpha
            (3, 0..=3) => true, // R, G, B, Alpha
            (_, 0..=2) => true, // Generic R,G,B
            _ => false,
        };

        if is_used {
            let mut ch_data = vec![0u8; pixel_count];
            match compression {
                0 => {
                    r.read_exact(&mut ch_data)
                        .map_err(|e| format!("Read raw channel {ch_idx}: {e}"))?;
                }
                1 => {
                    for row in 0..height as usize {
                        let idx = ch_idx as usize * height as usize + row;
                        let compressed_len = *row_counts
                            .get(idx)
                            .ok_or_else(|| format!("Row count index {idx} out of range"))?;
                        let mut compressed = vec![0u8; compressed_len];
                        r.read_exact(&mut compressed)
                            .map_err(|e| format!("Read RLE: {e}"))?;
                        let decompressed = unpack_bits(&compressed, width as usize);
                        let dst_start = row * width as usize;
                        let copy_len = decompressed.len().min(width as usize);
                        ch_data[dst_start..dst_start + copy_len]
                            .copy_from_slice(&decompressed[..copy_len]);
                    }
                }
                _ => return Err(format!("Unsupported compression: {compression}")),
            }
            planar_channels[ch_idx as usize] = Some(ch_data);
        } else {
            // Skip unused channel
            match compression {
                0 => {
                    r.seek(SeekFrom::Current(pixel_count as i64))
                        .map_err(|e| format!("Skip raw: {e}"))?;
                }
                1 => {
                    for row in 0..height {
                        let idx = ch_idx as usize * height as usize + row as usize;
                        let len = *row_counts
                            .get(idx)
                            .ok_or_else(|| format!("Row count index {idx} out of range"))?;
                        r.seek(SeekFrom::Current(len as i64))
                            .map_err(|e| format!("Skip RLE: {e}"))?;
                    }
                }
                _ => {}
            }
        }
    }

    // Step 2: Interleave planar buffers into final RGBA buffer (SIMD Swizzling)
    let mut rgba = vec![255u8; pixel_count * 4];
    for row in 0..height as usize {
        let start = row * width as usize;
        let end = start + width as usize;
        let dst_row = &mut rgba[row * width as usize * 4..(row + 1) * width as usize * 4];

        match (color_mode, channels) {
            (3, 3) | (3, 4) | (_, 3) | (_, 4) => {
                let r = planar_channels[0].as_ref().map(|d| &d[start..end]);
                let g = planar_channels[1].as_ref().map(|d| &d[start..end]);
                let b = planar_channels[2].as_ref().map(|d| &d[start..end]);
                if let (Some(r), Some(g), Some(b)) = (r, g, b) {
                    if channels >= 4 && planar_channels.get(3).and_then(|c| c.as_ref()).is_some() {
                        let a = planar_channels[3].as_ref().unwrap();
                        simd_swizzle::interleave_rgba(r, g, b, &a[start..end], dst_row);
                    } else {
                        simd_swizzle::interleave_rgb_with_alpha(r, g, b, 255, dst_row);
                    }
                }
            }
            (1, 1) | (1, 2) => {
                if let Some(gray) = &planar_channels[0] {
                    let g_row = &gray[start..end];
                    let a_row = if channels >= 2 {
                        planar_channels
                            .get(1)
                            .and_then(|c| c.as_ref())
                            .map(|d| &d[start..end])
                    } else {
                        None
                    };
                    for (col, &v) in g_row.iter().enumerate() {
                        let base = col * 4;
                        dst_row[base] = v;
                        dst_row[base + 1] = v;
                        dst_row[base + 2] = v;
                        if let Some(a) = a_row {
                            if col < a.len() {
                                dst_row[base + 3] = a[col];
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    Ok(PsbComposite {
        width,
        height,
        pixels: rgba,
    })
}

/// Initialize a tiled source for a PSB file.
pub fn open_tiled_source(path: &Path) -> Result<PsbTiledSource, String> {
    // On Windows, use FILE_FLAG_RANDOM_ACCESS to disable aggressive sequential
    // prefetching. Tile workers access scattered regions of a 6GB+ file — the
    // default sequential read-ahead causes workers' prefetched pages to evict
    // each other from the OS page cache, creating a "prefetch storm".
    let file = {
        let mut opts = std::fs::OpenOptions::new();
        opts.read(true);
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::fs::OpenOptionsExt;
            const FILE_FLAG_RANDOM_ACCESS: u32 = 0x10000000;
            opts.custom_flags(FILE_FLAG_RANDOM_ACCESS);
        }
        opts.open(path)
            .map_err(|e| format!("Cannot open file: {e}"))?
    };
    let mmap = unsafe { Mmap::map(&file).map_err(|e| format!("Mmap failed: {e}"))? };
    let mut cursor = std::io::Cursor::new(&mmap[..]);

    // 1. Parse Headers (same as read_composite but on cursor)
    let mut sig = [0u8; 4];
    cursor.read_exact(&mut sig).map_err(|e| e.to_string())?;
    let version = read_u16(&mut cursor)?;
    let is_psb = version == 2;
    cursor
        .seek(SeekFrom::Current(6))
        .map_err(|e| e.to_string())?;
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
    cursor
        .seek(SeekFrom::Current(cm_len as i64))
        .map_err(|e| e.to_string())?;
    let ir_len = read_u32(&mut cursor)?;
    cursor
        .seek(SeekFrom::Current(ir_len as i64))
        .map_err(|e| e.to_string())?;
    let lm_len = if is_psb {
        read_u64(&mut cursor)?
    } else {
        read_u32(&mut cursor)? as u64
    };
    cursor
        .seek(SeekFrom::Current(lm_len as i64))
        .map_err(|e| e.to_string())?;

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
                    row_offsets.push(
                        row_counts_start + (ch as u64 * pixel_count) + (row as u64 * width as u64),
                    );
                }
            }
        }
        1 => {
            // RLE: row counts follow, then the data
            let total_rows = channels as usize * height as usize;
            let mut counts = Vec::with_capacity(total_rows);
            for _ in 0..total_rows {
                let cnt = if is_psb {
                    read_u32(&mut cursor)? as u64
                } else {
                    read_u16(&mut cursor)? as u64
                };
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
            log::error!(
                "[{}] PSB: Unsupported compression method {}",
                path.display(),
                compression
            );
            return Err(format!("Unsupported compression: {compression}"));
        }
    }

    // Cache capacity: must hold at least (workers × tile_height × channels) rows
    // to avoid thrashing when multiple workers decode tiles concurrently.
    // moka handles LRU eviction and concurrent access internally.
    let tile_size = crate::tile_cache::get_tile_size() as usize;
    let worker_count = std::thread::available_parallelism()
        .map(|n| (n.get() / 2).clamp(4, 12))
        .unwrap_or(4);
    const ROW_CACHE_BUDGET: usize = 512 * 1024 * 1024; // 512MB
    let row_bytes = width as usize;
    let budget_rows = ROW_CACHE_BUDGET / row_bytes.max(1);
    let min_rows = channels as usize * tile_size * worker_count;
    let cache_capacity = budget_rows.max(min_rows);

    let row_cache = moka::sync::Cache::new(cache_capacity as u64);

    Ok(PsbTiledSource {
        path: path.to_path_buf(),
        mmap: Arc::new(mmap),
        width,
        height,
        channels,
        color_mode,
        is_psb,
        compression,
        row_offsets,
        row_cache,
    })
}

impl crate::loader::TiledImageSource for PsbTiledSource {
    fn width(&self) -> u32 {
        self.width
    }
    fn height(&self) -> u32 {
        self.height
    }

    fn extract_tile(&self, x: u32, y: u32, w: u32, h: u32) -> Vec<u8> {
        let mut rgba = vec![255u8; (w * h * 4) as usize];

        // 1. Group rows by tile-relative Y for all channels
        // This allows us to process all channels for a single row together (SIMD-friendly)
        let mut row_grid = vec![vec![None; self.channels as usize]; h as usize];
        for ch in 0..self.channels {
            let rows = self.get_rows_batch(ch, y, h);
            for (rel_y, data) in rows {
                if (rel_y as usize) < h as usize {
                    row_grid[rel_y as usize][ch as usize] = Some(data);
                }
            }
        }

        let start = x as usize;
        let end = (x + w) as usize;

        // Pre-calculate channel target offsets to avoid match/vec overhead in hot loops
        let channel_mappings: Vec<Vec<usize>> = (0..self.channels)
            .map(|ch_idx| match (self.color_mode, ch_idx) {
                (1, 0) => vec![0, 1, 2],
                (1, 1) => vec![3],
                (3, 0) => vec![0],
                (3, 1) => vec![1],
                (3, 2) => vec![2],
                (3, 3) => vec![3],
                (_, 0..=2) => vec![ch_idx as usize],
                _ => vec![],
            })
            .collect();

        // 2. Swizzle row-by-row
        for rel_y in 0..h as usize {
            let dst_row = &mut rgba[rel_y * w as usize * 4..(rel_y + 1) * w as usize * 4];
            let src_channels = &row_grid[rel_y];

            // Optimized fast-paths for common color modes
            let mut processed = false;
            match (self.color_mode, self.channels) {
                (3, 3) | (3, 4) => {
                    // RGB or RGBA (Mode 3)
                    let r = src_channels[0]
                        .as_ref()
                        .map(|d| &d[start..end.min(d.len())]);
                    let g = src_channels[1]
                        .as_ref()
                        .map(|d| &d[start..end.min(d.len())]);
                    let b = src_channels[2]
                        .as_ref()
                        .map(|d| &d[start..end.min(d.len())]);

                    if let (Some(r), Some(g), Some(b)) = (r, g, b) {
                        if self.channels == 4 {
                            if let Some(a) = src_channels[3]
                                .as_ref()
                                .map(|d| &d[start..end.min(d.len())])
                            {
                                simd_swizzle::interleave_rgba(r, g, b, a, dst_row);
                                processed = true;
                            }
                        } else {
                            simd_swizzle::interleave_rgb_with_alpha(r, g, b, 255, dst_row);
                            processed = true;
                        }
                    }
                }
                (1, 1) | (1, 2) => {
                    // Grayscale (Mode 1)
                    if let Some(gray) = src_channels[0]
                        .as_ref()
                        .map(|d| &d[start..end.min(d.len())])
                    {
                        let alpha = if self.channels == 2 {
                            src_channels[1]
                                .as_ref()
                                .map(|d| &d[start..end.min(d.len())])
                        } else {
                            None
                        };

                        for (col, &v) in gray.iter().enumerate() {
                            let base = col * 4;
                            dst_row[base] = v;
                            dst_row[base + 1] = v;
                            dst_row[base + 2] = v;
                            if let Some(a_buf) = alpha {
                                if col < a_buf.len() {
                                    dst_row[base + 3] = a_buf[col];
                                }
                            }
                        }
                        processed = true;
                    }
                }
                _ => {}
            }

            if !processed {
                // Scalar fallback for complex channel mappings
                for ch_idx in 0..self.channels {
                    let ch_target = &channel_mappings[ch_idx as usize];
                    if ch_target.is_empty() {
                        continue;
                    }

                    if let Some(row_data) = &src_channels[ch_idx as usize] {
                        let row_len = row_data.len();
                        if start < row_len {
                            let actual_end = end.min(row_len);
                            let data = &row_data[start..actual_end];
                            for (col, &val) in data.iter().enumerate() {
                                let base = col * 4;
                                for &target in ch_target {
                                    dst_row[base + target] = val;
                                }
                            }
                        }
                    }
                }
            }
        }
        rgba
    }

    fn generate_preview(&self, max_w: u32, max_h: u32) -> (u32, u32, Vec<u8>) {
        let scale = (max_w as f64 / self.width as f64)
            .min(max_h as f64 / self.height as f64)
            .min(1.0);
        let out_w = (self.width as f64 * scale).round().max(1.0) as u32;
        let out_h = (self.height as f64 * scale).round().max(1.0) as u32;

        let mut pixels = vec![255u8; (out_w * out_h * 4) as usize];

        // 1. Pre-calculate channel mappings to avoid hot-loop allocations
        let channel_mappings: Vec<Vec<usize>> = (0..self.channels)
            .map(|ch_idx| match (self.color_mode, ch_idx) {
                (1, 0) => vec![0usize, 1, 2],
                (1, 1) => vec![3],
                (3, 0) => vec![0],
                (3, 1) => vec![1],
                (3, 2) => vec![2],
                (3, 3) => vec![3],
                (_, 0..=2) => vec![ch_idx as usize],
                _ => vec![],
            })
            .collect();

        // 2. Pre-calculate X indices to avoid floating point math in inner loops
        let x_map: Vec<usize> = (0..out_w)
            .map(|out_x| ((out_x as f64 / scale) as usize).min(self.width as usize - 1))
            .collect();

        // For each sampled row, decode once per channel and pick pixels
        for out_y in 0..out_h {
            let src_y = ((out_y as f64 / scale) as u32).min(self.height - 1);
            let row_start_idx = out_y as usize * out_w as usize;

            for ch_idx in 0..self.channels {
                let ch_target = &channel_mappings[ch_idx as usize];
                if ch_target.is_empty() {
                    continue;
                }

                let row_data = self.get_row(ch_idx, src_y);
                let row_len = row_data.len();

                for out_x in 0..out_w as usize {
                    let src_x = x_map[out_x];
                    if src_x < row_len {
                        let val = row_data[src_x];
                        let dst_off = (row_start_idx + out_x) * 4;
                        for &target in ch_target {
                            pixels[dst_off + target] = val;
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
                let remaining = expected_len.saturating_sub(result.len());
                let actual_count = count.min(remaining);
                if actual_count > 0 {
                    result.resize(result.len() + actual_count, val);
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
    r.read_exact(&mut buf)
        .map_err(|e| format!("Read u16: {e}"))?;
    Ok(u16::from_be_bytes(buf))
}

fn read_u32(r: &mut impl Read) -> Result<u32, String> {
    let mut buf = [0u8; 4];
    r.read_exact(&mut buf)
        .map_err(|e| format!("Read u32: {e}"))?;
    Ok(u32::from_be_bytes(buf))
}

fn read_u64(r: &mut impl Read) -> Result<u64, String> {
    let mut buf = [0u8; 8];
    r.read_exact(&mut buf)
        .map_err(|e| format!("Read u64: {e}"))?;
    Ok(u64::from_be_bytes(buf))
}

/// Estimate the memory required to decode a PSD/PSB composite (in bytes).
/// Returns (width, height, channels, estimated_bytes) or an error.
pub fn estimate_memory(path: &Path) -> Result<(u32, u32, u32, u64), String> {
    let file = std::fs::File::open(path).map_err(|e| format!("Cannot open file: {e}"))?;
    let mut r = BufReader::new(file);

    let mut sig = [0u8; 4];
    r.read_exact(&mut sig)
        .map_err(|e| format!("Read error: {e}"))?;
    if &sig != b"8BPS" {
        return Err("Not a PSD/PSB file".into());
    }
    let _version = read_u16(&mut r)?;
    r.seek(SeekFrom::Current(6))
        .map_err(|e| format!("Seek error: {e}"))?;
    let channels = read_u16(&mut r)? as u32;
    let height = read_u32(&mut r)?;
    let width = read_u32(&mut r)?;

    // Optimized memory: width * height * 4 (the final RGBA output is the main consumer)
    let rgba = width as u64 * height as u64 * 4;
    Ok((width, height, channels, rgba))
}
