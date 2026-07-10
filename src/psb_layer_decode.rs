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

//! Per-layer channel decode for the PSD/PSB layer compositor.
//!
//! Split out of `psb_layer_composite` (see `docs/coding-rules.md` #12): decodes
//! a single layer's channels (depth 8) into a straight-alpha RGBA8 rect,
//! including mask-channel handling and the reference blend-onto helpers used
//! by unit tests. Full-stack composite orchestration stays in
//! `psb_layer_composite`.

use crate::psb_layer_composite::{
    CompositeTiming, LayerInfo, LayerMaskInfo, LayerRecord, accumulate_decoded_layer_bytes,
    checked_layer_pixel_count, dimensions_within_limit, layer_will_decode,
};

// -- Layer channel decode ---------------------------------------------

/// How often (in rows) to poll `cancel` inside a single channel's RLE decode.
const RLE_ROW_CANCEL_POLL_INTERVAL: usize = 64;

/// Decode one channel's image data (compression header + rows) into 8-bit samples.
/// `data` must be exactly the channel's declared byte range (depth 8 only, v1).
pub(crate) fn decode_channel_image(
    data: &[u8],
    width: u32,
    height: u32,
    is_psb: bool,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<Vec<u8>, crate::loader::DecodeError> {
    let mut r = std::io::Cursor::new(data);
    let compression = crate::psb_reader::read_u16(&mut r)?;
    let pixel_count = checked_layer_pixel_count(width, height)
        .ok_or_else(|| format!("PSD/PSB layer channel size {width}x{height} exceeds limit"))?;

    match compression {
        0 => {
            // Avoid zero-filling the copied prefix: grow from the raw slice.
            let avail = data.len().saturating_sub(2);
            let copy = avail.min(pixel_count);
            let mut out = data[2..2 + copy].to_vec();
            out.resize(pixel_count, 0);
            Ok(out)
        }
        1 => {
            let mut row_counts = Vec::with_capacity(height as usize);
            for row in 0..height as usize {
                if row % RLE_ROW_CANCEL_POLL_INTERVAL == 0 {
                    crate::psb_reader::check_decode_cancel(cancel)?;
                }
                let count = if is_psb {
                    crate::psb_reader::read_u32(&mut r)? as usize
                } else {
                    crate::psb_reader::read_u16(&mut r)? as usize
                };
                row_counts.push(count);
            }

            let mut out = vec![0u8; pixel_count];
            let mut row_buf = Vec::with_capacity(width as usize);
            let width_usize = width as usize;
            for (row, &count) in row_counts.iter().enumerate() {
                if row % RLE_ROW_CANCEL_POLL_INTERVAL == 0 {
                    crate::psb_reader::check_decode_cancel(cancel)?;
                }
                let start = r.position() as usize;
                let end = start
                    .checked_add(count)
                    .ok_or_else(|| "PSD/PSB layer channel RLE row length overflow".to_string())?;
                let compressed = data
                    .get(start..end)
                    .ok_or_else(|| "PSD/PSB layer channel RLE row out of bounds".to_string())?;
                crate::psb_reader::unpack_bits_into(&mut row_buf, compressed, width_usize);
                let dst_start = row
                    .checked_mul(width_usize)
                    .ok_or_else(|| "PSD/PSB layer channel row offset overflow".to_string())?;
                let dst_end = dst_start
                    .checked_add(width_usize)
                    .ok_or_else(|| "PSD/PSB layer channel row end overflow".to_string())?;
                out.get_mut(dst_start..dst_end)
                    .ok_or_else(|| "PSD/PSB layer channel row out of bounds".to_string())?
                    .copy_from_slice(&row_buf[..width_usize]);
                r.set_position(end as u64);
            }
            Ok(out)
        }
        2 | 3 => {
            crate::psb_reader::check_decode_cancel(cancel)?;
            let compressed = data
                .get(2..)
                .ok_or_else(|| "PSD/PSB layer channel ZIP payload missing".to_string())?;
            crate::psb_zip::decode_zip_channel_bytes(
                compressed,
                width as usize,
                height as usize,
                8,
                compression == 3,
            )
            .map_err(Into::into)
        }
        _ => Err(format!("Unsupported layer channel compression: {compression}").into()),
    }
}

// -- Layer RGBA assembly -------------------------------------------------

/// Build a layer's straight-alpha RGBA8 rect from its decoded channels.
/// `color[0..3]` map to C/M/Y/K (mode 4) or R/G/B (mode 3, and fallback).
/// `color[0]` alone is used as gray for mode 1. Opacity and the optional
/// user mask are folded into alpha.
pub(crate) struct LayerRgbaArgs<'a> {
    pub(crate) color_mode: u16,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) color: &'a [Option<Vec<u8>>; 4],
    pub(crate) alpha: Option<&'a [u8]>,
    pub(crate) mask: Option<&'a [u8]>,
    pub(crate) opacity: u8,
    pub(crate) cmyk_icc: &'a [u8],
}

