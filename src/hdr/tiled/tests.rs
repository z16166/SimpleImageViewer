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

use parking_lot::Mutex;
use std::sync::Arc;

use crate::hdr::tiled::{
    HdrTileBuffer, HdrTiledImageSource, HdrTiledSource, HdrTiledSourceKind,
    configured_hdr_tile_cache_max_bytes, set_global_hdr_tile_cache_max_bytes_for_tests,
};
use crate::hdr::types::{
    HdrColorProfile, HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat,
    HdrReference, HdrTransferFunction,
};

#[test]
fn extracts_rgba32f_tile_from_in_memory_hdr_buffer() {
    let image = HdrImageBuffer {
        width: 3,
        height: 2,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::LinearSrgb,
        metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
        rgba_f32: Arc::new(vec![
            0.0, 0.1, 0.2, 1.0, 1.0, 1.1, 1.2, 1.0, 2.0, 2.1, 2.2, 1.0, 3.0, 3.1, 3.2, 1.0,
            4.0, 4.1, 4.2, 1.0, 5.0, 5.1, 5.2, 1.0,
        ]),
    };

    let source = HdrTiledImageSource::new(image).expect("valid HDR tile source");
    let tile = source
        .extract_tile_rgba32f(1, 0, 2, 2)
        .expect("extract valid tile");

    assert_eq!(tile.width, 2);
    assert_eq!(tile.height, 2);
    assert_eq!(
        tile.rgba_f32.as_slice(),
        &[
            1.0, 1.1, 1.2, 1.0, 2.0, 2.1, 2.2, 1.0, 4.0, 4.1, 4.2, 1.0, 5.0, 5.1, 5.2, 1.0,
        ]
    );
}

#[test]
fn in_memory_hdr_tile_source_can_be_used_through_trait_object() {
    let source: Arc<dyn HdrTiledSource> =
        Arc::new(HdrTiledImageSource::new(test_image(2, 1)).expect("valid HDR tile source"));

    assert_eq!(source.source_kind(), HdrTiledSourceKind::InMemory);
    assert_eq!(source.source_kind().as_str(), "in-memory");
    assert_eq!(source.width(), 2);
    assert_eq!(source.height(), 1);
    let tile = source
        .extract_tile_rgba32f_arc(1, 0, 1, 1)
        .expect("extract through trait object");
    assert_eq!(tile.width, 1);
    assert_eq!(tile.height, 1);
    assert_eq!(tile.color_space, HdrColorSpace::LinearSrgb);
    assert_eq!(tile.rgba_f32.as_slice(), &[1.0, 1.0, 1.0, 1.0]);
}

#[test]
fn in_memory_hdr_tile_source_generates_hdr_preview() {
    let source = HdrTiledImageSource::new(test_image(4, 2)).expect("valid HDR tile source");

    let preview = source
        .generate_hdr_preview(2, 1)
        .expect("generate HDR preview");

    assert_eq!((preview.width, preview.height), (2, 1));
    assert_eq!(preview.format, HdrPixelFormat::Rgba32Float);
    assert_eq!(preview.color_space, HdrColorSpace::LinearSrgb);
    assert_eq!(preview.rgba_f32.len(), 2 * 4);
}

#[test]
fn in_memory_hdr_tile_source_preserves_hdr_metadata_on_preview_and_tiles() {
    let mut image = test_image(2, 1);
    image.metadata = HdrImageMetadata {
        transfer_function: HdrTransferFunction::Pq,
        reference: HdrReference::DisplayReferred,
        color_profile: HdrColorProfile::Cicp {
            color_primaries: 9,
            transfer_characteristics: 16,
            matrix_coefficients: 9,
            full_range: false,
        },
        ..HdrImageMetadata::default()
    };
    let source = HdrTiledImageSource::new(image).expect("valid HDR tile source");

    let preview = source
        .generate_hdr_preview(1, 1)
        .expect("generate HDR preview");
    let tile = source
        .extract_tile_rgba32f(0, 0, 1, 1)
        .expect("extract HDR tile");

    assert_eq!(preview.metadata.transfer_function, HdrTransferFunction::Pq);
    assert_eq!(preview.metadata.reference, HdrReference::DisplayReferred);
    assert_eq!(tile.metadata.transfer_function, HdrTransferFunction::Pq);
    assert_eq!(tile.metadata.reference, HdrReference::DisplayReferred);
}

