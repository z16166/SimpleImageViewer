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

fn compose_row(
    row_in: &[f32],
    row_out: &mut [f32],
    width: u32,
    gain_rgb: &[f32],
    path: ComposeFastPath,
    color_space: HdrColorSpace,
    transfer: HdrTransferFunction,
    metadata: &HdrImageMetadata,
    headroom_span: f32,
    weight: f32,
) {
    if path == ComposeFastPath::Scalar || width < SIMD_PIXELS_PER_STEP {
        compose_row_scalar(
            row_in,
            row_out,
            width,
            gain_rgb,
            color_space,
            transfer,
            metadata,
            headroom_span,
            weight,
        );
        return;
    }

    #[cfg(target_arch = "x86_64")]
    {
        if std::arch::is_x86_feature_detected!("sse4.1") {
            unsafe {
                compose_row_sse41(
                    row_in,
                    row_out,
                    width,
                    gain_rgb,
                    path,
                    color_space,
                    transfer,
                    metadata,
                    headroom_span,
                    weight,
                );
            }
            return;
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        unsafe {
            compose_row_neon(
                row_in,
                row_out,
                width,
                gain_rgb,
                path,
                color_space,
                transfer,
                metadata,
                headroom_span,
                weight,
            );
        }
        return;
    }

    compose_row_scalar(
        row_in,
        row_out,
        width,
        gain_rgb,
        color_space,
        transfer,
        metadata,
        headroom_span,
        weight,
    );
}

pub(crate) fn compose_apple_gain_map_pixels(
    base_pixels: &[f32],
    composed_pixels: &mut [f32],
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
    force_scalar: bool,
) {
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
            let mut gain_row = GainRowLinear {
                encoded: Vec::new(),
                rgb: Vec::new(),
            };
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
                path,
                color_space,
                transfer,
                metadata,
                headroom_span,
                weight,
            );
        });
}