pub(crate) fn layer_to_rgba8(args: LayerRgbaArgs<'_>) -> Vec<u8> {
    let Some(pixel_count) = checked_layer_pixel_count(args.width, args.height) else {
        return Vec::new();
    };
    let Some(rgba_len) = pixel_count.checked_mul(4) else {
        return Vec::new();
    };
    let opacity = args.opacity as u32;

    if args.color_mode == 4
        && let (Some(c), Some(m), Some(y), Some(k)) = (
            args.color[0].as_deref(),
            args.color[1].as_deref(),
            args.color[2].as_deref(),
            args.color[3].as_deref(),
        )
    {
        let icc = crate::psb_cmyk_cms::resolve_cmyk_icc(if args.cmyk_icc.is_empty() {
            None
        } else {
            Some(args.cmyk_icc)
        });
        let span = crate::psb_cmyk_cms::AdobeCmykSpan {
            c,
            m,
            y,
            k,
            alpha: args.alpha,
        };
        if let Some(mut rgba) = crate::psb_cmyk_cms::planar_cmyk_adobe_to_rgba8(&span, icc) {
            fold_opacity_mask_into_alpha(&mut rgba, opacity, args.mask);
            return rgba;
        }
    }

    // Gray fast path: broadcast G->RGB via SIMD, then fold opacity/mask into alpha.
    if args.color_mode == 1
        && let Some(gray) = args.color[0].as_deref()
        && gray.len() >= pixel_count
    {
        let mut rgba = vec![0u8; rgba_len];
        let g = &gray[..pixel_count];
        if let Some(a) = args.alpha.filter(|a| a.len() >= pixel_count) {
            simple_image_viewer::simd_swizzle::interleave_rgba(
                g,
                g,
                g,
                &a[..pixel_count],
                &mut rgba,
            );
        } else {
            simple_image_viewer::simd_swizzle::interleave_rgb_with_alpha(g, g, g, 255, &mut rgba);
        }
        fold_opacity_mask_into_alpha(&mut rgba, opacity, args.mask);
        return rgba;
    }

    let mut rgba = vec![0u8; rgba_len];
    let sample =
        |ch: &Option<Vec<u8>>, i: usize| ch.as_deref().and_then(|d| d.get(i)).copied().unwrap_or(0);

    for i in 0..pixel_count {
        let (r, g, b) = match args.color_mode {
            4 => crate::psb_reader::cmyk_to_rgb(
                sample(&args.color[0], i),
                sample(&args.color[1], i),
                sample(&args.color[2], i),
                sample(&args.color[3], i),
            ),
            1 => {
                let v = sample(&args.color[0], i);
                (v, v, v)
            }
            _ => (
                sample(&args.color[0], i),
                sample(&args.color[1], i),
                sample(&args.color[2], i),
            ),
        };

        let base_alpha = args.alpha.and_then(|a| a.get(i)).copied().unwrap_or(255) as u32;
        let mut a = base_alpha * opacity / 255;
        if let Some(m) = args.mask {
            let mv = m.get(i).copied().unwrap_or(255) as u32;
            a = a * mv / 255;
        }

        let off = i * 4;
        rgba[off] = r;
        rgba[off + 1] = g;
        rgba[off + 2] = b;
        rgba[off + 3] = a as u8;
    }

    rgba
}

fn fold_opacity_mask_into_alpha(rgba: &mut [u8], opacity: u32, mask: Option<&[u8]>) {
    let pixel_count = rgba.len() / 4;
    for i in 0..pixel_count {
        let off = i * 4 + 3;
        let mut a = rgba[off] as u32 * opacity / 255;
        if let Some(m) = mask {
            let mv = m.get(i).copied().unwrap_or(255) as u32;
            a = a * mv / 255;
        }
        rgba[off] = a as u8;
    }
}

