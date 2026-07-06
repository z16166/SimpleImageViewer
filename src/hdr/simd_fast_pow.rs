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

//! Four-lane `x^e` helper used from tone-map and gain-map SIMD kernels (SSE4.1 / NEON).
//!
//! Uses vectorized natural log / exp (Cephes-style minimax, sse_mathfun) so all four lanes
//! stay in SIMD. Callers must be compiled with `#[target_feature(enable = "...")]` and invoked
//! only after runtime feature detection (or unconditionally on aarch64).

#[cfg(target_arch = "aarch64")]
use core::arch::aarch64::*;
#[cfg(target_arch = "x86_64")]
use core::arch::x86_64::*;

/// Scalar reference for tests; positive bases only.
#[cfg(test)]
#[inline]
pub(crate) fn fast_powf_positive(base: f32, exponent: f32) -> f32 {
    debug_assert!(base > 0.0);
    base.powf(exponent)
}

#[cfg(target_arch = "x86_64")]
mod x86 {
    use super::*;

    const INV_MANT_MASK: i32 = !0x7f80_0000_u32 as i32;
    const EXP_BIAS: i32 = 0x7f;
    const EXP_HI: f32 = 88.376_262_664_794_9;
    const EXP_LO: f32 = -88.376_262_664_794_9;
    const LOG2EF: f32 = 1.442_695_040_888_963_4;
    const LOG_Q1: f32 = -2.121_944_40e-4;
    const LOG_Q2: f32 = 0.693_359_375;
    const EXP_C1: f32 = 0.693_359_375;
    const EXP_C2: f32 = -2.121_944_40e-4;
    const SQRTHF: f32 = 0.707_106_781_186_547_5;

    #[target_feature(enable = "sse4.1")]
    #[inline]
    unsafe fn log_ps(x: __m128) -> __m128 {
        let one = _mm_set1_ps(1.0);
        let half = _mm_set1_ps(0.5);
        let min_norm = _mm_set1_ps(f32::MIN_POSITIVE);
        let mut x = _mm_max_ps(x, min_norm);

        let mut imm0 = _mm_srli_epi32(_mm_castps_si128(x), 23);
        x = _mm_and_ps(x, _mm_castsi128_ps(_mm_set1_epi32(INV_MANT_MASK)));
        x = _mm_or_ps(x, half);
        imm0 = _mm_sub_epi32(imm0, _mm_set1_epi32(EXP_BIAS));
        let mut e = _mm_add_ps(_mm_cvtepi32_ps(imm0), one);

        let mask = _mm_cmplt_ps(x, _mm_set1_ps(SQRTHF));
        let tmp = _mm_and_ps(x, mask);
        x = _mm_sub_ps(x, one);
        e = _mm_sub_ps(e, _mm_and_ps(one, mask));
        x = _mm_add_ps(x, tmp);

        let z = _mm_mul_ps(x, x);
        let mut y = _mm_set1_ps(7.037_683_629_2e-2);
        y = _mm_add_ps(_mm_mul_ps(y, x), _mm_set1_ps(-1.151_461_031_0e-1));
        y = _mm_add_ps(_mm_mul_ps(y, x), _mm_set1_ps(1.167_699_874_0e-1));
        y = _mm_add_ps(_mm_mul_ps(y, x), _mm_set1_ps(-1.242_014_084_6e-1));
        y = _mm_add_ps(_mm_mul_ps(y, x), _mm_set1_ps(1.424_932_278_7e-1));
        y = _mm_add_ps(_mm_mul_ps(y, x), _mm_set1_ps(-1.666_805_766_5e-1));
        y = _mm_add_ps(_mm_mul_ps(y, x), _mm_set1_ps(2.000_071_476_5e-1));
        y = _mm_add_ps(_mm_mul_ps(y, x), _mm_set1_ps(-2.499_999_399_3e-1));
        y = _mm_add_ps(_mm_mul_ps(y, x), _mm_set1_ps(3.333_333_117_4e-1));
        y = _mm_mul_ps(_mm_mul_ps(y, x), z);

        let mut tmp = _mm_mul_ps(e, _mm_set1_ps(LOG_Q1));
        y = _mm_add_ps(y, tmp);
        tmp = _mm_mul_ps(z, half);
        y = _mm_sub_ps(y, tmp);
        tmp = _mm_mul_ps(e, _mm_set1_ps(LOG_Q2));
        x = _mm_add_ps(x, y);
        _mm_add_ps(x, tmp)
    }

