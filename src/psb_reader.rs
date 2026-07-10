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

//! Minimal PSD/PSB flattened-composite reader for viewing.
//!
//! Extracts only the Image Data section (merged composite). Layers, masks, and
//! image resources are skipped. Supports PSD (v1) and PSB (v2), channel depths
//! 8/16/32 (down-converted to RGBA8 for display), and RGB / Grayscale / CMYK.
//!
//! PSB differs from PSD mainly in: version = 2, some lengths are u64, and RLE
//! row byte counts are u32 instead of u16.
//!
//! Reference: Adobe Photoshop File Formats Specification (March 2013)

use memmap2::Mmap;
use std::cell::RefCell;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

// SIMD architecture-specific imports are handled within submodules

use simple_image_viewer::simd_swizzle;

/// Adobe Photoshop PSD/PSB maximum canvas dimension (pixels per side).
const PSD_MAX_DIMENSION: u32 = 300_000;
/// Adobe Photoshop PSD/PSB maximum channel count.
const PSD_MAX_CHANNELS: u32 = 56;
/// Bytes per RGBA pixel when assembling the composite image.
const RGBA_BYTES_PER_PIXEL: usize = 4;
/// Layer section large enough that a solid-fill composite is almost certainly layers-only.
const LAYERS_ONLY_LM_MIN_BYTES: u64 = 1_000_000;
/// Photoshop Image Resource IDs for embedded JPEG thumbnails.
const IR_THUMBNAIL_PS4: u16 = 1033;
const IR_THUMBNAIL_PS5: u16 = 1036;

/// User-facing error when the flattened composite is empty and no IR thumbnail exists.
pub const EMPTY_COMPOSITE_ERROR: &str = "PSD flattened composite is empty (layers-only file). \
Re-save in Photoshop with Maximize Compatibility enabled, or export to PNG/JPEG.";

/// Result of a cheap probe for layers-only PSD files (solid-fill composite).
#[derive(Debug)]
pub enum LayersOnlyCompositeProbe {
    /// Composite RLE does not look like a solid fill -- proceed with normal decode.
    NotApplicable,
    /// Solid-fill composite + large layer section -- run layer compositor.
    NeedsLayerComposite,
}

/// Decoded PSD/PSB composite image (full in-memory RGBA8).
#[derive(Debug)]
#[allow(dead_code)]
pub struct PsbComposite {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>, // RGBA8
}

/// Tiled source for PSD/PSB files that decodes regions on demand from a memory-mapped file.
/// Row cache is a moka LRU keyed by (channel, row); cached rows are already converted to 8-bit.
pub struct PsbTiledSource {
    #[allow(dead_code)]
    path: PathBuf,
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
    /// Concurrent LRU cache for decompressed 8-bit rows.
    row_cache: moka::sync::Cache<(u32, u32), Arc<Vec<u8>>>,
}

impl PsbTiledSource {
    #[inline]
    fn bytes_per_sample(&self) -> usize {
        (self.depth / 8) as usize
    }

    #[inline]
    fn raw_row_bytes(&self) -> usize {
        self.width as usize * self.bytes_per_sample()
    }

    /// Write one decompressed 8-bit row into `buf` (length must be `self.width`).
    fn decode_row_into(&self, buf: &mut Vec<u8>, ch_idx: u32, global_row: u32) {
        let out_len = self.width as usize;
        debug_assert_eq!(buf.len(), out_len);
        let raw_len = self.raw_row_bytes();
        let bps = self.bytes_per_sample();

        let idx = ch_idx as usize * self.height as usize + global_row as usize;
        match self.compression {
            0 => {
                let offset = match self.row_offsets.get(idx) {
                    Some(&o) => o as usize,
                    None => return,
                };
                let end = offset + raw_len;
                if end <= self.mmap.len() {
                    downconvert_samples_to_u8(buf, &self.mmap[offset..end], bps);
                }
            }
            1 => {
                let offset = match self.row_offsets.get(idx) {
                    Some(&o) => o as usize,
                    None => return,
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
                    if bps == 1 {
                        unpack_bits_into(buf, compressed, out_len);
                    } else {
                        let mut raw = Vec::new();
                        unpack_bits_into(&mut raw, compressed, raw_len);
                        downconvert_samples_to_u8(buf, &raw, bps);
                    }
                }
            }
            _ => {}
        }
    }

    /// Decode a single row without touching the cache. Pure computation.
    fn decode_row_unlocked(&self, ch_idx: u32, global_row: u32) -> Vec<u8> {
        let row_len = self.width as usize;
        with_psb_row_scratch(row_len, |buf| self.decode_row_into(buf, ch_idx, global_row))
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

/// Read the flattened composite image from a PSD/PSB file path.
#[allow(dead_code)]
pub fn read_composite(path: &Path) -> Result<PsbComposite, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("Cannot open file: {e}"))?;
    let mmap = unsafe { Mmap::map(&file).map_err(|e| format!("Mmap failed: {e}"))? };
    read_composite_from_bytes(&mmap[..])
}

