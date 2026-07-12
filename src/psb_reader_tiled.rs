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

//! Disk-tiled PSD/PSB source for huge files.
//!
//! Split out of `psb_reader` (see `docs/coding-rules.md` #12): decodes
//! regions on demand from a memory-mapped file instead of materializing the
//! full flattened composite in memory. Row-level helpers shared with the
//! full in-memory composite path (`channel_is_used`, `interleave_row_rgba8`,
//! `downconvert_samples_to_u8`, ...) stay in `psb_reader`.

use memmap2::Mmap;
use std::cell::RefCell;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::{Arc, OnceLock};

use simple_image_viewer::simd_swizzle;

use crate::psb_reader::{
    bytes_per_sample, channel_is_used, checked_section_end, cmyk_to_rgb, downconvert_samples_to_u8,
    extract_icc_profile_from_ir, max_rle_compressed_row_bytes, read_u16, read_u32, read_u64,
    seek_forward_within, tiled_compression_supported, unpack_bits_into, validate_psd_dimensions,
    validate_rle_row_counts,
};

type TiledRowBatch = Vec<(u32, Arc<Vec<u8>>)>;

/// Tiled source for PSD/PSB files that decodes regions on demand from a memory-mapped file.
/// Row cache is a moka LRU keyed by (channel, row); cached rows are already converted to 8-bit.
pub struct PsbTiledSource {
    #[allow(dead_code)]
    path: std::path::PathBuf,
    mmap: Arc<Mmap>,
    width: u32,
    height: u32,
    channels: u32,
    color_mode: u16,
    /// Bits per channel: 8, 16, or 32.
    depth: u16,
    #[allow(dead_code)]
    is_psb: bool,
    compression: u16,
    /// Absolute file offsets for the start of each row's data.
    /// Index: ch_idx * height + row_idx
    row_offsets: Vec<u64>,
    /// Always `None` on this SDR disk-tiled path. ZIP / ZIP+prediction Image
    /// Data is a single zlib stream without per-row offsets, so
    /// [`crate::psb_reader::tiled_compression_supported`] rejects compression
    /// 2|3 before construction. The `2 | 3` arm in [`Self::decode_row`] is a
    /// defensive dead path that fails closed if a caller ever bypasses that
    /// gate -- it must not silently zero-fill.
    zip_planar: Option<Arc<Vec<u8>>>,
    /// Concurrent LRU cache for decompressed 8-bit rows.
    /// Only successful rows are inserted; failed rows are never cached as zeros.
    row_cache: moka::sync::Cache<(u32, u32), Arc<Vec<u8>>>,
    /// First row-decode failure for this source (sticky). Tile/preview paths
    /// fail closed to empty output instead of caching white/black placeholders.
    row_decode_error: OnceLock<String>,
    /// Resolved CMYK ICC bytes (embedded IR 1039 or bundled default). Empty when not CMYK.
    cmyk_icc: Arc<[u8]>,
}

impl PsbTiledSource {
    #[inline]
    pub fn color_mode(&self) -> u16 {
        self.color_mode
    }

    #[inline]
    fn bytes_per_sample(&self) -> usize {
        (self.depth / 8) as usize
    }

    #[inline]
    fn raw_row_bytes(&self) -> Result<usize, String> {
        (self.width as usize)
            .checked_mul(self.bytes_per_sample())
            .ok_or_else(|| "PSD/PSB tiled raw row byte count overflow".to_string())
    }

    /// Resolve a stored file offset to a `usize` index, rejecting values that
    /// cannot address this process's address space (truncating `as usize` would
    /// be unsafe on 32-bit and for absurd u64 values).
    #[inline]
    fn row_file_offset(&self, idx: usize) -> Option<usize> {
        let &o = self.row_offsets.get(idx)?;
        usize::try_from(o).ok()
    }

