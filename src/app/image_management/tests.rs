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
use crate::hdr::types::HdrImageMetadata;
use crate::loader::PreviewBundle;
use crate::loader::{ImageLoader, TextureCache};
use crate::settings::Settings;
use crate::theme::{SystemThemeCache, ThemePalette};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(crate) fn write_min_sized_test_image(path: &Path) {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(
        path,
        vec![0u8; crate::constants::MIN_IMAGE_FILE_BYTES as usize],
    )
    .expect("write test image");
}

pub(crate) fn test_image_path(name: &str) -> PathBuf {
    PathBuf::from(format!(
        "{}siv-img-mgmt-{}-{}-{name}",
        std::env::temp_dir().display(),
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ))
}

pub(crate) fn set_test_image_files(app: &mut ImageViewerApp, names: &[&str]) {
    app.image_files = names
        .iter()
        .map(|name| {
            let path = test_image_path(name);
            write_min_sized_test_image(&path);
            path
        })
        .collect();
    app.file_byte_len_by_index =
        vec![crate::constants::MIN_IMAGE_FILE_BYTES; app.image_files.len()];
}

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
        first_cached_hdr_still_for_index(&hdr_image_cache, &HashMap::new(), &HashMap::new(), 0)
            .map(|b| b.rgba_f32[0]),
        Some(1.0)
    );

    hdr_image_cache.clear();
    let mut animation_cache = HashMap::new();
    animation_cache.insert(
        1,
        AnimationPlayback {
            image_index: 1,
            textures: std::sync::Arc::new(Vec::new()),
            hdr_frames: Some(vec![Arc::clone(&anim_hdr)]),
            delays: std::sync::Arc::new(Vec::new()),
            current_frame: 0,
            frame_start: Instant::now(),
            cpu_frames: None,
        },
    );
    assert_eq!(
        first_cached_hdr_still_for_index(&hdr_image_cache, &animation_cache, &HashMap::new(), 1)
            .map(|b| b.rgba_f32[0]),
        Some(2.0)
    );

    let pending = PendingAnimUpload {
        image_index: 2,
        hdr_frames: Some(vec![Arc::clone(&pending_hdr)]),
        frames: Vec::new(),
        textures: std::sync::Arc::new(Vec::new()),
        delays: std::sync::Arc::new(Vec::new()),
        next_frame: 0,
    };
    let mut pending_map = HashMap::new();
    pending_map.insert(2, pending);
    assert_eq!(
        first_cached_hdr_still_for_index(&hdr_image_cache, &HashMap::new(), &pending_map, 2)
            .map(|b| b.rgba_f32[0]),
        Some(3.0)
    );
    assert!(
        first_cached_hdr_still_for_index(&hdr_image_cache, &HashMap::new(), &pending_map, 9)
            .is_none()
    );
}

#[test]
fn prefetch_resource_guard_syncs_when_not_committed() {
    use super::prefetch_resource_index::PrefetchResourceGuard;

    let mut app = make_test_app();
    {
        let _guard = PrefetchResourceGuard::new(&mut app, 3);
    }
    assert!(!app.prefetch_resource_indices.contains(&3));
}

#[test]
fn prefetch_resource_guard_keeps_index_after_commit() {
    use super::prefetch_resource_index::PrefetchResourceGuard;

    let mut app = make_test_app();
    {
        let guard = PrefetchResourceGuard::new(&mut app, 4);
        guard.commit();
    }
    assert!(app.prefetch_resource_indices.contains(&4));
}

#[test]
fn navigation_preserves_current_tile_manager_for_restore() {
    let source = Arc::new(DummyTiledSource {
        width: 4096,
        height: 4096,
    });
    let mut app = make_test_app();
    app.tile_manager = Some(TileManager::with_source(
        7,
        crate::loader::decode_profile_stub(),
        source,
    ));

    preserve_current_tile_manager_for_navigation(&mut app, 7, 8);

    assert!(app.tile_manager.is_none());
    assert!(app.prefetched_tiles.contains_key(&7));
    assert_eq!(app.prefetched_tiles.get(&7).unwrap().image_index, 7);
    assert!(app.prefetch_resource_indices.contains(&7));
}

#[test]
fn tiled_bootstrap_preview_replaces_only_lower_rank_cached_preview() {
    use crate::loader::{PreviewStage, TexturePreviewBufferTag};

    assert!(should_upload_tiled_bootstrap_preview(false, None, None));
    assert!(should_upload_tiled_bootstrap_preview(true, None, None));
    assert!(!should_upload_tiled_bootstrap_preview(
        true,
        Some(TexturePreviewBufferTag::TiledRefinedLoader),
        Some(PreviewStage::Refined),
    ));
    assert!(!should_upload_tiled_bootstrap_preview(
        true,
        Some(TexturePreviewBufferTag::TiledBootstrap),
        Some(PreviewStage::Initial),
    ));
}

#[test]
fn sdr_upload_budget_counts_decoded_rgba_bytes() {
    assert_eq!(decoded_rgba_bytes(1024, 512), 1024 * 512 * 4);
}

#[test]
fn sdr_upload_budget_scales_with_hardware_tier() {
    assert_eq!(
        sdr_upload_budget_bytes_per_frame(HardwareTier::Low),
        16 * 1024 * 1024
    );
    assert_eq!(
        sdr_upload_budget_bytes_per_frame(HardwareTier::Medium),
        32 * 1024 * 1024
    );
    assert_eq!(
        sdr_upload_budget_bytes_per_frame(HardwareTier::High),
        64 * 1024 * 1024
    );
}

#[test]
fn sdr_upload_budget_allows_current_image_regardless_of_budget() {
    assert!(should_upload_sdr_this_frame(
        true,
        64 * 1024 * 1024,
        64 * 1024 * 1024,
        32 * 1024 * 1024
    ));
}

#[test]
fn sdr_upload_budget_defers_background_image_that_would_exceed_budget() {
    assert!(!should_upload_sdr_this_frame(
        false,
        24 * 1024 * 1024,
        16 * 1024 * 1024,
        32 * 1024 * 1024
    ));
}

#[test]
fn sdr_upload_budget_allows_one_large_background_image_per_frame() {
    assert!(should_upload_sdr_this_frame(
        false,
        0,
        64 * 1024 * 1024,
        32 * 1024 * 1024
    ));
}

#[test]
fn background_uploads_defer_while_transition_is_animating() {
    assert!(should_defer_background_upload_during_transition(
        false, true, None,
    ));
    assert!(!should_defer_background_upload_during_transition(
        true, true, None,
    ));
    assert!(!should_defer_background_upload_during_transition(
        false, false, None,
    ));
    assert!(should_defer_background_upload_during_transition(
        false,
        false,
        Some(std::time::Instant::now()),
    ));
    assert!(should_defer_background_upload_during_transition(
        false,
        false,
        Some(std::time::Instant::now() - std::time::Duration::from_millis(500)),
    ));
}

#[test]
fn refinement_uploads_defer_for_current_index_while_transition_is_animating() {
    assert!(should_defer_refinement_during_transition(true));
    assert!(!should_defer_refinement_during_transition(false));
}

#[test]
fn background_preload_memory_guard_uses_adaptive_reserve() {
    assert_eq!(background_preload_memory_guard_threshold_mb(4 * 1024), 1024);
    assert_eq!(
        background_preload_memory_guard_threshold_mb(16 * 1024),
        3276
    );
    assert_eq!(
        background_preload_memory_guard_threshold_mb(64 * 1024),
        4096
    );

    assert!(should_skip_background_preloads_for_memory(1023, 4 * 1024));
    assert!(!should_skip_background_preloads_for_memory(1024, 4 * 1024));
    assert!(should_skip_background_preloads_for_memory(4095, 64 * 1024));
    assert!(!should_skip_background_preloads_for_memory(4096, 64 * 1024));
}

#[test]
fn preload_decode_budget_estimates_compressed_images_conservatively() {
    assert_eq!(
        estimate_preload_decode_bytes(10 * 1024 * 1024),
        120 * 1024 * 1024
    );
    assert_eq!(estimate_preload_decode_bytes(0), 0);
}

#[test]
fn preload_budget_skips_first_oversized_background_candidate() {
    assert_eq!(
        decide_preload_for_budget(0, 0, 600 * 1024 * 1024, 100 * 1024 * 1024),
        PreloadBudgetDecision::SkipCandidate
    );
}

#[test]
fn preload_budget_stops_after_budget_is_exhausted() {
    assert_eq!(
        decide_preload_for_budget(1, 80 * 1024 * 1024, 40 * 1024 * 1024, 100 * 1024 * 1024),
        PreloadBudgetDecision::StopDirection
    );
}

#[test]
fn preload_budget_skips_first_new_oversized_even_after_existing_cached_items() {
    assert_eq!(
        decide_preload_for_budget(2, 0, 600 * 1024 * 1024, 100 * 1024 * 1024),
        PreloadBudgetDecision::SkipCandidate
    );
}

#[test]
fn preload_budget_requests_unknown_or_fitting_candidate() {
    assert_eq!(
        decide_preload_for_budget(0, 0, 0, 100 * 1024 * 1024),
        PreloadBudgetDecision::Request
    );
    assert_eq!(
        decide_preload_for_budget(1, 40 * 1024 * 1024, 32 * 1024 * 1024, 100 * 1024 * 1024),
        PreloadBudgetDecision::Request
    );
}

#[test]
fn oversized_preload_candidate_allows_near_budget_or_large_file() {
    let budget = 100 * 1024 * 1024;
    assert!(should_request_oversized_preload_candidate(
        1024 * 1024,
        150 * 1024 * 1024,
        budget
    ));
    assert!(should_request_oversized_preload_candidate(
        100 * 1024 * 1024,
        600 * 1024 * 1024,
        budget
    ));
    assert!(!should_request_oversized_preload_candidate(
        1024 * 1024,
        151 * 1024 * 1024,
        budget
    ));
}

#[test]
fn preload_direction_skips_oversized_first_candidate_and_tries_next() {
    let mut app = make_test_app();
    set_test_image_files(&mut app, &["current.jpg", "huge.jpg", "small.jpg"]);
    app.file_byte_len_by_index = vec![1, 10 * 1024 * 1024, 1024 * 1024];

    app.preload_direction("test", vec![1, 2], 1, 32 * 1024 * 1024);

    assert!(!app.loader.is_loading(1));
    assert!(app.loader.is_loading(2));
}

#[test]
fn preload_direction_requests_large_oversized_candidate_for_tiled_probe() {
    let mut app = make_test_app();
    set_test_image_files(&mut app, &["current.jpg", "small.jpg"]);
    app.file_byte_len_by_index = vec![1, 100 * 1024 * 1024];

    app.preload_direction("test", vec![1, 2], 1, 32 * 1024 * 1024);

    assert!(app.loader.is_loading(1));
    assert!(!app.loader.is_loading(2));
}

