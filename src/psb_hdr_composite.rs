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

//! HDR layer compositor for 16-bit and 32-bit PSD/PSB documents.
//!
//! Shared-[`crate::psb_layer_composite::LayerInfo`] entry point:
//! [`composite_layers_hdr_with_visibility_from_info`].  Unlike the SDR path in
//! `psb_layer_composite`, this does NOT fail on depth != 8 and works entirely
//! in linear-light f32 (no u8 quantisation until the caller tone-maps).
//!
//! Transfer function: probed from the embedded ICC profile in the image
//! resource section (`probe_icc_hdr`).  Falls back to Linear when no HDR ICC
//! is present (correct for 32-bit float PSD, which is already scene-linear).
//!
//! Clipping: mirrors `psb_layer_clip` -- `clipping == 0` starts a base;
//! following `clipping != 0` layers merge into the open group, then the group
//! is masked by the base alpha silhouette and blended with the base mode.

use std::sync::atomic::AtomicBool;

use crate::hdr::types::{
    HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat, HdrTransferFunction,
};
use crate::loader::DecodeError;
use crate::psb_hdr_blend::blend_separable_span_f32;
use crate::psb_icc_hdr::probe_icc_hdr;
use crate::psb_layer_blend_simd::SeparableBlendKind;
use crate::psb_layer_composite::dimensions_within_limit;
use crate::psb_layer_composite::{LayerRecord, strict_visibility_has_drawable_output};
use crate::psb_layer_decode::{
    channel_samples_to_f32, decode_channel_image, decode_mask_channel_to_layer,
    layer_channel_byte_ranges, layer_planes_to_rgba_f32,
};
use crate::psb_section_index::PsdSectionIndex;

// -- f32 canvas blend ---------------------------------------------------------

/// Blend a decoded layer's straight-alpha RGBA f32 rect onto the f32 canvas.
/// Clips the layer rect to the canvas bounds; out-of-bounds pixels are skipped.
#[allow(clippy::too_many_arguments)]
fn blend_f32_layer_onto(
    canvas: &mut [f32],
    canvas_w: u32,
    canvas_h: u32,
    layer_rgba: &[f32],
    left: i32,
    top: i32,
    lw: u32,
    lh: u32,
    kind: SeparableBlendKind,
) {
    if lw == 0 || lh == 0 || canvas_w == 0 || canvas_h == 0 {
        return;
    }
    let canvas_w_i = canvas_w as i64;
    let canvas_h_i = canvas_h as i64;
    let left_i = left as i64;
    let top_i = top as i64;
    let lw_i = lw as i64;
    let lh_i = lh as i64;

    let src_x0 = (-left_i).max(0);
    let src_y0 = (-top_i).max(0);
    let src_x1 = (canvas_w_i - left_i).min(lw_i);
    let src_y1 = (canvas_h_i - top_i).min(lh_i);
    if src_x0 >= src_x1 || src_y0 >= src_y1 {
        return;
    }

    let span_w = (src_x1 - src_x0) as usize;
    let span_floats = span_w * 4;

    for sy in src_y0..src_y1 {
        let dy = (top_i + sy) as usize;
        let dx0 = (left_i + src_x0) as usize;
        let d_off = dy * canvas_w as usize * 4 + dx0 * 4;
        let s_off = sy as usize * lw as usize * 4 + src_x0 as usize * 4;
        blend_separable_span_f32(
            &mut canvas[d_off..d_off + span_floats],
            &layer_rgba[s_off..s_off + span_floats],
            kind,
        );
    }
}

// -- f32 clipping groups (mirrors psb_layer_clip) -----------------------------

/// Decoded layer view for clip-aware f32 blending (bottom-to-top order).
struct ClipLayerRefF32<'a> {
    left: i32,
    top: i32,
    width: u32,
    height: u32,
    blend: [u8; 4],
    /// 0 = base / unclipped; non-zero = clipped to nearest base below.
    clipping: u8,
    rgba: &'a [f32],
}

