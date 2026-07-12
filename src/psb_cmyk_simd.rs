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

//! SIMD naive Photoshop CMYK -> RGB (0 = full ink) for planar row conversion.
//!
//! Formula matches [`crate::psb_reader::cmyk_to_rgb`]: `channel * k / 255`.

#[cfg(target_arch = "x86_64")]
const SSE_PIXELS: usize = 8;
#[cfg(target_arch = "x86_64")]
const AVX2_PIXELS: usize = 16;
#[cfg(target_arch = "aarch64")]
const NEON_PIXELS: usize = 8;

/// Convert planar CMYK(+optional A) samples to interleaved RGBA8.
pub fn cmyk_planes_to_rgba8(
    c: &[u8],
    m: &[u8],
    y: &[u8],
    k: &[u8],
    alpha: Option<&[u8]>,
    dst: &mut [u8],
) {
    let mut width = c
        .len()
        .min(m.len())
        .min(y.len())
        .min(k.len())
        .min(dst.len() / 4);
    if let Some(a) = alpha {
        width = width.min(a.len());
    }
    if width == 0 {
        return;
    }

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            unsafe {
                cmyk_planes_to_rgba8_avx2(
                    &c[..width],
                    &m[..width],
                    &y[..width],
                    &k[..width],
                    alpha.map(|a| &a[..width]),
                    &mut dst[..width * 4],
                );
            }
            return;
        }
        if is_x86_feature_detected!("sse2") {
            unsafe {
                cmyk_planes_to_rgba8_sse2(
                    &c[..width],
                    &m[..width],
                    &y[..width],
                    &k[..width],
                    alpha.map(|a| &a[..width]),
                    &mut dst[..width * 4],
                );
            }
            return;
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        unsafe {
            cmyk_planes_to_rgba8_neon(
                &c[..width],
                &m[..width],
                &y[..width],
                &k[..width],
                alpha.map(|a| &a[..width]),
                &mut dst[..width * 4],
            );
        }
        return;
    }

    cmyk_planes_to_rgba8_scalar(
        &c[..width],
        &m[..width],
        &y[..width],
        &k[..width],
        alpha.map(|a| &a[..width]),
        &mut dst[..width * 4],
    );
}

