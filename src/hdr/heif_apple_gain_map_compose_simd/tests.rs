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

use super::core::{ComposeRowTransform, classify_fast_path};
use super::{
    AppleGainMapComposePixels, GainRowLinear, compose_apple_gain_map_pixels, compose_row_scalar,
    precompute_gain_row_linear,
};
#[cfg(target_arch = "x86_64")]
use super::{load_rgb_interleaved4_sse41, store_rgb_interleaved4_sse41};
use crate::hdr::decode::bt709_nonlinear_channel_to_linear;
use crate::hdr::gain_map::sample_gain_map_rgb;
use crate::hdr::types::{HdrColorSpace, HdrImageMetadata, HdrTransferFunction};

fn precompute_gain_row_linear_legacy(
    gain_rgba: &[u8],
    gain_w: u32,
    gain_h: u32,
    y: u32,
    width: u32,
    height: u32,
    out: &mut GainRowLinear,
) {
    let w = width as usize;
    out.ensure_capacity(w);
    for x in 0..width {
        let raw = sample_gain_map_rgb(gain_rgba, gain_w, gain_h, x, y, width, height);
        let base = x as usize * 3;
        out.rgb[base] = bt709_nonlinear_channel_to_linear(raw[0]);
        out.rgb[base + 1] = bt709_nonlinear_channel_to_linear(raw[1]);
        out.rgb[base + 2] = bt709_nonlinear_channel_to_linear(raw[2]);
    }
}

fn compose_image_legacy_reference(
    base_pixels: &[f32],
    width: u32,
    height: u32,
    gain_rgba: &[u8],
    gain_w: u32,
    gain_h: u32,
    color_space: HdrColorSpace,
    transfer: HdrTransferFunction,
    metadata: &HdrImageMetadata,
    headroom_span: f32,
    weight: f32,
) -> Vec<f32> {
    let mut out = vec![0.0_f32; base_pixels.len()];
    let row_stride = width as usize * 4;
    let mut gain_row = GainRowLinear {
        encoded: Vec::new(),
        rgb: Vec::new(),
    };
    for y in 0..height {
        let start = y as usize * row_stride;
        let end = start + row_stride;
        precompute_gain_row_linear_legacy(
            gain_rgba,
            gain_w,
            gain_h,
            y,
            width,
            height,
            &mut gain_row,
        );
        compose_row_scalar(
            &base_pixels[start..end],
            &mut out[start..end],
            width,
            &gain_row.rgb,
            ComposeRowTransform {
                path: classify_fast_path(color_space, transfer, metadata),
                color_space,
                transfer,
                metadata,
                headroom_span,
                weight,
            },
        );
    }
    out
}

#[test]
fn precompute_gain_row_matches_legacy_reference() {
    const W: u32 = 67;
    const H: u32 = 19;
    const GAIN_W: u32 = 17;
    const GAIN_H: u32 = 11;
    let gain_rgba: Vec<u8> = (0..GAIN_W as usize * GAIN_H as usize * 4)
        .map(|i| ((i * 13 + 7) % 256) as u8)
        .collect();
    for y in 0..H {
        let mut legacy = GainRowLinear {
            encoded: Vec::new(),
            rgb: Vec::new(),
        };
        let mut optimized = GainRowLinear {
            encoded: Vec::new(),
            rgb: Vec::new(),
        };
        precompute_gain_row_linear_legacy(&gain_rgba, GAIN_W, GAIN_H, y, W, H, &mut legacy);
        precompute_gain_row_linear(&gain_rgba, GAIN_W, GAIN_H, y, W, H, &mut optimized);
        assert_eq!(
            legacy.rgb[..W as usize * 3],
            optimized.rgb[..W as usize * 3],
            "row {y} mismatch"
        );
    }
}