#[test]
fn tiled_hdr_preview_buffer_cache_still_compares_dimensions() {
    // Float HDR preview buffers are not tagged in `TextureCache`; size guards remain here.
    let mut cache = HashMap::<usize, Arc<crate::hdr::types::HdrImageBuffer>>::new();
    let small = Arc::new(crate::hdr::types::HdrImageBuffer {
        width: 1024,
        height: 512,
        format: crate::hdr::types::HdrPixelFormat::Rgba32Float,
        color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
        metadata: crate::hdr::types::HdrImageMetadata::from_color_space(
            crate::hdr::types::HdrColorSpace::LinearSrgb,
        ),
        rgba_f32: Arc::new(vec![0.0; 4]),
    });
    cache.insert(0, small);
    let cached_max = cache.get(&0).map(|cached| cached.width.max(cached.height));
    assert!(cached_max.is_none_or(|max| 4096 > max));
    assert!(!cached_max.is_none_or(|max| 1024 > max));
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
fn tiled_sdr_preview_cache_policy_uses_tag_rank_not_dimensions() {
    use crate::loader::{PreviewStage, TexturePreviewBufferTag};

    assert!(should_cache_tiled_sdr_preview(
        false,
        false,
        None,
        None,
        TexturePreviewBufferTag::TiledRefinedLoader,
        PreviewStage::Refined,
    ));
    assert!(!should_cache_tiled_sdr_preview(
        true,
        false,
        Some(TexturePreviewBufferTag::MainWindowSdr),
        Some(PreviewStage::Refined),
        TexturePreviewBufferTag::TiledRefinedLoader,
        PreviewStage::Refined,
    ));
    assert!(should_cache_tiled_sdr_preview(
        true,
        true,
        None,
        None,
        TexturePreviewBufferTag::TiledRefinedLoader,
        PreviewStage::Refined,
    ));
    assert!(should_cache_tiled_sdr_preview(
        true,
        true,
        Some(TexturePreviewBufferTag::TiledBootstrap),
        Some(PreviewStage::Initial),
        TexturePreviewBufferTag::TiledRefinedLoader,
        PreviewStage::Refined,
    ));
    assert!(!should_cache_tiled_sdr_preview(
        true,
        true,
        Some(TexturePreviewBufferTag::TiledRefinedLoader),
        Some(PreviewStage::Refined),
        TexturePreviewBufferTag::TiledOnDemandSdr,
        PreviewStage::Refined,
    ));
    assert!(!should_cache_tiled_sdr_preview(
        true,
        true,
        Some(TexturePreviewBufferTag::TiledRefinedLoader),
        Some(PreviewStage::Refined),
        TexturePreviewBufferTag::TiledBootstrap,
        PreviewStage::Initial,
    ));
}

#[test]
fn current_image_load_guard_treats_hdr_tiled_source_as_loaded() {
    assert!(current_image_has_loaded_asset(false, true, false, false));
    assert!(current_image_has_loaded_asset(false, false, true, false));
    assert!(current_image_has_loaded_asset(false, false, false, true));
    assert!(!current_image_has_loaded_asset(false, false, false, false));
}

#[test]
fn has_loaded_asset_treats_prefetched_tiles_as_loaded() {
    let source = Arc::new(DummyTiledSource {
        width: 11811,
        height: 11811,
    });
    let mut app = make_test_app();
    assert!(!app.has_loaded_asset(1));
    app.prefetched_tiles.insert(
        1,
        TileManager::with_source(1, crate::loader::decode_profile_stub(), source),
    );
    assert!(
        app.has_loaded_asset(1),
        "async PSD install into prefetched_tiles must stop preload respawn"
    );
}

#[test]
fn hdr_fallback_texture_without_hdr_plane_is_not_loaded_asset() {
    assert!(hdr_fallback_asset_is_loaded(false, false));
    assert!(hdr_fallback_asset_is_loaded(true, true));
    assert!(!hdr_fallback_asset_is_loaded(true, false));
}

fn startup_preload_defer_can_release_now(
    runtime_probe_completed: bool,
    native_hdr_surface_requests_enabled: bool,
    selection: Option<&crate::hdr::monitor::HdrMonitorSelection>,
    output_mode: crate::hdr::types::HdrOutputMode,
    probe_completed_at: Option<std::time::Instant>,
    interim_hdr_decode_capacity: f32,
    current_target_format: Option<wgpu::TextureFormat>,
    desired_target_format: Option<wgpu::TextureFormat>,
) -> bool {
    super::startup_preload_defer_can_release(
        runtime_probe_completed,
        native_hdr_surface_requests_enabled,
        selection,
        output_mode,
        probe_completed_at,
        std::time::Instant::now(),
        interim_hdr_decode_capacity,
        current_target_format,
        desired_target_format,
    )
}

const HDR_SWAP_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const SDR_SWAP_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8Unorm;

#[test]
fn startup_preload_defer_waits_for_swap_chain_before_release() {
    use crate::hdr::monitor::HdrMonitorSelection;
    use crate::hdr::types::HdrOutputMode;

    let selection_hdr = HdrMonitorSelection {
        hdr_supported: true,
        label: "EDR".to_string(),
        max_luminance_nits: None,
        max_full_frame_luminance_nits: None,
        max_hdr_capacity: Some(2.89),
        hdr_capacity_source: Some("macOS maximumExtendedDynamicRangeColorComponentValue"),
        native_surface_encoding: None,
        ..HdrMonitorSelection::new("", false)
    };

    assert!(!startup_preload_defer_can_release_now(
        true,
        true,
        Some(&selection_hdr),
        HdrOutputMode::MacOsEdr,
        None,
        16.0,
        Some(SDR_SWAP_FORMAT),
        Some(HDR_SWAP_FORMAT),
    ));
    assert!(startup_preload_defer_can_release_now(
        true,
        true,
        Some(&selection_hdr),
        HdrOutputMode::MacOsEdr,
        None,
        16.0,
        Some(HDR_SWAP_FORMAT),
        Some(HDR_SWAP_FORMAT),
    ));
}

#[test]
fn startup_preload_defer_waits_for_hdr_output_mode_after_runtime_probe() {
    use crate::hdr::monitor::HdrMonitorSelection;
    use crate::hdr::types::HdrOutputMode;

    let selection_sdr = Some(&HdrMonitorSelection {
        hdr_supported: false,
        label: "SDR".to_string(),
        max_luminance_nits: None,
        max_full_frame_luminance_nits: None,
        max_hdr_capacity: None,
        hdr_capacity_source: None,
        native_surface_encoding: None,
        ..HdrMonitorSelection::new("", false)
    });
    let selection_hdr_unknown = Some(&HdrMonitorSelection {
        hdr_supported: true,
        label: "EDR".to_string(),
        max_luminance_nits: None,
        max_full_frame_luminance_nits: None,
        max_hdr_capacity: None,
        hdr_capacity_source: None,
        native_surface_encoding: None,
        ..HdrMonitorSelection::new("", false)
    });
    let selection_hdr_source_only = Some(&HdrMonitorSelection {
        hdr_supported: true,
        label: "EDR".to_string(),
        max_luminance_nits: None,
        max_full_frame_luminance_nits: None,
        max_hdr_capacity: None,
        hdr_capacity_source: Some("macOS maximumExtendedDynamicRangeColorComponentValue"),
        native_surface_encoding: None,
        ..HdrMonitorSelection::new("", false)
    });
    let selection_hdr_known = Some(&HdrMonitorSelection {
        hdr_supported: true,
        label: "EDR".to_string(),
        max_luminance_nits: None,
        max_full_frame_luminance_nits: None,
        max_hdr_capacity: Some(2.89),
        hdr_capacity_source: Some("macOS maximumExtendedDynamicRangeColorComponentValue"),
        native_surface_encoding: None,
        ..HdrMonitorSelection::new("", false)
    });

    assert!(!startup_preload_defer_can_release_now(
        false,
        true,
        selection_hdr_known,
        HdrOutputMode::WindowsScRgb,
        None,
        1.0,
        Some(HDR_SWAP_FORMAT),
        Some(HDR_SWAP_FORMAT),
    ));
    assert!(startup_preload_defer_can_release_now(
        true,
        true,
        selection_sdr,
        HdrOutputMode::SdrToneMapped,
        None,
        1.0,
        Some(SDR_SWAP_FORMAT),
        Some(SDR_SWAP_FORMAT),
    ));
    assert!(!startup_preload_defer_can_release_now(
        true,
        true,
        selection_hdr_known,
        HdrOutputMode::SdrToneMapped,
        None,
        1.0,
        Some(HDR_SWAP_FORMAT),
        Some(HDR_SWAP_FORMAT),
    ));
    assert!(!startup_preload_defer_can_release_now(
        true,
        true,
        selection_hdr_unknown,
        HdrOutputMode::WindowsScRgb,
        None,
        1.0,
        Some(HDR_SWAP_FORMAT),
        Some(HDR_SWAP_FORMAT),
    ));
    assert!(!super::monitor_hdr_decode_capacity_is_known(
        selection_hdr_source_only
    ));
    assert!(!startup_preload_defer_can_release_now(
        true,
        true,
        selection_hdr_source_only,
        HdrOutputMode::WindowsScRgb,
        None,
        1.0,
        Some(HDR_SWAP_FORMAT),
        Some(HDR_SWAP_FORMAT),
    ));
    assert!(startup_preload_defer_can_release_now(
        true,
        true,
        selection_hdr_known,
        HdrOutputMode::WindowsScRgb,
        None,
        1.0,
        Some(HDR_SWAP_FORMAT),
        Some(HDR_SWAP_FORMAT),
    ));
    assert!(startup_preload_defer_can_release_now(
        true,
        true,
        selection_hdr_known,
        HdrOutputMode::MacOsEdr,
        None,
        1.0,
        Some(HDR_SWAP_FORMAT),
        Some(HDR_SWAP_FORMAT),
    ));
    assert!(startup_preload_defer_can_release_now(
        true,
        true,
        selection_hdr_unknown,
        HdrOutputMode::MacOsEdr,
        None,
        4.926,
        Some(HDR_SWAP_FORMAT),
        Some(HDR_SWAP_FORMAT),
    ));
}

#[test]
fn startup_preload_defer_releases_when_native_hdr_surface_disabled() {
    use crate::hdr::monitor::HdrMonitorSelection;
    use crate::hdr::types::HdrOutputMode;

    let selection_wsi_hdr = HdrMonitorSelection {
        hdr_supported: true,
        label: "eDP-1".to_string(),
        max_luminance_nits: Some(450.0),
        max_full_frame_luminance_nits: None,
        max_hdr_capacity: None,
        hdr_capacity_source: Some("Vulkan WSI surface formats"),
        native_surface_encoding: None,
        ..HdrMonitorSelection::new("", false)
    };

    assert!(startup_preload_defer_can_release_now(
        true,
        false,
        Some(&selection_wsi_hdr),
        HdrOutputMode::SdrToneMapped,
        None,
        1.0,
        Some(SDR_SWAP_FORMAT),
        Some(HDR_SWAP_FORMAT),
    ));
    assert!(!startup_preload_defer_can_release_now(
        true,
        true,
        Some(&selection_wsi_hdr),
        HdrOutputMode::SdrToneMapped,
        None,
        1.0,
        Some(SDR_SWAP_FORMAT),
        Some(HDR_SWAP_FORMAT),
    ));
}

#[test]
fn startup_preload_defer_releases_after_probe_timeout_when_capacity_unknown() {
    use crate::hdr::monitor::HdrMonitorSelection;
    use crate::hdr::types::HdrOutputMode;
    use std::time::{Duration, Instant};

    let selection_hdr_unknown = HdrMonitorSelection {
        hdr_supported: true,
        label: "EDR".to_string(),
        max_luminance_nits: None,
        max_full_frame_luminance_nits: None,
        max_hdr_capacity: None,
        hdr_capacity_source: None,
        native_surface_encoding: None,
        ..HdrMonitorSelection::new("", false)
    };
    let now = Instant::now();
    let probe_at = now - super::STARTUP_PRELOAD_DEFER_MAX_AFTER_PROBE - Duration::from_secs(1);
    assert!(super::startup_preload_defer_can_release(
        true,
        true,
        Some(&selection_hdr_unknown),
        HdrOutputMode::WindowsScRgb,
        Some(probe_at),
        now,
        1.0,
        Some(SDR_SWAP_FORMAT),
        Some(HDR_SWAP_FORMAT),
    ));
}

#[test]
fn monitor_hdr_decode_capacity_is_known_when_edr_capacity_reported() {
    use crate::hdr::monitor::HdrMonitorSelection;

    assert!(!super::monitor_hdr_decode_capacity_is_known(None));
    assert!(super::monitor_hdr_decode_capacity_is_known(Some(
        &HdrMonitorSelection {
            hdr_supported: false,
            label: "SDR".to_string(),
            max_luminance_nits: None,
            max_full_frame_luminance_nits: None,
            max_hdr_capacity: None,
            hdr_capacity_source: None,
            native_surface_encoding: None,
            ..HdrMonitorSelection::new("", false)
        }
    )));
    assert!(!super::monitor_hdr_decode_capacity_is_known(Some(
        &HdrMonitorSelection {
            hdr_supported: true,
            label: "EDR".to_string(),
            max_luminance_nits: None,
            max_full_frame_luminance_nits: None,
            max_hdr_capacity: None,
            hdr_capacity_source: None,
            native_surface_encoding: None,
            ..HdrMonitorSelection::new("", false)
        }
    )));
    assert!(!super::monitor_hdr_decode_capacity_is_known(Some(
        &HdrMonitorSelection {
            hdr_supported: true,
            label: "EDR".to_string(),
            max_luminance_nits: None,
            max_full_frame_luminance_nits: None,
            max_hdr_capacity: None,
            hdr_capacity_source: Some("macOS maximumExtendedDynamicRangeColorComponentValue"),
            native_surface_encoding: None,
            ..HdrMonitorSelection::new("", false)
        }
    )));
    assert!(super::monitor_hdr_decode_capacity_is_known(Some(
        &HdrMonitorSelection {
            hdr_supported: true,
            label: "EDR".to_string(),
            max_luminance_nits: None,
            max_full_frame_luminance_nits: None,
            max_hdr_capacity: Some(2.89),
            hdr_capacity_source: Some("macOS maximumExtendedDynamicRangeColorComponentValue"),
            native_surface_encoding: None,
            ..HdrMonitorSelection::new("", false)
        }
    )));
}

#[test]
fn retention_keeps_registered_inflight_outside_prefetch_window() {
    use super::result_gate::ResultGateContext;

    let ctx = ResultGateContext {
        current_index: 233,
        image_count: 250,
        max_distance: 2,
    };
    assert!(ctx.retention_for(230, true).should_retain());
    assert!(!ctx.retention_for(230, false).should_retain());
}

#[test]
fn raw_hq_bootstrap_only_detects_texture_without_hdr_plane() {
    use std::collections::HashMap;
    use std::path::PathBuf;

    let files = vec![PathBuf::from("sample.ORF")];
    assert!(super::raw_hq_has_bootstrap_sdr_only(
        &files,
        0,
        true,
        &HashMap::new(),
        &HashMap::new(),
        true,
        false,
    ));
    assert!(!super::raw_hq_has_bootstrap_sdr_only(
        &files,
        0,
        true,
        &HashMap::new(),
        &HashMap::new(),
        false,
        false,
    ));
}

#[test]
fn neighbor_work_defers_while_current_main_in_flight() {
    assert!(super::should_defer_neighbor_work_for_current_main(
        false, true
    ));
    assert!(!super::should_defer_neighbor_work_for_current_main(
        true, true
    ));
    assert!(!super::should_defer_neighbor_work_for_current_main(
        false, false
    ));
    assert!(!super::should_defer_neighbor_work_for_current_main(
        true, false
    ));
}

#[test]
fn strip_skip_slow_allows_neighbors_when_current_main_loader_failed() {
    let mut app = make_test_app();
    app.settings.preload = true;
    app.prefetch_window_max_distance = crate::loader::DEFAULT_PREFETCH_WINDOW_DISTANCE;
    app.image_files = vec![
        PathBuf::from("a.heif"),
        PathBuf::from("b.heif"),
        PathBuf::from("c.heif"),
    ];
    app.current_index = 0;
    app.main_loader_failed_indices.insert(0);

    assert!(
        !app.strip_cold_skip_slow_embedded_sdr_primary(1),
        "neighbor strip should fall back to slow path when current load failed"
    );
    assert!(
        !app.strip_cold_skip_slow_embedded_sdr_primary(2),
        "second neighbor strip should fall back to slow path when current load failed"
    );
}

#[test]
fn install_image_error_persists_and_surfaces_when_not_current() {
    let mut app = make_test_app();
    app.image_files = vec![PathBuf::from("a.psd"), PathBuf::from("b.psd")];
    app.current_index = 0;
    app.record_installed_display_mode(1, crate::loader::RenderShape::Tiled);
    app.directory_tree_strip_tiled_attempted.insert(1);

    app.install_image_error(1, "hidden by the designer");

    assert!(app.main_loader_failed_indices.contains(&1));
    assert_eq!(
        app.main_loader_failed_errors.get(&1).map(String::as_str),
        Some("hidden by the designer")
    );
    assert!(
        app.error_message.is_none(),
        "non-current fail must not clobber current UI"
    );
    assert_eq!(app.installed_display_mode(1), None);
    assert!(!app.directory_tree_strip_tiled_attempted.contains(&1));

    app.current_index = 1;
    app.surface_main_loader_failure_for_current();
    let msg = app
        .error_message
        .as_deref()
        .expect("failed index should surface error");
    assert!(msg.contains("hidden by the designer"));

    app.note_main_loader_install_success(1);
    assert!(!app.main_loader_failed_indices.contains(&1));
    assert!(!app.main_loader_failed_errors.contains_key(&1));
}

#[test]
fn install_image_error_sets_message_when_current() {
    let mut app = make_test_app();
    app.image_files = vec![PathBuf::from("a.psd")];
    app.current_index = 0;

    app.install_image_error(0, "no displayable image");

    let msg = app
        .error_message
        .as_deref()
        .expect("current fail should set error_message");
    assert!(msg.contains("no displayable image"));
    assert!(app.main_loader_failed_indices.contains(&0));
}

#[test]
fn reload_current_does_not_reload_psd_only_directory() {
    let mut app = make_test_app();
    set_test_image_files(&mut app, &["a.psd"]);
    app.current_index = 0;
    app.error_message = Some("load failed".into());
    app.main_loader_failed_indices.insert(0);

    app.reload_current();

    assert!(
        !app.loader.is_loading(0),
        "reload_current is RAW-only and must not start a PSD reload"
    );
    assert!(app.error_message.is_some());
    assert!(app.main_loader_failed_indices.contains(&0));
}

#[test]
fn psd_hidden_layer_strategy_change_reloads_failed_current_psd() {
    let mut app = make_test_app();
    set_test_image_files(&mut app, &["a.psd", "b.jpg"]);
    app.current_index = 0;
    app.error_message = Some("load failed".into());
    app.main_loader_failed_indices.insert(0);
    app.main_loader_failed_errors
        .insert(0, "all layers hidden".into());
    app.settings.psd_hidden_layer_strategy = crate::settings::PsdHiddenLayerStrategy::ShowAllLayers;

    app.reload_after_psd_hidden_layer_strategy_change();

    assert!(
        app.loader.is_loading(0),
        "changing the PSD hidden-layer strategy must re-request the current PSD"
    );
    assert!(
        app.error_message.is_none(),
        "stale load error must clear so the canvas can show the new decode"
    );
    assert!(
        !app.main_loader_failed_indices.contains(&0),
        "failed mark must clear or schedule_preloads will refuse to retry"
    );
}

#[test]
fn navigate_back_to_failed_index_retries_load() {
    let mut app = make_test_app();
    set_test_image_files(&mut app, &["a.jpg", "b.jpg"]);
    app.current_index = 0;
    app.main_loader_failed_indices.insert(1);
    app.main_loader_failed_errors
        .insert(1, "transient oom".into());

    let ctx = egui::Context::default();
    app.navigate_to(1, &ctx);

    assert_eq!(app.current_index, 1);
    assert!(
        !app.main_loader_failed_indices.contains(&1),
        "re-navigation must clear the permanent gate so transient failures can retry"
    );
    assert!(
        app.loader.is_loading(1),
        "re-navigation to a failed index must request_load again"
    );
    assert!(
        app.error_message.is_none(),
        "retry clears the stale error until a new failure arrives"
    );
}

#[test]
fn strip_skip_slow_defers_neighbors_while_current_main_in_flight() {
    let mut app = make_test_app();
    app.settings.preload = true;
    app.prefetch_window_max_distance = crate::loader::DEFAULT_PREFETCH_WINDOW_DISTANCE;
    set_test_image_files(&mut app, &["a.heif", "b.heif"]);
    app.current_index = 0;
    app.loader.request_load(
        0,
        app.image_files[0].clone(),
        app.settings.raw_high_quality,
        app.settings.raw_demosaic_mode,
        app.settings.psd_hidden_layer_strategy,
    );

    assert!(
        app.strip_cold_skip_slow_embedded_sdr_primary(1),
        "neighbor strip should defer while current main decode is in flight"
    );
}

#[test]
fn strip_cold_defers_current_index_while_main_loader_in_flight() {
    let mut app = make_test_app();
    app.settings.preload = true;
    app.image_files = vec![PathBuf::from("a.avif"), PathBuf::from("b.avif")];
    app.current_index = 0;
    app.loader.request_load(
        0,
        app.image_files[0].clone(),
        app.settings.raw_high_quality,
        app.settings.raw_demosaic_mode,
        app.settings.psd_hidden_layer_strategy,
    );

    assert!(
        app.should_defer_neighbor_strip_for_current_main(0),
        "current-index strip cold path should defer until main asset is installed"
    );
}

#[test]
fn strip_neighbor_not_deferred_for_current_main_when_no_embedded_sdr_share() {
    let mut app = make_test_app();
    app.settings.preload = true;
    app.prefetch_window_max_distance = crate::loader::DEFAULT_PREFETCH_WINDOW_DISTANCE;
    app.image_files = vec![PathBuf::from("a.avif"), PathBuf::from("b.avif")];
    app.current_index = 0;
    app.loader.request_load(
        0,
        app.image_files[0].clone(),
        app.settings.raw_high_quality,
        app.settings.raw_demosaic_mode,
        app.settings.psd_hidden_layer_strategy,
    );

    assert!(
        !app.strip_path_benefits_from_main_loader_embedded_sdr_share(&app.image_files[0]),
        "plain .avif extension alone must not imply main-loader strip sharing"
    );
    assert!(
        !app.strip_path_benefits_from_main_loader_embedded_sdr_share(&app.image_files[1]),
        "neighbor plain .avif must not defer strip to main loader embedded-SDR path"
    );
}

#[test]
fn directory_tree_list_sort_restarts_current_main_loader_after_permute() {
    use crate::app::directory_tree::ImageListSortColumn;
    use crate::settings::BrowseMode;

    let mut app = make_test_app();
    app.settings.browse_mode = BrowseMode::Tree;
    app.settings.show_directory_tree_nav = true;
    set_test_image_files(&mut app, &["XMN_2332.NEF", "XMN_2333.NEF", "XMN_2334.NEF"]);
    app.file_byte_len_by_index = vec![crate::constants::MIN_IMAGE_FILE_BYTES; 3];
    app.file_modified_unix_by_index = vec![None, None, None];
    app.current_index = 0;
    app.loader.request_load(
        0,
        app.image_files[0].clone(),
        app.settings.raw_high_quality,
        app.settings.raw_demosaic_mode,
        app.settings.psd_hidden_layer_strategy,
    );
    assert!(app.loader.is_loading(0));

    assert!(app.apply_directory_tree_image_list_sort(ImageListSortColumn::Name, false,));
    assert_eq!(app.current_index, 0);
    assert!(
        app.image_files[0]
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.contains("2334"))
    );
    assert!(!app.loader.is_loading(2));
    assert!(app.loader.is_loading(0));
}

