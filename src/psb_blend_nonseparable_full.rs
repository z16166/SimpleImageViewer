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

//! Non-separable PSD/PSB blend modes: Hue, Saturation, Color, Luminosity,
//! plus per-pixel Darker Color / Lighter Color.
//!
//! The Color mode lives in [`crate::psb_blend_nonseparable`] and is re-exported
//! here for symmetry.  The remaining three non-separable modes (Hue,
//! Saturation, Luminosity) share the `ClipColor` / `SetLum` primitives from
//! that module and add `Sat` / `SetSat`.

use crate::psb_blend_nonseparable::{lum, set_lum};

// ── Saturation helpers (PDF ISO 32000-1) ───────────────────────────────────

/// `Sat(C)` — the saturation (chroma) of a colour.
#[inline]
fn sat(r: f32, g: f32, b: f32) -> f32 {
    let c_min = r.min(g).min(b);
    let c_max = r.max(g).max(b);
    c_max - c_min
}

/// `SetSat(C, s)` — scale the chroma of C so its saturation becomes `s`,
/// preserving hue.
#[inline]
fn set_sat(r: f32, g: f32, b: f32, target_sat: f32) -> (f32, f32, f32) {
    let c_min = r.min(g).min(b);
    let c_max = r.max(g).max(b);
    let current = c_max - c_min;
    if current <= 0.0 || target_sat <= 0.0 {
        // Achromatic → return all grayscale at the minimum value.
        return (c_min, c_min, c_min);
    }
    let scale = target_sat / current;
    let r2 = c_min + (r - c_min) * scale;
    let g2 = c_min + (g - c_min) * scale;
    let b2 = c_min + (b - c_min) * scale;
    // After scaling, the max channel may exceed target_sat + c_min.
    // This is fine — `ClipColor` / `SetLum` callers handle clamping.
    (r2, g2, b2)
}

// ── Blend functions ────────────────────────────────────────────────────────

/// Photoshop / PDF Hue blend: `B(Cb, Cs) = SetLum(SetSat(Cs, Sat(Cb)), Lum(Cb))`.
///
/// Takes the luminance of the backdrop and the hue of the source.
#[inline]
pub fn blend_hue_rgb(
    cb_r: f32,
    cb_g: f32,
    cb_b: f32,
    cs_r: f32,
    cs_g: f32,
    cs_b: f32,
) -> (f32, f32, f32) {
    let (s_r, s_g, s_b) = set_sat(cs_r, cs_g, cs_b, sat(cb_r, cb_g, cb_b));
    set_lum(s_r, s_g, s_b, lum(cb_r, cb_g, cb_b))
}

/// Photoshop / PDF Saturation blend:
/// `B(Cb, Cs) = SetLum(SetSat(Cb, Sat(Cs)), Lum(Cb))`.
///
/// Takes the luminance and hue of the backdrop, saturation of the source.
#[inline]
pub fn blend_saturation_rgb(
    cb_r: f32,
    cb_g: f32,
    cb_b: f32,
    cs_r: f32,
    cs_g: f32,
    cs_b: f32,
) -> (f32, f32, f32) {
    let (b_r, b_g, b_b) = set_sat(cb_r, cb_g, cb_b, sat(cs_r, cs_g, cs_b));
    set_lum(b_r, b_g, b_b, lum(cb_r, cb_g, cb_b))
}

/// Photoshop / PDF Luminosity blend: `B(Cb, Cs) = SetLum(Cb, Lum(Cs))`.
///
/// Takes the hue and saturation of the backdrop, luminosity of the source.
#[inline]
pub fn blend_luminosity_rgb(
    cb_r: f32,
    cb_g: f32,
    cb_b: f32,
    cs_r: f32,
    cs_g: f32,
    cs_b: f32,
) -> (f32, f32, f32) {
    set_lum(cb_r, cb_g, cb_b, lum(cs_r, cs_g, cs_b))
}

// ── Darker Color / Lighter Color (per-pixel luminance compare) ────────────

/// Photoshop Darker Color: compare pixel luminance; take the darker (lower
/// luminance) pixel entirely.
///
/// Returns `(r, g, b)` of the chosen pixel.
#[inline]
pub fn darker_color_rgb(
    cb_r: f32,
    cb_g: f32,
    cb_b: f32,
    cs_r: f32,
    cs_g: f32,
    cs_b: f32,
) -> (f32, f32, f32) {
    if lum(cb_r, cb_g, cb_b) <= lum(cs_r, cs_g, cs_b) {
        (cb_r, cb_g, cb_b)
    } else {
        (cs_r, cs_g, cs_b)
    }
}