/// Decode a user/real mask channel into a layer-sized alpha matte, or `None`
/// when the mask is disabled, empty, or oversized (caller keeps no-mask).
#[allow(clippy::too_many_arguments)]
pub(crate) fn decode_mask_channel_to_layer(
    slice: &[u8],
    mask_info: &LayerMaskInfo,
    layer_left: i32,
    layer_top: i32,
    layer_w: u32,
    layer_h: u32,
    is_psb: bool,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<Option<Vec<u8>>, crate::loader::DecodeError> {
    let mask_w = mask_info.width();
    let mask_h = mask_info.height();
    let mask_has_bounds = !mask_info.disabled && mask_w > 0 && mask_h > 0;
    if !mask_has_bounds {
        return Ok(None);
    }
    // Same oversized-rect guard as the layer rect: skip just this mask
    // (fall back to no mask) rather than erroring out the whole composite.
    if !dimensions_within_limit(mask_w, mask_h) {
        log::debug!(
            "PSD/PSB layer mask rect {mask_w}x{mask_h} exceeds dimension/pixel \
             limit (max side {}, max pixels {}), skipping mask",
            crate::psb_reader::PSD_MAX_DIMENSION,
            crate::psb_layer_composite::MAX_LAYER_PIXELS
        );
        return Ok(None);
    }
    let mask_pixels = decode_channel_image(slice, mask_w, mask_h, is_psb, cancel)?;
    Ok(Some(build_layer_sized_mask(
        mask_info,
        &mask_pixels,
        layer_left,
        layer_top,
        layer_w,
        layer_h,
    )))
}

/// Blit a decoded mask (its own `mask_info` rect, which may differ from the
/// layer's rect in size and/or offset) into a layer-sized alpha-multiplier
/// buffer. Layer pixels outside the mask's rect use `mask_info.default_color`
/// (the standard PSD convention for "area not covered by the mask").
pub(crate) fn build_layer_sized_mask(
    mask_info: &LayerMaskInfo,
    mask_pixels: &[u8],
    layer_left: i32,
    layer_top: i32,
    layer_w: u32,
    layer_h: u32,
) -> Vec<u8> {
    let Some(pixel_count) = checked_layer_pixel_count(layer_w, layer_h) else {
        return Vec::new();
    };
    let mut out = vec![mask_info.default_color; pixel_count];
    let mask_w = mask_info.width() as i64;
    let mask_h = mask_info.height() as i64;
    if mask_w == 0 || mask_h == 0 {
        return out;
    }

    let off_x = mask_info.left as i64 - layer_left as i64;
    let off_y = mask_info.top as i64 - layer_top as i64;
    let dst_x0 = off_x.max(0);
    let dst_y0 = off_y.max(0);
    let dst_x1 = (off_x + mask_w).min(layer_w as i64);
    let dst_y1 = (off_y + mask_h).min(layer_h as i64);
    if dst_x0 >= dst_x1 || dst_y0 >= dst_y1 {
        return out;
    }

    for dy in dst_y0..dst_y1 {
        let sy = (dy - off_y) as usize;
        let dst_row_start = dy as usize * layer_w as usize;
        let src_row_start = sy * mask_w as usize;
        for dx in dst_x0..dst_x1 {
            let sx = (dx - off_x) as usize;
            out[dst_row_start + dx as usize] = mask_pixels[src_row_start + sx];
        }
    }

    out
}

// -- Blend modes -----------------------------------------------------------

fn separable_blend_kind(
    blend: &[u8; 4],
) -> Option<crate::psb_layer_blend_simd::SeparableBlendKind> {
    use crate::psb_layer_blend_simd::SeparableBlendKind;
    match blend {
        b"norm" => Some(SeparableBlendKind::Normal),
        b"scrn" => Some(SeparableBlendKind::Screen),
        b"lddg" => Some(SeparableBlendKind::LinearDodge),
        b"mul " => Some(SeparableBlendKind::Multiply),
        _ => None,
    }
}

pub(crate) fn blend_mode_supported(blend: &[u8; 4]) -> bool {
    separable_blend_kind(blend).is_some()
}

/// Straight-alpha separable blend of `layer_rgba` onto `canvas` (PDF formula).
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn blend_separable_onto(
    canvas: &mut [u8],
    canvas_w: u32,
    canvas_h: u32,
    layer_rgba: &[u8],
    left: i32,
    top: i32,
    lw: u32,
    lh: u32,
    kind: crate::psb_layer_blend_simd::SeparableBlendKind,
) {
    if lw == 0 || lh == 0 || canvas_w == 0 || canvas_h == 0 {
        return;
    }

    let canvas_w_i = canvas_w as i64;
    let canvas_h_i = canvas_h as i64;
    let left = left as i64;
    let top = top as i64;
    let lw_i = lw as i64;
    let lh_i = lh as i64;

    let src_x0 = (-left).max(0);
    let src_y0 = (-top).max(0);
    let src_x1 = (canvas_w_i - left).min(lw_i);
    let src_y1 = (canvas_h_i - top).min(lh_i);
    if src_x0 >= src_x1 || src_y0 >= src_y1 {
        return;
    }

    let span_w = (src_x1 - src_x0) as usize;
    let span_bytes = span_w * 4;
    for sy in src_y0..src_y1 {
        let dy = (top + sy) as usize;
        let dx0 = (left + src_x0) as usize;
        let d_off = dy * canvas_w as usize * 4 + dx0 * 4;
        let s_off = sy as usize * lw as usize * 4 + src_x0 as usize * 4;
        crate::psb_layer_blend_simd::blend_separable_span(
            &mut canvas[d_off..d_off + span_bytes],
            &layer_rgba[s_off..s_off + span_bytes],
            kind,
        );
    }
}

/// Straight-alpha src-over convenience used by unit tests (Normal = B(Cb,Cs)=Cs).
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn blend_normal_onto(
    canvas: &mut [u8],
    canvas_w: u32,
    canvas_h: u32,
    layer_rgba: &[u8],
    left: i32,
    top: i32,
    lw: u32,
    lh: u32,
) {
    blend_separable_onto(
        canvas,
        canvas_w,
        canvas_h,
        layer_rgba,
        left,
        top,
        lw,
        lh,
        crate::psb_layer_blend_simd::SeparableBlendKind::Normal,
    );
}

/// Dispatch by PSD blend-mode key; unknown modes fall back to Normal (logged once).
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn blend_layer_onto(
    canvas: &mut [u8],
    canvas_w: u32,
    canvas_h: u32,
    layer_rgba: &[u8],
    left: i32,
    top: i32,
    lw: u32,
    lh: u32,
    blend: &[u8; 4],
) {
    let kind = match separable_blend_kind(blend) {
        Some(k) => k,
        None => {
            log_unsupported_blend_once(blend);
            crate::psb_layer_blend_simd::SeparableBlendKind::Normal
        }
    };
    blend_separable_onto(
        canvas, canvas_w, canvas_h, layer_rgba, left, top, lw, lh, kind,
    );
}

/// Log an unsupported blend-mode key once (unsupported modes fall back to Normal).
fn log_unsupported_blend_once(blend: &[u8; 4]) {
    static SEEN: std::sync::OnceLock<parking_lot::Mutex<std::collections::HashSet<[u8; 4]>>> =
        std::sync::OnceLock::new();
    let seen = SEEN.get_or_init(|| parking_lot::Mutex::new(std::collections::HashSet::new()));
    let mut seen = seen.lock();
    if seen.insert(*blend) {
        let key = String::from_utf8_lossy(blend).into_owned();
        log::debug!("PSD/PSB layer composite: unsupported blend mode '{key}', treating as Normal");
    }
}

// -- One-layer decode --------------------------------------------------------

pub(crate) struct DecodedLayer {
    pub(crate) left: i32,
    pub(crate) top: i32,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) blend: [u8; 4],
    /// 0 = base / unclipped; non-zero = clipped to nearest base below.
    pub(crate) clipping: u8,
    pub(crate) rgba: Vec<u8>,
}

pub(crate) struct LayerDecodeParams<'a> {
    pub(crate) color_mode: u16,
    pub(crate) is_psb: bool,
    pub(crate) should_decode: bool,
    pub(crate) cancel: Option<&'a std::sync::atomic::AtomicBool>,
    pub(crate) cmyk_icc: &'a [u8],
}

