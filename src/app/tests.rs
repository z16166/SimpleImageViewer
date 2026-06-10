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
use crate::hdr::types::{HdrColorSpace, HdrImageBuffer, HdrImageMetadata, HdrPixelFormat};
use std::collections::HashSet;
use std::sync::Arc;

#[test]
fn current_hdr_image_only_matches_its_source_index() {
    let image = Arc::new(HdrImageBuffer {
        width: 1,
        height: 1,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::LinearSrgb,
        metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
        rgba_f32: Arc::new(vec![1.0, 1.0, 1.0, 1.0]),
    });
    let current = CurrentHdrImage::new(7, Arc::clone(&image));

    assert!(current.image_for_index(6).is_none());
    assert!(Arc::ptr_eq(current.image_for_index(7).unwrap(), &image));
}

#[test]
fn current_hdr_tiled_image_only_matches_its_source_index() {
    let image = HdrImageBuffer {
        width: 1,
        height: 1,
        format: HdrPixelFormat::Rgba32Float,
        color_space: HdrColorSpace::LinearSrgb,
        metadata: HdrImageMetadata::from_color_space(HdrColorSpace::LinearSrgb),
        rgba_f32: Arc::new(vec![1.0, 1.0, 1.0, 1.0]),
    };
    let source: Arc<dyn crate::hdr::tiled::HdrTiledSource> = Arc::new(
        crate::hdr::tiled::HdrTiledImageSource::new(image).expect("valid HDR tiled source"),
    );
    let current = CurrentHdrTiledImage::new(7, Arc::clone(&source));

    assert!(current.source_for_index(6).is_none());
    assert!(Arc::ptr_eq(current.source_for_index(7).unwrap(), &source));
}

#[test]
fn hardware_tier_scales_hdr_tile_cache_budget() {
    assert_eq!(HardwareTier::Low.hdr_tile_cache_mb(), 256);
    assert_eq!(HardwareTier::Medium.hdr_tile_cache_mb(), 512);
    assert_eq!(HardwareTier::High.hdr_tile_cache_mb(), 1024);
}

#[test]
fn memory_aware_tile_cache_budgets_keep_tier_defaults_when_memory_is_available() {
    assert_eq!(
        memory_aware_tile_cache_budgets_mb(HardwareTier::High, 16 * 1024),
        (2048, 1024)
    );
}

#[test]
fn memory_aware_tile_cache_budgets_shrink_when_available_memory_is_low() {
    let (cpu_mb, hdr_mb) = memory_aware_tile_cache_budgets_mb(HardwareTier::High, 2048);

    assert!(cpu_mb < HardwareTier::High.cpu_cache_mb());
    assert!(hdr_mb < HardwareTier::High.hdr_tile_cache_mb());
    assert!(cpu_mb + hdr_mb <= 512);
    assert!(cpu_mb >= 256);
    assert!(hdr_mb >= 256);
}

#[test]
fn sdr_output_mode_uses_sdr_ultra_hdr_decode_capacity() {
    let settings = crate::hdr::types::HdrToneMapSettings {
        exposure_ev: 0.0,
        sdr_white_nits: 200.0,
        max_display_nits: 1000.0,
    };

    assert_eq!(
        ultra_hdr_decode_capacity_for_output_mode(
            settings,
            crate::hdr::types::HdrOutputMode::SdrToneMapped,
            None
        ),
        1.0
    );
    assert_eq!(
        ultra_hdr_decode_capacity_for_output_mode(
            settings,
            crate::hdr::types::HdrOutputMode::WindowsScRgb,
            None
        ),
        5.0
    );
}

#[test]
fn native_output_uses_monitor_peak_luminance_for_ultra_hdr_capacity() {
    let settings = crate::hdr::types::HdrToneMapSettings {
        exposure_ev: 0.0,
        sdr_white_nits: 200.0,
        max_display_nits: 1000.0,
    };
    let monitor = crate::hdr::monitor::HdrMonitorSelection {
        hdr_supported: true,
        label: "HDR".to_string(),
        max_luminance_nits: Some(1200.0),
        max_full_frame_luminance_nits: Some(600.0),
        max_hdr_capacity: None,
        hdr_capacity_source: Some("Windows DXGI MaxLuminance"),
        native_surface_encoding: Some(crate::hdr::monitor::HdrNativeSurfaceEncoding::LinearScRgb),
    };

    assert_eq!(
        ultra_hdr_decode_capacity_for_output_mode(
            settings,
            crate::hdr::types::HdrOutputMode::WindowsScRgb,
            Some(&monitor)
        ),
        6.0
    );
}

