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

//! Disk-backed tiled HDR source for PSD/PSB flattened Image Data.
//!
//! Supports RGB depth 16/32 with raw, RLE, or budgeted ZIP planar inflate.
//! Gray/CMYK and over-budget ZIP fall through to other paths -- see
//! `docs/psd-psb-known-limits.md`.

use memmap2::Mmap;
use parking_lot::Mutex;
use std::cell::RefCell;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use crate::hdr::tiled::{
    HdrTileBuffer, HdrTileCache, HdrTiledSource, HdrTiledSourceKind,
    configured_hdr_tile_cache_max_bytes, hdr_preview_from_tiled_source_nearest,
    validate_tile_bounds,
};
use crate::hdr::types::{
    DEFAULT_SDR_WHITE_NITS, HdrColorProfile, HdrColorSpace, HdrImageMetadata, HdrLuminanceMetadata,
    HdrReference, HdrTransferFunction,
};
use crate::psb_icc_hdr::probe_icc_hdr;
use crate::psb_reader::{
    bytes_per_sample, checked_section_end, extract_icc_profile_from_ir,
    max_rle_compressed_row_bytes, read_u16, read_u32, read_u64, seek_forward_within,
    tiled_compression_supported, unpack_bits_into, validate_psd_dimensions,
    validate_rle_row_counts,
};

const ROW_CACHE_BUDGET: u64 = 512 * 1024 * 1024;

/// Phase 3 budget for lazily inflating a ZIP (compression 2|3) flattened Image
/// Data section into an in-memory planar buffer. Larger sections refuse flat
/// ZIP HDR tiling at open so the caller can fall through to layers / SDR.
const ZIP_PLANAR_MAX_BYTES: usize = 256 * 1024 * 1024;

/// Disk-backed HDR source for flattened PSD/PSB RGB Image Data.
#[derive(Debug)]
pub struct PsbHdrTiledFlatSource {
    path: PathBuf,
    mmap: Arc<Mmap>,
    width: u32,
    height: u32,
    channels: u32,
    depth: u16,
    compression: u16,
    row_offsets: Vec<u64>,
    /// Byte range of the zlib payload in `mmap` for compression 2|3; `None`
    /// for raw / RLE.
    zip_compressed_range: Option<(usize, usize)>,
    /// Lazily inflated ZIP planar buffer (all channels, channel-major). `None`
    /// inner value means inflate failed; the row decode then yields blanks.
    zip_planar: OnceLock<Option<Arc<Vec<u8>>>>,
    row_cache: moka::sync::Cache<(u32, u32), Arc<Vec<u8>>>,
    tile_cache: Mutex<HdrTileCache>,
    metadata: HdrImageMetadata,
    input_transfer: HdrTransferFunction,
    sdr_white_nits: f32,
}

impl PsbHdrTiledFlatSource {
    #[inline]
    fn bytes_per_sample(&self) -> usize {
        (self.depth / 8) as usize
    }

    #[inline]
    fn raw_row_bytes(&self) -> Result<usize, String> {
        (self.width as usize)
            .checked_mul(self.bytes_per_sample())
            .ok_or_else(|| "PSD/PSB HDR tiled raw row byte count overflow".to_string())
    }

    #[inline]
    fn row_file_offset(&self, idx: usize) -> Option<usize> {
        usize::try_from(*self.row_offsets.get(idx)?).ok()
    }