    /// Write one decompressed 8-bit row into `buf` (length must be `self.width`).
    fn decode_row_into(
        &self,
        buf: &mut Vec<u8>,
        ch_idx: u32,
        global_row: u32,
    ) -> Result<(), String> {
        let out_len = self.width as usize;
        // Release builds must keep this check: a short `buf` would OOB in downconvert.
        if buf.len() != out_len {
            return Err(format!(
                "PSD/PSB tiled row destination length mismatch: got {}, expected {out_len}",
                buf.len()
            ));
        }
        let raw_len = self.raw_row_bytes().inspect_err(|_| buf.fill(0))?;
        let bps = self.bytes_per_sample();

        let idx = (ch_idx as usize)
            .checked_mul(self.height as usize)
            .and_then(|base| base.checked_add(global_row as usize))
            .ok_or_else(|| {
                buf.fill(0);
                "PSD/PSB tiled row index overflow".to_string()
            })?;
        match self.compression {
            0 => {
                // Raw mode must have a populated offset table (ZIP leaves it empty
                // and is refused at open; still guard against a hollow table).
                if self.row_offsets.is_empty() {
                    buf.fill(0);
                    return Err("PSD/PSB tiled raw row offset table is empty".to_string());
                }
                let Some(offset) = self.row_file_offset(idx) else {
                    buf.fill(0);
                    return Err("PSD/PSB tiled raw row offset is missing".to_string());
                };
                let Some(end) = offset.checked_add(raw_len) else {
                    buf.fill(0);
                    return Err("PSD/PSB tiled raw row end overflow".to_string());
                };
                if end > self.mmap.len() {
                    buf.fill(0);
                    return Err("PSD/PSB tiled raw row is out of bounds".to_string());
                }
                downconvert_samples_to_u8(buf, &self.mmap[offset..end], bps);
            }
            1 => {
                let Some(offset) = self.row_file_offset(idx) else {
                    buf.fill(0);
                    return Err("PSD/PSB tiled RLE row offset is missing".to_string());
                };
                let next_offset = if let Some(next_idx) = idx
                    .checked_add(1)
                    .filter(|&next| next < self.row_offsets.len())
                {
                    match usize::try_from(self.row_offsets[next_idx]) {
                        Ok(v) => v,
                        Err(_) => {
                            buf.fill(0);
                            return Err("PSD/PSB tiled RLE next row offset overflow".to_string());
                        }
                    }
                } else {
                    self.mmap.len()
                };
                if offset >= self.mmap.len()
                    || next_offset > self.mmap.len()
                    || next_offset <= offset
                {
                    buf.fill(0);
                    return Err("PSD/PSB tiled RLE row range is invalid".to_string());
                }
                let compressed = &self.mmap[offset..next_offset];
                if bps == 1 {
                    unpack_bits_into(buf, compressed, out_len).map_err(|e| {
                        buf.fill(0);
                        format!("PSD/PSB tiled RLE row decode failed: {e}")
                    })?;
                } else {
                    // Separate TLS from PSB_ROW_SCRATCH (already borrowed by caller).
                    with_psb_raw_row_scratch(raw_len, |raw| {
                        unpack_bits_into(raw, compressed, raw_len).map_err(|e| {
                            buf.fill(0);
                            format!("PSD/PSB tiled RLE row decode failed: {e}")
                        })?;
                        downconvert_samples_to_u8(buf, raw, bps);
                        Ok::<(), String>(())
                    })?;
                }
            }
            2 | 3 => {
                let Some(planar) = self.zip_planar.as_ref() else {
                    buf.fill(0);
                    return Err("PSD/PSB tiled ZIP planar data is unavailable".to_string());
                };
                let Some(offset) = self.row_file_offset(idx) else {
                    buf.fill(0);
                    return Err("PSD/PSB tiled ZIP row offset is missing".to_string());
                };
                let Some(end) = offset.checked_add(raw_len) else {
                    buf.fill(0);
                    return Err("PSD/PSB tiled ZIP row end overflow".to_string());
                };
                if end > planar.len() {
                    buf.fill(0);
                    return Err("PSD/PSB tiled ZIP row is out of bounds".to_string());
                }
                downconvert_samples_to_u8(buf, &planar[offset..end], bps);
            }
            _ => {
                buf.fill(0);
                return Err(format!(
                    "PSD/PSB tiled row has unsupported compression {}",
                    self.compression
                ));
            }
        }
        Ok(())
    }

