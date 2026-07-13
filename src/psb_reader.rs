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
//! Checklist #12 approaching-split: near the ~2000-line limit. If growing,
//! prefer extracting IR walker or composite helpers next.
//!
//! Reference: Adobe Photoshop File Formats Specification (March 2013)

use memmap2::Mmap;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::AtomicBool;

// SIMD architecture-specific imports are handled within submodules

use simple_image_viewer::simd_swizzle;

/// Photoshop Image Data / channel compression: raw (uncompressed).
pub(crate) const PSD_COMPRESSION_RAW: u16 = 0;
/// Photoshop PackBits RLE compression.
pub(crate) const PSD_COMPRESSION_RLE: u16 = 1;
/// Photoshop ZIP (zlib) compression without prediction.
pub(crate) const PSD_COMPRESSION_ZIP: u16 = 2;
/// Photoshop ZIP with horizontal difference prediction.
pub(crate) const PSD_COMPRESSION_ZIP_PREDICTION: u16 = 3;

/// Photoshop color mode: Grayscale.
pub(crate) const PSD_COLOR_MODE_GRAYSCALE: u16 = 1;
/// Photoshop color mode: RGB.
pub(crate) const PSD_COLOR_MODE_RGB: u16 = 3;
/// Photoshop color mode: CMYK.
pub(crate) const PSD_COLOR_MODE_CMYK: u16 = 4;

/// Layer channel ID: transparency mask (alpha).
pub(crate) const PSD_CHANNEL_ID_ALPHA: i16 = -1;
/// Layer channel ID: user-supplied layer mask.
pub(crate) const PSD_CHANNEL_ID_USER_MASK: i16 = -2;
/// Layer channel ID: real user mask (vector + raster combined).
pub(crate) const PSD_CHANNEL_ID_REAL_USER_MASK: i16 = -3;
/// Inclusive upper bound for color-channel IDs (0=R/C/Gray, 1=G/M, 2=B/Y, 3=K).
pub(crate) const PSD_CHANNEL_ID_COLOR_MAX: i16 = 3;

/// Adobe Photoshop PSD/PSB maximum canvas dimension (pixels per side).
pub(crate) const PSD_MAX_DIMENSION: u32 = 300_000;
/// Hard cap on document canvas total pixels for P1 / P2 / HDR *full-canvas*
/// allocations ([`checked_pixel_count`], HDR flat decode, layer budgets).
///
/// `PSD_MAX_DIMENSION` alone still allows `300_000 x 300_000`. This matches the
/// per-layer pixel budget so a single canvas allocation cannot reserve tens of
/// GB before any pixel data is read.
///
/// Do **not** apply this in [`validate_psd_dimensions`]: Hubble-class PSB
/// documents exceed this cap but must still parse so `load_psd` can open them
/// via on-demand disk tiling instead of a full RGBA buffer.
pub(crate) const MAX_DOCUMENT_PIXELS: u64 = 1024 * 1024 * 1024;
/// Adobe Photoshop PSD/PSB maximum channel count.
const PSD_MAX_CHANNELS: u32 = 56;
/// Absolute cap on a single ZIP Image Data inflate (all planar channels).
///
/// Matches [`crate::psb_layer_composite::MAX_COMPOSITE_DECODED_BYTES`] so a
/// 32-bit multi-channel document cannot allocate unbounded zlib output when a
/// caller skips the RAM precheck that uses [`estimate_memory_from_bytes`].
pub(crate) const MAX_ZIP_PLANAR_INFLATE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
/// Bytes per RGBA pixel when assembling the composite image.
const RGBA_BYTES_PER_PIXEL: usize = 4;
/// Bytes per display-linear HDR RGBA f32 pixel.
const HDR_RGBA_F32_BYTES_PER_PIXEL: u64 = 16;
/// Photoshop Image Resource IDs for embedded JPEG thumbnails.
const IR_THUMBNAIL_PS4: u16 = 1033;
const IR_THUMBNAIL_PS5: u16 = 1036;
/// Photoshop IR 1033/1036 thumbnail resource header length (bytes before JPEG payload).
/// Layout: format(4) + width(4) + height(4) + widthbytes(4) + size(4) + compressed(4)
/// + bits/pixel(2) + planes(2) = 28.
const IR_JPEG_THUMBNAIL_HEADER_LEN: usize = 28;
/// Offset of "Size after compression" (u32 BE) in the IR 1033/1036 thumbnail header.
const IR_JPEG_THUMBNAIL_COMPRESSED_SIZE_OFFSET: usize = 20;
/// Photoshop Image Resource: ICC Profile Settings (raw ICC bytes).
const IR_ICC_PROFILE: u16 = 1039;
/// Pixel-index mask for cancel polling in RGBA8 full-buffer scans (~every 256 KiB).
const RGBA8_CANCEL_POLL_MASK: usize = 0x3_FFFF;
/// Row-count index mask for cancel polling while reading RLE byte counts (~every 1024 rows).
const RLE_ROW_COUNT_CANCEL_POLL_INTERVAL: usize = 0x3FF;
/// Row-index mask for cancel polling during RLE per-row decode / skip / interleave (~every 64 rows).
const RLE_ROW_DECODE_CANCEL_POLL_INTERVAL: usize = 0x3F;

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
            // `row * width` is safe: `checked_pixel_count` already proved width*height
            // fits in usize, and row < height. Reuse start/end for the RGBA byte range.
            let start = row * width as usize;
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