/// Decode one layer's channels from `channel_data[*cursor..]`, advancing `*cursor`
/// past every channel regardless of `should_decode` so later layers stay aligned.
pub(crate) fn decode_one_layer(
    channel_data: &[u8],
    cursor: &mut usize,
    record: &LayerRecord,
    params: &LayerDecodeParams<'_>,
) -> Result<Option<DecodedLayer>, crate::loader::DecodeError> {
    let width = record.width();
    let height = record.height();
    let has_bounds = params.should_decode && width > 0 && height > 0;
    // Treat an oversized layer rect as corrupt *for that layer only*: skip
    // decoding it (still advancing `cursor` past its channel bytes below so
    // later layers stay aligned) rather than erroring out the whole
    // composite, since every other layer in the file may well be fine.
    if has_bounds && !dimensions_within_limit(width, height) {
        log::debug!(
            "PSD/PSB layer rect {width}x{height} exceeds dimension/pixel limit \
             (max side {}, max pixels {}), skipping layer",
            crate::psb_reader::PSD_MAX_DIMENSION,
            crate::psb_layer_composite::MAX_LAYER_PIXELS
        );
    }
    let can_decode = has_bounds && dimensions_within_limit(width, height);

    let mut color: [Option<Vec<u8>>; 4] = [None, None, None, None];
    let mut alpha: Option<Vec<u8>> = None;
    let mut mask: Option<Vec<u8>> = None;

    for ch in &record.channels {
        let data_len = ch.data_len as usize;
        let start = *cursor;
        let end = start
            .checked_add(data_len)
            .ok_or_else(|| "PSD/PSB layer channel data length overflow".to_string())?;
        let slice = channel_data
            .get(start..end)
            .ok_or_else(|| "PSD/PSB layer channel data out of bounds".to_string())?;
        *cursor = end;

        if !can_decode {
            continue;
        }

        match ch.id {
            -1 => match decode_channel_image(slice, width, height, params.is_psb, params.cancel) {
                Ok(data) => alpha = Some(data),
                Err(e) if e.is_cancelled() => return Err(e),
                Err(e) => log::debug!("PSD/PSB layer alpha channel decode failed: {e}"),
            },
            -2 | -3 => {
                // Channel -2 = user mask; -3 = real user mask (combined
                // vector+user). Prefer -3 when both are present: it is the
                // authoritative rendered mask. Geometry comes from
                // `real_mask` for -3 when parsed, otherwise the user-mask
                // rect. Missing/disabled/oversized rects skip that channel.
                let mask_info = if ch.id == -3 {
                    record.real_mask.as_ref().or(record.mask.as_ref())
                } else if record.real_mask.is_some() && record.channels.iter().any(|c| c.id == -3) {
                    // User mask is superseded by a real user mask channel.
                    None
                } else {
                    record.mask.as_ref()
                };
                if let Some(mask_info) = mask_info {
                    match decode_mask_channel_to_layer(
                        slice,
                        mask_info,
                        record.left,
                        record.top,
                        width,
                        height,
                        params.is_psb,
                        params.cancel,
                    ) {
                        Ok(Some(layer_mask)) => mask = Some(layer_mask),
                        Ok(None) => {}
                        Err(e) if e.is_cancelled() => return Err(e),
                        Err(e) => {
                            log::debug!("PSD/PSB layer mask channel {} decode failed: {e}", ch.id);
                        }
                    }
                }
            }
            0..=3 => {
                let idx = ch.id as usize;
                match decode_channel_image(slice, width, height, params.is_psb, params.cancel) {
                    Ok(data) => color[idx] = Some(data),
                    Err(e) if e.is_cancelled() => return Err(e),
                    Err(e) => log::debug!("PSD/PSB layer color channel {idx} decode failed: {e}"),
                }
            }
            _ => {}
        }
    }

    if !can_decode {
        return Ok(None);
    }

    if !blend_mode_supported(&record.blend) {
        log_unsupported_blend_once(&record.blend);
    }

    let rgba = layer_to_rgba8(LayerRgbaArgs {
        color_mode: params.color_mode,
        width,
        height,
        color: &color,
        alpha: alpha.as_deref(),
        mask: mask.as_deref(),
        opacity: record.opacity,
        cmyk_icc: params.cmyk_icc,
    });

    Ok(Some(DecodedLayer {
        left: record.left,
        top: record.top,
        width,
        height,
        blend: record.blend,
        clipping: record.clipping,
        rgba,
    }))
}

// -- Per-layer decode planning -----------------------------------------------

/// Precompute each layer's `[start, end)` byte range in the contiguous channel
/// image data block. Validates bounds once so parallel workers can slice
/// independently without a shared cursor.
pub(crate) fn layer_channel_byte_ranges(
    records: &[LayerRecord],
    channel_data_len: usize,
) -> Result<Vec<(usize, usize)>, crate::loader::DecodeError> {
    let mut ranges = Vec::with_capacity(records.len());
    let mut cursor = 0usize;
    for record in records {
        let start = cursor;
        for ch in &record.channels {
            cursor = cursor
                .checked_add(ch.data_len as usize)
                .ok_or_else(|| "PSD/PSB layer channel data length overflow".to_string())?;
        }
        if cursor > channel_data_len {
            return Err("PSD/PSB layer channel data out of bounds".into());
        }
        ranges.push((start, cursor));
    }
    Ok(ranges)
}

/// Whether the GPU all-at-once batch path is eligible: every layer that will
/// actually be composited must use Normal blend with no clipping, and the
/// batch's total decoded RGBA footprint must fit
/// [`crate::psb_layer_composite::MAX_COMPOSITE_DECODED_BYTES`]. Metadata-only
/// (no channel decode), so the GPU-vs-CPU-streaming choice is made before
/// paying for any pixel work. Returns the total decoded-byte footprint on
/// success (currently unused by callers beyond the eligibility signal, but
/// kept for logging/diagnostics).
pub(crate) fn gpu_batch_eligible_decoded_bytes(
    info: &LayerInfo<'_>,
    visible: &[bool],
) -> Option<u64> {
    let mut decoded_bytes = 0u64;
    for (i, record) in info.records.iter().enumerate() {
        let visible_i = visible.get(i).copied().unwrap_or(false);
        if !layer_will_decode(record, visible_i) {
            continue;
        }
        if record.blend != *b"norm" || record.clipping != 0 {
            return None;
        }
        decoded_bytes =
            accumulate_decoded_layer_bytes(decoded_bytes, record.width(), record.height()).ok()?;
    }
    Some(decoded_bytes)
}