    /// Record the first row-decode failure and log it once at warn.
    fn note_row_decode_error(&self, ch_idx: u32, global_row: u32, err: &str) {
        let msg = format!(
            "PSD/PSB tiled row decode failed for channel {ch_idx}, row {global_row}: {err}"
        );
        if self.row_decode_error.set(msg.clone()).is_ok() {
            log::warn!("{msg}");
        }
    }

    /// Decode a single row without touching the cache. Pure computation.
    fn decode_row_unlocked(&self, ch_idx: u32, global_row: u32) -> Result<Vec<u8>, String> {
        let row_len = self.width as usize;
        PSB_ROW_SCRATCH.with(|scratch| {
            let mut scratch = scratch.borrow_mut();
            prepare_psb_row_buf(&mut scratch, row_len);
            self.decode_row_into(&mut scratch, ch_idx, global_row)?;
            Ok(std::mem::replace(
                &mut *scratch,
                Vec::with_capacity(row_len),
            ))
        })
    }

    /// Get a decompressed row for a given channel and global row index.
    /// Successful rows are cached (moka coalesces concurrent inserts). Failures
    /// are sticky and never cached as zero-filled placeholders.
    fn get_row(&self, ch_idx: u32, global_row: u32) -> Result<Arc<Vec<u8>>, String> {
        if let Some(err) = self.row_decode_error.get() {
            return Err(err.clone());
        }
        let key = (ch_idx, global_row);
        if let Some(hit) = self.row_cache.get(&key) {
            return Ok(hit);
        }
        match self.decode_row_unlocked(ch_idx, global_row) {
            Ok(row) => {
                let arc = Arc::new(row);
                self.row_cache.insert(key, Arc::clone(&arc));
                Ok(arc)
            }
            Err(e) => {
                self.note_row_decode_error(ch_idx, global_row, &e);
                Err(e)
            }
        }
    }

    /// Batch-fetch rows for a tile. Each row is fetched through the moka cache
    /// which handles concurrent access, LRU eviction, and request coalescing.
    fn get_rows_batch(&self, ch_idx: u32, y: u32, h: u32) -> Result<TiledRowBatch, String> {
        let mut result = TiledRowBatch::with_capacity(h as usize);
        for row_in_tile in 0..h {
            let global_row = y + row_in_tile;
            if global_row >= self.height {
                continue;
            }
            let data = self.get_row(ch_idx, global_row)?;
            result.push((row_in_tile, data));
        }
        Ok(result)
    }

    fn extract_tile_inner(&self, x: u32, y: u32, w: u32, h: u32) -> Result<Vec<u8>, String> {
        let rgba_len = (w as usize)
            .checked_mul(h as usize)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| "PSD/PSB tiled RGBA length overflow".to_string())?;
        let mut rgba = vec![0u8; rgba_len];

        let mut row_grid = vec![vec![None; self.channels as usize]; h as usize];
        for ch in 0..self.channels {
            if !channel_is_used(self.color_mode, ch, self.channels) {
                continue;
            }
            let rows = self.get_rows_batch(ch, y, h)?;
            for (rel_y, data) in rows {
                if (rel_y as usize) < h as usize {
                    row_grid[rel_y as usize][ch as usize] = Some(data);
                }
            }
        }

        let start = x as usize;
        let end = (x + w) as usize;

