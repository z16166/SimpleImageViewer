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

use crate::psb_blend_separable::HDR_BLEND_EPSILON;
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
        SeparableBlendKind::Overlay => {
            if cb <= 0.5 {
                2.0 * cb * cs
            } else {
                1.0 - 2.0 * (1.0 - cb) * (1.0 - cs)
            }
        }
        SeparableBlendKind::SoftLight => {
            if cs <= 0.5 {
                cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb)
            } else {
                let d = if cb <= 0.25 {
                    ((16.0 * cb - 12.0) * cb + 4.0) * cb
                } else {
                    cb.sqrt()
                };
                cb + (2.0 * cs - 1.0) * (d - cb)
            }
        }
        SeparableBlendKind::HardLight => {
            if cs <= 0.5 {
                2.0 * cb * cs
            } else {
                1.0 - 2.0 * (1.0 - cb) * (1.0 - cs)
            }
        }
        // Non-separable modes (Color, Hue, Saturation, Luminosity) and
        // per-pixel modes (Darker Color, Lighter Color) are routed to the
        // scalar path in `blend_separable_span_f32` before reaching this
        // per-plane function.  If they somehow arrive here it's a logic bug.
        SeparableBlendKind::Color
        | SeparableBlendKind::Hue
        | SeparableBlendKind::Saturation
        | SeparableBlendKind::Luminosity
        | SeparableBlendKind::DarkerColor
        | SeparableBlendKind::LighterColor => {
            unreachable!("non-separable blend mode {:?} reached blend_b_f32", kind)
        }
        // ── New modes delegated to psb_blend_separable ─────────────────
        SeparableBlendKind::Darken => crate::psb_blend_separable::blend_darken(cb, cs),
        SeparableBlendKind::ColorBurn => crate::psb_blend_separable::blend_color_burn(cb, cs),
        SeparableBlendKind::LinearBurn => crate::psb_blend_separable::blend_linear_burn(cb, cs),
        SeparableBlendKind::Lighten => crate::psb_blend_separable::blend_lighten(cb, cs),
        SeparableBlendKind::ColorDodge => crate::psb_blend_separable::blend_color_dodge(cb, cs),
        SeparableBlendKind::VividLight => crate::psb_blend_separable::blend_vivid_light(cb, cs),
        SeparableBlendKind::LinearLight => crate::psb_blend_separable::blend_linear_light(cb, cs),
        SeparableBlendKind::PinLight => crate::psb_blend_separable::blend_pin_light(cb, cs),
        SeparableBlendKind::HardMix => crate::psb_blend_separable::blend_hard_mix(cb, cs),
        SeparableBlendKind::Difference => crate::psb_blend_separable::blend_difference(cb, cs),
        SeparableBlendKind::Exclusion => crate::psb_blend_separable::blend_exclusion(cb, cs),
        SeparableBlendKind::Subtract => crate::psb_blend_separable::blend_subtract(cb, cs),
        SeparableBlendKind::Divide => crate::psb_blend_separable::blend_divide(cb, cs),
        // Dissolve/PassThrough → Normal
        SeparableBlendKind::Dissolve | SeparableBlendKind::PassThrough => cs,
    }
}

/// Straight-alpha separable blend of `src` onto `dst` (same length, interleaved
/// RGBA f32 quads). Implements the PDF / ISO 32000 straight-alpha composite.
///
/// Color channels (R, G, B) are NOT clamped after blending so HDR values >1.0
/// are preserved. Alpha is clamped to [0, 1].
///
/// # Panics
/// Panics when `dst.len() != src.len()` or length is not a multiple of 4.
/// These invariants are required by the SIMD paths (pointer loads keyed off
/// `dst.len()`); release builds must not skip the checks.
pub fn blend_separable_span_f32(dst: &mut [f32], src: &[f32], kind: SeparableBlendKind) {
    assert_eq!(dst.len(), src.len());
    assert!(dst.len().is_multiple_of(4));
    if dst.is_empty() {
        return;
    }

    // Non-separable / per-pixel modes use cross-channel formulas and cannot
    // be vectorized per-plane. All other modes (all separable modes including
    // the 13 new ones) fall through to SIMD if available.
    if matches!(
        kind,
        SeparableBlendKind::Color
            | SeparableBlendKind::Hue
            | SeparableBlendKind::Saturation
            | SeparableBlendKind::Luminosity
            | SeparableBlendKind::DarkerColor
            | SeparableBlendKind::LighterColor
    ) {
        blend_separable_span_f32_scalar(dst, src, kind);
        return;
    }

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            unsafe {
                blend_separable_span_f32_avx2(dst, src, kind);
            }
            return;
        }
        if is_x86_feature_detected!("sse4.1") {
            unsafe {
                blend_separable_span_f32_sse41(dst, src, kind);
            }
            return;
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        unsafe {
            blend_separable_span_f32_neon(dst, src, kind);
        }
        return;
    }

    blend_separable_span_f32_scalar(dst, src, kind);
}

fn blend_separable_span_f32_scalar(dst: &mut [f32], src: &[f32], kind: SeparableBlendKind) {
    let n = dst.len() / 4;
    for i in 0..n {
        let off = i * 4;
        blend_one_pixel_f32(&mut dst[off..off + 4], &src[off..off + 4], kind);
    }
}