#[test]
fn directory_tree_list_sort_reschedules_neighbor_preloads_after_permute() {
    use crate::app::directory_tree::ImageListSortColumn;

    let mut app = make_test_app();
    app.settings.preload = true;
    app.cached_available_memory_mb = 8192;
    app.cached_total_memory_mb = 16384;
    app.prefetch_window_max_distance = crate::loader::DEFAULT_PREFETCH_WINDOW_DISTANCE;
    set_test_image_files(&mut app, &["a.jpg", "b.jpg", "c.jpg"]);
    app.file_byte_len_by_index = vec![100, 200, 300];
    app.file_modified_unix_by_index = vec![None, None, None];
    app.current_index = 1;
    app.hdr_image_cache.insert(
        1,
        std::sync::Arc::new(crate::hdr::types::HdrImageBuffer {
            width: 1,
            height: 1,
            format: crate::hdr::types::HdrPixelFormat::Rgba32Float,
            color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
            metadata: crate::hdr::types::HdrImageMetadata::from_color_space(
                crate::hdr::types::HdrColorSpace::LinearSrgb,
            ),
            rgba_f32: std::sync::Arc::new(vec![0.0; 4]),
        }),
    );

    assert!(app.apply_directory_tree_image_list_sort(ImageListSortColumn::Name, false));
    assert_eq!(app.current_index, 1);
    assert!(
        app.loader.is_loading(0) || app.loader.is_loading(2),
        "expected neighbor preloads to restart after list reorder"
    );
}

#[test]
fn directory_tree_list_sort_bootstraps_visible_strip_thumbnails() {
    use crate::app::directory_tree::ImageListSortColumn;
    use crate::settings::BrowseMode;

    let mut app = make_test_app();
    app.settings.browse_mode = BrowseMode::Tree;
    app.settings.show_directory_tree_nav = true;
    app.settings.directory_tree_show_list_previews = true;
    set_test_image_files(&mut app, &["a.jpg", "b.jpg", "c.jpg"]);
    app.file_byte_len_by_index = vec![100, 200, 300];
    app.file_modified_unix_by_index = vec![None, None, None];
    app.current_index = 0;

    assert!(app.apply_directory_tree_image_list_sort(ImageListSortColumn::Name, false));
    assert!(app.directory_tree_strip_bootstrap_after_scan);
    assert_eq!(app.directory_tree_strip_bootstrap_frames, 0);
}

#[test]
fn hdr_gain_map_sdr_display_change_refreshes_cached_heic_without_reload() {
    use crate::hdr::types::{
        HEIF_EMBEDDED_SDR_PRIMARY_GAIN_MAP_SOURCE, HdrGainMapMetadata, HdrImageBuffer,
        HdrImageMetadata, HdrPixelFormat,
    };
    use crate::settings::HdrGainMapSdrDisplayMode;

    let mut app = make_test_app();
    set_test_image_files(&mut app, &["photo.heic"]);
    app.current_index = 0;
    app.hdr_target_format = Some(wgpu::TextureFormat::Bgra8Unorm);
    app.settings.hdr_gain_map_sdr_display = HdrGainMapSdrDisplayMode::HdrToneMapped;

    let metadata = HdrImageMetadata {
        gain_map: Some(HdrGainMapMetadata {
            source: HEIF_EMBEDDED_SDR_PRIMARY_GAIN_MAP_SOURCE,
            target_hdr_capacity: None,
            diagnostic: String::new(),
            capped_display_referred: true,
            apple_heic_deferred: None,
            iso_deferred: None,
        }),
        ..Default::default()
    };
    let hdr = Arc::new(HdrImageBuffer {
        width: 4032,
        height: 3024,
        format: HdrPixelFormat::Rgba32Float,
        color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
        metadata,
        rgba_f32: Arc::new(vec![0.5; 4032 * 3024 * 4]),
    });
    app.current_hdr_image = Some(crate::app::CurrentHdrImage::new(0, Arc::clone(&hdr)));
    app.hdr_image_cache.insert(0, hdr);
    app.hdr_sdr_fallback_indices.insert(0);
    let ctx = egui::Context::default();
    app.upload_hdr_sdr_fallback_texture(
        0,
        &DecodedImage::new(64, 48, vec![128; 64 * 48 * 4]),
        &ctx,
    );
    assert!(app.active_hdr_plane_displays_index(0));

    app.settings.hdr_gain_map_sdr_display = HdrGainMapSdrDisplayMode::EmbeddedSdrMaster;
    app.reload_after_hdr_gain_map_sdr_display_change();

    assert!(app.hdr_image_cache.contains_key(&0));
    assert!(!app.loader.is_loading(0));
    assert!(!app.active_hdr_plane_displays_index(0));
}

#[test]
fn hdr_gain_map_sdr_display_change_reloads_heif_marked_ultra_hdr_sensitive() {
    use crate::settings::HdrGainMapSdrDisplayMode;

    let mut app = make_test_app();
    set_test_image_files(&mut app, &["photo.heic"]);
    app.current_index = 0;
    app.settings.hdr_gain_map_sdr_display = HdrGainMapSdrDisplayMode::EmbeddedSdrMaster;
    app.ultra_hdr_capacity_sensitive_indices.insert(0);
    let ctx = egui::Context::default();
    app.upload_static_sdr_texture(
        0,
        &DecodedImage::new(64, 48, vec![128; 64 * 48 * 4]),
        "img_0".into(),
        crate::loader::TexturePreviewBufferTag::MainWindowSdr,
        crate::loader::PreviewStage::Refined,
        &ctx,
    );

    app.settings.hdr_gain_map_sdr_display = HdrGainMapSdrDisplayMode::HdrToneMapped;
    app.reload_after_hdr_gain_map_sdr_display_change();

    assert!(!app.texture_cache.contains(0));
    assert!(app.loader.is_loading(0));
}

#[test]
fn neighbor_image_install_yields_until_current_ready() {
    assert!(super::should_yield_neighbor_image_install_until_current_ready(false, false, true));
    assert!(!super::should_yield_neighbor_image_install_until_current_ready(true, false, true));
    assert!(!super::should_yield_neighbor_image_install_until_current_ready(false, true, true));
    assert!(!super::should_yield_neighbor_image_install_until_current_ready(false, false, false));
}

#[test]
fn background_preload_defers_while_current_raw_gpu_path_active() {
    assert!(super::should_defer_background_preload_for_raw_gpu_current(
        true, true, true, false, false
    ));
    assert!(super::should_defer_background_preload_for_raw_gpu_current(
        true, true, false, true, false
    ));
    assert!(super::should_defer_background_preload_for_raw_gpu_current(
        true, true, false, false, true
    ));
    assert!(!super::should_defer_background_preload_for_raw_gpu_current(
        false, true, true, false, false
    ));
    assert!(!super::should_defer_background_preload_for_raw_gpu_current(
        true, false, true, false, false
    ));
}

#[test]
fn background_preload_schedule_with_force_neighbors() {
    let mut app = make_test_app();
    set_test_image_files(&mut app, &["current.NEF", "neighbor1.jpg", "neighbor2.jpg"]);
    app.current_index = 0;
    app.settings.raw_high_quality = true;
    app.settings.preload = true;
    app.cached_available_memory_mb = 8192;
    app.cached_total_memory_mb = 16384;

    // Simulate current RAW image has already loaded its HDR plane (e.g. after retain on capacity refine)
    let dummy_hdr = std::sync::Arc::new(crate::hdr::types::HdrImageBuffer {
        width: 100,
        height: 100,
        format: crate::hdr::types::HdrPixelFormat::Rgba32Float,
        color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
        metadata: crate::hdr::types::HdrImageMetadata::from_color_space(
            crate::hdr::types::HdrColorSpace::LinearSrgb,
        ),
        rgba_f32: std::sync::Arc::new(vec![0.0; 4]),
    });
    app.hdr_image_cache.insert(0, dummy_hdr);

    // Simulate current RAW image is loading on loader, and demosaic await present is true.
    // So current_raw_gpu_path_active will be true.
    app.loader.test_register_inflight(0);
    app.raw_gpu_demosaic_await_hdr_present = true;

    // Call schedule_preloads_with_options with force_neighbors = false.
    // Neighbors should NOT be preloaded since it is deferred.
    app.schedule_preloads_with_options(true, false);
    assert!(!app.loader.is_loading(1));
    assert!(!app.loader.is_loading(2));

    // Now current_is_loading becomes false (load task finished)
    // but raw_gpu_demosaic_await_hdr_present is still true (so current_raw_gpu_path_active is still true).
    app.loader.finish_image_request(0);

    // Call schedule_preloads_with_options with force_neighbors = true.
    // It should bypass the defer return and preload neighbor1.
    app.schedule_preloads_with_options(true, true);
    assert!(app.loader.is_loading(1));
}

