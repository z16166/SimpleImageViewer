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

//! for RGB-interleaved gain rows (aarch64 already uses `vld3q_f32` / `vst3q_f32`).

#[cfg(target_arch = "aarch64")]
use super::core::compose_row_neon;
#[cfg(target_arch = "x86_64")]
use super::core::compose_row_sse41;
use super::core::{
    ComposeFastPath, ComposeRowTransform, GainRowLinear, SIMD_PIXELS_PER_STEP, classify_fast_path,
    compose_row_scalar, precompute_gain_row_linear,
};
#[cfg(target_arch = "x86_64")]
use super::core_avx2::compose_row_avx2;
use crate::hdr::types::{HdrColorSpace, HdrImageMetadata, HdrTransferFunction};
use rayon::prelude::*;
use std::cell::RefCell;

thread_local! {
    static GAIN_ROW_SCRATCH: RefCell<GainRowLinear> = const { RefCell::new(GainRowLinear {
        encoded: Vec::new(),
        rgb: Vec::new(),
    }) };
}

fn compose_row(
    row_in: &[f32],
    row_out: &mut [f32],
    width: u32,
    gain_rgb: &[f32],
    transform: ComposeRowTransform<'_>,
) {
    let path = transform.path;
    if path == ComposeFastPath::Scalar || width < SIMD_PIXELS_PER_STEP {
        compose_row_scalar(row_in, row_out, width, gain_rgb, transform);
        return;
    }

    #[cfg(target_arch = "x86_64")]
    {
        if std::arch::is_x86_feature_detected!("avx2") {
            unsafe {
                compose_row_avx2(row_in, row_out, width, gain_rgb, transform);
            }
            return;
        } else if std::arch::is_x86_feature_detected!("sse4.1") {
            unsafe {
                compose_row_sse41(row_in, row_out, width, gain_rgb, transform);
            }
            return;
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        unsafe {
            compose_row_neon(row_in, row_out, width, gain_rgb, transform);
        }
        return;
    }

    #[cfg(not(target_arch = "aarch64"))]
    compose_row_scalar(row_in, row_out, width, gain_rgb, transform);
}

pub(crate) struct AppleGainMapComposePixels<'a> {
    pub(crate) base_pixels: &'a [f32],
    pub(crate) composed_pixels: &'a mut [f32],
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) gain_rgba: &'a [u8],
    pub(crate) gain_w: u32,
    pub(crate) gain_h: u32,
    pub(crate) color_space: HdrColorSpace,
    pub(crate) transfer: HdrTransferFunction,
    pub(crate) metadata: &'a HdrImageMetadata,
    pub(crate) headroom_span: f32,
    pub(crate) weight: f32,
    pub(crate) force_scalar: bool,
}

pub(crate) fn compose_apple_gain_map_pixels(input: AppleGainMapComposePixels<'_>) {
    let AppleGainMapComposePixels {
        base_pixels,
        composed_pixels,
        width,
        height,
        gain_rgba,
        gain_w,
        gain_h,
        color_space,
        transfer,
        metadata,
        headroom_span,
        weight,
        force_scalar,
    } = input;
    if width == 0 || height == 0 {
        return;
    }

    let path = if force_scalar {
        ComposeFastPath::Scalar
    } else {
        classify_fast_path(color_space, transfer, metadata)
    };
    let row_stride = width as usize * 4;

    composed_pixels
        .par_chunks_mut(row_stride)
        .zip(base_pixels.par_chunks(row_stride))
        .enumerate()
        .for_each(|(y, (row_out, row_in))| {
            GAIN_ROW_SCRATCH.with(|scratch| {
                let mut gain_row = scratch.borrow_mut();
                gain_row.ensure_capacity(width as usize);
                precompute_gain_row_linear(
                    gain_rgba,
                    gain_w,
                    gain_h,
                    y as u32,
                    width,
                    height,
                    &mut gain_row,
                );
                compose_row(
                    row_in,
                    row_out,
                    width,
                    &gain_row.rgb,
                    ComposeRowTransform {
                        path,
                        color_space,
                        transfer,
                        metadata,
                        headroom_span,
                        weight,
                    },
                );
            });
        });
}
