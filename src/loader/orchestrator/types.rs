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

//! Worker pool, deferred loads, refinement channels, tile queue orchestration ([`ImageLoader`]).

use crate::hdr::types::HdrToneMapSettings;
use crate::loader::{
    DecodeProfile, InFlightLoad, LoaderOutput, ProfileSpawnRelation, RefinementRequest,
    TileDecodeSource, TilePixelKind, profile_spawn_relation,
};
use crossbeam_channel::{Receiver, Sender};
use parking_lot::{Condvar, Mutex};

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64};

use super::preload_plan::PreloadPlanSnapshot;

/// Crossbeam sender that wakes the root window when a decode worker posts a result.
#[derive(Clone)]
pub(crate) struct LoaderOutputSender {
    inner: Sender<LoaderOutput>,
    root_wake: Arc<parking_lot::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>>,
}

impl LoaderOutputSender {
    pub(crate) fn new(inner: Sender<LoaderOutput>) -> Self {
        Self {
            inner,
            root_wake: Arc::new(parking_lot::Mutex::new(None)),
        }
    }

    pub(crate) fn set_root_wake(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        *self.root_wake.lock() = Some(wake);
    }

    pub(crate) fn send(
        &self,
        output: LoaderOutput,
    ) -> Result<(), crossbeam_channel::SendError<LoaderOutput>> {
        let result = self.inner.send(output);
        if result.is_ok()
            && let Some(wake) = self.root_wake.lock().as_ref()
        {
            wake();
        }
        result
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct TileInFlightKey {
    index: usize,
    profile_epoch: u64,
    col: u32,
    row: u32,
    pixel_kind: TilePixelKind,
}

impl TileInFlightKey {
    pub(crate) fn new(
        index: usize,
        profile_epoch: u64,
        col: u32,
        row: u32,
        pixel_kind: TilePixelKind,
    ) -> Self {
        Self {
            index,
            profile_epoch,
            col,
            row,
            pixel_kind,
        }
    }
}

pub(crate) struct TileRequest {
    pub(crate) profile_epoch: u64,
    pub(crate) priority: f32, // Higher is better
    pub(crate) index: usize,
    pub(crate) col: u32,
    pub(crate) row: u32,
    pub(crate) source: TileDecodeSource,
}

impl PartialEq for TileRequest {
    fn eq(&self, other: &Self) -> bool {
        self.profile_epoch == other.profile_epoch && self.priority == other.priority
    }
}
impl Eq for TileRequest {}
impl PartialOrd for TileRequest {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for TileRequest {
    fn cmp(&self, other: &Self) -> Ordering {
        self.profile_epoch.cmp(&other.profile_epoch).then_with(|| {
            self.priority
                .partial_cmp(&other.priority)
                .unwrap_or(Ordering::Equal)
        })
    }
}

/// Single-slot delayed fallback: replaces any pending job so rapid `request_load`
/// cannot spawn one OS thread per request (see `ImageLoader::request_load`).
pub(crate) struct DelayedFallbackJob {
    pub(crate) index: usize,
    pub(crate) decode_profile: DecodeProfile,
    pub(crate) path: PathBuf,
    pub(crate) high_quality: bool,
    pub(crate) raw_demosaic_mode: crate::settings::RawDemosaicMode,
    pub(crate) claimed: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) loading: Arc<Mutex<HashMap<usize, InFlightLoad>>>,
    pub(crate) tx: LoaderOutputSender,
    pub(crate) refine_tx: Sender<RefinementRequest>,
    pub(crate) hdr_target_capacity: f32,
    pub(crate) hdr_tone_map: HdrToneMapSettings,
    pub(crate) raw_open_prefetch: Arc<super::raw_prefetch::RawOpenPrefetch>,
    pub(crate) wgpu_device: Option<wgpu::Device>,
    pub(crate) wgpu_queue: Option<wgpu::Queue>,
    pub(crate) wgpu_device_id_at_spawn: u64,
    pub(crate) wgpu_is_opengl: bool,
    pub(crate) wgpu_device_id_live: Arc<AtomicU64>,
    pub(crate) hdr_callback_upload_active_live: Arc<std::sync::atomic::AtomicBool>,
}

pub(crate) fn should_spawn_load_task(
    loading: &mut HashMap<usize, InFlightLoad>,
    index: usize,
    profile: DecodeProfile,
) -> bool {
    match loading.get(&index) {
        Some(existing) => match profile_spawn_relation(&existing.profile, &profile) {
            ProfileSpawnRelation::Equal => false,
            ProfileSpawnRelation::Upgrade => {
                loading.insert(index, InFlightLoad { profile });
                true
            }
            ProfileSpawnRelation::Downgrade => false,
        },
        None => {
            loading.insert(index, InFlightLoad { profile });
            true
        }
    }
}

pub struct ImageLoader {
    pub(crate) raw_open_prefetch: std::sync::Arc<super::raw_prefetch::RawOpenPrefetch>,
    pub(crate) tx: LoaderOutputSender,
    pub rx: Receiver<LoaderOutput>,
    /// Maps image index -> registered in-flight load (profile + diagnostic generation).
    pub(crate) loading: Arc<Mutex<HashMap<usize, InFlightLoad>>>,
    pub(crate) preload_plan: Arc<PreloadPlanSnapshot>,
    pub(crate) pool: Arc<rayon::ThreadPool>,
    /// Priority queue for tile requests.
    pub(crate) tile_queue: Arc<(Mutex<BinaryHeap<TileRequest>>, Condvar)>,
    /// Channel for background refinement tasks (LibRaw).
    pub(crate) refine_tx: Sender<RefinementRequest>,
    /// Local deque for results that were polled but deferred due to per-frame
    /// upload quota. Drained before the crossbeam channel on the next frame.
    pub(crate) local_queue: std::collections::VecDeque<LoaderOutput>,
    /// Mutex holds at most one pending delayed fallback job; Condvar wakes the worker.
    pub(crate) delayed_fallback: Arc<(Mutex<Option<DelayedFallbackJob>>, Condvar)>,
    pub(crate) hdr_target_capacity_bits: Arc<AtomicU32>,
    pub(crate) hdr_tone_exposure_ev_bits: Arc<AtomicU32>,
    pub(crate) hdr_tone_sdr_white_nits_bits: Arc<AtomicU32>,
    pub(crate) hdr_tone_max_display_nits_bits: Arc<AtomicU32>,
    /// True when the main thread has an active HDR callback target format for pre-upload registration.
    pub(crate) hdr_callback_upload_active: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) wgpu_device: Option<wgpu::Device>,
    pub(crate) wgpu_queue: Option<wgpu::Queue>,
    /// Live epoch; compare before background GPU upload and on the main thread at registration.
    pub(crate) wgpu_device_id: Arc<AtomicU64>,
    pub(crate) wgpu_is_opengl: bool,
    pub(crate) output_mode_bits: Arc<AtomicU32>,
}
