//! `LoadResult` / preview bundles / `TileResult` smoke tests.

use std::sync::Arc;

use crate::hdr::types::{HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat};
use crate::loader::{
    DecodedImage, ImageData, LoadResult, PixelPlaneKind, PreviewBundle, PreviewResult, PreviewStage,
    RenderShape, TileDecodeSource, TilePixelKind, TileResult, TiledImageSource,
};
use crate::loader::tiled_sources::MemoryImageSource;
use crate::loader::orchestrator::TileInFlightKey;

#[test]
fn tile_inflight_keys_distinguish_sdr_and_hdr_outputs() {
    let sdr = TileInFlightKey::new(7, 11, 3, 4, TilePixelKind::Sdr);
    let hdr = TileInFlightKey::new(7, 11, 3, 4, TilePixelKind::Hdr);

    assert_ne!(sdr, hdr);
}

#[test]
fn tile_inflight_keys_distinguish_generations() {
    let older = TileInFlightKey::new(7, 11, 3, 4, TilePixelKind::Hdr);
    let newer = TileInFlightKey::new(7, 12, 3, 4, TilePixelKind::Hdr);

    assert_ne!(older, newer);
}

#[test]
fn tile_decode_source_reports_output_kind() {
    let sdr_source: Arc<dyn TiledImageSource> = Arc::new(
        MemoryImageSource::new(1, 1, Arc::new(vec![0, 0, 0, 255])),
    );
    let hdr_source: Arc<dyn crate::hdr::tiled::HdrTiledSource> = Arc::new(
        crate::hdr::tiled::HdrTiledImageSource::new(HdrImageBuffer {
            width: 1,
            height: 1,
            format: HdrPixelFormat::Rgba32Float,
            color_space: HdrColorSpace::LinearSrgb,
            metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
            rgba_f32: Arc::new(vec![0.0, 0.0, 0.0, 1.0]),
        })
        .expect("build HDR tiled source"),
    );

    assert_eq!(
        TileDecodeSource::Sdr(sdr_source).pixel_kind(),
        TilePixelKind::Sdr
    );
    assert_eq!(
        TileDecodeSource::Hdr(hdr_source).pixel_kind(),
        TilePixelKind::Hdr
    );
}

#[test]
fn load_result_exposes_unified_preview_bundle_without_compat_fields() {
    let sdr_preview = DecodedImage::new(2, 1, vec![0, 0, 0, 255, 255, 255, 255, 255]);
    let hdr_preview = Arc::new(HdrImageBuffer {
        width: 1,
        height: 1,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::LinearSrgb,
        metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
        rgba_f32: Arc::new(vec![0.0, 0.0, 0.0, 1.0]),
    });
    let bundle = PreviewBundle::initial()
        .with_sdr(sdr_preview.clone())
        .with_hdr(Arc::clone(&hdr_preview));

    let result = LoadResult {
        index: 1,
        generation: 2,
        result: Ok(ImageData::Static(sdr_preview.clone())),
        preview_bundle: bundle,
        ultra_hdr_capacity_sensitive: false,
        sdr_fallback_is_placeholder: false,
    };

    assert_eq!(result.preview_bundle.stage(), PreviewStage::Initial);
    assert_eq!(result.preview_bundle.sdr().expect("sdr preview").width, 2);
    assert_eq!(result.preview_bundle.hdr().expect("hdr preview").width, 1);
    let sdr_plane = result
        .preview_bundle
        .plane(PixelPlaneKind::Sdr)
        .expect("sdr plane");
    let hdr_plane = result
        .preview_bundle
        .plane(PixelPlaneKind::Hdr)
        .expect("hdr plane");
    assert_eq!(sdr_plane.kind(), PixelPlaneKind::Sdr);
    assert_eq!(sdr_plane.dimensions(), (2, 1));
    assert_eq!(hdr_plane.kind(), PixelPlaneKind::Hdr);
    assert_eq!(hdr_plane.dimensions(), (1, 1));
    assert_eq!(PreviewBundle::refined().stage(), PreviewStage::Refined);
}

