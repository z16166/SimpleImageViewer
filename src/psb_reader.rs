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
//! Extracts the Image Data section (merged composite) and optional IR
//! thumbnails. Layer compositing lives in `psb_layer_composite`. Supports PSD
//! (v1) and PSB (v2), channel depths 8/16/32 (down-converted to RGBA8 for
//! display), RGB / Grayscale / CMYK, and Image Data compression 0-3
//! (Raw / RLE / ZIP / ZIP+prediction).
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
pub(crate) const PSD_MAX_DIMENSION: u32 = 300_000;
/// Adobe Photoshop PSD/PSB maximum channel count.
const PSD_MAX_CHANNELS: u32 = 56;
/// Cap on fully-inflated ZIP / ZIP+prediction planar bytes retained inside a
/// disk-tiled source. zlib Image Data is one sequential stream, so tiled open
/// must inflate the whole planar buffer up front; beyond this budget we refuse
/// and let the caller degrade to P2/P3 instead of a multi-GB memory spike.
const MAX_TILED_ZIP_PLANAR_BYTES: usize = 512 * 1024 * 1024;
/// Bytes per RGBA pixel when assembling the composite image.
const RGBA_BYTES_PER_PIXEL: usize = 4;
/// Photoshop Image Resource IDs for embedded JPEG thumbnails.
const IR_THUMBNAIL_PS4: u16 = 1033;
const IR_THUMBNAIL_PS5: u16 = 1036;
/// Photoshop Image Resource: ICC Profile Settings (raw ICC bytes).
const IR_ICC_PROFILE: u16 = 1039;
/// Pixel-index mask for cancel polling in RGBA8 full-buffer scans (~every 256 KiB).
const RGBA8_CANCEL_POLL_MASK: usize = 0x3_FFFF;

