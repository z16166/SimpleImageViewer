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

//! SIMD straight-alpha separable blend (Normal / Screen / Linear Dodge / Multiply).
//!
//! Processes 4 (SSE2/NEON) or 8 (AVX2) pixels per iteration. Final u8 conversion
//! uses the same `round()` path as the scalar reference so results stay
//! bit-identical to the previous per-pixel f32 loop.

/// Photoshop / PDF separable blend mode for a horizontal RGBA8 span.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeparableBlendKind {
    Normal,
    Screen,
    LinearDodge,
    Multiply,
}

#[inline]
fn u8_to_f32(v: u8) -> f32 {
    // Match former LUT formula `(i as f32) / 255.0` (not `* (1/255)`).
    (v as f32) / 255.0
}

#[inline]
fn f32_to_u8_round(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

#[inline]
fn blend_b(kind: SeparableBlendKind, cb: f32, cs: f32) -> f32 {
    match kind {
        SeparableBlendKind::Normal => cs,
        SeparableBlendKind::Screen => 1.0 - (1.0 - cb) * (1.0 - cs),
        SeparableBlendKind::LinearDodge => (cb + cs).min(1.0),
        SeparableBlendKind::Multiply => cb * cs,
    }
}

/// Straight-alpha separable blend of `src` onto `dst` (same length, RGBA8).
pub fn blend_separable_span(dst: &mut [u8], src: &[u8], kind: SeparableBlendKind) {
    debug_assert_eq!(dst.len(), src.len());
    debug_assert!(dst.len().is_multiple_of(4));
    if dst.is_empty() {
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

    blend_separable_span_scalar(dst, src, kind);
}

fn blend_separable_span_scalar(dst: &mut [u8], src: &[u8], kind: SeparableBlendKind) {
    let n = dst.len() / 4;
    let is_normal = kind == SeparableBlendKind::Normal;
    for i in 0..n {
        let off = i * 4;
        blend_one_pixel(&mut dst[off..off + 4], &src[off..off + 4], kind, is_normal);
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

    for c in 0..3 {
        let sc = u8_to_f32(src[c]);
        let dc = u8_to_f32(dst[c]);
        let b = blend_b(kind, dc, sc);
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
/// `vld4_u8` reads 8 pixels (32 bytes); pad the 16-byte span so the load is
/// in-bounds (high 4 lanes are ignored).
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
        let mut pad = [0u8; 32];
        core::ptr::copy_nonoverlapping(ptr, pad.as_mut_ptr(), 16);
        let pix = vld4_u8(pad.as_ptr());
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
    };
    let term1 = _mm_mul_ps(_mm_mul_ps(sa, _mm_sub_ps(one, da)), sc);
    let term2 = _mm_mul_ps(_mm_mul_ps(sa, da), v_b);
    let term3 = _mm_mul_ps(_mm_mul_ps(da, _mm_sub_ps(one, sa)), dc);
    let co = _mm_add_ps(_mm_add_ps(term1, term2), term3);
    let oa_safe = _mm_max_ps(out_a, _mm_set1_ps(1e-20));
    let mut out = _mm_div_ps(co, oa_safe);
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
    };
    let term1 = _mm256_mul_ps(_mm256_mul_ps(sa, _mm256_sub_ps(one, da)), sc);
    let term2 = _mm256_mul_ps(_mm256_mul_ps(sa, da), v_b);
    let term3 = _mm256_mul_ps(_mm256_mul_ps(da, _mm256_sub_ps(one, sa)), dc);
    let co = _mm256_add_ps(_mm256_add_ps(term1, term2), term3);
    let oa_safe = _mm256_max_ps(out_a, _mm256_set1_ps(1e-20));
    let mut out = _mm256_div_ps(co, oa_safe);
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
    };
    let term1 = vmulq_f32(vmulq_f32(sa, vsubq_f32(one, da)), sc);
    let term2 = vmulq_f32(vmulq_f32(sa, da), v_b);
    let term3 = vmulq_f32(vmulq_f32(da, vsubq_f32(one, sa)), dc);
    let co = vaddq_f32(vaddq_f32(term1, term2), term3);
    let oa_safe = vmaxq_f32(out_a, vdupq_n_f32(1e-20));
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
}
