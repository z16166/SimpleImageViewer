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
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64};
use std::time::Duration;

/// Poll interval for worker threads waiting on condvars / channels during idle.
pub(crate) const WORKER_SHUTDOWN_POLL: Duration = Duration::from_millis(50);

use super::preload_plan::PreloadPlanSnapshot;

type RootWakeCallback = Arc<dyn Fn() + Send + Sync>;
type SharedRootWake = Arc<parking_lot::Mutex<Option<RootWakeCallback>>>;

/// Crossbeam sender that wakes the root window when a decode worker posts a result.
#[derive(Clone)]
pub(crate) struct LoaderOutputSender {
    inner: Sender<LoaderOutput>,
    root_wake: SharedRootWake,
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

    pub(crate) fn send(&self, output: LoaderOutput) -> Result<(), ()> {
        let result = self.inner.send(output).map_err(|_| ());
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
    pub(crate) embedded_iso_gain_map_sdr_master_live: Arc<std::sync::atomic::AtomicBool>,
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
            ProfileSpawnRelation::Downgrade => {
                // Supersede the registered profile so gate/install reject stale output; spawn the
                // downgraded request (old worker may still finish but is no longer registered).
                loading.insert(index, InFlightLoad { profile });
                true
            }
        },
        None => {
            loading.insert(index, InFlightLoad { profile });
            true
        }
    }
}

pub struct ImageLoader {
    /// When true, dedicated loader worker threads exit their idle wait loops.
    pub(crate) shutdown: Arc<AtomicBool>,
    pub(crate) raw_open_prefetch: std::sync::Arc<super::raw_prefetch::RawOpenPrefetch>,
    pub(crate) tx: LoaderOutputSender,
    pub rx: Receiver<LoaderOutput>,
    /// Shared with worker threads; maps image index -> registered in-flight load.
    pub(crate) loading: Arc<Mutex<HashMap<usize, InFlightLoad>>>,
    /// Worker-readable navigation window (atomics — see [`PreloadPlanSnapshot`]).
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
    /// User setting: show embedded ISO gain-map SDR master on SDR monitors (skip HDR GPU pre-upload).
    pub(crate) embedded_iso_gain_map_sdr_master: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) wgpu_device: Option<wgpu::Device>,
    pub(crate) wgpu_queue: Option<wgpu::Queue>,
    /// Live epoch; compare before background GPU upload and on the main thread at registration.
    pub(crate) wgpu_device_id: Arc<AtomicU64>,
    pub(crate) wgpu_is_opengl: bool,
    pub(crate) output_mode_bits: Arc<AtomicU32>,
    /// Dedicated OS threads running [`LoadIntent::Current`] decode (bounded — see
    /// [`crate::loader::MAX_CURRENT_IMAGE_OS_THREADS`]).
    pub(crate) current_image_os_threads: Arc<std::sync::atomic::AtomicUsize>,
    /// Main-thread-only HDR capacity requeue storm cap (not shared with workers; no lock needed).
    pub(crate) capacity_requeue_counts: std::collections::HashMap<usize, u32>,
}

impl ImageLoader {
    /// Wake all dedicated worker threads and tell them to exit idle waits.
    pub(crate) fn signal_shutdown(&self) {
        self.shutdown
            .store(true, std::sync::atomic::Ordering::Release);
        {
            let (_, cvar) = &*self.delayed_fallback;
            cvar.notify_all();
        }
        {
            let (_, cvar) = &*self.tile_queue;
            cvar.notify_all();
        }
        self.raw_open_prefetch.wake_waiters();
    }
}
