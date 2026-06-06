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

use super::*;
use crate::app::{HardwareTier, SettingsTab};
use crate::audio::AudioPlayer;
use crate::loader::PreviewBundle;
use crate::loader::{ImageLoader, TextureCache};
use crate::settings::Settings;
use crate::theme::{SystemThemeCache, ThemePalette};
use std::collections::{HashMap, HashSet};

#[test]
fn prefetch_window_distance_matches_circular_neighbors() {
    assert!(prefetch_window_contains(0, 100, 0, 2));
    assert!(prefetch_window_contains(0, 100, 2, 2));
    assert!(!prefetch_window_contains(0, 100, 3, 2));
    assert!(prefetch_window_contains(50, 100, 48, 2));
    assert!(!prefetch_window_contains(50, 100, 47, 2));
}

#[test]
fn output_mode_boundary_changes_only_when_crossing_hdr_and_sdr() {
    use crate::hdr::types::HdrOutputMode;

    assert!(output_mode_crosses_hdr_sdr_boundary(
        HdrOutputMode::SdrToneMapped,
        HdrOutputMode::WindowsScRgb
    ));
    assert!(output_mode_crosses_hdr_sdr_boundary(
        HdrOutputMode::MacOsEdr,
        HdrOutputMode::SdrToneMapped
    ));
    assert!(!output_mode_crosses_hdr_sdr_boundary(
        HdrOutputMode::WindowsScRgb,
        HdrOutputMode::MacOsEdr
    ));
    assert!(!output_mode_crosses_hdr_sdr_boundary(
        HdrOutputMode::SdrToneMapped,
        HdrOutputMode::SdrToneMapped
    ));
}

struct DummyTiledSource {
    width: u32,
    height: u32,
}

impl crate::loader::TiledImageSource for DummyTiledSource {
    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn extract_tile(&self, _x: u32, _y: u32, w: u32, h: u32) -> Arc<Vec<u8>> {
        Arc::new(vec![0; w as usize * h as usize * 4])
    }

    fn generate_preview(&self, _max_w: u32, _max_h: u32) -> (u32, u32, Vec<u8>) {
        (1, 1, vec![0, 0, 0, 255])
    }

    fn full_pixels(&self) -> Option<Arc<Vec<u8>>> {
        None
    }
}

#[test]
fn first_cached_hdr_still_prefers_static_cache_then_animation_then_pending() {
    use crate::hdr::types::{HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat};
    use std::sync::Arc;
    use std::time::Instant;

    let mk = |tag: u8| {
        Arc::new(HdrImageBuffer {
            width: 1,
            height: 1,
            format: HdrPixelFormat::Rgba32Float,
            color_space: HdrColorSpace::LinearSrgb,
            metadata: HdrImageMetadata::default(),
            rgba_f32: Arc::new(vec![tag as f32, 0.0, 0.0, 1.0]),
        })
    };

    let static_hdr = mk(1);
    let anim_hdr = mk(2);
    let pending_hdr = mk(3);

    let mut hdr_image_cache = HashMap::new();
    hdr_image_cache.insert(0, Arc::clone(&static_hdr));
    assert_eq!(
        first_cached_hdr_still_for_index(&hdr_image_cache, &HashMap::new(), None, 0)
            .map(|b| b.rgba_f32[0]),
        Some(1.0)
    );

    hdr_image_cache.clear();
    let mut animation_cache = HashMap::new();
    animation_cache.insert(
        1,
        AnimationPlayback {
            image_index: 1,
            textures: Vec::new(),
            hdr_frames: Some(vec![Arc::clone(&anim_hdr)]),
            delays: Vec::new(),
            current_frame: 0,
            frame_start: Instant::now(),
        },
    );
    assert_eq!(
        first_cached_hdr_still_for_index(&hdr_image_cache, &animation_cache, None, 1)
            .map(|b| b.rgba_f32[0]),
        Some(2.0)
    );

    let pending = PendingAnimUpload {
        image_index: 2,
        hdr_frames: Some(vec![Arc::clone(&pending_hdr)]),
        frames: Vec::new(),
        textures: Vec::new(),
        delays: Vec::new(),
        next_frame: 0,
    };
    assert_eq!(
        first_cached_hdr_still_for_index(&hdr_image_cache, &HashMap::new(), Some(&pending), 2)
            .map(|b| b.rgba_f32[0]),
        Some(3.0)
    );
    assert!(
        first_cached_hdr_still_for_index(&hdr_image_cache, &HashMap::new(), Some(&pending), 9)
            .is_none()
    );
}

#[test]
fn navigation_preserves_current_tile_manager_for_restore() {
    let source = Arc::new(DummyTiledSource {
        width: 4096,
        height: 4096,
    });
    let mut tile_manager = Some(TileManager::with_source(7, 42, source));
    let mut prefetched_tiles = HashMap::new();

    preserve_current_tile_manager_for_navigation(7, 8, &mut tile_manager, &mut prefetched_tiles);

    assert!(tile_manager.is_none());
    assert!(prefetched_tiles.contains_key(&7));
    assert_eq!(prefetched_tiles.get(&7).unwrap().generation, 42);
}

