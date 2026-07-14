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

//! All PSD/PSB separable blend-mode formulas as pure f32 functions.
//!
//! Each function returns B(cb, cs) per the PDF / ISO 32000-1 (or Adobe
//! Photoshop) definition.  Callers apply the straight-alpha composite formula
//! and final quantization / clamping themselves.
//!
//! Separable modes operate independently per channel (R, G, B).  Non-separable
//! modes (Hue, Saturation, Color, Luminosity) and per-pixel modes (Darker
//! Color, Lighter Color) live in [`crate::psb_blend_nonseparable_full`].

/// Epsilon for division-by-zero protection in HDR blend SIMD paths.
///
/// Used as `max(denominator, HDR_BLEND_EPSILON)` in both u8 and f32 blend
/// kernels to guard `ColorDodge`, `ColorBurn`, `LinearBurn`, `Divide` and
/// the `co / out_a` un-premultiply step.  The value `1e-20` is small enough
/// that no practical colour value is clipped.
pub(crate) const HDR_BLEND_EPSILON: f32 = 1e-20;

// ── Darken group ──────────────────────────────────────────────────────────

/// `B(cb, cs) = min(cb, cs)`
#[inline]
pub fn blend_darken(cb: f32, cs: f32) -> f32 {
    cb.min(cs)
}

/// `B(cb, cs) = 1 - min(1, (1 - cb) / cs)` when `cs > 0`, else `0`.
///
/// Adobe "Color Burn".  The `min(1, …)` clamp prevents negative values and
/// matches the PDF / ISO 32000-1 spec (also used in the WGSL shader).
#[inline]
pub fn blend_color_burn(cb: f32, cs: f32) -> f32 {
    if cs <= 0.0 {
        0.0
    } else {
        1.0 - ((1.0 - cb) / cs).min(1.0)
    }
}

/// `B(cb, cs) = max(0, cb + cs - 1)`
///
/// Adobe "Linear Burn" (subtractive).
#[inline]
pub fn blend_linear_burn(cb: f32, cs: f32) -> f32 {
    (cb + cs - 1.0).max(0.0)
}

// ── Lighten group ─────────────────────────────────────────────────────────

/// `B(cb, cs) = max(cb, cs)`
#[inline]
pub fn blend_lighten(cb: f32, cs: f32) -> f32 {
    cb.max(cs)
}

/// `B(cb, cs) = min(1, cb / (1 - cs))` when `cs < 1`, else `1`.
///
/// Adobe "Color Dodge".  The `min(1, …)` clamp matches the PDF / ISO 32000-1
/// spec (also used in the WGSL shader).
#[inline]
pub fn blend_color_dodge(cb: f32, cs: f32) -> f32 {
    if cs >= 1.0 {
        1.0
    } else {
        (cb / (1.0 - cs)).min(1.0)
    }
}

// ── Contrast group ────────────────────────────────────────────────────────

/// `B(cb, cs)`: Vivid Light — burn when `cs <= 0.5`, dodge otherwise.
#[inline]
pub fn blend_vivid_light(cb: f32, cs: f32) -> f32 {
    if cs <= 0.5 {
        // Vivid Light burn = ColorBurn(cb, 2*cs), per PDF ISO 32000-1.
        blend_color_burn(cb, 2.0 * cs)
    } else {
        // Vivid Light dodge = ColorDodge(cb, 2*cs-1), per PDF ISO 32000-1.
        blend_color_dodge(cb, 2.0 * cs - 1.0)
    }
}

/// `B(cb, cs) = max(0, cb + 2 * cs - 1)`
///
/// Adobe "Linear Light".  The `.max(0.0)` prevents negative colour values
/// that can arise when both operands are zero or near-zero (negative colour
/// values are never meaningful, even in HDR mode where values above 1.0 are
/// intentionally preserved as headroom).
#[inline]
pub fn blend_linear_light(cb: f32, cs: f32) -> f32 {
    (cb + 2.0 * cs - 1.0).max(0.0)
}

/// `B(cb, cs)`: Pin Light — `min(cb, 2*cs)` when `cs <= 0.5`, else `max(cb, 2*cs - 1)`.
#[inline]
pub fn blend_pin_light(cb: f32, cs: f32) -> f32 {
    if cs <= 0.5 {
        cb.min(2.0 * cs)
    } else {
        cb.max(2.0 * cs - 1.0)
    }
}

/// `B(cb, cs)`: Hard Mix — `1.0` when `cb + cs >= 1.0`, else `0.0`.
///
/// **Note**: The threshold (cb + cs >= 1.0) matches the PDF/Adobe definition for
/// SDR [0, 1] values. For HDR values > 1.0 the result is always 1.0, which is
/// intentionally coarse — a more nuanced HDR behaviour would need a different
/// formula (Adobe does not define one).
#[inline]
pub fn blend_hard_mix(cb: f32, cs: f32) -> f32 {
    if cb + cs >= 1.0 { 1.0 } else { 0.0 }
}

// ── Comparative group ─────────────────────────────────────────────────────