    fn decode_row_into(
        &self,
        buf: &mut Vec<u8>,
        ch_idx: u32,
        global_row: u32,
    ) -> Result<(), String> {
        let raw_len = self.raw_row_bytes().inspect_err(|_| buf.fill(0))?;
        if buf.len() != raw_len {
            return Err(format!(
                "PSD/PSB HDR tiled row destination length mismatch: got {}, expected {raw_len}",
                buf.len()
            ));
        }

        let idx = (ch_idx as usize)
            .checked_mul(self.height as usize)
            .and_then(|base| base.checked_add(global_row as usize))
            .ok_or_else(|| {
                buf.fill(0);
                "PSD/PSB HDR tiled row index overflow".to_string()
            })?;
        match self.compression {
            0 => {
                let Some(offset) = self.row_file_offset(idx) else {
                    buf.fill(0);
                    return Err("PSD/PSB HDR tiled raw row offset is missing".to_string());
                };
                let Some(end) = offset.checked_add(raw_len) else {
                    buf.fill(0);
                    return Err("PSD/PSB HDR tiled raw row end overflow".to_string());
                };
                if end <= self.mmap.len() {
                    buf.copy_from_slice(&self.mmap[offset..end]);
                } else {
                    buf.fill(0);
                    return Err("PSD/PSB HDR tiled raw row is out of bounds".to_string());
                }
            }
            1 => {
                let Some(offset) = self.row_file_offset(idx) else {
                    buf.fill(0);
                    return Err("PSD/PSB HDR tiled RLE row offset is missing".to_string());
                };
                let next_offset = if let Some(next_idx) = idx
                    .checked_add(1)
                    .filter(|&next| next < self.row_offsets.len())
                {
                    match usize::try_from(self.row_offsets[next_idx]) {
                        Ok(next) => next,
                        Err(_) => {
                            buf.fill(0);
                            return Err(
                                "PSD/PSB HDR tiled RLE next row offset overflow".to_string()
                            );
                        }
                    }
                } else {
                    self.mmap.len()
                };
                if offset < self.mmap.len()
                    && next_offset <= self.mmap.len()
                    && next_offset > offset
                {
                    let compressed = &self.mmap[offset..next_offset];
                    if let Err(e) = unpack_bits_into(buf, compressed, raw_len) {
                        buf.fill(0);
                        return Err(format!("PSD/PSB HDR tiled RLE row decode failed: {e}"));
                    }
                } else {
                    buf.fill(0);
                    return Err("PSD/PSB HDR tiled RLE row range is invalid".to_string());
                }
            }
            2 | 3 => {
                let Some(planar) = self.ensure_zip_planar() else {
                    buf.fill(0);
                    return Err("PSD/PSB HDR tiled ZIP planar data is unavailable".to_string());
                };
                let channel_bytes = raw_len.checked_mul(self.height as usize).ok_or_else(|| {
                    buf.fill(0);
                    "PSD/PSB HDR tiled ZIP channel byte count overflow".to_string()
                })?;
                let Some(offset) = (ch_idx as usize)
                    .checked_mul(channel_bytes)
                    .and_then(|base| {
                        (global_row as usize)
                            .checked_mul(raw_len)
                            .and_then(|row| base.checked_add(row))
                    })
                else {
                    buf.fill(0);
                    return Err("PSD/PSB HDR tiled ZIP row offset overflow".to_string());
                };
                let Some(end) = offset.checked_add(raw_len) else {
                    buf.fill(0);
                    return Err("PSD/PSB HDR tiled ZIP row end overflow".to_string());
                };
                if end <= planar.len() {
                    buf.copy_from_slice(&planar[offset..end]);
                } else {
                    buf.fill(0);
                    return Err("PSD/PSB HDR tiled ZIP row is out of bounds".to_string());
                }
            }
            _ => {
                buf.fill(0);
                return Err(format!(
                    "PSD/PSB HDR tiled row has unsupported compression {}",
                    self.compression
                ));
            }
        }
        Ok(())
    }