        for (rel_y, src_channels) in row_grid.iter().enumerate().take(h as usize) {
            let Some(dst_start) = (rel_y as u64)
                .checked_mul(w as u64)
                .and_then(|n| n.checked_mul(4))
                .and_then(|n| usize::try_from(n).ok())
            else {
                continue;
            };
            let Some(dst_end) = dst_start.checked_add((w as usize).saturating_mul(4)) else {
                continue;
            };
            let Some(dst_row) = rgba.get_mut(dst_start..dst_end) else {
                continue;
            };
            interleave_tile_row_rgba8(
                dst_row,
                src_channels,
                self.color_mode,
                self.channels,
                start,
                end,
                self.cmyk_icc.as_ref(),
            );
        }
        Ok(rgba)
    }

    fn generate_preview_inner(
        &self,
        max_w: u32,
        max_h: u32,
    ) -> Result<(u32, u32, Vec<u8>), String> {
        let scale = (max_w as f64 / self.width as f64)
            .min(max_h as f64 / self.height as f64)
            .min(1.0);
        let out_w = (self.width as f64 * scale).round().max(1.0) as u32;
        let out_h = (self.height as f64 * scale).round().max(1.0) as u32;

        let pixel_len = out_w
            .checked_mul(out_h)
            .and_then(|pixels| pixels.checked_mul(4))
            .map(|len| len as usize)
            .ok_or_else(|| "PSD/PSB tiled preview length overflow".to_string())?;
        let mut pixels = vec![0u8; pixel_len];

        let x_map: Vec<usize> = (0..out_w)
            .map(|out_x| ((out_x as f64 / scale) as usize).min(self.width as usize - 1))
            .collect();

        // Reuse across preview rows; used slots are overwritten each iteration.
        let mut channel_rows: Vec<Option<Arc<Vec<u8>>>> = vec![None; self.channels as usize];
        for out_y in 0..out_h {
            let src_y = ((out_y as f64 / scale) as u32).min(self.height - 1);
            let Some(row_start_idx) = (out_y as usize).checked_mul(out_w as usize) else {
                continue;
            };

            // Fetch one full-width row of each used channel, then sample columns.
            for ch_idx in 0..self.channels {
                if channel_is_used(self.color_mode, ch_idx, self.channels) {
                    channel_rows[ch_idx as usize] = Some(self.get_row(ch_idx, src_y)?);
                }
            }

            for (out_x, &src_x) in x_map.iter().enumerate().take(out_w as usize) {
                let Some(dst_off) = row_start_idx
                    .checked_add(out_x)
                    .and_then(|idx| idx.checked_mul(4))
                else {
                    continue;
                };
                if dst_off + 3 >= pixels.len() {
                    continue;
                }
                let rgba = sample_pixel_rgba8(&channel_rows, self.color_mode, self.channels, src_x);
                pixels[dst_off..dst_off + 4].copy_from_slice(&rgba);
            }
        }

        Ok((out_w, out_h, pixels))
    }
}

/// Standalone path-based opener (maps the file itself). Prefer
/// [`open_tiled_source_from_mmap`] when the caller already holds an `Arc<Mmap>`
/// (checklist #29 / `load_psd`). Kept for tests and direct callers; production
/// `load_psd` uses the mmap-reusing entry.
#[allow(dead_code)]
pub fn open_tiled_source(path: &Path) -> Result<PsbTiledSource, String> {
    // On Windows, use FILE_FLAG_RANDOM_ACCESS to disable aggressive sequential
    // prefetching. Tile workers access scattered regions of a 6GB+ file -- the
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
    open_tiled_source_from_mmap(path, Arc::new(mmap))
}