/// Decode every eligible visible layer (optionally in parallel). Blend order
/// is preserved: results are collected in record order, skipping layers that
/// decode to `None`.
///
/// Used only by the GPU all-at-once batch path (see
/// [`crate::psb_layer_composite::run_composite_pass_gpu_batch`]): every
/// decoded layer is held resident simultaneously, so the pre-pass sums the
/// *whole* layer stack into
/// [`crate::psb_layer_composite::MAX_COMPOSITE_DECODED_BYTES`] before any
/// allocation. The CPU streaming path never calls this; it bounds only the
/// current + prefetched layer via
/// [`crate::psb_layer_composite::check_streaming_pair_budget`].
pub(crate) fn decode_layers_for_composite(
    info: &LayerInfo<'_>,
    visible: &[bool],
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<Vec<DecodedLayer>, crate::loader::DecodeError> {
    let ranges = layer_channel_byte_ranges(&info.records, info.channel_data.len())?;
    // Fail fast on total decoded RGBA footprint before parallel alloc.
    let mut decoded_bytes = 0u64;
    for (i, record) in info.records.iter().enumerate() {
        let visible_i = visible.get(i).copied().unwrap_or(false);
        if !layer_will_decode(record, visible_i) {
            continue;
        }
        decoded_bytes =
            accumulate_decoded_layer_bytes(decoded_bytes, record.width(), record.height())?;
    }
    let decode_at = |i: usize,
                     record: &LayerRecord|
     -> Result<Option<DecodedLayer>, crate::loader::DecodeError> {
        crate::psb_reader::check_decode_cancel(cancel)?;
        let should_decode = visible[i]
            && !record.is_section_divider
            && !record.is_empty_bounds()
            && record.opacity > 0;
        let (start, end) = ranges[i];
        let mut cursor = 0usize;
        decode_one_layer(
            &info.channel_data[start..end],
            &mut cursor,
            record,
            &LayerDecodeParams {
                color_mode: info.color_mode,
                is_psb: info.is_psb,
                should_decode,
                cancel,
                cmyk_icc: info.cmyk_icc.as_slice(),
            },
        )
    };

    if info.records.len() >= crate::psb_layer_decode_pool::PARALLEL_LAYER_DECODE_MIN {
        // Dedicated pool (capped at 2-4 workers): do not nest into img-loader /
        // refinement / strip pools via bare `par_iter`.
        use rayon::prelude::*;
        let results: Vec<Result<Option<DecodedLayer>, crate::loader::DecodeError>> =
            crate::psb_layer_decode_pool::PSD_LAYER_DECODE_POOL.install(|| {
                info.records
                    .par_iter()
                    .enumerate()
                    .map(|(i, record)| decode_at(i, record))
                    .collect()
            });
        let mut layers = Vec::new();
        for result in results {
            if let Some(layer) = result? {
                layers.push(layer);
            }
        }
        return Ok(layers);
    }

    let mut layers = Vec::new();
    for (i, record) in info.records.iter().enumerate() {
        if let Some(layer) = decode_at(i, record)? {
            layers.push(layer);
        }
    }
    Ok(layers)
}

/// GPU all-at-once batch composite: decode every eligible visible layer up
/// front (bottom to top), then blend on the GPU in one pass. Falls back to a
/// single CPU `blend_layers_with_clipping` batch call (not streaming) if the
/// GPU dispatch itself fails -- the layers are already resident, so there is
/// nothing to save by streaming at that point.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_composite_pass_gpu_batch(
    info: &LayerInfo<'_>,
    visible: &[bool],
    canvas: &mut Vec<u8>,
    canvas_w: u32,
    canvas_h: u32,
    cancel: Option<&std::sync::atomic::AtomicBool>,
    gpu_ctx: &crate::psb_layer_blend_gpu::PsdGpuContext,
    timing: &mut CompositeTiming,
) -> Result<usize, crate::loader::DecodeError> {
    // Layer records and channel_data are both stored bottom to top (index 0
    // is the bottommost layer). Decoding may run in parallel once per-layer
    // byte ranges are known; blending still walks decoded layers bottom to top.
    let decode_t0 = std::time::Instant::now();
    let layers = decode_layers_for_composite(info, visible, cancel)?;
    // Decode includes PackBits + planar convert + CMYK/ICC; split CMS later if needed.
    timing.unpack_ms += decode_t0.elapsed().as_secs_f64() * 1000.0;
    timing.layers = layers.len();

    if layers.is_empty() {
        return Ok(0);
    }

    let blend_t0 = std::time::Instant::now();
    let clip_refs: Vec<crate::psb_layer_clip::ClipLayerRef<'_>> = layers
        .iter()
        .map(|l| crate::psb_layer_clip::ClipLayerRef {
            left: l.left,
            top: l.top,
            width: l.width,
            height: l.height,
            blend: l.blend,
            clipping: l.clipping,
            rgba: &l.rgba,
        })
        .collect();
    // Re-verified from the decoded layers themselves (not just the metadata
    // pre-check in `gpu_batch_eligible_decoded_bytes`): correctness of the
    // GPU dispatch must never depend solely on a prediction.
    let all_normal = layers.iter().all(|l| l.blend == *b"norm");
    let has_clipping = crate::psb_layer_clip::any_layer_clipped(&clip_refs);
    let used_gpu = if !all_normal || has_clipping {
        false
    } else {
        let layer_refs: Vec<crate::psb_layer_blend_gpu::DecodedLayerRef<'_>> = layers
            .iter()
            .map(|l| crate::psb_layer_blend_gpu::DecodedLayerRef {
                left: l.left,
                top: l.top,
                width: l.width,
                height: l.height,
                rgba: &l.rgba,
            })
            .collect();
        let readback_t0 = std::time::Instant::now();
        if let Some(gpu_pixels) = crate::psb_layer_blend_gpu::try_blend_layers_gpu(
            gpu_ctx,
            canvas_w,
            canvas_h,
            canvas,
            &layer_refs,
            cancel,
        ) {
            timing.readback_ms += readback_t0.elapsed().as_secs_f64() * 1000.0;
            // Take ownership of the GPU readback buffer (no full-canvas copy).
            *canvas = gpu_pixels;
            timing.mode = "gpu";
            true
        } else {
            false
        }
    };

    if !used_gpu {
        crate::psb_layer_clip::blend_layers_with_clipping(
            canvas, canvas_w, canvas_h, &clip_refs, cancel,
        )?;
        timing.mode = "cpu";
    }
    timing.blend_ms += blend_t0.elapsed().as_secs_f64() * 1000.0;
    // For GPU, blend_ms includes upload+dispatch+readback; readback_ms is nested.
    // Prefer reporting GPU wall as blend_ms and keep readback as a subset hint.
    Ok(layers.len())
}