    #[target_feature(enable = "sse4.1")]
    #[inline]
    unsafe fn exp_ps(x: __m128) -> __m128 {
        let one = _mm_set1_ps(1.0);
        let half = _mm_set1_ps(0.5);
        let mut x = _mm_min_ps(_mm_max_ps(x, _mm_set1_ps(EXP_LO)), _mm_set1_ps(EXP_HI));

        let mut fx = _mm_add_ps(_mm_mul_ps(x, _mm_set1_ps(LOG2EF)), half);
        let tmp = _mm_cvtepi32_ps(_mm_cvttps_epi32(fx));
        let mask = _mm_cmpgt_ps(tmp, fx);
        fx = _mm_sub_ps(tmp, _mm_and_ps(mask, one));

        let tmp = _mm_mul_ps(fx, _mm_set1_ps(EXP_C1));
        let z = _mm_mul_ps(fx, _mm_set1_ps(EXP_C2));
        x = _mm_sub_ps(x, tmp);
        x = _mm_sub_ps(x, z);
        let z2 = _mm_mul_ps(x, x);

        let mut y = _mm_set1_ps(1.987_569_150_0e-4);
        y = _mm_add_ps(_mm_mul_ps(y, x), _mm_set1_ps(1.398_199_950_7e-3));
        y = _mm_add_ps(_mm_mul_ps(y, x), _mm_set1_ps(8.333_451_907_3e-3));
        y = _mm_add_ps(_mm_mul_ps(y, x), _mm_set1_ps(4.166_579_589_4e-2));
        y = _mm_add_ps(_mm_mul_ps(y, x), _mm_set1_ps(1.666_666_545_9e-1));
        y = _mm_add_ps(_mm_mul_ps(y, x), _mm_set1_ps(5.000_000_120_1e-1));
        y = _mm_add_ps(_mm_mul_ps(y, z2), x);
        y = _mm_add_ps(y, one);

        let imm0 = _mm_slli_epi32(
            _mm_add_epi32(_mm_cvttps_epi32(fx), _mm_set1_epi32(EXP_BIAS)),
            23,
        );
        _mm_mul_ps(y, _mm_castsi128_ps(imm0))
    }

    #[target_feature(enable = "sse4.1")]
    #[inline]
    unsafe fn pow_ps(base: __m128, exponent: f32) -> __m128 {
        let zero = _mm_setzero_ps();
        let positive = _mm_cmpgt_ps(base, zero);
        let exp_vec = _mm_set1_ps(exponent);
        let pow = unsafe { exp_ps(_mm_mul_ps(exp_vec, log_ps(base))) };
        _mm_and_ps(pow, positive)
    }

    #[target_feature(enable = "sse4.1")]
    #[inline]
    pub(super) unsafe fn pow4_sse41(base: __m128, exponent: f32) -> __m128 {
        unsafe { pow_ps(base, exponent) }
    }

    #[target_feature(enable = "sse4.1")]
    #[inline]
    pub(super) unsafe fn exp2_4_sse41(exponents: __m128) -> __m128 {
        unsafe { exp_ps(_mm_mul_ps(exponents, _mm_set1_ps(std::f32::consts::LN_2))) }
    }