#[test]
fn compose_apple_gain_map_pixels_matches_legacy_reference() {
    const W: u32 = 67;
    const H: u32 = 19;
    const GAIN_W: u32 = 17;
    const GAIN_H: u32 = 11;
    let pixel_count = W as usize * H as usize * 4;
    let base_pixels: Vec<f32> = (0..pixel_count)
        .map(|i| ((i * 17 + 3) % 997) as f32 / 997.0)
        .collect();
    let gain_rgba: Vec<u8> = (0..GAIN_W as usize * GAIN_H as usize * 4)
        .map(|i| ((i * 13 + 7) % 256) as u8)
        .collect();
    let headroom_span = 3.0;
    let weight = 0.75;
    let metadata = HdrImageMetadata::from_color_space(HdrColorSpace::DisplayP3Linear);

    let legacy = compose_image_legacy_reference(
        &base_pixels,
        W,
        H,
        &gain_rgba,
        GAIN_W,
        GAIN_H,
        HdrColorSpace::DisplayP3Linear,
        HdrTransferFunction::Srgb,
        &metadata,
        headroom_span,
        weight,
    );
    let mut optimized = vec![0.0_f32; pixel_count];
    compose_apple_gain_map_pixels(AppleGainMapComposePixels {
        base_pixels: &base_pixels,
        composed_pixels: &mut optimized,
        width: W,
        height: H,
        gain_rgba: &gain_rgba,
        gain_w: GAIN_W,
        gain_h: GAIN_H,
        color_space: HdrColorSpace::DisplayP3Linear,
        transfer: HdrTransferFunction::Srgb,
        metadata: &metadata,
        headroom_span,
        weight,
        force_scalar: false,
    });
    assert_eq!(legacy, optimized);
}

#[test]
fn simd_compose_matches_scalar_for_common_heic_paths() {
    const W: u32 = 67;
    const H: u32 = 3;
    let pixel_count = W as usize * H as usize * 4;
    let base_pixels: Vec<f32> = (0..pixel_count)
        .map(|i| ((i * 17 + 3) % 997) as f32 / 997.0)
        .collect();
    let gain_rgba: Vec<u8> = (0..W as usize * H as usize * 4)
        .map(|i| ((i * 13 + 7) % 256) as u8)
        .collect();
    let headroom_span = 3.0;
    let weight = 0.75;

    let cases = [
        (
            HdrColorSpace::LinearSrgb,
            HdrTransferFunction::Srgb,
            HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
        ),
        (
            HdrColorSpace::DisplayP3Linear,
            HdrTransferFunction::Srgb,
            HdrImageMetadata::from_color_space(HdrColorSpace::DisplayP3Linear),
        ),
        (
            HdrColorSpace::LinearSrgb,
            HdrTransferFunction::Bt709,
            HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
        ),
        (
            HdrColorSpace::DisplayP3Linear,
            HdrTransferFunction::Bt709,
            HdrImageMetadata::from_color_space(HdrColorSpace::DisplayP3Linear),
        ),
    ];

    for (color_space, transfer, metadata) in cases {
        let reference = compose_image_legacy_reference(
            &base_pixels,
            W,
            H,
            &gain_rgba,
            W,
            H,
            color_space,
            transfer,
            &metadata,
            headroom_span,
            weight,
        );
        let mut optimized = vec![0.0_f32; pixel_count];
        compose_apple_gain_map_pixels(AppleGainMapComposePixels {
            base_pixels: &base_pixels,
            composed_pixels: &mut optimized,
            width: W,
            height: H,
            gain_rgba: &gain_rgba,
            gain_w: W,
            gain_h: H,
            color_space,
            transfer,
            metadata: &metadata,
            headroom_span,
            weight,
            force_scalar: false,
        });
        assert_eq!(
            reference, optimized,
            "parity failed for {color_space:?} + {transfer:?}"
        );
    }
}

