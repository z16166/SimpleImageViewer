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

//! `ImageLoader` interaction and HDR tile worker behavior.

use std::sync::Arc;
use std::time::Duration;

use crate::hdr::types::{HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat};
use crate::loader::{
    ImageLoader, LoadResult, LoaderOutput, PreviewBundle, TileDecodeSource, TilePixelKind,
};

#[test]
fn image_request_stays_inflight_until_ui_finishes_installing_result() {
    let mut loader = ImageLoader::new();
    let index = 7;
    loader.test_register_inflight(index);

    let load_result = LoadResult {
        index,
        decode_profile: crate::loader::decode_profile_stub(),
        source_key: 0,
        result: Err("synthetic".to_string()),
        preview_bundle: PreviewBundle::initial(),
        ultra_hdr_capacity_sensitive: false,
        sdr_fallback_is_placeholder: false,
        target_hdr_capacity: 1.0,
        raw_osd: None,
        uploaded_planes: None,
        staged_gpu_plane_upload: false,
        device_id: None,
    };
    loader.test_send_loader_output(LoaderOutput::Image(Box::new(load_result)));

    let output = loader.poll().expect("polled image result");
    assert!(matches!(output, LoaderOutput::Image(_)));
    assert!(loader.is_loading(index));

    loader.finish_image_request(index);
    assert!(!loader.is_loading(index));
}

#[test]
fn request_tile_decodes_hdr_source_into_hdr_cache_and_reports_hdr_ready() {
    let loader = ImageLoader::new();
    let source: Arc<dyn crate::hdr::tiled::HdrTiledSource> = Arc::new(
        crate::hdr::tiled::HdrTiledImageSource::new(HdrImageBuffer {
            width: crate::tile_cache::get_tile_size(),
            height: crate::tile_cache::get_tile_size(),
            format: HdrPixelFormat::Rgba32Float,
            color_space: HdrColorSpace::LinearSrgb,
            metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
            rgba_f32: Arc::new(vec![
                0.25;
                crate::tile_cache::get_tile_size() as usize
                    * crate::tile_cache::get_tile_size() as usize
                    * 4
            ]),
        })
        .expect("build HDR tiled source"),
    );

    loader.request_tile(
        3,
        crate::loader::decode_profile_stub(),
        1,
        TileDecodeSource::Hdr(Arc::clone(&source)),
        0,
        0,
    );

    let output = loader
        .rx
        .recv_timeout(Duration::from_secs(2))
        .expect("HDR tile ready result");
    match output {
        LoaderOutput::Tile(tile) => {
            assert_eq!(tile.index, 3);
            assert_eq!(tile.col, 0);
            assert_eq!(tile.row, 0);
            assert_eq!(tile.pixel_kind, TilePixelKind::Hdr);
        }
        _ => panic!("expected HDR tile-ready output"),
    }

    assert!(
        source
            .cached_tile_rgba32f_arc(
                0,
                0,
                crate::tile_cache::get_tile_size(),
                crate::tile_cache::get_tile_size(),
            )
            .is_some()
    );
}

#[test]
fn request_tile_reports_ready_when_hdr_tile_is_already_cached() {
    let loader = ImageLoader::new();
    let source: Arc<dyn crate::hdr::tiled::HdrTiledSource> = Arc::new(
        crate::hdr::tiled::HdrTiledImageSource::new(HdrImageBuffer {
            width: crate::tile_cache::get_tile_size(),
            height: crate::tile_cache::get_tile_size(),
            format: HdrPixelFormat::Rgba32Float,
            color_space: HdrColorSpace::LinearSrgb,
            metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
            rgba_f32: Arc::new(vec![
                0.25;
                crate::tile_cache::get_tile_size() as usize
                    * crate::tile_cache::get_tile_size() as usize
                    * 4
            ]),
        })
        .expect("build HDR tiled source"),
    );
    source
        .extract_tile_rgba32f_arc(
            0,
            0,
            crate::tile_cache::get_tile_size(),
            crate::tile_cache::get_tile_size(),
        )
        .expect("seed HDR tile cache");

    loader.request_tile(
        3,
        crate::loader::decode_profile_stub(),
        1,
        TileDecodeSource::Hdr(source),
        0,
        0,
    );

    let output = loader
        .rx
        .recv_timeout(Duration::from_secs(2))
        .expect("HDR cached tile ready result");
    match output {
        LoaderOutput::Tile(tile) => {
            assert_eq!(tile.index, 3);
            assert_eq!(tile.col, 0);
            assert_eq!(tile.row, 0);
            assert_eq!(tile.pixel_kind, TilePixelKind::Hdr);
        }
        _ => panic!("expected HDR tile-ready output"),
    }
}

struct FailingHdrTiledSource;

impl crate::hdr::tiled::HdrTiledSource for FailingHdrTiledSource {
    fn source_kind(&self) -> crate::hdr::tiled::HdrTiledSourceKind {
        crate::hdr::tiled::HdrTiledSourceKind::DiskBacked
    }

    fn width(&self) -> u32 {
        crate::tile_cache::get_tile_size()
    }

    fn height(&self) -> u32 {
        crate::tile_cache::get_tile_size()
    }

    fn color_space(&self) -> HdrColorSpace {
        HdrColorSpace::LinearSrgb
    }

    fn generate_hdr_preview(&self, _max_w: u32, _max_h: u32) -> Result<HdrImageBuffer, String> {
        Err("preview failed".to_string())
    }

    fn generate_sdr_preview(
        &self,
        _max_w: u32,
        _max_h: u32,
    ) -> Result<(u32, u32, Vec<u8>), String> {
        Err("preview failed".to_string())
    }

    fn extract_tile_rgba32f_arc(
        &self,
        _x: u32,
        _y: u32,
        _width: u32,
        _height: u32,
    ) -> Result<Arc<crate::hdr::tiled::HdrTileBuffer>, String> {
        Err("decode failed".to_string())
    }
}

#[test]
fn request_tile_reports_ready_when_hdr_decode_fails() {
    let loader = ImageLoader::new();
    let source: Arc<dyn crate::hdr::tiled::HdrTiledSource> = Arc::new(FailingHdrTiledSource);

    loader.request_tile(
        5,
        crate::loader::decode_profile_stub(),
        1,
        TileDecodeSource::Hdr(source),
        0,
        0,
    );

    let output = loader
        .rx
        .recv_timeout(Duration::from_secs(2))
        .expect("HDR failed tile ready result");
    match output {
        LoaderOutput::Tile(tile) => {
            assert_eq!(tile.index, 5);
            assert_eq!(tile.col, 0);
            assert_eq!(tile.row, 0);
            assert_eq!(tile.pixel_kind, TilePixelKind::Hdr);
        }
        _ => panic!("expected HDR tile-ready output"),
    }
}
