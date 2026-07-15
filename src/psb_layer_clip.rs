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

//! PSD/PSB clipping-group composite (base + consecutive clip layers).
//!
//! Photoshop: `clipping == 0` is a base; following layers with `clipping != 0`
//! are clipped to that base's alpha silhouette. Clip blend modes apply inside
//! the group; the group result is composited onto the backdrop with the base
//! blend mode.

use crate::psb_layer_blend_simd::{SeparableBlendKind, blend_separable_span};
use std::sync::Arc;

/// Decoded layer view for clip-aware blending (bottom-to-top order).
pub(crate) struct ClipLayerRef<'a> {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    pub blend: [u8; 4],
    /// 0 = base / unclipped; non-zero = clipped to nearest base below.
    pub clipping: u8,
    pub rgba: &'a [u8],
    /// When the rgba data lives in an `Arc<[u8]>`, this field provides a borrow
    /// so that clip groups can clone the Arc (cheap refcount bump) instead of
    /// copying the pixel data. `None` when constructed from a non-Arc source
    /// (e.g. test `Vec<u8>`).
    pub(crate) rgba_arc: Option<&'a Arc<[u8]>>,
}

pub(crate) fn any_layer_clipped(layers: &[ClipLayerRef<'_>]) -> bool {
    layers.iter().any(|l| l.clipping != 0)
}

#[allow(clippy::too_many_arguments)]
fn blend_onto(
    canvas: &mut [u8],
    canvas_w: u32,
    canvas_h: u32,
    layer_rgba: &[u8],
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
    let Some(span_bytes) = span_w.checked_mul(4) else {
        return;
    };
    for sy in src_y0..src_y1 {
        let dy = (top + sy) as usize;
        let dx0 = (left + src_x0) as usize;
        let Some(d_off) = dy
            .checked_mul(canvas_w as usize)
            .and_then(|row| row.checked_mul(4))
            .and_then(|row| row.checked_add(dx0.checked_mul(4)?))
        else {
            return;
        };
        let Some(s_off) = (sy as usize)
            .checked_mul(lw as usize)
            .and_then(|row| row.checked_mul(4))
            .and_then(|row| row.checked_add((src_x0 as usize).checked_mul(4)?))
        else {
            return;
        };
        let Some(d_end) = d_off.checked_add(span_bytes) else {
            return;
        };
        let Some(s_end) = s_off.checked_add(span_bytes) else {
            return;
        };
        if d_end > canvas.len() || s_end > layer_rgba.len() {
            return;
        }
        blend_separable_span(&mut canvas[d_off..d_end], &layer_rgba[s_off..s_end], kind);
    }
}

/// Snapshot base-layer alpha into a full-canvas plane (0 outside the base rect).
fn capture_base_alpha(
    canvas_w: u32,
    canvas_h: u32,
    base: &ClipLayerRef<'_>,
) -> Result<Vec<u8>, crate::loader::DecodeError> {
    let len = (canvas_w as usize)
        .checked_mul(canvas_h as usize)
        .ok_or_else(|| "PSD/PSB clip base-alpha plane size overflow".to_string())?;
    let mut plane = vec![0u8; len];

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
        let Some(dst_row) = dy
            .checked_mul(canvas_w as usize)
            .and_then(|row| row.checked_add(dx0))
        else {
            continue;
        };
        let Some(src_row) = (sy as usize)
            .checked_mul(base.width as usize)
            .and_then(|row| row.checked_add(src_x0 as usize))
        else {
            continue;
        };
        let Some(dst_end) = dst_row.checked_add(row_w) else {
            continue;
        };
        let Some(src_end) = src_row.checked_add(row_w) else {
            continue;
        };
        if dst_end > plane.len() || src_end.checked_mul(4).is_none_or(|n| n > base.rgba.len()) {
            continue;
        }
        gather_alpha_row(
            &mut plane[dst_row..dst_row + row_w],
            &base.rgba[src_row * 4..][..row_w * 4],
        );
    }
    Ok(plane)
}