fn cmyk_planes_to_rgba8_scalar(
    c: &[u8],
    m: &[u8],
    y: &[u8],
    k: &[u8],
    alpha: Option<&[u8]>,
    dst: &mut [u8],
) {
    for col in 0..c.len() {
        let (r, g, b) = crate::psb_reader::cmyk_to_rgb(c[col], m[col], y[col], k[col]);
        let base = col * 4;
        dst[base] = r;
        dst[base + 1] = g;
        dst[base + 2] = b;
        dst[base + 3] = alpha.map(|a| a[col]).unwrap_or(255);
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn cmyk_planes_to_rgba8_sse2(
    c: &[u8],
    m: &[u8],
    y: &[u8],
    k: &[u8],
    alpha: Option<&[u8]>,
    dst: &mut [u8],
) {
    use crate::psb_simd_mul_div255::mul_div255_u8x8;
    use core::arch::x86_64::*;
    let n = c.len();
    let mut i = 0usize;
    while i + SSE_PIXELS <= n {
        unsafe {
            let cv = _mm_loadl_epi64(c.as_ptr().add(i).cast());
            let mv = _mm_loadl_epi64(m.as_ptr().add(i).cast());
            let yv = _mm_loadl_epi64(y.as_ptr().add(i).cast());
            let kv = _mm_loadl_epi64(k.as_ptr().add(i).cast());
            let r = mul_div255_u8x8(cv, kv);
            let g = mul_div255_u8x8(mv, kv);
            let b = mul_div255_u8x8(yv, kv);
            let a = match alpha {
                Some(a) => _mm_loadl_epi64(a.as_ptr().add(i).cast()),
                None => _mm_set1_epi8(-1),
            };
            let rg = _mm_unpacklo_epi8(r, g);
            let ba = _mm_unpacklo_epi8(b, a);
            let rgba0 = _mm_unpacklo_epi16(rg, ba);
            let rgba1 = _mm_unpackhi_epi16(rg, ba);
            _mm_storeu_si128(dst.as_mut_ptr().add(i * 4).cast(), rgba0);
            _mm_storeu_si128(dst.as_mut_ptr().add(i * 4 + 16).cast(), rgba1);
        }
        i += SSE_PIXELS;
    }
    if i < n {
        cmyk_planes_to_rgba8_scalar(
            &c[i..],
            &m[i..],
            &y[i..],
            &k[i..],
            alpha.map(|a| &a[i..]),
            &mut dst[i * 4..],
        );
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn cmyk_planes_to_rgba8_avx2(
    c: &[u8],
    m: &[u8],
    y: &[u8],
    k: &[u8],
    alpha: Option<&[u8]>,
    dst: &mut [u8],
) {
    use crate::psb_simd_mul_div255::mul_div255_u8x16;
    use core::arch::x86_64::*;
    let n = c.len();
    let mut i = 0usize;
    while i + AVX2_PIXELS <= n {
        unsafe {
            let cv = _mm_loadu_si128(c.as_ptr().add(i).cast());
            let mv = _mm_loadu_si128(m.as_ptr().add(i).cast());
            let yv = _mm_loadu_si128(y.as_ptr().add(i).cast());
            let kv = _mm_loadu_si128(k.as_ptr().add(i).cast());
            let r = mul_div255_u8x16(cv, kv);
            let g = mul_div255_u8x16(mv, kv);
            let b = mul_div255_u8x16(yv, kv);
            let a = match alpha {
                Some(a) => _mm_loadu_si128(a.as_ptr().add(i).cast()),
                None => _mm_set1_epi8(-1),
            };
            let rg_lo = _mm_unpacklo_epi8(r, g);
            let rg_hi = _mm_unpackhi_epi8(r, g);
            let ba_lo = _mm_unpacklo_epi8(b, a);
            let ba_hi = _mm_unpackhi_epi8(b, a);
            let rgba0 = _mm_unpacklo_epi16(rg_lo, ba_lo);
            let rgba1 = _mm_unpackhi_epi16(rg_lo, ba_lo);
            let rgba2 = _mm_unpacklo_epi16(rg_hi, ba_hi);
            let rgba3 = _mm_unpackhi_epi16(rg_hi, ba_hi);
            let out = dst.as_mut_ptr().add(i * 4);
            _mm_storeu_si128(out.cast(), rgba0);
            _mm_storeu_si128(out.add(16).cast(), rgba1);
            _mm_storeu_si128(out.add(32).cast(), rgba2);
            _mm_storeu_si128(out.add(48).cast(), rgba3);
        }
        i += AVX2_PIXELS;
    }
    if i + SSE_PIXELS <= n {
        unsafe {
            cmyk_planes_to_rgba8_sse2(
                &c[i..],
                &m[i..],
                &y[i..],
                &k[i..],
                alpha.map(|a| &a[i..]),
                &mut dst[i * 4..],
            );
        }
    } else if i < n {
        cmyk_planes_to_rgba8_scalar(
            &c[i..],
            &m[i..],
            &y[i..],
            &k[i..],
            alpha.map(|a| &a[i..]),
            &mut dst[i * 4..],
        );
    }
}

#[cfg(target_arch = "aarch64")]
unsafe fn cmyk_planes_to_rgba8_neon(
    c: &[u8],
    m: &[u8],
    y: &[u8],
    k: &[u8],
    alpha: Option<&[u8]>,
    dst: &mut [u8],
) {
    use crate::psb_simd_mul_div255::mul_div255_u8x8_neon;
    use core::arch::aarch64::*;
    let n = c.len();
    let mut i = 0usize;
    while i + NEON_PIXELS <= n {
        unsafe {
            let cv = vld1_u8(c.as_ptr().add(i));
            let mv = vld1_u8(m.as_ptr().add(i));
            let yv = vld1_u8(y.as_ptr().add(i));
            let kv = vld1_u8(k.as_ptr().add(i));
            let r = mul_div255_u8x8_neon(cv, kv);
            let g = mul_div255_u8x8_neon(mv, kv);
            let b = mul_div255_u8x8_neon(yv, kv);
            let a = match alpha {
                Some(a) => vld1_u8(a.as_ptr().add(i)),
                None => vdup_n_u8(255),
            };
            let rg = vzip_u8(r, g);
            let ba = vzip_u8(b, a);
            let rgba_lo = vzip_u16(vreinterpret_u16_u8(rg.0), vreinterpret_u16_u8(ba.0));
            let rgba_hi = vzip_u16(vreinterpret_u16_u8(rg.1), vreinterpret_u16_u8(ba.1));
            vst1_u8(dst.as_mut_ptr().add(i * 4), vreinterpret_u8_u16(rgba_lo.0));
            vst1_u8(
                dst.as_mut_ptr().add(i * 4 + 8),
                vreinterpret_u8_u16(rgba_lo.1),
            );
            vst1_u8(
                dst.as_mut_ptr().add(i * 4 + 16),
                vreinterpret_u8_u16(rgba_hi.0),
            );
            vst1_u8(
                dst.as_mut_ptr().add(i * 4 + 24),
                vreinterpret_u8_u16(rgba_hi.1),
            );
        }
        i += NEON_PIXELS;
    }
    if i < n {
        cmyk_planes_to_rgba8_scalar(
            &c[i..],
            &m[i..],
            &y[i..],
            &k[i..],
            alpha.map(|a| &a[i..]),
            &mut dst[i * 4..],
        );
    }
}

/// Pack planar Adobe-polarity CMYK into interleaved lcms polarity (`255 - sample`).
///
/// `dst` must hold at least `n * 4` bytes where `n` is the common plane length.
pub fn pack_adobe_cmyk_inverted(c: &[u8], m: &[u8], y: &[u8], k: &[u8], dst: &mut [u8]) {
    let n = c
        .len()
        .min(m.len())
        .min(y.len())
        .min(k.len())
        .min(dst.len() / 4);
    if n == 0 {
        return;
    }
    let c = &c[..n];
    let m = &m[..n];
    let y = &y[..n];
    let k = &k[..n];
    let dst = &mut dst[..n * 4];

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            unsafe {
                pack_adobe_cmyk_inverted_avx2(c, m, y, k, dst);
            }
            return;
        }
        if is_x86_feature_detected!("sse2") {
            unsafe {
                pack_adobe_cmyk_inverted_sse2(c, m, y, k, dst);
            }
            return;
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        unsafe {
            pack_adobe_cmyk_inverted_neon(c, m, y, k, dst);
        }
        return;
    }

    pack_adobe_cmyk_inverted_scalar(c, m, y, k, dst);
}

fn pack_adobe_cmyk_inverted_scalar(c: &[u8], m: &[u8], y: &[u8], k: &[u8], dst: &mut [u8]) {
    for i in 0..c.len() {
        let base = i * 4;
        dst[base] = 255u8.wrapping_sub(c[i]);
        dst[base + 1] = 255u8.wrapping_sub(m[i]);
        dst[base + 2] = 255u8.wrapping_sub(y[i]);
        dst[base + 3] = 255u8.wrapping_sub(k[i]);
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn pack_adobe_cmyk_inverted_sse2(c: &[u8], m: &[u8], y: &[u8], k: &[u8], dst: &mut [u8]) {
    use core::arch::x86_64::*;
    let n = c.len();
    let mut i = 0usize;
    let ones = _mm_set1_epi8(-1); // 0xFF
    while i + SSE_PIXELS <= n {
        unsafe {
            let cv = _mm_xor_si128(_mm_loadl_epi64(c.as_ptr().add(i).cast()), ones);
            let mv = _mm_xor_si128(_mm_loadl_epi64(m.as_ptr().add(i).cast()), ones);
            let yv = _mm_xor_si128(_mm_loadl_epi64(y.as_ptr().add(i).cast()), ones);
            let kv = _mm_xor_si128(_mm_loadl_epi64(k.as_ptr().add(i).cast()), ones);
            let cm = _mm_unpacklo_epi8(cv, mv);
            let yk = _mm_unpacklo_epi8(yv, kv);
            let cmyk0 = _mm_unpacklo_epi16(cm, yk);
            let cmyk1 = _mm_unpackhi_epi16(cm, yk);
            _mm_storeu_si128(dst.as_mut_ptr().add(i * 4).cast(), cmyk0);
            _mm_storeu_si128(dst.as_mut_ptr().add(i * 4 + 16).cast(), cmyk1);
        }
        i += SSE_PIXELS;
    }
    if i < n {
        pack_adobe_cmyk_inverted_scalar(&c[i..], &m[i..], &y[i..], &k[i..], &mut dst[i * 4..]);
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn pack_adobe_cmyk_inverted_avx2(c: &[u8], m: &[u8], y: &[u8], k: &[u8], dst: &mut [u8]) {
    use core::arch::x86_64::*;
    let n = c.len();
    let mut i = 0usize;
    let ones = _mm_set1_epi8(-1);
    while i + AVX2_PIXELS <= n {
        unsafe {
            let cv = _mm_xor_si128(_mm_loadu_si128(c.as_ptr().add(i).cast()), ones);
            let mv = _mm_xor_si128(_mm_loadu_si128(m.as_ptr().add(i).cast()), ones);
            let yv = _mm_xor_si128(_mm_loadu_si128(y.as_ptr().add(i).cast()), ones);
            let kv = _mm_xor_si128(_mm_loadu_si128(k.as_ptr().add(i).cast()), ones);
            let cm_lo = _mm_unpacklo_epi8(cv, mv);
            let cm_hi = _mm_unpackhi_epi8(cv, mv);
            let yk_lo = _mm_unpacklo_epi8(yv, kv);
            let yk_hi = _mm_unpackhi_epi8(yv, kv);
            let out = dst.as_mut_ptr().add(i * 4);
            _mm_storeu_si128(out.cast(), _mm_unpacklo_epi16(cm_lo, yk_lo));
            _mm_storeu_si128(out.add(16).cast(), _mm_unpackhi_epi16(cm_lo, yk_lo));
            _mm_storeu_si128(out.add(32).cast(), _mm_unpacklo_epi16(cm_hi, yk_hi));
            _mm_storeu_si128(out.add(48).cast(), _mm_unpackhi_epi16(cm_hi, yk_hi));
        }
        i += AVX2_PIXELS;
    }
    if i + SSE_PIXELS <= n {
        unsafe {
            pack_adobe_cmyk_inverted_sse2(&c[i..], &m[i..], &y[i..], &k[i..], &mut dst[i * 4..]);
        }
    } else if i < n {
        pack_adobe_cmyk_inverted_scalar(&c[i..], &m[i..], &y[i..], &k[i..], &mut dst[i * 4..]);
    }
}

#[cfg(target_arch = "aarch64")]
unsafe fn pack_adobe_cmyk_inverted_neon(c: &[u8], m: &[u8], y: &[u8], k: &[u8], dst: &mut [u8]) {
    use core::arch::aarch64::*;
    let n = c.len();
    let mut i = 0usize;
    let ones = vdup_n_u8(255);
    while i + NEON_PIXELS <= n {
        unsafe {
            let cv = veor_u8(vld1_u8(c.as_ptr().add(i)), ones);
            let mv = veor_u8(vld1_u8(m.as_ptr().add(i)), ones);
            let yv = veor_u8(vld1_u8(y.as_ptr().add(i)), ones);
            let kv = veor_u8(vld1_u8(k.as_ptr().add(i)), ones);
            // vst4 stores C,M,Y,K interleaved for 8 pixels.
            let lanes = uint8x8x4_t(cv, mv, yv, kv);
            vst4_u8(dst.as_mut_ptr().add(i * 4), lanes);
        }
        i += NEON_PIXELS;
    }
    if i < n {
        pack_adobe_cmyk_inverted_scalar(&c[i..], &m[i..], &y[i..], &k[i..], &mut dst[i * 4..]);
    }
}

/// Write planar alpha samples into the A channel of interleaved RGBA8 (`dst[i*4+3]`).
///
/// RGB bytes are left untouched. Used after lcms2 RGB conversion + interleave.
pub fn write_alpha_plane_into_rgba8(alpha: &[u8], dst_rgba: &mut [u8]) {
    let n = alpha.len().min(dst_rgba.len() / 4);
    if n == 0 {
        return;
    }

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("ssse3") {
            unsafe {
                write_alpha_plane_into_rgba8_ssse3(&alpha[..n], &mut dst_rgba[..n * 4]);
            }
            return;
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        unsafe {
            write_alpha_plane_into_rgba8_neon(&alpha[..n], &mut dst_rgba[..n * 4]);
        }
        return;
    }

    write_alpha_plane_into_rgba8_scalar(&alpha[..n], &mut dst_rgba[..n * 4]);
}

fn write_alpha_plane_into_rgba8_scalar(alpha: &[u8], dst_rgba: &mut [u8]) {
    for (i, &a) in alpha.iter().enumerate() {
        dst_rgba[i * 4 + 3] = a;
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "ssse3")]
unsafe fn write_alpha_plane_into_rgba8_ssse3(alpha: &[u8], dst_rgba: &mut [u8]) {
    use core::arch::x86_64::*;
    const PIXELS: usize = 4;
    let n = alpha.len();
    let mut i = 0usize;
    // pshufb: place alpha bytes into A slots; -1 -> 0.
    let a_shuf = _mm_setr_epi8(-1, -1, -1, 0, -1, -1, -1, 1, -1, -1, -1, 2, -1, -1, -1, 3);
    let rgb_mask = _mm_setr_epi8(-1, -1, -1, 0, -1, -1, -1, 0, -1, -1, -1, 0, -1, -1, -1, 0);

    while i + PIXELS <= n {
        unsafe {
            let dst = dst_rgba.as_mut_ptr().add(i * 4);
            let px = _mm_loadu_si128(dst.cast());
            let a4 = _mm_cvtsi32_si128(i32::from_le_bytes([
                alpha[i],
                alpha[i + 1],
                alpha[i + 2],
                alpha[i + 3],
            ]));
            let scattered = _mm_shuffle_epi8(a4, a_shuf);
            let out = _mm_or_si128(_mm_and_si128(px, rgb_mask), scattered);
            _mm_storeu_si128(dst.cast(), out);
        }
        i += PIXELS;
    }

    if i < n {
        write_alpha_plane_into_rgba8_scalar(&alpha[i..], &mut dst_rgba[i * 4..]);
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn write_alpha_plane_into_rgba8_neon(alpha: &[u8], dst_rgba: &mut [u8]) {
    use core::arch::aarch64::*;
    const PIXELS: usize = 8;
    let n = alpha.len();
    let mut i = 0usize;

    while i + PIXELS <= n {
        unsafe {
            let dst = dst_rgba.as_mut_ptr().add(i * 4);
            let mut pix = vld4_u8(dst);
            pix.3 = vld1_u8(alpha.as_ptr().add(i));
            vst4_u8(dst, pix);
        }
        i += PIXELS;
    }

    if i < n {
        write_alpha_plane_into_rgba8_scalar(&alpha[i..], &mut dst_rgba[i * 4..]);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        cmyk_planes_to_rgba8, pack_adobe_cmyk_inverted, write_alpha_plane_into_rgba8,
        write_alpha_plane_into_rgba8_scalar,
    };
    use crate::psb_reader::cmyk_to_rgb;
    use crate::psb_simd_mul_div255::div255_u16_exact;

    #[test]
    fn div255_magic_matches_integer_div() {
        for x in 0u16..=65025 {
            assert_eq!(div255_u16_exact(x), (x / 255) as u8, "x={x}");
        }
    }

    #[test]
    fn cmyk_row_matches_scalar() {
        let n = 64usize;
        let c: Vec<u8> = (0..n).map(|i| (i * 3) as u8).collect();
        let m: Vec<u8> = (0..n).map(|i| (i * 5) as u8).collect();
        let y: Vec<u8> = (0..n).map(|i| (i * 7) as u8).collect();
        let k: Vec<u8> = (0..n).map(|i| 255u8.wrapping_sub(i as u8)).collect();
        let a: Vec<u8> = (0..n).map(|i| (200 + (i % 50)) as u8).collect();
        let mut dst = vec![0u8; n * 4];
        cmyk_planes_to_rgba8(&c, &m, &y, &k, Some(&a), &mut dst);
        for i in 0..n {
            let (r, g, b) = cmyk_to_rgb(c[i], m[i], y[i], k[i]);
            assert_eq!(&dst[i * 4..i * 4 + 4], &[r, g, b, a[i]], "i={i}");
        }
    }

    #[test]
    fn cmyk_opaque_default_alpha() {
        let c = [255u8, 0];
        let m = [255u8, 255];
        let y = [255u8, 255];
        let k = [255u8, 255];
        let mut dst = [0u8; 8];
        cmyk_planes_to_rgba8(&c, &m, &y, &k, None, &mut dst);
        assert_eq!(&dst[0..4], &[255, 255, 255, 255]);
        assert_eq!(&dst[4..8], &[0, 255, 255, 255]);
    }

    #[test]
    fn pack_adobe_inverted_matches_scalar() {
        let n = 100usize;
        let c: Vec<u8> = (0..n).map(|i| i as u8).collect();
        let m: Vec<u8> = (0..n).map(|i| (i * 2) as u8).collect();
        let y: Vec<u8> = (0..n).map(|i| (i * 3) as u8).collect();
        let k: Vec<u8> = (0..n).map(|i| 255u8.wrapping_sub((i * 5) as u8)).collect();
        let mut simd = vec![0u8; n * 4];
        let mut scalar = vec![0u8; n * 4];
        pack_adobe_cmyk_inverted(&c, &m, &y, &k, &mut simd);
        super::pack_adobe_cmyk_inverted_scalar(&c, &m, &y, &k, &mut scalar);
        assert_eq!(simd, scalar);
    }

    #[test]
    fn write_alpha_plane_matches_scalar() {
        let n = 37usize;
        let alpha: Vec<u8> = (0..n).map(|i| (i * 7) as u8).collect();
        let mut simd = vec![0xABu8; n * 4];
        let mut scalar = simd.clone();
        write_alpha_plane_into_rgba8(&alpha, &mut simd);
        write_alpha_plane_into_rgba8_scalar(&alpha, &mut scalar);
        assert_eq!(simd, scalar);
        for i in 0..n {
            assert_eq!(simd[i * 4 + 3], alpha[i]);
            assert_eq!(&simd[i * 4..i * 4 + 3], &[0xAB, 0xAB, 0xAB]);
        }
    }
}
