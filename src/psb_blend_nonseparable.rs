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

//! PDF / Photoshop non-separable blend helpers (Hue / Saturation / Color / Luminosity).
//!
//! Formulas follow ISO 32000-1 (PDF) blend modes, which Photoshop uses for the
//! `hue `, `sat `, `colr`, and `lum ` layer keys. Only Color (`colr`) is wired
//! into the compositor today; the SetLum/ClipColor primitives stay shared so
//! the remaining three modes can reuse them without re-deriving ClipColor.

/// PDF luminosity weights (ISO 32000-1 Table 136).
const LUM_R: f32 = 0.3;
const LUM_G: f32 = 0.59;
const LUM_B: f32 = 0.11;

#[inline]
pub(crate) fn lum(r: f32, g: f32, b: f32) -> f32 {
    LUM_R * r + LUM_G * g + LUM_B * b
}

/// ClipColor from ISO 32000-1: scale channels around luminosity so they stay
/// in [0, 1] without changing Lum.
#[inline]
pub(crate) fn clip_color(mut r: f32, mut g: f32, mut b: f32) -> (f32, f32, f32) {
    let l = lum(r, g, b);
    let n = r.min(g).min(b);
    let x = r.max(g).max(b);
    if n < 0.0 {
        let denom = l - n;
        if denom != 0.0 {
            let scale = l / denom;
            r = l + (r - l) * scale;
            g = l + (g - l) * scale;
            b = l + (b - l) * scale;
        }
    }
    if x > 1.0 {
        let denom = x - l;
        if denom != 0.0 {
            let scale = (1.0 - l) / denom;
            r = l + (r - l) * scale;
            g = l + (g - l) * scale;
            b = l + (b - l) * scale;
        }
    }
    (r, g, b)
}

/// SetLum(C, l): shift C so its luminosity becomes `l`, then ClipColor.
#[inline]
pub(crate) fn set_lum(r: f32, g: f32, b: f32, target_lum: f32) -> (f32, f32, f32) {
    let d = target_lum - lum(r, g, b);
    clip_color(r + d, g + d, b + d)
}

/// Photoshop / PDF Color blend: `B(Cb, Cs) = SetLum(Cs, Lum(Cb))`.
///
/// Keeps backdrop luminosity and takes hue/saturation from the source. A solid
/// blue Color layer therefore tints content instead of replacing it (unlike
/// falling back to Normal, which paints opaque blue over the whole canvas).
#[inline]
pub(crate) fn blend_color_rgb(
    cb_r: f32,
    cb_g: f32,
    cb_b: f32,
    cs_r: f32,
    cs_g: f32,
    cs_b: f32,
) -> (f32, f32, f32) {
    set_lum(cs_r, cs_g, cs_b, lum(cb_r, cb_g, cb_b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_blend_preserves_backdrop_luminosity() {
        let (br, bg, bb) = (0.8, 0.5, 0.2);
        let (sr, sg, sb) = (13.0 / 255.0, 27.0 / 255.0, 130.0 / 255.0);
        let (or, og, ob) = blend_color_rgb(br, bg, bb, sr, sg, sb);
        let lum_b = lum(br, bg, bb);
        let lum_o = lum(or, og, ob);
        assert!(
            (lum_o - lum_b).abs() < 1e-4,
            "lum backdrop={lum_b} out={lum_o}"
        );
        // Output must not equal the solid source (that would be Normal).
        assert!((or - sr).abs() + (og - sg).abs() + (ob - sb).abs() > 0.2);
    }

    #[test]
    fn color_on_mid_gray_yields_source_chrominance_at_gray_lum() {
        let gray = 0.5;
        let (sr, sg, sb) = (0.1, 0.2, 0.9);
        let (or, og, ob) = blend_color_rgb(gray, gray, gray, sr, sg, sb);
        assert!((lum(or, og, ob) - gray).abs() < 1e-4);
        // Relative channel ordering of the source should survive SetLum.
        assert!(ob > og && og > or);
    }
}