/// Multiply every pixel's alpha in `group` by the corresponding base-alpha sample.
///
/// RGB is left unchanged unless the resulting alpha is 0 (then cleared). This is
/// intentional straight-alpha masking: the group later blends with the base mode
/// via the PDF separable formula, which already weights by source alpha -- so
/// premultiplying RGB here would double-attenuate Screen / Multiply / Linear Dodge.
fn apply_base_alpha_mask(group: &mut [u8], base_alpha: &[u8]) {
    if group.len() != base_alpha.len().saturating_mul(4) || group.is_empty() {
        return;
    }

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("sse2") {
            unsafe {
                apply_base_alpha_mask_sse2(group, base_alpha);
            }
            return;
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        unsafe {
            apply_base_alpha_mask_neon(group, base_alpha);
        }
        return;
    }

    #[cfg(not(target_arch = "aarch64"))]
    {
        apply_base_alpha_mask_scalar(group, base_alpha);
    }
}

/// Scalar fallback: `alpha *= mask / 255` per pixel.
fn apply_base_alpha_mask_scalar(group: &mut [u8], base_alpha: &[u8]) {
    for (px, &mask) in group.chunks_exact_mut(4).zip(base_alpha.iter()) {
        if mask == 0 {
            px[0] = 0;
            px[1] = 0;
            px[2] = 0;
            px[3] = 0;
        } else if mask < 255 {
            let a = px[3] as u32 * mask as u32 / 255;
            px[3] = a as u8;
            if a == 0 {
                px[0] = 0;
                px[1] = 0;
                px[2] = 0;
            }
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn apply_base_alpha_mask_sse2(group: &mut [u8], base_alpha: &[u8]) {
    use crate::psb_simd_mul_div255::mul_div255_u8x8;
    use core::arch::x86_64::*;
    let n = group.len() / 4;
    let mut i = 0usize;
    while i + 8 <= n {
        let mut alpha = [0u8; 8];
        for lane in 0..8 {
            alpha[lane] = group[(i + lane) * 4 + 3];
        }
        unsafe {
            let av = _mm_loadl_epi64(alpha.as_ptr().cast());
            let mv = _mm_loadl_epi64(base_alpha.as_ptr().add(i).cast());
            let r = mul_div255_u8x8(av, mv);
            _mm_storel_epi64(alpha.as_mut_ptr().cast(), r);
        }
        for lane in 0..8 {
            let off = (i + lane) * 4;
            let a = alpha[lane];
            group[off + 3] = a;
            if a == 0 {
                group[off] = 0;
                group[off + 1] = 0;
                group[off + 2] = 0;
            }
        }
        i += 8;
    }
    if i < n {
        apply_base_alpha_mask_scalar(&mut group[i * 4..], &base_alpha[i..]);
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn apply_base_alpha_mask_neon(group: &mut [u8], base_alpha: &[u8]) {
    use crate::psb_simd_mul_div255::mul_div255_u8x16_neon;
    use core::arch::aarch64::*;
    let n = group.len() / 4;
    let mut i = 0usize;
    while i + 16 <= n {
        let mut new_alpha_arr = [0u8; 16];
        unsafe {
            let pix = vld4q_u8(group.as_ptr().add(i * 4));
            let alpha = pix.3;
            let mask = vld1q_u8(base_alpha.as_ptr().add(i));
            let new_alpha = mul_div255_u8x16_neon(alpha, mask);
            // Write new alpha back via vst4 (keeps RGB unchanged).
            let mut result = pix;
            result.3 = new_alpha;
            vst4q_u8(group.as_mut_ptr().add(i * 4), result);
            // Store to array so we can test per-lane below.
            vst1q_u8(new_alpha_arr.as_mut_ptr(), new_alpha);
        }
        // Clear RGB when new_alpha == 0 (scalar post-loop).
        for lane in 0..16 {
            let off = (i + lane) * 4;
            if new_alpha_arr[lane] == 0 {
                group[off] = 0;
                group[off + 1] = 0;
                group[off + 2] = 0;
            }
        }
        i += 16;
    }
    if i < n {
        apply_base_alpha_mask_scalar(&mut group[i * 4..], &base_alpha[i..]);
    }
}

// ---- gather_alpha_row: extract A byte from RGBA stride-4 layout ----

/// Copy alpha channel from a contiguous row of RGBA8 into a byte plane.
fn gather_alpha_row(dst: &mut [u8], src_rgba: &[u8]) {
    let n = dst.len().min(src_rgba.len() / 4);
    if n == 0 {
        return;
    }

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("ssse3") {
            unsafe {
                gather_alpha_row_ssse3(dst, src_rgba, n);
            }
            return;
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        unsafe {
            gather_alpha_row_neon(dst, src_rgba, n);
        }
        return;
    }

    #[cfg(target_arch = "x86_64")]
    gather_alpha_row_scalar(dst, src_rgba, n);
}

#[cfg(target_arch = "x86_64")]
fn gather_alpha_row_scalar(dst: &mut [u8], src_rgba: &[u8], n: usize) {
    for i in 0..n {
        dst[i] = src_rgba[i * 4 + 3];
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "ssse3")]
unsafe fn gather_alpha_row_ssse3(dst: &mut [u8], src_rgba: &[u8], n: usize) {
    use core::arch::x86_64::*;
    // pshufb: gather byte 3,7,11,15 from a 16-byte block into low 4 positions.
    let gather = _mm_setr_epi8(3, 7, 11, 15, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1);
    let mut i = 0usize;
    while i + 4 <= n {
        unsafe {
            let v = _mm_loadu_si128(src_rgba.as_ptr().add(i * 4).cast());
            let a = _mm_shuffle_epi8(v, gather);
            core::ptr::write_unaligned(
                dst.as_mut_ptr().add(i) as *mut u32,
                _mm_cvtsi128_si32(a) as u32,
            );
        }
        i += 4;
    }
    while i < n {
        dst[i] = src_rgba[i * 4 + 3];
        i += 1;
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn gather_alpha_row_neon(dst: &mut [u8], src_rgba: &[u8], n: usize) {
    use core::arch::aarch64::*;
    let mut i = 0usize;
    while i + 16 <= n {
        unsafe {
            let pix = vld4q_u8(src_rgba.as_ptr().add(i * 4));
            vst1q_u8(dst.as_mut_ptr().add(i), pix.3);
        }
        i += 16;
    }
    while i < n {
        dst[i] = src_rgba[i * 4 + 3];
        i += 1;
    }
}

/// One in-flight clipping group: a base layer plus every clip layer seen for
/// it so far. The group buffer (`temp` + `base_alpha`) is only materialized
/// once the first clip layer arrives, so a lone base (the common case) never
/// pays for it.
struct OpenClipGroup {
    base_left: i32,
    base_top: i32,
    base_width: u32,
    base_height: u32,
    base_blend: [u8; 4],
    /// Owned copy of the base pixels while the group is still unmaterialized.
    /// Dropped after `temp` + `base_alpha` have captured everything needed.
    base_rgba: Option<Arc<[u8]>>,
    /// Full-canvas group content once a clip layer has been merged in.
    temp: Option<Vec<u8>>,
    base_alpha: Option<Vec<u8>>,
}

impl OpenClipGroup {
    fn new(base: &ClipLayerRef<'_>) -> Self {
        let base_rgba = base
            .rgba_arc
            .map_or_else(|| Arc::from(base.rgba), |a| Arc::clone(a));
        Self {
            base_left: base.left,
            base_top: base.top,
            base_width: base.width,
            base_height: base.height,
            base_blend: base.blend,
            base_rgba: Some(base_rgba),
            temp: None,
            base_alpha: None,
        }
    }

    fn add_clip(
        &mut self,
        canvas_w: u32,
        canvas_h: u32,
        clip: &ClipLayerRef<'_>,
    ) -> Result<(), crate::loader::DecodeError> {
        if self.temp.is_none() {
            let canvas_len = (canvas_w as usize)
                .checked_mul(canvas_h as usize)
                .and_then(|n| n.checked_mul(4))
                .ok_or_else(|| "PSD/PSB clip group buffer size overflow".to_string())?;
            let mut temp = vec![0u8; canvas_len];
            let base_rgba = self
                .base_rgba
                .as_deref()
                .ok_or_else(|| "PSD/PSB clip group base pixels are missing".to_string())?;
            // Build group content: base first (Normal into empty), then clips with their modes.
            blend_onto(
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
            self.base_alpha = Some(capture_base_alpha(
                canvas_w,
                canvas_h,
                &ClipLayerRef {
                    left: self.base_left,
                    top: self.base_top,
                    width: self.base_width,
                    height: self.base_height,
                    blend: self.base_blend,
                    clipping: 0,
                    rgba: base_rgba,
                    rgba_arc: None,
                },
            )?);
            self.base_rgba = None;
            self.temp = Some(temp);
        }
        let temp = self
            .temp
            .as_mut()
            .ok_or_else(|| "PSD/PSB clip group buffer is missing".to_string())?;
        blend_onto(
            temp,
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

    fn finalize(
        self,
        canvas: &mut [u8],
        canvas_w: u32,
        canvas_h: u32,
    ) -> Result<(), crate::loader::DecodeError> {
        let OpenClipGroup {
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
                let base_rgba = base_rgba
                    .as_deref()
                    .ok_or_else(|| "PSD/PSB clip group base pixels are missing".to_string())?;
                blend_onto(
                    canvas,
                    canvas_w,
                    canvas_h,
                    base_rgba,
                    base_left,
                    base_top,
                    base_width,
                    base_height,
                    SeparableBlendKind::from_psd_key_or_normal(&base_blend),
                );
            }
            Some(mut temp) => {
                let base_alpha = base_alpha
                    .ok_or_else(|| "PSD/PSB clip group base alpha is missing".to_string())?;
                apply_base_alpha_mask(&mut temp, &base_alpha);
                blend_onto(
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
        Ok(())
    }
}

/// Streaming counterpart of [`blend_layers_with_clipping`]: lets callers feed
/// decoded layers one at a time (bottom-to-top) instead of holding every
/// layer's pixels resident at once.
///
/// Orphan clip layers (no base below) are skipped. Lone bases blend as usual.
pub(crate) struct ClipBlendState {
    canvas_w: u32,
    canvas_h: u32,
    open: Option<OpenClipGroup>,
}

impl ClipBlendState {
    pub(crate) fn new(canvas_w: u32, canvas_h: u32) -> Self {
        Self {
            canvas_w,
            canvas_h,
            open: None,
        }
    }

    /// Feed the next layer (bottom-to-top). May flush the previous group
    /// onto `canvas` if `layer` starts a new one.
    pub(crate) fn push_layer(
        &mut self,
        canvas: &mut [u8],
        layer: &ClipLayerRef<'_>,
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<(), crate::loader::DecodeError> {
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
        self.open = Some(OpenClipGroup::new(layer));
        Ok(())
    }

    /// Flush any open group onto `canvas`. Safe to call multiple times.
    pub(crate) fn finish(
        &mut self,
        canvas: &mut [u8],
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<(), crate::loader::DecodeError> {
        crate::psb_reader::check_decode_cancel(cancel)?;
        if let Some(open) = self.open.take() {
            open.finalize(canvas, self.canvas_w, self.canvas_h)?;
        }
        Ok(())
    }
}

/// Blend decoded layers bottom-to-top, honoring clipping groups.
///
/// Orphan clip layers (no base below) are skipped. Lone bases blend as usual.
pub(crate) fn blend_layers_with_clipping(
    canvas: &mut [u8],
    canvas_w: u32,
    canvas_h: u32,
    layers: &[ClipLayerRef<'_>],
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<(), crate::loader::DecodeError> {
    let mut state = ClipBlendState::new(canvas_w, canvas_h);
    for layer in layers {
        state.push_layer(canvas, layer, cancel)?;
    }
    state.finish(canvas, cancel)
}

#[cfg(test)]
mod tests {
    use super::{ClipBlendState, ClipLayerRef, apply_base_alpha_mask, blend_layers_with_clipping};

    fn solid_rgba(w: u32, h: u32, r: u8, g: u8, b: u8, a: u8) -> Vec<u8> {
        let mut v = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..w * h {
            v.extend_from_slice(&[r, g, b, a]);
        }
        v
    }

    fn px(canvas: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
        let o = ((y * w + x) * 4) as usize;
        [canvas[o], canvas[o + 1], canvas[o + 2], canvas[o + 3]]
    }

    /// Boundary alphas (incl. .5 after /255 round-trip) must keep RGB and match
    /// the GPU shader's `floor(a*255+0.5)` quantization path.
    #[test]
    fn apply_base_alpha_mask_preserves_rgb_at_half_boundaries() {
        // a=128, mask=128 -> (128*128)/255 = 64; RGB unchanged.
        let mut group = [10u8, 20, 30, 128, 40, 50, 60, 255, 70, 80, 90, 1];
        let mask = [128u8, 128, 1];
        apply_base_alpha_mask(&mut group, &mask);
        assert_eq!(&group[0..4], &[10, 20, 30, 64]);
        assert_eq!(&group[4..8], &[40, 50, 60, 128]);
        // a=1 * mask=1 / 255 = 0 -> RGB cleared.
        assert_eq!(&group[8..12], &[0, 0, 0, 0]);

        // f32 round-trip of every u8 matches CPU `f32_to_u8_round` and WGSL floor+0.5.
        for v in 0u16..=255 {
            let f = v as f32 / 255.0;
            let cpu = crate::psb_layer_blend_simd::f32_to_u8_round(f);
            let gpu_like = (f * 255.0 + 0.5).floor() as u8;
            assert_eq!(cpu, v as u8, "cpu round trip for {v}");
            assert_eq!(gpu_like, v as u8, "gpu-like round trip for {v}");
            assert_eq!(
                cpu,
                gpu_like,
                "CPU/GPU quantize diverge at {v} (contract {})",
                crate::psb_layer_blend_simd::UNIT_TO_U8_WGSL_FLOOR_BIAS
            );
        }
    }

    #[test]
    fn clipping_masks_clip_layer_to_base_alpha() {
        // Canvas 8x8. Red base 4x4 at (0,0). Blue clip 4x4 at (2,2) extends
        // past the base; with clipping, blue must only remain where base alpha > 0.
        let mut canvas = vec![0u8; 8 * 8 * 4];
        let base_rgba = solid_rgba(4, 4, 255, 0, 0, 255);
        let clip_rgba = solid_rgba(4, 4, 0, 0, 255, 255);
        let layers = [
            ClipLayerRef {
                left: 0,
                top: 0,
                width: 4,
                height: 4,
                blend: *b"norm",
                clipping: 0,
                rgba: &base_rgba,
                rgba_arc: None,
            },
            ClipLayerRef {
                left: 2,
                top: 2,
                width: 4,
                height: 4,
                blend: *b"norm",
                clipping: 1,
                rgba: &clip_rgba,
                rgba_arc: None,
            },
        ];

        blend_layers_with_clipping(&mut canvas, 8, 8, &layers, None).unwrap();

        // Overlap of base and clip -> blue (clip on top inside group).
        assert_eq!(px(&canvas, 8, 2, 2), [0, 0, 255, 255]);
        assert_eq!(px(&canvas, 8, 3, 3), [0, 0, 255, 255]);
        // Base only (no clip coverage) -> red.
        assert_eq!(px(&canvas, 8, 0, 0), [255, 0, 0, 255]);
        // Clip outside base silhouette -> transparent (the bug without clipping).
        assert_eq!(px(&canvas, 8, 4, 2), [0, 0, 0, 0]);
        assert_eq!(px(&canvas, 8, 5, 5), [0, 0, 0, 0]);
        assert_eq!(px(&canvas, 8, 2, 4), [0, 0, 0, 0]);
    }

    #[test]
    fn streaming_clip_matches_batch_api() {
        // Same two-layer fixture as clipping_masks_clip_layer_to_base_alpha.
        let base_rgba = solid_rgba(4, 4, 255, 0, 0, 255);
        let clip_rgba = solid_rgba(4, 4, 0, 0, 255, 255);
        let layers = [
            ClipLayerRef {
                left: 0,
                top: 0,
                width: 4,
                height: 4,
                blend: *b"norm",
                clipping: 0,
                rgba: &base_rgba,
                rgba_arc: None,
            },
            ClipLayerRef {
                left: 2,
                top: 2,
                width: 4,
                height: 4,
                blend: *b"norm",
                clipping: 1,
                rgba: &clip_rgba,
                rgba_arc: None,
            },
        ];

        let mut batch = vec![0u8; 8 * 8 * 4];
        blend_layers_with_clipping(&mut batch, 8, 8, &layers, None).unwrap();

        let mut stream = vec![0u8; 8 * 8 * 4];
        let mut state = ClipBlendState::new(8, 8);
        state.push_layer(&mut stream, &layers[0], None).unwrap();
        state.push_layer(&mut stream, &layers[1], None).unwrap();
        state.finish(&mut stream, None).unwrap();

        assert_eq!(batch, stream);
    }

    #[test]
    fn without_clipping_flag_clip_paints_outside_base() {
        let mut canvas = vec![0u8; 8 * 8 * 4];
        let base_rgba = solid_rgba(4, 4, 255, 0, 0, 255);
        let clip_rgba = solid_rgba(4, 4, 0, 0, 255, 255);
        let layers = [
            ClipLayerRef {
                left: 0,
                top: 0,
                width: 4,
                height: 4,
                blend: *b"norm",
                clipping: 0,
                rgba: &base_rgba,
                rgba_arc: None,
            },
            ClipLayerRef {
                left: 2,
                top: 2,
                width: 4,
                height: 4,
                blend: *b"norm",
                clipping: 0,
                rgba: &clip_rgba,
                rgba_arc: None,
            },
        ];

        blend_layers_with_clipping(&mut canvas, 8, 8, &layers, None).unwrap();

        assert_eq!(px(&canvas, 8, 4, 2), [0, 0, 255, 255]);
        assert_eq!(px(&canvas, 8, 5, 5), [0, 0, 255, 255]);
    }

    #[test]
    fn orphan_clip_layer_is_skipped() {
        let mut canvas = vec![0u8; 4 * 4 * 4];
        let clip_rgba = solid_rgba(2, 2, 0, 255, 0, 255);
        let layers = [ClipLayerRef {
            left: 0,
            top: 0,
            width: 2,
            height: 2,
            blend: *b"norm",
            clipping: 1,
            rgba: &clip_rgba,
            rgba_arc: None,
        }];

        blend_layers_with_clipping(&mut canvas, 4, 4, &layers, None).unwrap();
        assert!(canvas.iter().all(|&b| b == 0));
    }

    /// Local visual fixture from `scripts/gen_psd_clipping_fixture.py` (gitignored).
    /// Skips when the file is absent so CI stays green without the PSD.
    #[test]
    fn local_clipping_on_fixture_masks_blue_to_red_base() {
        let path = std::path::Path::new("tests/data/psd_clipping/clipping_on.psd");
        if !path.exists() {
            return;
        }
        let bytes = std::fs::read(path).expect("read clipping_on.psd");
        let composite =
            crate::psb_layer_composite::composite_layers_from_bytes_with_cancel(&bytes, None, None)
                .expect("composite clipping_on.psd");
        assert_eq!((composite.width, composite.height), (256, 256));

        let px = |x: u32, y: u32| -> [u8; 4] {
            let o = ((y * 256 + x) * 4) as usize;
            [
                composite.pixels[o],
                composite.pixels[o + 1],
                composite.pixels[o + 2],
                composite.pixels[o + 3],
            ]
        };

        // Inside red base, outside blue clip -> red.
        let r = px(50, 100);
        assert!(
            r[0] > 200 && r[1] < 80 && r[2] < 80 && r[3] > 200,
            "expected red got {r:?}"
        );

        // Overlap region -> blue-ish (clip on top).
        let o = px(100, 100);
        assert!(
            o[2] > o[0] && o[3] > 100,
            "expected blue-ish overlap got {o:?}"
        );

        // Blue clip outside red base -> must stay transparent.
        let outside = px(200, 100);
        assert_eq!(outside, [0, 0, 0, 0], "clip must not paint outside base");

        // P1 blank Image Data must degrade to P2 and still apply clipping.
        let main = crate::psb_sdr_main::decode_psd_sdr_main_from_bytes_with_cancel(
            &bytes,
            None,
            None,
            crate::settings::PsdHiddenLayerStrategy::Heuristic,
        )
        .expect("decode_psd_sdr_main clipping_on.psd");
        let o = ((100u32 * 256 + 200) * 4) as usize;
        assert_eq!(
            &main.composite.pixels[o..o + 4],
            &[0, 0, 0, 0],
            "P2 path must also mask clip outside base"
        );
    }
}