#[test]
fn tile_backed_hdr_preview_samples_expected_source_pixels() {
    let pixels = (0..12)
        .flat_map(|value| {
            let value = value as f32;
            [value, value, value, 1.0]
        })
        .collect();
    let source = HdrTiledImageSource::new(HdrImageBuffer {
        width: 4,
        height: 3,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::LinearSrgb,
        metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
        rgba_f32: Arc::new(pixels),
    })
    .expect("valid HDR tile source");

    let preview = super::hdr_preview_from_tiled_source_nearest(&source, 2, 2)
        .expect("generate tiled HDR preview");

    assert_eq!((preview.width, preview.height), (2, 2));
    assert_eq!(
        preview.rgba_f32.as_slice(),
        &[
            0.0, 0.0, 0.0, 1.0, 3.0, 3.0, 3.0, 1.0, 8.0, 8.0, 8.0, 1.0, 11.0, 11.0, 11.0, 1.0,
        ]
    );
}

#[test]
fn disk_backed_hdr_preview_samples_each_output_row() {
    let source = RecordingDiskBackedSource::new(1, 128);

    let preview = super::hdr_preview_from_tiled_source_nearest(&source, 64, 64)
        .expect("generate disk-backed preview");

    let mut requested_rows = source.requested_rows.lock().clone();
    requested_rows.sort_unstable();
    requested_rows.dedup();
    assert_eq!((preview.width, preview.height), (1, 64));
    assert_eq!(requested_rows.len(), 64);
    assert_eq!(requested_rows[0], 0);
    assert_eq!(requested_rows[63], 127);
}

#[test]
fn rejects_malformed_hdr_tile_source_buffer() {
    let image = HdrImageBuffer {
        width: 2,
        height: 2,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::LinearSrgb,
        metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
        rgba_f32: Arc::new(vec![1.0; 4]),
    };

    let err = HdrTiledImageSource::new(image).expect_err("reject malformed source");

    assert!(err.contains("expected 16 floats"));
}

#[test]
fn repeated_tile_extraction_reuses_cached_tile_buffer() {
    let image = HdrImageBuffer {
        width: 2,
        height: 1,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::LinearSrgb,
        metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
        rgba_f32: Arc::new(vec![1.0; 2 * 4]),
    };
    let source = HdrTiledImageSource::new(image).expect("valid HDR tile source");

    let first = source
        .extract_tile_rgba32f_arc(0, 0, 1, 1)
        .expect("extract first tile");
    let second = source
        .extract_tile_rgba32f_arc(0, 0, 1, 1)
        .expect("extract cached tile");

    assert!(Arc::ptr_eq(&first, &second));
}

#[test]
fn hdr_tile_cache_evicts_least_recently_used_tile_when_over_budget() {
    let source =
        HdrTiledImageSource::new_with_cache_budget(test_image(4, 1), 2 * tile_bytes(1, 1))
            .expect("valid HDR tile source");

    let first = source
        .extract_tile_rgba32f_arc(0, 0, 1, 1)
        .expect("extract first tile");
    let _second = source
        .extract_tile_rgba32f_arc(1, 0, 1, 1)
        .expect("extract second tile");
    let _third = source
        .extract_tile_rgba32f_arc(2, 0, 1, 1)
        .expect("extract third tile");
    let first_after_eviction = source
        .extract_tile_rgba32f_arc(0, 0, 1, 1)
        .expect("re-extract first tile");

    assert!(!Arc::ptr_eq(&first, &first_after_eviction));
    assert_eq!(source.cached_tile_count(), 2);
    assert!(source.cached_tile_bytes() <= 2 * tile_bytes(1, 1));
}

#[test]
fn hdr_tile_cache_keeps_protected_visible_tiles_when_over_budget() {
    let mut cache = super::HdrTileCache::new(2 * tile_bytes(1, 1));
    let first_key = (0, 0, 1, 1);
    let second_key = (1, 0, 1, 1);
    let third_key = (2, 0, 1, 1);

    cache.insert(first_key, Arc::new(hdr_tile(1, 1, 1.0)));
    cache.insert(second_key, Arc::new(hdr_tile(1, 1, 2.0)));
    cache.set_protected_keys([first_key]);
    cache.insert(third_key, Arc::new(hdr_tile(1, 1, 3.0)));

    assert!(cache.get(first_key).is_some());
    assert!(cache.get(third_key).is_some());
    assert!(cache.get(second_key).is_none());
    assert!(cache.current_bytes() <= 2 * tile_bytes(1, 1));
}

#[test]
fn hdr_tile_cache_budget_scales_with_physical_memory() {
    let gib = 1024 * 1024 * 1024;

    assert_eq!(
        super::hdr_tile_cache_budget_for_memory(4 * gib),
        256 * 1024 * 1024
    );
    assert_eq!(super::hdr_tile_cache_budget_for_memory(32 * gib), 2 * gib);
    assert_eq!(super::hdr_tile_cache_budget_for_memory(128 * gib), 4 * gib);
}

