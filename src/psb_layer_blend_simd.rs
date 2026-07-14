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

//! SIMD straight-alpha separable blend with all 28 blend modes.
//!
//! Normal / Screen / Linear Dodge / Multiply / Overlay / Soft Light /
//! Hard Light are SIMD-accelerated via explicit SSE2/AVX2/NEON kernels
//! processing 4 or 8 pixels per iteration.
//! The remaining separable modes (Darken, Lighten, ColorBurn, ColorDodge,
//! LinearBurn, LinearLight, VividLight, PinLight, HardMix, Difference,
//! Exclusion, Subtract, Divide) fall through to a per-pixel scalar path
//! in [`crate::psb_blend_separable`].
//!
//! HDR f32 blending has its own SIMD kernels in [`crate::psb_hdr_blend`].
//!
//! Final u8 conversion uses the same `round()` path as the scalar reference
//! so results stay bit-identical to the per-pixel f32 loop.
//!
//! Note: Normal-mode integer SIMD (avoiding u8<->f32) is a possible follow-up,
//! but must stay bit-identical to the f32 `round()` reference for partial alpha;
//! opaque Normal already uses a memcpy fast path.

/// Photoshop / PDF separable blend mode for a horizontal RGBA8 span.
///
/// Newer modes (added in bulk for full PSD/PSB coverage) delegate per-channel
/// formulas to [`crate::psb_blend_separable`] and non-separable / per-pixel
/// modes to [`crate::psb_blend_nonseparable_full`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeparableBlendKind {
    // ── Original four (SIMD-accelerated) ───────────────────────────────
    Normal,
    Screen,
    LinearDodge,
    Multiply,
    // ── Overlay / Soft Light / Hard Light (SIMD-accelerated) ───────────
    /// Photoshop `over` (Overlay). SIMD-accelerated via SSE2/AVX2/NEON.
    Overlay,
    /// Photoshop `sLit` (Soft Light). SIMD-accelerated via SSE2/AVX2/NEON.
    SoftLight,
    /// Photoshop `hLit` (Hard Light). SIMD-accelerated via SSE2/AVX2/NEON.
    HardLight,
    // ── Non-separable (scalar-only) ────────────────────────────────────
    /// Photoshop `colr` (Color). Non-separable; scalar path only.
    ///
    /// Must not fall back to [`Self::Normal`]: a full-canvas solid Color fill
    /// would otherwise paint opaque blue/brand fills over the whole document.
    Color,
    /// Photoshop `hue ` (Hue). Non-separable; scalar path only.
    Hue,
    /// Photoshop `sat ` (Saturation). Non-separable; scalar path only.
    Saturation,
    /// Photoshop `lum ` (Luminosity). Non-separable; scalar path only.
    Luminosity,
    // ── Darken group (separable, scalar-only) ──────────────────────────
    Darken,
    ColorBurn,
    LinearBurn,
    /// Photoshop `dkCl` (Darker Color) — per-pixel luminance compare.
    DarkerColor,
    // ── Lighten group (separable, scalar-only) ─────────────────────────
    Lighten,
    ColorDodge,
    /// Photoshop `lgCl` (Lighter Color) — per-pixel luminance compare.
    LighterColor,
    // ── Contrast group (separable, scalar-only) ────────────────────────
    VividLight,
    LinearLight,
    PinLight,
    HardMix,
    // ── Comparative group (separable, scalar-only) ─────────────────────
    Difference,
    Exclusion,
    Subtract,
    Divide,
    // ── Special (treated as Normal) ────────────────────────────────────
    /// `diss` (Dissolve) — stochastic alpha dither; not implemented, treated
    /// as Normal (dissolve requires random dithering we do not support).
    Dissolve,
    /// `pass` (Pass Through) — layer-group pass-through.  Treated as Normal
    /// in flat bottom‑up compositing (no group isolation boundary to preserve).
    PassThrough,
}

impl SeparableBlendKind {
    /// Map a Photoshop 4-byte blend-mode key to a supported separable kind.
    /// Unsupported keys return `None` (callers that only need a fallback can use
    /// [`Self::from_psd_key_or_normal`]).
    #[inline]
    pub fn from_psd_key(blend: &[u8; 4]) -> Option<Self> {
        match blend {
            b"norm" => Some(Self::Normal),
            b"scrn" => Some(Self::Screen),
            b"lddg" => Some(Self::LinearDodge),
            b"mul " => Some(Self::Multiply),
            b"over" => Some(Self::Overlay),
            b"sLit" => Some(Self::SoftLight),
            b"hLit" => Some(Self::HardLight),
            b"colr" => Some(Self::Color),
            b"hue " => Some(Self::Hue),
            b"sat " => Some(Self::Saturation),
            b"lum " => Some(Self::Luminosity),
            b"dark" => Some(Self::Darken),
            b"idiv" => Some(Self::ColorBurn),
            b"lbrn" => Some(Self::LinearBurn),
            b"dkCl" => Some(Self::DarkerColor),
            b"lite" => Some(Self::Lighten),
            b"div " => Some(Self::ColorDodge),
            b"lgCl" => Some(Self::LighterColor),
            b"vLit" => Some(Self::VividLight),
            b"lLit" => Some(Self::LinearLight),
            b"pLit" => Some(Self::PinLight),
            b"hMix" => Some(Self::HardMix),
            b"diff" => Some(Self::Difference),
            b"excl" => Some(Self::Exclusion),
            b"subt" => Some(Self::Subtract),
            b"fdiv" => Some(Self::Divide),
            b"diss" => Some(Self::Dissolve),
            b"pass" => Some(Self::PassThrough),
            _ => None,
        }
    }