    #[target_feature(enable = "sse4.1")]
    #[inline]
    pub(super) unsafe fn exp4_sse41(x: __m128) -> __m128 {
        unsafe { exp_ps(x) }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
#[inline]
pub(crate) unsafe fn pow4_sse41(base: __m128, exponent: f32) -> __m128 {
    unsafe { x86::pow4_sse41(base, exponent) }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
#[inline]
pub(crate) unsafe fn exp2_4_sse41(exponents: __m128) -> __m128 {
    unsafe { x86::exp2_4_sse41(exponents) }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
#[inline]
pub(crate) unsafe fn exp4_sse41(x: __m128) -> __m128 {
    unsafe { x86::exp4_sse41(x) }
}

#[cfg(target_arch = "aarch64")]
mod arm {
    use super::*;

    const INV_MANT_MASK: u32 = !0x7f80_0000;
    const EXP_BIAS: i32 = 0x7f;
    const EXP_HI: f32 = 88.376_262_664_794_9;
    const EXP_LO: f32 = -88.376_262_664_794_9;
    const LOG2EF: f32 = 1.442_695_040_888_963_4;
    const LOG_Q1: f32 = -2.121_944_40e-4;
    const LOG_Q2: f32 = 0.693_359_375;
    const EXP_C1: f32 = 0.693_359_375;
    const EXP_C2: f32 = -2.121_944_40e-4;
    const SQRTHF: f32 = 0.707_106_781_186_547_5;

    #[target_feature(enable = "neon")]
    #[inline]
    unsafe fn log_ps(x: float32x4_t) -> float32x4_t {
        let one = vdupq_n_f32(1.0);
        let half = vdupq_n_f32(0.5);
        let min_norm = vdupq_n_f32(f32::MIN_POSITIVE);
        let mut x = vmaxq_f32(x, min_norm);

        let bits = vreinterpretq_s32_f32(x);
        let mut imm0 = vshrq_n_s32(bits, 23);
        x = vreinterpretq_f32_s32(vandq_s32(
            bits,
            vreinterpretq_s32_u32(vdupq_n_u32(INV_MANT_MASK)),
        ));
        x = vorrq_f32(x, half);
        imm0 = vsubq_s32(imm0, vdupq_n_s32(EXP_BIAS));
        let mut e = vaddq_f32(vcvtq_f32_s32(imm0), one);

        let mask = vcltq_f32(x, vdupq_n_f32(SQRTHF));
        let tmp = vreinterpretq_f32_u32(vandq_u32(
            vreinterpretq_u32_f32(x),
            vreinterpretq_u32_f32(mask),
        ));
        x = vsubq_f32(x, one);
        e = vsubq_f32(
            e,
            vreinterpretq_f32_u32(vandq_u32(
                vreinterpretq_u32_f32(one),
                vreinterpretq_u32_f32(mask),
            )),
        );
        x = vaddq_f32(x, tmp);

        let z = vmulq_f32(x, x);
        let mut y = vdupq_n_f32(7.037_683_629_2e-2);
        y = vmlaq_f32(vdupq_n_f32(-1.151_461_031_0e-1), y, x);
        y = vmlaq_f32(vdupq_n_f32(1.167_699_874_0e-1), y, x);
        y = vmlaq_f32(vdupq_n_f32(-1.242_014_084_6e-1), y, x);
        y = vmlaq_f32(vdupq_n_f32(1.424_932_278_7e-1), y, x);
        y = vmlaq_f32(vdupq_n_f32(-1.666_805_766_5e-1), y, x);
        y = vmlaq_f32(vdupq_n_f32(2.000_071_476_5e-1), y, x);
        y = vmlaq_f32(vdupq_n_f32(-2.499_999_399_3e-1), y, x);
        y = vmlaq_f32(vdupq_n_f32(3.333_333_117_4e-1), y, x);
        y = vmulq_f32(vmulq_f32(y, x), z);

        let mut tmp = vmulq_f32(e, vdupq_n_f32(LOG_Q1));
        y = vaddq_f32(y, tmp);
        tmp = vmulq_f32(z, half);
        y = vsubq_f32(y, tmp);
        tmp = vmulq_f32(e, vdupq_n_f32(LOG_Q2));
        x = vaddq_f32(x, y);
        vaddq_f32(x, tmp)
    }

    #[target_feature(enable = "neon")]
    #[inline]
    unsafe fn exp_ps(x: float32x4_t) -> float32x4_t {
        let one = vdupq_n_f32(1.0);
        let half = vdupq_n_f32(0.5);
        let mut x = vminq_f32(vmaxq_f32(x, vdupq_n_f32(EXP_LO)), vdupq_n_f32(EXP_HI));

        let mut fx = vaddq_f32(vmulq_f32(x, vdupq_n_f32(LOG2EF)), half);
        let tmp = vcvtq_f32_s32(vcvtq_s32_f32(fx));
        let mask = vcgtq_f32(tmp, fx);
        fx = vsubq_f32(
            tmp,
            vreinterpretq_f32_u32(vandq_u32(
                vreinterpretq_u32_f32(one),
                vreinterpretq_u32_f32(mask),
            )),
        );

        let tmp = vmulq_f32(fx, vdupq_n_f32(EXP_C1));
        let z = vmulq_f32(fx, vdupq_n_f32(EXP_C2));
        x = vsubq_f32(x, tmp);
        x = vsubq_f32(x, z);
        let z2 = vmulq_f32(x, x);

        let mut y = vdupq_n_f32(1.987_569_150_0e-4);
        y = vmlaq_f32(vdupq_n_f32(1.398_199_950_7e-3), y, x);
        y = vmlaq_f32(vdupq_n_f32(8.333_451_907_3e-3), y, x);
        y = vmlaq_f32(vdupq_n_f32(4.166_579_589_4e-2), y, x);
        y = vmlaq_f32(vdupq_n_f32(1.666_666_545_9e-1), y, x);
        y = vmlaq_f32(vdupq_n_f32(5.000_000_120_1e-1), y, x);
        y = vaddq_f32(vmulq_f32(y, z2), x);
        y = vaddq_f32(y, one);

        let imm0 = vshlq_n_s32(vaddq_s32(vcvtq_s32_f32(fx), vdupq_n_s32(EXP_BIAS)), 23);
        vmulq_f32(y, vreinterpretq_f32_s32(imm0))
    }

    #[target_feature(enable = "neon")]
    #[inline]
    unsafe fn pow_ps(base: float32x4_t, exponent: f32) -> float32x4_t {
        let zero = vdupq_n_f32(0.0);
        let positive = vcgtq_f32(base, zero);
        let exp_vec = vdupq_n_f32(exponent);
        let pow = exp_ps(vmulq_f32(exp_vec, log_ps(base)));
        vreinterpretq_f32_u32(vandq_u32(
            vreinterpretq_u32_f32(pow),
            vreinterpretq_u32_f32(positive),
        ))
    }

    #[target_feature(enable = "neon")]
    #[inline]
    pub(super) unsafe fn pow4_neon(base: float32x4_t, exponent: f32) -> float32x4_t {
        pow_ps(base, exponent)
    }

    #[target_feature(enable = "neon")]
    #[inline]
    pub(super) unsafe fn exp2_4_neon(exponents: float32x4_t) -> float32x4_t {
        exp_ps(vmulq_f32(
            exponents,
            vdupq_n_f32(std::f32::consts::LN_2),
        ))
    }

    #[target_feature(enable = "neon")]
    #[inline]
    pub(super) unsafe fn exp4_neon(x: float32x4_t) -> float32x4_t {
        exp_ps(x)
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
#[inline]
pub(crate) unsafe fn pow4_neon(base: float32x4_t, exponent: f32) -> float32x4_t {
    unsafe { arm::pow4_neon(base, exponent) }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
#[inline]
pub(crate) unsafe fn exp2_4_neon(exponents: float32x4_t) -> float32x4_t {
    unsafe { arm::exp2_4_neon(exponents) }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
#[inline]
pub(crate) unsafe fn exp4_neon(x: float32x4_t) -> float32x4_t {
    unsafe { arm::exp4_neon(x) }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXPONENTS: [f32; 6] = [1.0 / 2.4, 2.4, 1.0 / 0.45, 1.0 / 2.2, 5.0, 0.25];

    #[test]
    fn scalar_fast_powf_matches_std_powf_on_tone_map_range() {
        for exp in EXPONENTS {
            let mut x = 0.0_f32;
            while x <= 1.0 {
                let approx = fast_powf_positive(x.max(f32::MIN_POSITIVE), exp);
                let exact = x.max(f32::MIN_POSITIVE).powf(exp);
                let rel = if exact > 1.0e-8 {
                    (approx - exact).abs() / exact
                } else {
                    approx - exact
                };
                assert!(
                    rel <= 2.0e-5,
                    "x={x} exp={exp} approx={approx} exact={exact} rel={rel}"
                );
                x += 0.013;
            }
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn pow4_sse41_matches_std_powf() {
        if !std::arch::is_x86_feature_detected!("sse4.1") {
            return;
        }
        for exp in EXPONENTS {
            let mut x = 0.0_f32;
            while x <= 1.0 {
                let lanes = [
                    x,
                    (x + 0.01).min(1.0),
                    (x + 0.02).min(1.0),
                    (x + 0.03).min(1.0),
                ];
                let expected: [f32; 4] = lanes.map(|v| if v <= 0.0 { 0.0 } else { v.powf(exp) });
                let got = unsafe {
                    let base = _mm_set_ps(lanes[3], lanes[2], lanes[1], lanes[0]);
                    let out = pow4_sse41(base, exp);
                    let mut buf = [0.0_f32; 4];
                    _mm_storeu_ps(buf.as_mut_ptr(), out);
                    buf
                };
                for (lane, (&g, &e)) in got.iter().zip(expected.iter()).enumerate() {
                    let rel = if e > 1.0e-8 { (g - e).abs() / e } else { g - e };
                    assert!(
                        rel <= 2.0e-4,
                        "lane={lane} x={} exp={exp} got={g} expected={e}",
                        lanes[lane]
                    );
                }
                x += 0.017;
            }
        }
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn pow4_neon_matches_std_powf() {
        for exp in EXPONENTS {
            let mut x = 0.0_f32;
            while x <= 1.0 {
                let lanes = [
                    x,
                    (x + 0.01).min(1.0),
                    (x + 0.02).min(1.0),
                    (x + 0.03).min(1.0),
                ];
                let expected: [f32; 4] = lanes.map(|v| if v <= 0.0 { 0.0 } else { v.powf(exp) });
                let got = unsafe {
                    let base = vld1q_f32(lanes.as_ptr());
                    let out = pow4_neon(base, exp);
                    let mut buf = [0.0_f32; 4];
                    vst1q_f32(buf.as_mut_ptr(), out);
                    buf
                };
                for (lane, (&g, &e)) in got.iter().zip(expected.iter()).enumerate() {
                    let rel = if e > 1.0e-8 { (g - e).abs() / e } else { g - e };
                    assert!(
                        rel <= 2.0e-4,
                        "lane={lane} x={} exp={exp} got={g} expected={e}",
                        lanes[lane]
                    );
                }
                x += 0.017;
            }
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn exp2_4_sse41_matches_std_exp2() {
        if !std::arch::is_x86_feature_detected!("sse4.1") {
            return;
        }
        let mut x = -4.0_f32;
        while x <= 4.0 {
            let lanes = [x, x + 0.25, x + 0.5, x + 0.75];
            let expected: [f32; 4] = lanes.map(|v| 2.0_f32.powf(v));
            let got = unsafe {
                let exponents = _mm_set_ps(lanes[3], lanes[2], lanes[1], lanes[0]);
                let out = exp2_4_sse41(exponents);
                let mut buf = [0.0_f32; 4];
                _mm_storeu_ps(buf.as_mut_ptr(), out);
                buf
            };
            for (lane, (&g, &e)) in got.iter().zip(expected.iter()).enumerate() {
                let rel = if e.abs() > 1.0e-8 {
                    (g - e).abs() / e.abs()
                } else {
                    g - e
                };
                assert!(
                    rel <= 2.0e-4,
                    "lane={lane} x={} got={g} expected={e}",
                    lanes[lane]
                );
            }
            x += 0.2;
        }
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn exp2_4_neon_matches_std_exp2() {
        let mut x = -4.0_f32;
        while x <= 4.0 {
            let lanes = [x, x + 0.25, x + 0.5, x + 0.75];
            let expected: [f32; 4] = lanes.map(|v| 2.0_f32.powf(v));
            let got = unsafe {
                let exponents = vld1q_f32(lanes.as_ptr());
                let out = exp2_4_neon(exponents);
                let mut buf = [0.0_f32; 4];
                vst1q_f32(buf.as_mut_ptr(), out);
                buf
            };
            for (lane, (&g, &e)) in got.iter().zip(expected.iter()).enumerate() {
                let rel = if e.abs() > 1.0e-8 {
                    (g - e).abs() / e.abs()
                } else {
                    g - e
                };
                assert!(
                    rel <= 2.0e-4,
                    "lane={lane} x={} got={g} expected={e}",
                    lanes[lane]
                );
            }
            x += 0.2;
        }
    }
}