/// Snapshot base-layer alpha into a full-canvas plane (0 outside the base rect).
fn capture_base_alpha_f32(
    canvas_w: u32,
    canvas_h: u32,
    base: &ClipLayerRefF32<'_>,
) -> Result<Vec<f32>, DecodeError> {
    let len = (canvas_w as usize)
        .checked_mul(canvas_h as usize)
        .ok_or_else(|| "PSD/PSB HDR clip base-alpha plane size overflow".to_string())?;
    let mut plane = vec![0.0f32; len];

    let left = base.left as i64;
    let top = base.top as i64;
    let lw = base.width as i64;
    let lh = base.height as i64;
    let cw = canvas_w as i64;
    let ch = canvas_h as i64;

    let src_x0 = (-left).max(0);
    let src_y0 = (-top).max(0);
    let src_x1 = (cw - left).min(lw);
    let src_y1 = (ch - top).min(lh);
    if src_x0 >= src_x1 || src_y0 >= src_y1 {
        return Ok(plane);
    }

    for sy in src_y0..src_y1 {
        let dy = (top + sy) as usize;
        let dx0 = (left + src_x0) as usize;
        let row_w = (src_x1 - src_x0) as usize;
        let dst_row = dy * canvas_w as usize + dx0;
        let src_row = sy as usize * base.width as usize + src_x0 as usize;
        for x in 0..row_w {
            plane[dst_row + x] = base.rgba[(src_row + x) * 4 + 3];
        }
    }
    Ok(plane)
}

/// Multiply every pixel's alpha in `group` by the corresponding base-alpha sample.
///
/// # Panics
/// Panics when `group.len() != base_alpha.len() * 4`. Callers must keep the
/// RGBA interleaved buffer and per-pixel mask in lockstep; SIMD paths assume it.
fn apply_base_alpha_mask_f32(group: &mut [f32], base_alpha: &[f32]) {
    assert_eq!(group.len(), base_alpha.len() * 4);

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            unsafe {
                apply_base_alpha_mask_f32_avx2(group, base_alpha);
            }
            return;
        }
        if is_x86_feature_detected!("sse4.1") {
            unsafe {
                apply_base_alpha_mask_f32_sse41(group, base_alpha);
            }
            return;
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        unsafe {
            apply_base_alpha_mask_f32_neon(group, base_alpha);
        }
        return;
    }

    apply_base_alpha_mask_f32_scalar(group, base_alpha);
}

fn apply_base_alpha_mask_f32_scalar(group: &mut [f32], base_alpha: &[f32]) {
    let (pixels, _) = group.as_chunks_mut::<4>();
    for (px, &mask) in pixels.iter_mut().zip(base_alpha.iter()) {
        apply_one_base_alpha_mask(px, mask);
    }
}

