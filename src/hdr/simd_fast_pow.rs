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
//! Callers must be compiled with `#[target_feature(enable = "...")]` and invoked only after
//! runtime feature detection (or unconditionally on aarch64).

#[cfg(target_arch = "x86_64")]
use core::arch::x86_64::*;
#[cfg(target_arch = "aarch64")]
use core::arch::aarch64::*;

/// Scalar reference for tests; positive bases only.
#[inline]
pub(crate) fn fast_powf_positive(base: f32, exponent: f32) -> f32 {
    debug_assert!(base > 0.0);
    base.powf(exponent)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
#[inline]
pub(crate) unsafe fn pow4_sse41(base: __m128, exponent: f32) -> __m128 {
    let mut lanes = [0.0_f32; 4];
    unsafe {
        _mm_storeu_ps(lanes.as_mut_ptr(), base);
    }
    for lane in &mut lanes {
        *lane = if *lane <= 0.0 {
            0.0
        } else {
            lane.powf(exponent)
        };
    }
    unsafe { _mm_loadu_ps(lanes.as_ptr()) }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
#[inline]
pub(crate) unsafe fn pow4_neon(base: float32x4_t, exponent: f32) -> float32x4_t {
    let mut lanes = [0.0_f32; 4];
    unsafe {
        vst1q_f32(lanes.as_mut_ptr(), base);
    }
    for lane in &mut lanes {
        *lane = if *lane <= 0.0 {
            0.0
        } else {
            lane.powf(exponent)
        };
    }
    unsafe { vld1q_f32(lanes.as_ptr()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXPONENTS: [f32; 6] = [
        1.0 / 2.4,
        2.4,
        1.0 / 0.45,
        1.0 / 2.2,
        5.0,
        0.25,
    ];

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
    fn pow4_sse41_matches_scalar_fast_powf() {
        if !std::arch::is_x86_feature_detected!("sse4.1") {
            return;
        }
        for exp in EXPONENTS {
            let mut x = 0.0_f32;
            while x <= 1.0 {
                let lanes = [x, (x + 0.01).min(1.0), (x + 0.02).min(1.0), (x + 0.03).min(1.0)];
                let expected: [f32; 4] = lanes.map(|v| {
                    if v <= 0.0 {
                        0.0
                    } else {
                        fast_powf_positive(v, exp)
                    }
                });
                let got = unsafe {
                    let base = _mm_set_ps(lanes[3], lanes[2], lanes[1], lanes[0]);
                    let out = pow4_sse41(base, exp);
                    let mut buf = [0.0_f32; 4];
                    _mm_storeu_ps(buf.as_mut_ptr(), out);
                    buf
                };
                for (lane, (&g, &e)) in got.iter().zip(expected.iter()).enumerate() {
                    let rel = if e > 1.0e-8 {
                        (g - e).abs() / e
                    } else {
                        g - e
                    };
                    assert!(
                        rel <= 2.0e-5,
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
    fn pow4_neon_matches_scalar_fast_powf() {
        for exp in EXPONENTS {
            let mut x = 0.0_f32;
            while x <= 1.0 {
                let lanes = [x, (x + 0.01).min(1.0), (x + 0.02).min(1.0), (x + 0.03).min(1.0)];
                let expected: [f32; 4] = lanes.map(|v| {
                    if v <= 0.0 {
                        0.0
                    } else {
                        fast_powf_positive(v, exp)
                    }
                });
                let got = unsafe {
                    let base = vld1q_f32(lanes.as_ptr());
                    let out = pow4_neon(base, exp);
                    let mut buf = [0.0_f32; 4];
                    vst1q_f32(buf.as_mut_ptr(), out);
                    buf
                };
                for (lane, (&g, &e)) in got.iter().zip(expected.iter()).enumerate() {
                    let rel = if e > 1.0e-8 {
                        (g - e).abs() / e
                    } else {
                        g - e
                    };
                    assert!(
                        rel <= 2.0e-5,
                        "lane={lane} x={} exp={exp} got={g} expected={e}",
                        lanes[lane]
                    );
                }
                x += 0.017;
            }
        }
    }
}