/// Build a tiled source from an already-mapped file (checklist #29: avoid a
/// second open/mmap when `load_psd` already mapped the path).
pub fn open_tiled_source_from_mmap(path: &Path, mmap: Arc<Mmap>) -> Result<PsbTiledSource, String> {
    let mut cursor = std::io::Cursor::new(&mmap[..]);

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

    validate_psd_dimensions(width, height, channels)?;
    let bps = bytes_per_sample(depth)?;
    let row_raw_bytes = (width as u64)
        .checked_mul(bps as u64)
        .ok_or_else(|| "PSD/PSB row byte count overflow".to_string())?;

    // Skip Sections 2, 3, 4 (capture ICC from IR when present).
    let file_size = mmap.len() as u64;
    let cm_len = read_u32(&mut cursor)?;
    seek_forward_within(&mut cursor, cm_len as u64, file_size, "color mode data")
        .map_err(|e| e.to_string())?;
    let ir_len = read_u32(&mut cursor)? as u64;
    let ir_start = cursor.position();
    let ir_end = checked_section_end(ir_start, ir_len, file_size, "image resources")?;
    let embedded_icc = extract_icc_profile_from_ir(&mmap[..], ir_start, ir_end);
    seek_forward_within(&mut cursor, ir_len, file_size, "image resources")
        .map_err(|e| e.to_string())?;
    let lm_len = if is_psb {
        read_u64(&mut cursor)?
    } else {
        read_u32(&mut cursor)? as u64
    };
    seek_forward_within(&mut cursor, lm_len, file_size, "layer and mask info")
        .map_err(|e| e.to_string())?;

    let compression = read_u16(&mut cursor)?;
    tiled_compression_supported(compression)?;
    let row_counts_start = cursor.position();

    let mut row_offsets = Vec::with_capacity(channels as usize * height as usize);
    // ZIP (2|3) never reaches here: `tiled_compression_supported` rejected it.
    // Keep the field as `None` so the defensive decode_row arm stays fail-closed.
    let zip_planar: Option<Arc<Vec<u8>>> = None;

    match compression {
        0 => {
            let channel_bytes = row_raw_bytes
                .checked_mul(height as u64)
                .ok_or_else(|| "PSD/PSB channel byte count overflow".to_string())?;
            for ch in 0..channels {
                let ch_base = (ch as u64)
                    .checked_mul(channel_bytes)
                    .ok_or_else(|| "PSD/PSB channel offset overflow".to_string())?;
                for row in 0..height {
                    let row_off = (row as u64)
                        .checked_mul(row_raw_bytes)
                        .ok_or_else(|| "PSD/PSB row offset overflow".to_string())?;
                    let offset = row_counts_start
                        .checked_add(ch_base)
                        .and_then(|v| v.checked_add(row_off))
                        .ok_or_else(|| "PSD/PSB raw row offset overflow".to_string())?;
                    row_offsets.push(offset);
                }
            }
        }
        1 => {
            let total_rows = (channels as usize)
                .checked_mul(height as usize)
                .ok_or_else(|| "PSD/PSB row count overflow".to_string())?;
            let mut counts = Vec::with_capacity(total_rows);
            for _ in 0..total_rows {
                let cnt = if is_psb {
                    read_u32(&mut cursor)? as u64
                } else {
                    read_u16(&mut cursor)? as u64
                };
                counts.push(cnt);
            }
            let remaining = mmap.len().saturating_sub(cursor.position() as usize) as u64;
            let row_counts_usize: Vec<usize> = counts.iter().map(|&c| c as usize).collect();
            let max_row = max_rle_compressed_row_bytes(
                usize::try_from(row_raw_bytes)
                    .map_err(|_| "PSD/PSB RLE row byte count overflow".to_string())?,
            )?;
            validate_rle_row_counts(&row_counts_usize, remaining, max_row)?;
            let data_start = cursor.position();
            let mut running_offset = data_start;
            for cnt in counts {
                row_offsets.push(running_offset);
                running_offset += cnt;
            }
        }
        _ => {
            log::error!(
                "[{}] PSD/PSB: Unsupported compression method {}",
                path.display(),
                compression
            );
            return Err(format!("Unsupported compression: {compression}"));
        }
    }

    // Row cache: bounded by total decompressed bytes (`ROW_CACHE_BUDGET`), not entry count, so
    // ultra-wide rows cannot inflate the eviction budget (each entry weighs `decode_row.len()`).
    const ROW_CACHE_BUDGET: u64 = 512 * 1024 * 1024; // total decompressed row bytes

    let row_cache = moka::sync::Cache::builder()
        .max_capacity(ROW_CACHE_BUDGET)
        .weigher(|_key: &(u32, u32), value: &Arc<Vec<u8>>| {
            u32::try_from(value.len()).unwrap_or(u32::MAX)
        })
        .build();

    let cmyk_icc: Arc<[u8]> = if color_mode == 4 {
        Arc::<[u8]>::from(crate::psb_cmyk_cms::resolve_cmyk_icc(embedded_icc.as_deref()).to_vec())
    } else {
        Arc::from([])
    };

    Ok(PsbTiledSource {
        path: path.to_path_buf(),
        mmap,
        width,
        height,
        channels,
        color_mode,
        depth,
        is_psb,
        compression,
        row_offsets,
        zip_planar,
        row_cache,
        row_decode_error: OnceLock::new(),
        cmyk_icc,
    })
}