#[test]
fn tiled_bootstrap_preview_replaces_only_missing_or_smaller_cached_preview() {
    assert!(should_upload_tiled_bootstrap_preview(false, None, 512));
    assert!(should_upload_tiled_bootstrap_preview(true, None, 512));
    assert!(should_upload_tiled_bootstrap_preview(true, Some(128), 512));
    assert!(!should_upload_tiled_bootstrap_preview(
        true,
        Some(1024),
        512
    ));
    assert!(!should_upload_tiled_bootstrap_preview(true, Some(512), 512));
}

#[test]
fn tiled_hdr_preview_replaces_only_missing_or_smaller_cached_preview() {
    assert!(should_cache_tiled_hdr_preview(None, 1024));
    assert!(should_cache_tiled_hdr_preview(Some(1024), 4096));
    assert!(!should_cache_tiled_hdr_preview(Some(4096), 1024));
    assert!(!should_cache_tiled_hdr_preview(Some(4096), 4096));
}

#[test]
fn current_hdr_tiled_preview_updates_only_when_larger_preview_is_cached() {
    let initial = Arc::new(crate::hdr::types::HdrImageBuffer {
        width: 512,
        height: 256,
        format: crate::hdr::types::HdrPixelFormat::Rgba32Float,
        color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
        metadata: crate::hdr::types::HdrImageMetadata::from_color_space(
            crate::hdr::types::HdrColorSpace::LinearSrgb,
        ),
        rgba_f32: Arc::new(vec![0.0; 512 * 256 * 4]),
    });
    let refined = Arc::new(crate::hdr::types::HdrImageBuffer {
        width: 4096,
        height: 2048,
        format: crate::hdr::types::HdrPixelFormat::Rgba32Float,
        color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
        metadata: crate::hdr::types::HdrImageMetadata::from_color_space(
            crate::hdr::types::HdrColorSpace::LinearSrgb,
        ),
        rgba_f32: Arc::new(vec![0.0; 4]),
    });
    let smaller = Arc::new(crate::hdr::types::HdrImageBuffer {
        width: 1024,
        height: 512,
        format: crate::hdr::types::HdrPixelFormat::Rgba32Float,
        color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
        metadata: crate::hdr::types::HdrImageMetadata::from_color_space(
            crate::hdr::types::HdrColorSpace::LinearSrgb,
        ),
        rgba_f32: Arc::new(vec![0.0; 4]),
    });
    let mut cache = HashMap::new();
    let mut current = None;

    cache_hdr_tiled_preview_state(
        7,
        7,
        &mut cache,
        &mut current,
        Some(Arc::clone(&initial)),
        "test.exr",
    );
    cache_hdr_tiled_preview_state(
        7,
        7,
        &mut cache,
        &mut current,
        Some(Arc::clone(&refined)),
        "test.exr",
    );
    cache_hdr_tiled_preview_state(7, 7, &mut cache, &mut current, Some(smaller), "test.exr");

    let cached = cache.get(&7).expect("preview should be cached");
    assert_eq!(cached.width, 4096);
    let current = current
        .as_ref()
        .and_then(|preview| preview.image_for_index(7))
        .expect("current preview should match image index");
    assert_eq!(current.width, 4096);
}

#[test]
fn tiled_sdr_preview_cache_policy_preserves_full_images_and_larger_previews() {
    assert!(should_cache_tiled_sdr_preview(false, false, None, 512));
    assert!(!should_cache_tiled_sdr_preview(true, false, Some(128), 512));
    assert!(should_cache_tiled_sdr_preview(true, true, None, 512));
    assert!(should_cache_tiled_sdr_preview(true, true, Some(128), 512));
    assert!(!should_cache_tiled_sdr_preview(true, true, Some(512), 512));
    assert!(!should_cache_tiled_sdr_preview(true, true, Some(1024), 512));
}

#[test]
fn current_image_load_guard_treats_hdr_tiled_source_as_loaded() {
    assert!(current_image_has_loaded_asset(false, true, false, false));
    assert!(current_image_has_loaded_asset(false, false, true, false));
    assert!(current_image_has_loaded_asset(false, false, false, true));
    assert!(!current_image_has_loaded_asset(false, false, false, false));
}

#[test]
fn hdr_fallback_texture_without_hdr_plane_is_not_loaded_asset() {
    assert!(hdr_fallback_asset_is_loaded(false, false));
    assert!(hdr_fallback_asset_is_loaded(true, true));
    assert!(!hdr_fallback_asset_is_loaded(true, false));
}