    /// Inflate the ZIP flattened Image Data once, applying prediction undo for
    /// compression 3. Returns `None` when this source is not ZIP-backed or the
    /// inflate fails (the caller then produces a blank row).
    fn ensure_zip_planar(&self) -> Option<Arc<Vec<u8>>> {
        let (start, end) = self.zip_compressed_range?;
        let width = self.width as usize;
        let depth = self.depth;
        let compression = self.compression;
        let expected = self
            .raw_row_bytes()
            .ok()?
            .checked_mul(self.height as usize)
            .and_then(|channel_bytes| channel_bytes.checked_mul(self.channels as usize))?;
        let cached = self.zip_planar.get_or_init(|| {
            let compressed = self.mmap.get(start..end)?;
            let mut planar = match crate::psb_zip::inflate_zlib_exact(compressed, expected) {
                Ok(planar) => planar,
                Err(e) => {
                    log::debug!("PSD/PSB HDR tiled ZIP inflate failed: {e}");
                    return None;
                }
            };
            if compression == 3
                && let Err(e) = crate::psb_zip::undo_zip_prediction(&mut planar, width, depth)
            {
                log::debug!("PSD/PSB HDR tiled ZIP prediction undo failed: {e}");
                return None;
            }
            Some(Arc::new(planar))
        });
        cached.clone()
    }

    fn decode_row_uncached(&self, ch_idx: u32, global_row: u32) -> Vec<u8> {
        let raw_len = match self.raw_row_bytes() {
            Ok(len) => len,
            Err(e) => {
                log::debug!(
                    "PSD/PSB HDR tiled row decode failed for channel {ch_idx}, row {global_row}: {e}"
                );
                return Vec::new();
            }
        };
        HDR_PSB_ROW_SCRATCH.with(|scratch| {
            let mut scratch = scratch.borrow_mut();
            scratch.clear();
            scratch.resize(raw_len, 0);
            if let Err(e) = self.decode_row_into(&mut scratch, ch_idx, global_row) {
                log::debug!(
                    "PSD/PSB HDR tiled row decode failed for channel {ch_idx}, row {global_row}: {e}"
                );
                scratch.fill(0);
            }
            std::mem::replace(&mut *scratch, Vec::with_capacity(raw_len))
        })
    }

    fn get_row(&self, ch_idx: u32, global_row: u32) -> Arc<Vec<u8>> {
        self.row_cache.get_with((ch_idx, global_row), || {
            Arc::new(self.decode_row_uncached(ch_idx, global_row))
        })
    }

    fn extract_tile_uncached(
        &self,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    ) -> Result<HdrTileBuffer, String> {
        let pixel_count = (width as usize)
            .checked_mul(height as usize)
            .ok_or_else(|| "PSD/PSB HDR tile pixel count overflow".to_string())?;
        let rgba_len = pixel_count
            .checked_mul(4)
            .ok_or_else(|| "PSD/PSB HDR tile rgba length overflow".to_string())?;
        let mut rgba = vec![0.0f32; rgba_len];
        let start = x as usize;
        let end = (x + width) as usize;
        let bps = self.bytes_per_sample();

        for rel_y in 0..height as usize {
            let src_y = y + rel_y as u32;
            let r = self.get_row(0, src_y);
            let g = self.get_row(1, src_y);
            let b = self.get_row(2, src_y);
            let a = if self.channels >= 4 {
                Some(self.get_row(3, src_y))
            } else {
                None
            };
            let dst_start = rel_y
                .checked_mul(width as usize)
                .and_then(|v| v.checked_mul(4))
                .ok_or_else(|| "PSD/PSB HDR tile row offset overflow".to_string())?;
            interleave_rgb_row_rgba32f(
                &mut rgba[dst_start..dst_start + width as usize * 4],
                NativeRowSpan {
                    r: &r,
                    g: &g,
                    b: &b,
                    a: a.as_deref().map(Vec::as_slice),
                    start,
                    end,
                },
                self.depth,
                bps,
                self.input_transfer,
                self.sdr_white_nits,
            )?;
        }

        Ok(HdrTileBuffer::new_with_metadata(
            width,
            height,
            HdrColorSpace::LinearSrgb,
            self.metadata.clone(),
            Arc::new(rgba),
        ))
    }