// User-facing PSD empty-composite messages live in `locales/*.yaml`
// (`error.psd_all_layers_hidden`, `error.psd_no_displayable_image`).

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
    /// For ZIP modes these are offsets into [`Self::zip_planar`].
    row_offsets: Vec<u64>,
    /// Fully inflated planar bytes when Image Data uses ZIP / ZIP+prediction.
    zip_planar: Option<Arc<Vec<u8>>>,
    /// Concurrent LRU cache for decompressed 8-bit rows.
    row_cache: moka::sync::Cache<(u32, u32), Arc<Vec<u8>>>,
    /// Resolved CMYK ICC bytes (embedded IR 1039 or bundled default). Empty when not CMYK.
    cmyk_icc: Arc<[u8]>,
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
        // Release builds must keep this check: a short `buf` would OOB in downconvert.
        if buf.len() != out_len {
            return;
        }
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
                        // Separate TLS from PSB_ROW_SCRATCH (already borrowed by caller).
                        with_psb_raw_row_scratch(raw_len, |raw| {
                            unpack_bits_into(raw, compressed, raw_len);
                            downconvert_samples_to_u8(buf, raw, bps);
                        });
                    }
                }
            }
            2 | 3 => {
                let Some(planar) = self.zip_planar.as_ref() else {
                    return;
                };
                let offset = match self.row_offsets.get(idx) {
                    Some(&o) => o as usize,
                    None => return,
                };
                let end = offset + raw_len;
                if end <= planar.len() {
                    downconvert_samples_to_u8(buf, &planar[offset..end], bps);
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
pub fn read_composite(path: &Path) -> Result<PsbComposite, crate::loader::DecodeError> {
    let file = std::fs::File::open(path).map_err(|e| format!("Cannot open file: {e}"))?;
    let mmap = unsafe { Mmap::map(&file).map_err(|e| format!("Mmap failed: {e}"))? };
    read_composite_from_bytes(&mmap[..])
}

/// Read the flattened composite from an in-memory PSD/PSB byte slice (e.g. mmap).
pub fn read_composite_from_bytes(bytes: &[u8]) -> Result<PsbComposite, crate::loader::DecodeError> {
    read_composite_from_bytes_with_cancel(bytes, None)
}

/// Like [`read_composite_from_bytes`], but polls `cancel` in hot loops so long decodes
/// can abort when the loader drops the request (directory change, navigate away, etc.).
pub fn read_composite_from_bytes_with_cancel(
    bytes: &[u8],
    cancel: Option<&AtomicBool>,
) -> Result<PsbComposite, crate::loader::DecodeError> {
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
        return Err(format!("Unknown PSD/PSB version: {version}").into());
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

    log::debug!(
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
    seek_forward_within(&mut r, cm_len as u64, file_size, "color mode data")?;

    // -- Section 3: Image Resources --
    let ir_len = read_u32(&mut r)? as u64;
    let ir_start = r
        .stream_position()
        .map_err(|e| format!("Stream position error: {e}"))?;
    let ir_end = checked_section_end(ir_start, ir_len, file_size, "image resources")?;
    let embedded_icc = extract_icc_profile_from_ir(bytes, ir_start, ir_end);
    seek_forward_within(&mut r, ir_len, file_size, "image resources")?;

    // -- Section 4: Layer and Mask Information --
    let lm_len = if is_psb {
        read_u64(&mut r)?
    } else {
        read_u32(&mut r)? as u64
    };
    seek_forward_within(&mut r, lm_len, file_size, "layer and mask info")?;

    check_decode_cancel(cancel)?;

    // -- Section 5: Image Data (flattened composite) --
    let compression = read_u16(&mut r)?;
    // Spec: 0=Raw, 1=RLE, 2=ZIP, 3=ZIP+prediction. Anything else is invalid.
    if compression > 3 {
        return Err(format!("Invalid PSD/PSB Image Data compression: {compression}").into());
    }

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

    if compression == 2 || compression == 3 {
        // ZIP / ZIP+prediction: one zlib stream for all planar channels.
        let data_start = r
            .stream_position()
            .map_err(|e| format!("Stream position error: {e}"))? as usize;
        let compressed = bytes
            .get(data_start..)
            .ok_or_else(|| "PSD/PSB ZIP image data out of bounds".to_string())?;
        let expected = (channels as usize)
            .checked_mul(raw_channel_bytes)
            .ok_or_else(|| "PSD/PSB ZIP planar size overflow".to_string())?;
        check_decode_cancel(cancel)?;
        let mut planar = crate::psb_zip::inflate_zlib_exact(compressed, expected)?;
        if compression == 3 {
            crate::psb_zip::undo_zip_prediction(&mut planar, width as usize, depth)?;
        }
        check_decode_cancel(cancel)?;
        for ch_idx in 0..channels {
            if !channel_is_used(color_mode, ch_idx, channels) {
                continue;
            }
            let start = ch_idx as usize * raw_channel_bytes;
            let end = start + raw_channel_bytes;
            let raw = planar
                .get(start..end)
                .ok_or_else(|| "PSD/PSB ZIP channel slice out of bounds".to_string())?;
            let mut ch_u8 = vec![0u8; samples_per_channel];
            downconvert_samples_to_u8(&mut ch_u8, raw, bps);
            planar_channels[ch_idx as usize] = Some(ch_u8);
        }
    } else {
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
                        let mut compressed = Vec::new();
                        for row in 0..height as usize {
                            if row & 0x3F == 0 {
                                check_decode_cancel(cancel)?;
                            }
                            let idx = ch_idx as usize * height as usize + row;
                            let compressed_len = *row_counts
                                .get(idx)
                                .ok_or_else(|| format!("Row count index {idx} out of range"))?;
                            compressed.resize(compressed_len, 0);
                            r.read_exact(&mut compressed)
                                .map_err(|e| format!("Read RLE: {e}"))?;
                            unpack_bits_into(&mut row_raw, &compressed, row_raw_bytes);
                            let dst_start = row * width as usize;
                            let dst_end = dst_start + width as usize;
                            downconvert_samples_to_u8(
                                &mut ch_u8[dst_start..dst_end],
                                &row_raw,
                                bps,
                            );
                        }
                    }
                    _ => return Err(format!("Unsupported compression: {compression}").into()),
                }
                planar_channels[ch_idx as usize] = Some(ch_u8);
            } else {
                match compression {
                    0 => {
                        seek_forward_within(
                            &mut r,
                            raw_channel_bytes as u64,
                            file_size,
                            "raw channel data",
                        )?;
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
                            seek_forward_within(&mut r, len as u64, file_size, "RLE row data")?;
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    // Step 2: Interleave into RGBA8 (CMYK goes through lcms2 when possible).
    let mut rgba = vec![255u8; checked_rgba_len(pixel_count)?];
    let cmyk_cms_ok = color_mode == 4
        && channels >= 4
        && planar_channels[0].is_some()
        && planar_channels[1].is_some()
        && planar_channels[2].is_some()
        && planar_channels[3].is_some()
        && {
            let icc = crate::psb_cmyk_cms::resolve_cmyk_icc(embedded_icc.as_deref());
            let c = planar_channels[0].as_deref().unwrap();
            let m = planar_channels[1].as_deref().unwrap();
            let y = planar_channels[2].as_deref().unwrap();
            let k = planar_channels[3].as_deref().unwrap();
            let a = if channels >= 5 {
                planar_channels.get(4).and_then(|c| c.as_deref())
            } else {
                None
            };
            let span = crate::psb_cmyk_cms::AdobeCmykSpan {
                c,
                m,
                y,
                k,
                alpha: a,
            };
            crate::psb_cmyk_cms::cmyk_span_adobe_to_rgba8(&span, icc, &mut rgba)
        };
    if !cmyk_cms_ok {
        for row in 0..height as usize {
            if row & 0x3F == 0 {
                check_decode_cancel(cancel)?;
            }
            let start = row * width as usize;
            let end = start + width as usize;
            let dst_row = &mut rgba[row * width as usize * 4..(row + 1) * width as usize * 4];
            interleave_row_rgba8(dst_row, &planar_channels, color_mode, channels, start, end);
        }
    }

    Ok(PsbComposite {
        width,
        height,
        pixels: rgba,
    })
}

#[inline]
pub(crate) fn check_decode_cancel(
    cancel: Option<&AtomicBool>,
) -> Result<(), crate::loader::DecodeError> {
    if cancel.is_some_and(|c| c.load(Ordering::Acquire)) {
        Err(crate::loader::DecodeError::Cancelled)
    } else {
        Ok(())
    }
}

/// Absolute blank barrier for P1 flattened composites (RGBA8).
///
/// Returns true when the buffer is semantically empty:
/// - every alpha byte is 0 (fully transparent), or
/// - every RGB triple is (0,0,0) (absolute pure black).
///
/// Structural decode success alone is not enough; this is an O(N) SIMD scan
/// with early exit once both a nonzero alpha and a nonzero RGB sample exist.
/// Polls `cancel` on large buffers when provided.
pub fn rgba8_is_absolutely_blank_with_cancel(
    pixels: &[u8],
    cancel: Option<&AtomicBool>,
) -> Result<bool, crate::loader::DecodeError> {
    if pixels.len() < 4 {
        return Ok(true);
    }
    let n = pixels.len() - (pixels.len() % 4);
    if n == 0 {
        return Ok(true);
    }
    let pixels = &pixels[..n];

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            return unsafe { rgba8_absolutely_blank_avx2(pixels, cancel) };
        }
        if is_x86_feature_detected!("sse2") {
            return unsafe { rgba8_absolutely_blank_sse2(pixels, cancel) };
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        return unsafe { rgba8_absolutely_blank_neon(pixels, cancel) };
    }

    #[cfg(not(target_arch = "aarch64"))]
    {
        rgba8_absolutely_blank_scalar(pixels, cancel)
    }
}

/// Scan RGBA8; returns `(any_nonzero_rgb, any_nonzero_alpha)`.
fn rgba8_any_rgb_alpha_scalar(
    pixels: &[u8],
    cancel: Option<&AtomicBool>,
    mut any_rgb: bool,
    mut any_a: bool,
) -> Result<(bool, bool), crate::loader::DecodeError> {
    let mut i = 0usize;
    while i + 4 <= pixels.len() {
        if i & RGBA8_CANCEL_POLL_MASK == 0 {
            check_decode_cancel(cancel)?;
        }
        if (pixels[i] | pixels[i + 1] | pixels[i + 2]) != 0 {
            any_rgb = true;
        }
        if pixels[i + 3] != 0 {
            any_a = true;
        }
        if any_rgb && any_a {
            return Ok((true, true));
        }
        i += 4;
    }
    Ok((any_rgb, any_a))
}

fn rgba8_absolutely_blank_scalar(
    pixels: &[u8],
    cancel: Option<&AtomicBool>,
) -> Result<bool, crate::loader::DecodeError> {
    let (any_rgb, any_a) = rgba8_any_rgb_alpha_scalar(pixels, cancel, false, false)?;
    Ok(!any_rgb || !any_a)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn rgba8_absolutely_blank_sse2(
    pixels: &[u8],
    cancel: Option<&AtomicBool>,
) -> Result<bool, crate::loader::DecodeError> {
    use core::arch::x86_64::*;
    let mut any_rgb = false;
    let mut any_a = false;
    let n = pixels.len();
    let mut i = 0usize;
    let rgb_mask = _mm_set1_epi32(0x00FF_FFFF_u32 as i32);
    let a_mask = _mm_set1_epi32(0xFF00_0000_u32 as i32);
    let zero = _mm_setzero_si128();
    while i + 16 <= n {
        if i & RGBA8_CANCEL_POLL_MASK == 0 {
            check_decode_cancel(cancel)?;
        }
        let v = unsafe { _mm_loadu_si128(pixels.as_ptr().add(i).cast()) };
        let rgb = _mm_and_si128(v, rgb_mask);
        let alpha = _mm_and_si128(v, a_mask);
        if _mm_movemask_epi8(_mm_cmpeq_epi8(rgb, zero)) != 0xFFFF {
            any_rgb = true;
        }
        if _mm_movemask_epi8(_mm_cmpeq_epi8(alpha, zero)) != 0xFFFF {
            any_a = true;
        }
        if any_rgb && any_a {
            return Ok(false);
        }
        i += 16;
    }
    let (any_rgb, any_a) = rgba8_any_rgb_alpha_scalar(&pixels[i..], cancel, any_rgb, any_a)?;
    Ok(!any_rgb || !any_a)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn rgba8_absolutely_blank_avx2(
    pixels: &[u8],
    cancel: Option<&AtomicBool>,
) -> Result<bool, crate::loader::DecodeError> {
    use core::arch::x86_64::*;
    let mut any_rgb = false;
    let mut any_a = false;
    let n = pixels.len();
    let mut i = 0usize;
    let rgb_mask = _mm256_set1_epi32(0x00FF_FFFF_u32 as i32);
    let a_mask = _mm256_set1_epi32(0xFF00_0000_u32 as i32);
    let zero = _mm256_setzero_si256();
    while i + 32 <= n {
        if i & RGBA8_CANCEL_POLL_MASK == 0 {
            check_decode_cancel(cancel)?;
        }
        let v = unsafe { _mm256_loadu_si256(pixels.as_ptr().add(i).cast()) };
        let rgb = _mm256_and_si256(v, rgb_mask);
        let alpha = _mm256_and_si256(v, a_mask);
        if _mm256_movemask_epi8(_mm256_cmpeq_epi8(rgb, zero)) != -1 {
            any_rgb = true;
        }
        if _mm256_movemask_epi8(_mm256_cmpeq_epi8(alpha, zero)) != -1 {
            any_a = true;
        }
        if any_rgb && any_a {
            return Ok(false);
        }
        i += 32;
    }
    let (any_rgb, any_a) = rgba8_any_rgb_alpha_scalar(&pixels[i..], cancel, any_rgb, any_a)?;
    Ok(!any_rgb || !any_a)
}

#[cfg(target_arch = "aarch64")]
unsafe fn rgba8_absolutely_blank_neon(
    pixels: &[u8],
    cancel: Option<&AtomicBool>,
) -> Result<bool, crate::loader::DecodeError> {
    use core::arch::aarch64::*;
    let mut any_rgb = false;
    let mut any_a = false;
    let n = pixels.len();
    let mut i = 0usize;
    let rgb_mask = vdupq_n_u32(0x00FF_FFFF);
    let a_mask = vdupq_n_u32(0xFF00_0000);
    while i + 16 <= n {
        if i & RGBA8_CANCEL_POLL_MASK == 0 {
            check_decode_cancel(cancel)?;
        }
        let v = unsafe { vld1q_u8(pixels.as_ptr().add(i)) };
        let vu = vreinterpretq_u32_u8(v);
        let rgb = vandq_u32(vu, rgb_mask);
        let alpha = vandq_u32(vu, a_mask);
        if vmaxvq_u32(rgb) != 0 {
            any_rgb = true;
        }
        if vmaxvq_u32(alpha) != 0 {
            any_a = true;
        }
        if any_rgb && any_a {
            return Ok(false);
        }
        i += 16;
    }
    let (any_rgb, any_a) = rgba8_any_rgb_alpha_scalar(&pixels[i..], cancel, any_rgb, any_a)?;
    Ok(!any_rgb || !any_a)
}

/// Zero-information barrier for P2 strict layer composites (RGBA8).
///
/// Returns true when the buffer has no visual information content:
/// - every alpha byte is 0 (fully transparent), or
/// - every RGB triple is identical (solid fill; variance / range is 0).
///
/// Unlike P1 absolute blank (all-RGB-0 only), any solid color fails here
/// (white, gray, etc.). Early-exits once a nonzero alpha and an RGB that
/// differs from the first pixel are both observed. Polls `cancel` on large
/// buffers when provided.
pub fn rgba8_is_zero_information_with_cancel(
    pixels: &[u8],
    cancel: Option<&AtomicBool>,
) -> Result<bool, crate::loader::DecodeError> {
    if pixels.len() < 4 {
        return Ok(true);
    }
    let n = pixels.len() - (pixels.len() % 4);
    if n == 0 {
        return Ok(true);
    }
    let pixels = &pixels[..n];

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            return unsafe { rgba8_zero_information_avx2(pixels, cancel) };
        }
        if is_x86_feature_detected!("sse2") {
            return unsafe { rgba8_zero_information_sse2(pixels, cancel) };
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        return unsafe { rgba8_zero_information_neon(pixels, cancel) };
    }

    #[cfg(not(target_arch = "aarch64"))]
    {
        rgba8_zero_information_scalar(pixels, cancel)
    }
}

/// Scan RGBA8 against `ref_rgb`; returns `(rgb_varies, any_nonzero_alpha)`.
fn rgba8_rgb_varies_any_alpha_scalar(
    pixels: &[u8],
    cancel: Option<&AtomicBool>,
    ref_r: u8,
    ref_g: u8,
    ref_b: u8,
    mut rgb_varies: bool,
    mut any_a: bool,
) -> Result<(bool, bool), crate::loader::DecodeError> {
    let mut i = 0usize;
    while i + 4 <= pixels.len() {
        if i & RGBA8_CANCEL_POLL_MASK == 0 {
            check_decode_cancel(cancel)?;
        }
        if pixels[i] != ref_r || pixels[i + 1] != ref_g || pixels[i + 2] != ref_b {
            rgb_varies = true;
        }
        if pixels[i + 3] != 0 {
            any_a = true;
        }
        if rgb_varies && any_a {
            return Ok((true, true));
        }
        i += 4;
    }
    Ok((rgb_varies, any_a))
}

fn rgba8_zero_information_scalar(
    pixels: &[u8],
    cancel: Option<&AtomicBool>,
) -> Result<bool, crate::loader::DecodeError> {
    let ref_r = pixels[0];
    let ref_g = pixels[1];
    let ref_b = pixels[2];
    let (rgb_varies, any_a) =
        rgba8_rgb_varies_any_alpha_scalar(pixels, cancel, ref_r, ref_g, ref_b, false, false)?;
    // Zero info when fully transparent or solid RGB (no variance).
    Ok(!any_a || !rgb_varies)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn rgba8_zero_information_sse2(
    pixels: &[u8],
    cancel: Option<&AtomicBool>,
) -> Result<bool, crate::loader::DecodeError> {
    use core::arch::x86_64::*;
    let ref_r = pixels[0];
    let ref_g = pixels[1];
    let ref_b = pixels[2];
    let ref_rgb = _mm_set1_epi32(u32::from_le_bytes([ref_r, ref_g, ref_b, 0]) as i32);
    let rgb_mask = _mm_set1_epi32(0x00FF_FFFF_u32 as i32);
    let a_mask = _mm_set1_epi32(0xFF00_0000_u32 as i32);
    let zero = _mm_setzero_si128();
    let mut rgb_varies = false;
    let mut any_a = false;
    let n = pixels.len();
    let mut i = 0usize;
    while i + 16 <= n {
        if i & RGBA8_CANCEL_POLL_MASK == 0 {
            check_decode_cancel(cancel)?;
        }
        let v = unsafe { _mm_loadu_si128(pixels.as_ptr().add(i).cast()) };
        let rgb = _mm_and_si128(v, rgb_mask);
        let alpha = _mm_and_si128(v, a_mask);
        if _mm_movemask_epi8(_mm_cmpeq_epi8(rgb, ref_rgb)) != 0xFFFF {
            rgb_varies = true;
        }
        if _mm_movemask_epi8(_mm_cmpeq_epi8(alpha, zero)) != 0xFFFF {
            any_a = true;
        }
        if rgb_varies && any_a {
            return Ok(false);
        }
        i += 16;
    }
    let (rgb_varies, any_a) = rgba8_rgb_varies_any_alpha_scalar(
        &pixels[i..],
        cancel,
        ref_r,
        ref_g,
        ref_b,
        rgb_varies,
        any_a,
    )?;
    Ok(!any_a || !rgb_varies)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn rgba8_zero_information_avx2(
    pixels: &[u8],
    cancel: Option<&AtomicBool>,
) -> Result<bool, crate::loader::DecodeError> {
    use core::arch::x86_64::*;
    let ref_r = pixels[0];
    let ref_g = pixels[1];
    let ref_b = pixels[2];
    let ref_rgb = _mm256_set1_epi32(u32::from_le_bytes([ref_r, ref_g, ref_b, 0]) as i32);
    let rgb_mask = _mm256_set1_epi32(0x00FF_FFFF_u32 as i32);
    let a_mask = _mm256_set1_epi32(0xFF00_0000_u32 as i32);
    let zero = _mm256_setzero_si256();
    let mut rgb_varies = false;
    let mut any_a = false;
    let n = pixels.len();
    let mut i = 0usize;
    while i + 32 <= n {
        if i & RGBA8_CANCEL_POLL_MASK == 0 {
            check_decode_cancel(cancel)?;
        }
        let v = unsafe { _mm256_loadu_si256(pixels.as_ptr().add(i).cast()) };
        let rgb = _mm256_and_si256(v, rgb_mask);
        let alpha = _mm256_and_si256(v, a_mask);
        if _mm256_movemask_epi8(_mm256_cmpeq_epi8(rgb, ref_rgb)) != -1 {
            rgb_varies = true;
        }
        if _mm256_movemask_epi8(_mm256_cmpeq_epi8(alpha, zero)) != -1 {
            any_a = true;
        }
        if rgb_varies && any_a {
            return Ok(false);
        }
        i += 32;
    }
    let (rgb_varies, any_a) = rgba8_rgb_varies_any_alpha_scalar(
        &pixels[i..],
        cancel,
        ref_r,
        ref_g,
        ref_b,
        rgb_varies,
        any_a,
    )?;
    Ok(!any_a || !rgb_varies)
}

#[cfg(target_arch = "aarch64")]
unsafe fn rgba8_zero_information_neon(
    pixels: &[u8],
    cancel: Option<&AtomicBool>,
) -> Result<bool, crate::loader::DecodeError> {
    use core::arch::aarch64::*;
    let ref_r = pixels[0];
    let ref_g = pixels[1];
    let ref_b = pixels[2];
    let ref_rgb = vdupq_n_u32(u32::from_le_bytes([ref_r, ref_g, ref_b, 0]));
    let rgb_mask = vdupq_n_u32(0x00FF_FFFF);
    let a_mask = vdupq_n_u32(0xFF00_0000);
    let mut rgb_varies = false;
    let mut any_a = false;
    let n = pixels.len();
    let mut i = 0usize;
    while i + 16 <= n {
        if i & RGBA8_CANCEL_POLL_MASK == 0 {
            check_decode_cancel(cancel)?;
        }
        let v = unsafe { vld1q_u8(pixels.as_ptr().add(i)) };
        let vu = vreinterpretq_u32_u8(v);
        let rgb = vandq_u32(vu, rgb_mask);
        let alpha = vandq_u32(vu, a_mask);
        // Any lane differing from ref => RGB variance.
        let eq = vceqq_u32(rgb, ref_rgb);
        if vminvq_u32(eq) == 0 {
            rgb_varies = true;
        }
        if vmaxvq_u32(alpha) != 0 {
            any_a = true;
        }
        if rgb_varies && any_a {
            return Ok(false);
        }
        i += 16;
    }
    let (rgb_varies, any_a) = rgba8_rgb_varies_any_alpha_scalar(
        &pixels[i..],
        cancel,
        ref_r,
        ref_g,
        ref_b,
        rgb_varies,
        any_a,
    )?;
    Ok(!any_a || !rgb_varies)
}

pub fn extract_icc_profile_from_ir(bytes: &[u8], ir_start: u64, ir_end: u64) -> Option<Vec<u8>> {
    for_each_image_resource(bytes, ir_start, ir_end, |rid, data| {
        if rid == IR_ICC_PROFILE && !data.is_empty() {
            Some(data.to_vec())
        } else {
            None
        }
    })
}

/// Extract embedded ICC (IR 1039) from a full PSD/PSB byte buffer.
#[cfg(test)]
pub fn extract_embedded_icc_from_psd(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.len() < 30 {
        return None;
    }
    let mut r = std::io::Cursor::new(bytes);
    let mut sig = [0u8; 4];
    r.read_exact(&mut sig).ok()?;
    if &sig != b"8BPS" {
        return None;
    }
    let version = read_u16(&mut r).ok()?;
    if version != 1 && version != 2 {
        return None;
    }
    r.seek(SeekFrom::Current(6)).ok()?;
    let _channels = read_u16(&mut r).ok()?;
    let _height = read_u32(&mut r).ok()?;
    let _width = read_u32(&mut r).ok()?;
    let _depth = read_u16(&mut r).ok()?;
    let _color_mode = read_u16(&mut r).ok()?;
    let cm_len = read_u32(&mut r).ok()? as u64;
    seek_forward(&mut r, cm_len).ok()?;
    let ir_len = read_u32(&mut r).ok()? as u64;
    let ir_start = r.stream_position().ok()?;
    let ir_end = ir_start.saturating_add(ir_len).min(bytes.len() as u64);
    extract_icc_profile_from_ir(bytes, ir_start, ir_end)
}

/// Try to extract Photoshop Image Resource 1033/1036 JPEG thumbnail as RGBA8.
pub fn try_extract_photoshop_thumbnail(bytes: &[u8]) -> Option<PsbComposite> {
    let file_size = bytes.len() as u64;
    let mut r = std::io::Cursor::new(bytes);
    let mut sig = [0u8; 4];
    r.read_exact(&mut sig).ok()?;
    if &sig != b"8BPS" {
        return None;
    }
    let version = read_u16(&mut r).ok()?;
    if version != 1 && version != 2 {
        return None;
    }
    r.seek(SeekFrom::Current(6)).ok()?;
    let _channels = read_u16(&mut r).ok()?;
    let _height = read_u32(&mut r).ok()?;
    let _width = read_u32(&mut r).ok()?;
    let _depth = read_u16(&mut r).ok()?;
    let _color_mode = read_u16(&mut r).ok()?;
    let cm_len = read_u32(&mut r).ok()? as u64;
    seek_forward(&mut r, cm_len).ok()?;
    let ir_len = read_u32(&mut r).ok()? as u64;
    let ir_start = r.stream_position().ok()?;
    let ir_end = ir_start.saturating_add(ir_len).min(file_size);
    extract_photoshop_thumbnail_from_ir(bytes, ir_start, ir_end)
}

/// Parse Photoshop Image Resource 1033/1036 JPEG thumbnail into RGBA8.
fn extract_photoshop_thumbnail_from_ir(
    bytes: &[u8],
    ir_start: u64,
    ir_end: u64,
) -> Option<PsbComposite> {
    for_each_image_resource(bytes, ir_start, ir_end, |rid, data| {
        if (rid == IR_THUMBNAIL_PS4 || rid == IR_THUMBNAIL_PS5) && data.len() >= 28 {
            decode_photoshop_thumbnail_resource(data)
        } else {
            None
        }
    })
}

/// Walk Photoshop Image Resources (8BIM/8B64), invoking `on_resource` for each.
/// Returns the first `Some` from the callback.
fn for_each_image_resource<T>(
    bytes: &[u8],
    ir_start: u64,
    ir_end: u64,
    mut on_resource: impl FnMut(u16, &[u8]) -> Option<T>,
) -> Option<T> {
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
            pos += 1;
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
        if let Some(found) = on_resource(rid, &bytes[pos..data_end]) {
            return Some(found);
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
    let row_counts_start = cursor.position();

    let mut row_offsets = Vec::with_capacity(channels as usize * height as usize);
    let mut zip_planar: Option<Arc<Vec<u8>>> = None;

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
        2 | 3 => {
            let compressed = &mmap[cursor.position() as usize..];
            let channel_bytes = (height as usize)
                .checked_mul(width as usize)
                .and_then(|n| n.checked_mul(bps))
                .ok_or_else(|| "PSD/PSB ZIP channel byte count overflow".to_string())?;
            let expected = (channels as usize)
                .checked_mul(channel_bytes)
                .ok_or_else(|| "PSD/PSB ZIP planar size overflow".to_string())?;
            if expected > MAX_TILED_ZIP_PLANAR_BYTES {
                return Err(format!(
                    "PSD/PSB ZIP planar size {expected} exceeds tiled inflate budget \
                     {MAX_TILED_ZIP_PLANAR_BYTES}"
                ));
            }
            let mut inflated = crate::psb_zip::inflate_zlib_exact(compressed, expected)?;
            if compression == 3 {
                crate::psb_zip::undo_zip_prediction(&mut inflated, width as usize, depth)?;
            }
            for ch in 0..channels as usize {
                for row in 0..height as usize {
                    let off = ch * channel_bytes + row * width as usize * bps;
                    row_offsets.push(off as u64);
                }
            }
            zip_planar = Some(Arc::new(inflated));
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
        mmap: Arc::new(mmap),
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
                self.cmyk_icc.as_ref(),
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
    // Intermediate BE samples for RLE+16/32-bit rows (must not share PSB_ROW_SCRATCH).
    static PSB_RAW_ROW_SCRATCH: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
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

/// Scratch for PackBits expand before 16/32-bit down-convert (separate TLS key).
fn with_psb_raw_row_scratch(raw_len: usize, f: impl FnOnce(&mut Vec<u8>)) {
    PSB_RAW_ROW_SCRATCH.with(|scratch| {
        let mut scratch = scratch.borrow_mut();
        let cap = scratch.capacity();
        if cap < raw_len {
            scratch.reserve(raw_len - cap);
        }
        f(&mut scratch);
    });
}

/// PackBits RLE decompression (Macintosh PackBits variant) into an existing buffer.
pub(crate) fn unpack_bits_into(result: &mut Vec<u8>, data: &[u8], expected_len: usize) {
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
                    let start = result.len();
                    result.resize(start + actual_count, 0);
                    crate::psb_packbits_simd::fill_bytes(&mut result[start..], val);
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
pub(crate) fn downconvert_samples_to_u8(dst: &mut [u8], src: &[u8], bps: usize) {
    let n = dst.len();
    match bps {
        1 => {
            let copy = n.min(src.len());
            dst[..copy].copy_from_slice(&src[..copy]);
            if copy < n {
                dst[copy..].fill(0);
            }
        }
        2 => crate::psb_downconvert_simd::u16be_to_u8(dst, src),
        4 => crate::psb_downconvert_simd::f32be_to_u8(dst, src),
        _ => dst.fill(0),
    }
}

/// Convert one PSD/PSB CMYK sample to approximate display RGB.
///
/// Photoshop stores CMYK channel bytes with **0 = 100% ink** and **255 = 0% ink**
/// (Adobe Photoshop File Formats Specification). The naive "0 = no ink" formula
/// inverts whites to black and turns solid 0xFF M/Y into a red cast.
#[inline]
pub(crate) fn cmyk_to_rgb(c: u8, m: u8, y: u8, k: u8) -> (u8, u8, u8) {
    let c = c as u32;
    let m = m as u32;
    let y = y as u32;
    let k = k as u32;
    let r = (c * k / 255) as u8;
    let g = (m * k / 255) as u8;
    let b = (y * k / 255) as u8;
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
                crate::psb_cmyk_simd::cmyk_planes_to_rgba8(c, m, y, k, a, dst_row);
            }
        }
        1 => {
            if let Some(gray) = planar.first().and_then(|c| c.as_ref()) {
                let g_row = &gray[start..end];
                if channels >= 2
                    && let Some(a) = planar.get(1).and_then(|c| c.as_ref())
                {
                    simd_swizzle::interleave_rgba(g_row, g_row, g_row, &a[start..end], dst_row);
                } else {
                    simd_swizzle::interleave_rgb_with_alpha(g_row, g_row, g_row, 255, dst_row);
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

/// Seek forward `len` bytes only when the resulting position stays within
/// `file_size`. `Cursor` allows seeking past EOF, which would otherwise defer
/// failure to a later `read_exact`.
pub(crate) fn seek_forward_within(
    r: &mut impl Seek,
    len: u64,
    file_size: u64,
    label: &str,
) -> Result<(), String> {
    let pos = r
        .stream_position()
        .map_err(|e| format!("Stream position error: {e}"))?;
    let end = checked_section_end(pos, len, file_size, label)?;
    debug_assert!(end <= file_size);
    seek_forward(r, len)
}

fn checked_section_end(start: u64, len: u64, file_size: u64, label: &str) -> Result<u64, String> {
    let end = start
        .checked_add(len)
        .ok_or_else(|| format!("PSD/PSB {label} length overflow"))?;
    if end > file_size {
        return Err(format!(
            "PSD/PSB {label} exceeds file size ({end} > {file_size})"
        ));
    }
    Ok(end)
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
    fn extract_embedded_icc_absent_on_brochure_without_1039() {
        let path = std::path::Path::new(
            r"F:\BaiduNetdiskDownload\素材库\45套 psd企业画册模板\12\01-02.psd",
        );
        if !path.is_file() {
            return;
        }
        let bytes = std::fs::read(path).expect("read");
        assert!(super::extract_embedded_icc_from_psd(&bytes).is_none());
    }

    #[test]
    fn cmyk_black_and_white() {
        // Adobe polarity: 0 = 100% ink, 255 = 0% ink.
        assert_eq!(cmyk_to_rgb(255, 255, 255, 255), (255, 255, 255));
        assert_eq!(cmyk_to_rgb(255, 255, 255, 0), (0, 0, 0));
        assert_eq!(cmyk_to_rgb(0, 255, 255, 255), (0, 255, 255));
    }

    #[test]
    fn invalid_image_data_compression_is_rejected() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"8BPS");
        bytes.extend_from_slice(&1u16.to_be_bytes());
        bytes.extend_from_slice(&[0u8; 6]);
        bytes.extend_from_slice(&3u16.to_be_bytes());
        bytes.extend_from_slice(&2u32.to_be_bytes());
        bytes.extend_from_slice(&2u32.to_be_bytes());
        bytes.extend_from_slice(&8u16.to_be_bytes());
        bytes.extend_from_slice(&3u16.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&99u16.to_be_bytes());
        let err = super::read_composite_from_bytes(&bytes).expect_err("bad compression");
        assert!(
            err.as_str()
                .contains("Invalid PSD/PSB Image Data compression"),
            "unexpected err: {err}"
        );
    }

    #[test]
    fn read_composite_rejects_color_mode_section_past_eof() {
        // Cursor::seek past EOF succeeds; without a file_size check the failure
        // is deferred to a later read_exact with a less specific error.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"8BPS");
        bytes.extend_from_slice(&1u16.to_be_bytes());
        bytes.extend_from_slice(&[0u8; 6]);
        bytes.extend_from_slice(&3u16.to_be_bytes());
        bytes.extend_from_slice(&2u32.to_be_bytes());
        bytes.extend_from_slice(&2u32.to_be_bytes());
        bytes.extend_from_slice(&8u16.to_be_bytes());
        bytes.extend_from_slice(&3u16.to_be_bytes());
        // Color Mode Data length claims far more bytes than remain.
        bytes.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        let err = super::read_composite_from_bytes(&bytes).expect_err("past eof");
        let msg = err.as_str();
        assert!(
            msg.contains("color mode") && msg.contains("exceeds"),
            "expected early section-bound error, got: {msg}"
        );
    }

    #[test]
    fn try_extract_photoshop_thumbnail_returns_none_on_empty() {
        assert!(super::try_extract_photoshop_thumbnail(&[]).is_none());
    }

    fn craft_rgb8_psd(compression: u16, planar: &[u8], width: u32, height: u32) -> Vec<u8> {
        assert_eq!(planar.len(), (width * height * 3) as usize);
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"8BPS");
        bytes.extend_from_slice(&1u16.to_be_bytes());
        bytes.extend_from_slice(&[0u8; 6]);
        bytes.extend_from_slice(&3u16.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&8u16.to_be_bytes());
        bytes.extend_from_slice(&3u16.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&compression.to_be_bytes());
        match compression {
            0 => bytes.extend_from_slice(planar),
            2 | 3 => {
                let mut payload = planar.to_vec();
                if compression == 3 {
                    let w = width as usize;
                    let h = height as usize;
                    for ch in 0..3 {
                        let base = ch * w * h;
                        for y in 0..h {
                            let row = base + y * w;
                            for x in (1..w).rev() {
                                payload[row + x] =
                                    payload[row + x].wrapping_sub(payload[row + x - 1]);
                            }
                        }
                    }
                }
                bytes.extend_from_slice(&miniz_oxide::deflate::compress_to_vec_zlib(&payload, 6));
            }
            _ => panic!("craft helper only supports 0/2/3"),
        }
        bytes
    }

    #[test]
    fn read_composite_zip_and_zip_prediction_rgb8() {
        let width = 4u32;
        let height = 2u32;
        let mut planar = vec![0u8; (width * height * 3) as usize];
        for (i, b) in planar.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(3).wrapping_add(7);
        }
        for compression in [2u16, 3u16] {
            let bytes = craft_rgb8_psd(compression, &planar, width, height);
            let composite = super::read_composite_from_bytes(&bytes).expect("decode");
            assert_eq!((composite.width, composite.height), (width, height));
            assert_eq!(composite.pixels[0], planar[0]);
            assert_eq!(composite.pixels[1], planar[(width * height) as usize]);
            assert_eq!(composite.pixels[2], planar[(width * height * 2) as usize]);
            assert_eq!(composite.pixels[3], 255);
        }
    }

    #[test]
    fn read_composite_zip_fixtures_if_present() {
        let dir = std::path::Path::new("tests/data/psd_compress");
        for name in [
            "rgb8_raw.psd",
            "rgb8_rle.psd",
            "rgb8_zip.psd",
            "rgb8_zip_prediction.psd",
        ] {
            let path = dir.join(name);
            if !path.is_file() {
                eprintln!("skip missing fixture {}", path.display());
                continue;
            }
            let bytes = std::fs::read(&path).expect("read fixture");
            let composite = super::read_composite_from_bytes(&bytes).expect(name);
            assert_eq!((composite.width, composite.height), (8, 4));
            assert_eq!(composite.pixels.len(), 8 * 4 * 4);
            assert!(
                !super::rgba8_is_zero_information_with_cancel(&composite.pixels, None).unwrap()
            );
        }
    }

    #[test]
    fn rgba8_absolute_blank_detects_all_transparent_and_all_black() {
        assert!(super::rgba8_is_absolutely_blank_with_cancel(&[], None).unwrap());
        assert!(
            super::rgba8_is_absolutely_blank_with_cancel(&[0, 0, 0, 0, 0, 0, 0, 0], None).unwrap()
        );
        assert!(
            super::rgba8_is_absolutely_blank_with_cancel(&[0, 0, 0, 255, 0, 0, 0, 255], None)
                .unwrap()
        );
        assert!(
            super::rgba8_is_absolutely_blank_with_cancel(&[10, 20, 30, 0, 40, 50, 60, 0], None)
                .unwrap()
        );
        assert!(
            !super::rgba8_is_absolutely_blank_with_cancel(&[0, 0, 0, 255, 1, 0, 0, 255], None)
                .unwrap()
        );
        assert!(
            !super::rgba8_is_absolutely_blank_with_cancel(&[255, 255, 255, 255], None).unwrap()
        );
    }

    #[test]
    fn rgba8_absolute_blank_large_buffer_with_single_lit_pixel() {
        let mut pixels = vec![0u8; 4096 * 4];
        for px in pixels.chunks_exact_mut(4) {
            px[3] = 255;
        }
        assert!(super::rgba8_is_absolutely_blank_with_cancel(&pixels, None).unwrap());
        let off = 1234 * 4;
        pixels[off] = 7;
        assert!(!super::rgba8_is_absolutely_blank_with_cancel(&pixels, None).unwrap());
    }

    #[test]
    fn rgba8_zero_information_detects_transparent_and_solid_fills() {
        assert!(super::rgba8_is_zero_information_with_cancel(&[], None).unwrap());
        // Fully transparent (RGB may vary).
        assert!(
            super::rgba8_is_zero_information_with_cancel(&[10, 20, 30, 0, 40, 50, 60, 0], None)
                .unwrap()
        );
        // Solid black / white / gray.
        assert!(
            super::rgba8_is_zero_information_with_cancel(&[0, 0, 0, 255, 0, 0, 0, 128], None)
                .unwrap()
        );
        assert!(
            super::rgba8_is_zero_information_with_cancel(
                &[255, 255, 255, 255, 255, 255, 255, 200],
                None
            )
            .unwrap()
        );
        assert!(
            super::rgba8_is_zero_information_with_cancel(
                &[128, 128, 128, 255, 128, 128, 128, 255],
                None
            )
            .unwrap()
        );
        // Two distinct opaque RGB samples => has information.
        assert!(
            !super::rgba8_is_zero_information_with_cancel(&[0, 0, 0, 255, 1, 0, 0, 255], None)
                .unwrap()
        );
    }

    #[test]
    fn rgba8_zero_information_large_solid_then_one_variant() {
        let mut pixels = vec![0u8; 4096 * 4];
        for px in pixels.chunks_exact_mut(4) {
            px[0] = 128;
            px[1] = 128;
            px[2] = 128;
            px[3] = 255;
        }
        assert!(super::rgba8_is_zero_information_with_cancel(&pixels, None).unwrap());
        let off = 2000 * 4;
        pixels[off] = 129;
        assert!(!super::rgba8_is_zero_information_with_cancel(&pixels, None).unwrap());
    }

    #[test]
    fn composite_decode_aborts_when_cancel_already_set() {
        // Minimal valid-looking header is not required: cancel is checked before parsing.
        let cancel = AtomicBool::new(true);
        let err = match read_composite_from_bytes_with_cancel(&[], Some(&cancel)) {
            Ok(_) => panic!("expected cancel error"),
            Err(e) => e,
        };
        assert!(err.is_cancelled());
        assert_eq!(err.as_str(), crate::loader::DECODE_CANCELLED);
        assert!(cancel.load(Ordering::Acquire));
    }
}