#[test]
fn first_batch_preload_waits_when_scan_done_is_already_available() {
    assert!(!should_schedule_first_batch_preload(true, 3, true, false));
    assert!(should_schedule_first_batch_preload(true, 3, false, false));
    assert!(!should_schedule_first_batch_preload(false, 3, false, false));
    assert!(!should_schedule_first_batch_preload(true, 0, false, false));
}

#[test]
fn first_batch_preload_waits_for_startup_target() {
    assert!(!should_schedule_first_batch_preload(true, 3, false, true));
}

#[test]
fn startup_target_detects_explicit_image_or_resume_image() {
    let explicit = PathBuf::from("explicit.jpg");
    let resumed = PathBuf::from("resumed.jpg");

    assert!(has_startup_target(Some(&explicit), false, None));
    assert!(has_startup_target(None, true, Some(&resumed)));
    assert!(!has_startup_target(None, false, Some(&resumed)));
    assert!(!has_startup_target(None, true, None));
}

#[test]
fn transition_preroll_starts_one_frame_in_progress() {
    assert_eq!(transition_preroll_duration(0), Duration::ZERO);
    assert_eq!(transition_preroll_duration(5), Duration::from_millis(4));
    assert_eq!(transition_preroll_duration(16), Duration::from_millis(15));
    assert_eq!(transition_preroll_duration(800), Duration::from_millis(16));
}

#[test]
fn pending_transition_starts_only_when_target_texture_is_ready() {
    assert!(!can_start_pending_transition(Some(7), 6, true));
    assert!(!can_start_pending_transition(Some(7), 7, false));
    assert!(can_start_pending_transition(Some(7), 7, true));
}

#[test]
fn target_render_ready_requires_hdr_plane_or_non_placeholder_sdr() {
    assert!(target_is_render_ready(true, false, false));
    assert!(!target_is_render_ready(true, false, true));
    assert!(target_is_render_ready(false, true, true));
    assert!(!target_is_render_ready(false, false, false));
}

#[test]
fn transition_can_start_immediately_when_target_is_already_cached() {
    assert!(should_start_transition_immediately(true, true));
    assert!(!should_start_transition_immediately(false, true));
    assert!(!should_start_transition_immediately(true, false));
}

#[test]
fn transition_source_texture_selection_clears_when_current_missing() {
    assert!(select_transition_source_texture(None, false, None).is_none());
}

#[test]
fn transition_source_texture_skips_placeholder_fallback_frames() {
    assert!(select_transition_source_texture(None, true, None).is_none());
}

#[test]
fn transition_source_selection_reuses_previous_when_current_unusable() {
    assert!(select_transition_source_texture(None, false, None).is_none());
    assert!(select_transition_source_texture(None, true, None).is_none());
}

#[test]
fn transition_source_hdr_selection_clears_when_current_missing() {
    assert!(select_transition_source_hdr(None, false, None).is_none());
}

#[test]
fn transition_source_hdr_skips_placeholder_fallback_frames() {
    assert!(select_transition_source_hdr(None, true, None).is_none());
}

#[test]
fn transition_source_hdr_selection_reuses_previous_when_current_unusable() {
    let dummy_hdr = Arc::new(crate::hdr::types::HdrImageBuffer {
        width: 1,
        height: 1,
        format: crate::hdr::types::HdrPixelFormat::Rgba32Float,
        color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
        metadata: crate::hdr::types::HdrImageMetadata::from_color_space(
            crate::hdr::types::HdrColorSpace::LinearSrgb,
        ),
        rgba_f32: Arc::new(vec![0.0; 4]),
    });

    let res = select_transition_source_hdr(None, true, Some(Arc::clone(&dummy_hdr)));
    assert!(res.is_some());
    assert_eq!(res.unwrap().width, 1);
}

#[test]
fn transition_source_hdr_happy_path_returns_current() {
    let dummy_hdr = Arc::new(crate::hdr::types::HdrImageBuffer {
        width: 1,
        height: 1,
        format: crate::hdr::types::HdrPixelFormat::Rgba32Float,
        color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
        metadata: crate::hdr::types::HdrImageMetadata::from_color_space(
            crate::hdr::types::HdrColorSpace::LinearSrgb,
        ),
        rgba_f32: Arc::new(vec![0.0; 4]),
    });

    let res = select_transition_source_hdr(Some(Arc::clone(&dummy_hdr)), false, None);
    assert!(res.is_some());
    assert_eq!(res.unwrap().width, 1);
}

#[test]
fn transition_none_keeps_source_frame_until_target_is_ready() {
    assert!(!should_reset_transition_when_source_texture_missing(true));
    assert!(should_reset_transition_when_source_texture_missing(false));
}

#[test]
fn navigation_direction_matches_wrap_aware_next_prev_behavior() {
    assert!(navigation_is_forward(1, 2, 10));
    assert!(!navigation_is_forward(2, 1, 10));
    assert!(navigation_is_forward(9, 0, 10));
    assert!(!navigation_is_forward(0, 9, 10));
}

