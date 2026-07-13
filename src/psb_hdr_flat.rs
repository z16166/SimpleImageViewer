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

//! PSD/PSB HDR flattened composite decode.
//!
//! Reads Image Data at native 16/32-bit samples and produces an
//! [`HdrImageBuffer`] with display-linear rgba_f32. The SDR path in
//! `psb_reader` downconverts to u8 during read; this module keeps the
//! full precision and applies ICC-probed transfer decoding.
//!
//! 32-bit: Photoshop stores channels as big-endian IEEE 754 float (linear light).
//! 16-bit: channels are big-endian u16 [0,65535]; transfer is applied from ICC probe.
//! CMYK: downconverted to u8, converted via lcms2 ICC (same as SDR) with a
//! naive Adobe-invert fallback, then sRGB-to-linear.
//! All output is display-linear (transfer_function = Linear) in rgba_f32.

use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::hdr::types::{
    HdrColorProfile, HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrLuminanceMetadata,
    HdrPixelFormat, HdrReference, HdrTransferFunction,
};
use crate::psb_icc_hdr::{log_16bit_transfer_assumption, probe_icc_hdr};
use crate::psb_reader::{
    MAX_ZIP_PLANAR_INFLATE_BYTES, PSD_COLOR_MODE_CMYK, PSD_COLOR_MODE_GRAYSCALE,
    PSD_COMPRESSION_RAW, PSD_COMPRESSION_RLE, PSD_COMPRESSION_ZIP, PSD_COMPRESSION_ZIP_PREDICTION,
    bytes_per_sample, channel_is_used, check_decode_cancel, downconvert_samples_to_u8,
    ensure_supported_color_mode, extract_icc_profile_from_ir, max_rle_compressed_row_bytes,
    read_u16, read_u32, seek_forward_within, unpack_bits_into, validate_rle_row_counts,
};
use crate::psb_section_index::PsdSectionIndex;