/// Read the flattened composite from an in-memory PSD/PSB byte slice (e.g. mmap).
pub fn read_composite_from_bytes(bytes: &[u8]) -> Result<PsbComposite, String> {
    read_composite_from_bytes_with_cancel(bytes, None)
}

/// Like [`read_composite_from_bytes`], but polls `cancel` in hot loops so long decodes
/// can abort when the loader drops the request (directory change, navigate away, etc.).
pub fn read_composite_from_bytes_with_cancel(
    bytes: &[u8],
    cancel: Option<&AtomicBool>,
) -> Result<PsbComposite, String> {
    let file_size = bytes.len() as u64;
    let mut r = std::io::Cursor::new(bytes);

    check_decode_cancel(cancel)?;

    // -- Section 1: File Header --
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

    r.seek(SeekFrom::Current(6))
        .map_err(|e| format!("Seek error: {e}"))?;

    let channels = read_u16(&mut r)? as u32;
    let height = read_u32(&mut r)?;
    let width = read_u32(&mut r)?;
    let depth = read_u16(&mut r)?;
    let color_mode = read_u16(&mut r)?;

    validate_psd_dimensions(width, height, channels)?;
    let bps = bytes_per_sample(depth)?;

    log::info!(
        "PSD/PSB header: {}x{}, {} channels, {}-bit, color_mode={}, version={}",
        width,
        height,
        channels,
        depth,
        color_mode,
        version
    );

    // -- Section 2: Color Mode Data --
    let cm_len = read_u32(&mut r)?;
    seek_forward(&mut r, cm_len as u64)?;

    // -- Section 3: Image Resources --
    let ir_len = read_u32(&mut r)?;
    seek_forward(&mut r, ir_len as u64)?;

    // -- Section 4: Layer and Mask Information --
    let lm_len = if is_psb {
        read_u64(&mut r)?
    } else {
        read_u32(&mut r)? as u64
    };
    seek_forward(&mut r, lm_len)?;

    check_decode_cancel(cancel)?;

    // -- Section 5: Image Data (flattened composite) --
    let compression = read_u16(&mut r)?;

    let pixel_count = checked_pixel_count(width, height)?;
    let samples_per_channel = pixel_count;
    let raw_channel_bytes = samples_per_channel
        .checked_mul(bps)
        .ok_or_else(|| "PSD/PSB channel byte count overflow".to_string())?;
    let row_raw_bytes = (width as usize)
        .checked_mul(bps)
        .ok_or_else(|| "PSD/PSB row byte count overflow".to_string())?;

    let total_rows = (height as usize)
        .checked_mul(channels as usize)
        .ok_or_else(|| "PSD/PSB row count overflow".to_string())?;
    let mut row_counts = Vec::new();
    if compression == 1 {
        row_counts.reserve(total_rows);
        for i in 0..total_rows {
            if i & 0x3FF == 0 {
                check_decode_cancel(cancel)?;
            }
            let count = if is_psb {
                read_u32(&mut r)? as usize
            } else {
                read_u16(&mut r)? as usize
            };
            row_counts.push(count);
        }
        let remaining = file_size.saturating_sub(
            r.stream_position()
                .map_err(|e| format!("Stream position error: {e}"))?,
        );
        validate_rle_total_bytes(&row_counts, remaining)?;
    }

    // Step 1: Read planar channels and down-convert to 8-bit samples.
    let mut planar_channels: Vec<Option<Vec<u8>>> = vec![None; channels as usize];
    for ch_idx in 0..channels {
        check_decode_cancel(cancel)?;
        let is_used = channel_is_used(color_mode, ch_idx, channels);

        if is_used {
            let mut ch_u8 = vec![0u8; samples_per_channel];
            match compression {
                0 => {
                    let mut raw = vec![0u8; raw_channel_bytes];
                    r.read_exact(&mut raw)
                        .map_err(|e| format!("Read raw channel {ch_idx}: {e}"))?;
                    check_decode_cancel(cancel)?;
                    downconvert_samples_to_u8(&mut ch_u8, &raw, bps);
                }
                1 => {
                    let mut row_raw = Vec::with_capacity(row_raw_bytes);
                    for row in 0..height as usize {
                        if row & 0x3F == 0 {
                            check_decode_cancel(cancel)?;
                        }
                        let idx = ch_idx as usize * height as usize + row;
                        let compressed_len = *row_counts
                            .get(idx)
                            .ok_or_else(|| format!("Row count index {idx} out of range"))?;
                        let mut compressed = vec![0u8; compressed_len];
                        r.read_exact(&mut compressed)
                            .map_err(|e| format!("Read RLE: {e}"))?;
                        unpack_bits_into(&mut row_raw, &compressed, row_raw_bytes);
                        let dst_start = row * width as usize;
                        let dst_end = dst_start + width as usize;
                        downconvert_samples_to_u8(&mut ch_u8[dst_start..dst_end], &row_raw, bps);
                    }
                }
                _ => return Err(format!("Unsupported compression: {compression}")),
            }
            planar_channels[ch_idx as usize] = Some(ch_u8);
        } else {
            match compression {
                0 => {
                    seek_forward(&mut r, raw_channel_bytes as u64)?;
                }
                1 => {
                    for row in 0..height {
                        if row & 0x3F == 0 {
                            check_decode_cancel(cancel)?;
                        }
                        let idx = ch_idx as usize * height as usize + row as usize;
                        let len = *row_counts
                            .get(idx)
                            .ok_or_else(|| format!("Row count index {idx} out of range"))?;
                        seek_forward(&mut r, len as u64)?;
                    }
                }
                _ => {}
            }
        }
    }

    // Step 2: Interleave into RGBA8.
    let mut rgba = vec![255u8; checked_rgba_len(pixel_count)?];
    for row in 0..height as usize {
        if row & 0x3F == 0 {
            check_decode_cancel(cancel)?;
        }
        let start = row * width as usize;
        let end = start + width as usize;
        let dst_row = &mut rgba[row * width as usize * 4..(row + 1) * width as usize * 4];
        interleave_row_rgba8(dst_row, &planar_channels, color_mode, channels, start, end);
    }

    Ok(PsbComposite {
        width,
        height,
        pixels: rgba,
    })
}

