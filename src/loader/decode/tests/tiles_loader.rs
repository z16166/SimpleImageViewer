//! `ImageLoader` interaction and HDR tile worker behavior.

use std::sync::Arc;
use std::time::Duration;

use crate::hdr::types::{HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat};
use crate::loader::{ImageLoader, LoadResult, LoaderOutput, PreviewBundle, TileDecodeSource, TilePixelKind};

#[test]
fn image_request_stays_inflight_until_ui_finishes_installing_result() {
    let mut loader = ImageLoader::new();
    let index = 7;
    let generation = 11;
    loader.test_register_inflight(index, generation);

    let load_result = LoadResult {
        index,
        generation,
        result: Err("synthetic".to_string()),
        preview_bundle: PreviewBundle::initial(),
        ultra_hdr_capacity_sensitive: false,
        sdr_fallback_is_placeholder: false,
    };
    loader.test_send_loader_output(LoaderOutput::Image(load_result));

    let output = loader.poll().expect("polled image result");
    assert!(matches!(output, LoaderOutput::Image(_)));
    assert!(loader.is_loading(index, generation));

    loader.finish_image_request(index, generation);
    assert!(!loader.is_loading(index, generation));
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

    loader.request_tile(3, 0, 1.0, TileDecodeSource::Hdr(Arc::clone(&source)), 0, 0);

    let output = loader
        .rx
        .recv_timeout(Duration::from_secs(2))
        .expect("HDR tile ready result");
    match output {
        LoaderOutput::Tile(tile) => {
            assert_eq!(tile.index, 3);
            assert_eq!(tile.generation, 0);
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

    loader.request_tile(3, 9, 1.0, TileDecodeSource::Hdr(source), 0, 0);

    let output = loader
        .rx
        .recv_timeout(Duration::from_secs(2))
        .expect("HDR cached tile ready result");
    match output {
        LoaderOutput::Tile(tile) => {
            assert_eq!(tile.index, 3);
            assert_eq!(tile.generation, 9);
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

    loader.request_tile(5, 13, 1.0, TileDecodeSource::Hdr(source), 0, 0);

    let output = loader
        .rx
        .recv_timeout(Duration::from_secs(2))
        .expect("HDR failed tile ready result");
    match output {
        LoaderOutput::Tile(tile) => {
            assert_eq!(tile.index, 5);
            assert_eq!(tile.generation, 13);
            assert_eq!(tile.col, 0);
            assert_eq!(tile.row, 0);
            assert_eq!(tile.pixel_kind, TilePixelKind::Hdr);
        }
        _ => panic!("expected HDR tile-ready output"),
    }
}
