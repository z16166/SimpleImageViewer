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

//! Decode / display profile snapshots for loader spawn and result acceptance (generation-plan Phase A).

use crate::hdr::types::HdrOutputMode;
use crate::settings::RawDemosaicMode;

use super::RenderShape;

pub const HDR_CAPACITY_MATCH_EPSILON: f32 = 0.001;
/// Default preload radius until the main thread publishes navigation.
pub const DEFAULT_PREFETCH_WINDOW_DISTANCE: usize = 2;
/// Cap img-loader rayon pool size and neighbor prefetch concurrency.
pub const MAX_IMG_LOADER_THREADS: usize = 3;
/// Cap dedicated OS threads for [`LoadIntent::Current`] (fast navigation otherwise spawns unboundedly).
pub const MAX_CURRENT_IMAGE_OS_THREADS: usize = 2;

/// Whether a load was requested for the current image or a neighbor prefetch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LoadIntent {
    Current,
    NeighborPrefetch,
}

/// Snapshot taken when spawning a decode / refine worker.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodeProfile {
    pub raw_high_quality: bool,
    pub raw_demosaic_mode: RawDemosaicMode,
    pub output_mode: HdrOutputMode,
    pub ultra_hdr_decode_capacity: f32,
    pub render_shape: RenderShape,
    pub load_intent: LoadIntent,
    /// Bumped on decode-profile invalidation; workers early-exit when request epoch lags snapshot.
    pub profile_epoch: u64,
}

/// Placeholder profile for tests and tile-manager bootstrap before a real load is registered.
/// `profile_epoch: 0` matches a live epoch-0 snapshot; callers that need staleness isolation
/// should use [`decode_profile_with_epoch`].
pub fn decode_profile_stub() -> DecodeProfile {
    DecodeProfile {
        raw_high_quality: false,
        raw_demosaic_mode: RawDemosaicMode::Gpu,
        output_mode: HdrOutputMode::SdrToneMapped,
        ultra_hdr_decode_capacity: 1.0,
        render_shape: RenderShape::Unknown,
        load_intent: LoadIntent::NeighborPrefetch,
        profile_epoch: 0,
    }
}

pub fn decode_profile_with_epoch(epoch: u64) -> DecodeProfile {
    DecodeProfile {
        profile_epoch: epoch,
        ..decode_profile_stub()
    }
}

/// Runtime display requirements assembled on the main thread at install / poll.
/// `animation_playback` (generation-plan §3.F) is intentionally omitted: playback speed /
/// pause affects display refresh only and does not change decode profile acceptance.
#[derive(Debug, Clone, PartialEq)]
pub struct DisplayRequirements {
    pub raw_high_quality: bool,
    pub raw_demosaic_mode: RawDemosaicMode,
    pub output_mode: HdrOutputMode,
    pub ultra_hdr_decode_capacity: f32,
    pub render_shape: RenderShape,
    pub load_intent: LoadIntent,
    pub device_id: Option<u64>,
}