    pub(crate) fn is_absolute_blank(
        &self,
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<bool, crate::loader::DecodeError> {
        let mut any_rgb = false;
        let mut any_alpha = false;
        let strip_rows = crate::constants::PSB_DISK_TILED_BLANK_PROBE_STRIP_ROWS;
        let mut y = 0u32;
        while y < self.height {
            crate::psb_reader::check_decode_cancel(cancel)?;
            let h = (self.height - y).min(strip_rows);
            let tile = self.extract_tile_rgba32f_arc(0, y, self.width, h)?;
            feed_rgba32f_blank_flags(&tile.rgba_f32, &mut any_rgb, &mut any_alpha);
            if any_rgb && any_alpha {
                return Ok(false);
            }
            y = y.saturating_add(h);
        }
        Ok(!any_rgb || !any_alpha)
    }
}

/// Open a disk-backed tiled HDR source for PSD/PSB flattened Image Data.
pub fn open_hdr_tiled_flat_source(path: &Path) -> Result<PsbHdrTiledFlatSource, String> {
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
            .map_err(|e| format!("Cannot open PSD/PSB HDR tiled file: {e}"))?
    };
    let mmap = unsafe { Mmap::map(&file).map_err(|e| format!("Mmap failed: {e}"))? };
    open_hdr_tiled_flat_source_from_mmap(path, Arc::new(mmap))
}