#[inline]
pub(crate) fn check_decode_cancel(
    cancel: Option<&AtomicBool>,
) -> Result<(), crate::loader::DecodeError> {
    crate::loader::check_decode_cancel(cancel)
}

/// Absolute blank barrier for P1 flattened composites (RGBA8).
///
/// Returns true when the buffer is semantically empty:
/// - every alpha byte is 0 (fully transparent), or
/// - (Gray / RGB only) every RGB triple is (0,0,0) (absolute pure black).
///
/// For other color modes (CMYK, Lab, Indexed, ...), only the all-alpha-0
/// rule applies. Those modes can yield RGB-0 after conversion (or incomplete
/// channel mapping) for non-empty content, so treating RGB-0 as blank would
/// false-positive and skip a valid P1 flat.
///
/// Structural decode success alone is not enough; this is an O(N) SIMD scan
/// with early exit once a nonzero sample that disproves blank is found.
/// Polls `cancel` on large buffers when provided.
pub fn rgba8_is_absolutely_blank_with_cancel(
    pixels: &[u8],
    cancel: Option<&AtomicBool>,
    color_mode: u16,
) -> Result<bool, crate::loader::DecodeError> {
    if pixels.len() < 4 {
        return Ok(true);
    }
    let n = pixels.len() - (pixels.len() % 4);
    if n == 0 {
        return Ok(true);
    }
    let pixels = &pixels[..n];
    let use_rgb0 = color_mode_uses_rgb0_blank(color_mode);

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            return unsafe { rgba8_absolutely_blank_avx2(pixels, cancel, use_rgb0) };
        }
        if is_x86_feature_detected!("sse2") {
            return unsafe { rgba8_absolutely_blank_sse2(pixels, cancel, use_rgb0) };
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        return unsafe { rgba8_absolutely_blank_neon(pixels, cancel, use_rgb0) };
    }

    #[cfg(not(target_arch = "aarch64"))]
    {
        rgba8_absolutely_blank_scalar(pixels, cancel, use_rgb0)
    }
}

/// Gray and RGB: all-RGB-0 is a reliable empty-flat signal.
#[inline]
pub(crate) fn color_mode_uses_rgb0_blank(color_mode: u16) -> bool {
    matches!(color_mode, PSD_COLOR_MODE_GRAYSCALE | PSD_COLOR_MODE_RGB)
}

/// Scan RGBA8; returns `(any_nonzero_rgb, any_nonzero_alpha)`.
fn rgba8_any_rgb_alpha_scalar(
    pixels: &[u8],
    cancel: Option<&AtomicBool>,
    mut any_rgb: bool,
    mut any_a: bool,
    use_rgb0: bool,
) -> Result<(bool, bool), crate::loader::DecodeError> {
    let mut i = 0usize;
    while i + 4 <= pixels.len() {
        if i & RGBA8_CANCEL_POLL_MASK == 0 {
            check_decode_cancel(cancel)?;
        }
        if use_rgb0 && (pixels[i] | pixels[i + 1] | pixels[i + 2]) != 0 {
            any_rgb = true;
        }
        if pixels[i + 3] != 0 {
            any_a = true;
        }
        if any_a && (!use_rgb0 || any_rgb) {
            return Ok((any_rgb, true));
        }
        i += 4;
    }
    Ok((any_rgb, any_a))
}

