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

//! HDR-aware f32 separable blend for PSD/PSB layer compositing.
//!
//! Unlike the u8 SIMD path in `psb_layer_blend_simd`, color channels are NOT
//! clamped to 1.0 so HDR headroom above SDR white is preserved through
//! LinearDodge and Normal. Alpha is still clamped to [0, 1].

use crate::psb_layer_blend_simd::SeparableBlendKind;

/// Blend-function B(Cb, Cs) per separable mode (operates in linear-light f32).
///
/// LinearDodge does NOT apply min(1.0): values >1.0 represent HDR headroom.
/// Screen uses Cb+Cs-Cb*Cs (algebraically identical to 1-(1-Cb)(1-Cs) but
/// naturally extends to values >1 without an intermediate clamp).
#[inline]
fn blend_b_f32(kind: SeparableBlendKind, cb: f32, cs: f32) -> f32 {
    match kind {
        SeparableBlendKind::Normal => cs,
        SeparableBlendKind::Screen => cb + cs - cb * cs,
        SeparableBlendKind::LinearDodge => cb + cs,
        SeparableBlendKind::Multiply => cb * cs,
    }
}

/// Straight-alpha separable blend of `src` onto `dst` (same length, interleaved
/// RGBA f32 quads). Implements the PDF / ISO 32000 straight-alpha composite.
///
/// Color channels (R, G, B) are NOT clamped after blending so HDR values >1.0
/// are preserved. Alpha is clamped to [0, 1].
///
/// # Panics (debug)
/// Panics when `dst.len() != src.len()` or length is not a multiple of 4.
pub fn blend_separable_span_f32(dst: &mut [f32], src: &[f32], kind: SeparableBlendKind) {
    debug_assert_eq!(dst.len(), src.len());
    debug_assert!(dst.len().is_multiple_of(4));

    let n = dst.len() / 4;
    for i in 0..n {
        let off = i * 4;
        let sa = src[off + 3];
        if sa == 0.0 {
            continue;
        }
        let da = dst[off + 3];
        let out_a = sa + da * (1.0 - sa);
        if out_a <= 0.0 {
            dst[off] = 0.0;
            dst[off + 1] = 0.0;
            dst[off + 2] = 0.0;
            dst[off + 3] = 0.0;
            continue;
        }
        let inv_out_a = 1.0 / out_a;
        for c in 0..3 {
            let sc = src[off + c];
            let dc = dst[off + c];
            let b = blend_b_f32(kind, dc, sc);
            let co = sa * (1.0 - da) * sc + sa * da * b + da * (1.0 - sa) * dc;
            dst[off + c] = co * inv_out_a;
        }
        dst[off + 3] = out_a.clamp(0.0, 1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_blend_preserves_hdr_headroom() {
        // Fully opaque bright HDR src onto opaque dark dst; output must be >1.0.
        let mut dst = [0.1f32, 0.1, 0.1, 1.0];
        let src = [2.5f32, 2.5, 2.5, 1.0];
        blend_separable_span_f32(&mut dst, &src, SeparableBlendKind::Normal);
        assert!(
            dst[0] > 1.0,
            "HDR Normal blend must not clamp to 1.0, got {}",
            dst[0]
        );
        assert!(
            (dst[0] - 2.5).abs() < 1e-5,
            "opaque over opaque must equal src, got {}",
            dst[0]
        );
    }

    #[test]
    fn linear_dodge_preserves_hdr_headroom() {
        // LinearDodge adds colors: 0.8 + 0.9 = 1.7 > 1.0 (HDR headroom).
        let mut dst = [0.8f32, 0.8, 0.8, 1.0];
        let src = [0.9f32, 0.9, 0.9, 1.0];
        blend_separable_span_f32(&mut dst, &src, SeparableBlendKind::LinearDodge);
        assert!(
            dst[0] > 1.0,
            "LinearDodge must not clamp HDR result, got {}",
            dst[0]
        );
        assert!(
            (dst[0] - 1.7).abs() < 1e-4,
            "LinearDodge opaque+opaque: expected 1.7, got {}",
            dst[0]
        );
    }

    #[test]
    fn transparent_src_leaves_dst_unchanged() {
        let original = [0.5f32, 0.3, 0.7, 0.8];
        let mut dst = original;
        let src = [99.0f32, 99.0, 99.0, 0.0]; // fully transparent
        blend_separable_span_f32(&mut dst, &src, SeparableBlendKind::Normal);
        assert_eq!(dst, original, "transparent src must not modify dst");
    }

    #[test]
    fn alpha_is_clamped_to_one() {
        // Both src and dst are fully opaque; alpha output must be exactly 1.0.
        let mut dst = [0.5f32, 0.5, 0.5, 1.0];
        let src = [0.2f32, 0.2, 0.2, 1.0];
        blend_separable_span_f32(&mut dst, &src, SeparableBlendKind::Normal);
        assert!(
            dst[3] <= 1.0,
            "alpha must be clamped to 1.0, got {}",
            dst[3]
        );
        assert!(
            (dst[3] - 1.0).abs() < 1e-6,
            "alpha should be 1.0, got {}",
            dst[3]
        );
    }

    #[test]
    fn multiply_dark_produces_zero() {
        // Multiply of two fully-opaque black pixels stays black.
        let mut dst = [0.0f32, 0.0, 0.0, 1.0];
        let src = [1.0f32, 1.0, 1.0, 1.0];
        blend_separable_span_f32(&mut dst, &src, SeparableBlendKind::Multiply);
        assert!(
            (dst[0] - 0.0).abs() < 1e-6,
            "Multiply(0,1)=0, got {}",
            dst[0]
        );
    }
}