/// Registered in-flight load for an index (replaces generation-only `loading[idx]`).
#[derive(Debug, Clone, PartialEq)]
pub struct InFlightLoad {
    pub profile: DecodeProfile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileSpawnRelation {
    Equal,
    Upgrade,
    Downgrade,
}

pub fn output_mode_is_hdr(mode: HdrOutputMode) -> bool {
    mode != HdrOutputMode::SdrToneMapped
}

pub fn profile_spawn_relation(
    registered: &DecodeProfile,
    requested: &DecodeProfile,
) -> ProfileSpawnRelation {
    if registered == requested {
        return ProfileSpawnRelation::Equal;
    }
    if profile_is_upgrade(registered, requested) {
        ProfileSpawnRelation::Upgrade
    } else {
        ProfileSpawnRelation::Downgrade
    }
}

/// True when an in-flight registration no longer matches a finished load worker profile.
pub fn in_flight_profile_supersedes_load_result(
    spawn_profile: &DecodeProfile,
    in_flight: &DecodeProfile,
) -> bool {
    matches!(
        profile_spawn_relation(spawn_profile, in_flight),
        ProfileSpawnRelation::Downgrade | ProfileSpawnRelation::Upgrade
    )
}

/// True when an in-flight registration is a strict upgrade over a finished HQ refinement worker.
pub fn in_flight_profile_supersedes_hq_refinement(
    adoptee_profile: &DecodeProfile,
    in_flight: &DecodeProfile,
) -> bool {
    profile_spawn_relation(adoptee_profile, in_flight) == ProfileSpawnRelation::Upgrade
}

fn profile_is_upgrade(old: &DecodeProfile, new: &DecodeProfile) -> bool {
    if new.raw_high_quality && !old.raw_high_quality {
        return true;
    }
    if output_mode_is_hdr(new.output_mode) && !output_mode_is_hdr(old.output_mode) {
        return true;
    }
    if new.ultra_hdr_decode_capacity > old.ultra_hdr_decode_capacity + HDR_CAPACITY_MATCH_EPSILON {
        return true;
    }
    // Demosaic partial order: Cpu -> Gpu is Upgrade; Gpu -> Cpu is Downgrade.
    // GPU demosaic runs on the render thread with near-zero loader-thread cost; CPU demosaic
    // consumes the loader thread pool, so switching back from Gpu to Cpu is a throughput
    // regression, not a capability gain.
    if new.raw_demosaic_mode != old.raw_demosaic_mode {
        return matches!(
            (old.raw_demosaic_mode, new.raw_demosaic_mode),
            (RawDemosaicMode::Cpu, RawDemosaicMode::Gpu)
        );
    }
    if new.load_intent == LoadIntent::Current && old.load_intent == LoadIntent::NeighborPrefetch {
        return true;
    }
    if new.profile_epoch > old.profile_epoch && profile_decode_capabilities_equal(old, new) {
        return true;
    }
    false
}

fn profile_decode_capabilities_equal(old: &DecodeProfile, new: &DecodeProfile) -> bool {
    old.raw_high_quality == new.raw_high_quality
        && old.raw_demosaic_mode == new.raw_demosaic_mode
        && old.output_mode == new.output_mode
        && (old.ultra_hdr_decode_capacity - new.ultra_hdr_decode_capacity).abs()
            <= HDR_CAPACITY_MATCH_EPSILON
        && old.render_shape == new.render_shape
}

/// Core profile fields that must match for install-time acceptance.
/// `render_shape` is checked separately at install (§3.C); `load_intent` may differ when a
/// neighbor prefetch result becomes the current image.
pub fn profile_core_matches(result: &DecodeProfile, display: &DisplayRequirements) -> bool {
    result.raw_high_quality == display.raw_high_quality
        && result.raw_demosaic_mode == display.raw_demosaic_mode
        && result.output_mode == display.output_mode
        && (result.ultra_hdr_decode_capacity - display.ultra_hdr_decode_capacity).abs()
            <= HDR_CAPACITY_MATCH_EPSILON
}

/// Result profile satisfies what the viewer currently needs (neighbor prefetch may upgrade to current).
pub fn profile_satisfies_display(result: &DecodeProfile, display: &DisplayRequirements) -> bool {
    if !profile_core_matches(result, display) {
        return false;
    }
    if display.render_shape != RenderShape::Unknown
        && result.render_shape != RenderShape::Unknown
        && result.render_shape != display.render_shape
    {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_profile() -> DecodeProfile {
        DecodeProfile {
            raw_high_quality: false,
            raw_demosaic_mode: RawDemosaicMode::Gpu,
            output_mode: HdrOutputMode::SdrToneMapped,
            ultra_hdr_decode_capacity: 1.0,
            render_shape: RenderShape::Unknown,
            load_intent: LoadIntent::NeighborPrefetch,
            profile_epoch: 0,
        }
    }

    #[test]
    fn equal_profiles_do_not_spawn_again() {
        let a = base_profile();
        let b = a.clone();
        assert_eq!(profile_spawn_relation(&a, &b), ProfileSpawnRelation::Equal);
    }

    #[test]
    fn raw_hq_toggle_is_upgrade() {
        let old = base_profile();
        let new = DecodeProfile {
            raw_high_quality: true,
            ..base_profile()
        };
        assert_eq!(
            profile_spawn_relation(&old, &new),
            ProfileSpawnRelation::Upgrade
        );
    }

    #[test]
    fn neighbor_to_current_intent_is_upgrade() {
        let old = base_profile();
        let new = DecodeProfile {
            load_intent: LoadIntent::Current,
            ..base_profile()
        };
        assert_eq!(
            profile_spawn_relation(&old, &new),
            ProfileSpawnRelation::Upgrade
        );
    }

    #[test]
    fn gpu_to_cpu_demosaic_is_downgrade() {
        let old = base_profile();
        let new = DecodeProfile {
            raw_demosaic_mode: RawDemosaicMode::Cpu,
            ..base_profile()
        };
        assert_eq!(
            profile_spawn_relation(&old, &new),
            ProfileSpawnRelation::Downgrade
        );
    }

    #[test]
    fn cpu_to_gpu_demosaic_is_upgrade() {
        let old = DecodeProfile {
            raw_demosaic_mode: RawDemosaicMode::Cpu,
            ..base_profile()
        };
        let new = base_profile();
        assert_eq!(
            profile_spawn_relation(&old, &new),
            ProfileSpawnRelation::Upgrade
        );
    }

    #[test]
    fn hq_downgrade_with_epoch_bump_is_downgrade() {
        let old = DecodeProfile {
            raw_high_quality: true,
            profile_epoch: 0,
            ..base_profile()
        };
        let new = DecodeProfile {
            raw_high_quality: false,
            profile_epoch: 1,
            ..base_profile()
        };
        assert_eq!(
            profile_spawn_relation(&old, &new),
            ProfileSpawnRelation::Downgrade
        );
    }

    #[test]
    fn equal_in_flight_profile_does_not_supersede_load_result() {
        let profile = base_profile();
        assert!(!in_flight_profile_supersedes_load_result(
            &profile, &profile
        ));
    }

    #[test]
    fn hq_downgrade_with_epoch_bump_supersedes_load_result() {
        let worker = DecodeProfile {
            raw_high_quality: true,
            profile_epoch: 0,
            ..base_profile()
        };
        let in_flight = DecodeProfile {
            raw_high_quality: false,
            profile_epoch: 1,
            ..base_profile()
        };
        assert!(in_flight_profile_supersedes_load_result(
            &worker, &in_flight
        ));
    }

    #[test]
    fn epoch_only_bump_supersedes_via_spawn_relation_not_or_branch() {
        let worker = base_profile();
        let mut in_flight = base_profile();
        in_flight.profile_epoch = worker.profile_epoch + 1;
        assert_eq!(
            profile_spawn_relation(&worker, &in_flight),
            ProfileSpawnRelation::Upgrade
        );
        assert!(in_flight_profile_supersedes_load_result(
            &worker, &in_flight
        ));
        assert!(in_flight_profile_supersedes_hq_refinement(
            &worker, &in_flight
        ));
    }

    #[test]
    fn lower_epoch_in_flight_does_not_supersede_hq_refinement() {
        let worker = DecodeProfile {
            profile_epoch: 5,
            ..base_profile()
        };
        let in_flight = DecodeProfile {
            profile_epoch: 3,
            ..base_profile()
        };
        assert!(!in_flight_profile_supersedes_hq_refinement(
            &worker, &in_flight
        ));
    }

    #[test]
    fn profile_core_ignores_load_intent_for_display_match() {
        let result = DecodeProfile {
            load_intent: LoadIntent::NeighborPrefetch,
            ..base_profile()
        };
        let display = DisplayRequirements {
            raw_high_quality: result.raw_high_quality,
            raw_demosaic_mode: result.raw_demosaic_mode,
            output_mode: result.output_mode,
            ultra_hdr_decode_capacity: result.ultra_hdr_decode_capacity,
            render_shape: RenderShape::Unknown,
            load_intent: LoadIntent::Current,
            device_id: None,
        };
        assert!(profile_satisfies_display(&result, &display));
    }
}