/// Build an HDR flat tiled source from an already-mapped file (checklist #29).
pub fn open_hdr_tiled_flat_source_from_mmap(
    path: &Path,
    mmap: Arc<Mmap>,
) -> Result<PsbHdrTiledFlatSource, String> {
    let mut cursor = std::io::Cursor::new(&mmap[..]);

    let mut sig = [0u8; 4];
    cursor.read_exact(&mut sig).map_err(|e| e.to_string())?;
    if sig != *b"8BPS" {
        return Err("PSD/PSB HDR tiled source requires 8BPS signature".to_string());
    }
    let version = read_u16(&mut cursor)?;
    if version != 1 && version != 2 {
        return Err(format!(
            "Unsupported PSD/PSB version for HDR tiled source: {version}"
        ));
    }
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
    if depth == 8 {
        return Err(
            "PSD/PSB HDR tiled flat source requires 16 or 32-bit depth; got 8-bit".to_string(),
        );
    }
    if depth != 16 && depth != 32 {
        return Err(format!(
            "PSD/PSB HDR tiled flat source supports only 16 or 32-bit depth; got {depth}"
        ));
    }
    if color_mode == 1 {
        return Err("PSD/PSB HDR tiled flat source does not support Gray mode yet".to_string());
    }
    if color_mode == 4 {
        return Err("PSD/PSB HDR tiled flat source does not support CMYK mode yet".to_string());
    }
    if color_mode != 3 {
        return Err(format!(
            "PSD/PSB HDR tiled flat source supports RGB color mode only; got {color_mode}"
        ));
    }
    if channels < 3 {
        return Err(format!(
            "PSD/PSB HDR tiled RGB source requires at least 3 channels; got {channels}"
        ));
    }

    let bps = bytes_per_sample(depth)?;
    let row_raw_bytes = (width as u64)
        .checked_mul(bps as u64)
        .ok_or_else(|| "PSD/PSB HDR tiled row byte count overflow".to_string())?;

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
    // Phase 3 accepts ZIP (2|3) here; raw/RLE still go through
    // `tiled_compression_supported`, which rejects ZIP as non-tileable.
    if compression != 2 && compression != 3 {
        tiled_compression_supported(compression)?;
    }

    let data_start = cursor.position();
    let mut row_offsets = Vec::with_capacity(channels as usize * height as usize);
    let mut zip_compressed_range: Option<(usize, usize)> = None;
    match compression {
        0 => {
            let channel_bytes = row_raw_bytes
                .checked_mul(height as u64)
                .ok_or_else(|| "PSD/PSB HDR tiled channel byte count overflow".to_string())?;
            for ch in 0..channels {
                let ch_base = (ch as u64)
                    .checked_mul(channel_bytes)
                    .ok_or_else(|| "PSD/PSB HDR tiled channel offset overflow".to_string())?;
                for row in 0..height {
                    let row_off = (row as u64)
                        .checked_mul(row_raw_bytes)
                        .ok_or_else(|| "PSD/PSB HDR tiled row offset overflow".to_string())?;
                    row_offsets.push(
                        data_start
                            .checked_add(ch_base)
                            .and_then(|v| v.checked_add(row_off))
                            .ok_or_else(|| {
                                "PSD/PSB HDR tiled raw row offset overflow".to_string()
                            })?,
                    );
                }
            }
        }
        1 => {
            let total_rows = (channels as usize)
                .checked_mul(height as usize)
                .ok_or_else(|| "PSD/PSB HDR tiled row count overflow".to_string())?;
            let mut counts = Vec::with_capacity(total_rows);
            for _ in 0..total_rows {
                let count = if is_psb {
                    read_u32(&mut cursor)? as u64
                } else {
                    read_u16(&mut cursor)? as u64
                };
                counts.push(count);
            }
            let remaining = mmap.len().saturating_sub(cursor.position() as usize) as u64;
            let row_counts_usize: Vec<usize> = counts.iter().map(|&c| c as usize).collect();
            validate_rle_row_counts(
                &row_counts_usize,
                remaining,
                max_rle_compressed_row_bytes(
                    usize::try_from(row_raw_bytes)
                        .map_err(|_| "PSD/PSB HDR tiled RLE row byte count overflow".to_string())?,
                )?,
            )?;
            let mut running_offset = cursor.position();
            for count in counts {
                row_offsets.push(running_offset);
                running_offset = running_offset
                    .checked_add(count)
                    .ok_or_else(|| "PSD/PSB HDR tiled RLE row offset overflow".to_string())?;
            }
        }
        2 | 3 => {
            // ZIP is a single zlib stream of all channels planar (channel-major).
            // Refuse when the inflated planar buffer would exceed the budget so
            // the caller can fall through to the layer tiler / SDR path.
            let channel_bytes = row_raw_bytes
                .checked_mul(height as u64)
                .ok_or_else(|| "PSD/PSB HDR tiled ZIP channel byte count overflow".to_string())?;
            let expected = channel_bytes
                .checked_mul(channels as u64)
                .ok_or_else(|| "PSD/PSB HDR tiled ZIP planar size overflow".to_string())?;
            if expected > ZIP_PLANAR_MAX_BYTES as u64 {
                return Err(format!(
                    "PSD/PSB HDR tiled ZIP planar {expected} bytes exceeds budget {ZIP_PLANAR_MAX_BYTES}"
                ));
            }
            let start = usize::try_from(data_start)
                .map_err(|_| "PSD/PSB HDR tiled ZIP data offset overflow".to_string())?;
            zip_compressed_range = Some((start, mmap.len()));
        }
        _ => {
            return Err(format!(
                "Unsupported PSD/PSB HDR tiled compression: {compression}"
            ));
        }
    }

    let icc_probe = embedded_icc
        .as_deref()
        .map(probe_icc_hdr)
        .unwrap_or_default();
    let input_transfer = if depth == 32 {
        HdrTransferFunction::Linear
    } else {
        match icc_probe.transfer {
            HdrTransferFunction::Unknown => HdrTransferFunction::Linear,
            tf => tf,
        }
    };
    let color_profile = if let Some(icc) = embedded_icc {
        HdrColorProfile::Icc(Arc::new(icc))
    } else {
        HdrColorProfile::LinearSrgb
    };
    let metadata = HdrImageMetadata {
        transfer_function: HdrTransferFunction::Linear,
        reference: HdrReference::DisplayReferred,
        color_profile,
        luminance: HdrLuminanceMetadata {
            mastering_max_nits: icc_probe.peak_nits,
            sdr_white_nits: Some(DEFAULT_SDR_WHITE_NITS),
            ..Default::default()
        },
        gain_map: None,
        raw_gpu_source: None,
    };

    let row_cache = moka::sync::Cache::builder()
        .max_capacity(ROW_CACHE_BUDGET)
        .weigher(|_key: &(u32, u32), value: &Arc<Vec<u8>>| {
            u32::try_from(value.len()).unwrap_or(u32::MAX)
        })
        .build();

    Ok(PsbHdrTiledFlatSource {
        path: path.to_path_buf(),
        mmap,
        width,
        height,
        channels,
        depth,
        compression,
        row_offsets,
        zip_compressed_range,
        zip_planar: OnceLock::new(),
        row_cache,
        tile_cache: Mutex::new(HdrTileCache::new(configured_hdr_tile_cache_max_bytes())),
        metadata,
        input_transfer,
        sdr_white_nits: DEFAULT_SDR_WHITE_NITS,
    })
}