#[test]
fn image_file_size_pairs_keep_known_sizes_and_fill_missing_sizes() {
    let paths = vec![
        PathBuf::from("a.jpg"),
        PathBuf::from("b.jpg"),
        PathBuf::from("c.jpg"),
    ];

    let pairs = image_file_size_pairs_with_missing_sizes_as_zero(paths, vec![10, 20]);

    assert_eq!(
        pairs,
        vec![
            (PathBuf::from("a.jpg"), 10),
            (PathBuf::from("b.jpg"), 20),
            (PathBuf::from("c.jpg"), 0),
        ]
    );
}

#[test]
fn asset_update_repaint_policy_centralizes_current_and_tile_rules() {
    assert!(should_request_repaint_for_asset_update(
        AssetUpdateKind::ImageLoaded,
        true,
        false
    ));
    assert!(!should_request_repaint_for_asset_update(
        AssetUpdateKind::ImageLoaded,
        false,
        false
    ));
    assert!(should_request_repaint_for_asset_update(
        AssetUpdateKind::PreviewUpgraded,
        true,
        false
    ));
    assert!(!should_request_repaint_for_asset_update(
        AssetUpdateKind::PreviewUpgraded,
        false,
        false
    ));
    assert!(should_request_repaint_for_asset_update(
        AssetUpdateKind::TileReady,
        true,
        true
    ));
    assert!(!should_request_repaint_for_asset_update(
        AssetUpdateKind::TileReady,
        true,
        false
    ));
    assert!(should_request_repaint_for_asset_update(
        AssetUpdateKind::RefinedFullPlane,
        true,
        false
    ));
}

#[test]
fn tiled_manager_install_helper_preserves_source_and_generation() {
    let source: Arc<dyn crate::loader::TiledImageSource> = Arc::new(DummyTiledSource {
        width: 1024,
        height: 768,
    });

    let tm = build_tiled_manager_with_best_preview(9, 17, source, None);

    assert_eq!(tm.image_index, 9);
    assert_eq!(tm.generation, 17);
    assert_eq!(tm.full_width, 1024);
    assert_eq!(tm.full_height, 768);
}

#[test]
fn current_hdr_tiled_preview_match_is_index_scoped() {
    let image = Arc::new(crate::hdr::types::HdrImageBuffer {
        width: 1,
        height: 1,
        format: crate::hdr::types::HdrPixelFormat::Rgba32Float,
        color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
        metadata: crate::hdr::types::HdrImageMetadata::from_color_space(
            crate::hdr::types::HdrColorSpace::LinearSrgb,
        ),
        rgba_f32: Arc::new(vec![0.0, 0.0, 0.0, 1.0]),
    });
    let current = crate::app::CurrentHdrImage::new(4, image);

    assert!(current_hdr_tiled_preview_matches_index(Some(&current), 4));
    assert!(!current_hdr_tiled_preview_matches_index(Some(&current), 5));
    assert!(!current_hdr_tiled_preview_matches_index(None, 4));
}

#[test]
fn view_change_invalidates_only_tile_manager_generation() {
    let source: Arc<dyn crate::loader::TiledImageSource> = Arc::new(DummyTiledSource {
        width: 1024,
        height: 768,
    });
    let mut tile_manager = Some(TileManager::with_source(4, 9, source));
    let loader_generation = 3;

    assert!(invalidate_tile_manager_requests_for_view_change(
        &mut tile_manager
    ));

    let tile_manager = tile_manager.expect("tile manager should remain installed");
    assert_eq!(tile_manager.generation, 10);
    assert_eq!(loader_generation, 3);
}

#[test]
fn hdr_load_result_capacity_is_stale_when_sensitive_hdr_mismatch() {
    let load = LoadResult {
        index: 0,
        generation: 1,
        source_key: 0,
        result: Ok(crate::loader::ImageData::Hdr {
            hdr: crate::hdr::types::HdrImageBuffer {
                width: 1,
                height: 1,
                format: crate::hdr::types::HdrPixelFormat::Rgba32Float,
                color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
                metadata: crate::hdr::types::HdrImageMetadata::default(),
                rgba_f32: Arc::new(vec![0.0; 4]),
            },
            fallback: crate::loader::DecodedImage::new(1, 1, vec![0, 0, 0, 255]),
        }),
        preview_bundle: PreviewBundle::initial(),
        ultra_hdr_capacity_sensitive: true,
        sdr_fallback_is_placeholder: false,
        target_hdr_capacity: 1.0,
    };
    assert!(hdr_load_result_capacity_is_stale(&load, 2.0));
    assert!(!hdr_load_result_capacity_is_stale(&load, 1.0));
}