#[test]
fn hdr_osd_state_change_tracks_native_output_fields() {
    let sdr = crate::app::HdrOutputStateSnapshot::new(
        crate::hdr::types::HdrOutputMode::SdrToneMapped,
        false,
        Some(wgpu::TextureFormat::Bgra8Unorm),
    );
    let hdr = crate::app::HdrOutputStateSnapshot::new(
        crate::hdr::types::HdrOutputMode::WindowsScRgb,
        true,
        Some(wgpu::TextureFormat::Rgba16Float),
    );

    assert!(crate::app::hdr_output_state_changed(sdr, hdr));
    assert!(!crate::app::hdr_output_state_changed(hdr, hdr));
}

#[test]
fn native_output_uses_monitor_hdr_capacity_multiplier_before_peak_nits() {
    let settings = crate::hdr::types::HdrToneMapSettings {
        exposure_ev: 0.0,
        sdr_white_nits: 200.0,
        max_display_nits: 1000.0,
    };
    let monitor = crate::hdr::monitor::HdrMonitorSelection {
        hdr_supported: true,
        label: "macOS EDR".to_string(),
        max_luminance_nits: Some(1200.0),
        max_full_frame_luminance_nits: None,
        max_hdr_capacity: Some(2.5),
        hdr_capacity_source: Some("macOS maximumExtendedDynamicRangeColorComponentValue"),
        native_surface_encoding: Some(crate::hdr::monitor::HdrNativeSurfaceEncoding::LinearScRgb),
    };

    assert_eq!(
        ultra_hdr_decode_capacity_for_output_mode(
            settings,
            crate::hdr::types::HdrOutputMode::MacOsEdr,
            Some(&monitor)
        ),
        2.5
    );
}

#[test]
fn capacity_refresh_targets_all_hdr_cache_indices() {
    let static_hdr = HashSet::from([1_usize, 4]);
    let hdr_tiled = HashSet::from([2_usize, 4]);
    let hdr_fallback = HashSet::from([3_usize, 4]);

    assert_eq!(
        collect_ultra_hdr_capacity_sensitive_indices(&static_hdr, &hdr_tiled, &hdr_fallback),
        vec![1, 2, 3, 4]
    );
}

#[test]
fn capacity_refresh_reloads_current_when_current_is_hdr() {
    let static_hdr = HashSet::from([7_usize]);
    let hdr_tiled = HashSet::new();
    let hdr_fallback = HashSet::new();
    let ultra_hdr = HashSet::from([7_usize]);

    let refresh =
        plan_ultra_hdr_capacity_refresh(7, &static_hdr, &hdr_tiled, &hdr_fallback, &ultra_hdr);

    assert_eq!(refresh.indices_to_invalidate, vec![7]);
    assert!(refresh.reload_current);
    assert!(capacity_refresh_should_reschedule_preloads(&refresh));
}

#[test]
fn capacity_refresh_ignores_non_ultra_hdr_caches() {
    let static_hdr = HashSet::from([7_usize]);
    let hdr_tiled = HashSet::from([8_usize]);
    let hdr_fallback = HashSet::from([9_usize]);
    let ultra_hdr = HashSet::new();

    let refresh =
        plan_ultra_hdr_capacity_refresh(7, &static_hdr, &hdr_tiled, &hdr_fallback, &ultra_hdr);

    assert!(refresh.indices_to_invalidate.is_empty());
    assert!(!refresh.reload_current);
    assert!(!capacity_refresh_should_reschedule_preloads(&refresh));
}

#[test]
fn hotkey_issue_message_reports_load_errors() {
    let message = build_hotkeys_issue_message(Some("bad yaml"), &[], &[])
        .expect("load error should be user-visible");
    assert!(message.contains("bad yaml"));
}

#[test]
fn hotkey_issue_message_reports_conflicts_and_warnings() {
    let conflicts = vec![crate::hotkeys::model::HotkeyConflict {
        key: "D".to_string(),
        actions: vec![
            crate::hotkeys::model::HotkeyActionId::NextImage,
            crate::hotkeys::model::HotkeyActionId::PrevImage,
        ],
    }];
    let warnings = vec![crate::hotkeys::model::HotkeyWarning::InvalidKey {
        action_id: crate::hotkeys::model::HotkeyActionId::NextImage,
        key: "Foo".to_string(),
    }];
    let message = build_hotkeys_issue_message(None, &conflicts, &warnings)
        .expect("validation issues should be user-visible");
    assert!(message.contains("D"));
    assert!(message.contains("Foo"));
}

#[test]
fn empty_capacity_refresh_does_not_reschedule_preloads() {
    let refresh = UltraHdrCapacityRefresh {
        indices_to_invalidate: Vec::new(),
        reload_current: false,
    };

    assert!(!capacity_refresh_should_reschedule_preloads(&refresh));
}