    /// Like [`Self::from_psd_key`], falling back to [`Self::Normal`] for unknown
    /// keys after a one-time debug log (never silent).
    #[inline]
    pub fn from_psd_key_or_normal(blend: &[u8; 4]) -> Self {
        match Self::from_psd_key(blend) {
            Some(kind) => kind,
            None => {
                log_unsupported_blend_once(blend);
                Self::Normal
            }
        }
    }

    /// True when this kind has a dedicated SIMD kernel (others use scalar).
    #[inline]
    pub(crate) fn has_simd_kernel(self) -> bool {
        matches!(
            self,
            Self::Normal
                | Self::Screen
                | Self::LinearDodge
                | Self::Multiply
                | Self::Overlay
                | Self::SoftLight
                | Self::HardLight
        )
    }
}

/// Log an unsupported blend-mode key once (unsupported modes fall back to Normal).
pub(crate) fn log_unsupported_blend_once(blend: &[u8; 4]) {
    static SEEN: std::sync::OnceLock<parking_lot::Mutex<std::collections::HashSet<[u8; 4]>>> =
        std::sync::OnceLock::new();
    let seen = SEEN.get_or_init(|| parking_lot::Mutex::new(std::collections::HashSet::new()));
    let mut seen = seen.lock();
    if seen.insert(*blend) {
        let key = String::from_utf8_lossy(blend).into_owned();
        log::debug!("PSD/PSB layer composite: unsupported blend mode '{key}', treating as Normal");
    }
}

#[inline]
fn u8_to_f32(v: u8) -> f32 {
    // Match former LUT formula `(i as f32) / 255.0` (not `* (1/255)`).
    (v as f32) / 255.0
}