#[test]
fn hdr_load_result_capacity_is_stale_ignores_non_sensitive_loads() {
    let load = LoadResult {
        index: 0,
        generation: 1,
        source_key: 0,
        result: Ok(crate::loader::ImageData::Hdr {
            hdr: crate::hdr::types::HdrImageBuffer {
                width: 1,
                height: 1,
                format: crate::hdr::types::HdrPixelFormat::Rgba32Float,
                color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
                metadata: crate::hdr::types::HdrImageMetadata::default(),
                rgba_f32: Arc::new(vec![0.0; 4]),
            },
            fallback: crate::loader::DecodedImage::new(1, 1, vec![0, 0, 0, 255]),
        }),
        preview_bundle: PreviewBundle::initial(),
        ultra_hdr_capacity_sensitive: false,
        sdr_fallback_is_placeholder: false,
        target_hdr_capacity: 1.0,
    };
    assert!(!hdr_load_result_capacity_is_stale(&load, 4.0));
}

#[test]
fn test_find_index_for_path_impl_matching() {
    let files = vec![
        PathBuf::from("H:/images/photo1.jpg"),
        PathBuf::from("H:/images/photo2.PNG"),
        PathBuf::from("H:/images/SUBDIR/photo3.jpg"),
    ];

    // 1. Exact match
    assert_eq!(
        find_index_for_path_impl(&files, &PathBuf::from("H:/images/photo1.jpg")),
        Some(0)
    );

    // 2. Case variation on extension
    assert_eq!(
        find_index_for_path_impl(&files, &PathBuf::from("H:/images/photo2.png")),
        Some(1)
    );

    // 3. Slash variations (Windows vs Unix style)
    assert_eq!(
        find_index_for_path_impl(&files, &PathBuf::from("H:\\images\\photo1.jpg")),
        Some(0)
    );

    // 4. Case variation in filename/directory
    assert_eq!(
        find_index_for_path_impl(&files, &PathBuf::from("h:/images/PHOTO2.png")),
        Some(1)
    );

    // 5. Subdirectory matching
    assert_eq!(
        find_index_for_path_impl(&files, &PathBuf::from("H:\\images\\SUBDIR\\photo3.jpg")),
        Some(2)
    );

    // 6. Not found
    assert_eq!(
        find_index_for_path_impl(&files, &PathBuf::from("H:/images/nonexistent.jpg")),
        None
    );
}

#[test]
fn first_cached_hdr_or_tiled_preview_selection_rules() {
    use crate::hdr::types::{HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat};
    use std::sync::Arc;

    let mk = |tag: u8| {
        Arc::new(HdrImageBuffer {
            width: 1,
            height: 1,
            format: HdrPixelFormat::Rgba32Float,
            color_space: HdrColorSpace::LinearSrgb,
            metadata: HdrImageMetadata::default(),
            rgba_f32: Arc::new(vec![tag as f32, 0.0, 0.0, 1.0]),
        })
    };

    let static_hdr = mk(1);
    let preview_hdr = mk(4);

    let mut hdr_image_cache = HashMap::new();
    let mut hdr_tiled_preview_cache = HashMap::new();

    // 1. None cached -> returns None
    assert!(
        first_cached_hdr_or_tiled_preview_for_index(
            &hdr_image_cache,
            &HashMap::new(),
            None,
            &hdr_tiled_preview_cache,
            None,
            0
        )
        .is_none()
    );

    // 2. Only preview cached -> returns preview
    hdr_tiled_preview_cache.insert(0, Arc::clone(&preview_hdr));
    assert_eq!(
        first_cached_hdr_or_tiled_preview_for_index(
            &hdr_image_cache,
            &HashMap::new(),
            None,
            &hdr_tiled_preview_cache,
            None,
            0
        )
        .map(|b| b.rgba_f32[0]),
        Some(4.0)
    );

    // 3. Both cached -> prefers full static image (from first_cached_hdr_still_for_index)
    hdr_image_cache.insert(0, Arc::clone(&static_hdr));
    assert_eq!(
        first_cached_hdr_or_tiled_preview_for_index(
            &hdr_image_cache,
            &HashMap::new(),
            None,
            &hdr_tiled_preview_cache,
            None,
            0
        )
        .map(|b| b.rgba_f32[0]),
        Some(1.0)
    );

    // 4. current_hdr_tiled_preview fallback when caches hit miss
    hdr_tiled_preview_cache.clear();
    hdr_image_cache.clear();
    let fallback_hdr = mk(5);
    let current_image = crate::app::CurrentHdrImage::new(0, Arc::clone(&fallback_hdr));
    assert_eq!(
        first_cached_hdr_or_tiled_preview_for_index(
            &hdr_image_cache,
            &HashMap::new(),
            None,
            &hdr_tiled_preview_cache,
            Some(&current_image),
            0
        )
        .map(|b| b.rgba_f32[0]),
        Some(5.0)
    );
}