fn rgba8_absolutely_blank_scalar(
    pixels: &[u8],
    cancel: Option<&AtomicBool>,
    use_rgb0: bool,
) -> Result<bool, crate::loader::DecodeError> {
    let (any_rgb, any_a) = rgba8_any_rgb_alpha_scalar(pixels, cancel, false, false, use_rgb0)?;
    Ok(if use_rgb0 { !any_rgb || !any_a } else { !any_a })
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn rgba8_absolutely_blank_sse2(
    pixels: &[u8],
    cancel: Option<&AtomicBool>,
    use_rgb0: bool,
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
        if use_rgb0 {
            let rgb = _mm_and_si128(v, rgb_mask);
            if _mm_movemask_epi8(_mm_cmpeq_epi8(rgb, zero)) != 0xFFFF {
                any_rgb = true;
            }
        }
        let alpha = _mm_and_si128(v, a_mask);
        if _mm_movemask_epi8(_mm_cmpeq_epi8(alpha, zero)) != 0xFFFF {
            any_a = true;
        }
        if any_a && (!use_rgb0 || any_rgb) {
            return Ok(false);
        }
        i += 16;
    }
    let (any_rgb, any_a) =
        rgba8_any_rgb_alpha_scalar(&pixels[i..], cancel, any_rgb, any_a, use_rgb0)?;
    Ok(if use_rgb0 { !any_rgb || !any_a } else { !any_a })
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn rgba8_absolutely_blank_avx2(
    pixels: &[u8],
    cancel: Option<&AtomicBool>,
    use_rgb0: bool,
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
        if use_rgb0 {
            let rgb = _mm256_and_si256(v, rgb_mask);
            if _mm256_movemask_epi8(_mm256_cmpeq_epi8(rgb, zero)) != -1 {
                any_rgb = true;
            }
        }
        let alpha = _mm256_and_si256(v, a_mask);
        if _mm256_movemask_epi8(_mm256_cmpeq_epi8(alpha, zero)) != -1 {
            any_a = true;
        }
        if any_a && (!use_rgb0 || any_rgb) {
            return Ok(false);
        }
        i += 32;
    }
    let (any_rgb, any_a) =
        rgba8_any_rgb_alpha_scalar(&pixels[i..], cancel, any_rgb, any_a, use_rgb0)?;
    Ok(if use_rgb0 { !any_rgb || !any_a } else { !any_a })
}

#[cfg(target_arch = "aarch64")]
unsafe fn rgba8_absolutely_blank_neon(
    pixels: &[u8],
    cancel: Option<&AtomicBool>,
    use_rgb0: bool,
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
        if use_rgb0 {
            let rgb = vandq_u32(vu, rgb_mask);
            if vmaxvq_u32(rgb) != 0 {
                any_rgb = true;
            }
        }
        let alpha = vandq_u32(vu, a_mask);
        if vmaxvq_u32(alpha) != 0 {
            any_a = true;
        }
        if any_a && (!use_rgb0 || any_rgb) {
            return Ok(false);
        }
        i += 16;
    }
    let (any_rgb, any_a) =
        rgba8_any_rgb_alpha_scalar(&pixels[i..], cancel, any_rgb, any_a, use_rgb0)?;
    Ok(if use_rgb0 { !any_rgb || !any_a } else { !any_a })
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
///
/// SDR compares u8 lanes exactly. HDR's matching barrier
/// (`psb_hdr_main::rgba_f32_is_zero_information_with_cancel`) uses a small
/// f32 EPS for the same transparent-or-solid-RGB rule, because float
/// composites accumulate blend/ICC noise that exact equality would mis-reject.
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