#[cfg(target_arch = "x86_64")]
fn gather_gain_rgb4_scalar_reference(
    interleaved: &[f32],
    pixel_offset: usize,
) -> ([f32; 4], [f32; 4], [f32; 4]) {
    let base = pixel_offset * 3;
    let mut r = [0.0_f32; 4];
    let mut g = [0.0_f32; 4];
    let mut b = [0.0_f32; 4];
    for pixel in 0..4 {
        let src = base + pixel * 3;
        r[pixel] = interleaved[src];
        g[pixel] = interleaved[src + 1];
        b[pixel] = interleaved[src + 2];
    }
    (r, g, b)
}

#[cfg(target_arch = "x86_64")]
fn planar_to_interleaved_reference(planar: &([f32; 4], [f32; 4], [f32; 4])) -> [f32; 12] {
    let (r, g, b) = planar;
    let mut out = [0.0_f32; 12];
    for pixel in 0..4 {
        out[pixel * 3] = r[pixel];
        out[pixel * 3 + 1] = g[pixel];
        out[pixel * 3 + 2] = b[pixel];
    }
    out
}

#[cfg(target_arch = "x86_64")]
#[test]
fn sse41_rgb_interleaved_load_store_matches_scalar_reference() {
    use core::arch::x86_64::*;

    if !std::arch::is_x86_feature_detected!("sse4.1") {
        return;
    }

    let interleaved: Vec<f32> = (0..12 * 8)
        .map(|i| (i as f32 * 0.125 - 3.0).sin() * 0.5 + 0.5)
        .collect();

    let pattern: [f32; 12] = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0];
    let expected_pattern = gather_gain_rgb4_scalar_reference(&pattern, 0);
    let (pr, pg, pb) = unsafe { load_rgb_interleaved4_sse41(pattern.as_ptr()) };
    let mut pr_lanes = [0.0_f32; 4];
    let mut pg_lanes = [0.0_f32; 4];
    let mut pb_lanes = [0.0_f32; 4];
    unsafe {
        _mm_storeu_ps(pr_lanes.as_mut_ptr(), pr);
        _mm_storeu_ps(pg_lanes.as_mut_ptr(), pg);
        _mm_storeu_ps(pb_lanes.as_mut_ptr(), pb);
    }
    assert_eq!(expected_pattern.0, pr_lanes, "pattern R");
    assert_eq!(expected_pattern.1, pg_lanes, "pattern G");
    assert_eq!(expected_pattern.2, pb_lanes, "pattern B");
    let mut pattern_roundtrip = [0.0_f32; 12];
    unsafe {
        store_rgb_interleaved4_sse41(pattern_roundtrip.as_mut_ptr(), pr, pg, pb);
    }
    assert_eq!(pattern, pattern_roundtrip, "pattern roundtrip");

    for block in 0..8usize {
        let offset = block * 12;
        let chunk = &interleaved[offset..offset + 12];
        let expected = gather_gain_rgb4_scalar_reference(chunk, 0);

        let (r, g, b) = unsafe { load_rgb_interleaved4_sse41(chunk.as_ptr()) };
        let mut r_lanes = [0.0_f32; 4];
        let mut g_lanes = [0.0_f32; 4];
        let mut b_lanes = [0.0_f32; 4];
        unsafe {
            _mm_storeu_ps(r_lanes.as_mut_ptr(), r);
            _mm_storeu_ps(g_lanes.as_mut_ptr(), g);
            _mm_storeu_ps(b_lanes.as_mut_ptr(), b);
        }
        assert_eq!(expected.0, r_lanes, "load R mismatch at block {block}");
        assert_eq!(expected.1, g_lanes, "load G mismatch at block {block}");
        assert_eq!(expected.2, b_lanes, "load B mismatch at block {block}");

        let mut roundtrip = [0.0_f32; 12];
        unsafe {
            store_rgb_interleaved4_sse41(roundtrip.as_mut_ptr(), r, g, b);
        }
        assert_eq!(
            chunk, &roundtrip,
            "store roundtrip mismatch at block {block}"
        );

        let reference_interleaved = planar_to_interleaved_reference(&expected);
        assert_eq!(
            chunk, reference_interleaved,
            "reference interleave mismatch at block {block}"
        );
    }
}