#[test]
fn image_data_exposes_render_shape_and_available_planes() {
    let sdr = DecodedImage::new(1, 1, vec![0, 0, 0, 255]);
    let hdr = HdrImageBuffer {
        width: 1,
        height: 1,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::LinearSrgb,
        metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
        rgba_f32: Arc::new(vec![0.0, 0.0, 0.0, 1.0]),
    };
    let static_sdr = ImageData::Static(sdr.clone());
    let static_hdr = ImageData::Hdr {
        hdr: hdr.clone(),
        fallback: sdr.clone(),
    };
    let tiled_sdr_source: Arc<dyn TiledImageSource> = Arc::new(
        MemoryImageSource::new(1, 1, Arc::new(vec![0, 0, 0, 255])),
    );
    let tiled_hdr_source: Arc<dyn crate::hdr::tiled::HdrTiledSource> = Arc::new(
        crate::hdr::tiled::HdrTiledImageSource::new(hdr).expect("build HDR tiled source"),
    );
    let tiled_hdr = ImageData::HdrTiled {
        hdr: Arc::clone(&tiled_hdr_source),
        fallback: Arc::clone(&tiled_sdr_source),
    };

    assert_eq!(static_sdr.preferred_render_shape(), RenderShape::Static);
    assert!(static_sdr.has_plane(PixelPlaneKind::Sdr));
    assert!(!static_sdr.has_plane(PixelPlaneKind::Hdr));
    assert!(static_sdr.static_sdr().is_some());

    assert_eq!(static_hdr.preferred_render_shape(), RenderShape::Static);
    assert!(static_hdr.has_plane(PixelPlaneKind::Sdr));
    assert!(static_hdr.has_plane(PixelPlaneKind::Hdr));
    assert!(static_hdr.static_hdr().is_some());

    assert_eq!(tiled_hdr.preferred_render_shape(), RenderShape::Tiled);
    assert!(tiled_hdr.has_plane(PixelPlaneKind::Sdr));
    assert!(tiled_hdr.has_plane(PixelPlaneKind::Hdr));
    assert!(tiled_hdr.tiled_sdr_source().is_some());
    assert!(tiled_hdr.tiled_hdr_source().is_some());
}

#[test]
fn preview_result_exposes_refined_sdr_preview_bundle() {
    let preview = DecodedImage::new(2, 1, vec![0, 0, 0, 255, 255, 255, 255, 255]);
    let update = PreviewResult::from_sdr_preview(3, 5, Ok(preview.clone()));

    assert!(update.error.is_none());
    assert_eq!(update.preview_bundle.stage(), PreviewStage::Refined);
    assert_eq!(
        update
            .preview_bundle
            .plane(PixelPlaneKind::Sdr)
            .expect("sdr preview plane")
            .dimensions(),
        (2, 1)
    );
}

#[test]
fn preview_result_exposes_refined_hdr_preview_bundle() {
    let hdr_preview = Arc::new(HdrImageBuffer {
        width: 2,
        height: 1,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::LinearSrgb,
        metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
        rgba_f32: Arc::new(vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0, 1.0]),
    });
    let update = PreviewResult {
        index: 3,
        generation: 5,
        preview_bundle: PreviewBundle::refined().with_hdr(Arc::clone(&hdr_preview)),
        error: None,
    };

    assert!(update.error.is_none());
    assert_eq!(update.preview_bundle.stage(), PreviewStage::Refined);
    assert_eq!(
        update
            .preview_bundle
            .plane(PixelPlaneKind::Hdr)
            .expect("hdr preview plane")
            .dimensions(),
        (2, 1)
    );
    // HDR refinement results carry HDR pixels only — the SDR fallback plane is derived
    // lazily at render time by `select_render_backend`'s HDR-plane fallback (and the
    // HDR image plane shader's `SdrToneMapped` output mode). Keeping the loader side
    // HDR-only avoids tone-mapping a 4K HQ preview on systems that will only present
    // it through the native scRGB pipeline.
    assert!(update.preview_bundle.sdr().is_none());
}

#[test]
fn tile_result_exposes_shared_pending_key_and_repaint_policy() {
    let result = TileResult {
        index: 7,
        generation: 11,
        col: 3,
        row: 4,
        pixel_kind: TilePixelKind::Hdr,
    };

    assert_eq!(
        result.pending_key(),
        crate::tile_cache::PendingTileKey::new(
            crate::tile_cache::TileCoord { col: 3, row: 4 },
            TilePixelKind::Hdr,
        )
    );
    assert!(result.should_request_repaint());
}