#[inline]
fn blend_one_pixel_f32(dst: &mut [f32], src: &[f32], kind: SeparableBlendKind) {
    let sa = src[3];
    if sa == 0.0 {
        return;
    }
    let da = dst[3];
    let out_a = sa + da * (1.0 - sa);
    if out_a <= 0.0 {
        dst[0] = 0.0;
        dst[1] = 0.0;
        dst[2] = 0.0;
        dst[3] = 0.0;
        return;
    }
    let inv_out_a = 1.0 / out_a;
    let (br, bg, bb) = match kind {
        SeparableBlendKind::Color => crate::psb_blend_nonseparable::blend_color_rgb(
            dst[0], dst[1], dst[2], src[0], src[1], src[2],
        ),
        SeparableBlendKind::Hue => crate::psb_blend_nonseparable_full::blend_hue_rgb(
            dst[0], dst[1], dst[2], src[0], src[1], src[2],
        ),
        SeparableBlendKind::Saturation => crate::psb_blend_nonseparable_full::blend_saturation_rgb(
            dst[0], dst[1], dst[2], src[0], src[1], src[2],
        ),
        SeparableBlendKind::Luminosity => crate::psb_blend_nonseparable_full::blend_luminosity_rgb(
            dst[0], dst[1], dst[2], src[0], src[1], src[2],
        ),
        SeparableBlendKind::DarkerColor => crate::psb_blend_nonseparable_full::darker_color_rgb(
            dst[0], dst[1], dst[2], src[0], src[1], src[2],
        ),
        SeparableBlendKind::LighterColor => crate::psb_blend_nonseparable_full::lighter_color_rgb(
            dst[0], dst[1], dst[2], src[0], src[1], src[2],
        ),
        _ => {
            // All other modes (separable) go through blend_b_f32 per channel.
            for c in 0..3 {
                let sc = src[c];
                let dc = dst[c];
                let b = blend_b_f32(kind, dc, sc);
                let co = sa * (1.0 - da) * sc + sa * da * b + da * (1.0 - sa) * dc;
                dst[c] = co * inv_out_a;
            }
            dst[3] = out_a.clamp(0.0, 1.0);
            return;
        }
    };
    // Cross-channel blend result applied with straight-alpha.
    let channels = [
        (src[0], dst[0], br),
        (src[1], dst[1], bg),
        (src[2], dst[2], bb),
    ];
    for (c, (sc, dc, b)) in channels.into_iter().enumerate() {
        let co = sa * (1.0 - da) * sc + sa * da * b + da * (1.0 - sa) * dc;
        dst[c] = co * inv_out_a;
    }
    dst[3] = out_a.clamp(0.0, 1.0);
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
#[inline]
unsafe fn load_rgba_f32x4_planes(
    ptr: *const f32,
) -> (
    core::arch::x86_64::__m128,
    core::arch::x86_64::__m128,
    core::arch::x86_64::__m128,
    core::arch::x86_64::__m128,
) {
    use core::arch::x86_64::*;
    unsafe {
        let p0 = _mm_loadu_ps(ptr);
        let p1 = _mm_loadu_ps(ptr.add(4));
        let p2 = _mm_loadu_ps(ptr.add(8));
        let t3 = _mm_loadu_ps(ptr.add(12));
        let t0 = _mm_unpacklo_ps(p0, p1);
        let t1 = _mm_unpacklo_ps(p2, t3);
        let t2 = _mm_unpackhi_ps(p0, p1);
        let t3u = _mm_unpackhi_ps(p2, t3);
        let r = _mm_movelh_ps(t0, t1);
        let g = _mm_movehl_ps(t1, t0);
        let b = _mm_movelh_ps(t2, t3u);
        let a = _mm_movehl_ps(t3u, t2);
        (r, g, b, a)
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
#[inline]
unsafe fn store_rgba_f32x4_planes(
    ptr: *mut f32,
    r: core::arch::x86_64::__m128,
    g: core::arch::x86_64::__m128,
    b: core::arch::x86_64::__m128,
    a: core::arch::x86_64::__m128,
) {
    use core::arch::x86_64::*;
    unsafe {
        let rg_lo = _mm_unpacklo_ps(r, g);
        let rg_hi = _mm_unpackhi_ps(r, g);
        let ba_lo = _mm_unpacklo_ps(b, a);
        let ba_hi = _mm_unpackhi_ps(b, a);
        let p0 = _mm_movelh_ps(rg_lo, ba_lo);
        let p1 = _mm_movehl_ps(ba_lo, rg_lo);
        let p2 = _mm_movelh_ps(rg_hi, ba_hi);
        let p3 = _mm_movehl_ps(ba_hi, rg_hi);
        _mm_storeu_ps(ptr, p0);
        _mm_storeu_ps(ptr.add(4), p1);
        _mm_storeu_ps(ptr.add(8), p2);
        _mm_storeu_ps(ptr.add(12), p3);
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn blend_plane_f32_sse41(
    sc: core::arch::x86_64::__m128,
    dc: core::arch::x86_64::__m128,
    sa: core::arch::x86_64::__m128,
    da: core::arch::x86_64::__m128,
    out_a: core::arch::x86_64::__m128,
    kind: SeparableBlendKind,
) -> core::arch::x86_64::__m128 {
    use core::arch::x86_64::*;
    let one = _mm_set1_ps(1.0);
    let zero = _mm_set1_ps(0.0);
    let v_b = match kind {
        SeparableBlendKind::Normal => sc,
        SeparableBlendKind::Multiply => _mm_mul_ps(dc, sc),
        SeparableBlendKind::Screen => _mm_sub_ps(_mm_add_ps(dc, sc), _mm_mul_ps(dc, sc)),
        SeparableBlendKind::LinearDodge => _mm_add_ps(dc, sc),
        SeparableBlendKind::Overlay => {
            let half = _mm_set1_ps(0.5);
            let two = _mm_set1_ps(2.0);
            let lo = _mm_mul_ps(_mm_mul_ps(two, dc), sc);
            let hi = _mm_sub_ps(
                one,
                _mm_mul_ps(_mm_mul_ps(two, _mm_sub_ps(one, dc)), _mm_sub_ps(one, sc)),
            );
            let mask = _mm_cmple_ps(dc, half);
            _mm_blendv_ps(hi, lo, mask)
        }
        SeparableBlendKind::SoftLight => {
            let half = _mm_set1_ps(0.5);
            let quarter = _mm_set1_ps(0.25);
            let two = _mm_set1_ps(2.0);
            // d = cb <= 0.25 ? ((16*cb - 12)*cb + 4)*cb : sqrt(cb)
            let d_poly = _mm_mul_ps(
                _mm_add_ps(
                    _mm_mul_ps(
                        _mm_sub_ps(_mm_mul_ps(_mm_set1_ps(16.0), dc), _mm_set1_ps(12.0)),
                        dc,
                    ),
                    _mm_set1_ps(4.0),
                ),
                dc,
            );
            let d_sqrt = _mm_sqrt_ps(dc);
            let cb_le_quarter = _mm_cmple_ps(dc, quarter);
            let d = _mm_blendv_ps(d_sqrt, d_poly, cb_le_quarter);
            // cs <= 0.5: cb - (1 - 2*cs) * cb * (1 - cb)
            let branch1 = _mm_sub_ps(
                dc,
                _mm_mul_ps(
                    _mm_mul_ps(_mm_sub_ps(one, _mm_mul_ps(two, sc)), dc),
                    _mm_sub_ps(one, dc),
                ),
            );
            // cs > 0.5: cb + (2*cs - 1) * (d - cb)
            let branch2 = _mm_add_ps(
                dc,
                _mm_mul_ps(_mm_sub_ps(_mm_mul_ps(two, sc), one), _mm_sub_ps(d, dc)),
            );
            let cs_le_half = _mm_cmple_ps(sc, half);
            _mm_blendv_ps(branch2, branch1, cs_le_half)
        }
        SeparableBlendKind::HardLight => {
            let half = _mm_set1_ps(0.5);
            let two = _mm_set1_ps(2.0);
            let lo = _mm_mul_ps(_mm_mul_ps(two, dc), sc);
            let hi = _mm_sub_ps(
                one,
                _mm_mul_ps(_mm_mul_ps(two, _mm_sub_ps(one, dc)), _mm_sub_ps(one, sc)),
            );
            let mask = _mm_cmple_ps(sc, half);
            _mm_blendv_ps(hi, lo, mask)
        }
        // ── New separable modes (13) ────────────────────────────────────
        SeparableBlendKind::Darken => _mm_min_ps(dc, sc),
        SeparableBlendKind::Lighten => _mm_max_ps(dc, sc),
        SeparableBlendKind::LinearBurn => {
            _mm_max_ps(_mm_add_ps(_mm_add_ps(dc, sc), _mm_set1_ps(-1.0)), zero)
        }
        SeparableBlendKind::LinearLight => {
            _mm_add_ps(dc, _mm_sub_ps(_mm_mul_ps(_mm_set1_ps(2.0), sc), one))
        }
        SeparableBlendKind::Difference => {
            let diff = _mm_sub_ps(dc, sc);
            _mm_max_ps(diff, _mm_sub_ps(zero, diff))
        }
        SeparableBlendKind::Exclusion => {
            // cb + cs - 2*cb*cs
            _mm_sub_ps(
                _mm_add_ps(dc, sc),
                _mm_mul_ps(_mm_mul_ps(_mm_set1_ps(2.0), dc), sc),
            )
        }
        SeparableBlendKind::Subtract => _mm_max_ps(_mm_sub_ps(dc, sc), zero),
        SeparableBlendKind::ColorBurn => {
            // cs > 0 ? 1 - min(1, (1-cb)/cs) : 0
            let cs_gt_zero = _mm_cmpgt_ps(sc, zero);
            let safe_cs = _mm_max_ps(sc, _mm_set1_ps(HDR_BLEND_EPSILON));
            let when_cs_gt_zero = _mm_sub_ps(
                one,
                _mm_min_ps(one, _mm_div_ps(_mm_sub_ps(one, dc), safe_cs)),
            );
            _mm_blendv_ps(zero, when_cs_gt_zero, cs_gt_zero)
        }
        SeparableBlendKind::ColorDodge => {
            // cs >= 1 ? 1 : min(1, cb/(1-cs))
            let cs_ge_one = _mm_cmpge_ps(sc, one);
            let safe_denom = _mm_max_ps(_mm_sub_ps(one, sc), _mm_set1_ps(HDR_BLEND_EPSILON));
            let when_cs_lt_one = _mm_min_ps(one, _mm_div_ps(dc, safe_denom));
            _mm_blendv_ps(when_cs_lt_one, one, cs_ge_one)
        }
        SeparableBlendKind::VividLight => {
            let half = _mm_set1_ps(0.5);
            let two = _mm_set1_ps(2.0);
            let cs_le_half = _mm_cmple_ps(sc, half);
            // burn branch: cs <= 0.5 -> color_burn(cb, 2*cs)
            let two_cs = _mm_mul_ps(two, sc);
            let cs_le_zero = _mm_cmple_ps(sc, zero);
            let safe_two_cs = _mm_max_ps(two_cs, _mm_set1_ps(HDR_BLEND_EPSILON));
            let burn = _mm_sub_ps(
                one,
                _mm_min_ps(one, _mm_div_ps(_mm_sub_ps(one, dc), safe_two_cs)),
            );
            let burn = _mm_blendv_ps(burn, zero, cs_le_zero);
            // dodge branch: cs > 0.5 -> color_dodge(cb, 2*cs-1)
            let cs_ge_one = _mm_cmpge_ps(sc, one);
            let safe_denom = _mm_max_ps(_mm_sub_ps(two, two_cs), _mm_set1_ps(HDR_BLEND_EPSILON));
            let dodge = _mm_min_ps(one, _mm_div_ps(dc, safe_denom));
            let dodge = _mm_blendv_ps(dodge, one, cs_ge_one);
            _mm_blendv_ps(dodge, burn, cs_le_half)
        }
        SeparableBlendKind::PinLight => {
            let half = _mm_set1_ps(0.5);
            let two = _mm_set1_ps(2.0);
            let two_cs = _mm_mul_ps(two, sc);
            let cs_le_half = _mm_cmple_ps(sc, half);
            let low = _mm_min_ps(dc, two_cs);
            let high = _mm_max_ps(dc, _mm_sub_ps(two_cs, one));
            _mm_blendv_ps(high, low, cs_le_half)
        }
        SeparableBlendKind::HardMix => {
            let ge_one = _mm_cmpge_ps(_mm_add_ps(dc, sc), one);
            _mm_blendv_ps(zero, one, ge_one)
        }
        SeparableBlendKind::Divide => {
            // cs > 0 ? min(1, cb/cs) : 1
            let cs_gt_zero = _mm_cmpgt_ps(sc, zero);
            let safe_cs = _mm_max_ps(sc, _mm_set1_ps(HDR_BLEND_EPSILON));
            let div_result = _mm_min_ps(one, _mm_div_ps(dc, safe_cs));
            _mm_blendv_ps(one, div_result, cs_gt_zero)
        }
        // Dissolve/PassThrough -> Normal; non-separable routed to scalar.
        SeparableBlendKind::Dissolve | SeparableBlendKind::PassThrough => sc,
        // Non-separable modes (Color, Hue, Saturation, Luminosity, DarkerColor,
        // LighterColor) are routed to scalar before reaching here.
        _ => unreachable!("non-separable modes should not reach SIMD plane blend"),
    };
    let term1 = _mm_mul_ps(_mm_mul_ps(sa, _mm_sub_ps(one, da)), sc);
    let term2 = _mm_mul_ps(_mm_mul_ps(sa, da), v_b);
    let term3 = _mm_mul_ps(_mm_mul_ps(da, _mm_sub_ps(one, sa)), dc);
    let co = _mm_add_ps(_mm_add_ps(term1, term2), term3);
    let oa_safe = _mm_max_ps(out_a, _mm_set1_ps(HDR_BLEND_EPSILON));
    let rcp = _mm_rcp_ps(oa_safe);
    let inv = _mm_mul_ps(rcp, _mm_sub_ps(_mm_set1_ps(2.0), _mm_mul_ps(oa_safe, rcp)));
    let mut out = _mm_mul_ps(co, inv);
    let sa_zero = _mm_cmpeq_ps(sa, zero);
    out = _mm_blendv_ps(out, dc, sa_zero);
    let oa_le0 = _mm_cmple_ps(out_a, zero);
    out = _mm_blendv_ps(out, zero, oa_le0);
    out
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn blend_separable_span_f32_sse41(dst: &mut [f32], src: &[f32], kind: SeparableBlendKind) {
    use core::arch::x86_64::*;
    const LANES: usize = 4;
    let n = dst.len() / 4;
    let mut i = 0usize;
    let one = _mm_set1_ps(1.0);
    let zero = _mm_set1_ps(0.0);

    while i + LANES <= n {
        let base = i * 4;
        unsafe {
            let (v_sr, v_sg, v_sb, v_sa) = load_rgba_f32x4_planes(src.as_ptr().add(base));
            let (v_dr, v_dg, v_db, v_da) = load_rgba_f32x4_planes(dst.as_ptr().add(base));
            // All-transparent src: skip the chunk.
            if _mm_movemask_ps(_mm_cmpeq_ps(v_sa, zero)) == 0xF {
                i += LANES;
                continue;
            }
            let v_out_a = _mm_add_ps(v_sa, _mm_mul_ps(v_da, _mm_sub_ps(one, v_sa)));
            let out_r = blend_plane_f32_sse41(v_sr, v_dr, v_sa, v_da, v_out_a, kind);
            let out_g = blend_plane_f32_sse41(v_sg, v_dg, v_sa, v_da, v_out_a, kind);
            let out_b = blend_plane_f32_sse41(v_sb, v_db, v_sa, v_da, v_out_a, kind);
            let out_a = _mm_min_ps(_mm_max_ps(v_out_a, zero), one);
            let oa_le0 = _mm_cmple_ps(v_out_a, zero);
            let out_a = _mm_blendv_ps(out_a, zero, oa_le0);
            let sa_zero = _mm_cmpeq_ps(v_sa, zero);
            let out_a = _mm_blendv_ps(out_a, v_da, sa_zero);
            store_rgba_f32x4_planes(dst.as_mut_ptr().add(base), out_r, out_g, out_b, out_a);
        }
        i += LANES;
    }

    while i < n {
        let off = i * 4;
        blend_one_pixel_f32(&mut dst[off..off + 4], &src[off..off + 4], kind);
        i += 1;
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2", enable = "sse4.1")]
#[inline]
unsafe fn load_rgba_f32x8_planes(
    ptr: *const f32,
) -> (
    core::arch::x86_64::__m256,
    core::arch::x86_64::__m256,
    core::arch::x86_64::__m256,
    core::arch::x86_64::__m256,
) {
    use core::arch::x86_64::*;
    unsafe {
        let (r0, g0, b0, a0) = load_rgba_f32x4_planes(ptr);
        let (r1, g1, b1, a1) = load_rgba_f32x4_planes(ptr.add(16));
        (
            _mm256_set_m128(r1, r0),
            _mm256_set_m128(g1, g0),
            _mm256_set_m128(b1, b0),
            _mm256_set_m128(a1, a0),
        )
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2", enable = "sse4.1")]
#[inline]
unsafe fn store_rgba_f32x8_planes(
    ptr: *mut f32,
    r: core::arch::x86_64::__m256,
    g: core::arch::x86_64::__m256,
    b: core::arch::x86_64::__m256,
    a: core::arch::x86_64::__m256,
) {
    use core::arch::x86_64::*;
    unsafe {
        store_rgba_f32x4_planes(
            ptr,
            _mm256_castps256_ps128(r),
            _mm256_castps256_ps128(g),
            _mm256_castps256_ps128(b),
            _mm256_castps256_ps128(a),
        );
        store_rgba_f32x4_planes(
            ptr.add(16),
            _mm256_extractf128_ps(r, 1),
            _mm256_extractf128_ps(g, 1),
            _mm256_extractf128_ps(b, 1),
            _mm256_extractf128_ps(a, 1),
        );
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn blend_plane_f32_avx2(
    sc: core::arch::x86_64::__m256,
    dc: core::arch::x86_64::__m256,
    sa: core::arch::x86_64::__m256,
    da: core::arch::x86_64::__m256,
    out_a: core::arch::x86_64::__m256,
    kind: SeparableBlendKind,
) -> core::arch::x86_64::__m256 {
    use core::arch::x86_64::*;
    let one = _mm256_set1_ps(1.0);
    let zero = _mm256_set1_ps(0.0);
    let v_b = match kind {
        SeparableBlendKind::Normal => sc,
        SeparableBlendKind::Multiply => _mm256_mul_ps(dc, sc),
        SeparableBlendKind::Screen => _mm256_sub_ps(_mm256_add_ps(dc, sc), _mm256_mul_ps(dc, sc)),
        SeparableBlendKind::LinearDodge => _mm256_add_ps(dc, sc),
        SeparableBlendKind::Overlay => {
            let half = _mm256_set1_ps(0.5);
            let two = _mm256_set1_ps(2.0);
            let lo = _mm256_mul_ps(_mm256_mul_ps(two, dc), sc);
            let hi = _mm256_sub_ps(
                one,
                _mm256_mul_ps(
                    _mm256_mul_ps(two, _mm256_sub_ps(one, dc)),
                    _mm256_sub_ps(one, sc),
                ),
            );
            let mask = _mm256_cmp_ps(dc, half, _CMP_LE_OQ);
            _mm256_blendv_ps(hi, lo, mask)
        }
        SeparableBlendKind::SoftLight => {
            let half = _mm256_set1_ps(0.5);
            let quarter = _mm256_set1_ps(0.25);
            let two = _mm256_set1_ps(2.0);
            // d = cb <= 0.25 ? ((16*cb - 12)*cb + 4)*cb : sqrt(cb)
            let d_poly = _mm256_mul_ps(
                _mm256_add_ps(
                    _mm256_mul_ps(
                        _mm256_sub_ps(
                            _mm256_mul_ps(_mm256_set1_ps(16.0), dc),
                            _mm256_set1_ps(12.0),
                        ),
                        dc,
                    ),
                    _mm256_set1_ps(4.0),
                ),
                dc,
            );
            let d_sqrt = _mm256_sqrt_ps(dc);
            let cb_le_quarter = _mm256_cmp_ps(dc, quarter, _CMP_LE_OQ);
            let d = _mm256_blendv_ps(d_sqrt, d_poly, cb_le_quarter);
            // cs <= 0.5: cb - (1 - 2*cs) * cb * (1 - cb)
            let branch1 = _mm256_sub_ps(
                dc,
                _mm256_mul_ps(
                    _mm256_mul_ps(_mm256_sub_ps(one, _mm256_mul_ps(two, sc)), dc),
                    _mm256_sub_ps(one, dc),
                ),
            );
            // cs > 0.5: cb + (2*cs - 1) * (d - cb)
            let branch2 = _mm256_add_ps(
                dc,
                _mm256_mul_ps(
                    _mm256_sub_ps(_mm256_mul_ps(two, sc), one),
                    _mm256_sub_ps(d, dc),
                ),
            );
            let cs_le_half = _mm256_cmp_ps(sc, half, _CMP_LE_OQ);
            _mm256_blendv_ps(branch2, branch1, cs_le_half)
        }
        SeparableBlendKind::HardLight => {
            let half = _mm256_set1_ps(0.5);
            let two = _mm256_set1_ps(2.0);
            let lo = _mm256_mul_ps(_mm256_mul_ps(two, dc), sc);
            let hi = _mm256_sub_ps(
                one,
                _mm256_mul_ps(
                    _mm256_mul_ps(two, _mm256_sub_ps(one, dc)),
                    _mm256_sub_ps(one, sc),
                ),
            );
            let mask = _mm256_cmp_ps(sc, half, _CMP_LE_OQ);
            _mm256_blendv_ps(hi, lo, mask)
        }
        // ── New separable modes (13) ────────────────────────────────────
        SeparableBlendKind::Darken => _mm256_min_ps(dc, sc),
        SeparableBlendKind::Lighten => _mm256_max_ps(dc, sc),
        SeparableBlendKind::LinearBurn => _mm256_max_ps(
            _mm256_add_ps(_mm256_add_ps(dc, sc), _mm256_set1_ps(-1.0)),
            zero,
        ),
        SeparableBlendKind::LinearLight => _mm256_add_ps(
            dc,
            _mm256_sub_ps(_mm256_mul_ps(_mm256_set1_ps(2.0), sc), one),
        ),
        SeparableBlendKind::Difference => {
            let diff = _mm256_sub_ps(dc, sc);
            _mm256_max_ps(diff, _mm256_sub_ps(zero, diff))
        }
        SeparableBlendKind::Exclusion => _mm256_sub_ps(
            _mm256_add_ps(dc, sc),
            _mm256_mul_ps(_mm256_mul_ps(_mm256_set1_ps(2.0), dc), sc),
        ),
        SeparableBlendKind::Subtract => _mm256_max_ps(_mm256_sub_ps(dc, sc), zero),
        SeparableBlendKind::ColorBurn => {
            let cs_gt_zero = _mm256_cmp_ps(sc, zero, _CMP_GT_OQ);
            let safe_cs = _mm256_max_ps(sc, _mm256_set1_ps(HDR_BLEND_EPSILON));
            let when_cs_gt_zero = _mm256_sub_ps(
                one,
                _mm256_min_ps(one, _mm256_div_ps(_mm256_sub_ps(one, dc), safe_cs)),
            );
            _mm256_blendv_ps(zero, when_cs_gt_zero, cs_gt_zero)
        }
        SeparableBlendKind::ColorDodge => {
            let cs_ge_one = _mm256_cmp_ps(sc, one, _CMP_GE_OQ);
            let safe_denom =
                _mm256_max_ps(_mm256_sub_ps(one, sc), _mm256_set1_ps(HDR_BLEND_EPSILON));
            let when_cs_lt_one = _mm256_min_ps(one, _mm256_div_ps(dc, safe_denom));
            _mm256_blendv_ps(when_cs_lt_one, one, cs_ge_one)
        }
        SeparableBlendKind::VividLight => {
            let half = _mm256_set1_ps(0.5);
            let two = _mm256_set1_ps(2.0);
            let cs_le_half = _mm256_cmp_ps(sc, half, _CMP_LE_OQ);
            let two_cs = _mm256_mul_ps(two, sc);
            let cs_le_zero = _mm256_cmp_ps(sc, zero, _CMP_LE_OQ);
            let safe_two_cs = _mm256_max_ps(two_cs, _mm256_set1_ps(HDR_BLEND_EPSILON));
            let burn = _mm256_sub_ps(
                one,
                _mm256_min_ps(one, _mm256_div_ps(_mm256_sub_ps(one, dc), safe_two_cs)),
            );
            let burn = _mm256_blendv_ps(burn, zero, cs_le_zero);
            let cs_ge_one = _mm256_cmp_ps(sc, one, _CMP_GE_OQ);
            let safe_denom = _mm256_max_ps(
                _mm256_sub_ps(two, two_cs),
                _mm256_set1_ps(HDR_BLEND_EPSILON),
            );
            let dodge = _mm256_min_ps(one, _mm256_div_ps(dc, safe_denom));
            let dodge = _mm256_blendv_ps(dodge, one, cs_ge_one);
            _mm256_blendv_ps(dodge, burn, cs_le_half)
        }
        SeparableBlendKind::PinLight => {
            let half = _mm256_set1_ps(0.5);
            let two = _mm256_set1_ps(2.0);
            let two_cs = _mm256_mul_ps(two, sc);
            let cs_le_half = _mm256_cmp_ps(sc, half, _CMP_LE_OQ);
            let low = _mm256_min_ps(dc, two_cs);
            let high = _mm256_max_ps(dc, _mm256_sub_ps(two_cs, one));
            _mm256_blendv_ps(high, low, cs_le_half)
        }
        SeparableBlendKind::HardMix => {
            let ge_one = _mm256_cmp_ps(_mm256_add_ps(dc, sc), one, _CMP_GE_OQ);
            _mm256_blendv_ps(zero, one, ge_one)
        }
        SeparableBlendKind::Divide => {
            let cs_gt_zero = _mm256_cmp_ps(sc, zero, _CMP_GT_OQ);
            let safe_cs = _mm256_max_ps(sc, _mm256_set1_ps(HDR_BLEND_EPSILON));
            let div_result = _mm256_min_ps(one, _mm256_div_ps(dc, safe_cs));
            _mm256_blendv_ps(one, div_result, cs_gt_zero)
        }
        // Dissolve/PassThrough -> Normal; non-separable routed to scalar.
        SeparableBlendKind::Dissolve | SeparableBlendKind::PassThrough => sc,
        // Non-separable modes (Color, Hue, Saturation, Luminosity, DarkerColor,
        // LighterColor) are routed to scalar before reaching here.
        _ => unreachable!("non-separable modes should not reach SIMD plane blend"),
    };
    let term1 = _mm256_mul_ps(_mm256_mul_ps(sa, _mm256_sub_ps(one, da)), sc);
    let term2 = _mm256_mul_ps(_mm256_mul_ps(sa, da), v_b);
    let term3 = _mm256_mul_ps(_mm256_mul_ps(da, _mm256_sub_ps(one, sa)), dc);
    let co = _mm256_add_ps(_mm256_add_ps(term1, term2), term3);
    let oa_safe = _mm256_max_ps(out_a, _mm256_set1_ps(HDR_BLEND_EPSILON));
    let rcp = _mm256_rcp_ps(oa_safe);
    let inv = _mm256_mul_ps(
        rcp,
        _mm256_sub_ps(_mm256_set1_ps(2.0), _mm256_mul_ps(oa_safe, rcp)),
    );
    let mut out = _mm256_mul_ps(co, inv);
    let sa_zero = _mm256_cmp_ps(sa, zero, _CMP_EQ_OQ);
    out = _mm256_blendv_ps(out, dc, sa_zero);
    let oa_le0 = _mm256_cmp_ps(out_a, zero, _CMP_LE_OQ);
    out = _mm256_blendv_ps(out, zero, oa_le0);
    out
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2", enable = "sse4.1")]
unsafe fn blend_separable_span_f32_avx2(dst: &mut [f32], src: &[f32], kind: SeparableBlendKind) {
    use core::arch::x86_64::*;
    const LANES: usize = 8;
    let n = dst.len() / 4;
    let mut i = 0usize;
    let one = _mm256_set1_ps(1.0);
    let zero = _mm256_set1_ps(0.0);

    while i + LANES <= n {
        let base = i * 4;
        unsafe {
            let (v_sr, v_sg, v_sb, v_sa) = load_rgba_f32x8_planes(src.as_ptr().add(base));
            let (v_dr, v_dg, v_db, v_da) = load_rgba_f32x8_planes(dst.as_ptr().add(base));
            if _mm256_movemask_ps(_mm256_cmp_ps(v_sa, zero, _CMP_EQ_OQ)) == 0xFF {
                i += LANES;
                continue;
            }
            let v_out_a = _mm256_add_ps(v_sa, _mm256_mul_ps(v_da, _mm256_sub_ps(one, v_sa)));
            let out_r = blend_plane_f32_avx2(v_sr, v_dr, v_sa, v_da, v_out_a, kind);
            let out_g = blend_plane_f32_avx2(v_sg, v_dg, v_sa, v_da, v_out_a, kind);
            let out_b = blend_plane_f32_avx2(v_sb, v_db, v_sa, v_da, v_out_a, kind);
            let out_a = _mm256_min_ps(_mm256_max_ps(v_out_a, zero), one);
            let oa_le0 = _mm256_cmp_ps(v_out_a, zero, _CMP_LE_OQ);
            let out_a = _mm256_blendv_ps(out_a, zero, oa_le0);
            let sa_zero = _mm256_cmp_ps(v_sa, zero, _CMP_EQ_OQ);
            let out_a = _mm256_blendv_ps(out_a, v_da, sa_zero);
            store_rgba_f32x8_planes(dst.as_mut_ptr().add(base), out_r, out_g, out_b, out_a);
        }
        i += LANES;
    }

    if i < n {
        unsafe {
            blend_separable_span_f32_sse41(&mut dst[i * 4..], &src[i * 4..], kind);
        }
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
#[inline]
unsafe fn load_rgba_f32x4_planes_neon(
    ptr: *const f32,
) -> (
    core::arch::aarch64::float32x4_t,
    core::arch::aarch64::float32x4_t,
    core::arch::aarch64::float32x4_t,
    core::arch::aarch64::float32x4_t,
) {
    use core::arch::aarch64::*;
    unsafe {
        let pix = vld4q_f32(ptr);
        (pix.0, pix.1, pix.2, pix.3)
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
#[inline]
unsafe fn store_rgba_f32x4_planes_neon(
    ptr: *mut f32,
    r: core::arch::aarch64::float32x4_t,
    g: core::arch::aarch64::float32x4_t,
    b: core::arch::aarch64::float32x4_t,
    a: core::arch::aarch64::float32x4_t,
) {
    use core::arch::aarch64::*;
    unsafe {
        vst4q_f32(ptr, float32x4x4_t(r, g, b, a));
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn blend_plane_f32_neon(
    sc: core::arch::aarch64::float32x4_t,
    dc: core::arch::aarch64::float32x4_t,
    sa: core::arch::aarch64::float32x4_t,
    da: core::arch::aarch64::float32x4_t,
    out_a: core::arch::aarch64::float32x4_t,
    kind: SeparableBlendKind,
) -> core::arch::aarch64::float32x4_t {
    use core::arch::aarch64::*;
    let one = vdupq_n_f32(1.0);
    let zero = vdupq_n_f32(0.0);
    let v_b = match kind {
        SeparableBlendKind::Normal => sc,
        SeparableBlendKind::Multiply => vmulq_f32(dc, sc),
        SeparableBlendKind::Screen => vsubq_f32(vaddq_f32(dc, sc), vmulq_f32(dc, sc)),
        SeparableBlendKind::LinearDodge => vaddq_f32(dc, sc),
        SeparableBlendKind::Overlay => {
            let half = vdupq_n_f32(0.5);
            let two = vdupq_n_f32(2.0);
            let lo = vmulq_f32(vmulq_f32(two, dc), sc);
            let hi = vsubq_f32(
                one,
                vmulq_f32(vmulq_f32(two, vsubq_f32(one, dc)), vsubq_f32(one, sc)),
            );
            let mask = vcleq_f32(dc, half);
            vbslq_f32(mask, lo, hi)
        }
        SeparableBlendKind::SoftLight => {
            let half = vdupq_n_f32(0.5);
            let quarter = vdupq_n_f32(0.25);
            let two = vdupq_n_f32(2.0);
            // d = cb <= 0.25 ? ((16*cb - 12)*cb + 4)*cb : sqrt(cb)
            let d_poly = vmulq_f32(
                vaddq_f32(
                    vmulq_f32(
                        vsubq_f32(vmulq_f32(vdupq_n_f32(16.0), dc), vdupq_n_f32(12.0)),
                        dc,
                    ),
                    vdupq_n_f32(4.0),
                ),
                dc,
            );
            let d_sqrt = vsqrtq_f32(dc);
            let cb_le_quarter = vcleq_f32(dc, quarter);
            let d = vbslq_f32(cb_le_quarter, d_poly, d_sqrt);
            // cs <= 0.5: cb - (1 - 2*cs) * cb * (1 - cb)
            let branch1 = vsubq_f32(
                dc,
                vmulq_f32(
                    vmulq_f32(vsubq_f32(one, vmulq_f32(two, cs)), dc),
                    vsubq_f32(one, dc),
                ),
            );
            // cs > 0.5: cb + (2*cs - 1) * (d - cb)
            let branch2 = vaddq_f32(
                dc,
                vmulq_f32(vsubq_f32(vmulq_f32(two, cs), one), vsubq_f32(d, dc)),
            );
            let cs_le_half = vcleq_f32(cs, half);
            vbslq_f32(cs_le_half, branch1, branch2)
        }
        SeparableBlendKind::HardLight => {
            let half = vdupq_n_f32(0.5);
            let two = vdupq_n_f32(2.0);
            let lo = vmulq_f32(vmulq_f32(two, dc), sc);
            let hi = vsubq_f32(
                one,
                vmulq_f32(vmulq_f32(two, vsubq_f32(one, dc)), vsubq_f32(one, sc)),
            );
            let mask = vcleq_f32(cs, half);
            vbslq_f32(mask, lo, hi)
        }
        // ── New separable modes (13) ────────────────────────────────────
        SeparableBlendKind::Darken => vminq_f32(dc, sc),
        SeparableBlendKind::Lighten => vmaxq_f32(dc, sc),
        SeparableBlendKind::LinearBurn => {
            vmaxq_f32(vaddq_f32(vaddq_f32(dc, sc), vdupq_n_f32(-1.0)), zero)
        }
        SeparableBlendKind::LinearLight => {
            vaddq_f32(dc, vsubq_f32(vmulq_f32(vdupq_n_f32(2.0), sc), one))
        }
        SeparableBlendKind::Difference => vabsq_f32(vsubq_f32(dc, sc)),
        SeparableBlendKind::Exclusion => {
            // cb + cs - 2*cb*cs
            vsubq_f32(
                vaddq_f32(dc, sc),
                vmulq_f32(vmulq_f32(vdupq_n_f32(2.0), dc), sc),
            )
        }
        SeparableBlendKind::Subtract => vmaxq_f32(vsubq_f32(dc, sc), zero),
        SeparableBlendKind::ColorBurn => {
            // cs > 0 ? 1 - min(1, (1-cb)/cs) : 0
            let cs_gt_zero = vcgtq_f32(sc, zero);
            let safe_cs = vmaxq_f32(sc, vdupq_n_f32(HDR_BLEND_EPSILON));
            let when_cs_gt_zero =
                vsubq_f32(one, vminq_f32(one, vdivq_f32(vsubq_f32(one, dc), safe_cs)));
            vbslq_f32(cs_gt_zero, when_cs_gt_zero, zero)
        }
        SeparableBlendKind::ColorDodge => {
            // cs >= 1 ? 1 : min(1, cb/(1-cs))
            let cs_ge_one = vcgeq_f32(sc, one);
            let safe_denom = vmaxq_f32(vsubq_f32(one, sc), vdupq_n_f32(HDR_BLEND_EPSILON));
            let when_cs_lt_one = vminq_f32(one, vdivq_f32(dc, safe_denom));
            vbslq_f32(cs_ge_one, one, when_cs_lt_one)
        }
        SeparableBlendKind::VividLight => {
            let half = vdupq_n_f32(0.5);
            let two = vdupq_n_f32(2.0);
            let cs_le_half = vcleq_f32(sc, half);
            let two_cs = vmulq_f32(two, sc);
            let cs_le_zero = vcleq_f32(sc, zero);
            let safe_two_cs = vmaxq_f32(two_cs, vdupq_n_f32(HDR_BLEND_EPSILON));
            let burn = vsubq_f32(
                one,
                vminq_f32(one, vdivq_f32(vsubq_f32(one, dc), safe_two_cs)),
            );
            let burn = vbslq_f32(cs_le_zero, zero, burn);
            let cs_ge_one = vcgeq_f32(sc, one);
            let safe_denom = vmaxq_f32(vsubq_f32(two, two_cs), vdupq_n_f32(HDR_BLEND_EPSILON));
            let dodge = vminq_f32(one, vdivq_f32(dc, safe_denom));
            let dodge = vbslq_f32(cs_ge_one, one, dodge);
            vbslq_f32(cs_le_half, burn, dodge)
        }
        SeparableBlendKind::PinLight => {
            let half = vdupq_n_f32(0.5);
            let two = vdupq_n_f32(2.0);
            let two_cs = vmulq_f32(two, sc);
            let cs_le_half = vcleq_f32(sc, half);
            let low = vminq_f32(dc, two_cs);
            let high = vmaxq_f32(dc, vsubq_f32(two_cs, one));
            vbslq_f32(cs_le_half, low, high)
        }
        SeparableBlendKind::HardMix => {
            let ge_one = vcgeq_f32(vaddq_f32(dc, sc), one);
            vbslq_f32(ge_one, one, zero)
        }
        SeparableBlendKind::Divide => {
            // cs > 0 ? min(1, cb/cs) : 1
            let cs_gt_zero = vcgtq_f32(sc, zero);
            let safe_cs = vmaxq_f32(sc, vdupq_n_f32(HDR_BLEND_EPSILON));
            let div_result = vminq_f32(one, vdivq_f32(dc, safe_cs));
            vbslq_f32(cs_gt_zero, div_result, one)
        }
        // Dissolve/PassThrough -> Normal; non-separable routed to scalar.
        SeparableBlendKind::Dissolve | SeparableBlendKind::PassThrough => sc,
        // Non-separable modes (Color, Hue, Saturation, Luminosity, DarkerColor,
        // LighterColor) are routed to scalar before reaching here.
        _ => unreachable!("non-separable modes should not reach SIMD plane blend"),
    };
    let term1 = vmulq_f32(vmulq_f32(sa, vsubq_f32(one, da)), sc);
    let term2 = vmulq_f32(vmulq_f32(sa, da), v_b);
    let term3 = vmulq_f32(vmulq_f32(da, vsubq_f32(one, sa)), dc);
    let co = vaddq_f32(vaddq_f32(term1, term2), term3);
    let oa_safe = vmaxq_f32(out_a, vdupq_n_f32(HDR_BLEND_EPSILON));
    let mut out = vdivq_f32(co, oa_safe);
    let sa_zero = vceqq_f32(sa, zero);
    out = vbslq_f32(sa_zero, dc, out);
    let oa_le0 = vcleq_f32(out_a, zero);
    out = vbslq_f32(oa_le0, zero, out);
    out
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn blend_separable_span_f32_neon(dst: &mut [f32], src: &[f32], kind: SeparableBlendKind) {
    use core::arch::aarch64::*;
    const LANES: usize = 4;
    let n = dst.len() / 4;
    let mut i = 0usize;
    let one = vdupq_n_f32(1.0);
    let zero = vdupq_n_f32(0.0);

    while i + LANES <= n {
        let base = i * 4;
        unsafe {
            let (v_sr, v_sg, v_sb, v_sa) = load_rgba_f32x4_planes_neon(src.as_ptr().add(base));
            let (v_dr, v_dg, v_db, v_da) = load_rgba_f32x4_planes_neon(dst.as_ptr().add(base));
            let sa_zero = vceqq_f32(v_sa, zero);
            // Skip when every lane is transparent.
            if vminvq_u32(sa_zero) == 0xFFFF_FFFF {
                i += LANES;
                continue;
            }
            let v_out_a = vaddq_f32(v_sa, vmulq_f32(v_da, vsubq_f32(one, v_sa)));
            let out_r = blend_plane_f32_neon(v_sr, v_dr, v_sa, v_da, v_out_a, kind);
            let out_g = blend_plane_f32_neon(v_sg, v_dg, v_sa, v_da, v_out_a, kind);
            let out_b = blend_plane_f32_neon(v_sb, v_db, v_sa, v_da, v_out_a, kind);
            let mut out_a = vminq_f32(vmaxq_f32(v_out_a, zero), one);
            let oa_le0 = vcleq_f32(v_out_a, zero);
            out_a = vbslq_f32(oa_le0, zero, out_a);
            out_a = vbslq_f32(sa_zero, v_da, out_a);
            store_rgba_f32x4_planes_neon(dst.as_mut_ptr().add(base), out_r, out_g, out_b, out_a);
        }
        i += LANES;
    }

    while i < n {
        let off = i * 4;
        blend_one_pixel_f32(&mut dst[off..off + 4], &src[off..off + 4], kind);
        i += 1;
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

    #[test]
    fn simd_matches_scalar_across_modes() {
        for kind in [
            SeparableBlendKind::Normal,
            SeparableBlendKind::Screen,
            SeparableBlendKind::LinearDodge,
            SeparableBlendKind::Multiply,
            SeparableBlendKind::Overlay,
            SeparableBlendKind::SoftLight,
            SeparableBlendKind::HardLight,
            // ── 13 new separable modes ──
            SeparableBlendKind::Darken,
            SeparableBlendKind::Lighten,
            SeparableBlendKind::LinearBurn,
            SeparableBlendKind::LinearLight,
            SeparableBlendKind::Difference,
            SeparableBlendKind::Exclusion,
            SeparableBlendKind::Subtract,
            SeparableBlendKind::ColorBurn,
            SeparableBlendKind::ColorDodge,
            SeparableBlendKind::VividLight,
            SeparableBlendKind::PinLight,
            SeparableBlendKind::HardMix,
            SeparableBlendKind::Divide,
            // Dissolve/PassThrough also use SIMD (treated as Normal).
            SeparableBlendKind::Dissolve,
            SeparableBlendKind::PassThrough,
        ] {
            let mut dst_simd = [
                0.1f32, 0.2, 0.3, 1.0, 0.5, 0.5, 0.5, 0.5, 2.0, 1.5, 0.8, 1.0, 0.0, 0.0, 0.0, 0.0,
                0.9, 0.1, 0.2, 0.25, 1.2, 0.3, 0.4, 0.75, 0.05, 0.05, 0.05, 1.0, 0.7, 0.8, 0.9,
                0.1,
            ];
            let mut dst_ref = dst_simd;
            let src = [
                0.4f32, 0.5, 0.6, 0.5, 1.5, 1.5, 1.5, 1.0, 0.0, 0.0, 0.0, 0.0, 0.2, 0.3, 0.4, 0.8,
                0.1, 0.2, 0.3, 1.0, 0.0, 1.0, 0.0, 0.5, 3.0, 2.0, 1.0, 0.3, 0.5, 0.5, 0.5, 0.0,
            ];
            blend_separable_span_f32(&mut dst_simd, &src, kind);
            blend_separable_span_f32_scalar(&mut dst_ref, &src, kind);
            for (i, (a, b)) in dst_simd.iter().zip(dst_ref.iter()).enumerate() {
                assert!(
                    (a - b).abs() < 1e-4,
                    "mismatch at {i} for {kind:?}: simd={a} scalar={b}"
                );
            }
        }
    }
}
