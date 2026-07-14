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
//! Constants and utility helpers have been extracted to [`psb_reader_util`]
//! and re-exported here (checklist #12 2000-line limit).
//!
//! Reference: Adobe Photoshop File Formats Specification (March 2013)

use memmap2::Mmap;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::AtomicBool;

// SIMD architecture-specific imports are handled within submodules

// Constants, utility helpers, and SIMD blank-detection functions are
// in `psb_reader_util` and re-exported here to stay under the 2000-line
// threshold (checklist #12).
pub(crate) use crate::psb_reader_util::*;

// All constants (compression, color mode, channel IDs, limits, IR, cancel
// poll masks) are defined in `psb_reader_util` and re-exported above.

/// Decoded PSD/PSB composite image (full in-memory RGBA8).
#[derive(Debug)]
#[allow(dead_code)]
pub struct PsbComposite {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>, // RGBA8
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
    check_decode_cancel(cancel)?;
    let index = crate::psb_section_index::PsdSectionIndex::parse(bytes)?;
    read_composite_from_index(&index, bytes, cancel)
}

/// Decode the flattened Image Data section starting at `index.image_data_pos`.
///
/// Does not re-parse the header, color mode data, image resources, or layer
/// and mask info sections -- `index` already located and validated them.
pub fn read_composite_from_index(
    index: &crate::psb_section_index::PsdSectionIndex,
    bytes: &[u8],
    cancel: Option<&AtomicBool>,
) -> Result<PsbComposite, crate::loader::DecodeError> {
    let file_size = bytes.len() as u64;

    let width = index.width;
    let height = index.height;
    let channels = index.channels;
    let depth = index.depth;
    let color_mode = index.color_mode;
    let is_psb = index.is_psb;
    ensure_supported_color_mode(color_mode)?;
    let bps = bytes_per_sample(depth)?;
    let embedded_icc = extract_icc_profile_from_ir(bytes, index.ir_start, index.ir_end);

    log::debug!(
        "PSD/PSB header: {}x{}, {} channels, {}-bit, color_mode={}, version={}",
        width,
        height,
        channels,
        depth,
        color_mode,
        if is_psb { 2 } else { 1 }
    );

    check_decode_cancel(cancel)?;

    // -- Section 5: Image Data (flattened composite) --
    let compression = index.image_data_compression(bytes)?;
    // Spec: 0=Raw, 1=RLE, 2=ZIP, 3=ZIP+prediction. Anything else is invalid.
    if compression > 3 {
        return Err(format!("Invalid PSD/PSB Image Data compression: {compression}").into());
    }

    let mut r = std::io::Cursor::new(bytes);
    r.seek(SeekFrom::Start(index.image_data_pos + 2))
        .map_err(|e| format!("Seek error: {e}"))?;

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
    if compression == PSD_COMPRESSION_RLE {
        row_counts.reserve(total_rows);
        for i in 0..total_rows {
            if i & RLE_ROW_COUNT_CANCEL_POLL_INTERVAL == 0 {
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
        validate_rle_row_counts(
            &row_counts,
            remaining,
            max_rle_compressed_row_bytes(row_raw_bytes)?,
        )?;
    }

    // --- Bitmap depth=1: packed 1-bit per pixel ---
    // The general read path treats every channel as `pixel_count * bps` bytes,
    // but depth=1 Bitmap stores packed bits: 8 pixels per byte.
    if color_mode == PSD_COLOR_MODE_BITMAP && depth == 1 {
        let packed_count = crate::psb_color_convert::bitmap_packed_byte_count(pixel_count);
        let packed_row = crate::psb_color_convert::bitmap_packed_byte_count(width as usize);
        let mut ch_u8 = vec![0u8; pixel_count];
        match compression {
            PSD_COMPRESSION_RAW => {
                ensure_readable_within(
                    &mut r,
                    packed_count as u64,
                    file_size,
                    "bitmap raw channel",
                )?;
                let mut packed = vec![0u8; packed_count];
                r.read_exact(&mut packed)
                    .map_err(|e| format!("Read bitmap channel: {e}"))?;
                crate::psb_color_convert::bitmap_expand_bits_to_u8(&mut ch_u8, &packed);
            }
            PSD_COMPRESSION_RLE => {
                for row in 0..height as usize {
                    if row & RLE_ROW_DECODE_CANCEL_POLL_INTERVAL == 0 {
                        check_decode_cancel(cancel)?;
                    }
                    let idx = row; // single channel, row = row index
                    let compressed_len = *row_counts
                        .get(idx)
                        .ok_or_else(|| format!("Bitmap RLE row index {idx} out of range"))?;
                    ensure_readable_within(
                        &mut r,
                        compressed_len as u64,
                        file_size,
                        "bitmap RLE row",
                    )?;
                    let mut packed_row_buf = Vec::new();
                    let mut compressed = vec![0u8; compressed_len];
                    r.read_exact(&mut compressed)
                        .map_err(|e| format!("Read bitmap RLE: {e}"))?;
                    unpack_bits_into(&mut packed_row_buf, &compressed, packed_row)?;
                    let dst_start = row
                        .checked_mul(width as usize)
                        .ok_or_else(|| "PSD/PSB bitmap row offset overflow".to_string())?;
                    let dst_end = dst_start + width as usize;
                    crate::psb_color_convert::bitmap_expand_bits_to_u8(
                        &mut ch_u8[dst_start..dst_end],
                        &packed_row_buf,
                    );
                }
            }
            PSD_COMPRESSION_ZIP | PSD_COMPRESSION_ZIP_PREDICTION => {
                let data_start = r.stream_position().map_err(|e| format!("{e}"))? as usize;
                let compressed = bytes
                    .get(data_start..)
                    .ok_or_else(|| "PSD/PSB ZIP bitmap out of bounds".to_string())?;
                let packed_total = (channels as usize)
                    .checked_mul(packed_count)
                    .ok_or_else(|| "PSD/PSB bitmap ZIP size overflow".to_string())?;
                let planar = crate::psb_zip::inflate_zlib_exact(compressed, packed_total)?;
                let raw = planar
                    .get(..packed_count)
                    .ok_or_else(|| "PSD/PSB bitmap ZIP channel slice OOB".to_string())?;
                crate::psb_color_convert::bitmap_expand_bits_to_u8(&mut ch_u8, raw);
            }
            _ => return Err(format!("Unsupported compression for bitmap: {compression}").into()),
        }
        let mut rgba = vec![255u8; checked_rgba_len(pixel_count)?];
        // Remainder check: all height rows fit in the sample count.
        for row in 0..height as usize {
            if row & RLE_ROW_DECODE_CANCEL_POLL_INTERVAL == 0 {
                check_decode_cancel(cancel)?;
            }
            let s = row
                .checked_mul(width as usize)
                .ok_or_else(|| "PSD/PSB bitmap RGBA row offset overflow".to_string())?;
            let e = s + width as usize;
            let dstart = s * 4;
            let dend = e * 4;
            let dst_row = rgba
                .get_mut(dstart..dend)
                .ok_or_else(|| format!("PSD/PSB bitmap RGBA row OOB ({dstart}..{dend})"))?;
            let src_row = ch_u8.get(s..e).unwrap_or(&[]);
            for col in 0..width as usize {
                let v = src_row.get(col).copied().unwrap_or(0);
                dst_row[col * 4] = v;
                dst_row[col * 4 + 1] = v;
                dst_row[col * 4 + 2] = v;
                dst_row[col * 4 + 3] = 255;
            }
        }
        return Ok(PsbComposite {
            width,
            height,
            pixels: rgba,
        });
    }

    // Step 1: Read planar channels and down-convert to 8-bit samples.
    let mut planar_channels: Vec<Option<Vec<u8>>> = vec![None; channels as usize];

    if compression == PSD_COMPRESSION_ZIP || compression == PSD_COMPRESSION_ZIP_PREDICTION {
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
        if (expected as u64) > MAX_ZIP_PLANAR_INFLATE_BYTES {
            return Err(format!(
                "PSD/PSB ZIP planar {expected} bytes exceeds budget {MAX_ZIP_PLANAR_INFLATE_BYTES}"
            )
            .into());
        }
        check_decode_cancel(cancel)?;
        let mut planar = crate::psb_zip::inflate_zlib_exact(compressed, expected)?;
        if compression == PSD_COMPRESSION_ZIP_PREDICTION {
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
        // Reuse across channels: RLE row scratch + compressed row buffer.
        let mut row_raw = Vec::with_capacity(row_raw_bytes);
        let mut compressed = Vec::new();
        for ch_idx in 0..channels {
            check_decode_cancel(cancel)?;
            let is_used = channel_is_used(color_mode, ch_idx, channels);

            if is_used {
                let mut ch_u8 = vec![0u8; samples_per_channel];
                match compression {
                    PSD_COMPRESSION_RAW => {
                        ensure_readable_within(
                            &mut r,
                            raw_channel_bytes as u64,
                            file_size,
                            "raw channel data",
                        )?;
                        let mut raw = vec![0u8; raw_channel_bytes];
                        r.read_exact(&mut raw)
                            .map_err(|e| format!("Read raw channel {ch_idx}: {e}"))?;
                        check_decode_cancel(cancel)?;
                        downconvert_samples_to_u8(&mut ch_u8, &raw, bps);
                    }
                    PSD_COMPRESSION_RLE => {
                        for row in 0..height as usize {
                            if row & RLE_ROW_DECODE_CANCEL_POLL_INTERVAL == 0 {
                                check_decode_cancel(cancel)?;
                            }
                            let idx = ch_idx as usize * height as usize + row;
                            let compressed_len = *row_counts
                                .get(idx)
                                .ok_or_else(|| format!("Row count index {idx} out of range"))?;
                            ensure_readable_within(
                                &mut r,
                                compressed_len as u64,
                                file_size,
                                "RLE row data",
                            )?;
                            // Reuse capacity across rows; read_exact overwrites every byte.
                            compressed.clear();
                            if compressed.capacity() < compressed_len {
                                compressed.reserve(compressed_len);
                            }
                            // SAFETY: capacity >= compressed_len; read_exact fills all bytes
                            // or we return Err without reading `compressed`.
                            unsafe {
                                compressed.set_len(compressed_len);
                            }
                            r.read_exact(&mut compressed)
                                .map_err(|e| format!("Read RLE: {e}"))?;
                            unpack_bits_into(&mut row_raw, &compressed, row_raw_bytes)?;
                            // Safe: `checked_pixel_count` already proved width*height fits
                            // in usize, and row < height here.
                            let dst_start = row.checked_mul(width as usize).ok_or_else(|| {
                                "PSD/PSB layer row*width overflow: checked_pixel_count guarantees \
                                 safety, but decode paths must not panic"
                                    .to_string()
                            })?;
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
                    PSD_COMPRESSION_RAW => {
                        seek_forward_within(
                            &mut r,
                            raw_channel_bytes as u64,
                            file_size,
                            "raw channel data",
                        )?;
                    }
                    PSD_COMPRESSION_RLE => {
                        // Sum precomputed row counts and skip the whole unused
                        // channel in one seek (avoids height sequential seeks).
                        seek_rle_channel_skip(
                            &mut r,
                            &row_counts,
                            ch_idx as usize,
                            height as usize,
                            file_size,
                            "RLE unused channel",
                            cancel,
                        )?;
                    }
                    _ => {}
                }
            }
        }
    }

    // --- Indexed (2): expand palette indices into RGB planar channels ---
    if color_mode == PSD_COLOR_MODE_INDEXED {
        let palette = crate::psb_color_convert::extract_indexed_palette(bytes).ok_or_else(|| {
            "Indexed (palette) PSD/PSB missing 768-byte palette in Color Mode Data section"
                .to_string()
        })?;
        // Take ownership of the index plane (avoids borrow conflict with take below).
        let indices_opt = planar_channels[0].take();
        if let Some(indices) = indices_opt {
            let n = pixel_count.min(indices.len());
            let orig_alpha = if channels >= 2 {
                planar_channels[1].take()
            } else {
                None
            };
            let mut r_plane = vec![0u8; n];
            let mut g_plane = vec![0u8; n];
            let mut b_plane = vec![0u8; n];
            for i in 0..n {
                let idx = indices[i] as usize;
                if idx < 256 {
                    let pb = idx * 3;
                    r_plane[i] = palette.get(pb).copied().unwrap_or(0);
                    g_plane[i] = palette.get(pb + 1).copied().unwrap_or(0);
                    b_plane[i] = palette.get(pb + 2).copied().unwrap_or(0);
                }
            }
            let a_plane = orig_alpha.unwrap_or_else(|| vec![255u8; n]);
            planar_channels = vec![Some(r_plane), Some(g_plane), Some(b_plane), Some(a_plane)];
        }
    }
    // After Indexed expansion the effective channel count becomes 3 (no alpha) or 4.
    let channels = if color_mode == PSD_COLOR_MODE_INDEXED {
        4u32
    } else {
        channels
    };

    // Step 2: Interleave into RGBA8 (CMYK goes through lcms2 when possible).
    let mut rgba = vec![255u8; checked_rgba_len(pixel_count)?];
    let cmyk_cms_ok = color_mode == PSD_COLOR_MODE_CMYK
        && channels >= 4
        && match (
            planar_channels[0].as_deref(),
            planar_channels[1].as_deref(),
            planar_channels[2].as_deref(),
            planar_channels[3].as_deref(),
        ) {
            (Some(c), Some(m), Some(y), Some(k)) => {
                let icc = crate::psb_cmyk_cms::resolve_cmyk_icc(embedded_icc.as_deref());
                let a = if channels >= 5 {
                    planar_channels.get(4).and_then(|ch| ch.as_deref())
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
            }
            _ => false,
        };
    if !cmyk_cms_ok {
        for row in 0..height as usize {
            if row & RLE_ROW_DECODE_CANCEL_POLL_INTERVAL == 0 {
                check_decode_cancel(cancel)?;
            }
            // Defensive range check: `checked_pixel_count` already proved width*height
            // fits in usize, but use checked_mul for consistency with checklist #35.
            let start = row
                .checked_mul(width as usize)
                .ok_or_else(|| "PSD/PSB RGBA row offset overflow".to_string())?;
            let end = start + width as usize;
            let dst_start = start
                .checked_mul(RGBA_BYTES_PER_PIXEL)
                .ok_or_else(|| "PSD/PSB RGBA row offset overflow".to_string())?;
            let dst_end = end
                .checked_mul(RGBA_BYTES_PER_PIXEL)
                .ok_or_else(|| "PSD/PSB RGBA row end overflow".to_string())?;
            let dst_row = rgba.get_mut(dst_start..dst_end).ok_or_else(|| {
                format!("PSD/PSB RGBA row slice out of bounds ({dst_start}..{dst_end})")
            })?;
            interleave_row_rgba8(dst_row, &planar_channels, color_mode, channels, start, end);
        }
    }

    Ok(PsbComposite {
        width,
        height,
        pixels: rgba,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        HDR_RGBA_F32_BYTES_PER_PIXEL, MAX_DOCUMENT_PIXELS, MAX_ZIP_PLANAR_INFLATE_BYTES,
        PACKBITS_MAX_NOOPS_PER_ROW, PACKBITS_TOO_MANY_NOOPS, PSD_COMPRESSION_RAW,
        PSD_COMPRESSION_ZIP, PSD_COMPRESSION_ZIP_PREDICTION, PSD_MAX_DIMENSION,
        RGBA_BYTES_PER_PIXEL, channel_is_used, checked_pixel_count, cmyk_to_rgb,
        downconvert_samples_to_u8, ensure_supported_color_mode, estimate_memory_from_bytes,
        for_each_image_resource, max_rle_compressed_row_bytes,
        read_composite_from_bytes_with_cancel, seek_rle_channel_skip, unpack_bits_into,
        validate_psd_dimensions, validate_rle_row_counts,
    };
    use std::io::Cursor;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn minimal_psd_header(width: u32, height: u32, channels: u16, depth: u16) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(26);
        bytes.extend_from_slice(b"8BPS");
        bytes.extend_from_slice(&1u16.to_be_bytes());
        bytes.extend_from_slice(&[0u8; 6]);
        bytes.extend_from_slice(&channels.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&depth.to_be_bytes());
        bytes.extend_from_slice(&3u16.to_be_bytes()); // RGB
        bytes
    }

    #[test]
    fn validate_psd_dimensions_allows_hubble_class_over_full_canvas_cap() {
        // Structural validation must accept canvases above MAX_DOCUMENT_PIXELS so
        // load_psd can open them via disk tiling (e.g. Hubble heic1502a.psb).
        assert!(validate_psd_dimensions(69_536, 22_230, 3).is_ok());
        assert!(validate_psd_dimensions(32_769, 32_769, 3).is_ok());
        let err = validate_psd_dimensions(PSD_MAX_DIMENSION + 1, 1, 3).unwrap_err();
        assert!(err.contains(&PSD_MAX_DIMENSION.to_string()), "err={err}");
    }

    #[test]
    fn checked_pixel_count_rejects_over_document_pixel_cap() {
        assert!(checked_pixel_count(32_768, 32_768).is_ok());
        let err = checked_pixel_count(32_769, 32_769).unwrap_err();
        assert!(err.contains(&MAX_DOCUMENT_PIXELS.to_string()), "err={err}");
        let err = checked_pixel_count(PSD_MAX_DIMENSION, PSD_MAX_DIMENSION).unwrap_err();
        assert!(err.contains("exceed maximum"), "err={err}");
    }

    #[test]
    fn estimate_memory_accepts_hubble_class_dimensions() {
        let header = minimal_psd_header(69_536, 22_230, 3, 8);
        let (w, h, ch, estimated) = estimate_memory_from_bytes(&header).unwrap();
        assert_eq!((w, h, ch), (69_536, 22_230, 3));
        let pixels = 69_536u64 * 22_230u64;
        assert_eq!(estimated, pixels * 4 + pixels * 3);
        assert!(pixels > MAX_DOCUMENT_PIXELS);
    }

    #[test]
    fn estimate_memory_hdr32_counts_planar_peak_and_rgba_f32() {
        let width = 64u32;
        let height = 32u32;
        let channels = 3u16;
        let header = minimal_psd_header(width, height, channels, 32);
        let (w, h, ch, estimated) = estimate_memory_from_bytes(&header).unwrap();
        assert_eq!((w, h, ch), (width, height, channels as u32));
        let pixels = u64::from(width) * u64::from(height);
        let planar = pixels * u64::from(channels) * 4;
        let expected = planar * 2 + pixels * HDR_RGBA_F32_BYTES_PER_PIXEL;
        assert_eq!(estimated, expected);
        // Must be strictly above the old RGBA8+planar underestimate (16 B/px for RGB32).
        let old_underestimate = pixels * RGBA_BYTES_PER_PIXEL as u64 + planar;
        assert!(estimated > old_underestimate);
    }

    #[test]
    fn estimate_memory_sdr8_is_rgba_plus_planar() {
        let header = minimal_psd_header(10, 20, 4, 8);
        let (_, _, _, estimated) = estimate_memory_from_bytes(&header).unwrap();
        assert_eq!(estimated, 10 * 20 * 4 + 10 * 20 * 4);
    }

    #[test]
    fn image_resource_walk_accepts_8bim_and_stops_at_8b64() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"8BIM");
        bytes.extend_from_slice(&1039u16.to_be_bytes());
        bytes.extend_from_slice(&[0, 0]);
        bytes.extend_from_slice(&3u32.to_be_bytes());
        bytes.extend_from_slice(&[1, 2, 3, 0]);
        bytes.extend_from_slice(b"8B64");
        bytes.extend_from_slice(&1040u16.to_be_bytes());
        bytes.extend_from_slice(&[0, 0]);
        bytes.extend_from_slice(&0u64.to_be_bytes());

        let found = for_each_image_resource(&bytes, 0, bytes.len() as u64, |rid, data| {
            (rid == 1039).then(|| data.to_vec())
        });
        assert_eq!(found, Some(vec![1, 2, 3]));

        let mut visited = Vec::new();
        let _: Option<()> = for_each_image_resource(&bytes, 0, bytes.len() as u64, |rid, _| {
            visited.push(rid);
            None
        });
        assert_eq!(visited, vec![1039]);
    }

    #[test]
    fn unpack_bits_rejects_excessive_packbits_noops() {
        let data = vec![0x80u8; PACKBITS_MAX_NOOPS_PER_ROW + 1];
        let mut out = Vec::new();
        let err = unpack_bits_into(&mut out, &data, 16).unwrap_err();
        assert_eq!(err, PACKBITS_TOO_MANY_NOOPS);
        assert_eq!(out.len(), 16);
        assert!(out.iter().all(|&b| b == 0));
    }

    #[test]
    fn unpack_bits_allows_noop_count_at_limit() {
        let mut data = vec![0x80u8; PACKBITS_MAX_NOOPS_PER_ROW];
        // Literal run: n=3 copies next 4 bytes.
        data.push(3);
        data.extend_from_slice(&[1, 2, 3, 4]);
        let mut out = Vec::new();
        unpack_bits_into(&mut out, &data, 4).unwrap();
        assert_eq!(out, [1, 2, 3, 4]);
    }

    #[test]
    fn unpack_bits_rejects_truncated_literal() {
        // n=3 requires 4 payload bytes; only 2 follow.
        let data = [3u8, 1, 2];
        let mut out = Vec::new();
        let err = unpack_bits_into(&mut out, &data, 4).unwrap_err();
        assert!(err.contains("PackBits"), "{err}");
        assert_eq!(out.len(), 4);
        assert!(out.iter().all(|&b| b == 0));
    }

    #[test]
    fn unpack_bits_rejects_repeat_missing_value() {
        // n=-1 (0xFF) repeats the next byte twice; value byte absent.
        let data = [0xFFu8];
        let mut out = Vec::new();
        let err = unpack_bits_into(&mut out, &data, 2).unwrap_err();
        assert!(err.contains("PackBits"), "{err}");
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|&b| b == 0));
    }

    #[test]
    fn unpack_bits_rejects_early_eof_before_expected_len() {
        // Literal of 2 bytes, but expected_len is 4.
        let data = [1u8, 10, 20];
        let mut out = Vec::new();
        let err = unpack_bits_into(&mut out, &data, 4).unwrap_err();
        assert!(err.contains("PackBits"), "{err}");
        assert_eq!(out.len(), 4);
        assert!(out.iter().all(|&b| b == 0));
    }

    #[test]
    fn unpack_bits_allows_trailing_padding_after_exact_fill() {
        let mut data = vec![3u8, 1, 2, 3, 4];
        data.extend_from_slice(&[0x80, 0x80]); // trailing no-ops / padding
        let mut out = Vec::new();
        unpack_bits_into(&mut out, &data, 4).unwrap();
        assert_eq!(out, [1, 2, 3, 4]);
    }

    #[test]
    fn validate_rle_row_counts_rejects_oversized_single_row() {
        let row_raw = 16usize;
        let max_row = max_rle_compressed_row_bytes(row_raw).unwrap();
        let err = validate_rle_row_counts(&[max_row + 1], 1_000_000, max_row).unwrap_err();
        assert!(err.contains("exceeds limit"), "{err}");
    }

    #[test]
    fn seek_rle_channel_skip_jumps_whole_channel_in_one_seek() {
        // Two channels x 3 rows: skip channel 1 (counts 10+20+30 = 60).
        let counts = [1usize, 2, 3, 10, 20, 30];
        let mut buf = [0u8; 100];
        let mut cur = Cursor::new(&mut buf[..]);
        cur.set_position(1 + 2 + 3); // after channel 0
        seek_rle_channel_skip(&mut cur, &counts, 1, 3, 100, "test skip", None).unwrap();
        assert_eq!(cur.position(), 1 + 2 + 3 + 60);
    }

    #[test]
    fn validate_rle_row_counts_accepts_at_limit() {
        let row_raw = 16usize;
        let max_row = max_rle_compressed_row_bytes(row_raw).unwrap();
        validate_rle_row_counts(&[max_row, max_row], (max_row * 2) as u64, max_row).unwrap();
    }

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
    fn tiled_compression_supported_refuses_zip_modes() {
        assert!(super::tiled_compression_supported(0).is_ok());
        assert!(super::tiled_compression_supported(1).is_ok());

        for compression in [2, 3] {
            let err = super::tiled_compression_supported(compression).unwrap_err();
            assert_eq!(err, "PSD/PSB ZIP Image Data cannot be opened as disk-tiled");
        }
    }

    #[test]
    fn read_composite_rejects_color_mode_section_past_eof() {
        // Cursor::seek past EOF succeeds; without a file_size check the failure
        // is deferred to a later read_exact with a less specific error.
        // Pad to PSD_MIN_STRUCTURAL_LEN (38) so Truncated does not fire first.
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
        bytes.extend_from_slice(&[0u8; 8]); // ir_len + lm_len placeholders
        assert_eq!(bytes.len(), 38);
        let err = super::read_composite_from_bytes(&bytes).expect_err("past eof");
        let msg = err.as_str();
        assert!(
            msg.contains("color mode") && msg.contains("exceeds"),
            "expected early section-bound error, got: {msg}"
        );
    }

    #[test]
    fn read_composite_rejects_truncated_raw_channel_with_section_bound_error() {
        // 2x2 RGB8 raw needs 12 planar bytes after the compression field; omit them.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"8BPS");
        bytes.extend_from_slice(&1u16.to_be_bytes());
        bytes.extend_from_slice(&[0u8; 6]);
        bytes.extend_from_slice(&3u16.to_be_bytes());
        bytes.extend_from_slice(&2u32.to_be_bytes());
        bytes.extend_from_slice(&2u32.to_be_bytes());
        bytes.extend_from_slice(&8u16.to_be_bytes());
        bytes.extend_from_slice(&3u16.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes()); // color mode data
        bytes.extend_from_slice(&0u32.to_be_bytes()); // image resources
        bytes.extend_from_slice(&0u32.to_be_bytes()); // layer/mask
        bytes.extend_from_slice(&0u16.to_be_bytes()); // raw compression
        // Only 3 of 12 needed planar bytes.
        bytes.extend_from_slice(&[1, 2, 3]);
        let err = super::read_composite_from_bytes(&bytes).expect_err("truncated raw");
        let msg = err.as_str();
        assert!(
            msg.contains("raw channel data") && msg.contains("exceeds"),
            "expected section-bound error matching seek_forward_within, got: {msg}"
        );
    }

    #[test]
    fn photoshop_thumbnail_returns_none_when_jpeg_size_truncated() {
        let mut data = vec![0u8; super::IR_JPEG_THUMBNAIL_HEADER_LEN + 4];
        data[0..4].copy_from_slice(&1u32.to_be_bytes()); // JPEG format
        data[4..8].copy_from_slice(&8u32.to_be_bytes()); // width
        data[8..12].copy_from_slice(&8u32.to_be_bytes()); // height
        let compressed_off = super::IR_JPEG_THUMBNAIL_COMPRESSED_SIZE_OFFSET;
        // Claim far more JPEG bytes than the resource actually holds.
        data[compressed_off..compressed_off + 4].copy_from_slice(&1024u32.to_be_bytes());
        assert!(super::decode_photoshop_thumbnail_resource(&data).is_none());
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
            PSD_COMPRESSION_RAW => bytes.extend_from_slice(planar),
            PSD_COMPRESSION_ZIP | PSD_COMPRESSION_ZIP_PREDICTION => {
                let mut payload = planar.to_vec();
                if compression == PSD_COMPRESSION_ZIP_PREDICTION {
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
            _ => panic!("craft helper only supports raw/zip/zip+prediction"),
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
        assert!(super::rgba8_is_absolutely_blank_with_cancel(&[], None, 3).unwrap());
        assert!(
            super::rgba8_is_absolutely_blank_with_cancel(&[0, 0, 0, 0, 0, 0, 0, 0], None, 3)
                .unwrap()
        );
        assert!(
            super::rgba8_is_absolutely_blank_with_cancel(&[0, 0, 0, 255, 0, 0, 0, 255], None, 3)
                .unwrap()
        );
        assert!(
            super::rgba8_is_absolutely_blank_with_cancel(&[10, 20, 30, 0, 40, 50, 60, 0], None, 3)
                .unwrap()
        );
        assert!(
            !super::rgba8_is_absolutely_blank_with_cancel(&[0, 0, 0, 255, 1, 0, 0, 255], None, 3)
                .unwrap()
        );
        assert!(
            !super::rgba8_is_absolutely_blank_with_cancel(&[255, 255, 255, 255], None, 3).unwrap()
        );
    }

    #[test]
    fn rgba8_absolute_blank_large_buffer_with_single_lit_pixel() {
        let mut pixels = vec![0u8; 4096 * 4];
        for px in pixels.chunks_exact_mut(4) {
            px[3] = 255;
        }
        assert!(super::rgba8_is_absolutely_blank_with_cancel(&pixels, None, 3).unwrap());
        let off = 1234 * 4;
        pixels[off] = 7;
        assert!(!super::rgba8_is_absolutely_blank_with_cancel(&pixels, None, 3).unwrap());
    }

    #[test]
    fn rgba8_absolute_blank_cmyk_ignores_rgb0() {
        // Opaque pure-black RGBA is blank for RGB mode, but not for CMYK (mode 4):
        // RGB-0 after conversion must not discard a non-transparent flat.
        let black = [0u8, 0, 0, 255, 0, 0, 0, 255];
        assert!(super::rgba8_is_absolutely_blank_with_cancel(&black, None, 3).unwrap());
        assert!(!super::rgba8_is_absolutely_blank_with_cancel(&black, None, 4).unwrap());
        // Fully transparent is blank in every mode.
        let clear = [0u8, 0, 0, 0, 10, 20, 30, 0];
        assert!(super::rgba8_is_absolutely_blank_with_cancel(&clear, None, 4).unwrap());
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

    #[test]
    fn unsupported_color_modes_are_rejected() {
        // All supported modes must pass.
        for mode in [0u16, 1, 2, 3, 4, 7, 8, 9] {
            assert!(
                ensure_supported_color_mode(mode).is_ok(),
                "mode={mode} should now be supported"
            );
        }
        // Modes 5 and 6 are unassigned; any other value is rejected.
        for mode in [5u16, 6] {
            let err = ensure_supported_color_mode(mode).unwrap_err();
            assert!(
                err.contains(&mode.to_string()) || err.contains("color"),
                "mode={mode} err={err}"
            );
            assert!(!channel_is_used(mode, 0, 3));
            assert!(!channel_is_used(mode, 1, 3));
            assert!(!channel_is_used(mode, 2, 3));
        }
        // Verify channel usage for supported modes.
        assert!(channel_is_used(3, 0, 3));
        assert!(channel_is_used(3, 2, 3));
        assert!(channel_is_used(0, 0, 1)); // Bitmap: ch0 used
        assert!(channel_is_used(0, 1, 2)); // Bitmap: ch1 (alpha) used when present
        assert!(channel_is_used(2, 0, 1)); // Indexed: ch0 used
        assert!(channel_is_used(7, 0, 3)); // Multichannel: ch0 used
        assert!(channel_is_used(7, 2, 3)); // Multichannel: ch2 used
        assert!(channel_is_used(8, 0, 1)); // Duotone: ch0 used
        assert!(channel_is_used(9, 0, 4)); // Lab: ch0 used
        assert!(channel_is_used(9, 2, 4)); // Lab: ch2 used
        assert!(channel_is_used(9, 3, 4)); // Lab: ch3 (alpha) used
    }

    #[test]
    fn zip_planar_inflate_budget_is_named_and_finite() {
        // Guardrail: budget must stay aligned with the 8 GiB composite decode cap.
        assert_eq!(MAX_ZIP_PLANAR_INFLATE_BYTES, 8 * 1024 * 1024 * 1024);
        assert!(MAX_ZIP_PLANAR_INFLATE_BYTES > 0);
    }
}