/// CPU streaming composite (default path): decode one layer, blend it via
/// [`crate::psb_layer_clip::ClipBlendState::push_layer`], then drop its RGBA
/// before decoding the next one. At most
/// [`crate::psb_layer_composite::LAYER_PREFETCH_WINDOW`] decoded layers are
/// resident at once: while the current layer blends on this thread, the
/// next layer's channels decode in parallel on
/// [`crate::psb_layer_decode_pool::PSD_LAYER_DECODE_POOL`].
///
/// Layers that are hidden/dividers/empty/oversized decode to `None` quickly
/// (no channel decode work, just cursor bookkeeping); the scan past them is
/// sequential rather than overlapped with the previous layer's blend, since
/// there is no real decode cost to hide.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_composite_pass_cpu_streaming(
    info: &LayerInfo<'_>,
    visible: &[bool],
    canvas: &mut [u8],
    canvas_w: u32,
    canvas_h: u32,
    cancel: Option<&std::sync::atomic::AtomicBool>,
    timing: &mut CompositeTiming,
    peak_tracker: &StreamingPeakTracker,
) -> Result<usize, crate::loader::DecodeError> {
    let ranges = layer_channel_byte_ranges(&info.records, info.channel_data.len())?;
    let record_count = info.records.len();
    let decode_t0 = std::time::Instant::now();

    let decode_at = |i: usize| -> Result<Option<DecodedLayer>, crate::loader::DecodeError> {
        crate::psb_reader::check_decode_cancel(cancel)?;
        let record = &info.records[i];
        let should_decode = visible[i]
            && !record.is_section_divider
            && !record.is_empty_bounds()
            && record.opacity > 0;
        let (start, end) = ranges[i];
        let mut cursor = 0usize;
        decode_one_layer(
            &info.channel_data[start..end],
            &mut cursor,
            record,
            &LayerDecodeParams {
                color_mode: info.color_mode,
                is_psb: info.is_psb,
                should_decode,
                cancel,
                cmyk_icc: info.cmyk_icc.as_slice(),
            },
        )
    };

    let mut clip_state = crate::psb_layer_clip::ClipBlendState::new(canvas_w, canvas_h);
    let mut composited = 0usize;
    let mut idx = 0usize;

    let mut current: Option<DecodedLayer> = None;
    while idx < record_count {
        let decoded = decode_at(idx)?;
        idx += 1;
        if decoded.is_some() {
            current = decoded;
            peak_tracker.acquire();
            break;
        }
    }

    while let Some(layer) = current.take() {
        composited += 1;
        let next_idx = idx;
        let next_will_decode = next_idx < record_count
            && layer_will_decode(&info.records[next_idx], visible[next_idx]);
        if next_will_decode {
            let next_record = &info.records[next_idx];
            crate::psb_layer_composite::check_streaming_pair_budget(
                &layer,
                next_record.width(),
                next_record.height(),
            )?;
        }

        let clip_ref = crate::psb_layer_clip::ClipLayerRef {
            left: layer.left,
            top: layer.top,
            width: layer.width,
            height: layer.height,
            blend: layer.blend,
            clipping: layer.clipping,
            rgba: &layer.rgba,
        };

        let (next_result, blend_result) = if next_idx < record_count {
            let prefetch_next = || {
                let result = decode_at(next_idx);
                if matches!(result, Ok(Some(_))) {
                    peak_tracker.acquire();
                }
                result
            };
            crate::psb_layer_decode_pool::PSD_LAYER_DECODE_POOL.install(|| {
                rayon::join(prefetch_next, || {
                    clip_state.push_layer(canvas, &clip_ref, cancel)
                })
            })
        } else {
            (Ok(None), clip_state.push_layer(canvas, &clip_ref, cancel))
        };

        blend_result?;
        peak_tracker.release();
        drop(layer);
        idx = next_idx + 1;

        current = match next_result? {
            Some(next_layer) => Some(next_layer),
            None => {
                // Scan forward sequentially past skipped layers -- see doc
                // comment above for why this does not need to overlap.
                let mut found = None;
                while idx < record_count {
                    let decoded = decode_at(idx)?;
                    idx += 1;
                    if decoded.is_some() {
                        found = decoded;
                        break;
                    }
                }
                if found.is_some() {
                    peak_tracker.acquire();
                }
                found
            }
        };
    }

    clip_state.finish(canvas, cancel)?;
    // Decode and blend interleave on this path (prefetch overlaps with the
    // previous layer's blend), so there is no clean split between the two;
    // report the whole pass as unpack_ms and leave blend_ms at its default.
    timing.unpack_ms += decode_t0.elapsed().as_secs_f64() * 1000.0;
    timing.layers = composited;
    timing.mode = "cpu";
    Ok(composited)
}

