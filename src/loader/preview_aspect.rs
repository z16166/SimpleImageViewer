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

use crate::loader::DecodedImage;

const PREVIEW_ASPECT_TOLERANCE: f32 = 0.08;

pub(crate) fn decoded_looks_like_black_placeholder(decoded: &DecodedImage) -> bool {
    let rgba = decoded.rgba();
    if rgba.len() < 4 {
        return true;
    }
    let pixel_count = rgba.len() / 4;
    let stride = (pixel_count / 64).max(1);
    rgba.chunks_exact(4)
        .step_by(stride)
        .all(|px| px[0] == 0 && px[1] == 0 && px[2] == 0)
}

fn preview_aspect_tolerance(
    preview_width: u32,
    preview_height: u32,
    logical_width: u32,
    logical_height: u32,
) -> f32 {
    let short_preview = preview_width.min(preview_height);
    let long_logical = logical_width.max(logical_height);
    let short_logical = logical_width.min(logical_height);
    if short_preview <= 8 || long_logical >= short_logical.saturating_mul(8) {
        return 0.35;
    }
    if short_preview <= 32 {
        return 0.20;
    }
    PREVIEW_ASPECT_TOLERANCE
}

pub fn preview_aspect_matches_logical(
    preview_width: u32,
    preview_height: u32,
    logical_width: u32,
    logical_height: u32,
) -> bool {
    if logical_width == 0 || logical_height == 0 || preview_width == 0 || preview_height == 0 {
        return false;
    }
    let logical_aspect = logical_width as f32 / logical_height as f32;
    let preview_aspect = preview_width as f32 / preview_height as f32;
    let tolerance =
        preview_aspect_tolerance(preview_width, preview_height, logical_width, logical_height);
    (logical_aspect - preview_aspect).abs() / logical_aspect.max(0.001) <= tolerance
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_square_preview_for_tall_logical_image() {
        assert!(!preview_aspect_matches_logical(512, 512, 1538, 16_380));
    }

    #[test]
    fn accepts_panorama_preview_after_integer_rounding() {
        assert!(preview_aspect_matches_logical(3, 128, 1000, 50_000));
    }

    #[test]
    fn black_placeholder_detection_samples_large_buffers() {
        let black = DecodedImage::new(4096, 2048, vec![0; 4096 * 2048 * 4]);
        assert!(decoded_looks_like_black_placeholder(&black));

        let mut rgba = vec![0; 256 * 256 * 4];
        rgba[0] = 10;
        let colored = DecodedImage::new(256, 256, rgba);
        assert!(!decoded_looks_like_black_placeholder(&colored));
    }
}