#[allow(clippy::needless_lifetimes, dead_code)]
pub(crate) fn find_image_resource<'a>(
    bytes: &'a [u8],
    ir_start: u64,
    ir_end: u64,
    rid: u16,
) -> Option<&'a [u8]> {
    let base = bytes.as_ptr() as usize;
    let (start, len) = for_each_image_resource(bytes, ir_start, ir_end, |resource_id, data| {
        if resource_id != rid {
            return None;
        }
        let start = (data.as_ptr() as usize).checked_sub(base)?;
        Some((start, data.len()))
    })?;
    bytes.get(start..start.checked_add(len)?)
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
    seek_forward_within(&mut r, cm_len, bytes.len() as u64, "color mode data").ok()?;
    let ir_len = read_u32(&mut r).ok()? as u64;
    let ir_start = r.stream_position().ok()?;
    let ir_end = ir_start.saturating_add(ir_len).min(bytes.len() as u64);
    extract_icc_profile_from_ir(bytes, ir_start, ir_end)
}

/// Try to extract Photoshop Image Resource 1033/1036 JPEG thumbnail as RGBA8.
///
/// Prefers [`PsdSectionIndex::parse`] IR bounds when the structural walk
/// succeeds. On structural failure (unsupported color mode, truncated
/// layer/mask, etc.) falls back to an IR-only locate so P3 recovery still
/// works when Image Resources remain readable.
pub fn try_extract_photoshop_thumbnail(bytes: &[u8]) -> Option<PsbComposite> {
    if let Ok(index) = crate::psb_section_index::PsdSectionIndex::parse(bytes) {
        return extract_photoshop_thumbnail_from_ir(bytes, index.ir_start, index.ir_end);
    }
    let (ir_start, ir_end) =
        crate::psb_section_index::PsdSectionIndex::locate_image_resources(bytes).ok()?;
    extract_photoshop_thumbnail_from_ir(bytes, ir_start, ir_end)
}

/// Parse Photoshop Image Resource 1033/1036 JPEG thumbnail into RGBA8.
pub(crate) fn extract_photoshop_thumbnail_from_ir(
    bytes: &[u8],
    ir_start: u64,
    ir_end: u64,
) -> Option<PsbComposite> {
    for_each_image_resource(bytes, ir_start, ir_end, |rid, data| {
        if (rid == IR_THUMBNAIL_PS4 || rid == IR_THUMBNAIL_PS5)
            && data.len() >= IR_JPEG_THUMBNAIL_HEADER_LEN
        {
            decode_photoshop_thumbnail_resource(data)
        } else {
            None
        }
    })
}

/// Walk Photoshop Image Resources (8BIM), invoking `on_resource` for each.
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
        // 8B64 with a u64 length belongs to Additional Layer Information
        // (`tagged_block_uses_u64_len`), not Adobe Image Resource Blocks.
        if sig == b"8B64" {
            break;
        }
        if sig != b"8BIM" {
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
        pos = pos.checked_add(name_len)?;
        if (name_len + 1) % 2 == 1 {
            pos = pos.checked_add(1)?;
        }
        if pos + 4 > end {
            break;
        }
        let size = u32::from_be_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
            as usize;
        pos += 4;
        let data_end = pos.checked_add(size)?;
        if data_end > end {
            // Truncated IR: stop walking rather than reading past the section.
            // Remaining resources are unavailable; callers that needed a specific
            // rid simply observe None (ICC / thumbnail missing).
            log::warn!(
                "PSD/PSB image resource {rid} size {size} exceeds remaining IR bytes (pos={pos}, end={end}); stopping walk"
            );
            break;
        }
        if let Some(found) = on_resource(rid, &bytes[pos..data_end]) {
            return Some(found);
        }
        pos = data_end;
        if size % 2 == 1 {
            pos = pos.checked_add(1)?;
        }
    }
    None
}