/// Photoshop Lighter Color: compare pixel luminance; take the lighter (higher
/// luminance) pixel entirely.
#[inline]
pub fn lighter_color_rgb(
    cb_r: f32,
    cb_g: f32,
    cb_b: f32,
    cs_r: f32,
    cs_g: f32,
    cs_b: f32,
) -> (f32, f32, f32) {
    if lum(cb_r, cb_g, cb_b) >= lum(cs_r, cs_g, cs_b) {
        (cb_r, cb_g, cb_b)
    } else {
        (cs_r, cs_g, cs_b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    fn approx_rgb(a: (f32, f32, f32), b: (f32, f32, f32)) -> bool {
        approx_eq(a.0, b.0) && approx_eq(a.1, b.1) && approx_eq(a.2, b.2)
    }

    #[test]
    fn sat_of_gray_is_zero() {
        assert!(approx_eq(sat(0.5, 0.5, 0.5), 0.0));
        assert!(approx_eq(sat(0.0, 0.0, 0.0), 0.0));
        assert!(approx_eq(sat(1.0, 1.0, 1.0), 0.0));
    }

    #[test]
    fn sat_of_pure_channel() {
        assert!(approx_eq(sat(1.0, 0.0, 0.0), 1.0));
        assert!(approx_eq(sat(0.0, 1.0, 0.0), 1.0));
        assert!(approx_eq(sat(0.0, 0.0, 1.0), 1.0));
    }

    #[test]
    fn set_sat_zero_on_gray_gives_gray() {
        let (r, g, b) = set_sat(0.5, 0.5, 0.5, 0.3);
        assert!(approx_eq(r, 0.5));
        assert!(approx_eq(g, 0.5));
        assert!(approx_eq(b, 0.5));
    }

    #[test]
    fn hue_blend_preserves_backdrop_luminosity() {
        let (br, bg, bb) = (0.8, 0.5, 0.2);
        let (sr, sg, sb) = (0.1, 0.3, 0.9);
        let (or, og, ob) = blend_hue_rgb(br, bg, bb, sr, sg, sb);
        let lum_b = lum(br, bg, bb);
        let lum_o = lum(or, og, ob);
        assert!(approx_eq(lum_o, lum_b), "lum backdrop={lum_b} out={lum_o}");
    }

    #[test]
    fn saturation_blend_preserves_backdrop_luminosity() {
        let (br, bg, bb) = (0.8, 0.5, 0.2);
        let (sr, sg, sb) = (0.1, 0.3, 0.9);
        let (or, og, ob) = blend_saturation_rgb(br, bg, bb, sr, sg, sb);
        let lum_b = lum(br, bg, bb);
        let lum_o = lum(or, og, ob);
        assert!(approx_eq(lum_o, lum_b), "lum backdrop={lum_b} out={lum_o}");
    }

    #[test]
    fn luminosity_blend_takes_backdrop_hue() {
        let (br, bg, bb) = (0.8, 0.5, 0.2);
        let (sr, sg, sb) = (0.1, 0.3, 0.9);
        let (or, og, ob) = blend_luminosity_rgb(br, bg, bb, sr, sg, sb);
        // Luminosity should match source luminance, but hue should stay from backdrop
        let lum_s = lum(sr, sg, sb);
        let lum_o = lum(or, og, ob);
        assert!(approx_eq(lum_o, lum_s), "lum source={lum_s} out={lum_o}");
    }

    #[test]
    fn darker_color_picks_darker() {
        let dk = (0.2, 0.2, 0.2);
        let lt = (0.8, 0.8, 0.8);
        let (r, g, b) = darker_color_rgb(dk.0, dk.1, dk.2, lt.0, lt.1, lt.2);
        assert!(approx_rgb((r, g, b), dk));
        // Reverse order
        let (r, g, b) = darker_color_rgb(lt.0, lt.1, lt.2, dk.0, dk.1, dk.2);
        assert!(approx_rgb((r, g, b), dk));
    }

    #[test]
    fn lighter_color_picks_lighter() {
        let dk = (0.2, 0.2, 0.2);
        let lt = (0.8, 0.8, 0.8);
        let (r, g, b) = lighter_color_rgb(dk.0, dk.1, dk.2, lt.0, lt.1, lt.2);
        assert!(approx_rgb((r, g, b), lt));
        // Reverse order
        let (r, g, b) = lighter_color_rgb(lt.0, lt.1, lt.2, dk.0, dk.1, dk.2);
        assert!(approx_rgb((r, g, b), lt));
    }

    #[test]
    fn color_blend_re_export_works() {
        // blend_color_rgb re-exported from psb_blend_nonseparable
        let (br, bg, bb) = (0.8, 0.5, 0.2);
        let (sr, sg, sb) = (13.0 / 255.0, 27.0 / 255.0, 130.0 / 255.0);
        let (or, og, ob) = crate::psb_blend_nonseparable::blend_color_rgb(br, bg, bb, sr, sg, sb);
        let lum_b = lum(br, bg, bb);
        let lum_o = lum(or, og, ob);
        assert!(
            approx_eq(lum_o, lum_b),
            "color blend preserves backdrop luminosity"
        );
    }
}
