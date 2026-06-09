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
    LoaderOutput,
    RefinementRequest, TileDecodeSource, TilePixelKind,
};
use crossbeam_channel::{Receiver, Sender};
use image::DynamicImage;
use parking_lot::{Condvar, Mutex};

pub(crate) enum EitherDevelop {
    Sdr(DynamicImage),
    Hdr(crate::hdr::types::HdrImageBuffer),
}
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct TileInFlightKey {
    index: usize,
    generation: u64,
    col: u32,
    row: u32,
    pixel_kind: TilePixelKind,
}

impl TileInFlightKey {
    pub(crate) fn new(
        index: usize,
        generation: u64,
        col: u32,
        row: u32,
        pixel_kind: TilePixelKind,
    ) -> Self {
        Self {
            index,
            generation,
            col,
            row,
            pixel_kind,
        }
    }
}

pub(crate) struct TileRequest {
    pub(crate) generation: u64,
    pub(crate) priority: f32, // Higher is better
    pub(crate) index: usize,
    pub(crate) col: u32,
    pub(crate) row: u32,
    pub(crate) source: TileDecodeSource,
}

impl PartialEq for TileRequest {
    fn eq(&self, other: &Self) -> bool {
        self.generation == other.generation && self.priority == other.priority
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
        self.generation.cmp(&other.generation).then_with(|| {
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
    pub(crate) generation: u64,
    pub(crate) path: PathBuf,
    pub(crate) high_quality: bool,
    pub(crate) claimed: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) loading: Arc<Mutex<HashMap<usize, u64>>>,
    pub(crate) current_gen: Arc<std::sync::atomic::AtomicU64>,
    pub(crate) tx: Sender<LoaderOutput>,
    pub(crate) refine_tx: Sender<RefinementRequest>,
    pub(crate) hdr_target_capacity: f32,
    pub(crate) hdr_tone_map: HdrToneMapSettings,
}

pub(crate) fn should_spawn_load_task(
    loading: &mut HashMap<usize, u64>,
    index: usize,
    generation: u64,
) -> bool {
    match loading.get(&index).copied() {
        Some(existing) if generation <= existing => false,
        _ => {
            loading.insert(index, generation);
            true
        }
    }
}

pub struct ImageLoader {
    pub(crate) tx: Sender<LoaderOutput>,
    pub rx: Receiver<LoaderOutput>,
    /// Maps image index -> latest requested generation ID.
    pub(crate) loading: Arc<Mutex<HashMap<usize, u64>>>,
    /// Global generation counter — updated on every navigation.
    /// Spawned tasks check this to detect staleness and abort early.
    pub(crate) current_gen: Arc<std::sync::atomic::AtomicU64>,
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
}