#[test]
fn hdr_tile_cache_refreshes_lru_on_repeated_access() {
    let source =
        HdrTiledImageSource::new_with_cache_budget(test_image(4, 1), 2 * tile_bytes(1, 1))
            .expect("valid HDR tile source");

    let first = source
        .extract_tile_rgba32f_arc(0, 0, 1, 1)
        .expect("extract first tile");
    let second = source
        .extract_tile_rgba32f_arc(1, 0, 1, 1)
        .expect("extract second tile");
    let first_refreshed = source
        .extract_tile_rgba32f_arc(0, 0, 1, 1)
        .expect("refresh first tile");
    let _third = source
        .extract_tile_rgba32f_arc(2, 0, 1, 1)
        .expect("extract third tile");
    let second_after_eviction = source
        .extract_tile_rgba32f_arc(1, 0, 1, 1)
        .expect("re-extract second tile");

    assert!(Arc::ptr_eq(&first, &first_refreshed));
    assert!(!Arc::ptr_eq(&second, &second_after_eviction));
}

#[test]
fn default_hdr_tile_source_uses_global_cache_budget() {
    let old_budget = configured_hdr_tile_cache_max_bytes();
    set_global_hdr_tile_cache_max_bytes_for_tests(tile_bytes(1, 1));

    let source = HdrTiledImageSource::new(test_image(2, 1)).expect("valid HDR tile source");

    set_global_hdr_tile_cache_max_bytes_for_tests(old_budget);
    assert_eq!(source.cache_budget_bytes(), tile_bytes(1, 1));
}

#[test]
fn sdr_preview_is_exposure_neutral_for_hdr_source_tiles() {
    let source = HdrTiledImageSource::new(HdrImageBuffer {
        width: 1,
        height: 1,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::LinearSrgb,
        metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
        rgba_f32: Arc::new(vec![4.0, 4.0, 4.0, 1.0]),
    })
    .expect("valid HDR tile source");

    let (_width, _height, pixels) = source
        .generate_sdr_preview(1, 1)
        .expect("generate SDR preview");

    assert_eq!(
        pixels[0], 230,
        "fallback previews intentionally use neutral exposure; user exposure is applied by HDR rendering uniforms"
    );
}

#[test]
fn sdr_preview_keeps_visible_rgb_opaque_when_alpha_is_zero_everywhere() {
    let preview = HdrImageBuffer {
        width: 1,
        height: 1,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::LinearSrgb,
        metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
        rgba_f32: Arc::new(vec![0.25, 0.5, 1.0, 0.0]),
    };

    let (_width, _height, pixels) =
        super::sdr_preview_from_hdr_preview(&preview).expect("generate SDR preview");

    assert_ne!(
        pixels[3], 0,
        "visible RGB previews should not become fully transparent"
    );
}

fn test_image(width: u32, height: u32) -> HdrImageBuffer {
    HdrImageBuffer {
        width,
        height,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::LinearSrgb,
        metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
        rgba_f32: Arc::new(vec![1.0; width as usize * height as usize * 4]),
    }
}

fn tile_bytes(width: u32, height: u32) -> usize {
    width as usize * height as usize * 4 * std::mem::size_of::<f32>()
}

fn hdr_tile(width: u32, height: u32, value: f32) -> super::HdrTileBuffer {
    super::HdrTileBuffer::new(
        width,
        height,
        HdrColorSpace::LinearSrgb,
        Arc::new(vec![value; width as usize * height as usize * 4]),
    )
}

struct RecordingDiskBackedSource {
    width: u32,
    height: u32,
    requested_rows: Mutex<Vec<u32>>,
}

impl RecordingDiskBackedSource {
    fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            requested_rows: Mutex::new(Vec::new()),
        }
    }
}

impl HdrTiledSource for RecordingDiskBackedSource {
    fn source_kind(&self) -> HdrTiledSourceKind {
        HdrTiledSourceKind::DiskBacked
    }

    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn color_space(&self) -> HdrColorSpace {
        HdrColorSpace::LinearSrgb
    }

    fn generate_hdr_preview(&self, max_w: u32, max_h: u32) -> Result<HdrImageBuffer, String> {
        super::hdr_preview_from_tiled_source_nearest(self, max_w, max_h)
    }

    fn generate_sdr_preview(
        &self,
        max_w: u32,
        max_h: u32,
    ) -> Result<(u32, u32, Vec<u8>), String> {
        let preview = self.generate_hdr_preview(max_w, max_h)?;
        super::sdr_preview_from_hdr_preview(&preview)
    }

    fn extract_tile_rgba32f_arc(
        &self,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    ) -> Result<Arc<HdrTileBuffer>, String> {
        assert_eq!(x, 0);
        assert_eq!(width, self.width);
        assert_eq!(height, 1);
        self.requested_rows.lock().push(y);
        Ok(Arc::new(HdrTileBuffer::new(
            width,
            height,
            HdrColorSpace::LinearSrgb,
            Arc::new(vec![y as f32, y as f32, y as f32, 1.0]),
        )))
    }
}
