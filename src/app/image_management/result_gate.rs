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
    RenderShape, SourceKey, TileResult, profile_satisfies_display, source_key_for_path,
};

use super::hdr_load_result_capacity_is_stale;
use super::prefetch_retention::{PrefetchCacheRetention, prefetch_cache_retention};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateDecision {
    Accept,
    Discard,
    Requeue,
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

pub(super) fn source_key_matches_index(
    image_files: &[PathBuf],
    index: usize,
    source_key: SourceKey,
) -> bool {
    image_files
        .get(index)
        .is_some_and(|path| source_key_for_path(path) == source_key)
}

/// Install-time hard check: decoded shape vs an explicit required shape (never `Unknown`).
pub fn render_shape_matches_install(image_data: &ImageData, required: RenderShape) -> bool {
    if required == RenderShape::Unknown {
        return false;
    }
    image_data.preferred_render_shape() == required
}

fn render_shape_acceptable_at_install(
    load_result: &LoadResult,
    display: &DisplayRequirements,
) -> bool {
    let Ok(data) = &load_result.result else {
        return true;
    };
    let actual = data.preferred_render_shape();
    if display.render_shape != RenderShape::Unknown
        && !render_shape_matches_install(data, display.render_shape)
    {
        return false;
    }
    if load_result.decode_profile.render_shape != RenderShape::Unknown
        && actual != load_result.decode_profile.render_shape
    {
        return false;
    }
    true
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
    // ③ source_key identity
    if !source_key_matches_index(image_files, idx, load_result.source_key) {
        return GateDecision::Discard;
    }
    // ① preload window (+ in-flight retention while registered)
    if !ctx.retention_for(idx, is_loading).should_retain() {
        return GateDecision::Discard;
    }
    // ② decode/display profile
    if !profile_satisfies_display(&load_result.decode_profile, display) {
        return GateDecision::Discard;
    }
    if !render_shape_acceptable_at_install(load_result, display) {
        return GateDecision::Discard;
    }
    // RAW HQ bootstrap without an HDR plane cannot satisfy the current viewer.
    if load_result.decode_profile.raw_high_quality
        && load_result.raw_osd.is_some()
        && !load_result
            .result
            .as_ref()
            .is_ok_and(|d| d.has_plane(crate::loader::PixelPlaneKind::Hdr))
    {
        return GateDecision::Discard;
    }
    // device_id mismatch for pre-uploaded planes is handled in
    // `try_register_preuploaded_hdr_plane` (planes dropped, decode result still installs).
    if hdr_load_result_capacity_is_stale(load_result, display.ultra_hdr_decode_capacity) {
        return GateDecision::Requeue;
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
    existing_stage: Option<PreviewStage>,
) -> GateDecision {
    let idx = preview.index;
    if !source_key_matches_index(image_files, idx, preview.source_key) {
        return GateDecision::Discard;
    }
    if !profile_satisfies_display(&preview.decode_profile, display) {
        return GateDecision::Discard;
    }
    let incoming = preview.preview_bundle.stage();
    if let Some(existing) = existing_stage
        && !preview_stage_should_upgrade(existing, incoming)
    {
        return GateDecision::Discard;
    }
    // Refined HQ previews are expensive to regenerate for large tiled sources (PSB/EXR).
    // Keep them even when the index falls outside the prefetch retention window.
    if incoming == PreviewStage::Refined {
        return GateDecision::Accept;
    }
    if !ctx.retention_for(idx, is_loading).should_retain() {
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
    // Tile workers emit epoch-only stubs; binding is (index, profile_epoch) per generation-plan.
    if tile.decode_profile.profile_epoch != tm_profile.profile_epoch {
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
        (_, PreviewStage::Refined) | (PreviewStage::Initial, PreviewStage::Initial)
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
    use crate::loader::LoadIntent;
    use crate::settings::RawDemosaicMode;

    fn sample_profile(intent: LoadIntent) -> DecodeProfile {
        DecodeProfile {
            raw_high_quality: false,
            psd_hidden_layer_heuristic: false,
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
            psd_hidden_layer_heuristic: false,
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
        let mut files = Vec::new();
        for i in 0..10 {
            files.push(PathBuf::from(format!("img{i}.jpg")));
        }
        let key = source_key_for_path(&files[5]);
        let load = LoadResult {
            index: 5,
            decode_profile: profile,
            source_key: key,
            result: Ok(crate::loader::ImageData::Static(
                crate::loader::DecodedImage::new(1, 1, vec![0]),
            )),
            preview_bundle: crate::loader::PreviewBundle::initial(),
            ultra_hdr_capacity_sensitive: false,
            sdr_fallback_is_placeholder: false,
            target_hdr_capacity: 1.0,
            raw_osd: None,
            psd_osd: None,
            uploaded_planes: None,
            staged_gpu_plane_upload: false,
            device_id: None,
        };
        assert_eq!(
            gate_load_result(&ctx, &load, &files, &display, false),
            GateDecision::Accept
        );
    }

    #[test]
    fn rejects_hq_decode_profile_after_display_downgrade() {
        let ctx = ResultGateContext {
            current_index: 0,
            image_count: 1,
            max_distance: 2,
        };
        let files = vec![PathBuf::from("img0.cr2")];
        let hq_profile = DecodeProfile {
            raw_high_quality: true,
            ..sample_profile(LoadIntent::Current)
        };
        let sdr_display = DisplayRequirements {
            raw_high_quality: false,
            ..sample_display(LoadIntent::Current)
        };
        let load = LoadResult {
            index: 0,
            decode_profile: hq_profile,
            source_key: source_key_for_path(&files[0]),
            result: Ok(crate::loader::ImageData::Static(
                crate::loader::DecodedImage::new(1, 1, vec![0]),
            )),
            preview_bundle: crate::loader::PreviewBundle::initial(),
            ultra_hdr_capacity_sensitive: false,
            sdr_fallback_is_placeholder: false,
            target_hdr_capacity: 1.0,
            raw_osd: None,
            psd_osd: None,
            uploaded_planes: None,
            staged_gpu_plane_upload: false,
            device_id: None,
        };
        assert_eq!(
            gate_load_result(&ctx, &load, &files, &sdr_display, false),
            GateDecision::Discard
        );
    }

    #[test]
    fn accepts_load_with_stale_preupload_device_id() {
        let ctx = ResultGateContext {
            current_index: 0,
            image_count: 1,
            max_distance: 2,
        };
        let files = vec![PathBuf::from("img0.hdr")];
        let display = DisplayRequirements {
            device_id: Some(1),
            ..sample_display(LoadIntent::Current)
        };
        let load = LoadResult {
            index: 0,
            decode_profile: sample_profile(LoadIntent::Current),
            source_key: source_key_for_path(&files[0]),
            result: Ok(crate::loader::ImageData::Static(
                crate::loader::DecodedImage::new(1, 1, vec![0]),
            )),
            preview_bundle: crate::loader::PreviewBundle::initial(),
            ultra_hdr_capacity_sensitive: false,
            sdr_fallback_is_placeholder: false,
            target_hdr_capacity: 1.0,
            raw_osd: None,
            psd_osd: None,
            uploaded_planes: None,
            staged_gpu_plane_upload: false,
            device_id: Some(999),
        };
        assert_eq!(
            gate_load_result(&ctx, &load, &files, &display, false),
            GateDecision::Accept
        );
    }

    #[test]
    fn rejects_initial_preview_when_refined_already_installed() {
        let ctx = ResultGateContext {
            current_index: 0,
            image_count: 1,
            max_distance: 2,
        };
        let files = vec![PathBuf::from("img0.jpg")];
        let preview = PreviewResult {
            index: 0,
            decode_profile: sample_profile(LoadIntent::Current),
            source_key: source_key_for_path(&files[0]),
            preview_bundle: crate::loader::PreviewBundle::initial(),
            raw_bootstrap_osd: None,
            sdr_texture_tag: None,
            cpu_demosaic_ms: None,
            error: None,
        };
        let display = sample_display(LoadIntent::Current);
        assert_eq!(
            gate_preview_result(
                &ctx,
                &preview,
                &files,
                &display,
                false,
                Some(PreviewStage::Refined)
            ),
            GateDecision::Discard
        );
    }

    #[test]
    fn accepts_refined_preview_outside_prefetch_window() {
        let ctx = ResultGateContext {
            current_index: 37,
            image_count: 100,
            max_distance: 2,
        };
        let mut files = Vec::new();
        for i in 0..100 {
            files.push(PathBuf::from(format!("img{i}.psb")));
        }
        let preview = PreviewResult {
            index: 40,
            decode_profile: sample_profile(LoadIntent::NeighborPrefetch),
            source_key: source_key_for_path(&files[40]),
            preview_bundle: crate::loader::PreviewBundle::refined(),
            raw_bootstrap_osd: None,
            sdr_texture_tag: None,
            cpu_demosaic_ms: None,
            error: None,
        };
        let display = sample_display(LoadIntent::Current);
        assert_eq!(
            gate_preview_result(
                &ctx,
                &preview,
                &files,
                &display,
                false,
                Some(PreviewStage::Initial)
            ),
            GateDecision::Accept
        );
    }

    #[test]
    fn accepts_tile_result_when_worker_profile_is_epoch_stub() {
        let ctx = ResultGateContext {
            current_index: 0,
            image_count: 1,
            max_distance: 2,
        };
        let files = vec![PathBuf::from("img0.exr")];
        let tm_profile = DecodeProfile {
            raw_high_quality: true,
            psd_hidden_layer_heuristic: false,
            raw_demosaic_mode: RawDemosaicMode::Gpu,
            output_mode: crate::hdr::types::HdrOutputMode::WindowsScRgb,
            ultra_hdr_decode_capacity: 2.0,
            render_shape: RenderShape::Tiled,
            load_intent: LoadIntent::Current,
            profile_epoch: 7,
        };
        let tile = TileResult {
            index: 0,
            decode_profile: crate::loader::decode_profile_with_epoch(7),
            col: 0,
            row: 0,
            pixel_kind: crate::loader::TilePixelKind::Hdr,
        };
        assert_eq!(
            gate_tile_result(
                &ctx,
                &tile,
                0,
                &tm_profile,
                &files,
                source_key_for_path(&files[0]),
                false,
            ),
            GateDecision::Accept
        );
    }

    #[test]
    fn discards_tile_result_when_profile_epoch_mismatch() {
        let ctx = ResultGateContext {
            current_index: 0,
            image_count: 1,
            max_distance: 2,
        };
        let files = vec![PathBuf::from("img0.exr")];
        let tm_profile = sample_profile(LoadIntent::Current);
        let tile = TileResult {
            index: 0,
            decode_profile: crate::loader::decode_profile_with_epoch(99),
            col: 0,
            row: 0,
            pixel_kind: crate::loader::TilePixelKind::Sdr,
        };
        assert_eq!(
            gate_tile_result(
                &ctx,
                &tile,
                0,
                &tm_profile,
                &files,
                source_key_for_path(&files[0]),
                false,
            ),
            GateDecision::Discard
        );
    }

    #[test]
    fn discards_initial_preview_outside_prefetch_window() {
        let ctx = ResultGateContext {
            current_index: 37,
            image_count: 100,
            max_distance: 2,
        };
        let mut files = Vec::new();
        for i in 0..100 {
            files.push(PathBuf::from(format!("img{i}.psb")));
        }
        let preview = PreviewResult {
            index: 40,
            decode_profile: sample_profile(LoadIntent::NeighborPrefetch),
            source_key: source_key_for_path(&files[40]),
            preview_bundle: crate::loader::PreviewBundle::initial(),
            raw_bootstrap_osd: None,
            sdr_texture_tag: None,
            cpu_demosaic_ms: None,
            error: None,
        };
        let display = sample_display(LoadIntent::Current);
        assert_eq!(
            gate_preview_result(&ctx, &preview, &files, &display, false, None),
            GateDecision::Discard
        );
    }
}