#[inline]
fn check_decode_cancel(cancel: Option<&AtomicBool>) -> Result<(), String> {
    if cancel.is_some_and(|c| c.load(Ordering::Acquire)) {
        Err(crate::loader::DECODE_CANCELLED.to_string())
    } else {
        Ok(())
    }
}

/// Cheap probe: solid-fill RLE composite + large layer section => layers-only file.
pub fn probe_layers_only_composite(bytes: &[u8]) -> Result<LayersOnlyCompositeProbe, String> {
    let file_size = bytes.len() as u64;
    let mut r = std::io::Cursor::new(bytes);

    let mut sig = [0u8; 4];
    r.read_exact(&mut sig)
        .map_err(|e| format!("Read error: {e}"))?;
    if &sig != b"8BPS" {
        return Ok(LayersOnlyCompositeProbe::NotApplicable);
    }
    let version = read_u16(&mut r)?;
    if version != 1 && version != 2 {
        return Ok(LayersOnlyCompositeProbe::NotApplicable);
    }
    let is_psb = version == 2;
    r.seek(SeekFrom::Current(6))
        .map_err(|e| format!("Seek error: {e}"))?;
    let channels = read_u16(&mut r)? as u32;
    let height = read_u32(&mut r)?;
    let width = read_u32(&mut r)?;
    let _depth = read_u16(&mut r)?;
    let _color_mode = read_u16(&mut r)?;
    validate_psd_dimensions(width, height, channels)?;

    let cm_len = read_u32(&mut r)?;
    seek_forward(&mut r, cm_len as u64)?;
    let ir_len = read_u32(&mut r)?;
    seek_forward(&mut r, ir_len as u64)?;
    let lm_len = if is_psb {
        read_u64(&mut r)?
    } else {
        read_u32(&mut r)? as u64
    };
    seek_forward(&mut r, lm_len)?;

    if lm_len < LAYERS_ONLY_LM_MIN_BYTES {
        return Ok(LayersOnlyCompositeProbe::NotApplicable);
    }

    let compression = read_u16(&mut r)?;
    if compression != 1 {
        return Ok(LayersOnlyCompositeProbe::NotApplicable);
    }

    let total_rows = (height as usize)
        .checked_mul(channels as usize)
        .ok_or_else(|| "PSD/PSB row count overflow".to_string())?;
    let mut row_counts = Vec::with_capacity(total_rows);
    for _ in 0..total_rows {
        let count = if is_psb {
            read_u32(&mut r)? as usize
        } else {
            read_u16(&mut r)? as usize
        };
        row_counts.push(count);
    }
    let remaining = file_size.saturating_sub(
        r.stream_position()
            .map_err(|e| format!("Stream position error: {e}"))?,
    );
    validate_rle_total_bytes(&row_counts, remaining)?;

    if !rle_row_counts_look_like_solid_fill(width as usize, &row_counts) {
        return Ok(LayersOnlyCompositeProbe::NotApplicable);
    }

    log::warn!(
        "PSD/PSB {}x{} has solid-fill composite RLE with large layer section ({lm_len} bytes) -- treating as layers-only",
        width,
        height
    );

    Ok(LayersOnlyCompositeProbe::NeedsLayerComposite)
}

