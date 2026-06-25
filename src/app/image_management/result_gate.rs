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

//! Unified accept/discard gate for loader outputs (generation-plan Phase A).

use std::path::PathBuf;

use crate::loader::{
    DecodeProfile, DisplayRequirements, ImageData, LoadResult, PreviewResult, PreviewStage,
    SourceKey, TileResult, profile_satisfies_display, source_key_for_path,
};

use super::prefetch_retention::{PrefetchCacheRetention, prefetch_cache_retention};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateDecision {
    Accept,
    Discard,
    Requeue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateDiscardReason {
    SourceKeyMismatch,
    OutsidePreloadWindow,
    ProfileMismatch,
    StaleRefinedNotification,
    StaleTileBinding,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateAcceptReason {
    CurrentIndex,
    WithinPreloadWindow,
    InFlightLoad,
    ProfileMatch,
}

/// Context for window-based retention shared with prefetch cache eviction.
pub struct ResultGateContext {
    pub current_index: usize,
    pub image_count: usize,
    pub max_distance: usize,
}

impl ResultGateContext {
    pub fn retention_for(&self, idx: usize, is_loading: bool) -> PrefetchCacheRetention {
        prefetch_cache_retention(
            self.current_index,
            self.image_count,
            self.max_distance,
            idx,
            is_loading,
        )
    }
}

fn source_key_matches_index(
    image_files: &[PathBuf],
    index: usize,
    source_key: SourceKey,
) -> bool {
    image_files
        .get(index)
        .is_some_and(|path| source_key_for_path(path) == source_key)
}

/// Install-time hard check: decoded shape vs viewer requirement.
pub fn render_shape_matches_install(
    image_data: &ImageData,
    required: crate::loader::RenderShape,
) -> bool {
    if required == crate::loader::RenderShape::Unknown {
        return true;
    }
    image_data.preferred_render_shape() == required
}

/// Gate for full `LoadResult` before GPU upload / cache install.
pub fn gate_load_result(
    ctx: &ResultGateContext,
    load_result: &LoadResult,
    image_files: &[PathBuf],
    display: &DisplayRequirements,
    is_loading: bool,
) -> GateDecision {
    let idx = load_result.index;
    if !source_key_matches_index(image_files, idx, load_result.source_key) {
        return GateDecision::Discard;
    }
    if !ctx.retention_for(idx, is_loading).should_retain() {
        return GateDecision::Discard;
    }
    if !profile_satisfies_display(&load_result.decode_profile, display) {
        return GateDecision::Discard;
    }
    if display.render_shape != crate::loader::RenderShape::Unknown {
        if let Ok(data) = &load_result.result {
            if !render_shape_matches_install(data, display.render_shape) {
                return GateDecision::Discard;
            }
        }
    }
    if load_result.decode_profile.raw_high_quality
        && load_result.raw_osd.is_some()
        && !load_result
            .result
            .as_ref()
            .is_ok_and(|d| d.has_plane(crate::loader::PixelPlaneKind::Hdr))
    {
        return GateDecision::Discard;
    }
    GateDecision::Accept
}

/// Gate for HQ preview updates (replaces generation tolerance).
pub fn gate_preview_result(
    ctx: &ResultGateContext,
    preview: &PreviewResult,
    image_files: &[PathBuf],
    display: &DisplayRequirements,
    is_loading: bool,
) -> GateDecision {
    let idx = preview.index;
    if !source_key_matches_index(image_files, idx, preview.source_key) {
        return GateDecision::Discard;
    }
    if !ctx.retention_for(idx, is_loading).should_retain() {
        return GateDecision::Discard;
    }
    if !profile_satisfies_display(&preview.decode_profile, display) {
        return GateDecision::Discard;
    }
    GateDecision::Accept
}

/// Gate for tile decode completion (pending marker clear).
pub fn gate_tile_result(
    ctx: &ResultGateContext,
    tile: &TileResult,
    tm_index: usize,
    tm_profile: &DecodeProfile,
    image_files: &[PathBuf],
    source_key: SourceKey,
    is_loading: bool,
) -> GateDecision {
    if tile.index != tm_index {
        return GateDecision::Discard;
    }
    if !source_key_matches_index(image_files, tile.index, source_key) {
        return GateDecision::Discard;
    }
    if tile.decode_profile != *tm_profile {
        return GateDecision::Discard;
    }
    if !ctx.retention_for(tile.index, is_loading).should_retain() {
        return GateDecision::Discard;
    }
    GateDecision::Accept
}

/// Whether an incoming preview stage should replace an existing pyramid preview.
pub fn preview_stage_should_upgrade(existing: PreviewStage, incoming: PreviewStage) -> bool {
    matches!(
        (existing, incoming),
        (PreviewStage::Initial, PreviewStage::Refined)
    )
}

#[cfg(feature = "preload-debug")]
pub fn gate_decision_log_label(decision: GateDecision) -> &'static str {
    match decision {
        GateDecision::Accept => "accept",
        GateDecision::Discard => "discard",
        GateDecision::Requeue => "requeue",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::{LoadIntent, RenderShape};
    use crate::settings::RawDemosaicMode;

    fn sample_profile(intent: LoadIntent) -> DecodeProfile {
        DecodeProfile {
            raw_high_quality: false,
            raw_demosaic_mode: RawDemosaicMode::Gpu,
            output_mode: crate::hdr::types::HdrOutputMode::SdrToneMapped,
            ultra_hdr_decode_capacity: 1.0,
            render_shape: RenderShape::Unknown,
            load_intent: intent,
            profile_epoch: 0,
        }
    }

    fn sample_display(intent: LoadIntent) -> DisplayRequirements {
        DisplayRequirements {
            raw_high_quality: false,
            raw_demosaic_mode: RawDemosaicMode::Gpu,
            output_mode: crate::hdr::types::HdrOutputMode::SdrToneMapped,
            ultra_hdr_decode_capacity: 1.0,
            render_shape: RenderShape::Unknown,
            load_intent: intent,
            device_id: None,
        }
    }

    #[test]
    fn accepts_neighbor_prefetch_profile_when_user_navigates_to_index() {
        let ctx = ResultGateContext {
            current_index: 5,
            image_count: 100,
            max_distance: 2,
        };
        let profile = sample_profile(LoadIntent::NeighborPrefetch);
        let display = sample_display(LoadIntent::Current);
        let load = LoadResult {
            index: 5,
            decode_profile: profile,
            source_key: 42,
            result: Ok(crate::loader::ImageData::Static(
                crate::loader::DecodedImage::new(1, 1, vec![0]),
            )),
            preview_bundle: crate::loader::PreviewBundle::initial(),
            ultra_hdr_capacity_sensitive: false,
            sdr_fallback_is_placeholder: false,
            target_hdr_capacity: 1.0,
            raw_osd: None,
            uploaded_planes: None,
            device_id: None,
        };
        let files = vec![PathBuf::from("a.jpg")];
        // Extend files for index 5 - use dummy paths
        let mut files = Vec::new();
        for i in 0..10 {
            files.push(PathBuf::from(format!("img{i}.jpg")));
        }
        // Fix source key
        let key = source_key_for_path(&files[5]);
        let load = LoadResult {
            source_key: key,
            ..load
        };
        assert_eq!(
            gate_load_result(&ctx, &load, &files, &display, false),
            GateDecision::Accept
        );
    }
}
