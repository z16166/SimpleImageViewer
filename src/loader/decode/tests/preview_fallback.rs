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

use std::sync::Arc;

use crate::hdr::types::{HdrColorSpace, HdrImageMetadata, HdrPixelFormat};

#[test]
fn sdr_preview_to_hdr_preview_uses_linear_srgb_metadata() {
    let sdr = vec![255_u8, 127, 0, 255];
    let hdr =
        super::super::sdr_preview_to_hdr_preview(1, 1, &sdr).expect("valid SDR preview buffer");
    assert_eq!(hdr.color_space, HdrColorSpace::LinearSrgb);
    assert_eq!(
        hdr.metadata.transfer_function,
        crate::hdr::types::HdrTransferFunction::Linear
    );
    assert_eq!(
        hdr.metadata.color_profile,
        crate::hdr::types::HdrColorProfile::LinearSrgb
    );
    assert_eq!(hdr.rgba_f32.len(), 4);
    assert!(hdr.rgba_f32[0] > hdr.rgba_f32[1]);
}

#[test]
fn sdr_preview_to_hdr_preview_rejects_mismatched_rgba_length() {
    let err = super::super::sdr_preview_to_hdr_preview(2, 2, &[255, 0, 0, 255, 255, 0, 0, 255])
        .expect_err("truncated buffer must be rejected");
    assert!(err.contains("RGBA length mismatch"));
}

struct StubHdrSource {
    width: u32,
    height: u32,
    color_space: HdrColorSpace,
    hdr_result: Result<crate::hdr::types::HdrImageBuffer, String>,
    sdr_result: Result<(u32, u32, Vec<u8>), String>,
}

impl crate::hdr::tiled::HdrTiledSource for StubHdrSource {
    fn source_kind(&self) -> crate::hdr::tiled::HdrTiledSourceKind {
        crate::hdr::tiled::HdrTiledSourceKind::InMemory
    }

    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn color_space(&self) -> HdrColorSpace {
        self.color_space
    }

    fn generate_hdr_preview(
        &self,
        _max_w: u32,
        _max_h: u32,
    ) -> Result<crate::hdr::types::HdrImageBuffer, String> {
        self.hdr_result.clone()
    }

    fn generate_sdr_preview(
        &self,
        _max_w: u32,
        _max_h: u32,
    ) -> Result<(u32, u32, Vec<u8>), String> {
        self.sdr_result.clone()
    }

    fn extract_tile_rgba32f_arc(
        &self,
        _x: u32,
        _y: u32,
        _width: u32,
        _height: u32,
    ) -> Result<Arc<crate::hdr::tiled::HdrTileBuffer>, String> {
        Err("not used in this test".to_string())
    }
}

struct StubFallbackSource {
    width: u32,
    height: u32,
}

impl crate::loader::TiledImageSource for StubFallbackSource {
    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn extract_tile(&self, _x: u32, _y: u32, w: u32, h: u32) -> Arc<Vec<u8>> {
        Arc::new(vec![0; (w * h * 4) as usize])
    }

    fn generate_preview(&self, _max_w: u32, _max_h: u32) -> (u32, u32, Vec<u8>) {
        (1, 1, vec![0, 0, 0, 255])
    }

    fn full_pixels(&self) -> Option<Arc<Vec<u8>>> {
        None
    }
}

#[test]
fn hdr_mode_err_skips_source_sdr_preview_path() {
    let hdr_source: Arc<dyn crate::hdr::tiled::HdrTiledSource> = Arc::new(StubHdrSource {
        width: 16,
        height: 16,
        color_space: HdrColorSpace::Rec2020Linear,
        hdr_result: Err("decode failed".to_string()),
        sdr_result: Ok((
            2,
            2,
            vec![
                10, 20, 30, 255, 10, 20, 30, 255, 10, 20, 30, 255, 10, 20, 30, 255,
            ],
        )),
    });
    let fallback_source: Arc<dyn crate::loader::TiledImageSource> = Arc::new(StubFallbackSource {
        width: 16,
        height: 16,
    });
    let image_data = crate::loader::ImageData::HdrTiled {
        hdr: hdr_source,
        fallback: fallback_source,
    };

    let (preview, hdr_preview) =
        super::super::compute_hdr_tiled_initial_preview_for_test("stub.exr", &image_data, 2.0);
    assert!(preview.is_none());
    let hdr_preview = hdr_preview.expect("expects fallback-generated HDR preview");
    assert_eq!(hdr_preview.color_space, HdrColorSpace::LinearSrgb);
}

#[test]
fn hdr_mode_zero_sized_hdr_can_use_source_sdr_preview() {
    let hdr_source: Arc<dyn crate::hdr::tiled::HdrTiledSource> = Arc::new(StubHdrSource {
        width: 16,
        height: 16,
        color_space: HdrColorSpace::Rec2020Linear,
        hdr_result: Ok(crate::hdr::types::HdrImageBuffer {
            width: 0,
            height: 0,
            format: HdrPixelFormat::Rgba32Float,
            color_space: HdrColorSpace::Rec2020Linear,
            metadata: HdrImageMetadata::from_color_space(HdrColorSpace::Rec2020Linear),
            rgba_f32: Arc::new(Vec::new()),
        }),
        sdr_result: Ok((1, 1, vec![255, 255, 255, 255])),
    });
    let fallback_source: Arc<dyn crate::loader::TiledImageSource> = Arc::new(StubFallbackSource {
        width: 16,
        height: 16,
    });
    let image_data = crate::loader::ImageData::HdrTiled {
        hdr: hdr_source,
        fallback: fallback_source,
    };

    let (_preview, hdr_preview) =
        super::super::compute_hdr_tiled_initial_preview_for_test("stub-zero.exr", &image_data, 2.0);
    let hdr_preview = hdr_preview.expect("source SDR fallback should produce HDR preview");
    assert_eq!(hdr_preview.color_space, HdrColorSpace::LinearSrgb);
    assert_eq!(hdr_preview.width, 1);
    assert_eq!(hdr_preview.height, 1);
}