/// PackBits size for a row of identical samples (max run length 128).
#[inline]
fn packbits_identical_row_bytes(width: usize) -> usize {
    width.div_ceil(128).saturating_mul(2)
}

/// True when every RLE row is no larger than a solid-color PackBits encoding (with slack).
fn rle_row_counts_look_like_solid_fill(width: usize, row_counts: &[usize]) -> bool {
    if width == 0 || row_counts.is_empty() {
        return false;
    }
    let solid = packbits_identical_row_bytes(width);
    let max_ok = solid.saturating_mul(2).max(solid.saturating_add(8));
    row_counts.iter().all(|&c| c > 0 && c <= max_ok)
}

/// Parse Photoshop Image Resource 1033/1036 JPEG thumbnail into RGBA8.
#[allow(dead_code)]
fn extract_photoshop_thumbnail_from_ir(
    bytes: &[u8],
    ir_start: u64,
    ir_end: u64,
) -> Option<PsbComposite> {
    let mut pos = ir_start as usize;
    let end = (ir_end as usize).min(bytes.len());
    while pos + 12 <= end {
        let sig = &bytes[pos..pos + 4];
        if sig != b"8BIM" && sig != b"8B64" {
            break;
        }
        pos += 4;
        if pos + 2 > end {
            break;
        }
        let rid = u16::from_be_bytes([bytes[pos], bytes[pos + 1]]);
        pos += 2;
        if pos >= end {
            break;
        }
        let name_len = bytes[pos] as usize;
        pos += 1;
        pos += name_len;
        if (name_len + 1) % 2 == 1 {
            pos += 1; // pad to even
        }
        if pos + 4 > end {
            break;
        }
        let size = u32::from_be_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
            as usize;
        pos += 4;
        let data_end = pos.saturating_add(size);
        if data_end > end {
            break;
        }
        if (rid == IR_THUMBNAIL_PS4 || rid == IR_THUMBNAIL_PS5)
            && size >= 28
            && let Some(thumb) = decode_photoshop_thumbnail_resource(&bytes[pos..data_end])
        {
            return Some(thumb);
        }
        pos = data_end;
        if size % 2 == 1 {
            pos += 1;
        }
    }
    None
}

fn decode_photoshop_thumbnail_resource(data: &[u8]) -> Option<PsbComposite> {
    if data.len() < 28 {
        return None;
    }
    let format = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
    // 1 = JPEG RGB in current Photoshop thumbnail resources.
    if format != 1 {
        return None;
    }
    let width = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
    let height = u32::from_be_bytes([data[8], data[9], data[10], data[11]]);
    let compressed = u32::from_be_bytes([data[20], data[21], data[22], data[23]]) as usize;
    if width == 0 || height == 0 || compressed == 0 {
        return None;
    }
    let jpeg_start: usize = 28;
    let jpeg_end = jpeg_start.saturating_add(compressed).min(data.len());
    if jpeg_end <= jpeg_start {
        return None;
    }
    let jpeg = &data[jpeg_start..jpeg_end];
    let img = image::load_from_memory(jpeg).ok()?.to_rgba8();
    if img.width() == 0 || img.height() == 0 {
        return None;
    }
    Some(PsbComposite {
        width: img.width(),
        height: img.height(),
        pixels: img.into_raw(),
    })
}

/// True when sampled RGB luma is near-black (blank flattened composite).
pub fn rgba8_looks_visually_blank(pixels: &[u8]) -> bool {
    if pixels.len() < 4 {
        return true;
    }
    let pixel_count = pixels.len() / 4;
    let step_px = (pixel_count / 4096).max(1);
    let mut max_luma = 0u8;
    let mut i = 0usize;
    while i < pixel_count {
        let off = i * 4;
        let r = pixels[off] as u16;
        let g = pixels[off + 1] as u16;
        let b = pixels[off + 2] as u16;
        let luma = ((r * 30 + g * 59 + b * 11) / 100) as u8;
        max_luma = max_luma.max(luma);
        if max_luma > 8 {
            return false;
        }
        i += step_px;
    }
    true
}