/// Quantize a unit-interval float to `u8` with round-half-away-from-zero.
///
/// Contract shared with GPU `cs_apply_base_alpha_mask` / separable blend store:
/// Rust `f32::round` on non-negative values matches WGSL `floor(x * 255.0 + 0.5)`
/// (WGSL `round` is ties-to-even and must not be used for this path).
/// See [`UNIT_TO_U8_WGSL_FLOOR_BIAS`].
#[inline]
pub(crate) fn f32_to_u8_round(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// WGSL expression that must stay bit-aligned with [`f32_to_u8_round`] for
/// non-negative clamped inputs (shader string / review checklist 22).
#[allow(dead_code)] // used from binary crate tests, not from lib
pub(crate) const UNIT_TO_U8_WGSL_FLOOR_BIAS: &str = "floor(x * 255.0 + 0.5)";

#[inline]
fn blend_b(kind: SeparableBlendKind, cb: f32, cs: f32) -> f32 {
    match kind {
        SeparableBlendKind::Normal => cs,
        SeparableBlendKind::Screen => 1.0 - (1.0 - cb) * (1.0 - cs),
        SeparableBlendKind::LinearDodge => (cb + cs).min(1.0),
        SeparableBlendKind::Multiply => cb * cs,
        SeparableBlendKind::Overlay => {
            if cb <= 0.5 {
                2.0 * cb * cs
            } else {
                1.0 - 2.0 * (1.0 - cb) * (1.0 - cs)
            }
        }
        SeparableBlendKind::SoftLight => {
            // PDF soft-light (Photoshop): uses the D(cb) branch for cs > 0.5.
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
        // Non-separable: handled in `blend_one_pixel` via cross-channel blend
        // functions; per-channel callers must not reach here.
        SeparableBlendKind::Color
        | SeparableBlendKind::Hue
        | SeparableBlendKind::Saturation
        | SeparableBlendKind::Luminosity
        | SeparableBlendKind::DarkerColor
        | SeparableBlendKind::LighterColor => cs,
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

/// Straight-alpha separable blend of `src` onto `dst` (same length, RGBA8).
pub fn blend_separable_span(dst: &mut [u8], src: &[u8], kind: SeparableBlendKind) {
    assert_eq!(dst.len(), src.len());
    assert!(dst.len().is_multiple_of(4));
    if dst.is_empty() {
        return;
    }

    // Color and other non-separable modes are scalar only (SIMD kernels
    // cover Normal, Screen, LinearDodge, Multiply, Overlay, SoftLight,
    // HardLight).
    if !kind.has_simd_kernel() {
        blend_separable_span_scalar(dst, src, kind);
        return;
    }

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            unsafe {
                blend_separable_span_avx2(dst, src, kind);
            }
            return;
        }
        if is_x86_feature_detected!("sse2") {
            unsafe {
                blend_separable_span_sse2(dst, src, kind);
            }
            return;
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        unsafe {
            blend_separable_span_neon(dst, src, kind);
        }
        return;
    }

    #[cfg(not(target_arch = "aarch64"))]
    {
        blend_separable_span_scalar(dst, src, kind);
    }
}

fn blend_separable_span_scalar(dst: &mut [u8], src: &[u8], kind: SeparableBlendKind) {
    let is_normal = kind == SeparableBlendKind::Normal;
    // as_chunks avoids per-pixel slice bounds checks on dst[off..off+4].
    let (dst_px, _) = dst.as_chunks_mut::<4>();
    let (src_px, _) = src.as_chunks::<4>();
    for (d, s) in dst_px.iter_mut().zip(src_px.iter()) {
        blend_one_pixel(d, s, kind, is_normal);
    }
}

#[inline]
fn blend_one_pixel(dst: &mut [u8], src: &[u8], kind: SeparableBlendKind, is_normal: bool) {
    let sa = src[3];
    if sa == 0 {
        return;
    }
    if is_normal && sa == 255 {
        dst.copy_from_slice(src);
        return;
    }

    let sa_f = u8_to_f32(sa);
    let da_f = u8_to_f32(dst[3]);
    let out_a_f = sa_f + da_f * (1.0 - sa_f);
    if out_a_f <= 0.0 {
        dst.fill(0);
        return;
    }

    let sr = u8_to_f32(src[0]);
    let sg = u8_to_f32(src[1]);
    let sb = u8_to_f32(src[2]);
    let dr = u8_to_f32(dst[0]);
    let dg = u8_to_f32(dst[1]);
    let db = u8_to_f32(dst[2]);

    // Non-separable and per-pixel blend modes use cross-channel formulas.
    let (br, bg, bb) = match kind {
        SeparableBlendKind::Color => {
            crate::psb_blend_nonseparable::blend_color_rgb(dr, dg, db, sr, sg, sb)
        }
        SeparableBlendKind::Hue => {
            crate::psb_blend_nonseparable_full::blend_hue_rgb(dr, dg, db, sr, sg, sb)
        }
        SeparableBlendKind::Saturation => {
            crate::psb_blend_nonseparable_full::blend_saturation_rgb(dr, dg, db, sr, sg, sb)
        }
        SeparableBlendKind::Luminosity => {
            crate::psb_blend_nonseparable_full::blend_luminosity_rgb(dr, dg, db, sr, sg, sb)
        }
        SeparableBlendKind::DarkerColor => {
            crate::psb_blend_nonseparable_full::darker_color_rgb(dr, dg, db, sr, sg, sb)
        }
        SeparableBlendKind::LighterColor => {
            crate::psb_blend_nonseparable_full::lighter_color_rgb(dr, dg, db, sr, sg, sb)
        }
        _ => {
            // All other modes (separable) go through blend_b per channel.
            for c in 0..3 {
                let sc = u8_to_f32(src[c]);
                let dc = u8_to_f32(dst[c]);
                let b = blend_b(kind, dc, sc);
                let co = sa_f * (1.0 - da_f) * sc + sa_f * da_f * b + da_f * (1.0 - sa_f) * dc;
                dst[c] = f32_to_u8_round(co / out_a_f);
            }
            dst[3] = f32_to_u8_round(out_a_f);
            return;
        }
    };
    // Cross-channel blend result applied with straight-alpha.
    let channels = [(sr, dr, br), (sg, dg, bg), (sb, db, bb)];
    for (c, (sc, dc, b)) in channels.into_iter().enumerate() {
        let co = sa_f * (1.0 - da_f) * sc + sa_f * da_f * b + da_f * (1.0 - sa_f) * dc;
        dst[c] = f32_to_u8_round(co / out_a_f);
    }
    dst[3] = f32_to_u8_round(out_a_f);
}

#[inline]
fn store_pixel_f32(px: &mut [u8], r: f32, g: f32, b: f32, a: f32) {
    px[0] = f32_to_u8_round(r);
    px[1] = f32_to_u8_round(g);
    px[2] = f32_to_u8_round(b);
    px[3] = f32_to_u8_round(a);
}

/// Load 4 RGBA8 pixels and convert to planar f32 (RRRR/GGGG/BBBB/AAAA).
/// Uses unpack + `cvtepi32_ps` + `/255` instead of scattered LUT gathers.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn load_rgba8x4_f32_planes(
    ptr: *const u8,
) -> (
    core::arch::x86_64::__m128,
    core::arch::x86_64::__m128,
    core::arch::x86_64::__m128,
    core::arch::x86_64::__m128,
) {
    use core::arch::x86_64::*;
    unsafe {
        let v = _mm_loadu_si128(ptr.cast());
        let zero = _mm_setzero_si128();
        let scale_div = _mm_set1_ps(255.0);
        let lo16 = _mm_unpacklo_epi8(v, zero);
        let hi16 = _mm_unpackhi_epi8(v, zero);
        let p0 = _mm_div_ps(_mm_cvtepi32_ps(_mm_unpacklo_epi16(lo16, zero)), scale_div);
        let p1 = _mm_div_ps(_mm_cvtepi32_ps(_mm_unpackhi_epi16(lo16, zero)), scale_div);
        let p2 = _mm_div_ps(_mm_cvtepi32_ps(_mm_unpacklo_epi16(hi16, zero)), scale_div);
        let p3 = _mm_div_ps(_mm_cvtepi32_ps(_mm_unpackhi_epi16(hi16, zero)), scale_div);
        // Transpose pixel-major RGBA rows into channel planes.
        let t0 = _mm_unpacklo_ps(p0, p1); // r0 r1 g0 g1
        let t1 = _mm_unpacklo_ps(p2, p3); // r2 r3 g2 g3
        let t2 = _mm_unpackhi_ps(p0, p1); // b0 b1 a0 a1
        let t3 = _mm_unpackhi_ps(p2, p3); // b2 b3 a2 a3
        let r = _mm_movelh_ps(t0, t1);
        let g = _mm_movehl_ps(t1, t0);
        let b = _mm_movelh_ps(t2, t3);
        let a = _mm_movehl_ps(t3, t2);
        (r, g, b, a)
    }
}

/// Load 8 RGBA8 pixels into AVX planar f32 (two 4-pixel conversions + insert).
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2", enable = "sse2")]
unsafe fn load_rgba8x8_f32_planes(
    ptr: *const u8,
) -> (
    core::arch::x86_64::__m256,
    core::arch::x86_64::__m256,
    core::arch::x86_64::__m256,
    core::arch::x86_64::__m256,
) {
    use core::arch::x86_64::*;
    unsafe {
        let (r0, g0, b0, a0) = load_rgba8x4_f32_planes(ptr);
        let (r1, g1, b1, a1) = load_rgba8x4_f32_planes(ptr.add(16));
        (
            _mm256_set_m128(r1, r0),
            _mm256_set_m128(g1, g0),
            _mm256_set_m128(b1, b0),
            _mm256_set_m128(a1, a0),
        )
    }
}

/// Load 4 RGBA8 pixels via NEON deinterleave + u32 widen + `/255`.
///
/// Uses `vld4_lane_u8` four times so only the 16-byte span is read (no 32-byte
/// `vld4_u8` over-read / pad copy).
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn load_rgba8x4_f32_planes(
    ptr: *const u8,
) -> (
    core::arch::aarch64::float32x4_t,
    core::arch::aarch64::float32x4_t,
    core::arch::aarch64::float32x4_t,
    core::arch::aarch64::float32x4_t,
) {
    use core::arch::aarch64::*;
    unsafe {
        let zero = vdup_n_u8(0);
        let mut pix = uint8x8x4_t(zero, zero, zero, zero);
        pix = vld4_lane_u8::<0>(ptr, pix);
        pix = vld4_lane_u8::<1>(ptr.add(4), pix);
        pix = vld4_lane_u8::<2>(ptr.add(8), pix);
        pix = vld4_lane_u8::<3>(ptr.add(12), pix);
        let scale = vdupq_n_f32(255.0);
        let r = vdivq_f32(
            vcvtq_f32_u32(vmovl_u16(vget_low_u16(vmovl_u8(pix.0)))),
            scale,
        );
        let g = vdivq_f32(
            vcvtq_f32_u32(vmovl_u16(vget_low_u16(vmovl_u8(pix.1)))),
            scale,
        );
        let b = vdivq_f32(
            vcvtq_f32_u32(vmovl_u16(vget_low_u16(vmovl_u8(pix.2)))),
            scale,
        );
        let a = vdivq_f32(
            vcvtq_f32_u32(vmovl_u16(vget_low_u16(vmovl_u8(pix.3)))),
            scale,
        );
        (r, g, b, a)
    }
}

#[inline]
fn chunk_all_alpha(src: &[u8], lanes: usize, value: u8) -> bool {
    (0..lanes).all(|lane| src[lane * 4 + 3] == value)
}

/// Vectorized blend for one color plane (SSE2, 4 lanes).
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn blend_plane_sse2(
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
        SeparableBlendKind::Screen => {
            _mm_sub_ps(one, _mm_mul_ps(_mm_sub_ps(one, dc), _mm_sub_ps(one, sc)))
        }
        SeparableBlendKind::LinearDodge => _mm_min_ps(_mm_add_ps(dc, sc), one),
        SeparableBlendKind::Overlay => {
            let half = _mm_set1_ps(0.5);
            let two = _mm_set1_ps(2.0);
            let lo = _mm_mul_ps(_mm_mul_ps(two, dc), sc);
            let hi = _mm_sub_ps(
                one,
                _mm_mul_ps(_mm_mul_ps(two, _mm_sub_ps(one, dc)), _mm_sub_ps(one, sc)),
            );
            let mask = _mm_cmple_ps(dc, half);
            // SSE2 emulation of blendv: (lo & mask) | (hi & ~mask)
            _mm_or_ps(_mm_and_ps(mask, lo), _mm_andnot_ps(mask, hi))
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
            let d = _mm_or_ps(
                _mm_and_ps(cb_le_quarter, d_poly),
                _mm_andnot_ps(cb_le_quarter, d_sqrt),
            );
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
            _mm_or_ps(
                _mm_and_ps(cs_le_half, branch1),
                _mm_andnot_ps(cs_le_half, branch2),
            )
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
            _mm_or_ps(_mm_and_ps(mask, lo), _mm_andnot_ps(mask, hi))
        }
        // All other modes (Color, Hue, Darken, Difference, etc.) are scalar-only.
        _ => sc,
    };
    let term1 = _mm_mul_ps(_mm_mul_ps(sa, _mm_sub_ps(one, da)), sc);
    let term2 = _mm_mul_ps(_mm_mul_ps(sa, da), v_b);
    let term3 = _mm_mul_ps(_mm_mul_ps(da, _mm_sub_ps(one, sa)), dc);
    let co = _mm_add_ps(_mm_add_ps(term1, term2), term3);
    // rcp + one Newton-Raphson step: inv = rcp * (2 - a * rcp).
    // out_a is almost never near 0 when sa > 0; u8 round absorbs residual error.
    let oa_safe = _mm_max_ps(
        out_a,
        _mm_set1_ps(crate::psb_blend_separable::HDR_BLEND_EPSILON),
    );
    let rcp = _mm_rcp_ps(oa_safe);
    let inv = _mm_mul_ps(rcp, _mm_sub_ps(_mm_set1_ps(2.0), _mm_mul_ps(oa_safe, rcp)));
    let mut out = _mm_mul_ps(co, inv);
    // sa==0: keep original dc (store is skipped by caller, but keep sane).
    let sa_zero = _mm_cmpeq_ps(sa, zero);
    out = _mm_or_ps(_mm_andnot_ps(sa_zero, out), _mm_and_ps(sa_zero, dc));
    let oa_le0 = _mm_cmple_ps(out_a, zero);
    out = _mm_or_ps(_mm_andnot_ps(oa_le0, out), _mm_and_ps(oa_le0, zero));
    out
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn blend_separable_span_sse2(dst: &mut [u8], src: &[u8], kind: SeparableBlendKind) {
    use core::arch::x86_64::*;
    const LANES: usize = 4;
    let n = dst.len() / 4;
    let is_normal = kind == SeparableBlendKind::Normal;
    let mut i = 0usize;

    while i + LANES <= n {
        let base = i * 4;
        let src_chunk = &src[base..base + LANES * 4];
        let dst_chunk = &mut dst[base..base + LANES * 4];

        if is_normal && chunk_all_alpha(src_chunk, LANES, 255) {
            unsafe {
                let v = _mm_loadu_si128(src_chunk.as_ptr().cast());
                _mm_storeu_si128(dst_chunk.as_mut_ptr().cast(), v);
            }
            i += LANES;
            continue;
        }
        if chunk_all_alpha(src_chunk, LANES, 0) {
            i += LANES;
            continue;
        }

        let mut dr = [0f32; LANES];
        let mut dg = [0f32; LANES];
        let mut db = [0f32; LANES];
        let mut da = [0f32; LANES];
        let mut sa = [0f32; LANES];
        unsafe {
            let (v_sr, v_sg, v_sb, v_sa) = load_rgba8x4_f32_planes(src_chunk.as_ptr());
            let (v_dr, v_dg, v_db, v_da) = load_rgba8x4_f32_planes(dst_chunk.as_ptr());
            let one = _mm_set1_ps(1.0);
            let v_out_a = _mm_add_ps(v_sa, _mm_mul_ps(v_da, _mm_sub_ps(one, v_sa)));
            let out_r = blend_plane_sse2(v_sr, v_dr, v_sa, v_da, v_out_a, kind);
            let out_g = blend_plane_sse2(v_sg, v_dg, v_sa, v_da, v_out_a, kind);
            let out_b = blend_plane_sse2(v_sb, v_db, v_sa, v_da, v_out_a, kind);
            _mm_storeu_ps(dr.as_mut_ptr(), out_r);
            _mm_storeu_ps(dg.as_mut_ptr(), out_g);
            _mm_storeu_ps(db.as_mut_ptr(), out_b);
            _mm_storeu_ps(da.as_mut_ptr(), v_out_a);
            _mm_storeu_ps(sa.as_mut_ptr(), v_sa);
        }

        for lane in 0..LANES {
            if sa[lane] == 0.0 {
                continue;
            }
            let o = lane * 4;
            store_pixel_f32(
                &mut dst_chunk[o..o + 4],
                dr[lane],
                dg[lane],
                db[lane],
                da[lane],
            );
        }
        i += LANES;
    }

    while i < n {
        let off = i * 4;
        blend_one_pixel(&mut dst[off..off + 4], &src[off..off + 4], kind, is_normal);
        i += 1;
    }
}

/// Vectorized blend for one color plane (AVX2, 8 lanes).
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn blend_plane_avx2(
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
        SeparableBlendKind::Screen => _mm256_sub_ps(
            one,
            _mm256_mul_ps(_mm256_sub_ps(one, dc), _mm256_sub_ps(one, sc)),
        ),
        SeparableBlendKind::LinearDodge => _mm256_min_ps(_mm256_add_ps(dc, sc), one),
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
        // All other modes (Color, Hue, Darken, etc.) are scalar-only.
        _ => sc,
    };
    let term1 = _mm256_mul_ps(_mm256_mul_ps(sa, _mm256_sub_ps(one, da)), sc);
    let term2 = _mm256_mul_ps(_mm256_mul_ps(sa, da), v_b);
    let term3 = _mm256_mul_ps(_mm256_mul_ps(da, _mm256_sub_ps(one, sa)), dc);
    let co = _mm256_add_ps(_mm256_add_ps(term1, term2), term3);
    // rcp + one Newton-Raphson step: inv = rcp * (2 - a * rcp).
    // out_a is almost never near 0 when sa > 0; u8 round absorbs residual error.
    let oa_safe = _mm256_max_ps(
        out_a,
        _mm256_set1_ps(crate::psb_blend_separable::HDR_BLEND_EPSILON),
    );
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
#[target_feature(enable = "avx2", enable = "sse2")]
unsafe fn blend_separable_span_avx2(dst: &mut [u8], src: &[u8], kind: SeparableBlendKind) {
    use core::arch::x86_64::*;
    const LANES: usize = 8;
    let n = dst.len() / 4;
    let is_normal = kind == SeparableBlendKind::Normal;
    let mut i = 0usize;

    while i + LANES <= n {
        let base = i * 4;
        let src_chunk = &src[base..base + LANES * 4];
        let dst_chunk = &mut dst[base..base + LANES * 4];

        if is_normal && chunk_all_alpha(src_chunk, LANES, 255) {
            unsafe {
                let v = _mm256_loadu_si256(src_chunk.as_ptr().cast());
                _mm256_storeu_si256(dst_chunk.as_mut_ptr().cast(), v);
            }
            i += LANES;
            continue;
        }
        if chunk_all_alpha(src_chunk, LANES, 0) {
            i += LANES;
            continue;
        }

        let mut dr = [0f32; LANES];
        let mut dg = [0f32; LANES];
        let mut db = [0f32; LANES];
        let mut da = [0f32; LANES];
        let mut sa = [0f32; LANES];
        unsafe {
            let (v_sr, v_sg, v_sb, v_sa) = load_rgba8x8_f32_planes(src_chunk.as_ptr());
            let (v_dr, v_dg, v_db, v_da) = load_rgba8x8_f32_planes(dst_chunk.as_ptr());
            let one = _mm256_set1_ps(1.0);
            let v_out_a = _mm256_add_ps(v_sa, _mm256_mul_ps(v_da, _mm256_sub_ps(one, v_sa)));
            let out_r = blend_plane_avx2(v_sr, v_dr, v_sa, v_da, v_out_a, kind);
            let out_g = blend_plane_avx2(v_sg, v_dg, v_sa, v_da, v_out_a, kind);
            let out_b = blend_plane_avx2(v_sb, v_db, v_sa, v_da, v_out_a, kind);
            _mm256_storeu_ps(dr.as_mut_ptr(), out_r);
            _mm256_storeu_ps(dg.as_mut_ptr(), out_g);
            _mm256_storeu_ps(db.as_mut_ptr(), out_b);
            _mm256_storeu_ps(da.as_mut_ptr(), v_out_a);
            _mm256_storeu_ps(sa.as_mut_ptr(), v_sa);
        }

        for lane in 0..LANES {
            if sa[lane] == 0.0 {
                continue;
            }
            let o = lane * 4;
            store_pixel_f32(
                &mut dst_chunk[o..o + 4],
                dr[lane],
                dg[lane],
                db[lane],
                da[lane],
            );
        }
        i += LANES;
    }

    if i < n {
        unsafe {
            blend_separable_span_sse2(&mut dst[i * 4..], &src[i * 4..], kind);
        }
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn blend_plane_neon(
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
        SeparableBlendKind::Screen => {
            vsubq_f32(one, vmulq_f32(vsubq_f32(one, dc), vsubq_f32(one, sc)))
        }
        SeparableBlendKind::LinearDodge => vminq_f32(vaddq_f32(dc, sc), one),
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
                    vmulq_f32(vsubq_f32(one, vmulq_f32(two, sc)), dc),
                    vsubq_f32(one, dc),
                ),
            );
            // cs > 0.5: cb + (2*cs - 1) * (d - cb)
            let branch2 = vaddq_f32(
                dc,
                vmulq_f32(vsubq_f32(vmulq_f32(two, sc), one), vsubq_f32(d, dc)),
            );
            let cs_le_half = vcleq_f32(sc, half);
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
            let mask = vcleq_f32(sc, half);
            vbslq_f32(mask, lo, hi)
        }
        // All other modes (Color, Hue, Darken, etc.) are scalar-only.
        _ => sc,
    };
    let term1 = vmulq_f32(vmulq_f32(sa, vsubq_f32(one, da)), sc);
    let term2 = vmulq_f32(vmulq_f32(sa, da), v_b);
    let term3 = vmulq_f32(vmulq_f32(da, vsubq_f32(one, sa)), dc);
    let co = vaddq_f32(vaddq_f32(term1, term2), term3);
    let oa_safe = vmaxq_f32(
        out_a,
        vdupq_n_f32(crate::psb_blend_separable::HDR_BLEND_EPSILON),
    );
    let mut out = vdivq_f32(co, oa_safe);
    // sa==0 -> keep dc; out_a<=0 -> zero
    let sa_zero = vceqq_f32(sa, zero);
    out = vbslq_f32(sa_zero, dc, out);
    let oa_le0 = vcleq_f32(out_a, zero);
    out = vbslq_f32(oa_le0, zero, out);
    out
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn blend_separable_span_neon(dst: &mut [u8], src: &[u8], kind: SeparableBlendKind) {
    use core::arch::aarch64::*;
    const LANES: usize = 4;
    let n = dst.len() / 4;
    let is_normal = kind == SeparableBlendKind::Normal;
    let mut i = 0usize;

    while i + LANES <= n {
        let base = i * 4;
        let src_chunk = &src[base..base + LANES * 4];
        let dst_chunk = &mut dst[base..base + LANES * 4];

        if is_normal && chunk_all_alpha(src_chunk, LANES, 255) {
            unsafe {
                let v = vld1q_u8(src_chunk.as_ptr());
                vst1q_u8(dst_chunk.as_mut_ptr(), v);
            }
            i += LANES;
            continue;
        }
        if chunk_all_alpha(src_chunk, LANES, 0) {
            i += LANES;
            continue;
        }

        let mut dr = [0f32; LANES];
        let mut dg = [0f32; LANES];
        let mut db = [0f32; LANES];
        let mut da = [0f32; LANES];
        let mut sa = [0f32; LANES];
        unsafe {
            let (v_sr, v_sg, v_sb, v_sa) = load_rgba8x4_f32_planes(src_chunk.as_ptr());
            let (v_dr, v_dg, v_db, v_da) = load_rgba8x4_f32_planes(dst_chunk.as_ptr());
            let one = vdupq_n_f32(1.0);
            let v_out_a = vaddq_f32(v_sa, vmulq_f32(v_da, vsubq_f32(one, v_sa)));
            let out_r = blend_plane_neon(v_sr, v_dr, v_sa, v_da, v_out_a, kind);
            let out_g = blend_plane_neon(v_sg, v_dg, v_sa, v_da, v_out_a, kind);
            let out_b = blend_plane_neon(v_sb, v_db, v_sa, v_da, v_out_a, kind);
            vst1q_f32(dr.as_mut_ptr(), out_r);
            vst1q_f32(dg.as_mut_ptr(), out_g);
            vst1q_f32(db.as_mut_ptr(), out_b);
            vst1q_f32(da.as_mut_ptr(), v_out_a);
            vst1q_f32(sa.as_mut_ptr(), v_sa);
        }

        for lane in 0..LANES {
            if sa[lane] == 0.0 {
                continue;
            }
            let o = lane * 4;
            store_pixel_f32(
                &mut dst_chunk[o..o + 4],
                dr[lane],
                dg[lane],
                db[lane],
                da[lane],
            );
        }
        i += LANES;
    }

    while i < n {
        let off = i * 4;
        blend_one_pixel(&mut dst[off..off + 4], &src[off..off + 4], kind, is_normal);
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f32_to_u8_round_matches_wgsl_floor_bias() {
        // Half-ties and dense sweep: must match floor(x*255+0.5), not ties-to-even.
        for scaled in [
            0.0f32, 0.5, 1.0, 1.5, 2.5, 126.5, 127.5, 128.5, 254.5, 255.0,
        ] {
            let unit = scaled / 255.0;
            let cpu = f32_to_u8_round(unit);
            let gpu_like = (unit.clamp(0.0, 1.0) * 255.0 + 0.5).floor() as u8;
            assert_eq!(cpu, gpu_like, "scaled={scaled}");
        }
        for v in 0u16..=255 {
            let f = v as f32 / 255.0;
            assert_eq!(f32_to_u8_round(f), v as u8, "v={v}");
        }
        // Confirm the documented WGSL snippet stays the reviewed contract.
        assert!(UNIT_TO_U8_WGSL_FLOOR_BIAS.contains("floor"));
        assert!(UNIT_TO_U8_WGSL_FLOOR_BIAS.contains("255.0"));
        assert!(UNIT_TO_U8_WGSL_FLOOR_BIAS.contains("+ 0.5"));
    }

    #[test]
    fn color_blend_key_maps_and_preserves_luminosity() {
        assert_eq!(
            SeparableBlendKind::from_psd_key(b"colr"),
            Some(SeparableBlendKind::Color)
        );
        // Backdrop mid-gray; source solid blue like the brochure fill layer.
        let mut dst = [128u8, 128, 128, 255];
        let src = [13u8, 27, 130, 255];
        let mut as_normal = dst;
        blend_separable_span_scalar(&mut dst, &src, SeparableBlendKind::Color);
        blend_separable_span_scalar(&mut as_normal, &src, SeparableBlendKind::Normal);
        // Normal would replace with solid blue; Color must keep gray luminosity.
        assert_ne!(dst, as_normal);
        let lum = |p: [u8; 4]| 0.3 * p[0] as f32 + 0.59 * p[1] as f32 + 0.11 * p[2] as f32;
        assert!((lum(dst) - lum([128, 128, 128, 255])).abs() < 2.0);
        // Result should still be bluish (B channel dominant).
        assert!(dst[2] > dst[0] && dst[2] > dst[1]);
    }

    #[test]
    fn normal_semi_transparent_matches_scalar_reference() {
        let mut dst_simd = [
            20u8, 20, 20, 255, 40, 40, 40, 255, 0, 0, 0, 0, 10, 10, 10, 128,
        ];
        let mut dst_ref = dst_simd;
        let src = [
            0u8, 255, 0, 128, 255, 0, 0, 255, 0, 0, 0, 0, 100, 100, 100, 64,
        ];
        blend_separable_span(&mut dst_simd, &src, SeparableBlendKind::Normal);
        blend_separable_span_scalar(&mut dst_ref, &src, SeparableBlendKind::Normal);
        assert_eq!(dst_simd, dst_ref);
    }

    #[test]
    fn screen_multiply_match_scalar() {
        for kind in [
            SeparableBlendKind::Screen,
            SeparableBlendKind::Multiply,
            SeparableBlendKind::LinearDodge,
        ] {
            let mut dst_simd = [
                40u8, 80, 120, 255, 10, 20, 30, 200, 255, 255, 255, 255, 0, 0, 0, 128,
            ];
            let mut dst_ref = dst_simd;
            let src = [
                0u8, 0, 0, 255, 128, 64, 32, 128, 50, 50, 50, 50, 200, 100, 0, 200,
            ];
            blend_separable_span(&mut dst_simd, &src, kind);
            blend_separable_span_scalar(&mut dst_ref, &src, kind);
            assert_eq!(dst_simd, dst_ref, "mismatch for {kind:?}");
        }
    }

    /// Arch-agnostic bit-identical check over a span long enough to exercise
    /// AVX2 (8 px), SSE/NEON (4 px), and scalar tail on every CI host.
    #[test]
    fn long_span_all_modes_match_scalar_bit_identical() {
        let n = 37usize; // 8+8+8+8+4+1 covers AVX2 / SSE / NEON / tail
        let mut dst_base = vec![0u8; n * 4];
        let mut src = vec![0u8; n * 4];
        for i in 0..n {
            let o = i * 4;
            dst_base[o] = (i * 3) as u8;
            dst_base[o + 1] = (i * 5) as u8;
            dst_base[o + 2] = (i * 7) as u8;
            dst_base[o + 3] = (40 + (i % 200)) as u8;
            src[o] = (255u8).wrapping_sub((i * 11) as u8);
            src[o + 1] = (i * 13) as u8;
            src[o + 2] = (i * 17) as u8;
            src[o + 3] = (10 + (i % 240)) as u8;
        }
        for kind in [
            SeparableBlendKind::Normal,
            SeparableBlendKind::Screen,
            SeparableBlendKind::Multiply,
            SeparableBlendKind::LinearDodge,
            SeparableBlendKind::Overlay,
            SeparableBlendKind::SoftLight,
            SeparableBlendKind::HardLight,
        ] {
            let mut simd = dst_base.clone();
            let mut scalar = dst_base.clone();
            blend_separable_span(&mut simd, &src, kind);
            blend_separable_span_scalar(&mut scalar, &src, kind);
            assert_eq!(simd, scalar, "public SIMD vs scalar mismatch for {kind:?}");
        }
    }
}