/// Tracks how many [`DecodedLayer`]s are resident at once during one CPU
/// streaming composite pass, and the peak seen. Every call site owns its own
/// instance (no shared/global state), so concurrent composites -- including
/// concurrent unit tests exercising this path -- can never interfere with
/// each other's counts.
#[derive(Default)]
pub(crate) struct StreamingPeakTracker {
    live: std::sync::atomic::AtomicUsize,
    peak: std::sync::atomic::AtomicUsize,
}

impl StreamingPeakTracker {
    pub(crate) fn acquire(&self) {
        use std::sync::atomic::Ordering;
        let live = self.live.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(live, Ordering::SeqCst);
    }

    pub(crate) fn release(&self) {
        self.live.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn peak(&self) -> usize {
        self.peak.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DecodedLayer, LayerDecodeParams, LayerRgbaArgs, blend_layer_onto, blend_normal_onto,
        blend_separable_onto, build_layer_sized_mask, decode_channel_image, decode_one_layer,
        layer_to_rgba8,
    };
    use crate::psb_layer_blend_simd::SeparableBlendKind;
    use crate::psb_layer_composite::{LayerChannel, LayerMaskInfo, LayerRecord};

    /// Build a minimal `LayerRecord` for decode tests; mirrors
    /// `psb_layer_composite::tests::mk_layer`, but only the fields these
    /// decode-path tests actually set are populated by callers.
    fn mk_layer(hidden: bool, is_section_divider: bool, section_type: Option<u32>) -> LayerRecord {
        LayerRecord {
            top: 0,
            left: 0,
            bottom: 1,
            right: 1,
            channels: Vec::new(),
            blend: *b"norm",
            opacity: 255,
            clipping: 0,
            flags: if hidden { 2 } else { 0 },
            mask_size: 0,
            mask: None,
            real_mask: None,
            is_section_divider,
            section_type,
        }
    }

    #[test]
    fn decode_channel_image_rejects_oversized_dims() {
        let data = [0u8, 0u8]; // compression = Raw
        let err =
            decode_channel_image(&data, u32::MAX, u32::MAX, false, None).expect_err("oversized");
        assert!(
            err.as_str().contains("exceeds limit"),
            "unexpected err: {err}"
        );
    }

    #[test]
    fn decode_one_layer_oversized_layer_rect_is_skipped() {
        // A malicious/malformed layer record claiming an absurd width would
        // otherwise make `decode_channel_image` try to `vec![0u8; w * h]`,
        // risking an allocation-failure abort. It must be skipped instead.
        let mut record = mk_layer(false, false, None);
        record.top = 0;
        record.left = 0;
        record.bottom = 1_000_000_000;
        record.right = 1_000_000_000;
        record.channels = vec![LayerChannel {
            id: -1,
            data_len: 0,
        }];

        let channel_data: [u8; 0] = [];
        let mut cursor = 0usize;
        let result = decode_one_layer(
            &channel_data,
            &mut cursor,
            &record,
            &LayerDecodeParams {
                color_mode: 3,
                is_psb: false,
                should_decode: true,
                cancel: None,
                cmyk_icc: &[],
            },
        );

        assert!(result.is_ok(), "oversized layer must not error out");
        assert!(
            result.unwrap().is_none(),
            "oversized layer must be skipped rather than decoded"
        );
        assert_eq!(cursor, 0, "cursor still advances past the channel bytes");
    }

    #[test]
    fn decode_one_layer_oversized_mask_rect_is_skipped() {
        // Same guard, but for the mask channel's own (potentially
        // independently-sized) rect rather than the layer's rect.
        let mut record = mk_layer(false, false, None);
        record.top = 0;
        record.left = 0;
        record.bottom = 2;
        record.right = 2;
        record.mask = Some(LayerMaskInfo {
            top: 0,
            left: 0,
            bottom: 1_000_000_000,
            right: 1_000_000_000,
            default_color: 0,
            disabled: false,
            has_parameters_applied: false,
        });
        record.channels = vec![LayerChannel {
            id: -2,
            data_len: 2,
        }];

        // Compression = 0 (raw), no pixel bytes follow -- irrelevant since
        // the oversized mask rect must be rejected before any read/alloc.
        let channel_data = [0u8, 0u8];
        let mut cursor = 0usize;
        let result = decode_one_layer(
            &channel_data,
            &mut cursor,
            &record,
            &LayerDecodeParams {
                color_mode: 3,
                is_psb: false,
                should_decode: true,
                cancel: None,
                cmyk_icc: &[],
            },
        );

        assert!(result.is_ok());
        let layer = result.unwrap().expect("layer rect itself is valid");
        assert_eq!(layer.width, 2);
        assert_eq!(layer.height, 2);
        // No mask could be decoded, so alpha defaults to fully opaque (255)
        // via `layer_to_rgba8`'s `unwrap_or(255)` fallback for a missing mask.
        assert!(layer.rgba.chunks_exact(4).all(|px| px[3] == 255));
    }

    #[test]
    fn blend_normal_onto_2x2_straight_alpha() {
        // Opaque red covers the top-left pixel; 50% green partially covers top-right;
        // the bottom row of the layer is fully transparent and must not touch the canvas.
        let mut canvas = vec![
            10, 10, 10, 255, // (0,0)
            20, 20, 20, 255, // (1,0)
            30, 30, 30, 255, // (0,1)
            40, 40, 40, 255, // (1,1)
        ];
        let layer = vec![
            255, 0, 0, 255, // (0,0) opaque red
            0, 255, 0, 128, // (1,0) 50% green
            0, 0, 0, 0, // (0,1) transparent
            0, 0, 0, 0, // (1,1) transparent
        ];

        blend_normal_onto(&mut canvas, 2, 2, &layer, 0, 0, 2, 2);

        assert_eq!(&canvas[0..4], &[255, 0, 0, 255]);
        assert_eq!(&canvas[8..12], &[30, 30, 30, 255]);
        assert_eq!(&canvas[12..16], &[40, 40, 40, 255]);

        // (1,0): green over gray20 at 50% alpha, straight-alpha src-over.
        let blended = &canvas[4..8];
        assert_eq!(blended[3], 255);
        assert_eq!(blended[0], 10); // (0*128 + 20*255*127/255) / 255 ~= 10
        assert_eq!(blended[1], 138); // (255*128 + 20*127) / 255 ~= 138
        assert_eq!(blended[2], 10);
    }

    #[test]
    fn blend_normal_onto_clips_to_canvas() {
        // A 3x3 opaque white layer at (1,1) only overlaps the canvas's bottom-right pixel.
        let mut canvas = vec![0u8; 2 * 2 * 4];
        let layer = vec![255u8; 3 * 3 * 4];
        blend_normal_onto(&mut canvas, 2, 2, &layer, 1, 1, 3, 3);
        assert_eq!(&canvas[0..12], &[0u8; 12]);
        assert_eq!(&canvas[12..16], &[255, 255, 255, 255]);
    }

    #[test]
    fn blend_screen_opaque_black_preserves_backdrop() {
        // Screen light-effect layers are often black + bright flare; black must
        // not paint an opaque rectangle (the Normal-fallback bug).
        let mut canvas = vec![40u8, 80, 120, 255, 40, 80, 120, 255];
        let layer = [0u8, 0, 0, 255, 255, 255, 255, 255];
        blend_separable_onto(
            &mut canvas,
            2,
            1,
            &layer,
            0,
            0,
            2,
            1,
            SeparableBlendKind::Screen,
        );
        assert_eq!(&canvas[0..4], &[40, 80, 120, 255]);
        assert_eq!(&canvas[4..8], &[255, 255, 255, 255]);
    }

    #[test]
    fn blend_layer_onto_dispatches_screen_key() {
        let mut canvas = vec![100u8, 100, 100, 255];
        let layer = [0u8, 0, 0, 255];
        blend_layer_onto(&mut canvas, 1, 1, &layer, 0, 0, 1, 1, b"scrn");
        assert_eq!(&canvas, &[100, 100, 100, 255]);
    }

    #[test]
    fn build_layer_sized_mask_smaller_mask_with_offset() {
        // 2x2 mask offset by (1,1) inside a 4x4 layer.
        let mask_info = LayerMaskInfo {
            top: 1,
            left: 1,
            bottom: 3,
            right: 3,
            default_color: 0,
            disabled: false,
            has_parameters_applied: false,
        };
        let mask_pixels = vec![10, 20, 30, 40];

        let out = build_layer_sized_mask(&mask_info, &mask_pixels, 0, 0, 4, 4);

        assert_eq!(out.len(), 16);
        let mut expected = vec![0u8; 16];
        expected[4 + 1] = 10;
        expected[4 + 2] = 20;
        expected[2 * 4 + 1] = 30;
        expected[2 * 4 + 2] = 40;
        assert_eq!(out, expected);
    }

    #[test]
    fn build_layer_sized_mask_default_color_outside_mask_rect() {
        // Mask rect falls entirely outside the layer's bounds, so every
        // output pixel must fall back to `default_color`.
        let mask_info = LayerMaskInfo {
            top: 10,
            left: 10,
            bottom: 11,
            right: 11,
            default_color: 255,
            disabled: false,
            has_parameters_applied: false,
        };
        let mask_pixels = vec![99];

        let out = build_layer_sized_mask(&mask_info, &mask_pixels, 0, 0, 3, 3);

        assert_eq!(out, vec![255u8; 9]);
    }

    #[test]
    fn layer_to_rgba8_cmyk_opacity_mask_numeric() {
        // 1x1 CMYK pixel (Adobe polarity 0=100% ink): c=204, m=153, y=102, k=204.
        // r = 204*204/255 = 163; g = 153*204/255 = 122; b = 102*204/255 = 81.
        let color: [Option<Vec<u8>>; 4] = [
            Some(vec![204]),
            Some(vec![153]),
            Some(vec![102]),
            Some(vec![204]),
        ];
        let alpha = vec![200u8];
        let mask = vec![128u8];

        // Force naive path: invalid ICC makes lcms fail closed to cmyk_to_rgb.
        let rgba = layer_to_rgba8(LayerRgbaArgs {
            color_mode: 4,
            width: 1,
            height: 1,
            color: &color,
            alpha: Some(&alpha),
            mask: Some(&mask),
            opacity: 200,
            cmyk_icc: b"not-icc",
        });

        // a = 200 * 200 / 255 = 156, then 156 * 128 / 255 = 78.
        assert_eq!(rgba, vec![163, 122, 81, 78]);
    }

    #[test]
    fn check_streaming_pair_budget_allows_two_max_sized_layers() {
        // MAX_COMPOSITE_DECODED_BYTES (8 GiB) is exactly twice the max
        // single-layer RGBA footprint (MAX_LAYER_PIXELS * 4 = 4 GiB), so the
        // 2-layer streaming window can never itself exceed the budget for
        // individually-valid layers -- the check exists to stay correct if
        // either constant's relationship changes later, not because it is
        // reachable today. Confirm the boundary case is accepted (not a
        // false rejection) and a tiny pair is trivially accepted too.
        let current = DecodedLayer {
            left: 0,
            top: 0,
            width: 32_768,
            height: 32_768,
            blend: *b"norm",
            clipping: 0,
            rgba: Vec::new(),
        };
        assert!(
            crate::psb_layer_composite::check_streaming_pair_budget(&current, 32_768, 32_768)
                .is_ok()
        );
        assert!(crate::psb_layer_composite::check_streaming_pair_budget(&current, 1, 1).is_ok());
    }
}