impl HdrTiledSource for PsbHdrTiledFlatSource {
    fn source_kind(&self) -> HdrTiledSourceKind {
        HdrTiledSourceKind::DiskBacked
    }

    fn source_name(&self) -> String {
        self.path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }

    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn color_space(&self) -> HdrColorSpace {
        HdrColorSpace::LinearSrgb
    }

    fn metadata(&self) -> HdrImageMetadata {
        self.metadata.clone()
    }

    fn generate_hdr_preview(
        &self,
        max_w: u32,
        max_h: u32,
    ) -> Result<crate::hdr::types::HdrImageBuffer, String> {
        hdr_preview_from_tiled_source_nearest(self, max_w, max_h)
    }

    fn generate_sdr_preview(&self, max_w: u32, max_h: u32) -> Result<(u32, u32, Vec<u8>), String> {
        let preview = self.generate_hdr_preview(max_w, max_h)?;
        let pixels = crate::hdr::decode::hdr_to_sdr_rgba8(&preview, 0.0)?;
        Ok((preview.width, preview.height, pixels))
    }

    fn cached_tile_rgba32f_arc(
        &self,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    ) -> Option<Arc<HdrTileBuffer>> {
        self.tile_cache.lock().get((x, y, width, height))
    }

    fn protect_cached_tiles(&self, tiles: &[(u32, u32, u32, u32)]) {
        self.tile_cache
            .lock()
            .set_protected_keys(tiles.iter().copied());
    }

    fn extract_tile_rgba32f_arc(
        &self,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    ) -> Result<Arc<HdrTileBuffer>, String> {
        validate_tile_bounds(self.width, self.height, x, y, width, height)?;
        let key = (x, y, width, height);
        {
            let mut cache = self.tile_cache.lock();
            if let Some(tile) = cache.get(key) {
                return Ok(tile);
            }
        }

        let tile = Arc::new(self.extract_tile_uncached(x, y, width, height)?);
        self.tile_cache.lock().insert(key, Arc::clone(&tile));
        Ok(tile)
    }
}

thread_local! {
    static HDR_PSB_ROW_SCRATCH: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

struct NativeRowSpan<'a> {
    r: &'a [u8],
    g: &'a [u8],
    b: &'a [u8],
    a: Option<&'a [u8]>,
    start: usize,
    end: usize,
}

fn interleave_rgb_row_rgba32f(
    dst: &mut [f32],
    span: NativeRowSpan<'_>,
    depth: u16,
    bps: usize,
    transfer: HdrTransferFunction,
    sdr_white_nits: f32,
) -> Result<(), String> {
    let width = span
        .end
        .checked_sub(span.start)
        .ok_or_else(|| "PSD/PSB HDR tile span underflow".to_string())?;
    if dst.len() < width * 4 {
        return Err("PSD/PSB HDR tile destination row is too small".to_string());
    }
    for i in 0..width {
        let sample_idx = span.start + i;
        let mut rgb = [
            native_sample_f32(span.r, sample_idx, bps),
            native_sample_f32(span.g, sample_idx, bps),
            native_sample_f32(span.b, sample_idx, bps),
        ];
        if depth == 16
            && !matches!(
                transfer,
                HdrTransferFunction::Linear
                    | HdrTransferFunction::Gamma
                    | HdrTransferFunction::Unknown
            )
        {
            rgb = crate::hdr::decode::decode_transfer_to_display_linear(
                rgb,
                transfer,
                sdr_white_nits,
            );
        }
        let alpha = span
            .a
            .map(|a| native_sample_f32(a, sample_idx, bps).clamp(0.0, 1.0))
            .unwrap_or(1.0);
        let off = i * 4;
        dst[off] = rgb[0];
        dst[off + 1] = rgb[1];
        dst[off + 2] = rgb[2];
        dst[off + 3] = alpha;
    }
    Ok(())
}