#[inline]
fn apply_one_base_alpha_mask(px: &mut [f32], mask: f32) {
    // Straight-alpha silhouette: scale alpha only (clear RGB iff a becomes 0).
    // Same contract as SDR `apply_base_alpha_mask` / GPU `cs_apply_base_alpha_mask`.
    if mask <= 0.0 {
        px[0] = 0.0;
        px[1] = 0.0;
        px[2] = 0.0;
        px[3] = 0.0;
    } else if mask < 1.0 {
        let a = px[3] * mask;
        px[3] = a.clamp(0.0, 1.0);
        if a <= 0.0 {
            px[0] = 0.0;
            px[1] = 0.0;
            px[2] = 0.0;
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn apply_base_alpha_mask_f32_sse41(group: &mut [f32], base_alpha: &[f32]) {
    use core::arch::x86_64::*;
    const LANES: usize = 4;
    let n = base_alpha.len();
    let mut i = 0usize;
    let zero = _mm_set1_ps(0.0);
    let one = _mm_set1_ps(1.0);

    while i + LANES <= n {
        unsafe {
            let mask = _mm_loadu_ps(base_alpha.as_ptr().add(i));
            let base = group.as_mut_ptr().add(i * 4);
            let le0 = _mm_cmple_ps(mask, zero);
            let lt1 = _mm_cmplt_ps(mask, one);
            let partial = _mm_andnot_ps(le0, lt1);
            let le0_bits = _mm_movemask_ps(le0);
            let partial_bits = _mm_movemask_ps(partial);

            if le0_bits == 0xF {
                for lane in 0..LANES {
                    _mm_storeu_ps(base.add(lane * 4), zero);
                }
                i += LANES;
                continue;
            }

            if partial_bits != 0 {
                let a = _mm_setr_ps(*base.add(3), *base.add(7), *base.add(11), *base.add(15));
                let scaled = _mm_mul_ps(a, mask);
                let new_a = _mm_min_ps(_mm_max_ps(scaled, zero), one);
                let a_le0 = _mm_cmple_ps(scaled, zero);
                let clear_rgb = _mm_or_ps(le0, a_le0);
                let clear_bits = _mm_movemask_ps(clear_rgb);

                let mut a_tmp = [0f32; LANES];
                _mm_storeu_ps(a_tmp.as_mut_ptr(), new_a);
                for (lane, &new_alpha) in a_tmp.iter().enumerate() {
                    let bit = 1 << lane;
                    let p = base.add(lane * 4);
                    if (le0_bits & bit) != 0 {
                        _mm_storeu_ps(p, zero);
                    } else if (partial_bits & bit) != 0 {
                        *p.add(3) = new_alpha;
                        if (clear_bits & bit) != 0 {
                            *p = 0.0;
                            *p.add(1) = 0.0;
                            *p.add(2) = 0.0;
                        }
                    }
                }
            } else if le0_bits != 0 {
                for lane in 0..LANES {
                    if (le0_bits & (1 << lane)) != 0 {
                        _mm_storeu_ps(base.add(lane * 4), zero);
                    }
                }
            }
        }
        i += LANES;
    }

    while i < n {
        apply_one_base_alpha_mask(&mut group[i * 4..i * 4 + 4], base_alpha[i]);
        i += 1;
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn apply_base_alpha_mask_f32_avx2(group: &mut [f32], base_alpha: &[f32]) {
    use core::arch::x86_64::*;
    const LANES: usize = 8;
    let n = base_alpha.len();
    let mut i = 0usize;
    let zero = _mm256_set1_ps(0.0);
    let one = _mm256_set1_ps(1.0);
    let alpha_idx = _mm256_setr_epi32(3, 7, 11, 15, 19, 23, 27, 31);

    while i + LANES <= n {
        unsafe {
            let mask = _mm256_loadu_ps(base_alpha.as_ptr().add(i));
            let base = group.as_mut_ptr().add(i * 4);
            let le0 = _mm256_cmp_ps(mask, zero, _CMP_LE_OQ);
            let lt1 = _mm256_cmp_ps(mask, one, _CMP_LT_OQ);
            let partial = _mm256_andnot_ps(le0, lt1);

            if _mm256_movemask_ps(le0) == 0xFF {
                for lane in 0..LANES {
                    _mm_storeu_ps(base.add(lane * 4), _mm_setzero_ps());
                }
                i += LANES;
                continue;
            }

            let le0_bits = _mm256_movemask_ps(le0);
            let partial_bits = _mm256_movemask_ps(partial);
            if partial_bits != 0 {
                let a = _mm256_i32gather_ps::<4>(base, alpha_idx);
                let scaled = _mm256_mul_ps(a, mask);
                let new_a = _mm256_min_ps(_mm256_max_ps(scaled, zero), one);
                let a_le0 = _mm256_cmp_ps(scaled, zero, _CMP_LE_OQ);
                let clear_rgb = _mm256_or_ps(le0, a_le0);
                let clear_bits = _mm256_movemask_ps(clear_rgb);

                let mut a_tmp = [0f32; LANES];
                _mm256_storeu_ps(a_tmp.as_mut_ptr(), new_a);
                for (lane, &new_alpha) in a_tmp.iter().enumerate() {
                    let bit = 1 << lane;
                    let p = base.add(lane * 4);
                    if (le0_bits & bit) != 0 {
                        _mm_storeu_ps(p, _mm_setzero_ps());
                    } else if (partial_bits & bit) != 0 {
                        *p.add(3) = new_alpha;
                        if (clear_bits & bit) != 0 {
                            *p = 0.0;
                            *p.add(1) = 0.0;
                            *p.add(2) = 0.0;
                        }
                    }
                }
            } else if le0_bits != 0 {
                for lane in 0..LANES {
                    if (le0_bits & (1 << lane)) != 0 {
                        _mm_storeu_ps(base.add(lane * 4), _mm_setzero_ps());
                    }
                }
            }
        }
        i += LANES;
    }

    if i < n {
        apply_base_alpha_mask_f32_scalar(&mut group[i * 4..], &base_alpha[i..]);
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn apply_base_alpha_mask_f32_neon(group: &mut [f32], base_alpha: &[f32]) {
    use core::arch::aarch64::*;
    const LANES: usize = 4;
    let n = base_alpha.len();
    let mut i = 0usize;
    let zero = vdupq_n_f32(0.0);
    let one = vdupq_n_f32(1.0);

    while i + LANES <= n {
        unsafe {
            let mask = vld1q_f32(base_alpha.as_ptr().add(i));
            let base = group.as_mut_ptr().add(i * 4);
            let pix = vld4q_f32(base);
            let le0 = vcleq_f32(mask, zero);
            let lt1 = vcltq_f32(mask, one);
            let partial = vbicq_u32(lt1, le0);

            let mut r = pix.0;
            let mut g = pix.1;
            let mut b = pix.2;
            let mut a = pix.3;

            let scaled = vmulq_f32(a, mask);
            let new_a = vminq_f32(vmaxq_f32(scaled, zero), one);
            let a_le0 = vcleq_f32(scaled, zero);
            let clear_rgb = vorrq_u32(le0, a_le0);

            a = vbslq_f32(partial, new_a, a);
            a = vbslq_f32(le0, zero, a);
            r = vbslq_f32(clear_rgb, zero, r);
            g = vbslq_f32(clear_rgb, zero, g);
            b = vbslq_f32(clear_rgb, zero, b);

            vst4q_f32(base, float32x4x4_t(r, g, b, a));
        }
        i += LANES;
    }

    while i < n {
        apply_one_base_alpha_mask(&mut group[i * 4..i * 4 + 4], base_alpha[i]);
        i += 1;
    }
}

/// One in-flight clipping group: a base layer plus every clip layer seen for
/// it so far. The group buffer (`temp` + `base_alpha`) is only materialized
/// once the first clip layer arrives, so a lone base (the common case) never
/// pays for it.
struct OpenClipGroupF32 {
    base_left: i32,
    base_top: i32,
    base_width: u32,
    base_height: u32,
    base_blend: [u8; 4],
    /// Owned copy of the base pixels while the group is still unmaterialized.
    /// Dropped after `temp` + `base_alpha` have captured everything needed.
    base_rgba: Option<Vec<f32>>,
    /// Full-canvas group content once a clip layer has been merged in.
    temp: Option<Vec<f32>>,
    base_alpha: Option<Vec<f32>>,
}

impl OpenClipGroupF32 {
    fn new(base: &ClipLayerRefF32<'_>) -> Self {
        Self {
            base_left: base.left,
            base_top: base.top,
            base_width: base.width,
            base_height: base.height,
            base_blend: base.blend,
            base_rgba: Some(base.rgba.to_vec()),
            temp: None,
            base_alpha: None,
        }
    }

    fn add_clip(
        &mut self,
        canvas_w: u32,
        canvas_h: u32,
        clip: &ClipLayerRefF32<'_>,
    ) -> Result<(), DecodeError> {
        if self.temp.is_none() {
            let canvas_len = (canvas_w as usize)
                .checked_mul(canvas_h as usize)
                .and_then(|n| n.checked_mul(4))
                .ok_or_else(|| "PSD/PSB HDR clip group buffer size overflow".to_string())?;
            let mut temp = vec![0.0f32; canvas_len];
            let base_rgba = self
                .base_rgba
                .as_deref()
                .expect("base_rgba is present until temp is initialized");
            // Build group content: base first (Normal into empty), then clips with their modes.
            blend_f32_layer_onto(
                &mut temp,
                canvas_w,
                canvas_h,
                base_rgba,
                self.base_left,
                self.base_top,
                self.base_width,
                self.base_height,
                SeparableBlendKind::Normal,
            );
            self.base_alpha = Some(capture_base_alpha_f32(
                canvas_w,
                canvas_h,
                &ClipLayerRefF32 {
                    left: self.base_left,
                    top: self.base_top,
                    width: self.base_width,
                    height: self.base_height,
                    blend: self.base_blend,
                    clipping: 0,
                    rgba: base_rgba,
                },
            )?);
            self.base_rgba = None;
            self.temp = Some(temp);
        }
        blend_f32_layer_onto(
            self.temp.as_mut().expect("temp initialized above"),
            canvas_w,
            canvas_h,
            clip.rgba,
            clip.left,
            clip.top,
            clip.width,
            clip.height,
            SeparableBlendKind::from_psd_key_or_normal(&clip.blend),
        );
        Ok(())
    }

    fn finalize(self, canvas: &mut [f32], canvas_w: u32, canvas_h: u32) {
        let OpenClipGroupF32 {
            base_left,
            base_top,
            base_width,
            base_height,
            base_blend,
            base_rgba,
            temp,
            base_alpha,
        } = self;
        match temp {
            None => {
                blend_f32_layer_onto(
                    canvas,
                    canvas_w,
                    canvas_h,
                    base_rgba
                        .as_deref()
                        .expect("base_rgba is present until temp is initialized"),
                    base_left,
                    base_top,
                    base_width,
                    base_height,
                    SeparableBlendKind::from_psd_key_or_normal(&base_blend),
                );
            }
            Some(mut temp) => {
                let base_alpha = base_alpha.expect("base_alpha is set whenever temp is set");
                apply_base_alpha_mask_f32(&mut temp, &base_alpha);
                blend_f32_layer_onto(
                    canvas,
                    canvas_w,
                    canvas_h,
                    &temp,
                    0,
                    0,
                    canvas_w,
                    canvas_h,
                    SeparableBlendKind::from_psd_key_or_normal(&base_blend),
                );
            }
        }
    }
}

/// Streaming f32 counterpart of the u8 [`crate::psb_layer_clip::ClipBlendState`].
///
/// Orphan clip layers (no base below) are skipped. Lone bases blend as usual.
struct ClipBlendStateF32 {
    canvas_w: u32,
    canvas_h: u32,
    open: Option<OpenClipGroupF32>,
}

impl ClipBlendStateF32 {
    fn new(canvas_w: u32, canvas_h: u32) -> Self {
        Self {
            canvas_w,
            canvas_h,
            open: None,
        }
    }

    /// Feed the next layer (bottom-to-top). May flush the previous group
    /// onto `canvas` if `layer` starts a new one.
    fn push_layer(
        &mut self,
        canvas: &mut [f32],
        layer: &ClipLayerRefF32<'_>,
        cancel: Option<&AtomicBool>,
    ) -> Result<(), DecodeError> {
        crate::psb_reader::check_decode_cancel(cancel)?;
        if layer.clipping != 0 {
            if let Some(open) = self.open.as_mut() {
                open.add_clip(self.canvas_w, self.canvas_h, layer)?;
            }
            // Else: clipped with no base in the decoded stack -- invisible.
            return Ok(());
        }
        // A new base starts here; flush whatever group was open before it.
        self.finish(canvas, cancel)?;
        self.open = Some(OpenClipGroupF32::new(layer));
        Ok(())
    }

    /// Flush any open group onto `canvas`. Safe to call multiple times.
    fn finish(
        &mut self,
        canvas: &mut [f32],
        cancel: Option<&AtomicBool>,
    ) -> Result<(), DecodeError> {
        crate::psb_reader::check_decode_cancel(cancel)?;
        if let Some(open) = self.open.take() {
            open.finalize(canvas, self.canvas_w, self.canvas_h);
        }
        Ok(())
    }
}

/// Blend decoded f32 layers bottom-to-top, honoring clipping groups.
#[cfg(test)]
fn blend_layers_with_clipping_f32(
    canvas: &mut [f32],
    canvas_w: u32,
    canvas_h: u32,
    layers: &[ClipLayerRefF32<'_>],
    cancel: Option<&AtomicBool>,
) -> Result<(), DecodeError> {
    let mut state = ClipBlendStateF32::new(canvas_w, canvas_h);
    for layer in layers {
        state.push_layer(canvas, layer, cancel)?;
    }
    state.finish(canvas, cancel)
}

// -- per-layer f32 decode -----------------------------------------------------

/// Decode one layer's channels to a straight-alpha RGBA f32 rect, or `None`
/// when the layer should not contribute (hidden, empty, zero-opacity, oversized).
struct LayerF32DecodeArgs<'a> {
    channel_data: &'a [u8],
    record: &'a LayerRecord,
    color_mode: u16,
    depth: u16,
    is_psb: bool,
    transfer: HdrTransferFunction,
    sdr_white_nits: f32,
    cancel: Option<&'a AtomicBool>,
}

fn decode_layer_to_f32(args: LayerF32DecodeArgs<'_>) -> Result<Option<Vec<f32>>, DecodeError> {
    let LayerF32DecodeArgs {
        channel_data,
        record,
        color_mode,
        depth,
        is_psb,
        transfer,
        sdr_white_nits,
        cancel,
    } = args;
    let width = record.width();
    let height = record.height();
    if width == 0 || height == 0 || !dimensions_within_limit(width, height) {
        return Ok(None);
    }

    // Alpha always uses Linear (alpha is not subject to colour transfer).
    let linear = HdrTransferFunction::Linear;

    let mut color: [Option<Vec<f32>>; 4] = [None, None, None, None];
    let mut alpha: Option<Vec<f32>> = None;
    let mut mask: Option<Vec<f32>> = None;

    let mut cursor = 0usize;
    for ch in &record.channels {
        let data_len = ch.data_len as usize;
        let start = cursor;
        let end = start
            .checked_add(data_len)
            .ok_or_else(|| "PSD/PSB HDR layer channel data length overflow".to_string())?;
        let slice = channel_data
            .get(start..end)
            .ok_or_else(|| "PSD/PSB HDR layer channel data out of bounds".to_string())?;
        cursor = end;

        match ch.id {
            -1 => {
                // Alpha channel: linear, no transfer decode.
                match decode_channel_image(slice, width, height, depth, is_psb, cancel) {
                    Ok(raw) => {
                        alpha = Some(channel_samples_to_f32(&raw, depth, linear, sdr_white_nits)?)
                    }
                    Err(e) if e.is_cancelled() => return Err(e),
                    Err(e) => log::debug!("PSD/PSB HDR layer alpha decode failed: {e}"),
                }
            }
            -2 | -3 => {
                let mask_info = if ch.id == -3 {
                    record.real_mask.as_ref().or(record.mask.as_ref())
                } else if record.real_mask.is_some() && record.channels.iter().any(|c| c.id == -3) {
                    None
                } else {
                    record.mask.as_ref()
                };
                if let Some(mi) = mask_info {
                    match decode_mask_channel_to_layer(
                        slice,
                        mi,
                        record.left,
                        record.top,
                        width,
                        height,
                        depth,
                        is_psb,
                        cancel,
                    ) {
                        Ok(Some(raw_u8)) => {
                            // Raw bytes from decode_mask_channel_to_layer are
                            // already layer-sized (built by build_layer_sized_mask).
                            // For 8-bit docs they are u8; for 16/32-bit docs they
                            // may be wider: convert the raw bytes via
                            // channel_samples_to_f32 before the mask blit.
                            // However build_layer_sized_mask always returns u8
                            // regardless of depth, so treat them as depth-8.
                            let f32_mask: Vec<f32> =
                                raw_u8.iter().map(|&b| b as f32 / 255.0).collect();
                            mask = Some(f32_mask);
                        }
                        Ok(None) => {}
                        Err(e) if e.is_cancelled() => return Err(e),
                        Err(e) => {
                            log::debug!("PSD/PSB HDR layer mask ch {} decode failed: {e}", ch.id);
                        }
                    }
                }
            }
            0..=3 => {
                let idx = ch.id as usize;
                match decode_channel_image(slice, width, height, depth, is_psb, cancel) {
                    Ok(raw) => {
                        color[idx] = Some(channel_samples_to_f32(
                            &raw,
                            depth,
                            transfer,
                            sdr_white_nits,
                        )?);
                    }
                    Err(e) if e.is_cancelled() => return Err(e),
                    Err(e) => {
                        log::debug!("PSD/PSB HDR layer color ch {idx} decode failed: {e}");
                    }
                }
            }
            _ => {}
        }
    }

    let rgba = layer_planes_to_rgba_f32(
        color_mode,
        width,
        height,
        &color,
        alpha.as_deref(),
        mask.as_deref(),
        record.opacity,
    );

    Ok(Some(rgba))
}

// -- public entry point -------------------------------------------------------

/// Composite an already-parsed layer stack into a linear-light RGBA f32 buffer
/// suitable for HDR display.
///
/// Does NOT fail on depth != 8: this is the HDR entry point for 16-bit
/// (PQ/HLG ICC-marked) and 32-bit documents.  The 8-bit u8 compositor in
/// `composite_layers_from_info` keeps its depth==8 guard; SDR main routes
/// 16/32-bit through this f32 path and tone-maps to RGBA8 when the display
/// environment is SDR-only.
///
/// Transfer function comes from `probe_icc_hdr` on the embedded ICC in the
/// image-resource section.  32-bit float PSD is always linear (no transfer).
///
/// Returns [`DecodeError::NoDrawableVisibleLayers`] when no visible layer
/// intersects the canvas (no pixel work is performed).
///
/// Callers that drive P2/P2.5 from a shared [`crate::psb_layer_composite::LayerInfo`]
/// should use this entry (not re-parse layer records per stage).
pub fn composite_layers_hdr_with_visibility_from_info(
    info: &crate::psb_layer_composite::LayerInfo<'_>,
    bytes: &[u8],
    index: &PsdSectionIndex,
    visible: &[bool],
    cancel: Option<&AtomicBool>,
    sdr_white_nits: f32,
) -> Result<HdrImageBuffer, DecodeError> {
    crate::psb_reader::check_decode_cancel(cancel)?;
    composite_layers_hdr_with_visibility(info, bytes, index, visible, cancel, sdr_white_nits)
}

fn composite_layers_hdr_with_visibility(
    info: &crate::psb_layer_composite::LayerInfo<'_>,
    bytes: &[u8],
    index: &PsdSectionIndex,
    visible: &[bool],
    cancel: Option<&AtomicBool>,
    sdr_white_nits: f32,
) -> Result<HdrImageBuffer, DecodeError> {
    if visible.len() != info.records.len() {
        return Err(DecodeError::Message(
            "PSD HDR visibility mask length mismatch".into(),
        ));
    }

    let depth = info.depth;
    let color_mode = info.color_mode;
    let canvas_w = info.width;
    let canvas_h = info.height;

    let embedded_icc =
        crate::psb_reader::extract_icc_profile_from_ir(bytes, index.ir_start, index.ir_end);
    let icc_probe = embedded_icc
        .as_deref()
        .map(probe_icc_hdr)
        .unwrap_or_default();
    // 32-bit float PSD is scene-linear by spec; for 16-bit use probed transfer.
    let transfer = if depth == 32 || !icc_probe.marks_hdr {
        HdrTransferFunction::Linear
    } else {
        icc_probe.transfer
    };

    if !strict_visibility_has_drawable_output(canvas_w, canvas_h, &info.records, visible) {
        return Err(DecodeError::NoDrawableVisibleLayers);
    }

    let pixel_count = (canvas_w as usize)
        .checked_mul(canvas_h as usize)
        .ok_or_else(|| DecodeError::Message("HDR canvas dimensions overflow".into()))?;
    let canvas_f32_len = pixel_count
        .checked_mul(4)
        .ok_or_else(|| DecodeError::Message("HDR canvas RGBA f32 length overflow".into()))?;

    // CMYK: paper white (all channels 1.0); others: transparent black.
    let mut canvas = if color_mode == 4 {
        vec![1.0f32; canvas_f32_len]
    } else {
        vec![0.0f32; canvas_f32_len]
    };

    let ranges = layer_channel_byte_ranges(&info.records, info.channel_data.len())?;

    let mut clip_state = ClipBlendStateF32::new(canvas_w, canvas_h);

    for (i, record) in info.records.iter().enumerate() {
        crate::psb_reader::check_decode_cancel(cancel)?;

        let vis = visible.get(i).copied().unwrap_or(false);
        let will_decode =
            vis && !record.is_section_divider && !record.is_empty_bounds() && record.opacity > 0;

        if !will_decode {
            continue;
        }

        let (start, end) = ranges[i];
        match decode_layer_to_f32(LayerF32DecodeArgs {
            channel_data: &info.channel_data[start..end],
            record,
            color_mode,
            depth,
            is_psb: info.is_psb,
            transfer,
            sdr_white_nits,
            cancel,
        }) {
            Ok(Some(rgba_f32)) => {
                let clip_ref = ClipLayerRefF32 {
                    left: record.left,
                    top: record.top,
                    width: record.width(),
                    height: record.height(),
                    blend: record.blend,
                    clipping: record.clipping,
                    rgba: &rgba_f32,
                };
                clip_state.push_layer(&mut canvas, &clip_ref, cancel)?;
            }
            Ok(None) => {}
            Err(e) if e.is_cancelled() => return Err(e),
            Err(e) => {
                log::debug!("PSD/PSB HDR layer {i} decode failed (skipped): {e}");
            }
        }
    }
    clip_state.finish(&mut canvas, cancel)?;

    let color_space = HdrColorSpace::LinearSrgb;
    let mut metadata = HdrImageMetadata::from_color_space(color_space);
    metadata.transfer_function = HdrTransferFunction::Linear;
    if let Some(nits) = icc_probe.peak_nits {
        metadata.luminance.mastering_max_nits = Some(nits);
    }

    Ok(HdrImageBuffer {
        width: canvas_w,
        height: canvas_h,
        format: HdrPixelFormat::Rgba32Float,
        color_space,
        metadata,
        rgba_f32: std::sync::Arc::new(canvas),
    })
}

#[cfg(test)]
mod tests {
    use super::{ClipLayerRefF32, blend_layers_with_clipping_f32};

    fn solid_rgba_f32(w: u32, h: u32, r: f32, g: f32, b: f32, a: f32) -> Vec<f32> {
        let mut v = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..w * h {
            v.extend_from_slice(&[r, g, b, a]);
        }
        v
    }

    fn px(canvas: &[f32], w: u32, x: u32, y: u32) -> [f32; 4] {
        let o = ((y * w + x) * 4) as usize;
        [canvas[o], canvas[o + 1], canvas[o + 2], canvas[o + 3]]
    }

    fn assert_px_eq(got: [f32; 4], want: [f32; 4]) {
        for i in 0..4 {
            assert!(
                (got[i] - want[i]).abs() < 1e-5,
                "channel {i}: got {got:?} want {want:?}"
            );
        }
    }

    #[test]
    fn clipping_masks_clip_layer_to_base_alpha_f32() {
        // Canvas 8x8. Red base 4x4 at (0,0). Blue clip 4x4 at (2,2) extends
        // past the base; with clipping, blue must only remain where base alpha > 0.
        let mut canvas = vec![0.0f32; 8 * 8 * 4];
        let base_rgba = solid_rgba_f32(4, 4, 1.0, 0.0, 0.0, 1.0);
        let clip_rgba = solid_rgba_f32(4, 4, 0.0, 0.0, 1.0, 1.0);
        let layers = [
            ClipLayerRefF32 {
                left: 0,
                top: 0,
                width: 4,
                height: 4,
                blend: *b"norm",
                clipping: 0,
                rgba: &base_rgba,
            },
            ClipLayerRefF32 {
                left: 2,
                top: 2,
                width: 4,
                height: 4,
                blend: *b"norm",
                clipping: 1,
                rgba: &clip_rgba,
            },
        ];

        blend_layers_with_clipping_f32(&mut canvas, 8, 8, &layers, None).unwrap();

        // Overlap of base and clip -> blue (clip on top inside group).
        assert_px_eq(px(&canvas, 8, 2, 2), [0.0, 0.0, 1.0, 1.0]);
        assert_px_eq(px(&canvas, 8, 3, 3), [0.0, 0.0, 1.0, 1.0]);
        // Base only (no clip coverage) -> red.
        assert_px_eq(px(&canvas, 8, 0, 0), [1.0, 0.0, 0.0, 1.0]);
        // Clip outside base silhouette -> transparent (the bug without clipping).
        assert_px_eq(px(&canvas, 8, 4, 2), [0.0, 0.0, 0.0, 0.0]);
        assert_px_eq(px(&canvas, 8, 5, 5), [0.0, 0.0, 0.0, 0.0]);
        assert_px_eq(px(&canvas, 8, 2, 4), [0.0, 0.0, 0.0, 0.0]);
    }
}
