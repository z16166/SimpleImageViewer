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

//! SIMD fold of layer opacity and optional mask into interleaved RGBA8 alpha.
//!
//! Semantics match the former scalar loop in `psb_layer_decode`:
//! `a = a * opacity / 255`, then if mask present `a = a * mask[i] / 255`
//! (two truncated integer divides; do not fuse into one multiply).
//!
//! Order is the PSD/Photoshop convention: opacity first, then mask.

#[cfg(target_arch = "x86_64")]
const SSE_PIXELS: usize = 8;
#[cfg(target_arch = "x86_64")]
const AVX2_PIXELS: usize = 16;
#[cfg(target_arch = "aarch64")]
const NEON_PIXELS: usize = 16;

use crate::psb_simd_mul_div255::div255_u16_exact;

/// Fold layer opacity and optional mask into the alpha channel of `rgba`.
///
/// `rgba` must be interleaved RGBA8 (`len % 4 == 0`). A short `mask` pads
/// missing samples as 255 (same as `m.get(i).copied().unwrap_or(255)`).
pub fn fold_opacity_mask_into_alpha(rgba: &mut [u8], opacity: u8, mask: Option<&[u8]>) {
    debug_assert!(rgba.len().is_multiple_of(4));
    if rgba.is_empty() {
        return;
    }
    if opacity == 255 && mask.is_none() {
        return;
    }

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            unsafe {
                fold_opacity_mask_into_alpha_avx2(rgba, opacity, mask);
            }
            return;
        }
        if is_x86_feature_detected!("sse4.1") {
            unsafe {
                fold_opacity_mask_into_alpha_sse41(rgba, opacity, mask);
            }
            return;
        }
        if is_x86_feature_detected!("sse2") {
            unsafe {
                fold_opacity_mask_into_alpha_sse2(rgba, opacity, mask);
            }
            return;
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        unsafe {
            fold_opacity_mask_into_alpha_neon(rgba, opacity, mask);
        }
        return;
    }

    fold_opacity_mask_into_alpha_scalar(rgba, opacity, mask);
}