/// Chunk size for SIMD HDR interleave so cancel can be polled between batches.
const HDR_INTERLEAVE_CHUNK_PIXELS: usize = 1 << 16;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Decode the flattened Image Data section into a display-linear [`HdrImageBuffer`].
///
/// Only meaningful for `depth` 16 or 32. Returns `Err` for 8-bit depth --
/// callers should use the SDR path (`read_composite_from_index`) for 8-bit.
///
/// `sdr_white_nits` is used to scale PQ/HLG absolute luminance to display-relative
/// linear light (e.g. 203 nits for a reference display).
pub fn read_composite_hdr_from_index(
    index: &PsdSectionIndex,
    bytes: &[u8],
    cancel: Option<&AtomicBool>,
    sdr_white_nits: f32,
) -> Result<HdrImageBuffer, crate::loader::DecodeError> {
    let depth = index.depth;
    if depth == 8 {
        return Err("HDR path requires 16 or 32-bit depth; use SDR path for 8-bit".into());
    }
    let bps = bytes_per_sample(depth)?;

    let file_size = bytes.len() as u64;
    let width = index.width;
    let height = index.height;
    let channels = index.channels;
    let color_mode = index.color_mode;
    let is_psb = index.is_psb;
    ensure_supported_color_mode(color_mode)?;

    let embedded_icc = extract_icc_profile_from_ir(bytes, index.ir_start, index.ir_end);

    log::debug!(
        "PSB HDR flat: {}x{} {} ch {}-bit mode={} psb={}",
        width,
        height,
        channels,
        depth,
        color_mode,
        is_psb
    );

    check_decode_cancel(cancel)?;

    let compression = index.image_data_compression(bytes)?;
    if compression > PSD_COMPRESSION_ZIP_PREDICTION {
        return Err(format!("Invalid PSD/PSB HDR compression: {compression}").into());
    }

    let mut r = std::io::Cursor::new(bytes);
    r.seek(SeekFrom::Start(index.image_data_pos + 2))
        .map_err(|e| format!("Seek error: {e}"))?;

    let pixel_count = hdr_checked_pixel_count(width, height)?;
    let raw_channel_bytes = pixel_count
        .checked_mul(bps)
        .ok_or_else(|| "PSD/PSB HDR channel byte count overflow".to_string())?;
    let row_raw_bytes = (width as usize)
        .checked_mul(bps)
        .ok_or_else(|| "PSD/PSB HDR row byte count overflow".to_string())?;

    let total_rows = (height as usize)
        .checked_mul(channels as usize)
        .ok_or_else(|| "PSD/PSB HDR row count overflow".to_string())?;
    let mut row_counts: Vec<usize> = Vec::new();
    if compression == PSD_COMPRESSION_RLE {
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
        validate_rle_row_counts(
            &row_counts,
            remaining,
            max_rle_compressed_row_bytes(row_raw_bytes)?,
        )?;
    }

    // Read planar channels, keeping raw native-depth bytes (no downconversion here).
    let mut planar_raw: Vec<Option<Vec<u8>>> = vec![None; channels as usize];

    if compression == PSD_COMPRESSION_ZIP || compression == PSD_COMPRESSION_ZIP_PREDICTION {
        let data_start = r
            .stream_position()
            .map_err(|e| format!("Stream position error: {e}"))? as usize;
        let compressed = bytes
            .get(data_start..)
            .ok_or_else(|| "PSD/PSB HDR ZIP data out of bounds".to_string())?;
        let expected = (channels as usize)
            .checked_mul(raw_channel_bytes)
            .ok_or_else(|| "PSD/PSB HDR ZIP planar size overflow".to_string())?;
        if (expected as u64) > MAX_ZIP_PLANAR_INFLATE_BYTES {
            return Err(format!(
                "PSD/PSB HDR ZIP planar {expected} bytes exceeds budget {MAX_ZIP_PLANAR_INFLATE_BYTES}"
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
                .ok_or_else(|| "PSD/PSB HDR ZIP channel slice out of bounds".to_string())?;
            planar_raw[ch_idx as usize] = Some(raw.to_vec());
        }
    } else {
        for ch_idx in 0..channels {
            check_decode_cancel(cancel)?;
            let is_used = channel_is_used(color_mode, ch_idx, channels);
            if is_used {
                let mut raw = vec![0u8; raw_channel_bytes];
                match compression {
                    PSD_COMPRESSION_RAW => {
                        r.read_exact(&mut raw)
                            .map_err(|e| format!("Read HDR raw ch {ch_idx}: {e}"))?;
                    }
                    PSD_COMPRESSION_RLE => {
                        let mut row_raw = Vec::with_capacity(row_raw_bytes);
                        let mut compressed_buf = Vec::new();
                        for row in 0..height as usize {
                            if row & 0x3F == 0 {
                                check_decode_cancel(cancel)?;
                            }
                            let idx = ch_idx as usize * height as usize + row;
                            let clen = *row_counts
                                .get(idx)
                                .ok_or_else(|| format!("Row count {idx} out of range"))?;
                            compressed_buf.resize(clen, 0);
                            r.read_exact(&mut compressed_buf)
                                .map_err(|e| format!("Read HDR RLE ch {ch_idx}: {e}"))?;
                            unpack_bits_into(&mut row_raw, &compressed_buf, row_raw_bytes)?;
                            let dst = row * row_raw_bytes;
                            raw[dst..dst + row_raw_bytes].copy_from_slice(&row_raw);
                        }
                    }
                    _ => {
                        return Err(format!("Unsupported HDR compression: {compression}").into());
                    }
                }
                planar_raw[ch_idx as usize] = Some(raw);
            } else {
                match compression {
                    PSD_COMPRESSION_RAW => {
                        seek_forward_within(
                            &mut r,
                            raw_channel_bytes as u64,
                            file_size,
                            "HDR raw channel data",
                        )?;
                    }
                    PSD_COMPRESSION_RLE => {
                        // Sum precomputed row counts and skip the whole unused
                        // channel in one seek (avoids height sequential seeks).
                        crate::psb_reader::seek_rle_channel_skip(
                            &mut r,
                            &row_counts,
                            ch_idx as usize,
                            height as usize,
                            file_size,
                            "HDR RLE unused channel",
                            cancel,
                        )?;
                    }
                    _ => {}
                }
            }
        }
    }

    // ICC probe: extract transfer hint for 16-bit content.
    // 32-bit Photoshop float is always linear; ignore ICC transfer for depth=32.
    let icc_probe = embedded_icc
        .as_deref()
        .map(probe_icc_hdr)
        .unwrap_or_default();
    log_16bit_transfer_assumption(&icc_probe, depth);
    let transfer = if depth == 32 {
        HdrTransferFunction::Linear
    } else {
        match icc_probe.transfer {
            HdrTransferFunction::Unknown => HdrTransferFunction::Linear,
            tf => tf,
        }
    };

    // Build rgba_f32 from planar native samples.
    let rgba_count = pixel_count
        .checked_mul(4)
        .ok_or_else(|| "PSD/PSB HDR rgba_f32 size overflow".to_string())?;
    let mut rgba_f32 = vec![0.0f32; rgba_count];

    let ctx = SampleDecodeCtx {
        bps,
        depth,
        transfer,
        sdr_white_nits,
    };
    match color_mode {
        PSD_COLOR_MODE_GRAYSCALE => {
            interleave_gray_hdr(
                &mut rgba_f32,
                &planar_raw,
                channels,
                &ctx,
                pixel_count,
                cancel,
            )?;
        }
        PSD_COLOR_MODE_CMYK if channels >= 4 => {
            interleave_cmyk_hdr(
                &mut rgba_f32,
                &planar_raw,
                channels,
                bps,
                pixel_count,
                embedded_icc.as_deref(),
                cancel,
            )?;
        }
        _ => {
            interleave_rgb_hdr(
                &mut rgba_f32,
                &planar_raw,
                channels,
                &ctx,
                pixel_count,
                cancel,
            )?;
        }
    }

    // Build color profile: prefer embedded ICC, fall back to LinearSrgb.
    let color_profile = if let Some(icc) = embedded_icc {
        HdrColorProfile::Icc(Arc::new(icc))
    } else {
        HdrColorProfile::LinearSrgb
    };
    let luminance = HdrLuminanceMetadata {
        mastering_max_nits: icc_probe.peak_nits,
        sdr_white_nits: Some(sdr_white_nits),
        ..Default::default()
    };
    let metadata = HdrImageMetadata {
        transfer_function: HdrTransferFunction::Linear,
        reference: HdrReference::DisplayReferred,
        color_profile,
        luminance,
        gain_map: None,
        raw_gpu_source: None,
    };
    // Prefer ICC primaries (Rec.2020 / Display P3 / sRGB) over a hardcoded
    // LinearSrgb tag so wide-gamut 16/32-bit flats render with the right matrix.
    let color_space = match metadata.color_space_hint() {
        HdrColorSpace::Unknown => HdrColorSpace::LinearSrgb,
        cs => cs,
    };
    crate::hdr::types::log_unrecognized_embedded_icc_after_decode(&metadata);

    Ok(HdrImageBuffer {
        width,
        height,
        format: HdrPixelFormat::Rgba32Float,
        color_space,
        metadata,
        rgba_f32: Arc::new(rgba_f32),
    })
}

/// True when `pixels` is semantically blank: empty, all alpha == 0, or all RGB == 0.
///
/// Uses epsilon 1e-8 for float comparisons. O(N) with early exit when both
/// a nonzero alpha and a nonzero RGB sample are found (not blank).
#[allow(dead_code)]
pub fn rgba_f32_is_absolutely_blank(pixels: &[f32]) -> bool {
    rgba_f32_is_absolutely_blank_with_cancel(pixels, None).unwrap()
}

/// Cancellation-aware variant of [`rgba_f32_is_absolutely_blank`].
pub fn rgba_f32_is_absolutely_blank_with_cancel(
    pixels: &[f32],
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<bool, crate::loader::DecodeError> {
    if pixels.is_empty() || !pixels.len().is_multiple_of(4) {
        return Ok(true);
    }
    const EPS: f32 = 1e-8;
    const CANCEL_POLL_PIXELS: usize = 64 * 1024;
    let mut any_rgb = false;
    let mut any_alpha = false;
    let mut i = 0;
    while i + 4 <= pixels.len() {
        if (i / 4).is_multiple_of(CANCEL_POLL_PIXELS) {
            crate::psb_reader::check_decode_cancel(cancel)?;
        }
        if pixels[i].abs() > EPS || pixels[i + 1].abs() > EPS || pixels[i + 2].abs() > EPS {
            any_rgb = true;
        }
        if pixels[i + 3].abs() > EPS {
            any_alpha = true;
        }
        if any_rgb && any_alpha {
            return Ok(false);
        }
        i += 4;
    }
    Ok(true)
}

// ---------------------------------------------------------------------------
// Interleave helpers
// ---------------------------------------------------------------------------

/// Bundled sample-decode parameters shared across interleave functions.
struct SampleDecodeCtx {
    bps: usize,
    depth: u16,
    transfer: HdrTransferFunction,
    sdr_white_nits: f32,
}

fn interleave_rgb_hdr(
    rgba_f32: &mut [f32],
    planar_raw: &[Option<Vec<u8>>],
    channels: u32,
    ctx: &SampleDecodeCtx,
    pixel_count: usize,
    cancel: Option<&AtomicBool>,
) -> Result<(), crate::loader::DecodeError> {
    let r_ch = planar_raw.first().and_then(|c| c.as_deref());
    let g_ch = planar_raw.get(1).and_then(|c| c.as_deref());
    let b_ch = planar_raw.get(2).and_then(|c| c.as_deref());
    let a_ch = if channels >= 4 {
        planar_raw.get(3).and_then(|c| c.as_deref())
    } else {
        None
    };

    let mut start = 0usize;
    while start < pixel_count {
        check_decode_cancel(cancel)?;
        let n = (pixel_count - start).min(HDR_INTERLEAVE_CHUNK_PIXELS);
        let dst = &mut rgba_f32[start * 4..(start + n) * 4];
        let r = plane_chunk(r_ch, start, n, ctx.bps);
        let g = plane_chunk(g_ch, start, n, ctx.bps);
        let b = plane_chunk(b_ch, start, n, ctx.bps);
        let a = plane_chunk(a_ch, start, n, ctx.bps);
        match ctx.bps {
            2 => {
                crate::psb_hdr_interleave_simd::interleave_planar_u16be_rgba_f32(r, g, b, a, dst, n)
            }
            4 => {
                crate::psb_hdr_interleave_simd::interleave_planar_f32be_rgba_f32(r, g, b, a, dst, n)
            }
            _ => {
                for i in 0..n {
                    let gi = start + i;
                    let rv = r_ch.map_or(0.0, |ch| native_sample_f32(ch, gi, ctx.bps));
                    let gv = g_ch.map_or(0.0, |ch| native_sample_f32(ch, gi, ctx.bps));
                    let bv = b_ch.map_or(0.0, |ch| native_sample_f32(ch, gi, ctx.bps));
                    let av = alpha_sample_f32(a_ch, gi, ctx.bps);
                    let base = i * 4;
                    dst[base] = rv;
                    dst[base + 1] = gv;
                    dst[base + 2] = bv;
                    dst[base + 3] = av;
                }
            }
        }
        apply_transfer_chunk(dst, n, ctx);
        start += n;
    }
    Ok(())
}

fn interleave_gray_hdr(
    rgba_f32: &mut [f32],
    planar_raw: &[Option<Vec<u8>>],
    channels: u32,
    ctx: &SampleDecodeCtx,
    pixel_count: usize,
    cancel: Option<&AtomicBool>,
) -> Result<(), crate::loader::DecodeError> {
    let gray_ch = planar_raw.first().and_then(|c| c.as_deref());
    let a_ch = if channels >= 2 {
        planar_raw.get(1).and_then(|c| c.as_deref())
    } else {
        None
    };

    let mut start = 0usize;
    while start < pixel_count {
        check_decode_cancel(cancel)?;
        let n = (pixel_count - start).min(HDR_INTERLEAVE_CHUNK_PIXELS);
        let dst = &mut rgba_f32[start * 4..(start + n) * 4];
        let gray = plane_chunk(gray_ch, start, n, ctx.bps);
        let a = plane_chunk(a_ch, start, n, ctx.bps);
        match ctx.bps {
            2 => crate::psb_hdr_interleave_simd::interleave_planar_u16be_gray_f32(gray, a, dst, n),
            4 => crate::psb_hdr_interleave_simd::interleave_planar_f32be_gray_f32(gray, a, dst, n),
            _ => {
                for i in 0..n {
                    let gi = start + i;
                    let v = gray_ch.map_or(0.0, |ch| native_sample_f32(ch, gi, ctx.bps));
                    let av = alpha_sample_f32(a_ch, gi, ctx.bps);
                    let base = i * 4;
                    dst[base] = v;
                    dst[base + 1] = v;
                    dst[base + 2] = v;
                    dst[base + 3] = av;
                }
            }
        }
        apply_transfer_chunk(dst, n, ctx);
        start += n;
    }
    Ok(())
}

/// CMYK HDR: downconvert planes to u8, convert via lcms2 ICC (same as SDR),
/// then sRGB8 -> scene-linear f32. Falls back to the naive Adobe invert path
/// when CMS is unavailable or rejects the profile.
fn interleave_cmyk_hdr(
    rgba_f32: &mut [f32],
    planar_raw: &[Option<Vec<u8>>],
    channels: u32,
    bps: usize,
    pixel_count: usize,
    embedded_icc: Option<&[u8]>,
    cancel: Option<&AtomicBool>,
) -> Result<(), crate::loader::DecodeError> {
    // Downconvert each CMYK plane to u8 display values (SIMD in psb_downconvert_simd).
    let c_u8 = downconvert_channel_to_u8(
        planar_raw.first().and_then(|c| c.as_deref()),
        bps,
        pixel_count,
    );
    let m_u8 = downconvert_channel_to_u8(
        planar_raw.get(1).and_then(|c| c.as_deref()),
        bps,
        pixel_count,
    );
    let y_u8 = downconvert_channel_to_u8(
        planar_raw.get(2).and_then(|c| c.as_deref()),
        bps,
        pixel_count,
    );
    let k_u8 = downconvert_channel_to_u8(
        planar_raw.get(3).and_then(|c| c.as_deref()),
        bps,
        pixel_count,
    );
    let a_raw = if channels >= 5 {
        planar_raw.get(4).and_then(|c| c.as_deref())
    } else {
        None
    };
    let a_u8 = downconvert_channel_to_u8(a_raw, bps, pixel_count);

    // Missing CMYK planes default to 255 (no ink) to match the prior scalar path.
    let c = c_u8.unwrap_or_else(|| vec![255u8; pixel_count]);
    let m = m_u8.unwrap_or_else(|| vec![255u8; pixel_count]);
    let y = y_u8.unwrap_or_else(|| vec![255u8; pixel_count]);
    let k = k_u8.unwrap_or_else(|| vec![255u8; pixel_count]);
    let icc = crate::psb_cmyk_cms::resolve_cmyk_icc(embedded_icc);

    let mut rgba8 = vec![0u8; HDR_INTERLEAVE_CHUNK_PIXELS * 4];
    let mut start = 0usize;
    while start < pixel_count {
        check_decode_cancel(cancel)?;
        let n = (pixel_count - start).min(HDR_INTERLEAVE_CHUNK_PIXELS);
        let end = start + n;
        let alpha = a_u8.as_ref().map(|ch| &ch[start..end]);
        let dst8 = &mut rgba8[..n * 4];
        let span = crate::psb_cmyk_cms::AdobeCmykSpan {
            c: &c[start..end],
            m: &m[start..end],
            y: &y[start..end],
            k: &k[start..end],
            alpha,
        };
        if !crate::psb_cmyk_cms::cmyk_span_adobe_to_rgba8(&span, icc, dst8) {
            // CMS unavailable / profile rejected: same naive path as before.
            crate::psb_cmyk_simd::cmyk_planes_to_rgba8(
                span.c, span.m, span.y, span.k, span.alpha, dst8,
            );
        }
        simple_image_viewer::simd_pixel_convert::srgb8_rgba_to_scene_linear_f32(
            dst8,
            &mut rgba_f32[start * 4..end * 4],
        );
        start = end;
    }
    Ok(())
}

/// Slice one planar channel chunk; returns `None` when the plane is missing or short.
fn plane_chunk(channel: Option<&[u8]>, start: usize, count: usize, bps: usize) -> Option<&[u8]> {
    let ch = channel?;
    let byte_start = start.checked_mul(bps)?;
    let byte_len = count.checked_mul(bps)?;
    let byte_end = byte_start.checked_add(byte_len)?;
    if byte_end > ch.len() {
        return None;
    }
    Some(&ch[byte_start..byte_end])
}

/// Apply transfer to interleaved RGB (alpha unchanged). No-op for 32-bit / Linear.
fn apply_transfer_chunk(rgba: &mut [f32], pixel_count: usize, ctx: &SampleDecodeCtx) {
    if ctx.depth == 32
        || matches!(
            ctx.transfer,
            HdrTransferFunction::Linear | HdrTransferFunction::Gamma | HdrTransferFunction::Unknown
        )
    {
        return;
    }
    for i in 0..pixel_count {
        let base = i * 4;
        let [lr, lg, lb] = apply_rgb_transfer([rgba[base], rgba[base + 1], rgba[base + 2]], ctx);
        rgba[base] = lr;
        rgba[base + 1] = lg;
        rgba[base + 2] = lb;
    }
}

// ---------------------------------------------------------------------------
// Sample helpers
// ---------------------------------------------------------------------------

/// Read one native-depth sample from a planar channel as f32.
///
/// 32-bit: IEEE 754 big-endian float (Photoshop linear light).
/// 16-bit: big-endian u16 / 65535.0 (normalized [0,1]).
/// Bounds use `off + bps <= len` so the last in-range sample is included.
#[inline]
fn native_sample_f32(channel: &[u8], sample_idx: usize, bps: usize) -> f32 {
    let off = sample_idx * bps;
    match bps {
        2 if off + 2 <= channel.len() => {
            u16::from_be_bytes([channel[off], channel[off + 1]]) as f32 / 65535.0
        }
        4 if off + 4 <= channel.len() => f32::from_be_bytes([
            channel[off],
            channel[off + 1],
            channel[off + 2],
            channel[off + 3],
        ]),
        _ => 0.0,
    }
}

/// Read the alpha sample, clamping to [0,1] (both 16 and 32-bit alpha treated as linear).
#[inline]
fn alpha_sample_f32(channel: Option<&[u8]>, sample_idx: usize, bps: usize) -> f32 {
    match channel {
        Some(ch) => native_sample_f32(ch, sample_idx, bps).clamp(0.0, 1.0),
        None => 1.0,
    }
}

/// Apply transfer function to RGB samples (16-bit only; 32-bit is already linear).
#[inline]
fn apply_rgb_transfer(rgb: [f32; 3], ctx: &SampleDecodeCtx) -> [f32; 3] {
    if ctx.depth == 32 {
        rgb
    } else {
        crate::hdr::decode::decode_transfer_to_display_linear(rgb, ctx.transfer, ctx.sdr_white_nits)
    }
}

/// Downconvert a planar channel to 8-bit display values, returning `None` if input is `None`.
fn downconvert_channel_to_u8(
    raw: Option<&[u8]>,
    bps: usize,
    pixel_count: usize,
) -> Option<Vec<u8>> {
    raw.map(|src| {
        let mut dst = vec![0u8; pixel_count];
        downconvert_samples_to_u8(&mut dst, src, bps);
        dst
    })
}

/// Overflow-safe pixel count from dimensions, enforcing the document pixel cap.
fn hdr_checked_pixel_count(width: u32, height: u32) -> Result<usize, String> {
    let pixels = (width as u64)
        .checked_mul(height as u64)
        .ok_or_else(|| "PSD/PSB HDR pixel count overflow".to_string())?;
    if pixels > crate::psb_reader::MAX_DOCUMENT_PIXELS {
        return Err(format!(
            "PSD/PSB HDR dimensions {width}x{height} exceed maximum {} pixels",
            crate::psb_reader::MAX_DOCUMENT_PIXELS
        ));
    }
    usize::try_from(pixels).map_err(|_| "PSD/PSB HDR pixel count overflow".into())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_sample_f32_32bit_round_trips_be_float() {
        let val: f32 = 1.5;
        let bytes = val.to_be_bytes();
        let result = native_sample_f32(&bytes, 0, 4);
        assert!((result - val).abs() < 1e-7, "got {result}, want {val}");
    }

    #[test]
    fn native_sample_f32_32bit_linear_above_one_preserved() {
        // Photoshop 32-bit can store > 1.0 (HDR headroom).
        let val: f32 = 4.2;
        let bytes = val.to_be_bytes();
        assert!((native_sample_f32(&bytes, 0, 4) - val).abs() < 1e-7);
    }

    #[test]
    fn native_sample_f32_16bit_max_normalizes_to_one() {
        let bytes = [0xFF_u8, 0xFF];
        let result = native_sample_f32(&bytes, 0, 2);
        assert!((result - 1.0).abs() < 1e-7, "got {result}");
    }

    #[test]
    fn native_sample_f32_includes_last_u16_sample() {
        // Two samples, exactly 4 bytes -- last index must not be dropped.
        let bytes = [0x00, 0x00, 0xFF, 0xFF];
        assert_eq!(native_sample_f32(&bytes, 0, 2), 0.0);
        assert!((native_sample_f32(&bytes, 1, 2) - 1.0).abs() < 1e-7);
    }

    #[test]
    fn native_sample_f32_16bit_zero_normalizes_to_zero() {
        let bytes = [0x00_u8, 0x00];
        assert_eq!(native_sample_f32(&bytes, 0, 2), 0.0);
    }

    #[test]
    fn pq_high_code_produces_above_one_display_linear() {
        // PQ code 0.75 maps to ~8000 nits; at 203 nits SDR white that is >> 1.0.
        use crate::hdr::decode::pq_nonlinear_to_display_linear;
        let linear = pq_nonlinear_to_display_linear(0.75, 203.0);
        assert!(linear > 1.0, "PQ 0.75 should exceed SDR 1.0, got {linear}");
    }

    #[test]
    fn apply_rgb_transfer_32bit_passthrough() {
        let rgb = [2.0, 0.5, 0.1];
        let ctx = SampleDecodeCtx {
            bps: 4,
            depth: 32,
            transfer: HdrTransferFunction::Pq,
            sdr_white_nits: 203.0,
        };
        let out = apply_rgb_transfer(rgb, &ctx);
        // 32-bit must pass through unchanged regardless of transfer flag.
        assert_eq!(out, rgb);
    }

    #[test]
    fn apply_rgb_transfer_16bit_linear_passthrough() {
        let rgb = [0.5, 0.25, 0.1];
        let ctx = SampleDecodeCtx {
            bps: 2,
            depth: 16,
            transfer: HdrTransferFunction::Linear,
            sdr_white_nits: 203.0,
        };
        let out = apply_rgb_transfer(rgb, &ctx);
        assert_eq!(out, rgb);
    }

    #[test]
    fn rgba_f32_is_absolutely_blank_empty() {
        assert!(rgba_f32_is_absolutely_blank(&[]));
    }

    #[test]
    fn rgba_f32_is_absolutely_blank_all_transparent() {
        // RGBA: (1.0,0.5,0.2, 0.0) -- nonzero RGB but alpha==0
        let pixels = vec![1.0f32, 0.5, 0.2, 0.0];
        assert!(rgba_f32_is_absolutely_blank(&pixels));
    }

    #[test]
    fn rgba_f32_is_absolutely_blank_all_black() {
        // RGBA: (0.0,0.0,0.0, 1.0) -- zero RGB but nonzero alpha
        let pixels = vec![0.0f32, 0.0, 0.0, 1.0];
        assert!(rgba_f32_is_absolutely_blank(&pixels));
    }

    #[test]
    fn rgba_f32_is_absolutely_blank_not_blank() {
        // Nonzero RGB and nonzero alpha -- NOT blank.
        let pixels = vec![0.5f32, 0.3, 0.1, 1.0];
        assert!(!rgba_f32_is_absolutely_blank(&pixels));
    }

    #[test]
    fn rgba_f32_is_absolutely_blank_epsilon_boundary() {
        // Values exactly at epsilon are NOT counted as nonzero.
        let eps = 1e-8;
        let pixels = vec![eps * 0.5, 0.0, 0.0, 1.0]; // RGB below epsilon
        assert!(rgba_f32_is_absolutely_blank(&pixels));

        let pixels2 = vec![eps * 2.0, 0.0, 0.0, 1.0]; // RGB above epsilon
        assert!(!rgba_f32_is_absolutely_blank(&pixels2));
    }

    #[test]
    fn rgba_f32_is_absolutely_blank_honors_cancel() {
        let cancel = std::sync::atomic::AtomicBool::new(true);
        let err = rgba_f32_is_absolutely_blank_with_cancel(&[0.0; 4], Some(&cancel))
            .expect_err("cancelled blank scan");
        assert!(err.is_cancelled());
    }
}