fn make_test_app() -> ImageViewerApp {
    let (file_op_tx, file_op_rx) = crossbeam_channel::unbounded();
    let (lightweight_file_op_tx, _lightweight_file_op_rx) = crossbeam_channel::unbounded();
    let (save_tx, _save_rx) = crossbeam_channel::unbounded();
    let (_save_error_tx, save_error_rx) = crossbeam_channel::unbounded();
    let (_ipc_tx, ipc_rx) = crossbeam_channel::unbounded();

    let hotkeys_draft_config = crate::hotkeys::model::default_hotkey_config_file();
    let hotkeys_runtime = crate::hotkeys::rebuild_runtime_state(&hotkeys_draft_config);
    let (hotkeys_save_tx, _hotkeys_save_rx) = crossbeam_channel::unbounded();
    let (_hotkeys_save_error_tx, hotkeys_save_error_rx) = crossbeam_channel::unbounded();
    let context_menu_draft_config = crate::context_menu::model::default_context_menu_config_file();
    let context_menu_runtime =
        crate::context_menu::rebuild_runtime_state(&context_menu_draft_config);
    let (context_menu_save_tx, _context_menu_save_rx) = crossbeam_channel::unbounded();
    let (_context_menu_save_error_tx, context_menu_save_error_rx) = crossbeam_channel::unbounded();

    #[cfg(target_os = "linux")]
    let requested_vulkan_hdr_metadata = eframe::egui_wgpu::RequestedVulkanHdrMetadata::new();
    #[cfg(target_os = "linux")]
    let last_vulkan_hdr_metadata = None;

    ImageViewerApp {
        settings: Settings::default(),
        image_files: Vec::new(),
        file_byte_len_by_index: Vec::new(),
        current_index: 0,
        initial_image: None,
        scanning: false,
        hardware_tier: HardwareTier::Medium,
        loader: ImageLoader::new(),
        texture_cache: TextureCache::new(10),
        hdr_capabilities: crate::hdr::capabilities::HdrCapabilities::sdr("test"),
        hdr_renderer: crate::hdr::renderer::HdrImageRenderer::new(),
        hdr_target_format: None,
        hdr_monitor_state: crate::hdr::monitor::HdrMonitorState::default(),
        cached_window_placement: None,
        cached_restore_placement: None,
        requested_target_format: eframe::egui_wgpu::RequestedSurfaceFormat::new(),
        active_target_format: eframe::egui_wgpu::ActiveSurfaceFormat::new(),
        requested_rgb10a2_pq_encode: eframe::egui_wgpu::RequestedRgb10a2PqEncode::new(),
        gamma22_display_scale: eframe::egui_wgpu::Gamma22DisplayScale::new(),
        vulkan_wsi_hdr_gates: eframe::egui_wgpu::VulkanWsiHdrGatesMailbox::new(),
        #[cfg(target_os = "linux")]
        requested_vulkan_hdr_metadata,
        #[cfg(target_os = "linux")]
        last_vulkan_hdr_metadata,
        last_logged_swap_chain_format_request: None,
        rgb10a2_pq_encode_requested: false,
        ultra_hdr_decode_capacity: 1.0,
        ultra_hdr_decode_output_mode: crate::hdr::types::HdrOutputMode::SdrToneMapped,
        current_hdr_image: None,
        hdr_image_cache: HashMap::new(),
        current_hdr_tiled_image: None,
        hdr_tiled_source_cache: HashMap::new(),
        current_hdr_tiled_preview: None,
        hdr_tiled_preview_cache: HashMap::new(),
        hdr_sdr_fallback_indices: HashSet::new(),
        hdr_placeholder_fallback_indices: HashSet::new(),
        hdr_in_flight_fallback_refinements: HashSet::new(),
        deferred_sdr_uploads: HashMap::new(),
        ultra_hdr_capacity_sensitive_indices: HashSet::new(),
        animation: None,
        pan_offset: Vec2::ZERO,
        zoom_factor: 1.0,
        last_switch_time: Instant::now(),
        slideshow_paused: true,
        random_slideshow_order_ready: false,
        audio: AudioPlayer::new(),
        music_seeking_target_ms: None,
        music_seek_timeout: None,
        music_hud_last_activity: Instant::now(),
        show_settings: false,
        last_show_settings: false,
        settings_tab: SettingsTab::Library,
        about_icon_texture: None,
        images_ever_loaded: false,
        status_message: String::new(),
        error_message: None,
        is_font_error: false,
        modal_generation: 0,
        pending_fullscreen: None,
        font_families: Vec::new(),
        font_families_rx: None,
        temp_font_size: None,
        generation: 0,
        prefetch_prev_generation: None,
        cached_music_count: None,
        cached_pixels_per_point: 1.0,
        active_modal: None,
        music_scan_rx: None,
        scanning_music: false,
        music_scan_cancel: None,
        music_scan_path: None,
        scan_rx: None,
        scan_cancel: None,
        current_image_res: None,
        prev_texture: None,
        prev_hdr_image: None,
        transition_start: None,
        pending_transition_target: None,
        is_next: true,
        active_transition: TransitionStyle::None,
        osd: crate::ui::osd::OsdRenderer::new(),
        last_minimized: false,
        last_frame_time: Instant::now(),
        ipc_rx,
        animation_cache: HashMap::new(),
        tile_manager: None,
        prefetched_tiles: HashMap::new(),
        theme_cache: SystemThemeCache::default(),
        cached_palette: ThemePalette::dark(),
        is_printing: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        print_status_rx: None,
        pending_anim_frames: None,
        file_op_rx,
        file_op_tx,
        lightweight_file_op_tx,
        last_mouse_wheel_nav: 0.0,
        last_keyboard_nav: None,
        save_tx,
        save_error_rx,
        last_save_error: None,
        saver_handle: None,
        preload_budget_forward: 100 * 1024 * 1024,
        preload_budget_backward: 100 * 1024 * 1024,
        context_menu_pos: None,
        current_rotation: 0,
        tile_upload_quota: 32,
        cached_audio_devices: Vec::new(),
        music_hud_drag_offset: Vec2::ZERO,
        hotkeys_runtime,
        hotkeys_draft_config,
        hotkeys_save_error_rx,
        hotkeys_save_tx,
        hotkeys_saver_handle: None,
        last_hotkeys_save_error: None,
        hotkeys_apply_success_at: None,
        hotkeys_load_error: None,
        startup_hotkeys_alert_shown: false,
        hotkeys_capture_target: None,
        hotkeys_selected_row: None,
        hotkeys_add_row_dialog_open: false,
        hotkeys_add_row_action: crate::hotkeys::model::HotkeyActionId::NextImage,
        hotkeys_add_row_capture_active: false,
        hotkeys_add_row_captured_key: None,
        hotkeys_add_row_need_key_hint: false,
        context_menu_runtime,
        context_menu_draft_config,
        context_menu_save_error_rx,
        context_menu_save_tx,
        context_menu_saver_handle: None,
        last_context_menu_save_error: None,
        context_menu_apply_success_at: None,
        context_menu_selected_row: None,
        context_menu_scroll_to_selected: false,
        context_menu_drag_row: None,
        context_menu_help_open: false,
        context_menu_edit_dialog_open: false,
        context_menu_edit_target: None,
        context_menu_edit_draft: crate::context_menu::model::EditableContextMenuEntry::default(),
        refresh_scan_in_progress: false,
        refresh_scan_slideshow_was_playing: false,
        refresh_anchor_path: None,
    }
}