fn fold_opacity_mask_into_alpha_scalar(rgba: &mut [u8], opacity: u8, mask: Option<&[u8]>) {
    let pixel_count = rgba.len() / 4;
    let opacity_u16 = opacity as u16;
    for i in 0..pixel_count {
        let off = i * 4 + 3;
        let mut a = div255_u16_exact(rgba[off] as u16 * opacity_u16);
        if let Some(m) = mask {
            let mv = m.get(i).copied().unwrap_or(255) as u16;
            a = div255_u16_exact(a as u16 * mv);
        }
        rgba[off] = a;
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn fold_opacity_mask_into_alpha_sse2(rgba: &mut [u8], opacity: u8, mask: Option<&[u8]>) {
    use crate::psb_simd_mul_div255::mul_div255_u8x8;
    use core::arch::x86_64::*;
    let n = rgba.len() / 4;
    let mut i = 0usize;
    while i + SSE_PIXELS <= n {
        let mut alpha = [0u8; SSE_PIXELS];
        let mut mask_values = [255u8; SSE_PIXELS];
        for lane in 0..SSE_PIXELS {
            alpha[lane] = rgba[(i + lane) * 4 + 3];
            if let Some(m) = mask {
                mask_values[lane] = m.get(i + lane).copied().unwrap_or(255);
            }
        }

        unsafe {
            let mut av = _mm_loadl_epi64(alpha.as_ptr().cast());
            if opacity != 255 {
                av = mul_div255_u8x8(av, _mm_set1_epi8(opacity as i8));
            }
            if mask.is_some() {
                let mv = _mm_loadl_epi64(mask_values.as_ptr().cast());
                av = mul_div255_u8x8(av, mv);
            }
            _mm_storel_epi64(alpha.as_mut_ptr().cast(), av);
        }

        for lane in 0..SSE_PIXELS {
            rgba[(i + lane) * 4 + 3] = alpha[lane];
        }
        i += SSE_PIXELS;
    }

    if i < n {
        let mask = mask.map(|m| if i < m.len() { &m[i..] } else { &[] });
        fold_opacity_mask_into_alpha_scalar(&mut rgba[i * 4..], opacity, mask);
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn fold_opacity_mask_into_alpha_sse41(rgba: &mut [u8], opacity: u8, mask: Option<&[u8]>) {
    use crate::psb_simd_mul_div255::mul_div255_u8x8;
    use core::arch::x86_64::*;
    let n = rgba.len() / 4;
    let mut i = 0usize;
    while i + SSE_PIXELS <= n {
        let mut alpha = [0u8; SSE_PIXELS];
        let mut mask_values = [255u8; SSE_PIXELS];
        for lane in 0..SSE_PIXELS {
            alpha[lane] = rgba[(i + lane) * 4 + 3];
            if let Some(m) = mask {
                mask_values[lane] = m.get(i + lane).copied().unwrap_or(255);
            }
        }

        unsafe {
            let mut av = _mm_loadl_epi64(alpha.as_ptr().cast());
            if opacity != 255 {
                av = mul_div255_u8x8(av, _mm_set1_epi8(opacity as i8));
            }
            if mask.is_some() {
                let mv = _mm_loadl_epi64(mask_values.as_ptr().cast());
                av = mul_div255_u8x8(av, mv);
            }
            _mm_storel_epi64(alpha.as_mut_ptr().cast(), av);
        }

        for lane in 0..SSE_PIXELS {
            rgba[(i + lane) * 4 + 3] = alpha[lane];
        }
        i += SSE_PIXELS;
    }

    if i < n {
        let mask = mask.map(|m| if i < m.len() { &m[i..] } else { &[] });
        fold_opacity_mask_into_alpha_scalar(&mut rgba[i * 4..], opacity, mask);
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn fold_opacity_mask_into_alpha_avx2(rgba: &mut [u8], opacity: u8, mask: Option<&[u8]>) {
    use crate::psb_simd_mul_div255::mul_div255_u8x16;
    use core::arch::x86_64::*;
    let n = rgba.len() / 4;
    let mut i = 0usize;
    while i + AVX2_PIXELS <= n {
        let mut alpha = [0u8; AVX2_PIXELS];
        let mut mask_values = [255u8; AVX2_PIXELS];
        for lane in 0..AVX2_PIXELS {
            alpha[lane] = rgba[(i + lane) * 4 + 3];
            if let Some(m) = mask {
                mask_values[lane] = m.get(i + lane).copied().unwrap_or(255);
            }
        }

        unsafe {
            let mut av = _mm_loadu_si128(alpha.as_ptr().cast());
            if opacity != 255 {
                av = mul_div255_u8x16(av, _mm_set1_epi8(opacity as i8));
            }
            if mask.is_some() {
                let mv = _mm_loadu_si128(mask_values.as_ptr().cast());
                av = mul_div255_u8x16(av, mv);
            }
            _mm_storeu_si128(alpha.as_mut_ptr().cast(), av);
        }

        for lane in 0..AVX2_PIXELS {
            rgba[(i + lane) * 4 + 3] = alpha[lane];
        }
        i += AVX2_PIXELS;
    }

    if i < n {
        let mask = mask.map(|m| if i < m.len() { &m[i..] } else { &[] });
        fold_opacity_mask_into_alpha_scalar(&mut rgba[i * 4..], opacity, mask);
    }
}

#[cfg(target_arch = "aarch64")]
unsafe fn fold_opacity_mask_into_alpha_neon(rgba: &mut [u8], opacity: u8, mask: Option<&[u8]>) {
    use crate::psb_simd_mul_div255::mul_div255_u8x16_neon;
    use core::arch::aarch64::*;
    let n = rgba.len() / 4;
    let mut i = 0usize;
    while i + NEON_PIXELS <= n {
        let mut alpha = [0u8; NEON_PIXELS];
        let mut mask_values = [255u8; NEON_PIXELS];
        for lane in 0..NEON_PIXELS {
            alpha[lane] = rgba[(i + lane) * 4 + 3];
            if let Some(m) = mask {
                mask_values[lane] = m.get(i + lane).copied().unwrap_or(255);
            }
        }

        unsafe {
            let mut av = vld1q_u8(alpha.as_ptr());
            if opacity != 255 {
                av = mul_div255_u8x16_neon(av, vdupq_n_u8(opacity));
            }
            if mask.is_some() {
                av = mul_div255_u8x16_neon(av, vld1q_u8(mask_values.as_ptr()));
            }
            vst1q_u8(alpha.as_mut_ptr(), av);
        }

        for lane in 0..NEON_PIXELS {
            rgba[(i + lane) * 4 + 3] = alpha[lane];
        }
        i += NEON_PIXELS;
    }

    if i < n {
        let mask = mask.map(|m| if i < m.len() { &m[i..] } else { &[] });
        fold_opacity_mask_into_alpha_scalar(&mut rgba[i * 4..], opacity, mask);
    }
}

#[cfg(test)]
mod tests {
    use super::{fold_opacity_mask_into_alpha, fold_opacity_mask_into_alpha_scalar};
    use crate::psb_simd_mul_div255::div255_u16_exact;

    fn make_rgba_ramp(n: usize) -> Vec<u8> {
        let mut v = vec![0u8; n * 4];
        for i in 0..n {
            let o = i * 4;
            v[o] = (i % 256) as u8;
            v[o + 1] = ((i * 3) % 256) as u8;
            v[o + 2] = ((i * 5) % 256) as u8;
            v[o + 3] = ((i * 7) % 256) as u8;
        }
        v
    }

    #[test]
    fn div255_magic_matches_integer_div() {
        for x in 0u16..=65025 {
            assert_eq!(div255_u16_exact(x), (x / 255) as u8, "x={x}");
        }
    }

    #[test]
    fn fold_identity_when_opacity_255_no_mask() {
        let mut rgba = make_rgba_ramp(17);
        let before = rgba.clone();
        fold_opacity_mask_into_alpha(&mut rgba, 255, None);
        assert_eq!(rgba, before);
    }

    #[test]
    fn fold_long_row_matches_scalar_with_and_without_mask() {
        let n = 67usize; // odd, crosses SSE/AVX chunk sizes
        let opacities = [0u8, 1, 128, 200, 255];
        for &opacity in &opacities {
            let mut simd = make_rgba_ramp(n);
            let mut scalar = simd.clone();
            fold_opacity_mask_into_alpha(&mut simd, opacity, None);
            fold_opacity_mask_into_alpha_scalar(&mut scalar, opacity, None);
            assert_eq!(simd, scalar, "no-mask opacity={opacity}");

            let mask: Vec<u8> = (0..n).map(|i| ((i * 11) % 256) as u8).collect();
            let mut simd = make_rgba_ramp(n);
            let mut scalar = simd.clone();
            fold_opacity_mask_into_alpha(&mut simd, opacity, Some(&mask));
            fold_opacity_mask_into_alpha_scalar(&mut scalar, opacity, Some(&mask));
            assert_eq!(simd, scalar, "with-mask opacity={opacity}");
        }
    }

    #[test]
    fn fold_short_mask_pads_255() {
        let mut simd = make_rgba_ramp(8);
        let mut scalar = simd.clone();
        let mask = vec![128u8, 64]; // only 2 samples; rest pad 255
        fold_opacity_mask_into_alpha(&mut simd, 200, Some(&mask));
        fold_opacity_mask_into_alpha_scalar(&mut scalar, 200, Some(&mask));
        assert_eq!(simd, scalar);
    }

    #[test]
    fn fold_two_step_div_not_fused() {
        // a=3, opacity=171, mask=254:
        // step1: 3*171/255 = 2; step2: 2*254/255 = 1
        // fused: 3*171*254/(255*255) = 2 -- must not use single multiply
        let a = 3u8;
        let opacity = 171u8;
        let mask = 254u8;
        let stepwise = {
            let a1 = (a as u16 * opacity as u16) / 255;
            (a1 * mask as u16) / 255
        };
        let fused = (a as u32 * opacity as u32 * mask as u32) / (255 * 255);
        assert_ne!(
            stepwise as u32, fused,
            "fixture must distinguish stepwise from fused"
        );
        assert_eq!(stepwise, 1);

        let mut rgba = vec![10u8, 20, 30, a];
        fold_opacity_mask_into_alpha(&mut rgba, opacity, Some(&[mask]));
        assert_eq!(rgba[3], 1);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn fold_sse2_backend_matches_scalar_long_rows() {
        if !std::is_x86_feature_detected!("sse2") {
            return;
        }

        let n = 127usize;
        for mask_len in [0usize, 5, n] {
            for opacity in [0u8, 1, 77, 171, 255] {
                let mask: Vec<u8> = (0..mask_len).map(|i| ((i * 13 + 7) % 256) as u8).collect();
                let mask = (mask_len != 0).then_some(mask);
                let mask_ref = mask.as_deref();
                let mut simd = make_rgba_ramp(n);
                let mut scalar = simd.clone();
                unsafe {
                    super::fold_opacity_mask_into_alpha_sse2(&mut simd, opacity, mask_ref);
                }
                fold_opacity_mask_into_alpha_scalar(&mut scalar, opacity, mask_ref);
                assert_eq!(simd, scalar, "sse2 opacity={opacity} mask_len={mask_len}");
            }
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn fold_sse41_backend_matches_scalar_long_rows() {
        if !std::is_x86_feature_detected!("sse4.1") {
            return;
        }

        let n = 129usize;
        for mask_len in [0usize, 7, n] {
            for opacity in [0u8, 1, 77, 171, 255] {
                let mask: Vec<u8> = (0..mask_len).map(|i| ((i * 13 + 5) % 256) as u8).collect();
                let mask = (mask_len != 0).then_some(mask);
                let mask_ref = mask.as_deref();
                let mut simd = make_rgba_ramp(n);
                let mut scalar = simd.clone();
                unsafe {
                    super::fold_opacity_mask_into_alpha_sse41(&mut simd, opacity, mask_ref);
                }
                fold_opacity_mask_into_alpha_scalar(&mut scalar, opacity, mask_ref);
                assert_eq!(simd, scalar, "sse4.1 opacity={opacity} mask_len={mask_len}");
            }
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn fold_avx2_backend_matches_scalar_long_rows() {
        if !std::is_x86_feature_detected!("avx2") {
            return;
        }

        let n = 131usize;
        for mask_len in [0usize, 11, n] {
            for opacity in [0u8, 1, 77, 171, 255] {
                let mask: Vec<u8> = (0..mask_len).map(|i| ((i * 17 + 3) % 256) as u8).collect();
                let mask = (mask_len != 0).then_some(mask);
                let mask_ref = mask.as_deref();
                let mut simd = make_rgba_ramp(n);
                let mut scalar = simd.clone();
                unsafe {
                    super::fold_opacity_mask_into_alpha_avx2(&mut simd, opacity, mask_ref);
                }
                fold_opacity_mask_into_alpha_scalar(&mut scalar, opacity, mask_ref);
                assert_eq!(simd, scalar, "avx2 opacity={opacity} mask_len={mask_len}");
            }
        }
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn fold_neon_backend_matches_scalar_long_rows() {
        let n = 131usize;
        for mask_len in [0usize, 11, n] {
            for opacity in [0u8, 1, 77, 171, 255] {
                let mask: Vec<u8> = (0..mask_len).map(|i| ((i * 17 + 3) % 256) as u8).collect();
                let mask = (mask_len != 0).then_some(mask);
                let mask_ref = mask.as_deref();
                let mut simd = make_rgba_ramp(n);
                let mut scalar = simd.clone();
                unsafe {
                    super::fold_opacity_mask_into_alpha_neon(&mut simd, opacity, mask_ref);
                }
                fold_opacity_mask_into_alpha_scalar(&mut scalar, opacity, mask_ref);
                assert_eq!(simd, scalar, "neon opacity={opacity} mask_len={mask_len}");
            }
        }
    }
}