#[test]
fn schedule_preloads_only_requests_neighbors_within_retention_window() {
    let mut app = make_test_app();
    let names: Vec<String> = (0..10).map(|i| format!("img{i}.jpg")).collect();
    let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
    set_test_image_files(&mut app, &name_refs);
    app.current_index = 0;
    app.settings.preload = true;
    app.cached_available_memory_mb = 8192;
    app.cached_total_memory_mb = 16384;
    app.prefetch_window_max_distance = crate::loader::DEFAULT_PREFETCH_WINDOW_DISTANCE;
    app.hdr_image_cache.insert(
        0,
        std::sync::Arc::new(crate::hdr::types::HdrImageBuffer {
            width: 1,
            height: 1,
            format: crate::hdr::types::HdrPixelFormat::Rgba32Float,
            color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
            metadata: crate::hdr::types::HdrImageMetadata::from_color_space(
                crate::hdr::types::HdrColorSpace::LinearSrgb,
            ),
            rgba_f32: std::sync::Arc::new(vec![0.0; 4]),
        }),
    );

    app.schedule_preloads(true);

    for idx in 3..=7 {
        assert!(
            !app.loader.is_loading(idx),
            "unexpected preload outside retention window for idx={idx}"
        );
    }
    assert!(
        app.loader.is_loading(1)
            || app.loader.is_loading(2)
            || app.loader.is_loading(8)
            || app.loader.is_loading(9),
        "expected at least one in-window neighbor preload"
    );
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
fn pending_transition_yields_background_results_until_current_is_ready() {
    assert!(should_yield_background_result_for_pending_transition(
        false,
        Some(7),
        7,
    ));
    assert!(!should_yield_background_result_for_pending_transition(
        true,
        Some(7),
        7,
    ));
    assert!(!should_yield_background_result_for_pending_transition(
        false,
        Some(8),
        7,
    ));
    assert!(!should_yield_background_result_for_pending_transition(
        false, None, 7,
    ));
}

#[test]
fn post_transition_background_upload_quota_is_throttled() {
    assert_eq!(background_upload_quota_after_transition(3, None), 3);
    assert_eq!(
        background_upload_quota_after_transition(3, Some(std::time::Instant::now())),
        1,
    );
}

#[test]
fn post_transition_background_uploads_are_spaced() {
    let settled = std::time::Instant::now();
    let last_upload = std::time::Instant::now();
    assert!(should_space_background_upload_after_transition(
        false,
        Some(settled),
        Some(last_upload),
    ));
    assert!(!should_space_background_upload_after_transition(
        true,
        Some(settled),
        Some(last_upload),
    ));
    assert!(!should_space_background_upload_after_transition(
        false,
        None,
        Some(last_upload),
    ));
}

#[test]
fn preview_results_without_sdr_pixels_do_not_count_as_background_uploads() {
    let result = crate::loader::PreviewResult {
        index: 1,
        decode_profile: crate::loader::decode_profile_stub(),
        source_key: source_key_for_path(&PathBuf::from("preview.avif")),
        preview_bundle: PreviewBundle::refined(),
        error: None,
        cpu_demosaic_ms: None,
        raw_bootstrap_osd: None,
        sdr_texture_tag: None,
    };

    assert!(!preview_result_has_sdr_upload(&result));
}

#[test]
fn current_preview_updates_are_not_deferred_during_transition() {
    assert!(!should_defer_preview_update_during_transition(true, true));
    assert!(should_defer_preview_update_during_transition(false, true));
    assert!(!should_defer_preview_update_during_transition(false, false));
}

#[test]
fn hdr_gain_map_sdr_display_change_evicts_cached_gain_map_and_reloads_current() {
    use crate::hdr::types::{
        HEIF_EMBEDDED_SDR_PRIMARY_GAIN_MAP_SOURCE, HdrGainMapMetadata, HdrImageBuffer,
        HdrImageMetadata, HdrPixelFormat,
    };
    use crate::settings::HdrGainMapSdrDisplayMode;

    let mut app = make_test_app();
    set_test_image_files(&mut app, &["photo.heic"]);
    app.current_index = 0;
    app.settings.hdr_gain_map_sdr_display = HdrGainMapSdrDisplayMode::EmbeddedSdrMaster;

    let metadata = HdrImageMetadata {
        gain_map: Some(HdrGainMapMetadata {
            source: HEIF_EMBEDDED_SDR_PRIMARY_GAIN_MAP_SOURCE,
            target_hdr_capacity: None,
            diagnostic: String::new(),
            capped_display_referred: true,
            apple_heic_deferred: None,
            iso_deferred: None,
        }),
        ..Default::default()
    };
    let hdr = Arc::new(HdrImageBuffer {
        width: 4032,
        height: 3024,
        format: HdrPixelFormat::Rgba32Float,
        color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
        metadata,
        rgba_f32: Arc::new(Vec::new()),
    });
    app.hdr_image_cache.insert(0, hdr);
    app.hdr_sdr_fallback_indices.insert(0);

    app.settings.hdr_gain_map_sdr_display = HdrGainMapSdrDisplayMode::HdrToneMapped;
    app.reload_after_hdr_gain_map_sdr_display_change();

    assert!(!app.hdr_image_cache.contains_key(&0));
    assert!(app.loader.is_loading(0));
}

#[test]
fn hdr_gain_map_sdr_display_change_evicts_heif_tone_map_primary_cache() {
    use crate::hdr::types::{HdrGainMapMetadata, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat};
    use crate::settings::HdrGainMapSdrDisplayMode;

    let mut app = make_test_app();
    set_test_image_files(&mut app, &["photo.heic"]);
    app.current_index = 0;
    app.settings.hdr_gain_map_sdr_display = HdrGainMapSdrDisplayMode::HdrToneMapped;

    let metadata = HdrImageMetadata {
        gain_map: Some(HdrGainMapMetadata {
            source: "HEIF",
            target_hdr_capacity: None,
            diagnostic: "AppleHdrGainMap".to_string(),
            capped_display_referred: false,
            apple_heic_deferred: None,
            iso_deferred: None,
        }),
        ..Default::default()
    };
    let hdr = Arc::new(HdrImageBuffer {
        width: 4032,
        height: 3024,
        format: HdrPixelFormat::Rgba32Float,
        color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
        metadata,
        rgba_f32: Arc::new(vec![0.5; 4032 * 3024 * 4]),
    });
    app.hdr_image_cache.insert(0, hdr);
    app.hdr_sdr_fallback_indices.insert(0);

    app.settings.hdr_gain_map_sdr_display = HdrGainMapSdrDisplayMode::EmbeddedSdrMaster;
    app.reload_after_hdr_gain_map_sdr_display_change();

    assert!(!app.hdr_image_cache.contains_key(&0));
    assert!(app.loader.is_loading(0));
}

#[test]
fn hdr_gain_map_sdr_display_change_evicts_apple_heic_tone_map_cache() {
    use crate::hdr::types::{
        AppleHeicGainMapGpuSource, HdrGainMapMetadata, HdrImageBuffer, HdrImageMetadata,
        HdrPixelFormat,
    };
    use crate::settings::HdrGainMapSdrDisplayMode;

    let mut app = make_test_app();
    set_test_image_files(&mut app, &["photo.heic"]);
    app.current_index = 0;
    app.settings.hdr_gain_map_sdr_display = HdrGainMapSdrDisplayMode::HdrToneMapped;

    let metadata = HdrImageMetadata {
        gain_map: Some(HdrGainMapMetadata {
            source: "HEIF",
            target_hdr_capacity: Some(2.0),
            diagnostic: String::new(),
            capped_display_referred: false,
            apple_heic_deferred: Some(AppleHeicGainMapGpuSource {
                gain_rgba: Arc::new(vec![0; 4]),
                gain_width: 1,
                gain_height: 1,
                headroom_span: 1.0,
                stops: 1.0,
            }),
            iso_deferred: None,
        }),
        ..Default::default()
    };
    let hdr = Arc::new(HdrImageBuffer {
        width: 4032,
        height: 3024,
        format: HdrPixelFormat::Rgba32Float,
        color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
        metadata,
        rgba_f32: Arc::new(vec![0.5; 4032 * 3024 * 4]),
    });
    app.hdr_image_cache.insert(0, hdr);
    app.hdr_sdr_fallback_indices.insert(0);

    app.settings.hdr_gain_map_sdr_display = HdrGainMapSdrDisplayMode::EmbeddedSdrMaster;
    app.reload_after_hdr_gain_map_sdr_display_change();

    assert!(!app.hdr_image_cache.contains_key(&0));
    assert!(app.loader.is_loading(0));
}

#[test]
fn capture_transition_prefers_sdr_texture_for_embedded_iso_gain_map() {
    use crate::hdr::types::{HdrGainMapMetadata, IsoGainMapGpuSource};
    use crate::settings::HdrGainMapSdrDisplayMode;

    let mut app = make_test_app();
    set_test_image_files(&mut app, &["gain_map.avif"]);
    app.current_index = 0;
    app.hdr_target_format = Some(wgpu::TextureFormat::Bgra8Unorm);
    app.settings.hdr_gain_map_sdr_display = HdrGainMapSdrDisplayMode::EmbeddedSdrMaster;
    let iso_sdr = vec![128_u8, 64, 32, 255];
    let metadata = HdrImageMetadata {
        gain_map: Some(HdrGainMapMetadata {
            source: "AVIF",
            target_hdr_capacity: Some(4.0),
            diagnostic: String::new(),
            capped_display_referred: false,
            apple_heic_deferred: None,
            iso_deferred: Some(IsoGainMapGpuSource {
                sdr_rgba: Arc::new(iso_sdr.clone()),
                gain_rgba: Arc::new(vec![0; 4]),
                gain_width: 1,
                gain_height: 1,
                metadata: crate::hdr::gain_map::GainMapMetadata {
                    gain_map_min: [0.0; 3],
                    gain_map_max: [1.0; 3],
                    gamma: [1.0; 3],
                    offset_sdr: [0.0; 3],
                    offset_hdr: [0.0; 3],
                    hdr_capacity_min: 1.0,
                    hdr_capacity_max: 4.0,
                    backward_direction: false,
                },
            }),
        }),
        ..Default::default()
    };
    let hdr = Arc::new(crate::hdr::types::HdrImageBuffer {
        width: 1,
        height: 1,
        format: crate::hdr::types::HdrPixelFormat::Rgba32Float,
        color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
        metadata,
        rgba_f32: Arc::new(Vec::new()),
    });
    app.current_hdr_image = Some(crate::app::CurrentHdrImage::new(0, Arc::clone(&hdr)));
    app.hdr_image_cache.insert(0, hdr);
    app.hdr_sdr_fallback_indices.insert(0);
    let ctx = egui::Context::default();
    app.upload_hdr_sdr_fallback_texture(0, &DecodedImage::new(1, 1, iso_sdr), &ctx);

    let (texture, transition_hdr) = app.capture_transition_source_at_index(0, &ctx);
    assert!(texture.is_some());
    assert!(
        transition_hdr.is_none(),
        "embedded SDR master outgoing transition must reuse SDR texture, not HDR plane"
    );
}

#[test]
fn capture_transition_keeps_hdr_plane_for_tone_mapped_iso_gain_map() {
    use crate::hdr::types::{HdrGainMapMetadata, IsoGainMapGpuSource};
    use crate::settings::HdrGainMapSdrDisplayMode;

    let mut app = make_test_app();
    set_test_image_files(&mut app, &["gain_map.avif"]);
    app.current_index = 0;
    app.hdr_target_format = Some(wgpu::TextureFormat::Bgra8Unorm);
    app.settings.hdr_gain_map_sdr_display = HdrGainMapSdrDisplayMode::HdrToneMapped;
    let iso_sdr = vec![128_u8, 64, 32, 255];
    let metadata = HdrImageMetadata {
        gain_map: Some(HdrGainMapMetadata {
            source: "AVIF",
            target_hdr_capacity: Some(4.0),
            diagnostic: String::new(),
            capped_display_referred: false,
            apple_heic_deferred: None,
            iso_deferred: Some(IsoGainMapGpuSource {
                sdr_rgba: Arc::new(iso_sdr.clone()),
                gain_rgba: Arc::new(vec![0; 4]),
                gain_width: 1,
                gain_height: 1,
                metadata: crate::hdr::gain_map::GainMapMetadata {
                    gain_map_min: [0.0; 3],
                    gain_map_max: [1.0; 3],
                    gamma: [1.0; 3],
                    offset_sdr: [0.0; 3],
                    offset_hdr: [0.0; 3],
                    hdr_capacity_min: 1.0,
                    hdr_capacity_max: 4.0,
                    backward_direction: false,
                },
            }),
        }),
        ..Default::default()
    };
    let hdr = Arc::new(crate::hdr::types::HdrImageBuffer {
        width: 1,
        height: 1,
        format: crate::hdr::types::HdrPixelFormat::Rgba32Float,
        color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
        metadata,
        rgba_f32: Arc::new(Vec::new()),
    });
    app.current_hdr_image = Some(crate::app::CurrentHdrImage::new(0, Arc::clone(&hdr)));
    app.hdr_image_cache.insert(0, hdr);
    app.hdr_sdr_fallback_indices.insert(0);
    let ctx = egui::Context::default();
    app.upload_hdr_sdr_fallback_texture(0, &DecodedImage::new(1, 1, iso_sdr), &ctx);

    let (_, transition_hdr) = app.capture_transition_source_at_index(0, &ctx);
    assert!(transition_hdr.is_some());
}

#[test]
fn embedded_iso_gain_map_sdr_master_flushes_deferred_fallback_texture() {
    use crate::hdr::types::{HdrGainMapMetadata, IsoGainMapGpuSource};
    use crate::settings::HdrGainMapSdrDisplayMode;

    let mut app = make_test_app();
    set_test_image_files(&mut app, &["gain_map.avif"]);
    app.current_index = 0;
    app.hdr_target_format = Some(wgpu::TextureFormat::Bgra8Unorm);
    app.settings.hdr_gain_map_sdr_display = HdrGainMapSdrDisplayMode::EmbeddedSdrMaster;
    let iso_sdr = vec![128_u8, 64, 32, 255];
    let metadata = HdrImageMetadata {
        gain_map: Some(HdrGainMapMetadata {
            source: "AVIF",
            target_hdr_capacity: None,
            diagnostic: String::new(),
            capped_display_referred: false,
            apple_heic_deferred: None,
            iso_deferred: Some(IsoGainMapGpuSource {
                sdr_rgba: Arc::new(iso_sdr.clone()),
                gain_rgba: Arc::new(Vec::new()),
                gain_width: 0,
                gain_height: 0,
                metadata: crate::hdr::gain_map::GainMapMetadata {
                    gain_map_min: [0.0; 3],
                    gain_map_max: [1.0; 3],
                    gamma: [1.0; 3],
                    offset_sdr: [0.0; 3],
                    offset_hdr: [0.0; 3],
                    hdr_capacity_min: 1.0,
                    hdr_capacity_max: 4.0,
                    backward_direction: false,
                },
            }),
        }),
        ..Default::default()
    };
    let hdr = Arc::new(crate::hdr::types::HdrImageBuffer {
        width: 1,
        height: 1,
        format: crate::hdr::types::HdrPixelFormat::Rgba32Float,
        color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
        metadata,
        rgba_f32: Arc::new(Vec::new()),
    });
    app.current_hdr_image = Some(crate::app::CurrentHdrImage::new(0, Arc::clone(&hdr)));
    app.hdr_image_cache.insert(0, hdr);
    app.hdr_sdr_fallback_indices.insert(0);
    app.insert_deferred_sdr_upload(0, DecodedImage::new(1, 1, iso_sdr));

    let ctx = egui::Context::default();
    assert!(
        !app.active_hdr_plane_displays_index(0),
        "embedded SDR master should route through SDR texture, not HDR plane"
    );
    app.flush_deferred_sdr_upload_for_index(0, &ctx);

    assert!(app.texture_cache.contains(0));
    assert!(!app.deferred_sdr_uploads.contains_key(&0));
}

#[test]
fn placeholder_sdr_transition_source_is_kept_when_hdr_output_is_unavailable() {
    assert!(!should_drop_placeholder_sdr_transition_source(
        true, true, false
    ));
    assert!(should_drop_placeholder_sdr_transition_source(
        true, true, true
    ));
    assert!(!should_drop_placeholder_sdr_transition_source(
        false, true, true
    ));
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
fn transition_source_hdr_prefers_current_even_when_placeholder_fallback() {
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

    let res = select_transition_source_hdr(Some(Arc::clone(&dummy_hdr)), true, None);
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
fn transition_direction_reuses_wrap_aware_navigation_direction() {
    assert!(transition_direction_is_next(9, 0, 10));
    assert!(!transition_direction_is_next(0, 9, 10));
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
fn tiled_manager_install_helper_preserves_source_and_profile() {
    let source: Arc<dyn crate::loader::TiledImageSource> = Arc::new(DummyTiledSource {
        width: 1024,
        height: 768,
    });
    let profile = crate::loader::decode_profile_stub();

    let tm = build_tiled_manager_with_best_preview(9, profile.clone(), source, None);

    assert_eq!(tm.image_index, 9);
    assert_eq!(tm.decode_profile, profile);
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
fn view_change_clears_pending_tile_requests() {
    use crate::loader::TilePixelKind;
    use crate::tile_cache::{PendingTileKey, TileCoord, TileManager};

    let source: Arc<dyn crate::loader::TiledImageSource> = Arc::new(DummyTiledSource {
        width: 1024,
        height: 768,
    });
    let mut tile_manager = Some(TileManager::with_source(
        4,
        crate::loader::decode_profile_stub(),
        source,
    ));
    tile_manager
        .as_mut()
        .unwrap()
        .pending_tiles
        .insert(PendingTileKey::new(
            TileCoord { col: 0, row: 0 },
            TilePixelKind::Sdr,
        ));

    assert!(invalidate_tile_manager_requests_for_view_change(
        &mut tile_manager
    ));

    let tile_manager = tile_manager.expect("tile manager should remain installed");
    assert!(tile_manager.pending_tiles.is_empty());
}

#[test]
fn hdr_load_result_capacity_is_stale_when_sensitive_hdr_mismatch() {
    let load = LoadResult {
        index: 0,
        decode_profile: crate::loader::decode_profile_stub(),
        source_key: 0,
        result: Ok(crate::loader::ImageData::Hdr {
            hdr: Box::new(crate::hdr::types::HdrImageBuffer {
                width: 1,
                height: 1,
                format: crate::hdr::types::HdrPixelFormat::Rgba32Float,
                color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
                metadata: crate::hdr::types::HdrImageMetadata::default(),
                rgba_f32: Arc::new(vec![0.0; 4]),
            }),
            fallback: crate::loader::DecodedImage::new(1, 1, vec![0, 0, 0, 255]),
        }),
        preview_bundle: PreviewBundle::initial(),
        ultra_hdr_capacity_sensitive: true,
        sdr_fallback_is_placeholder: false,
        target_hdr_capacity: 1.0,
        raw_osd: None,
        psd_osd: None,
        uploaded_planes: None,
        staged_gpu_plane_upload: false,
        device_id: None,
    };
    assert!(hdr_load_result_capacity_is_stale(&load, 2.0));
    assert!(!hdr_load_result_capacity_is_stale(&load, 1.0));
}

#[test]
fn hdr_load_result_capacity_is_stale_ignores_hq_raw_scene_linear() {
    let load = LoadResult {
        index: 0,
        decode_profile: crate::loader::decode_profile_stub(),
        source_key: 0,
        result: Ok(crate::loader::ImageData::Hdr {
            hdr: Box::new(crate::hdr::types::HdrImageBuffer {
                width: 1,
                height: 1,
                format: crate::hdr::types::HdrPixelFormat::Rgba32Float,
                color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
                metadata: crate::hdr::types::HdrImageMetadata::default(),
                rgba_f32: Arc::new(vec![0.0; 4]),
            }),
            fallback: crate::loader::DecodedImage::new(1, 1, vec![0, 0, 0, 255]),
        }),
        preview_bundle: PreviewBundle::initial(),
        ultra_hdr_capacity_sensitive: true,
        sdr_fallback_is_placeholder: false,
        target_hdr_capacity: 3.478,
        raw_osd: Some(crate::loader::RawOsdInfo::empty()),
        psd_osd: None,
        uploaded_planes: None,
        staged_gpu_plane_upload: false,
        device_id: None,
    };
    assert!(!hdr_load_result_capacity_is_stale(&load, 3.786));
}

#[test]
fn hdr_load_result_capacity_is_stale_ignores_non_sensitive_loads() {
    let load = LoadResult {
        index: 0,
        decode_profile: crate::loader::decode_profile_stub(),
        source_key: 0,
        result: Ok(crate::loader::ImageData::Hdr {
            hdr: Box::new(crate::hdr::types::HdrImageBuffer {
                width: 1,
                height: 1,
                format: crate::hdr::types::HdrPixelFormat::Rgba32Float,
                color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
                metadata: crate::hdr::types::HdrImageMetadata::default(),
                rgba_f32: Arc::new(vec![0.0; 4]),
            }),
            fallback: crate::loader::DecodedImage::new(1, 1, vec![0, 0, 0, 255]),
        }),
        preview_bundle: PreviewBundle::initial(),
        ultra_hdr_capacity_sensitive: false,
        sdr_fallback_is_placeholder: false,
        target_hdr_capacity: 1.0,
        raw_osd: None,
        psd_osd: None,
        uploaded_planes: None,
        staged_gpu_plane_upload: false,
        device_id: None,
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
            &HashMap::new(),
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
            &HashMap::new(),
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
            &HashMap::new(),
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
            &HashMap::new(),
            &hdr_tiled_preview_cache,
            Some(&current_image),
            0
        )
        .map(|b| b.rgba_f32[0]),
        Some(5.0)
    );
}

pub(crate) fn make_test_app() -> ImageViewerApp {
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

    let (osd_event_tx, osd_event_rx) = crossbeam_channel::unbounded();
    ImageViewerApp {
        pixel_data_source: None,
        pixel_hover_cache: None,
        pixel_region_first_point: None,
        settings: Settings::default(),
        image_files: Vec::new(),
        cached_image_strip_path_index: None,
        #[cfg(feature = "avif-native")]
        cached_avif_strip_probe: parking_lot::Mutex::new(None),
        #[cfg(feature = "avif-native")]
        avif_strip_probe_inflight: parking_lot::Mutex::new(std::collections::HashSet::new()),
        #[cfg(feature = "avif-native")]
        avif_strip_probe_result_tx: {
            let (tx, _rx) = crossbeam_channel::bounded(1);
            tx
        },
        #[cfg(feature = "avif-native")]
        avif_strip_probe_result_rx: crossbeam_channel::never(),
        file_byte_len_by_index: Vec::new(),
        file_modified_unix_by_index: Vec::new(),
        current_index: 0,
        initial_image: None,
        scanning: false,
        hardware_tier: HardwareTier::Medium,
        loader: ImageLoader::new(),
        texture_cache: TextureCache::new(10),
        hdr_capabilities: crate::hdr::capabilities::HdrCapabilities::sdr("test"),
        hdr_renderer: crate::hdr::renderer::HdrImageRenderer::new(),
        wgpu_pipeline_cache: None,
        wgpu_adapter_info: None,
        current_device_id: 1,
        hdr_callback_resources_prewarm:
            crate::hdr::renderer::HdrCallbackResourcesPrewarm::new_shared(),
        hdr_target_format: None,
        hdr_monitor_state: crate::hdr::monitor::HdrMonitorState::default(),
        cached_window_placement: None,
        cached_restore_placement: None,
        cached_directory_tree_window_placement: None,
        cached_directory_tree_restore_placement: None,
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
        #[cfg(target_os = "linux")]
        last_logged_linux_hdr_runtime_diag: None,
        #[cfg(feature = "preload-debug")]
        hdr_preload_gate_log: crate::app::preload_hdr_gate::GateLogState::default(),
        rgb10a2_pq_encode_requested: false,
        ultra_hdr_decode_capacity: 1.0,
        ultra_hdr_decode_output_mode: crate::hdr::types::HdrOutputMode::SdrToneMapped,
        preload_deferred_for_hdr_capacity: false,
        current_hdr_image: None,
        hdr_image_cache: HashMap::new(),
        current_hdr_tiled_image: None,
        hdr_tiled_source_cache: HashMap::new(),
        current_hdr_tiled_preview: None,
        hdr_tiled_preview_cache: HashMap::new(),
        hdr_sdr_fallback_indices: HashSet::new(),
        hdr_placeholder_fallback_indices: HashSet::new(),
        hdr_raw_gpu_demosaic_pending_indices: HashSet::new(),
        hdr_raw_gpu_demosaic_baked_indices: HashSet::new(),
        hdr_raw_gpu_demosaic_pending_key_index: HashMap::new(),
        gpu_demosaic_failed_indices: HashSet::new(),
        main_loader_failed_indices: HashSet::new(),
        main_loader_failed_errors: HashMap::new(),
        raw_gpu_demosaic_await_hdr_present: false,
        raw_gpu_embedded_bootstrap_indices: HashSet::new(),
        hdr_register_prewarm_repush_counts: HashMap::new(),
        raw_demosaic_baked_notify: Arc::new(Mutex::new(Vec::new())),
        hdr_pending_work: crate::hdr::renderer::HdrPendingWorkQueues::new_shared(),
        cpu_raw_refinement_pending_indices: HashSet::new(),
        hq_tiled_preview_pending_indices: HashSet::new(),
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
        pending_open_directory: false,
        folder_picker: crate::app::folder_picker::FolderPickerRuntime::new(),
        directory_tree: crate::app::DirectoryTreeRuntime::new(),
        auto_hidden_directory_tree_nav: false,
        embedded_directory_tree_panel_bootstrapped: false,
        directory_tree_strip_cache:
            crate::app::directory_tree_strip_cache::DirectoryTreeStripCache::default(),
        directory_tree_strip_tiled_attempted: std::collections::HashSet::new(),
        directory_tree_strip_cold_attempted: std::collections::HashSet::new(),
        directory_tree_strip_cold_awaiting_main_loader: std::collections::HashSet::new(),
        directory_tree_strip_pending_main_handoff: std::collections::HashMap::new(),
        directory_tree_strip_generate_inflight: std::collections::HashSet::new(),
        directory_tree_strip_inflight_tokens: std::collections::HashMap::new(),
        directory_tree_strip_next_job_token: 0,
        directory_tree_strip_static_full_decode_inflight: std::collections::HashSet::new(),
        directory_tree_strip_preview_tx: {
            let (tx, _rx) = crossbeam_channel::unbounded();
            tx
        },
        directory_tree_strip_preview_rx: crossbeam_channel::never(),
        directory_tree_strip_inflight_release_tx: {
            let (tx, _rx) = crossbeam_channel::unbounded();
            tx
        },
        directory_tree_strip_inflight_release_rx: crossbeam_channel::never(),
        directory_tree_strip_pending_gpu_initial: VecDeque::new(),
        directory_tree_strip_pending_gpu_refined: VecDeque::new(),
        directory_tree_strip_pending_gpu_next_seq: 0,
        directory_tree_strip_pending_drop_scratch: Vec::new(),
        directory_tree_places_load_rx: None,
        font_families: Vec::new(),
        font_families_rx: None,
        cached_music_count: None,
        cached_pixels_per_point: 1.0,
        active_modal: None,
        music_scan_rx: None,
        scanning_music: false,
        music_scan_cancel: None,
        music_scan_path: None,
        scan_rx: None,
        scan_cancel: None,
        root_redraw_wake: None,
        directory_tree_theme: std::sync::Arc::new(parking_lot::Mutex::new(ThemePalette::dark())),
        pending_directory_tree_repaint: false,
        pending_directory_tree_select_index: None,
        pending_directory_tree_state_sync: false,
        pending_directory_tree_sync_warning: None,
        directory_tree_sync_defer_frames: 0,
        scan_generation: 0,
        scan_results_pending_since: None,
        pending_preload_after_directory_scan: false,
        pending_preload_after_scan_last_attempt: None,
        directory_tree_strip_bootstrap_after_scan: false,
        directory_tree_strip_bootstrap_frames: 0,
        strip_preload_cooldown_frames: 0,
        strip_stale_retain_last_generation: u64::MAX,
        strip_cold_awaiting_scratch: Vec::new(),
        strip_indices_scratch: Vec::new(),
        strip_cold_candidates_scratch: Vec::with_capacity(
            crate::app::directory_tree::MAX_COLD_STRIP_SCHEDULE_PER_FRAME,
        ),
        strip_cold_seen_scratch: Vec::with_capacity(
            crate::app::directory_tree::MAX_COLD_STRIP_SCHEDULE_PER_FRAME,
        ),
        current_image_res: None,
        canvas_display_timing: crate::preload_debug::CanvasDisplayTiming::default(),
        raw_metadata: crate::app::view_status::RawMetadataStore::new(osd_event_tx.clone()),
        image_status: crate::app::view_status::ImageViewStatus::new(osd_event_tx.clone()),
        current_file_name: String::new(),
        cached_keyboard_hint: rust_i18n::t!("hint.keyboard").to_string(),
        cached_directory_tree_viewport_title: rust_i18n::t!("directory_tree.title").to_string(),
        directory_tree_viewport_title_sent: false,
        cached_frame_render_plan: None,
        cached_frame_hdr_render_path: None,
        frame_effective_hdr_monitor_selection: None,
        prev_texture: None,
        prev_hdr_image: None,
        prev_transition_rect: None,
        transition_start: None,
        transition_settled_at: None,
        transition_end_hold: false,
        pending_transition_target: None,
        last_background_upload_at: None,
        is_next: true,
        active_transition: TransitionStyle::None,
        osd: crate::ui::osd::OsdRenderer::new(osd_event_rx),
        last_minimized: false,
        last_frame_time: Instant::now(),
        last_logic_shared_at: None,
        ipc_rx,
        animation_cache: HashMap::new(),
        installed_display_modes: HashMap::new(),
        tile_manager: None,
        tiled_primary_visible_scratch: HashSet::new(),
        tiled_visible_coords_scratch: HashSet::new(),
        tiled_visible_tiles_scratch: Vec::new(),
        tiled_primary_visible_tiles_scratch: Vec::new(),
        tiled_tile_visits_scratch: Vec::new(),
        tiled_protected_keys_scratch: Vec::new(),
        prefetched_tiles: HashMap::new(),
        prefetch_resource_indices: HashSet::new(),
        theme_cache: SystemThemeCache::default(),
        cached_palette: ThemePalette::dark(),
        is_printing: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        print_status_rx: None,
        pending_anim_frames: HashMap::new(),
        file_op_rx,
        file_op_tx,
        lightweight_file_op_tx,
        background_threads: crate::app::background_threads::BackgroundThreadJoiner::new(),
        last_mouse_wheel_nav: 0.0,
        last_canvas_rect: None,
        last_keyboard_nav: None,
        save_tx,
        save_error_rx,
        last_save_error: None,
        saver_handle: None,
        preload_budget_forward: 100 * 1024 * 1024,
        preload_budget_backward: 100 * 1024 * 1024,
        preload_memory: crate::app::preload_memory::PreloadMemorySnapshot::new(),
        cached_available_memory_mb: 0,
        cached_total_memory_mb: 0,
        prefetch_window_max_distance: crate::loader::DEFAULT_PREFETCH_WINDOW_DISTANCE,
        context_menu_pos: None,
        context_menu_viewport: None,
        context_menu_label_cache: None,
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
        context_menu_apply_error: None,
        context_menu_selected_row: None,
        context_menu_scroll_to_selected: false,
        context_menu_drag_row: None,
        context_menu_help_open: false,
        context_menu_edit_dialog_open: false,
        context_menu_edit_target: None,
        context_menu_edit_draft: crate::context_menu::model::EditableContextMenuEntry::default(),
        context_menu_exe_browse_requested: false,
        refresh_scan_in_progress: false,
        refresh_scan_slideshow_was_playing: false,
        refresh_anchor_path: None,
        refresh_strip_files_snapshot: None,
        explicit_quit: false,
        tray_state: None,
        hidden_to_tray: false,
        pending_hide_to_tray: false,
        tray_cmd_rx: crossbeam_channel::never(),
        copy_cut_overwrite_if_exists: false,
    }
}

#[test]
fn load_directory_preserves_tree_nav_selected_namespace_path() {
    let mut app = make_test_app();
    app.settings.browse_mode = crate::settings::BrowseMode::Tree;
    let namespace = PathBuf::from(r"\\?\siv-tree\Mount/%2Fcustom");
    app.settings.tree_nav_selected_namespace_path = Some(namespace.clone());

    let dir = std::env::temp_dir().join("siv_load_directory_namespace_test");
    let _ = std::fs::create_dir_all(&dir);
    app.load_directory(dir.clone());

    assert_eq!(
        app.settings.tree_nav_selected_namespace_path,
        Some(namespace)
    );

    if let Some(cancel) = app.scan_cancel.take() {
        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn hidden_tree_nav_uses_last_image_dir_for_cold_start_open() {
    let mut app = make_test_app();
    let old_tree_dir = PathBuf::from(r"D:\photos");
    let cli_image_dir = PathBuf::from(r"E:\photos");

    app.settings.browse_mode = crate::settings::BrowseMode::Tree;
    app.settings.show_directory_tree_nav = false;
    app.settings.tree_nav_selected_dir = Some(old_tree_dir);
    app.settings.last_image_dir = Some(cli_image_dir.clone());

    assert_eq!(app.current_browse_directory(), Some(cli_image_dir));
}

#[test]
fn load_directory_clears_in_progress_refresh_scan_state() {
    let mut app = make_test_app();
    app.refresh_scan_in_progress = true;
    app.refresh_scan_slideshow_was_playing = true;
    app.slideshow_paused = true;

    let dir = std::env::temp_dir().join("siv_load_directory_refresh_test");
    let _ = std::fs::create_dir_all(&dir);
    app.load_directory(dir.clone());

    assert!(!app.refresh_scan_in_progress);
    assert!(!app.refresh_scan_slideshow_was_playing);
    assert!(!app.slideshow_paused);

    if let Some(cancel) = app.scan_cancel.take() {
        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn sort_image_file_rows_in_place_reorders_parallel_columns() {
    let mut paths = vec![
        PathBuf::from("c.jpg"),
        PathBuf::from("a.jpg"),
        PathBuf::from("b.jpg"),
    ];
    let mut sizes = vec![30_u64, 10, 20];
    let mut modified = vec![Some(3_i64), Some(1), Some(2)];

    let old_to_new =
        super::directory::sort_image_file_rows_in_place(&mut paths, &mut sizes, &mut modified)
            .expect("order should change");

    assert_eq!(
        paths,
        vec![
            PathBuf::from("a.jpg"),
            PathBuf::from("b.jpg"),
            PathBuf::from("c.jpg"),
        ]
    );
    assert_eq!(sizes, vec![10, 20, 30]);
    assert_eq!(modified, vec![Some(1), Some(2), Some(3)]);
    assert_eq!(old_to_new, vec![2, 0, 1]);
}

#[test]
fn sort_image_file_rows_in_place_noop_when_already_sorted() {
    let mut paths = vec![PathBuf::from("a.jpg"), PathBuf::from("b.jpg")];
    let mut sizes = vec![1_u64, 2];
    let mut modified = vec![Some(1_i64), Some(2)];

    assert!(
        super::directory::sort_image_file_rows_in_place(&mut paths, &mut sizes, &mut modified,)
            .is_none()
    );
}

#[test]
fn relocate_index_keyed_cache_moves_raw_osd_info() {
    let mut app = make_test_app();
    app.raw_metadata
        .insert_or_update(2, crate::loader::RawOsdInfo::empty());

    app.relocate_index_keyed_cache(2, 0, true);

    assert!(!app.raw_metadata.contains_key(2));
    assert!(app.raw_metadata.contains_key(0));
}

#[test]
fn relocate_index_keyed_cache_moves_gpu_demosaic_failed_indices() {
    let mut app = make_test_app();
    app.gpu_demosaic_failed_indices.insert(2);

    app.relocate_index_keyed_cache(2, 0, true);

    assert!(!app.gpu_demosaic_failed_indices.contains(&2));
    assert!(app.gpu_demosaic_failed_indices.contains(&0));
}

#[test]
fn install_current_tiled_hdr_image_refreshes_hdr_osd_line() {
    let mut app = make_test_app();
    app.image_files = vec![PathBuf::from("hdr_tiled.avif")];
    app.current_index = 0;
    app.hdr_capabilities = crate::hdr::capabilities::HdrCapabilities::sdr("test");
    app.hdr_capabilities.available = true;
    app.hdr_capabilities.output_mode = crate::hdr::types::HdrOutputMode::WindowsScRgb;
    app.hdr_capabilities.native_presentation_enabled = true;
    app.hdr_target_format = Some(wgpu::TextureFormat::Rgba16Float);

    let source = Arc::new(DummyHdrTiledSource {
        width: 4096,
        height: 4096,
    });
    let ctx = eframe::egui::Context::default();

    app.install_tiled_image(
        crate::app::image_management::image_install::TiledImageInstall {
            idx: 0,
            decode_profile: crate::loader::decode_profile_stub(),
            source: Arc::clone(&source) as Arc<dyn crate::loader::TiledImageSource>,
            hdr_source: Some(Arc::clone(&source) as Arc<dyn crate::hdr::tiled::HdrTiledSource>),
            sdr_preview: None,
            hdr_preview: None,
            hdr_sdr_fallback: true,
            ultra_hdr_capacity_sensitive: false,
            ctx: &ctx,
        },
    );

    assert!(app.osd.has_hdr_line());
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
        crate::loader::decode_profile_stub(),
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
    app.texture_cache.insert(
        3,
        handle,
        crate::loader::TextureCacheInsert {
            orig_w: 1024,
            orig_h: 768,
            needs_tile_manager: true,
            buffer_tag: crate::loader::TexturePreviewBufferTag::TiledBootstrap,
            stage: crate::loader::PreviewStage::Initial,
            current_index: 0,
            total_count: 7,
        },
    );

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
fn evict_distant_prefetch_caches_evicts_stale_uploaded_static_textures() {
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

    let ctx = egui::Context::default();
    let color_image = egui::ColorImage::from_rgba_unmultiplied([1, 1], &[0, 0, 0, 255]);
    let stale = ctx.load_texture(
        "stale_tex",
        color_image.clone(),
        egui::TextureOptions::LINEAR,
    );
    let nearby = ctx.load_texture("nearby_tex", color_image, egui::TextureOptions::LINEAR);
    app.texture_cache.insert(
        3,
        stale,
        crate::loader::TextureCacheInsert {
            orig_w: 1024,
            orig_h: 768,
            needs_tile_manager: false,
            buffer_tag: crate::loader::TexturePreviewBufferTag::MainWindowSdr,
            stage: crate::loader::PreviewStage::Refined,
            current_index: 0,
            total_count: 7,
        },
    );
    app.texture_cache.insert(
        1,
        nearby,
        crate::loader::TextureCacheInsert {
            orig_w: 1024,
            orig_h: 768,
            needs_tile_manager: false,
            buffer_tag: crate::loader::TexturePreviewBufferTag::MainWindowSdr,
            stage: crate::loader::PreviewStage::Refined,
            current_index: 0,
            total_count: 7,
        },
    );

    app.evict_distant_prefetch_caches();

    assert!(!app.texture_cache.contains(3));
    assert!(app.texture_cache.contains(1));
}

#[test]
fn evict_distant_prefetch_caches_retains_outside_window_while_loading() {
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

    let ctx = egui::Context::default();
    let color_image = egui::ColorImage::from_rgba_unmultiplied([1, 1], &[0, 0, 0, 255]);
    let handle = ctx.load_texture("distant_loading", color_image, egui::TextureOptions::LINEAR);
    app.texture_cache.insert(
        5,
        handle,
        crate::loader::TextureCacheInsert {
            orig_w: 1024,
            orig_h: 768,
            needs_tile_manager: false,
            buffer_tag: crate::loader::TexturePreviewBufferTag::MainWindowSdr,
            stage: crate::loader::PreviewStage::Refined,
            current_index: 0,
            total_count: 7,
        },
    );

    // Simulate in-flight neighbor preload at idx 5 (outside window distance 2).
    app.loader.test_register_inflight(5);

    app.evict_distant_prefetch_caches();

    assert!(app.texture_cache.contains(5));
}

#[test]
fn evict_distant_prefetch_caches_removes_pixel_cache_for_distant_indices() {
    use crate::tile_cache::{PIXEL_CACHE, TileCoord};

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

    let pixels = std::sync::Arc::new(vec![0u8; 16]);
    PIXEL_CACHE
        .write()
        .insert(4, TileCoord { col: 0, row: 0 }, pixels);

    app.evict_distant_prefetch_caches();

    assert!(
        PIXEL_CACHE
            .write()
            .get(4, TileCoord { col: 0, row: 0 })
            .is_none()
    );
}

#[test]
fn flush_prefetch_neighbor_uploads_deferred_sdr_to_texture_cache() {
    let mut app = make_test_app();
    app.image_files = (0..10)
        .map(|i| std::path::PathBuf::from(format!("img{i}.jpg")))
        .collect();
    app.current_index = 2;
    app.prefetch_window_max_distance = crate::loader::DEFAULT_PREFETCH_WINDOW_DISTANCE;
    app.insert_deferred_sdr_upload(3, DecodedImage::new(1, 1, vec![1, 2, 3, 4]));

    let ctx = egui::Context::default();
    assert!(app.flush_deferred_sdr_for_completed_prefetch_neighbor(3, &ctx));
    assert!(app.texture_cache.contains(3));
    assert!(!app.deferred_sdr_uploads.contains_key(&3));
}

#[test]
fn navigate_inflight_reuse_promotes_neighbor_without_duplicate_load() {
    use crate::loader::LoadIntent;
    use eframe::egui;

    let mut app = make_test_app();
    app.image_files = (0..10)
        .map(|i| std::path::PathBuf::from(format!("img{i}.jpg")))
        .collect();
    app.current_index = 2;
    app.loader.test_register_inflight(3);

    let ctx = egui::Context::default();
    app.navigate_to(3, &ctx);

    assert_eq!(app.current_index, 3);
    assert!(app.loader.is_loading(3));
    assert_eq!(
        app.loader
            .in_flight_profile(3)
            .map(|profile| profile.load_intent),
        Some(LoadIntent::Current)
    );
}

#[test]
fn navigate_preserves_in_window_neighbor_loader_registrations() {
    use eframe::egui;

    let mut app = make_test_app();
    app.image_files = (0..10)
        .map(|i| PathBuf::from(format!("img{i}.jpg")))
        .collect();
    app.current_index = 2;
    app.loader.test_register_inflight(3);
    app.loader.test_register_inflight(4);

    let ctx = egui::Context::default();
    app.navigate_to(3, &ctx);

    assert_eq!(app.current_index, 3);
    assert!(
        app.loader.is_loading(4),
        "forward neighbor preload within retention window should survive navigation"
    );
}

#[test]
fn navigate_to_tiled_preview_without_tile_manager_triggers_load() {
    use crate::loader::RenderShape;
    use eframe::egui;

    let mut app = make_test_app();
    set_test_image_files(&mut app, &["img0.jpg", "img1.jpg"]);
    app.current_index = 0;

    let ctx = egui::Context::default();
    let color_image = egui::ColorImage::from_rgba_unmultiplied([1, 1], &[0, 0, 0, 255]);
    let handle = ctx.load_texture("test_tex", color_image, egui::TextureOptions::LINEAR);
    app.texture_cache.insert(
        1,
        handle,
        crate::loader::TextureCacheInsert {
            orig_w: 2048,
            orig_h: 1536,
            needs_tile_manager: true,
            buffer_tag: crate::loader::TexturePreviewBufferTag::TiledBootstrap,
            stage: crate::loader::PreviewStage::Initial,
            current_index: 0,
            total_count: 2,
        },
    );
    app.record_installed_display_mode(1, RenderShape::Tiled);

    assert!(app.texture_cache.contains(1));
    assert!(app.texture_cache.needs_tile_manager(1));
    assert!(app.index_uses_tiled_pipeline(1));
    assert!(app.tile_manager.is_none());
    assert!(!app.prefetched_tiles.contains_key(&1));
    assert!(!app.hdr_tiled_source_cache.contains_key(&1));

    app.navigate_to(1, &ctx);

    assert_eq!(app.current_image_res, Some((2048, 1536)));
    assert!(app.loader.is_loading(1));
}

#[test]
fn navigate_to_tiled_preview_without_display_mode_triggers_load() {
    use eframe::egui;

    let mut app = make_test_app();
    set_test_image_files(&mut app, &["img0.jpg", "img1.exr"]);
    app.current_index = 0;

    let ctx = egui::Context::default();
    let color_image = egui::ColorImage::from_rgba_unmultiplied([4, 2], &[0u8; 32]);
    let handle = ctx.load_texture("test_tex", color_image, egui::TextureOptions::LINEAR);
    app.texture_cache.insert(
        1,
        handle,
        crate::loader::TextureCacheInsert {
            orig_w: 24576,
            orig_h: 12288,
            needs_tile_manager: true,
            buffer_tag: crate::loader::TexturePreviewBufferTag::TiledBootstrap,
            stage: crate::loader::PreviewStage::Initial,
            current_index: 0,
            total_count: 2,
        },
    );

    assert!(app.texture_cache.needs_tile_manager(1));
    assert!(app.installed_display_mode(1).is_none());
    assert!(app.tile_manager.is_none());

    app.navigate_to(1, &ctx);

    assert_eq!(app.current_image_res, Some((24576, 12288)));
    assert!(app.loader.is_loading(1));
}

#[test]
fn test_resolve_initial_position_during_and_after_scan() {
    let mut app = make_test_app();
    set_test_image_files(&mut app, &["img0.jpg", "img2.jpg"]);
    let initial_path = app.image_files[1].clone();
    app.initial_image = Some(initial_path.clone());
    app.settings.resume_last_image = true;
    app.settings.last_viewed_image = Some(PathBuf::from("img1.jpg"));

    // Case 1: scanning is true (first batch)
    app.scanning = true;
    app.resolve_initial_position();
    // It should find the path in the unsorted/initial files and set current_index
    assert_eq!(app.current_index, 1);
    // But initial_image should not be consumed yet because scanning is true
    assert_eq!(app.initial_image, Some(initial_path.clone()));

    // Case 2: scanning is false (Done)
    app.scanning = false;
    app.resolve_initial_position();
    // It should still set current_index to the found path
    assert_eq!(app.current_index, 1);
    // And now initial_image should be consumed (set to None)
    assert!(app.initial_image.is_none());

    // Case 3: subsequent calls after scanning is done
    app.resolve_initial_position();
    // img1.jpg is not in the list; index stays at img2.jpg (index 1)
    assert_eq!(app.current_index, 1);
}

#[test]
fn raw_gpu_demosaic_sync_present_waits_for_bake_not_in_flight_pending() {
    let mut app = make_test_app();
    app.current_index = 0;

    app.hdr_raw_gpu_demosaic_pending_indices.insert(0);
    assert!(
        !app.raw_gpu_demosaic_needs_sync_present(),
        "in-flight bake must not enter sync-present (draw bootstrap instead)"
    );
    assert!(app.raw_gpu_demosaic_needs_repaint_wake());

    app.hdr_raw_gpu_demosaic_pending_indices.remove(&0);
    app.hdr_raw_gpu_demosaic_baked_indices.insert(0);
    assert!(app.raw_gpu_demosaic_needs_sync_present());
    assert!(app.raw_gpu_demosaic_needs_repaint_wake());

    app.hdr_raw_gpu_demosaic_baked_indices.remove(&0);
    app.raw_gpu_demosaic_await_hdr_present = true;
    assert!(app.raw_gpu_demosaic_needs_sync_present());
}

#[test]
fn cpu_raw_refinement_pending_keeps_async_repaint_wake() {
    let mut app = make_test_app();
    app.current_index = 2;

    app.cpu_raw_refinement_pending_indices.insert(2);
    assert!(app.cpu_raw_refinement_needs_repaint_wake());
    assert!(app.raw_async_work_needs_repaint_wake());

    app.cpu_raw_refinement_pending_indices.remove(&2);
    assert!(!app.cpu_raw_refinement_needs_repaint_wake());
}

#[test]
fn raw_demosaic_baked_notice_sentinel_triggers_cpu_fallback_correctly() {
    use crate::hdr::renderer::RawGpuDemosaicBakedNotice;
    use crate::hdr::types::{HdrImageBuffer, HdrPixelFormat};
    use crate::settings::RawDemosaicMode;
    use eframe::egui;
    use std::sync::Arc;

    let mut app = make_test_app();
    app.settings.raw_demosaic_mode = RawDemosaicMode::Gpu;
    app.settings.raw_high_quality = true;
    set_test_image_files(&mut app, &["sentinel_test.cr2"]);
    app.current_index = 0;

    let mut metadata = crate::raw_processor::raw_scene_linear_metadata();
    metadata.raw_gpu_source = Some(crate::hdr::types::RawGpuSource {
        raw_width: 4,
        raw_height: 4,
        width: 4,
        height: 4,
        raw_pixels: Arc::new(vec![0; 16]),
        black_level: [0.0; 4],
        cfa_scale: [1.0; 4],
        rgb_cam: [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        maximum: 65535.0,
        bayer_pattern: [0, 1, 1, 2],
        scene_color_scale: [1.0, 1.0, 1.0],
        demosaic_method: crate::settings::RawDemosaicMethod::Ppg,
        bootstrap_preview: None,
    });
    let hdr = Arc::new(HdrImageBuffer {
        width: 4,
        height: 4,
        format: HdrPixelFormat::Rgba32Float,
        color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
        metadata,
        rgba_f32: Arc::new(Vec::new()),
    });
    let image_key = crate::hdr::renderer::HdrImageKey::from_image(hdr.as_ref());
    app.hdr_image_cache.insert(0, hdr);
    app.hdr_raw_gpu_demosaic_pending_indices.insert(0);
    app.hdr_raw_gpu_demosaic_pending_key_index
        .insert(image_key, 0);
    app.raw_metadata.insert_or_update(
        0,
        crate::loader::RawOsdInfo {
            sensor_size: (4, 4),
            embedded_preview: None,
            render_pixels: crate::loader::RawRenderPixels::HqBootstrap {
                width: 2,
                height: 2,
            },
            demosaic_backend: Some(crate::loader::RawDemosaicBackend::Video),
            cpu_demosaic_ms: None,
            gpu_extract_ms: Some(1),
            gpu_demosaic_ms: None,
        },
    );
    app.raw_demosaic_baked_notify
        .lock()
        .push(RawGpuDemosaicBakedNotice {
            key: image_key,
            demosaic_ms: u32::MAX,
        });

    let ctx = egui::Context::default();
    app.tick_raw_gpu_demosaic_completion(&ctx, None);

    assert_eq!(app.settings.raw_demosaic_mode, RawDemosaicMode::Gpu);
    assert!(app.gpu_demosaic_failed_indices.contains(&0));
    assert!(!app.hdr_raw_gpu_demosaic_pending_indices.contains(&0));
    assert!(!app.hdr_image_cache.contains_key(&0));
    assert!(app.loader.is_loading(0));
    assert_eq!(app.raw_demosaic_mode_for_index(0), RawDemosaicMode::Cpu);

    use crate::loader::{LoadResult, LoaderOutput, PreviewBundle, source_key_for_path};
    let source_key = source_key_for_path(&app.image_files[0]);
    let mut decode_profile = crate::loader::decode_profile_stub();
    decode_profile.raw_high_quality = app.settings.raw_high_quality;
    decode_profile.raw_demosaic_mode = app.raw_demosaic_mode_for_index(0);
    app.loader
        .test_send_loader_output(LoaderOutput::Image(Box::new(LoadResult {
            index: 0,
            decode_profile,
            source_key,
            result: Err("synthetic cpu fallback complete".into()),
            preview_bundle: PreviewBundle::initial(),
            ultra_hdr_capacity_sensitive: false,
            sdr_fallback_is_placeholder: false,
            target_hdr_capacity: 1.0,
            raw_osd: None,
            psd_osd: None,
            uploaded_planes: None,
            staged_gpu_plane_upload: false,
            device_id: None,
        })));
    app.process_loaded_images(&ctx, &mut None);
    assert!(!app.loader.is_loading(0));
}

#[test]
fn test_process_loaded_images_with_preuploaded_planes_headless_no_panic() {
    use crate::loader::{
        DecodedImage, LoadResult, LoaderOutput, PreviewBundle, source_key_for_path,
    };

    let Some((device, queue)) = pollster::block_on(async {
        let instance = wgpu::Instance::default();
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                force_fallback_adapter: true,
                compatible_surface: None,
            })
            .await
            .ok()?;
        adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await
            .ok()
    }) else {
        log::warn!("Skipping GPU test: no adapter available");
        return;
    };

    let hdr = crate::hdr::types::HdrImageBuffer {
        width: 1,
        height: 1,
        format: crate::hdr::types::HdrPixelFormat::Rgba32Float,
        color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
        metadata: crate::hdr::types::HdrImageMetadata::from_color_space(
            crate::hdr::types::HdrColorSpace::LinearSrgb,
        ),
        rgba_f32: Arc::new(vec![0.0, 0.0, 0.0, 0.0]),
    };
    let uploaded = crate::hdr::renderer::test_upload_image_plane(&device, &queue, &hdr)
        .expect("background plane upload");

    let mut app = make_test_app();
    app.image_files = vec![std::path::PathBuf::from("preupload_test.hdr")];
    app.current_index = 0;
    app.loader.test_register_inflight(0);
    let source_key = source_key_for_path(&app.image_files[0]);
    let ctx = egui::Context::default();

    // Stale device epoch: registration is skipped and pre-uploaded planes are dropped.
    app.loader
        .test_send_loader_output(LoaderOutput::Image(Box::new(LoadResult {
            index: 0,
            decode_profile: crate::loader::decode_profile_stub(),
            source_key,
            result: Ok(crate::loader::ImageData::Hdr {
                hdr: Box::new(hdr),
                fallback: DecodedImage::new(1, 1, vec![0, 0, 0, 255]),
            }),
            preview_bundle: PreviewBundle::initial(),
            ultra_hdr_capacity_sensitive: false,
            sdr_fallback_is_placeholder: false,
            target_hdr_capacity: 1.0,
            raw_osd: None,
            psd_osd: None,
            uploaded_planes: Some(uploaded),
            staged_gpu_plane_upload: true,
            device_id: Some(999),
        })));
    app.process_loaded_images(&ctx, &mut None);
    assert!(!app.loader.is_loading(0));
    assert!(app.loader.poll().is_none());
    assert!(
        app.current_hdr_image.is_some(),
        "load result should still install after registration skip"
    );

    // Matching epoch but no frame context: registration is skipped and planes are dropped.
    let hdr = crate::hdr::types::HdrImageBuffer {
        width: 1,
        height: 1,
        format: crate::hdr::types::HdrPixelFormat::Rgba32Float,
        color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
        metadata: crate::hdr::types::HdrImageMetadata::from_color_space(
            crate::hdr::types::HdrColorSpace::LinearSrgb,
        ),
        rgba_f32: Arc::new(vec![0.0, 0.0, 0.0, 0.0]),
    };
    let uploaded = crate::hdr::renderer::test_upload_image_plane(&device, &queue, &hdr)
        .expect("background plane upload");
    app.loader.test_register_inflight(0);
    app.loader
        .test_send_loader_output(LoaderOutput::Image(Box::new(LoadResult {
            index: 0,
            decode_profile: crate::loader::decode_profile_stub(),
            source_key,
            result: Ok(crate::loader::ImageData::Hdr {
                hdr: Box::new(hdr),
                fallback: DecodedImage::new(1, 1, vec![0, 0, 0, 255]),
            }),
            preview_bundle: PreviewBundle::initial(),
            ultra_hdr_capacity_sensitive: false,
            sdr_fallback_is_placeholder: false,
            target_hdr_capacity: 1.0,
            raw_osd: None,
            psd_osd: None,
            uploaded_planes: Some(uploaded),
            staged_gpu_plane_upload: true,
            device_id: Some(app.current_device_id),
        })));
    app.process_loaded_images(&ctx, &mut None);
    assert!(!app.loader.is_loading(0));
    assert!(app.loader.poll().is_none());
    assert!(
        app.current_hdr_image.is_some(),
        "load result should still install after registration skip"
    );
}

#[test]
fn sync_loader_wgpu_context_bumps_epoch_on_device_replacement() {
    let Some((device_a, queue_a)) = pollster::block_on(async {
        let instance = wgpu::Instance::default();
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                force_fallback_adapter: true,
                compatible_surface: None,
            })
            .await
            .ok()?;
        adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await
            .ok()
    }) else {
        log::warn!("Skipping GPU test: no adapter available");
        return;
    };

    let Some((device_b, queue_b)) = pollster::block_on(async {
        let instance = wgpu::Instance::default();
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                force_fallback_adapter: true,
                compatible_surface: None,
            })
            .await
            .ok()?;
        adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await
            .ok()
    }) else {
        log::warn!("Skipping GPU test: no adapter available");
        return;
    };

    if device_a == device_b {
        log::warn!("Skipping device replacement test: adapter returned identical device handles");
        return;
    }

    let mut app = make_test_app();
    assert_eq!(app.current_device_id, 1);

    app.sync_loader_wgpu_context(device_a, queue_a);
    assert_eq!(app.current_device_id, 1);
    assert!(app.loader.wgpu_device_handle().is_some());

    app.sync_loader_wgpu_context(device_b, queue_b);
    assert_eq!(app.current_device_id, 2);
}

fn test_gpu_raw_pending_hdr(raw_pixels: Arc<Vec<u16>>) -> Arc<crate::hdr::types::HdrImageBuffer> {
    let mut metadata = crate::raw_processor::raw_scene_linear_metadata();
    metadata.raw_gpu_source = Some(crate::hdr::types::RawGpuSource {
        raw_width: 4,
        raw_height: 4,
        width: 4,
        height: 4,
        raw_pixels,
        black_level: [0.0; 4],
        cfa_scale: [1.0; 4],
        rgb_cam: [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        maximum: 65535.0,
        bayer_pattern: [0, 1, 1, 2],
        scene_color_scale: [1.0, 1.0, 1.0],
        demosaic_method: crate::settings::RawDemosaicMethod::Ppg,
        bootstrap_preview: None,
    });
    Arc::new(crate::hdr::types::HdrImageBuffer {
        width: 4,
        height: 4,
        format: crate::hdr::types::HdrPixelFormat::Rgba32Float,
        color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
        metadata,
        rgba_f32: Arc::new(Vec::new()),
    })
}

#[test]
fn resolve_raw_demosaic_success_notice_matches_via_side_map_after_pending_cleared() {
    use crate::hdr::renderer::RawGpuDemosaicBakedNotice;
    use loader_results::resolve_raw_demosaic_notice_indices;

    let raw_pixels = Arc::new(vec![0u16; 16]);
    let hdr = test_gpu_raw_pending_hdr(Arc::clone(&raw_pixels));
    let image_key = crate::hdr::renderer::HdrImageKey::from_image(hdr.as_ref());
    let hdr_cache = HashMap::from([(278_usize, hdr)]);
    let side_map = HashMap::from([(image_key, 278_usize)]);
    let notice = RawGpuDemosaicBakedNotice {
        key: image_key,
        demosaic_ms: 0,
    };

    let matched = resolve_raw_demosaic_notice_indices(
        &notice,
        false,
        &hdr_cache,
        &HashSet::new(),
        &side_map,
        None,
    );
    assert_eq!(matched, vec![278]);
}

#[test]
fn resolve_raw_demosaic_notice_indices_matches_via_pending_key_side_map() {
    use crate::hdr::renderer::RawGpuDemosaicBakedNotice;
    use loader_results::resolve_raw_demosaic_notice_indices;

    let raw_pixels = Arc::new(vec![0u16; 16]);
    let hdr = test_gpu_raw_pending_hdr(Arc::clone(&raw_pixels));
    let image_key = crate::hdr::renderer::HdrImageKey::from_image(hdr.as_ref());
    let pending = HashSet::from([2usize]);
    let side_map = HashMap::from([(image_key, 2usize)]);
    let notice = RawGpuDemosaicBakedNotice {
        key: image_key,
        demosaic_ms: u32::MAX,
    };

    let matched = resolve_raw_demosaic_notice_indices(
        &notice,
        true,
        &HashMap::new(),
        &pending,
        &side_map,
        None,
    );
    assert_eq!(matched, vec![2]);
}

#[test]
fn resolve_raw_demosaic_notice_indices_single_pending_fallback_requires_key_match() {
    use crate::hdr::renderer::RawGpuDemosaicBakedNotice;
    use loader_results::resolve_raw_demosaic_notice_indices;

    let unmatched_hdr = test_gpu_raw_pending_hdr(Arc::new(vec![1u16; 16]));
    let unmatched_key = crate::hdr::renderer::HdrImageKey::from_image(unmatched_hdr.as_ref());
    let pending = HashSet::from([7usize]);
    let notice = RawGpuDemosaicBakedNotice {
        key: unmatched_key,
        demosaic_ms: u32::MAX,
    };

    let matched = resolve_raw_demosaic_notice_indices(
        &notice,
        true,
        &HashMap::new(),
        &pending,
        &HashMap::new(),
        None,
    );
    assert!(matched.is_empty());

    let matched_hdr = test_gpu_raw_pending_hdr(Arc::new(vec![2u16; 16]));
    let matched_key = crate::hdr::renderer::HdrImageKey::from_image(matched_hdr.as_ref());
    let hdr_cache = HashMap::from([(7, matched_hdr)]);
    let notice = RawGpuDemosaicBakedNotice {
        key: matched_key,
        demosaic_ms: 12,
    };
    let matched = resolve_raw_demosaic_notice_indices(
        &notice,
        false,
        &hdr_cache,
        &pending,
        &HashMap::new(),
        None,
    );
    assert_eq!(matched, vec![7]);
}

#[test]
fn resolve_raw_demosaic_notice_indices_returns_empty_when_unmatched() {
    use crate::hdr::renderer::RawGpuDemosaicBakedNotice;
    use loader_results::resolve_raw_demosaic_notice_indices;

    let hdr_a = test_gpu_raw_pending_hdr(Arc::new(vec![2u16; 16]));
    let hdr_b = test_gpu_raw_pending_hdr(Arc::new(vec![3u16; 16]));
    let unmatched_hdr = test_gpu_raw_pending_hdr(Arc::new(vec![4u16; 16]));
    let unmatched_key = crate::hdr::renderer::HdrImageKey::from_image(unmatched_hdr.as_ref());
    let pending = HashSet::from([0usize, 1usize]);
    let hdr_cache = HashMap::from([(0, hdr_a), (1, hdr_b)]);
    let notice = RawGpuDemosaicBakedNotice {
        key: unmatched_key,
        demosaic_ms: u32::MAX,
    };

    let matched = resolve_raw_demosaic_notice_indices(
        &notice,
        true,
        &hdr_cache,
        &pending,
        &HashMap::new(),
        None,
    );
    assert!(matched.is_empty());
}

#[test]
fn raw_hdr_plane_ready_releases_embedded_bootstrap_not_fallback_slot() {
    let mut app = make_test_app();
    let mut metadata = crate::raw_processor::raw_scene_linear_metadata();
    metadata.raw_gpu_source = Some(crate::hdr::types::RawGpuSource {
        raw_width: 2,
        raw_height: 2,
        width: 2,
        height: 2,
        raw_pixels: Arc::new(vec![0; 4]),
        black_level: [0.0; 4],
        cfa_scale: [1.0; 4],
        rgb_cam: [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        maximum: 1.0,
        bayer_pattern: [0, 1, 1, 2],
        scene_color_scale: [1.0, 1.0, 1.0],
        demosaic_method: crate::settings::RawDemosaicMethod::Ppg,
        bootstrap_preview: Some(crate::loader::DecodedImage::new(1, 1, vec![1, 2, 3, 255])),
    });
    let hdr = Arc::new(crate::hdr::types::HdrImageBuffer {
        width: 2,
        height: 2,
        format: crate::hdr::types::HdrPixelFormat::Rgba32Float,
        color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
        metadata,
        rgba_f32: Arc::new(Vec::new()),
    });
    app.hdr_image_cache.insert(0, Arc::clone(&hdr));
    app.hdr_sdr_fallback_indices.insert(0);
    app.raw_gpu_embedded_bootstrap_indices.insert(0);
    let ctx = egui::Context::default();
    app.upload_raw_gpu_bootstrap_texture(
        0,
        &crate::loader::DecodedImage::new(1, 1, vec![9, 9, 9, 255]),
        &ctx,
    );
    assert!(app.texture_cache.contains(0));
    assert!(crate::loader::raw_gpu_source_has_bootstrap_preview(
        app.hdr_image_cache.get(&0).unwrap()
    ));

    app.on_raw_hdr_plane_ready(0);

    assert!(!app.raw_gpu_embedded_bootstrap_indices.contains(&0));
    assert!(!app.texture_cache.contains(0));
    assert!(app.hdr_sdr_fallback_indices.contains(&0));
    assert!(!crate::loader::raw_gpu_source_has_bootstrap_preview(
        app.hdr_image_cache.get(&0).unwrap()
    ));
}

#[test]
fn raw_hdr_plane_ready_releases_fallback_texture_keeps_fallback_slot() {
    let mut app = make_test_app();
    let mut metadata = crate::raw_processor::raw_scene_linear_metadata();
    metadata.raw_gpu_source = Some(crate::hdr::types::RawGpuSource {
        raw_width: 2,
        raw_height: 2,
        width: 2,
        height: 2,
        raw_pixels: Arc::new(vec![0; 4]),
        black_level: [0.0; 4],
        cfa_scale: [1.0; 4],
        rgb_cam: [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        maximum: 1.0,
        bayer_pattern: [0, 1, 1, 2],
        scene_color_scale: [1.0, 1.0, 1.0],
        demosaic_method: crate::settings::RawDemosaicMethod::Ppg,
        bootstrap_preview: Some(crate::loader::DecodedImage::new(1, 1, vec![1, 2, 3, 255])),
    });
    let hdr = Arc::new(crate::hdr::types::HdrImageBuffer {
        width: 2,
        height: 2,
        format: crate::hdr::types::HdrPixelFormat::Rgba32Float,
        color_space: crate::hdr::types::HdrColorSpace::LinearSrgb,
        metadata,
        rgba_f32: Arc::new(Vec::new()),
    });
    app.hdr_image_cache.insert(0, Arc::clone(&hdr));
    app.hdr_sdr_fallback_indices.insert(0);
    let ctx = egui::Context::default();
    app.upload_hdr_sdr_fallback_texture(
        0,
        &crate::loader::DecodedImage::new(1, 1, vec![9, 9, 9, 255]),
        &ctx,
    );
    assert!(app.texture_cache.contains(0));
    assert!(!app.raw_gpu_embedded_bootstrap_indices.contains(&0));

    app.on_raw_hdr_plane_ready(0);

    assert!(!app.texture_cache.contains(0));
    assert!(app.hdr_sdr_fallback_indices.contains(&0));
    assert!(!crate::loader::raw_gpu_source_has_bootstrap_preview(
        app.hdr_image_cache.get(&0).unwrap()
    ));
}

#[test]
fn apply_picked_image_directory_keeps_tree_settings_when_nav_hidden() {
    let mut app = make_test_app();
    app.settings.browse_mode = crate::settings::BrowseMode::Tree;
    app.settings.show_directory_tree_nav = false;
    app.settings.tree_nav_selected_dir = Some(PathBuf::from("/tree/root/old"));

    let picked = PathBuf::from("/tree/root/new");
    app.apply_picked_image_directory(picked.clone());

    assert_eq!(app.settings.browse_mode, crate::settings::BrowseMode::Tree);
    assert_eq!(app.settings.tree_nav_selected_dir, Some(picked.clone()));
    assert_eq!(app.settings.last_image_dir, Some(picked));
}

#[test]
fn ipc_double_click_updates_saved_gallery_directory_by_default() {
    let ctx = egui::Context::default();
    let mut app = make_test_app();
    let saved = std::env::temp_dir().join("siv_saved_gallery_default");
    let opened = std::env::temp_dir().join("siv_opened_gallery_default");
    std::fs::create_dir_all(&saved).unwrap();
    std::fs::create_dir_all(&opened).unwrap();
    let image = opened.join("opened.jpg");

    app.settings.last_image_dir = Some(saved);

    app.handle_ipc_open_image(image.clone(), &ctx, true);

    assert_eq!(app.current_browse_directory(), Some(opened.clone()));
    assert_eq!(app.settings.last_image_dir, Some(opened));
    assert_eq!(app.initial_image, Some(image));
    assert!(!app.settings.recursive);
}

#[test]
fn ipc_double_click_can_keep_saved_gallery_directory() {
    let ctx = egui::Context::default();
    let mut app = make_test_app();
    let saved = std::env::temp_dir().join("siv_saved_gallery_kept");
    let opened = std::env::temp_dir().join("siv_opened_gallery_kept");
    std::fs::create_dir_all(&saved).unwrap();
    std::fs::create_dir_all(&opened).unwrap();
    let image = opened.join("opened.jpg");

    app.settings.last_image_dir = Some(saved.clone());
    app.settings.keep_gallery_dir_on_double_click = true;

    app.handle_ipc_open_image(image.clone(), &ctx, true);

    assert_eq!(app.current_browse_directory(), Some(opened));
    assert_eq!(app.settings.last_image_dir, Some(saved));
    assert_eq!(app.initial_image, Some(image));
    assert!(!app.settings.recursive);
}

#[test]
fn ipc_double_click_transient_gallery_queues_persistent_setting_save() {
    let ctx = egui::Context::default();
    let mut app = make_test_app();
    let (save_tx, save_rx) = crossbeam_channel::unbounded();
    app.save_tx = save_tx;
    let saved = std::env::temp_dir().join("siv_saved_gallery_save_queued");
    let opened = std::env::temp_dir().join("siv_opened_gallery_save_queued");
    std::fs::create_dir_all(&saved).unwrap();
    std::fs::create_dir_all(&opened).unwrap();
    let image = opened.join("opened.jpg");

    app.settings.last_image_dir = Some(saved.clone());
    app.settings.keep_gallery_dir_on_double_click = true;
    app.settings.recursive = true;
    app.settings.auto_switch = true;

    app.handle_ipc_open_image(image, &ctx, true);

    let queued = save_rx.try_iter().last().expect("settings save queued");
    assert_eq!(queued.last_image_dir, Some(saved));
    assert!(!queued.recursive);
    assert!(!queued.auto_switch);
}

#[test]
fn embedded_panel_bootstrap_retries_until_embedded_nav_active() {
    use crate::settings::DirectoryTreeNavStyle;
    use eframe::egui::{self, Pos2, Rect};

    let ctx = egui::Context::default();
    let mut app = make_test_app();
    app.embedded_directory_tree_panel_bootstrapped = false;
    app.settings.browse_mode = crate::settings::BrowseMode::Tree;
    app.settings.show_directory_tree_nav = true;
    app.settings.directory_tree_nav_style = DirectoryTreeNavStyle::Embedded;
    app.auto_hidden_directory_tree_nav = true;

    let available = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1000.0, 800.0));
    app.bootstrap_embedded_directory_tree_panel_layout(&ctx, available);
    assert!(
        !app.embedded_directory_tree_panel_bootstrapped,
        "session auto-hide keeps nav inactive; bootstrap must not latch yet"
    );

    app.clear_auto_hidden_directory_tree_nav();
    app.bootstrap_embedded_directory_tree_panel_layout(&ctx, available);
    assert!(
        app.embedded_directory_tree_panel_bootstrapped,
        "once embedded nav is active, bootstrap seeds panel state once"
    );
}

#[test]
fn image_list_double_click_hides_tree_nav_without_persisting_toggle() {
    let ctx = egui::Context::default();
    let mut app = make_test_app();
    let (save_tx, save_rx) = crossbeam_channel::unbounded();
    app.save_tx = save_tx;
    let dir = std::env::temp_dir().join("siv_list_double_click_nav");
    std::fs::create_dir_all(&dir).unwrap();
    let first = dir.join("first.jpg");
    let second = dir.join("second.jpg");
    write_min_sized_test_image(&first);
    write_min_sized_test_image(&second);

    app.settings.browse_mode = crate::settings::BrowseMode::Tree;
    app.settings.show_directory_tree_nav = true;
    app.image_files = vec![first, second];
    app.file_byte_len_by_index = vec![
        crate::constants::MIN_IMAGE_FILE_BYTES,
        crate::constants::MIN_IMAGE_FILE_BYTES,
    ];
    app.file_modified_unix_by_index = vec![None, None];

    {
        let mut list = app.directory_tree.list.lock();
        list.scanning = false;
        list.image_list_reordering = false;
        list.image_rows = app
            .image_files
            .iter()
            .enumerate()
            .map(|(index, path)| {
                crate::app::directory_tree::DirectoryTreeFileRow::new(
                    path.clone(),
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("image")
                        .to_string(),
                    app.file_byte_len_by_index[index],
                    app.file_modified_unix_by_index[index],
                )
            })
            .collect();
    }

    app.directory_tree
        .command_tx
        .send(crate::app::directory_tree::DirectoryTreeCommand::SelectImageAndHideNav(1))
        .unwrap();
    app.process_directory_tree_events(&ctx);

    assert!(!app.directory_tree_settings_active());
    assert!(app.auto_hidden_directory_tree_nav);
    assert!(app.settings.show_directory_tree_nav);
    assert!(save_rx.try_iter().next().is_none());
}

#[test]
fn ipc_double_click_auto_hides_tree_nav_without_persisting_toggle() {
    let ctx = egui::Context::default();
    let mut app = make_test_app();
    let (save_tx, save_rx) = crossbeam_channel::unbounded();
    app.save_tx = save_tx;
    let saved = std::env::temp_dir().join("siv_tree_saved_gallery");
    let opened = std::env::temp_dir().join("siv_tree_opened_gallery");
    std::fs::create_dir_all(&saved).unwrap();
    std::fs::create_dir_all(&opened).unwrap();
    let image = opened.join("opened.jpg");

    app.settings.browse_mode = crate::settings::BrowseMode::Tree;
    app.settings.show_directory_tree_nav = true;
    app.settings.last_image_dir = Some(saved);

    app.handle_ipc_open_image(image, &ctx, true);

    assert!(!app.directory_tree_settings_active());
    assert_eq!(app.settings.browse_mode, crate::settings::BrowseMode::Tree);
    assert!(app.settings.show_directory_tree_nav);
    let queued = save_rx.try_iter().last().expect("settings save queued");
    assert_eq!(queued.browse_mode, crate::settings::BrowseMode::Tree);
    assert!(queued.show_directory_tree_nav);
}

#[test]
fn ipc_double_click_in_current_directory_disables_recursive_scan() {
    let ctx = egui::Context::default();
    let mut app = make_test_app();
    let opened = std::env::temp_dir().join("siv_opened_gallery_same_dir");
    std::fs::create_dir_all(&opened).unwrap();
    let first = opened.join("first.jpg");
    let second = opened.join("second.jpg");

    app.settings.last_image_dir = Some(opened);
    app.settings.recursive = true;
    app.image_files = vec![first, second.clone()];
    app.file_byte_len_by_index = vec![0; app.image_files.len()];
    app.file_modified_unix_by_index = vec![None; app.image_files.len()];

    app.handle_ipc_open_image(second, &ctx, true);

    assert_eq!(app.current_index, 1);
    assert!(!app.settings.recursive);
}

#[test]
fn picked_directory_replaces_transient_double_click_directory() {
    let mut app = make_test_app();
    let saved = std::env::temp_dir().join("siv_saved_gallery_before_pick");
    let transient = std::env::temp_dir().join("siv_transient_gallery_before_pick");
    let picked = std::env::temp_dir().join("siv_picked_gallery_after_transient");
    std::fs::create_dir_all(&saved).unwrap();
    std::fs::create_dir_all(&transient).unwrap();
    std::fs::create_dir_all(&picked).unwrap();

    app.settings.last_image_dir = Some(saved);
    app.load_directory_for_transient_gallery(transient);

    app.apply_picked_image_directory(picked.clone());

    assert_eq!(app.current_browse_directory(), Some(picked.clone()));
    assert_eq!(app.settings.last_image_dir, Some(picked));
    assert!(app.settings.transient_image_dir.is_none());
}

#[test]
fn reloading_current_transient_directory_keeps_saved_gallery_directory() {
    let mut app = make_test_app();
    let saved = std::env::temp_dir().join("siv_saved_gallery_before_reload");
    let transient = std::env::temp_dir().join("siv_transient_gallery_reload");
    std::fs::create_dir_all(&saved).unwrap();
    std::fs::create_dir_all(&transient).unwrap();

    app.settings.last_image_dir = Some(saved.clone());
    app.load_directory_for_transient_gallery(transient.clone());

    app.reload_current_browse_directory(transient.clone());

    assert_eq!(app.current_browse_directory(), Some(transient.clone()));
    assert_eq!(app.settings.transient_image_dir, Some(transient));
    assert_eq!(app.settings.last_image_dir, Some(saved));
}

#[test]
fn picked_directory_with_tree_nav_replaces_transient_double_click_directory() {
    let mut app = make_test_app();
    let saved = std::env::temp_dir().join("siv_saved_gallery_before_tree_pick");
    let transient = std::env::temp_dir().join("siv_transient_gallery_before_tree_pick");
    let picked = std::env::temp_dir().join("siv_tree_picked_gallery_after_transient");
    std::fs::create_dir_all(&saved).unwrap();
    std::fs::create_dir_all(&transient).unwrap();
    std::fs::create_dir_all(&picked).unwrap();

    app.settings.last_image_dir = Some(saved);
    app.load_directory_for_transient_gallery(transient);
    app.settings.show_directory_tree_nav = true;

    app.apply_picked_image_directory(picked.clone());

    assert_eq!(app.current_browse_directory(), Some(picked.clone()));
    assert_eq!(app.settings.tree_nav_selected_dir, Some(picked.clone()));
    assert_eq!(app.settings.last_image_dir, Some(picked));
    assert!(app.settings.transient_image_dir.is_none());
}

#[test]
fn reorder_directory_tree_strip_after_image_list_change_permutes_by_path() {
    let ctx = egui::Context::default();
    let mut app = make_test_app();
    let paths: Vec<PathBuf> = (0..3)
        .map(|i| PathBuf::from(format!(r"C:\photos\img{i}.jpg")))
        .collect();
    let old_files = paths.clone();
    let new_files = vec![paths[2].clone(), paths[0].clone(), paths[1].clone()];

    for (index, _) in paths.iter().enumerate() {
        let fill = ((index + 1) * 40) as u8;
        let decoded = crate::loader::DecodedImage::new(8, 8, vec![fill; 8 * 8 * 4]);
        app.directory_tree_strip_cache.upsert_from_decoded(
            index,
            &decoded,
            crate::app::directory_tree_strip_cache::StripDecodedUpsert {
                stage: crate::loader::PreviewStage::Refined,
                buffer_tag:
                    crate::app::directory_tree_strip_cache::StripPreviewBufferTag::StripDecodedPixels,
                logical_size: None,
                path: &paths[index],
                ctx: &ctx,
                strip_max_side: 128,
                strip_max_side_used: Some(128),
            },
        );
    }

    app.reorder_directory_tree_strip_after_image_list_change(&old_files, &new_files);

    assert!(app.directory_tree_strip_cache.contains(0));
    assert!(app.directory_tree_strip_cache.contains(1));
    assert!(app.directory_tree_strip_cache.contains(2));
}

#[test]
fn reorder_directory_tree_strip_after_image_list_change_invalidates_on_count_change() {
    let ctx = egui::Context::default();
    let mut app = make_test_app();
    let old_files: Vec<PathBuf> = (0..2)
        .map(|i| PathBuf::from(format!(r"C:\photos\img{i}.jpg")))
        .collect();
    let new_files = vec![
        old_files[0].clone(),
        old_files[1].clone(),
        PathBuf::from(r"C:\photos\img2.jpg"),
    ];

    let decoded = crate::loader::DecodedImage::new(8, 8, vec![128; 8 * 8 * 4]);
    app.directory_tree_strip_cache.upsert_from_decoded(
        0,
        &decoded,
        crate::app::directory_tree_strip_cache::StripDecodedUpsert {
            stage: crate::loader::PreviewStage::Refined,
            buffer_tag:
                crate::app::directory_tree_strip_cache::StripPreviewBufferTag::StripDecodedPixels,
            logical_size: None,
            path: &old_files[0],
            ctx: &ctx,
            strip_max_side: 128,
            strip_max_side_used: Some(128),
        },
    );

    app.reorder_directory_tree_strip_after_image_list_change(&old_files, &new_files);

    assert!(!app.directory_tree_strip_cache.contains(0));
}

fn write_strip_test_png(name: &str) -> PathBuf {
    use image::ImageEncoder;

    let path = std::env::temp_dir().join(format!(
        "siv_strip_full_decode_{name}_{}.png",
        std::process::id()
    ));
    let mut encoded = Vec::new();
    image::codecs::png::PngEncoder::new(&mut encoded)
        .write_image(&[255, 0, 0, 255], 1, 1, image::ColorType::Rgba8.into())
        .expect("encode test png");
    std::fs::write(&path, encoded).expect("write test png");
    path
}

fn png_chunk_crc(chunk_type: &[u8; 4], data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in chunk_type.iter().chain(data.iter()) {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

fn insert_apng_actl_chunk(mut png: Vec<u8>) -> Vec<u8> {
    let ihdr_len = u32::from_be_bytes(png[8..12].try_into().expect("ihdr length")) as usize;
    let insert_at = 8 + 4 + 4 + ihdr_len + 4;
    let chunk_type = *b"acTL";
    let mut data = Vec::new();
    data.extend_from_slice(&1u32.to_be_bytes());
    data.extend_from_slice(&0u32.to_be_bytes());
    let crc = png_chunk_crc(&chunk_type, &data);

    let mut chunk = Vec::new();
    chunk.extend_from_slice(&(data.len() as u32).to_be_bytes());
    chunk.extend_from_slice(&chunk_type);
    chunk.extend_from_slice(&data);
    chunk.extend_from_slice(&crc.to_be_bytes());
    png.splice(insert_at..insert_at, chunk);
    png
}

#[test]
fn strip_static_full_decode_inflight_blocks_main_preload_in_ring() {
    let mut app = make_test_app();
    let neighbor = write_strip_test_png("static_neighbor");
    app.image_files = vec![
        PathBuf::from("current.png"),
        neighbor.clone(),
        PathBuf::from("outside.bmp"),
        PathBuf::from("backward_neighbor.png"),
    ];
    app.set_current_index(0);
    app.settings.preload = true;
    app.prefetch_window_max_distance = 1;
    app.directory_tree_strip_generate_inflight.insert(1);
    app.directory_tree_strip_generate_inflight.insert(2);
    assert!(app.strip_path_provides_reusable_static_full_decode(&app.image_files[1]));
    if app.strip_path_provides_reusable_static_full_decode(&app.image_files[1]) {
        app.directory_tree_strip_static_full_decode_inflight
            .insert(1);
    }
    if app.strip_path_provides_reusable_static_full_decode(&app.image_files[2]) {
        app.directory_tree_strip_static_full_decode_inflight
            .insert(2);
    }

    assert!(app.strip_full_decode_inflight_should_block_main_load(1));
    assert!(!app.strip_full_decode_inflight_should_block_main_load(2));
    let _ = std::fs::remove_file(neighbor);
}

#[test]
fn strip_animated_png_inflight_does_not_block_main_preload() {
    let mut app = make_test_app();
    let animated = write_strip_test_png("animated_neighbor");
    let png = std::fs::read(&animated).expect("read test png");
    std::fs::write(&animated, insert_apng_actl_chunk(png)).expect("write test apng");
    app.image_files = vec![PathBuf::from("current.png"), animated.clone()];
    app.set_current_index(0);
    app.settings.preload = true;
    app.prefetch_window_max_distance = 1;
    app.directory_tree_strip_generate_inflight.insert(1);

    assert!(!app.strip_path_provides_reusable_static_full_decode(&app.image_files[1]));
    assert!(!app.strip_full_decode_inflight_should_block_main_load(1));
    let _ = std::fs::remove_file(animated);
}

#[test]
fn install_sdr_animated_image_queues_strip_preview_when_main_texture_oversized() {
    use crate::settings::BrowseMode;

    let ctx = egui::Context::default();
    let mut app = make_test_app();
    app.settings.browse_mode = BrowseMode::Tree;
    app.settings.show_directory_tree_nav = true;
    app.settings.directory_tree_show_list_previews = true;
    // Match the animation_spline JXL case: 320x320 main texture vs 256 strip max.
    app.settings.directory_tree_list_preview_size =
        crate::settings::DirectoryTreeListPreviewSize::Large;
    set_test_image_files(&mut app, &["input.jxl", "ref.apng"]);
    app.current_index = 0;
    app.directory_tree_strip_cold_awaiting_main_loader.insert(0);
    app.directory_tree_strip_cold_attempted.insert(0);

    let w = 320u32;
    let h = 320u32;
    let frame = crate::loader::AnimationFrame::new(
        w,
        h,
        vec![40; (w * h * 4) as usize],
        std::time::Duration::from_millis(40),
    );
    app.install_animated_image(0, &[frame], &ctx);

    assert!(
        app.texture_cache.contains(0),
        "main-window SDR texture should be installed for current animated frame"
    );
    let strip_work_queued = app.directory_tree_strip_cache.contains(0)
        || app.directory_tree_strip_generate_inflight.contains(&0)
        || app
            .directory_tree_strip_pending_main_handoff
            .contains_key(&0)
        || app
            .directory_tree_strip_pending_gpu_initial
            .iter()
            .any(|u| u.key.index == 0)
        || app
            .directory_tree_strip_pending_gpu_refined
            .iter()
            .any(|u| u.key.index == 0);
    assert!(
        strip_work_queued,
        "SDR animated install must hand first-frame pixels to strip (cache, resample, or GPU queue)"
    );
}

#[test]
fn strip_cold_does_not_defer_static_full_decode_after_lru_when_main_texture_present() {
    use crate::settings::BrowseMode;

    // Repro: large directory, strip LRU-evicts a preloaded PNG while main texture_cache
    // still holds the full-res SDR. Cold must self-decode instead of awaiting a handoff
    // that will never be re-installed.
    let ctx = egui::Context::default();
    let mut app = make_test_app();
    app.settings.browse_mode = BrowseMode::Tree;
    app.settings.show_directory_tree_nav = true;
    app.settings.directory_tree_show_list_previews = true;
    app.settings.preload = true;
    app.settings.directory_tree_list_preview_size =
        crate::settings::DirectoryTreeListPreviewSize::Large;
    let keep = write_strip_test_png("keep_lru");
    let target = write_strip_test_png("target_lru");
    app.image_files = vec![keep.clone(), target.clone()];
    app.file_byte_len_by_index = vec![1024, 1024];
    app.current_index = 0;
    app.prefetch_window_max_distance = 2;

    // Oversized vs strip_max_side (256) but within egui test max texture side (2048).
    let w = 720u32;
    let h = 1280u32;
    let color_image = egui::ColorImage::from_rgba_unmultiplied(
        [w as usize, h as usize],
        &vec![80u8; (w * h * 4) as usize],
    );
    let handle = ctx.load_texture("main_oversized", color_image, egui::TextureOptions::LINEAR);
    app.texture_cache.insert(
        1,
        handle,
        crate::loader::TextureCacheInsert {
            orig_w: w,
            orig_h: h,
            needs_tile_manager: false,
            buffer_tag: crate::loader::TexturePreviewBufferTag::MainWindowSdr,
            stage: crate::loader::PreviewStage::Refined,
            current_index: 0,
            total_count: 2,
        },
    );
    // Successful strip then GPU clear keeps logical_sizes (same as LRU eviction).
    let strip_px = DecodedImage::new(144, 256, vec![90; 144 * 256 * 4]);
    app.directory_tree_strip_cache.upsert_from_decoded(
        1,
        &strip_px,
        crate::app::directory_tree_strip_cache::StripDecodedUpsert {
            stage: crate::loader::PreviewStage::Refined,
            buffer_tag:
                crate::app::directory_tree_strip_cache::StripPreviewBufferTag::StripDecodedPixels,
            logical_size: Some((w, h)),
            path: &app.image_files[1],
            ctx: &ctx,
            strip_max_side: 256,
            strip_max_side_used: Some(256),
        },
    );
    app.directory_tree_strip_cache.clear_gpu_textures();
    assert!(!app.directory_tree_strip_cache.contains(1));
    assert_eq!(
        app.directory_tree_strip_cache.logical_sizes().get(&1),
        Some(&(w, h))
    );
    app.directory_tree_strip_cold_attempted.insert(1);
    app.directory_tree_strip_cold_awaiting_main_loader.insert(1);

    assert!(
        app.strip_cold_static_full_decode_can_share_with_main(1, &app.image_files[1]),
        "PNG shares static full decode with main loader"
    );
    assert!(
        !app.strip_cold_skip_slow_static_full_decode_primary(1, true),
        "after strip LRU eviction, cold must not skip static full decode while awaiting a dead handoff"
    );

    app.release_strip_cold_awaiting_main_loader_if_resolved(1);
    assert!(
        !app.directory_tree_strip_cold_awaiting_main_loader
            .contains(&1),
        "awaiting_main_loader must clear when main SDR is ready but strip handoff is gone"
    );
    assert!(
        app.strip_index_needs_cold_thumbnail(1),
        "visible strip must be eligible for cold regeneration after release"
    );
    let _ = std::fs::remove_file(keep);
    let _ = std::fs::remove_file(target);
}

#[test]
fn strip_jpeg_fast_path_inflight_does_not_block_main_preload() {
    let mut app = make_test_app();
    app.image_files = vec![PathBuf::from("current.png"), PathBuf::from("neighbor.jpg")];
    app.set_current_index(0);
    app.settings.preload = true;
    app.prefetch_window_max_distance = 1;
    app.directory_tree_strip_generate_inflight.insert(1);

    assert!(!app.strip_full_decode_inflight_should_block_main_load(1));
}

#[test]
fn strip_hdr_fallback_inflight_does_not_block_main_preload() {
    let mut app = make_test_app();
    app.image_files = vec![
        PathBuf::from("current.png"),
        PathBuf::from("neighbor.hdr"),
        PathBuf::from("neighbor.exr"),
    ];
    app.set_current_index(0);
    app.settings.preload = true;
    app.prefetch_window_max_distance = 2;
    app.directory_tree_strip_generate_inflight.insert(1);
    app.directory_tree_strip_generate_inflight.insert(2);

    assert!(!app.strip_path_provides_reusable_static_full_decode(&app.image_files[1]));
    assert!(!app.strip_path_provides_reusable_static_full_decode(&app.image_files[2]));
    assert!(!app.strip_full_decode_inflight_should_block_main_load(1));
    assert!(!app.strip_full_decode_inflight_should_block_main_load(2));
}