impl crate::loader::TiledImageSource for PsbTiledSource {
    fn width(&self) -> u32 {
        self.width
    }
    fn height(&self) -> u32 {
        self.height
    }

    fn extract_tile(&self, x: u32, y: u32, w: u32, h: u32) -> std::sync::Arc<Vec<u8>> {
        std::sync::Arc::new(self.extract_tile_inner(x, y, w, h).unwrap_or_default())
    }

    fn generate_preview(&self, max_w: u32, max_h: u32) -> (u32, u32, Vec<u8>) {
        self.generate_preview_inner(max_w, max_h)
            .unwrap_or_default()
    }

    fn full_pixels(&self) -> Option<Arc<Vec<u8>>> {
        None
    }
}

// Tile workers decode rows in parallel; per-thread scratch avoids per-row allocations.
thread_local! {
    static PSB_ROW_SCRATCH: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    // Intermediate BE samples for RLE+16/32-bit rows (must not share PSB_ROW_SCRATCH).
    static PSB_RAW_ROW_SCRATCH: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

#[inline]
fn prepare_psb_row_buf(buf: &mut Vec<u8>, row_len: usize) {
    buf.clear();
    buf.resize(row_len, 0);
}

/// Scratch for PackBits expand before 16/32-bit down-convert (separate TLS key).
fn with_psb_raw_row_scratch<R>(raw_len: usize, f: impl FnOnce(&mut Vec<u8>) -> R) -> R {
    PSB_RAW_ROW_SCRATCH.with(|scratch| {
        let mut scratch = scratch.borrow_mut();
        let cap = scratch.capacity();
        if cap < raw_len {
            scratch.reserve(raw_len - cap);
        }
        f(&mut scratch)
    })
}

fn interleave_tile_row_rgba8(
    dst_row: &mut [u8],
    src_channels: &[Option<Arc<Vec<u8>>>],
    color_mode: u16,
    channels: u32,
    start: usize,
    end: usize,
    cmyk_icc: &[u8],
) {
    let slice = |ch: usize| -> Option<&[u8]> {
        src_channels.get(ch).and_then(|o| {
            o.as_ref().map(|d| {
                let e = end.min(d.len());
                let s = start.min(e);
                &d[s..e]
            })
        })
    };

    match color_mode {
        4 if channels >= 4 => {
            if let (Some(c), Some(m), Some(y), Some(k)) = (slice(0), slice(1), slice(2), slice(3)) {
                let a = if channels >= 5 { slice(4) } else { None };
                let width = c.len().min(m.len()).min(y.len()).min(k.len());
                let icc = crate::psb_cmyk_cms::resolve_cmyk_icc(if cmyk_icc.is_empty() {
                    None
                } else {
                    Some(cmyk_icc)
                });
                let span = crate::psb_cmyk_cms::AdobeCmykSpan {
                    c: &c[..width],
                    m: &m[..width],
                    y: &y[..width],
                    k: &k[..width],
                    alpha: a.map(|buf| &buf[..width.min(buf.len())]),
                };
                if crate::psb_cmyk_cms::cmyk_span_adobe_to_rgba8(
                    &span,
                    icc,
                    &mut dst_row[..width * 4],
                ) {
                    return;
                }
                crate::psb_cmyk_simd::cmyk_planes_to_rgba8(
                    &c[..width],
                    &m[..width],
                    &y[..width],
                    &k[..width],
                    a.map(|buf| &buf[..width.min(buf.len())]),
                    &mut dst_row[..width * 4],
                );
            }
        }
        1 => {
            if let Some(gray) = slice(0) {
                if channels >= 2
                    && let Some(a) = slice(1)
                {
                    simd_swizzle::interleave_rgba(gray, gray, gray, a, dst_row);
                } else {
                    simd_swizzle::interleave_rgb_with_alpha(gray, gray, gray, 255, dst_row);
                }
            }
        }
        _ => {
            if let (Some(r), Some(g), Some(b)) = (slice(0), slice(1), slice(2)) {
                if channels >= 4
                    && let Some(a) = slice(3)
                {
                    simd_swizzle::interleave_rgba(r, g, b, a, dst_row);
                } else {
                    simd_swizzle::interleave_rgb_with_alpha(r, g, b, 255, dst_row);
                }
            }
        }
    }
}

fn sample_pixel_rgba8(
    channel_rows: &[Option<Arc<Vec<u8>>>],
    color_mode: u16,
    channels: u32,
    x: usize,
) -> [u8; 4] {
    let get = |ch: usize| -> u8 {
        channel_rows
            .get(ch)
            .and_then(|o| o.as_ref())
            .and_then(|d| d.get(x).copied())
            .unwrap_or(0)
    };
    match color_mode {
        4 if channels >= 4 => {
            let (r, g, b) = cmyk_to_rgb(get(0), get(1), get(2), get(3));
            let a = if channels >= 5 { get(4) } else { 255 };
            [r, g, b, a]
        }
        1 => {
            let v = get(0);
            let a = if channels >= 2 { get(1) } else { 255 };
            [v, v, v, a]
        }
        _ => {
            let a = if channels >= 4 { get(3) } else { 255 };
            [get(0), get(1), get(2), a]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::TiledImageSource;
    use std::io::Write;

    fn write_temp_psd(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "{name}-{}-{}.psd",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let mut file = std::fs::File::create(&path).expect("create temp psd");
        file.write_all(bytes).expect("write temp psd");
        path
    }

    /// 1x1 RGB RLE PSD whose PackBits payloads are truncated (open succeeds;
    /// row decode must fail closed).
    fn tiny_rle_rgb_corrupt_packbits() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"8BPS");
        bytes.extend_from_slice(&1u16.to_be_bytes());
        bytes.extend_from_slice(&[0u8; 6]);
        bytes.extend_from_slice(&3u16.to_be_bytes());
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.extend_from_slice(&8u16.to_be_bytes());
        bytes.extend_from_slice(&3u16.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&1u16.to_be_bytes()); // RLE
        for _ in 0..3 {
            bytes.extend_from_slice(&2u16.to_be_bytes());
        }
        // n=3 literal needs 4 payload bytes; only 1 follows -> decode Err.
        for _ in 0..3 {
            bytes.extend_from_slice(&[3u8, 1u8]);
        }
        bytes
    }

    #[test]
    fn tiled_row_decode_failure_returns_empty_tile() {
        let path = write_temp_psd("psb_tiled_corrupt_rle", &tiny_rle_rgb_corrupt_packbits());
        let source = open_tiled_source(&path).expect("open should accept counts-only validation");
        let tile = source.extract_tile(0, 0, 1, 1);
        assert!(
            tile.is_empty(),
            "row decode failure must fail closed (empty tile), not a zero/white placeholder"
        );
        let preview = source.generate_preview(64, 64);
        assert_eq!(preview, (0, 0, Vec::new()));
        let _ = std::fs::remove_file(path);
    }
}