#[inline]
fn native_sample_f32(row: &[u8], sample_idx: usize, bps: usize) -> f32 {
    let off = sample_idx.saturating_mul(bps);
    match bps {
        2 if off + 1 < row.len() => u16::from_be_bytes([row[off], row[off + 1]]) as f32 / 65535.0,
        4 if off + 3 < row.len() => {
            f32::from_be_bytes([row[off], row[off + 1], row[off + 2], row[off + 3]])
        }
        _ => 0.0,
    }
}

fn feed_rgba32f_blank_flags(pixels: &[f32], any_rgb: &mut bool, any_alpha: &mut bool) {
    const EPS: f32 = 1e-8;
    let mut i = 0usize;
    while i + 4 <= pixels.len() {
        if pixels[i].abs() > EPS || pixels[i + 1].abs() > EPS || pixels[i + 2].abs() > EPS {
            *any_rgb = true;
        }
        if pixels[i + 3].abs() > EPS {
            *any_alpha = true;
        }
        if *any_rgb && *any_alpha {
            return;
        }
        i += 4;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn psb_hdr_tiled_open_rejects_8_bit_flat_file() {
        let channels = [vec![0.0], vec![0.0], vec![0.0]];
        let path = write_temp_psd("psb_hdr_tiled_reject_8", 8, &channels, None);
        let err = open_hdr_tiled_flat_source(&path).expect_err("8-bit HDR tiled should fail");
        assert!(err.contains("requires 16 or 32-bit depth"), "{err}");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn psb_hdr_tiled_interleaves_32_bit_rgb_tile() {
        let channels = [
            vec![1.0, 2.0, 3.0, 4.0],
            vec![0.5, 0.6, 0.7, 0.8],
            vec![0.0, 0.1, 0.2, 0.3],
        ];
        let path = write_temp_psd("psb_hdr_tiled_rgb32", 32, &channels, None);
        let source = open_hdr_tiled_flat_source(&path).expect("open 32-bit RGB");
        assert_eq!(source.source_kind(), HdrTiledSourceKind::DiskBacked);
        let tile = source
            .extract_tile_rgba32f_arc(0, 0, 2, 2)
            .expect("extract tile");
        assert_eq!(tile.width, 2);
        assert_eq!(tile.height, 2);
        assert_eq!(
            tile.rgba_f32.as_slice(),
            &[
                1.0, 0.5, 0.0, 1.0, 2.0, 0.6, 0.1, 1.0, 3.0, 0.7, 0.2, 1.0, 4.0, 0.8, 0.3, 1.0,
            ]
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn psb_hdr_tiled_16_bit_rgb_samples_normalize() {
        let mut dst = vec![0.0f32; 8];
        let r = [0x00, 0x00, 0xFF, 0xFF];
        let g = [0x80, 0x00, 0x40, 0x00];
        let b = [0x20, 0x00, 0x10, 0x00];
        interleave_rgb_row_rgba32f(
            &mut dst,
            NativeRowSpan {
                r: &r,
                g: &g,
                b: &b,
                a: None,
                start: 0,
                end: 2,
            },
            16,
            2,
            HdrTransferFunction::Linear,
            DEFAULT_SDR_WHITE_NITS,
        )
        .expect("interleave");
        assert_eq!(dst[0], 0.0);
        assert!((dst[4] - 1.0).abs() < 1e-6);
        assert_eq!(dst[3], 1.0);
        assert_eq!(dst[7], 1.0);
    }

    fn write_temp_psd(
        name: &str,
        depth: u16,
        channels: &[Vec<f32>; 3],
        alpha: Option<&[f32]>,
    ) -> PathBuf {
        let mut path = std::env::temp_dir();
        let unique = format!(
            "{name}_{}_{}.psd",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        path.push(unique);
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"8BPS");
        bytes.extend_from_slice(&1u16.to_be_bytes());
        bytes.extend_from_slice(&[0u8; 6]);
        let channel_count = if alpha.is_some() { 4u16 } else { 3u16 };
        bytes.extend_from_slice(&channel_count.to_be_bytes());
        bytes.extend_from_slice(&2u32.to_be_bytes());
        bytes.extend_from_slice(&2u32.to_be_bytes());
        bytes.extend_from_slice(&depth.to_be_bytes());
        bytes.extend_from_slice(&3u16.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&0u16.to_be_bytes());
        for channel in channels {
            for &v in channel {
                append_sample(&mut bytes, depth, v);
            }
        }
        if let Some(alpha) = alpha {
            for &v in alpha {
                append_sample(&mut bytes, depth, v);
            }
        }
        std::fs::write(&path, bytes).expect("write temp PSD");
        path
    }

    #[test]
    fn psb_hdr_tiled_zip_flat_32_bit_rgb_tile() {
        // Flattened RGB planar (channel-major), each channel 2x2 f32 BE, then
        // zlib-compressed as a single stream (compression 2, no prediction).
        let width = 2usize;
        let height = 2usize;
        let px = width * height;
        let rgb = [1.0f32, 0.25, 0.1];
        let mut planar = Vec::new();
        for &v in &rgb {
            for _ in 0..px {
                planar.extend_from_slice(&v.to_be_bytes());
            }
        }
        let compressed = miniz_oxide::deflate::compress_to_vec_zlib(&planar, 6);

        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"8BPS");
        bytes.extend_from_slice(&1u16.to_be_bytes());
        bytes.extend_from_slice(&[0u8; 6]);
        bytes.extend_from_slice(&3u16.to_be_bytes()); // channels
        bytes.extend_from_slice(&(height as u32).to_be_bytes());
        bytes.extend_from_slice(&(width as u32).to_be_bytes());
        bytes.extend_from_slice(&32u16.to_be_bytes()); // depth
        bytes.extend_from_slice(&3u16.to_be_bytes()); // RGB
        bytes.extend_from_slice(&0u32.to_be_bytes()); // color mode data
        bytes.extend_from_slice(&0u32.to_be_bytes()); // image resources
        bytes.extend_from_slice(&0u32.to_be_bytes()); // empty layer/mask
        bytes.extend_from_slice(&2u16.to_be_bytes()); // ZIP without prediction
        bytes.extend_from_slice(&compressed);

        let mut path = std::env::temp_dir();
        path.push(format!(
            "psb_hdr_tiled_zip32_{}_{}.psd",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, bytes).expect("write temp ZIP PSD");

        let source = open_hdr_tiled_flat_source(&path).expect("open ZIP flat 32-bit");
        let tile = source
            .extract_tile_rgba32f_arc(0, 0, 2, 2)
            .expect("extract ZIP tile");
        assert_eq!((tile.width, tile.height), (2, 2));
        let out = &tile.rgba_f32[0..4];
        assert!((out[0] - 1.0).abs() < 1e-5, "R: {out:?}");
        assert!((out[1] - 0.25).abs() < 1e-5, "G: {out:?}");
        assert!((out[2] - 0.1).abs() < 1e-5, "B: {out:?}");
        let _ = std::fs::remove_file(path);
    }

    fn append_sample(bytes: &mut Vec<u8>, depth: u16, value: f32) {
        match depth {
            8 => bytes.push((value.clamp(0.0, 1.0) * 255.0).round() as u8),
            16 => bytes.extend_from_slice(
                &((value.clamp(0.0, 1.0) * 65535.0).round() as u16).to_be_bytes(),
            ),
            32 => bytes.extend_from_slice(&value.to_be_bytes()),
            _ => unreachable!(),
        }
    }
}
