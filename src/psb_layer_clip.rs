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
}

pub(crate) fn any_layer_clipped(layers: &[ClipLayerRef<'_>]) -> bool {
    layers.iter().any(|l| l.clipping != 0)
}

fn separable_kind(blend: &[u8; 4]) -> SeparableBlendKind {
    match blend {
        b"norm" => SeparableBlendKind::Normal,
        b"scrn" => SeparableBlendKind::Screen,
        b"lddg" => SeparableBlendKind::LinearDodge,
        b"mul " => SeparableBlendKind::Multiply,
        _ => SeparableBlendKind::Normal,
    }
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
    let span_bytes = span_w * 4;
    for sy in src_y0..src_y1 {
        let dy = (top + sy) as usize;
        let dx0 = (left + src_x0) as usize;
        let d_off = dy * canvas_w as usize * 4 + dx0 * 4;
        let s_off = sy as usize * lw as usize * 4 + src_x0 as usize * 4;
        blend_separable_span(
            &mut canvas[d_off..d_off + span_bytes],
            &layer_rgba[s_off..s_off + span_bytes],
            kind,
        );
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
        let dst_row = dy * canvas_w as usize + dx0;
        let src_row = sy as usize * base.width as usize + src_x0 as usize;
        for x in 0..row_w {
            plane[dst_row + x] = base.rgba[(src_row + x) * 4 + 3];
        }
    }
    Ok(plane)
}

/// Multiply every pixel's alpha in `group` by the corresponding base-alpha sample.
fn apply_base_alpha_mask(group: &mut [u8], base_alpha: &[u8]) {
    debug_assert_eq!(group.len(), base_alpha.len() * 4);
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

fn composite_clip_group(
    canvas: &mut [u8],
    canvas_w: u32,
    canvas_h: u32,
    group: &[ClipLayerRef<'_>],
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<(), crate::loader::DecodeError> {
    debug_assert!(!group.is_empty());
    let base = &group[0];
    if group.len() == 1 {
        blend_onto(
            canvas,
            canvas_w,
            canvas_h,
            base.rgba,
            base.left,
            base.top,
            base.width,
            base.height,
            separable_kind(&base.blend),
        );
        return Ok(());
    }

    crate::psb_reader::check_decode_cancel(cancel)?;
    let canvas_len = (canvas_w as usize)
        .checked_mul(canvas_h as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| "PSD/PSB clip group buffer size overflow".to_string())?;
    let mut temp = vec![0u8; canvas_len];
    let base_alpha = capture_base_alpha(canvas_w, canvas_h, base)?;

    // Build group content: base first (Normal into empty), then clips with their modes.
    blend_onto(
        &mut temp,
        canvas_w,
        canvas_h,
        base.rgba,
        base.left,
        base.top,
        base.width,
        base.height,
        SeparableBlendKind::Normal,
    );
    for clip in &group[1..] {
        crate::psb_reader::check_decode_cancel(cancel)?;
        blend_onto(
            &mut temp,
            canvas_w,
            canvas_h,
            clip.rgba,
            clip.left,
            clip.top,
            clip.width,
            clip.height,
            separable_kind(&clip.blend),
        );
    }

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
        separable_kind(&base.blend),
    );
    Ok(())
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
    let mut i = 0usize;
    while i < layers.len() {
        crate::psb_reader::check_decode_cancel(cancel)?;
        if layers[i].clipping != 0 {
            // Clipped with no base in the decoded stack -- invisible.
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < layers.len() && layers[j].clipping != 0 {
            j += 1;
        }
        composite_clip_group(canvas, canvas_w, canvas_h, &layers[i..j], cancel)?;
        i = j;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ClipLayerRef, blend_layers_with_clipping};

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
            },
            ClipLayerRef {
                left: 2,
                top: 2,
                width: 4,
                height: 4,
                blend: *b"norm",
                clipping: 1,
                rgba: &clip_rgba,
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
            },
            ClipLayerRef {
                left: 2,
                top: 2,
                width: 4,
                height: 4,
                blend: *b"norm",
                clipping: 0,
                rgba: &clip_rgba,
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
        let composite = crate::psb_layer_composite::composite_layers_from_bytes_with_cancel(
            &bytes, None, None,
        )
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
        assert!(r[0] > 200 && r[1] < 80 && r[2] < 80 && r[3] > 200, "expected red got {r:?}");

        // Overlap region -> blue-ish (clip on top).
        let o = px(100, 100);
        assert!(o[2] > o[0] && o[3] > 100, "expected blue-ish overlap got {o:?}");

        // Blue clip outside red base -> must stay transparent.
        let outside = px(200, 100);
        assert_eq!(outside, [0, 0, 0, 0], "clip must not paint outside base");

        // P1 blank Image Data must degrade to P2 and still apply clipping.
        let main = crate::psb_layer_composite::decode_psd_sdr_main_from_bytes_with_cancel(
            &bytes, None, None,
        )
        .expect("decode_psd_sdr_main clipping_on.psd");
        let o = ((100u32 * 256 + 200) * 4) as usize;
        assert_eq!(
            &main.pixels[o..o + 4],
            &[0, 0, 0, 0],
            "P2 path must also mask clip outside base"
        );
    }
}