struct DummyHdrTiledSource {
    width: u32,
    height: u32,
}

impl crate::hdr::tiled::HdrTiledSource for DummyHdrTiledSource {
    fn source_kind(&self) -> crate::hdr::tiled::HdrTiledSourceKind {
        crate::hdr::tiled::HdrTiledSourceKind::InMemory
    }
    fn width(&self) -> u32 {
        self.width
    }
    fn height(&self) -> u32 {
        self.height
    }
    fn color_space(&self) -> crate::hdr::types::HdrColorSpace {
        crate::hdr::types::HdrColorSpace::LinearSrgb
    }
    fn generate_hdr_preview(
        &self,
        _max_w: u32,
        _max_h: u32,
    ) -> Result<crate::hdr::types::HdrImageBuffer, String> {
        Ok(crate::hdr::types::HdrImageBuffer {
            width: 1,
            height: 1,
            format: crate::hdr::types::HdrPixelFormat::Rgba32Float,
            color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
            metadata: crate::hdr::types::HdrImageMetadata::default(),
            rgba_f32: Arc::new(vec![0.0; 4]),
        })
    }
    fn generate_sdr_preview(
        &self,
        _max_w: u32,
        _max_h: u32,
    ) -> Result<(u32, u32, Vec<u8>), String> {
        Ok((1, 1, vec![0, 0, 0, 255]))
    }
    fn extract_tile_rgba32f_arc(
        &self,
        _x: u32,
        _y: u32,
        width: u32,
        height: u32,
    ) -> Result<Arc<crate::hdr::tiled::HdrTileBuffer>, String> {
        Ok(Arc::new(crate::hdr::tiled::HdrTileBuffer::new(
            width,
            height,
            crate::hdr::types::HdrColorSpace::LinearSrgb,
            Arc::new(vec![0.0; width as usize * height as usize * 4]),
        )))
    }
}