fn decode_photoshop_thumbnail_resource(data: &[u8]) -> Option<PsbComposite> {
    if data.len() < IR_JPEG_THUMBNAIL_HEADER_LEN {
        return None;
    }
    let format = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
    // 1 = JPEG RGB in current Photoshop thumbnail resources.
    if format != 1 {
        return None;
    }
    let width = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
    let height = u32::from_be_bytes([data[8], data[9], data[10], data[11]]);
    let compressed_off = IR_JPEG_THUMBNAIL_COMPRESSED_SIZE_OFFSET;
    let compressed = u32::from_be_bytes([
        data[compressed_off],
        data[compressed_off + 1],
        data[compressed_off + 2],
        data[compressed_off + 3],
    ]) as usize;
    if width == 0 || height == 0 || compressed == 0 {
        return None;
    }
    let jpeg_start = IR_JPEG_THUMBNAIL_HEADER_LEN;
    // Declared size past the resource payload is truncated/malformed -- do not
    // feed a partial JPEG into the decoder.
    let jpeg_end = jpeg_start.checked_add(compressed)?;
    if jpeg_end > data.len() {
        return None;
    }
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

/// Cap PackBits `n == -128` no-ops per row. Spec allows unlimited no-ops, but each
/// consumes one input byte without advancing output -- unbounded runs are a DoS vector.
pub(crate) const PACKBITS_MAX_NOOPS_PER_ROW: usize = 4096;

pub(crate) const PACKBITS_TOO_MANY_NOOPS: &str = "PSD/PSB PackBits RLE exceeds no-op limit";

/// PackBits RLE decompression (Macintosh PackBits variant) into an existing buffer.
///
/// Fail-closed: truncated literals/runs or early EOF before `expected_len` return
/// [`Err`]. Exceeding [`PACKBITS_MAX_NOOPS_PER_ROW`] also returns [`Err`].
/// On error, `result` is zero-filled to `expected_len`. Trailing bytes after a
/// successful fill are ignored (Photoshop row payloads may be padded).
pub(crate) fn unpack_bits_into(
    result: &mut Vec<u8>,
    data: &[u8],
    expected_len: usize,
) -> Result<(), String> {
    result.clear();
    if result.capacity() < expected_len {
        result.reserve(expected_len - result.capacity());
    }

    let fail = |result: &mut Vec<u8>, msg: &str| -> Result<(), String> {
        result.clear();
        result.resize(expected_len, 0);
        Err(msg.to_string())
    };

    let mut i = 0;
    let mut noop_count = 0usize;
    while i < data.len() && result.len() < expected_len {
        let n = data[i] as i8;
        i += 1;
        if n >= 0 {
            // Copy next (n+1) bytes literally
            let count = n as usize + 1;
            if i + count > data.len() {
                return fail(result, "PSD/PSB PackBits truncated literal run");
            }
            let remaining = expected_len - result.len();
            let take = count.min(remaining);
            result.extend_from_slice(&data[i..i + take]);
            i += count;
        } else if n > -128 {
            // Repeat next byte (1-n) times
            let count = (1 - n as i16) as usize;
            if i >= data.len() {
                return fail(result, "PSD/PSB PackBits repeat run missing value byte");
            }
            let val = data[i];
            i += 1;
            let remaining = expected_len - result.len();
            let actual_count = count.min(remaining);
            if actual_count > 0 {
                let start = result.len();
                result.resize(start + actual_count, 0);
                crate::psb_packbits_simd::fill_bytes(&mut result[start..], val);
            }
        } else {
            // n == -128: PackBits no-op (consumes the control byte only).
            noop_count += 1;
            if noop_count > PACKBITS_MAX_NOOPS_PER_ROW {
                return fail(result, PACKBITS_TOO_MANY_NOOPS);
            }
        }
    }
    if result.len() < expected_len {
        return fail(result, "PSD/PSB PackBits output shorter than expected");
    }
    Ok(())
}

// -- Helpers ---------------------------------------------------------

pub(crate) fn bytes_per_sample(depth: u16) -> Result<usize, String> {
    match depth {
        8 => Ok(1),
        16 => Ok(2),
        32 => Ok(4),
        _ => Err(format!(
            "Unsupported PSD/PSB bit depth {depth} (supported: 8, 16, 32)"
        )),
    }
}

/// Raw (0) and PackBits RLE (1) expose per-row extents, so disk-tiled open can
/// seek/decode one row at a time. ZIP / ZIP+prediction (2|3) store Image Data
/// as a single zlib stream of all planar channels -- there is no independent
/// row offset table, so true row-at-a-time tiling would require a full inflate
/// first (defeating the disk-tiled memory goal). Those modes must use the
/// static full-canvas decode path (or the budgeted HDR ZIP flat helper).
pub(crate) fn tiled_compression_supported(compression: u16) -> Result<(), String> {
    match compression {
        PSD_COMPRESSION_RAW | PSD_COMPRESSION_RLE => Ok(()),
        PSD_COMPRESSION_ZIP | PSD_COMPRESSION_ZIP_PREDICTION => {
            Err("PSD/PSB ZIP Image Data cannot be opened as disk-tiled".into())
        }
        other => Err(format!("Unsupported compression: {other}")),
    }
}

#[inline]
pub(crate) fn channel_is_used(color_mode: u16, ch_idx: u32, channels: u32) -> bool {
    match color_mode {
        PSD_COLOR_MODE_GRAYSCALE => ch_idx <= 1, // Gray, Alpha
        PSD_COLOR_MODE_RGB => ch_idx <= 3,       // R, G, B, Alpha
        PSD_COLOR_MODE_CMYK => ch_idx < 4 || (channels >= 5 && ch_idx == 4), // C,M,Y,K[,A]
        // Unsupported modes are rejected by [`ensure_supported_color_mode`]
        // before decode; never silently treat them as RGB.
        _ => false,
    }
}

/// Reject Bitmap / Indexed / Lab / Duotone / Multichannel etc. (checklist #15).
///
/// Only Gray / RGB / CMYK have color conversion paths.
pub(crate) fn ensure_supported_color_mode(color_mode: u16) -> Result<(), String> {
    match color_mode {
        PSD_COLOR_MODE_GRAYSCALE | PSD_COLOR_MODE_RGB | PSD_COLOR_MODE_CMYK => Ok(()),
        _ => Err(rust_i18n::t!("error.psd_unsupported_color_mode", mode = color_mode).to_string()),
    }
}

/// Convert planar samples (8/16/32-bit BE) into 8-bit display samples.
/// `dst.len()` is the sample count; `src` must hold `dst.len() * bps` bytes (or be truncated).
///
/// 16-bit uses the high byte of each BE u16 (`v >> 8`) with no gamma remapping --
/// matching Photoshop's composite storage, not a display-referred tone curve.
/// 8-bit and 16-bit sources therefore stay bit-faithful to the file even when
/// mixed depths appear across layers.
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
            let c = planar
                .get(0)
                .and_then(|ch| ch.as_ref())
                .and_then(|d| d.get(start..end));
            let m = planar
                .get(1)
                .and_then(|ch| ch.as_ref())
                .and_then(|d| d.get(start..end));
            let y = planar
                .get(2)
                .and_then(|ch| ch.as_ref())
                .and_then(|d| d.get(start..end));
            let k = planar
                .get(3)
                .and_then(|ch| ch.as_ref())
                .and_then(|d| d.get(start..end));
            let a = if channels >= 5 {
                planar
                    .get(4)
                    .and_then(|ch| ch.as_ref())
                    .and_then(|d| d.get(start..end))
            } else {
                None
            };
            if let (Some(c), Some(m), Some(y), Some(k)) = (c, m, y, k) {
                crate::psb_cmyk_simd::cmyk_planes_to_rgba8(c, m, y, k, a, dst_row);
            }
        }
        1 => {
            if let Some(g_row) = planar
                .first()
                .and_then(|ch| ch.as_ref())
                .and_then(|d| d.get(start..end))
            {
                if channels >= 2
                    && let Some(a_row) = planar
                        .get(1)
                        .and_then(|ch| ch.as_ref())
                        .and_then(|d| d.get(start..end))
                {
                    simd_swizzle::interleave_rgba(g_row, g_row, g_row, a_row, dst_row);
                } else {
                    simd_swizzle::interleave_rgb_with_alpha(g_row, g_row, g_row, 255, dst_row);
                }
            }
        }
        3 => {
            // RGB
            let r = planar
                .first()
                .and_then(|ch| ch.as_ref())
                .and_then(|d| d.get(start..end));
            let g = planar
                .get(1)
                .and_then(|ch| ch.as_ref())
                .and_then(|d| d.get(start..end));
            let b = planar
                .get(2)
                .and_then(|ch| ch.as_ref())
                .and_then(|d| d.get(start..end));
            if let (Some(r), Some(g), Some(b)) = (r, g, b) {
                if channels >= 4
                    && let Some(a_row) = planar
                        .get(3)
                        .and_then(|ch| ch.as_ref())
                        .and_then(|d| d.get(start..end))
                {
                    simd_swizzle::interleave_rgba(r, g, b, a_row, dst_row);
                } else {
                    simd_swizzle::interleave_rgb_with_alpha(r, g, b, 255, dst_row);
                }
            }
        }
        _ => {
            // Unsupported modes are rejected before decode; leave the row blank.
        }
    }
}