/// Initialize a tiled source for a PSD/PSB file.
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

    // Skip Sections 2, 3, 4
    let cm_len = read_u32(&mut cursor)?;
    seek_forward(&mut cursor, cm_len as u64).map_err(|e| e.to_string())?;
    let ir_len = read_u32(&mut cursor)?;
    seek_forward(&mut cursor, ir_len as u64).map_err(|e| e.to_string())?;
    let lm_len = if is_psb {
        read_u64(&mut cursor)?
    } else {
        read_u32(&mut cursor)? as u64
    };
    seek_forward(&mut cursor, lm_len).map_err(|e| e.to_string())?;

    let compression = read_u16(&mut cursor)?;
    let row_counts_start = cursor.position();

    let mut row_offsets = Vec::with_capacity(channels as usize * height as usize);

    match compression {
        0 => {
            let channel_bytes = row_raw_bytes
                .checked_mul(height as u64)
                .ok_or_else(|| "PSD/PSB channel byte count overflow".to_string())?;
            for ch in 0..channels {
                for row in 0..height {
                    row_offsets.push(
                        row_counts_start
                            + (ch as u64 * channel_bytes)
                            + (row as u64 * row_raw_bytes),
                    );
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
            validate_rle_total_bytes(&row_counts_usize, remaining)?;
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

    Ok(PsbTiledSource {
        path: path.to_path_buf(),
        mmap: Arc::new(mmap),
        width,
        height,
        channels,
        color_mode,
        depth,
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

    fn extract_tile(&self, x: u32, y: u32, w: u32, h: u32) -> std::sync::Arc<Vec<u8>> {
        let mut rgba = vec![255u8; (w * h * 4) as usize];

        let mut row_grid = vec![vec![None; self.channels as usize]; h as usize];
        for ch in 0..self.channels {
            if !channel_is_used(self.color_mode, ch, self.channels) {
                continue;
            }
            let rows = self.get_rows_batch(ch, y, h);
            for (rel_y, data) in rows {
                if (rel_y as usize) < h as usize {
                    row_grid[rel_y as usize][ch as usize] = Some(data);
                }
            }
        }

        let start = x as usize;
        let end = (x + w) as usize;

        for rel_y in 0..h as usize {
            let dst_row = &mut rgba[rel_y * w as usize * 4..(rel_y + 1) * w as usize * 4];
            let src_channels = &row_grid[rel_y];
            interleave_tile_row_rgba8(
                dst_row,
                src_channels,
                self.color_mode,
                self.channels,
                start,
                end,
            );
        }
        std::sync::Arc::new(rgba)
    }

    fn generate_preview(&self, max_w: u32, max_h: u32) -> (u32, u32, Vec<u8>) {
        let scale = (max_w as f64 / self.width as f64)
            .min(max_h as f64 / self.height as f64)
            .min(1.0);
        let out_w = (self.width as f64 * scale).round().max(1.0) as u32;
        let out_h = (self.height as f64 * scale).round().max(1.0) as u32;

        let Some(pixel_len) = out_w
            .checked_mul(out_h)
            .and_then(|pixels| pixels.checked_mul(4))
            .map(|len| len as usize)
        else {
            return (0, 0, Vec::new());
        };
        let mut pixels = vec![255u8; pixel_len];

        let x_map: Vec<usize> = (0..out_w)
            .map(|out_x| ((out_x as f64 / scale) as usize).min(self.width as usize - 1))
            .collect();

        for out_y in 0..out_h {
            let src_y = ((out_y as f64 / scale) as u32).min(self.height - 1);
            let row_start_idx = out_y as usize * out_w as usize;

            // Fetch one full-width row of each used channel, then sample columns.
            let mut channel_rows: Vec<Option<Arc<Vec<u8>>>> = vec![None; self.channels as usize];
            for ch_idx in 0..self.channels {
                if channel_is_used(self.color_mode, ch_idx, self.channels) {
                    channel_rows[ch_idx as usize] = Some(self.get_row(ch_idx, src_y));
                }
            }

            for (out_x, &src_x) in x_map.iter().enumerate().take(out_w as usize) {
                let dst_off = (row_start_idx + out_x) * 4;
                if dst_off + 3 >= pixels.len() {
                    continue;
                }
                let rgba = sample_pixel_rgba8(&channel_rows, self.color_mode, self.channels, src_x);
                pixels[dst_off..dst_off + 4].copy_from_slice(&rgba);
            }
        }

        (out_w, out_h, pixels)
    }

    fn full_pixels(&self) -> Option<Arc<Vec<u8>>> {
        None
    }
}

// Tile workers decode rows in parallel; per-thread scratch avoids per-row allocations.
thread_local! {
    static PSB_ROW_SCRATCH: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

#[inline]
fn prepare_psb_row_buf(buf: &mut Vec<u8>, row_len: usize) {
    buf.clear();
    buf.resize(row_len, 0);
}

/// Decode one row into reusable thread-local storage, then move the bytes out for caching.
fn with_psb_row_scratch(row_len: usize, f: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    PSB_ROW_SCRATCH.with(|scratch| {
        let mut scratch = scratch.borrow_mut();
        prepare_psb_row_buf(&mut scratch, row_len);
        f(&mut scratch);
        std::mem::replace(&mut *scratch, Vec::with_capacity(row_len))
    })
}

/// PackBits RLE decompression (Macintosh PackBits variant) into an existing buffer.
fn unpack_bits_into(result: &mut Vec<u8>, data: &[u8], expected_len: usize) {
    result.clear();
    if result.capacity() < expected_len {
        result.reserve(expected_len - result.capacity());
    }

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
    result.resize(expected_len, 0);
}

// -- Helpers ---------------------------------------------------------

fn bytes_per_sample(depth: u16) -> Result<usize, String> {
    match depth {
        8 => Ok(1),
        16 => Ok(2),
        32 => Ok(4),
        _ => Err(format!(
            "Unsupported PSD/PSB bit depth {depth} (supported: 8, 16, 32)"
        )),
    }
}

#[inline]
fn channel_is_used(color_mode: u16, ch_idx: u32, channels: u32) -> bool {
    match color_mode {
        1 => ch_idx <= 1,                                  // Gray, Alpha
        3 => ch_idx <= 3,                                  // R, G, B, Alpha
        4 => ch_idx < 4 || (channels >= 5 && ch_idx == 4), // C,M,Y,K[,A]
        _ => ch_idx <= 2,
    }
}

/// Convert planar samples (8/16/32-bit BE) into 8-bit display samples.
/// `dst.len()` is the sample count; `src` must hold `dst.len() * bps` bytes (or be truncated).
fn downconvert_samples_to_u8(dst: &mut [u8], src: &[u8], bps: usize) {
    let n = dst.len();
    match bps {
        1 => {
            let copy = n.min(src.len());
            dst[..copy].copy_from_slice(&src[..copy]);
            if copy < n {
                dst[copy..].fill(0);
            }
        }
        2 => {
            for (i, out) in dst.iter_mut().enumerate() {
                let off = i * 2;
                if off + 1 < src.len() {
                    let v = u16::from_be_bytes([src[off], src[off + 1]]);
                    *out = (v >> 8) as u8;
                } else {
                    *out = 0;
                }
            }
        }
        4 => {
            for (i, out) in dst.iter_mut().enumerate() {
                let off = i * 4;
                if off + 3 < src.len() {
                    let bits =
                        u32::from_be_bytes([src[off], src[off + 1], src[off + 2], src[off + 3]]);
                    let f = f32::from_bits(bits);
                    *out = (f.clamp(0.0, 1.0) * 255.0).round() as u8;
                } else {
                    *out = 0;
                }
            }
        }
        _ => dst.fill(0),
    }
}

#[inline]
fn cmyk_to_rgb(c: u8, m: u8, y: u8, k: u8) -> (u8, u8, u8) {
    let c = c as u32;
    let m = m as u32;
    let y = y as u32;
    let k = k as u32;
    let r = ((255 - c) * (255 - k) / 255) as u8;
    let g = ((255 - m) * (255 - k) / 255) as u8;
    let b = ((255 - y) * (255 - k) / 255) as u8;
    (r, g, b)
}

fn interleave_row_rgba8(
    dst_row: &mut [u8],
    planar: &[Option<Vec<u8>>],
    color_mode: u16,
    channels: u32,
    start: usize,
    end: usize,
) {
    let width = end.saturating_sub(start);
    match color_mode {
        4 if channels >= 4 => {
            let c = planar[0].as_ref().map(|d| &d[start..end]);
            let m = planar[1].as_ref().map(|d| &d[start..end]);
            let y = planar[2].as_ref().map(|d| &d[start..end]);
            let k = planar[3].as_ref().map(|d| &d[start..end]);
            let a = if channels >= 5 {
                planar
                    .get(4)
                    .and_then(|c| c.as_ref())
                    .map(|d| &d[start..end])
            } else {
                None
            };
            if let (Some(c), Some(m), Some(y), Some(k)) = (c, m, y, k) {
                for col in 0..width {
                    let (r, g, b) = cmyk_to_rgb(c[col], m[col], y[col], k[col]);
                    let base = col * 4;
                    dst_row[base] = r;
                    dst_row[base + 1] = g;
                    dst_row[base + 2] = b;
                    dst_row[base + 3] = a.map(|buf| buf[col]).unwrap_or(255);
                }
            }
        }
        1 => {
            if let Some(gray) = planar.first().and_then(|c| c.as_ref()) {
                let g_row = &gray[start..end];
                let a_row = if channels >= 2 {
                    planar
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
                    if let Some(a) = a_row
                        && col < a.len()
                    {
                        dst_row[base + 3] = a[col];
                    }
                }
            }
        }
        _ => {
            // RGB (mode 3) and generic 3/4-channel fallback
            let r = planar
                .first()
                .and_then(|c| c.as_ref())
                .map(|d| &d[start..end]);
            let g = planar
                .get(1)
                .and_then(|c| c.as_ref())
                .map(|d| &d[start..end]);
            let b = planar
                .get(2)
                .and_then(|c| c.as_ref())
                .map(|d| &d[start..end]);
            if let (Some(r), Some(g), Some(b)) = (r, g, b) {
                if channels >= 4
                    && let Some(a) = planar.get(3).and_then(|c| c.as_ref())
                {
                    simd_swizzle::interleave_rgba(r, g, b, &a[start..end], dst_row);
                } else {
                    simd_swizzle::interleave_rgb_with_alpha(r, g, b, 255, dst_row);
                }
            }
        }
    }
}

fn interleave_tile_row_rgba8(
    dst_row: &mut [u8],
    src_channels: &[Option<Arc<Vec<u8>>>],
    color_mode: u16,
    channels: u32,
    start: usize,
    end: usize,
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
                for col in 0..width {
                    let (r, g, b) = cmyk_to_rgb(c[col], m[col], y[col], k[col]);
                    let base = col * 4;
                    dst_row[base] = r;
                    dst_row[base + 1] = g;
                    dst_row[base + 2] = b;
                    dst_row[base + 3] = a.map(|buf| buf[col]).unwrap_or(255);
                }
            }
        }
        1 => {
            if let Some(gray) = slice(0) {
                let alpha = if channels >= 2 { slice(1) } else { None };
                for (col, &v) in gray.iter().enumerate() {
                    let base = col * 4;
                    dst_row[base] = v;
                    dst_row[base + 1] = v;
                    dst_row[base + 2] = v;
                    if let Some(a_buf) = alpha
                        && col < a_buf.len()
                    {
                        dst_row[base + 3] = a_buf[col];
                    }
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

fn validate_psd_dimensions(width: u32, height: u32, channels: u32) -> Result<(), String> {
    if width == 0 || height == 0 {
        return Err("PSD/PSB dimensions must be non-zero".into());
    }
    if width > PSD_MAX_DIMENSION || height > PSD_MAX_DIMENSION {
        return Err(format!(
            "PSD/PSB dimensions {width}x{height} exceed maximum {PSD_MAX_DIMENSION}"
        ));
    }
    if channels == 0 || channels > PSD_MAX_CHANNELS {
        return Err(format!(
            "PSD/PSB channel count {channels} is out of range (1..={PSD_MAX_CHANNELS})"
        ));
    }
    Ok(())
}

fn checked_pixel_count(width: u32, height: u32) -> Result<usize, String> {
    (width as u64)
        .checked_mul(height as u64)
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| "PSD/PSB pixel count overflow".into())
}

fn checked_rgba_len(pixel_count: usize) -> Result<usize, String> {
    pixel_count
        .checked_mul(RGBA_BYTES_PER_PIXEL)
        .ok_or_else(|| "PSD/PSB RGBA buffer size overflow".into())
}

pub(crate) fn seek_forward(r: &mut impl Seek, len: u64) -> Result<(), String> {
    if len > i64::MAX as u64 {
        return Err(format!(
            "PSD/PSB section length {len} exceeds seekable range"
        ));
    }
    r.seek(SeekFrom::Current(len as i64))
        .map_err(|e| format!("Seek error: {e}"))?;
    Ok(())
}

fn validate_rle_total_bytes(row_counts: &[usize], remaining: u64) -> Result<(), String> {
    let total = row_counts.iter().try_fold(0u64, |acc, &len| {
        acc.checked_add(len as u64)
            .ok_or_else(|| "PSD/PSB RLE total length overflow".to_string())
    })?;
    if total > remaining {
        return Err(format!(
            "PSD/PSB RLE compressed data ({total} bytes) exceeds remaining file size ({remaining} bytes)"
        ));
    }
    Ok(())
}

pub(crate) fn read_u16(r: &mut impl Read) -> Result<u16, String> {
    let mut buf = [0u8; 2];
    r.read_exact(&mut buf)
        .map_err(|e| format!("Read u16: {e}"))?;
    Ok(u16::from_be_bytes(buf))
}

pub(crate) fn read_u32(r: &mut impl Read) -> Result<u32, String> {
    let mut buf = [0u8; 4];
    r.read_exact(&mut buf)
        .map_err(|e| format!("Read u32: {e}"))?;
    Ok(u32::from_be_bytes(buf))
}

pub(crate) fn read_u64(r: &mut impl Read) -> Result<u64, String> {
    let mut buf = [0u8; 8];
    r.read_exact(&mut buf)
        .map_err(|e| format!("Read u64: {e}"))?;
    Ok(u64::from_be_bytes(buf))
}

/// Estimate the memory required to decode a PSD/PSB composite (in bytes) from header bytes.
/// Returns (width, height, channels, estimated_bytes) or an error.
///
/// Estimate covers the final RGBA8 display buffer plus temporary planar decode
/// storage scaled by bit depth (worst-case all channels kept as raw samples).
pub fn estimate_memory_from_bytes(bytes: &[u8]) -> Result<(u32, u32, u32, u64), String> {
    if bytes.len() < 26 {
        return Err("PSD/PSB header is too short".into());
    }
    if &bytes[0..4] != b"8BPS" {
        return Err("Not a PSD/PSB file (invalid signature)".into());
    }
    let version = u16::from_be_bytes([bytes[4], bytes[5]]);
    if version != 1 && version != 2 {
        return Err(format!("Unknown PSD/PSB version: {version}"));
    }
    let channels = u16::from_be_bytes([bytes[12], bytes[13]]) as u32;
    let height = u32::from_be_bytes([bytes[14], bytes[15], bytes[16], bytes[17]]);
    let width = u32::from_be_bytes([bytes[18], bytes[19], bytes[20], bytes[21]]);
    let depth = u16::from_be_bytes([bytes[22], bytes[23]]);

    validate_psd_dimensions(width, height, channels)?;
    let bps = bytes_per_sample(depth)? as u64;

    let pixels = (width as u64)
        .checked_mul(height as u64)
        .ok_or_else(|| "PSD/PSB memory estimate overflow".to_string())?;
    let rgba = pixels
        .checked_mul(RGBA_BYTES_PER_PIXEL as u64)
        .ok_or_else(|| "PSD/PSB memory estimate overflow".to_string())?;
    let planar = pixels
        .checked_mul(channels as u64)
        .and_then(|n| n.checked_mul(bps))
        .ok_or_else(|| "PSD/PSB memory estimate overflow".to_string())?;
    let estimated = rgba
        .checked_add(planar)
        .ok_or_else(|| "PSD/PSB memory estimate overflow".to_string())?;
    Ok((width, height, channels, estimated))
}

#[cfg(test)]
mod tests {
    use super::{cmyk_to_rgb, downconvert_samples_to_u8, read_composite_from_bytes_with_cancel};
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn downconvert_16bit_uses_high_byte() {
        let src = [0x12, 0x34, 0xAB, 0xCD];
        let mut dst = [0u8; 2];
        downconvert_samples_to_u8(&mut dst, &src, 2);
        assert_eq!(dst, [0x12, 0xAB]);
    }

    #[test]
    fn downconvert_32bit_float_clamps() {
        let mut src = Vec::new();
        for f in [0.0f32, 0.5, 1.0, 2.0] {
            src.extend_from_slice(&f.to_be_bytes());
        }
        let mut dst = [0u8; 4];
        downconvert_samples_to_u8(&mut dst, &src, 4);
        assert_eq!(dst[0], 0);
        assert_eq!(dst[1], 128);
        assert_eq!(dst[2], 255);
        assert_eq!(dst[3], 255);
    }

    #[test]
    fn cmyk_black_and_white() {
        assert_eq!(cmyk_to_rgb(0, 0, 0, 0), (255, 255, 255));
        assert_eq!(cmyk_to_rgb(0, 0, 0, 255), (0, 0, 0));
        assert_eq!(cmyk_to_rgb(255, 0, 0, 0), (0, 255, 255));
    }

    #[test]
    fn solid_rle_row_heuristic_matches_packbits_identical_row() {
        let width = 5031usize;
        let solid = super::packbits_identical_row_bytes(width);
        assert_eq!(solid, 80);
        let counts = vec![solid; 100];
        assert!(super::rle_row_counts_look_like_solid_fill(width, &counts));
        let mut varied = counts;
        varied[50] = solid * 10;
        assert!(!super::rle_row_counts_look_like_solid_fill(width, &varied));
    }

    #[test]
    fn rgba_blank_detects_near_black() {
        let black = vec![0u8; 64];
        assert!(super::rgba8_looks_visually_blank(&black));
        let mut bright = vec![0u8; 64];
        bright[0] = 200;
        bright[1] = 200;
        bright[2] = 200;
        bright[3] = 255;
        assert!(!super::rgba8_looks_visually_blank(&bright));
    }

    #[test]
    fn layers_only_probe_on_11_psd_corpus() {
        let path =
            std::path::Path::new(r"F:\BaiduNetdiskDownload\素材库\45套 psd企业画册模板\11\11.psd");
        if !path.is_file() {
            eprintln!("skipping 11.psd layers-only probe; sample missing");
            return;
        }
        let bytes = std::fs::read(path).expect("read 11.psd");
        match super::probe_layers_only_composite(&bytes).expect("probe") {
            super::LayersOnlyCompositeProbe::NeedsLayerComposite => {}
            other => panic!("expected layer composite probe for 11.psd, got {other:?}"),
        }
    }

    #[test]
    fn composite_decode_aborts_when_cancel_already_set() {
        // Minimal valid-looking header is not required: cancel is checked before parsing.
        let cancel = AtomicBool::new(true);
        let err = match read_composite_from_bytes_with_cancel(&[], Some(&cancel)) {
            Ok(_) => panic!("expected cancel error"),
            Err(e) => e,
        };
        assert_eq!(err, crate::loader::DECODE_CANCELLED);
        assert!(cancel.load(Ordering::Acquire));
    }
}