impl crate::loader::TiledImageSource for DummyHdrTiledSource {
    fn width(&self) -> u32 {
        self.width
    }
    fn height(&self) -> u32 {
        self.height
    }
    fn extract_tile(&self, _x: u32, _y: u32, w: u32, h: u32) -> Arc<Vec<u8>> {
        Arc::new(vec![0; w as usize * h as usize * 4])
    }
    fn generate_preview(&self, _max_w: u32, _max_h: u32) -> (u32, u32, Vec<u8>) {
        (1, 1, vec![0, 0, 0, 255])
    }
    fn full_pixels(&self) -> Option<Arc<Vec<u8>>> {
        None
    }
}

#[test]
fn evict_distant_prefetch_caches_evicts_all_distant_prefetched_tiled_and_hdr_resources() {
    use eframe::egui;

    let mut app = make_test_app();
    app.image_files = vec![
        PathBuf::from("img0.jpg"),
        PathBuf::from("img1.jpg"),
        PathBuf::from("img2.jpg"),
        PathBuf::from("img3.jpg"),
        PathBuf::from("img4.jpg"),
        PathBuf::from("img5.jpg"),
        PathBuf::from("img6.jpg"),
    ];
    app.current_index = 0;

    let dummy_source = Arc::new(DummyHdrTiledSource {
        width: 1024,
        height: 768,
    });

    let tm3 = TileManager::with_source(
        3,
        42,
        Arc::clone(&dummy_source) as Arc<dyn crate::loader::TiledImageSource>,
    );
    app.prefetched_tiles.insert(3, tm3);
    app.hdr_tiled_source_cache.insert(
        3,
        Arc::clone(&dummy_source) as Arc<dyn crate::hdr::tiled::HdrTiledSource>,
    );
    app.hdr_sdr_fallback_indices.insert(3);

    let ctx = egui::Context::default();
    let color_image = egui::ColorImage::from_rgba_unmultiplied([1, 1], &[0, 0, 0, 255]);
    let handle = ctx.load_texture("test_tex", color_image, egui::TextureOptions::LINEAR);
    app.texture_cache.insert(3, handle, 1024, 768, true, 0, 7);

    assert!(app.prefetched_tiles.contains_key(&3));
    assert!(app.hdr_tiled_source_cache.contains_key(&3));
    assert!(app.hdr_sdr_fallback_indices.contains(&3));
    assert!(app.texture_cache.contains(3));

    app.evict_distant_prefetch_caches();

    assert!(!app.prefetched_tiles.contains_key(&3));
    assert!(!app.hdr_tiled_source_cache.contains_key(&3));
    assert!(!app.hdr_sdr_fallback_indices.contains(&3));
    assert!(!app.texture_cache.contains(3));
}

#[test]
fn navigate_to_tiled_preview_without_tile_manager_triggers_load() {
    use eframe::egui;

    let mut app = make_test_app();
    app.image_files = vec![PathBuf::from("img0.jpg"), PathBuf::from("img1.jpg")];
    app.current_index = 0;

    let ctx = egui::Context::default();
    let color_image = egui::ColorImage::from_rgba_unmultiplied([1, 1], &[0, 0, 0, 255]);
    let handle = ctx.load_texture("test_tex", color_image, egui::TextureOptions::LINEAR);
    app.texture_cache.insert(1, handle, 2048, 1536, true, 0, 2);

    assert!(app.texture_cache.contains(1));
    assert!(app.texture_cache.is_preview_placeholder(1));
    assert!(app.tile_manager.is_none());
    assert!(!app.prefetched_tiles.contains_key(&1));
    assert!(!app.hdr_tiled_source_cache.contains_key(&1));

    app.navigate_to(1);

    assert_eq!(app.current_image_res, Some((2048, 1536)));
    assert!(app.loader.is_loading(1, app.generation));
}

#[test]
fn test_resolve_initial_position_during_and_after_scan() {
    let mut app = make_test_app();
    let initial_path = PathBuf::from("img2.jpg");
    app.initial_image = Some(initial_path.clone());
    app.image_files = vec![
        PathBuf::from("img0.jpg"),
        PathBuf::from("img1.jpg"),
        PathBuf::from("img2.jpg"),
    ];
    app.settings.resume_last_image = true;
    app.settings.last_viewed_image = Some(PathBuf::from("img1.jpg"));

    // Case 1: scanning is true (first batch)
    app.scanning = true;
    app.resolve_initial_position();
    // It should find the path in the unsorted/initial files and set current_index
    assert_eq!(app.current_index, 2);
    // But initial_image should not be consumed yet because scanning is true
    assert_eq!(app.initial_image, Some(initial_path.clone()));

    // Case 2: scanning is false (Done)
    app.scanning = false;
    app.resolve_initial_position();
    // It should still set current_index to the found path
    assert_eq!(app.current_index, 2);
    // And now initial_image should be consumed (set to None)
    assert!(app.initial_image.is_none());

    // Case 3: subsequent calls after scanning is done
    app.resolve_initial_position();
    // Since initial_image was consumed, it should fall back to resume_last_image (img1.jpg)
    assert_eq!(app.current_index, 1);
}