/// `B(cb, cs) = |cb - cs|`
#[inline]
pub fn blend_difference(cb: f32, cs: f32) -> f32 {
    (cb - cs).abs()
}

/// `B(cb, cs) = cb + cs - 2 * cb * cs`
#[inline]
pub fn blend_exclusion(cb: f32, cs: f32) -> f32 {
    cb + cs - 2.0 * cb * cs
}

/// `B(cb, cs) = max(0, cb - cs)`
///
/// Adobe "Subtract".
#[inline]
pub fn blend_subtract(cb: f32, cs: f32) -> f32 {
    (cb - cs).max(0.0)
}

/// `B(cb, cs)`: Adobe "Divide" — `min(1, cb / cs)` when `cs > 0`, else `1`.
///
/// Returns `1.0` when `cs ≤ 0` (source is black or negative). This matches
/// Adobe Photoshop's behaviour: dividing by zero or a negative value cannot
/// produce a meaningful colour, so the result is treated as pure white.
/// Some alternative implementations return `cb` unchanged (no-op for zero
/// divisor), but Photoshop defaults to white, which this follows.
#[inline]
pub fn blend_divide(cb: f32, cs: f32) -> f32 {
    if cs <= 0.0 { 1.0 } else { (cb / cs).min(1.0) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    #[test]
    fn darken_min() {
        assert!(approx_eq(blend_darken(0.3, 0.7), 0.3));
        assert!(approx_eq(blend_darken(0.7, 0.3), 0.3));
    }

    #[test]
    fn lighten_max() {
        assert!(approx_eq(blend_lighten(0.3, 0.7), 0.7));
        assert!(approx_eq(blend_lighten(0.7, 0.3), 0.7));
    }

    #[test]
    fn difference_symmetry() {
        assert!(approx_eq(blend_difference(0.3, 0.7), 0.4));
        assert!(approx_eq(blend_difference(0.7, 0.3), 0.4));
        assert!(approx_eq(blend_difference(0.0, 0.0), 0.0));
        assert!(approx_eq(blend_difference(1.0, 1.0), 0.0));
    }

    #[test]
    fn exclusion_basic() {
        assert!(approx_eq(blend_exclusion(0.0, 0.0), 0.0));
        assert!(approx_eq(blend_exclusion(1.0, 1.0), 0.0));
        assert!(approx_eq(blend_exclusion(0.5, 0.5), 0.5));
    }

    #[test]
    fn subtract_never_negative() {
        assert!(approx_eq(blend_subtract(0.5, 0.3), 0.2));
        assert!(approx_eq(blend_subtract(0.3, 0.5), 0.0)); // max(0, …) clamps to 0
    }

    #[test]
    fn divide_clamp_overflow() {
        assert!(approx_eq(blend_divide(0.5, 0.5), 1.0));
        assert!(approx_eq(blend_divide(0.0, 0.5), 0.0));
        assert!(approx_eq(blend_divide(0.5, 0.0), 1.0));
        // 0.8/0.2=4 → min(1,4)=1
        assert!(approx_eq(blend_divide(0.8, 0.2), 1.0));
    }

    #[test]
    fn hard_mix_threshold() {
        assert!(approx_eq(blend_hard_mix(0.6, 0.5), 1.0));
        assert!(approx_eq(blend_hard_mix(0.2, 0.3), 0.0));
    }

    #[test]
    fn linear_burn_never_negative() {
        assert!(approx_eq(blend_linear_burn(0.3, 0.3), 0.0)); // max(0, …) clamps to 0
        assert!(approx_eq(blend_linear_burn(0.8, 0.7), 0.5));
    }

    #[test]
    fn color_burn_basic() {
        assert!(approx_eq(blend_color_burn(1.0, 0.5), 1.0));
        assert!(approx_eq(blend_color_burn(0.5, 0.0), 0.0));
        assert!(approx_eq(blend_color_burn(0.5, 1.0), 0.5));
        // (1-0.5)/0.1 = 5 → min(1,5)=1 → 1-1=0, not -4
        assert!(approx_eq(blend_color_burn(0.5, 0.1), 0.0));
    }

    #[test]
    fn color_dodge_clamp_overflow() {
        assert!(approx_eq(blend_color_dodge(0.5, 0.0), 0.5));
        assert!(approx_eq(blend_color_dodge(0.5, 1.0), 1.0));
        // 0.9/(1-0.1)=1 → min(1,1)=1
        assert!(approx_eq(blend_color_dodge(0.9, 0.1), 1.0));
    }

    #[test]
    fn vivid_light_extremes() {
        assert!(approx_eq(blend_vivid_light(0.5, 0.0), 0.0));
        assert!(approx_eq(blend_vivid_light(0.5, 1.0), 1.0));
    }

    #[test]
    fn pin_light_extremes() {
        assert!(approx_eq(blend_pin_light(0.2, 0.4), (0.2f32).min(0.8)));
        assert!(approx_eq(blend_pin_light(0.8, 0.6), (0.8f32).max(0.2)));
    }
}
