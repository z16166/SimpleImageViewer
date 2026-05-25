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

//! Bit-exact `f32::powf` for fixed IEC transfer exponents via lazy direct-mapped LUTs.
//!
//! For each exponent the representable `f32` values in the high-segment domain occupy a
//! **contiguous `f32::to_bits()` range**, so `powf(x, exp)` is a single indexed load with no
//! interpolation. Tables are built once on first use (~90 MiB BT.709, ~119 MiB sRGB).

#![allow(dead_code)] // scalar helpers + domain constants are used in unit tests and for docs

use std::sync::OnceLock;

/// `(c + 0.099) / 1.099` at BT.709 linear-segment break (`c = 0.018 × 4.5`).
pub(crate) const BT709_HIGH_ADJUSTED_MIN: f32 = (0.081 + 0.099) / 1.099;
/// `(c + 0.055) / 1.055` at IEC sRGB linear-segment end (`c = 0.04045`).
pub(crate) const SRGB_HIGH_ADJUSTED_MIN: f32 = (0.04045 + 0.055) / 1.055;

const BT709_POW_EXP: f32 = 1.0 / 0.45;
const SRGB_POW_EXP: f32 = 2.4;

const BT709_MIN_BITS: u32 = 0x3e27_b753;
const BT709_MAX_BITS: u32 = 0x3f80_0000;
const SRGB_MIN_BITS: u32 = 0x3db9_4a66;
const SRGB_MAX_BITS: u32 = 0x3f80_0000;

struct PowLut {
    min_bits: u32,
    max_bits: u32,
    exp: f32,
    values: Box<[f32]>,
}

impl PowLut {
    fn build(min_bits: u32, max_bits: u32, exp: f32) -> Self {
        let len = (max_bits - min_bits + 1) as usize;
        let mut values = Vec::with_capacity(len);
        for bits in min_bits..=max_bits {
            let x = f32::from_bits(bits);
            values.push(x.powf(exp));
        }
        Self {
            min_bits,
            max_bits,
            exp,
            values: values.into_boxed_slice(),
        }
    }

    #[inline]
    fn lookup(&self, x: f32) -> f32 {
        let bits = x.to_bits();
        if bits < self.min_bits || bits > self.max_bits {
            // SIMD transfer kernels evaluate the high-segment pow for every lane before
            // blending; low-segment lanes can sit below `min_bits` and are discarded.
            return x.powf(self.exp);
        }
        self.values[(bits - self.min_bits) as usize]
    }
}

static BT709_POW_LUT: OnceLock<PowLut> = OnceLock::new();
static SRGB_POW_LUT: OnceLock<PowLut> = OnceLock::new();

#[inline]
fn bt709_lut() -> &'static PowLut {
    BT709_POW_LUT.get_or_init(|| PowLut::build(BT709_MIN_BITS, BT709_MAX_BITS, BT709_POW_EXP))
}

#[inline]
fn srgb_lut() -> &'static PowLut {
    SRGB_POW_LUT.get_or_init(|| PowLut::build(SRGB_MIN_BITS, SRGB_MAX_BITS, SRGB_POW_EXP))
}

/// `((c + 0.099) / 1.099).powf(1/0.45)` for `c` already in the BT.709 high segment.
#[inline]
pub(crate) fn pow_bt709_high_adjusted(adjusted: f32) -> f32 {
    bt709_lut().lookup(adjusted)
}

/// `((c + 0.055) / 1.055).powf(2.4)` for `c` already in the IEC sRGB high segment.
#[inline]
pub(crate) fn pow_srgb_high_adjusted(adjusted: f32) -> f32 {
    srgb_lut().lookup(adjusted)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
pub(crate) unsafe fn pow_bt709_high_adjusted4_sse41(v: core::arch::x86_64::__m128) -> core::arch::x86_64::__m128 {
    use core::arch::x86_64::*;
    unsafe {
        let lut = bt709_lut();
        let mut lanes = [0.0_f32; 4];
        _mm_storeu_ps(lanes.as_mut_ptr(), v);
        for lane in &mut lanes {
            *lane = lut.lookup(*lane);
        }
        _mm_loadu_ps(lanes.as_ptr())
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
pub(crate) unsafe fn pow_srgb_high_adjusted4_sse41(v: core::arch::x86_64::__m128) -> core::arch::x86_64::__m128 {
    use core::arch::x86_64::*;
    unsafe {
        let lut = srgb_lut();
        let mut lanes = [0.0_f32; 4];
        _mm_storeu_ps(lanes.as_mut_ptr(), v);
        for lane in &mut lanes {
            *lane = lut.lookup(*lane);
        }
        _mm_loadu_ps(lanes.as_ptr())
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
pub(crate) unsafe fn pow_bt709_high_adjusted4_neon(v: core::arch::aarch64::float32x4_t) -> core::arch::aarch64::float32x4_t {
    use core::arch::aarch64::*;
    let lut = bt709_lut();
    let mut lanes = [0.0_f32; 4];
    vst1q_f32(lanes.as_mut_ptr(), v);
    for lane in &mut lanes {
        *lane = lut.lookup(*lane);
    }
    vld1q_f32(lanes.as_ptr())
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
pub(crate) unsafe fn pow_srgb_high_adjusted4_neon(v: core::arch::aarch64::float32x4_t) -> core::arch::aarch64::float32x4_t {
    use core::arch::aarch64::*;
    let lut = srgb_lut();
    let mut lanes = [0.0_f32; 4];
    vst1q_f32(lanes.as_mut_ptr(), v);
    for lane in &mut lanes {
        *lane = lut.lookup(*lane);
    }
    vld1q_f32(lanes.as_ptr())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exhaustive_lut_matches_powf(min_bits: u32, max_bits: u32, exp: f32, lookup: fn(f32) -> f32) {
        for bits in min_bits..=max_bits {
            let x = f32::from_bits(bits);
            let expected = x.powf(exp);
            let actual = lookup(x);
            assert_eq!(
                actual.to_bits(),
                expected.to_bits(),
                "powf mismatch at bits {bits:#010x} (x={x})"
            );
        }
    }

    #[test]
    fn bt709_lut_matches_powf_exhaustive() {
        exhaustive_lut_matches_powf(
            BT709_MIN_BITS,
            BT709_MAX_BITS,
            BT709_POW_EXP,
            pow_bt709_high_adjusted,
        );
    }

    #[test]
    fn srgb_lut_matches_powf_exhaustive() {
        exhaustive_lut_matches_powf(
            SRGB_MIN_BITS,
            SRGB_MAX_BITS,
            SRGB_POW_EXP,
            pow_srgb_high_adjusted,
        );
    }
}