pub(crate) fn validate_psd_dimensions(
    width: u32,
    height: u32,
    channels: u32,
) -> Result<(), String> {
    if width == 0 || height == 0 {
        return Err("PSD/PSB dimensions must be non-zero".into());
    }
    if width > PSD_MAX_DIMENSION || height > PSD_MAX_DIMENSION {
        return Err(format!(
            "PSD/PSB dimensions {width}x{height} exceed maximum {PSD_MAX_DIMENSION}"
        ));
    }
    // Structural only: per-side Photoshop limit + channel count. Total-pixel
    // budget ([`MAX_DOCUMENT_PIXELS`]) applies to full-canvas decode paths, not
    // header/section parsing (disk-tiled Hubble-class PSB exceeds that budget).
    (width as u64)
        .checked_mul(height as u64)
        .ok_or_else(|| "PSD/PSB pixel count overflow".to_string())?;
    if channels == 0 || channels > PSD_MAX_CHANNELS {
        return Err(format!(
            "PSD/PSB channel count {channels} is out of range (1..={PSD_MAX_CHANNELS})"
        ));
    }
    Ok(())
}

/// Returns `width * height` as `usize`, or an error on overflow / document cap.
///
/// On success, any `row * width` with `row < height` also fits in `usize`
/// (including on 32-bit targets). Callers may rely on that instead of
/// repeating `checked_mul` in row loops.
fn checked_pixel_count(width: u32, height: u32) -> Result<usize, String> {
    let pixels = (width as u64)
        .checked_mul(height as u64)
        .ok_or_else(|| "PSD/PSB pixel count overflow".to_string())?;
    if pixels > MAX_DOCUMENT_PIXELS {
        return Err(format!(
            "PSD/PSB dimensions {width}x{height} exceed maximum {MAX_DOCUMENT_PIXELS} pixels"
        ));
    }
    usize::try_from(pixels).map_err(|_| "PSD/PSB pixel count overflow".into())
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

/// Ensure the next `len` bytes from the current position stay within `file_size`
/// before a subsequent `read_exact`, so OOB errors match [`seek_forward_within`].
pub(crate) fn ensure_readable_within(
    r: &mut impl Seek,
    len: u64,
    file_size: u64,
    label: &str,
) -> Result<(), String> {
    let pos = r
        .stream_position()
        .map_err(|e| format!("Stream position error: {e}"))?;
    let _ = checked_section_end(pos, len, file_size, label)?;
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
    // Bounds come from checked_section_end (always on, not debug_assert).
    ensure_readable_within(r, len, file_size, label)?;
    seek_forward(r, len)
}

/// Skip one unused RLE channel by summing its precomputed row counts and
/// performing a single bounded seek (instead of `height` per-row seeks).
pub(crate) fn seek_rle_channel_skip(
    r: &mut impl Seek,
    row_counts: &[usize],
    channel_idx: usize,
    height: usize,
    file_size: u64,
    label: &str,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<(), crate::loader::DecodeError> {
    let start = channel_idx
        .checked_mul(height)
        .ok_or_else(|| format!("PSD/PSB {label}: channel row index overflow"))?;
    let end = start
        .checked_add(height)
        .ok_or_else(|| format!("PSD/PSB {label}: channel row range overflow"))?;
    let counts = row_counts
        .get(start..end)
        .ok_or_else(|| format!("PSD/PSB {label}: row counts out of range ({start}..{end})"))?;

    let mut total = 0u64;
    for (i, &len) in counts.iter().enumerate() {
        if i & RLE_ROW_DECODE_CANCEL_POLL_INTERVAL == 0 {
            check_decode_cancel(cancel)?;
        }
        total = total
            .checked_add(len as u64)
            .ok_or_else(|| format!("PSD/PSB {label}: compressed length overflow"))?;
    }
    seek_forward_within(r, total, file_size, label)?;
    Ok(())
}

pub(crate) fn checked_section_end(
    start: u64,
    len: u64,
    file_size: u64,
    label: &str,
) -> Result<u64, String> {
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

pub(crate) fn validate_rle_total_bytes(row_counts: &[usize], remaining: u64) -> Result<(), String> {
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

/// PackBits can expand slightly above the raw row size; cap at 2x as a hard DoS bound.
pub(crate) fn max_rle_compressed_row_bytes(row_raw_bytes: usize) -> Result<usize, String> {
    row_raw_bytes
        .checked_mul(2)
        .ok_or_else(|| "PSD/PSB RLE row size limit overflow".to_string())
}

/// Validate each RLE row count against a per-row cap, then the total vs remaining bytes.
pub(crate) fn validate_rle_row_counts(
    row_counts: &[usize],
    remaining: u64,
    max_compressed_row_bytes: usize,
) -> Result<(), String> {
    for (i, &len) in row_counts.iter().enumerate() {
        if len > max_compressed_row_bytes {
            return Err(format!(
                "PSD/PSB RLE row {i} compressed length ({len}) exceeds limit ({max_compressed_row_bytes})"
            ));
        }
    }
    validate_rle_total_bytes(row_counts, remaining)
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

/// PSD uses u16 RLE row byte counts; PSB uses u32 (Photoshop file format).
#[inline]
pub(crate) fn read_rle_row_count(r: &mut impl Read, is_psb: bool) -> Result<usize, String> {
    if is_psb {
        Ok(read_u32(r)? as usize)
    } else {
        Ok(read_u16(r)? as usize)
    }
}

pub(crate) fn read_u64(r: &mut impl Read) -> Result<u64, String> {
    let mut buf = [0u8; 8];
    r.read_exact(&mut buf)
        .map_err(|e| format!("Read u64: {e}"))?;
    Ok(u64::from_be_bytes(buf))
}

/// Estimate peak resident decode buffers from header bytes.
///
/// Returns `(width, height, channels, estimated_bytes)`.
///
/// - 8-bit SDR: RGBA8 canvas + native-depth planar working set.
/// - 16/32-bit HDR flat: rgba_f32 canvas plus up to two planar copies (ZIP
///   inflate buffer coexists with per-channel `to_vec` copies before the
///   inflate buffer is dropped). Layer composite / clip scratch are not
///   known from the header alone; the RAM precheck must not under-count the
///   flat HDR peak.
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
    let planar = pixels
        .checked_mul(channels as u64)
        .and_then(|n| n.checked_mul(bps))
        .ok_or_else(|| "PSD/PSB memory estimate overflow".to_string())?;
    let estimated = if depth >= 16 {
        let rgba_f32 = pixels
            .checked_mul(HDR_RGBA_F32_BYTES_PER_PIXEL)
            .ok_or_else(|| "PSD/PSB memory estimate overflow".to_string())?;
        // ZIP path may keep inflate output and per-channel copies together.
        let planar_peak = planar
            .checked_mul(2)
            .ok_or_else(|| "PSD/PSB memory estimate overflow".to_string())?;
        planar_peak
            .checked_add(rgba_f32)
            .ok_or_else(|| "PSD/PSB memory estimate overflow".to_string())?
    } else {
        let rgba = pixels
            .checked_mul(RGBA_BYTES_PER_PIXEL as u64)
            .ok_or_else(|| "PSD/PSB memory estimate overflow".to_string())?;
        rgba.checked_add(planar)
            .ok_or_else(|| "PSD/PSB memory estimate overflow".to_string())?
    };
    Ok((width, height, channels, estimated))
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
        for mode in [0u16, 2, 7, 8, 9] {
            let err = ensure_supported_color_mode(mode).unwrap_err();
            assert!(
                err.contains(&mode.to_string()) || err.contains("color"),
                "mode={mode} err={err}"
            );
            assert!(!channel_is_used(mode, 0, 3));
            assert!(!channel_is_used(mode, 1, 3));
            assert!(!channel_is_used(mode, 2, 3));
        }
        assert!(ensure_supported_color_mode(1).is_ok());
        assert!(ensure_supported_color_mode(3).is_ok());
        assert!(ensure_supported_color_mode(4).is_ok());
        assert!(channel_is_used(3, 0, 3));
        assert!(channel_is_used(3, 2, 3));
    }

    #[test]
    fn zip_planar_inflate_budget_is_named_and_finite() {
        // Guardrail: budget must stay aligned with the 8 GiB composite decode cap.
        assert_eq!(MAX_ZIP_PLANAR_INFLATE_BYTES, 8 * 1024 * 1024 * 1024);
        assert!(MAX_ZIP_PLANAR_INFLATE_BYTES > 0);
    }
}
