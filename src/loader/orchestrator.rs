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
use crate::loader::decode::load_image_file;
use crate::loader::{
    hdr_to_sdr_with_user_tone, hq_preview_max_side, DecodedImage, HdrSdrFallbackResult, ImageData,
    LoaderOutput, LoadResult, PreviewBundle, PreviewResult, RefinementRequest, TileDecodeSource,
    TilePixelKind, TileResult,
};
use crate::loader::preview_caps::REFINEMENT_POOL;
use crate::raw_processor::RawProcessor;
use crossbeam_channel::{Receiver, Sender, TryRecvError};
use image::DynamicImage;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::AtomicU32;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct TileInFlightKey {
    index: usize,
    generation: u64,
    col: u32,
    row: u32,
    pixel_kind: TilePixelKind,
}

impl TileInFlightKey {
    pub(crate) fn new(index: usize, generation: u64, col: u32, row: u32, pixel_kind: TilePixelKind) -> Self {
        Self {
            index,
            generation,
            col,
            row,
            pixel_kind,
        }
    }
}

struct TileRequest {
    generation: u64,
    priority: f32, // Higher is better
    index: usize,
    col: u32,
    row: u32,
    source: TileDecodeSource,
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
struct DelayedFallbackJob {
    index: usize,
    generation: u64,
    path: PathBuf,
    high_quality: bool,
    claimed: Arc<std::sync::atomic::AtomicBool>,
    loading: Arc<Mutex<HashMap<usize, u64>>>,
    current_gen: Arc<std::sync::atomic::AtomicU64>,
    tx: Sender<LoaderOutput>,
    refine_tx: Sender<RefinementRequest>,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
}

pub struct ImageLoader {
    tx: Sender<LoaderOutput>,
    pub rx: Receiver<LoaderOutput>,
    /// Maps image index -> latest requested generation ID.
    loading: Arc<Mutex<HashMap<usize, u64>>>,
    /// Global generation counter — updated on every navigation.
    /// Spawned tasks check this to detect staleness and abort early.
    current_gen: Arc<std::sync::atomic::AtomicU64>,
    pool: Arc<rayon::ThreadPool>,
    /// Priority queue for tile requests.
    tile_queue: Arc<(Mutex<BinaryHeap<TileRequest>>, Condvar)>,
    /// Channel for background refinement tasks (LibRaw).
    refine_tx: Sender<RefinementRequest>,
    /// Local deque for results that were polled but deferred due to per-frame
    /// upload quota. Drained before the crossbeam channel on the next frame.
    local_queue: std::collections::VecDeque<LoaderOutput>,
    /// Mutex holds at most one pending delayed fallback job; Condvar wakes the worker.
    delayed_fallback: Arc<(Mutex<Option<DelayedFallbackJob>>, Condvar)>,
    hdr_target_capacity_bits: Arc<AtomicU32>,
    hdr_tone_exposure_ev_bits: Arc<AtomicU32>,
    hdr_tone_sdr_white_nits_bits: Arc<AtomicU32>,
    hdr_tone_max_display_nits_bits: Arc<AtomicU32>,
}

impl ImageLoader {
    pub fn new() -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        let (refine_tx, refine_rx): (Sender<RefinementRequest>, Receiver<RefinementRequest>) =
            crossbeam_channel::unbounded();
        let pool_builder =
            rayon::ThreadPoolBuilder::new().thread_name(|i| format!("img-loader-{i}"));

        #[cfg(target_os = "windows")]
        let pool_builder = pool_builder.spawn_handler(|rayon_thread| {
            let mut builder = std::thread::Builder::new();
            if let Some(name) = rayon_thread.name() {
                builder = builder.name(name.to_owned());
            }
            if let Some(stack_size) = rayon_thread.stack_size() {
                builder = builder.stack_size(stack_size);
            }

            builder.spawn(move || {
                match crate::wic::ComGuard::new() {
                    Ok(_com) => rayon_thread.run(),
                    Err(e) => {
                        log::error!("Failed to initialize COM on loader worker thread: {:?}", e);
                        // Still run the rayon thread tasks, but WIC calls will likely fail gracefully
                        rayon_thread.run()
                    }
                }
            })?;
            Ok(())
        });

        let pool = match pool_builder.build() {
            Ok(p) => p,
            Err(e) => {
                log::error!(
                    "[Loader] Failed to create image loader thread pool: {}. Falling back to minimal pool.",
                    e
                );
                rayon::ThreadPoolBuilder::new()
                    .num_threads(1)
                    .build()
                    .unwrap()
            }
        };

        let current_gen = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let hdr_target_capacity_bits = Arc::new(AtomicU32::new(
            HdrToneMapSettings::default()
                .target_hdr_capacity()
                .to_bits(),
        ));

        let default_tone = HdrToneMapSettings::default();
        let hdr_tone_exposure_ev_bits = Arc::new(AtomicU32::new(default_tone.exposure_ev.to_bits()));
        let hdr_tone_sdr_white_nits_bits =
            Arc::new(AtomicU32::new(default_tone.sdr_white_nits.to_bits()));
        let hdr_tone_max_display_nits_bits =
            Arc::new(AtomicU32::new(default_tone.max_display_nits.to_bits()));

        let delayed_fallback = Arc::new((Mutex::new(None::<DelayedFallbackJob>), Condvar::new()));
        {
            let state = Arc::clone(&delayed_fallback);
            let _ = std::thread::Builder::new()
                .name("loader-fallback".to_string())
                .spawn(move || {
                    let (lock, cvar) = &*state;
                    loop {
                        let mut job = {
                            let mut g = lock.lock().unwrap();
                            loop {
                                while g.is_none() {
                                    g = cvar.wait(g).unwrap();
                                }
                                if let Some(j) = g.take() {
                                    break j;
                                }
                            }
                        };
                        loop {
                            std::thread::sleep(Duration::from_millis(50));
                            let mut g = lock.lock().unwrap();
                            if let Some(newer) = g.take() {
                                job = newer;
                                drop(g);
                                continue;
                            }
                            drop(g);
                            break;
                        }

                        let global_gen = job.current_gen.load(std::sync::atomic::Ordering::Relaxed);
                        if job.generation != global_gen {
                            let mut loading = job.loading.lock().unwrap();
                            if loading.get(&job.index) == Some(&job.generation) {
                                loading.remove(&job.index);
                            }
                            continue;
                        }
                        if job
                            .claimed
                            .compare_exchange(
                                false,
                                true,
                                std::sync::atomic::Ordering::AcqRel,
                                std::sync::atomic::Ordering::Relaxed,
                            )
                            .is_err()
                        {
                            continue;
                        }

                        #[cfg(target_os = "windows")]
                        let _com = crate::wic::ComGuard::new();

                        Self::do_load(
                            job.index,
                            job.generation,
                            &job.path,
                            job.tx.clone(),
                            job.refine_tx.clone(),
                            job.loading.clone(),
                            job.current_gen.clone(),
                            job.high_quality,
                            job.hdr_target_capacity,
                            job.hdr_tone_map,
                        );
                    }
                });
        }

        let tile_queue: Arc<(Mutex<BinaryHeap<TileRequest>>, Condvar)> =
            Arc::new((Mutex::new(BinaryHeap::new()), Condvar::new()));
        // Shared set of tiles currently being decoded — prevents duplicate work across workers
        let in_flight: Arc<Mutex<std::collections::HashSet<TileInFlightKey>>> =
            Arc::new(Mutex::new(std::collections::HashSet::new()));

        // Spawn dedicated tile worker threads.
        // Windows with mimalloc + moka: 4 workers is the sweet spot (~90ms/tile, ~44 tiles/sec).
        // seek_read was tested but slower than mmap (syscall overhead > page fault cost).
        #[cfg(target_os = "windows")]
        let worker_count = std::thread::available_parallelism()
            .map(|n| (n.get() / 2).clamp(2, 4))
            .unwrap_or(4);
        #[cfg(not(target_os = "windows"))]
        let worker_count = std::thread::available_parallelism()
            .map(|n| (n.get() / 2).clamp(4, 12))
            .unwrap_or(4);

        for i in 0..worker_count {
            let queue = Arc::clone(&tile_queue);
            let tx = tx.clone();
            let gen_ref = Arc::clone(&current_gen);
            let flight = Arc::clone(&in_flight);

            std::thread::Builder::new()
                .name(format!("tile-worker-{}", i))
                .spawn(move || {
                    #[cfg(target_os = "windows")]
                    let _com = crate::wic::ComGuard::new();

                    loop {
                        let request = {
                            let (lock, cvar) = &*queue;
                            let mut heap = lock.lock().unwrap();
                            while heap.is_empty() {
                                heap = cvar.wait(heap).unwrap();
                            }
                            if let Some(req) = heap.pop() {
                                req
                            } else {
                                continue;
                            }
                        };

                        // Check if this request is still relevant for the global counter
                        if gen_ref.load(std::sync::atomic::Ordering::Relaxed) > request.generation {
                            continue;
                        }

                        let pixel_kind = request.source.pixel_kind();
                        let tile_key = TileInFlightKey::new(
                            request.index,
                            request.generation,
                            request.col,
                            request.row,
                            pixel_kind,
                        );

                        let tile_size = crate::tile_cache::get_tile_size();
                        let x = request.col * tile_size;
                        let y = request.row * tile_size;

                        let already_cached = match &request.source {
                            TileDecodeSource::Sdr(_) => {
                                if let Ok(mut cache) = crate::tile_cache::PIXEL_CACHE.lock() {
                                    cache
                                        .get(
                                            request.index,
                                            crate::tile_cache::TileCoord {
                                                col: request.col,
                                                row: request.row,
                                            },
                                        )
                                        .is_some()
                                } else {
                                    false
                                }
                            }
                            TileDecodeSource::Hdr(source) => {
                                let tw = tile_size.min(source.width() - x);
                                let th = tile_size.min(source.height() - y);
                                source.cached_tile_rgba32f_arc(x, y, tw, th).is_some()
                            }
                        };
                        if already_cached {
                            let _ = tx.send(LoaderOutput::Tile(TileResult {
                                index: request.index,
                                generation: request.generation,
                                col: request.col,
                                row: request.row,
                                pixel_kind,
                            }));
                            continue;
                        }

                        // Claim this tile — skip if another worker is already decoding it
                        {
                            let mut set = flight.lock().unwrap();
                            if !set.insert(tile_key) {
                                continue; // Another worker is already on it
                            }
                        }

                        match request.source {
                            TileDecodeSource::Sdr(source) => {
                                let tw = tile_size.min(source.width() - x);
                                let th = tile_size.min(source.height() - y);

                                #[cfg(feature = "tile-debug")]
                                let t0 = std::time::Instant::now();
                                let pixels = source.extract_tile(x, y, tw, th);
                                #[cfg(feature = "tile-debug")]
                                {
                                    let decode_ms = t0.elapsed().as_millis();
                                    if decode_ms > 50 {
                                        log::info!(
                                            "[tile-worker-{}] decode tile ({},{}) {}x{} took {}ms",
                                            i,
                                            request.col,
                                            request.row,
                                            tw,
                                            th,
                                            decode_ms
                                        );
                                    }
                                }

                                // Insert into PIXEL_CACHE immediately from the worker thread.
                                // This MUST happen BEFORE removing from in_flight to close the
                                // race window: without this, another worker could see the tile as
                                // "not cached AND not in-flight" and start a redundant decode.
                                {
                                    let coord = crate::tile_cache::TileCoord {
                                        col: request.col,
                                        row: request.row,
                                    };
                                    if let Ok(mut cache) = crate::tile_cache::PIXEL_CACHE.lock() {
                                        cache.insert(request.index, coord, pixels);
                                    }
                                }
                            }
                            TileDecodeSource::Hdr(source) => {
                                let tw = tile_size.min(source.width() - x);
                                let th = tile_size.min(source.height() - y);
                                #[cfg(feature = "tile-debug")]
                                let t0 = std::time::Instant::now();
                                let result = source.extract_tile_rgba32f_arc(x, y, tw, th);
                                #[cfg(feature = "tile-debug")]
                                {
                                    let decode_ms = t0.elapsed().as_millis();
                                    if decode_ms > 50 {
                                        log::info!(
                                            "[tile-worker-{}] decode HDR tile file=\"{}\" index={} generation={} coord=({},{}) size={}x{} took {}ms",
                                            i,
                                            source.source_name(),
                                            request.index,
                                            request.generation,
                                            request.col,
                                            request.row,
                                            tw,
                                            th,
                                            decode_ms
                                        );
                                    }
                                }
                                if let Err(err) = result {
                                    log::warn!(
                                        "[tile-worker-{}] HDR tile decode failed file=\"{}\" index={} generation={} coord=({},{}): {}",
                                        i,
                                        source.source_name(),
                                        request.index,
                                        request.generation,
                                        request.col,
                                        request.row,
                                        err
                                    );
                                    let mut set = flight.lock().unwrap();
                                    set.remove(&tile_key);
                                    drop(set);
                                    let _ = tx.send(LoaderOutput::Tile(TileResult {
                                        index: request.index,
                                        generation: request.generation,
                                        col: request.col,
                                        row: request.row,
                                        pixel_kind,
                                    }));
                                    continue;
                                }
                            }
                        }

                        // Remove from in-flight set (cache already has the data)
                        {
                            let mut set = flight.lock().unwrap();
                            set.remove(&tile_key);
                        }

                        // Notify main thread that tile is ready for GPU upload
                        let _ = tx.send(LoaderOutput::Tile(TileResult {
                            index: request.index,
                            generation: request.generation,
                            col: request.col,
                            row: request.row,
                            pixel_kind,
                        }));
                    }
                })
                .ok();
        }

        // Start dedicated Background Refinement Worker (Throttled)
        let worker_tx = tx.clone();
        let worker_gen = current_gen.clone();
        let _ = std::thread::Builder::new()
            .name("refinement-worker".to_string())
            .spawn(move || {
                while let Ok(req) = refine_rx.recv() {
                    // 1. Pre-develop staleness check
                    let global_gen = worker_gen.load(std::sync::atomic::Ordering::Relaxed);
                    if req.generation < global_gen {
                        log::info!("[Refinement] Skipping stale request for {:?} (gen {} < {})",
                            req.path.file_name().unwrap_or_default(), req.generation, global_gen);
                        continue;
                    }

                    // 2. Perform heavy development
                    log::debug!("[Refinement] Starting full demosaic for {:?} (gen={})", req.path.file_name().unwrap_or_default(), req.generation);
                    let t0 = std::time::Instant::now();

                    let mut processor = match RawProcessor::new() {
                        Some(p) => p,
                        None => {
                            log::error!("[Refinement] Failed to create RawProcessor");
                            continue;
                        }
                    };

                    match processor.open(&req.path) {
                        Ok(()) => {},
                        Err(e) => {
                            log::error!("[Refinement] Failed to open {:?}: {}", req.path.file_name().unwrap_or_default(), e);
                            continue;
                        }
                    }

                    if let Some(flip) = req.orientation_override {
                        processor.set_user_flip(flip);
                    }

                    match processor.develop() {
                        Ok(full_img) => {
                            let elapsed = t0.elapsed();

                            // 3. Post-develop staleness check — develop() takes seconds.
                            // If the user navigated away during that time, discard the
                            // ~400MB result immediately instead of storing it.
                            let global_gen = worker_gen.load(std::sync::atomic::Ordering::Relaxed);
                            if req.generation < global_gen {
                                log::info!("[Refinement] Discarding stale develop result for {:?} (gen {} < {}) — saving ~400MB",
                                    req.path.file_name().unwrap_or_default(), req.generation, global_gen);
                                continue;
                            }

                            // SINGLE-PASS: into_rgba8() avoids cloning since processor.develop() 
                            // now returns ImageRgba8 directly.
                            let rgba = full_img.into_rgba8();
                            let (w, h) = rgba.dimensions();
                            let pixels = rgba.into_raw();

                            let dynamic = if let Some(buf) = image::ImageBuffer::from_raw(w, h, pixels) {
                                DynamicImage::ImageRgba8(buf)
                            } else {
                                log::error!("[Refinement] Failed to create image buffer from raw bits ({}x{})", w, h);
                                continue;
                            };

                            // Generate a high-quality preview for the UI so the user gets
                            // a sharp full-screen image immediately, without needing to zoom in past the tile threshold.
                            let limit = hq_preview_max_side();
                            let scaled = dynamic.thumbnail(limit, limit);
                            let prev_rgba = scaled.into_rgba8();
                            let preview = DecodedImage::new(
                                prev_rgba.width(),
                                prev_rgba.height(),
                                prev_rgba.into_raw(),
                            );

                            let mut dev_lock = req.developed_image.write();
                            *dev_lock = Some(dynamic);
                            drop(dev_lock);

                            let _ = worker_tx.send(LoaderOutput::Preview(
                                PreviewResult::from_sdr_preview(
                                    req.index,
                                    req.generation,
                                    Ok(preview),
                                ),
                            ));

                            // Notify UI to clear cache and cross-fade
                            let _ = worker_tx.send(LoaderOutput::Refined(req.index, req.generation));
                            log::info!("[Refinement] Completed {}x{} in {:.1}s", w, h, elapsed.as_secs_f64());
                        }
                        Err(e) => {
                            log::error!("[Refinement] LibRaw develop failed for {:?} after {:.1}s: {}", 
                                req.path.file_name().unwrap_or_default(), t0.elapsed().as_secs_f64(), e);
                        }
                    }
                }
            });

        Self {
            tx,
            rx,
            loading: Arc::new(Mutex::new(HashMap::new())),
            current_gen,
            pool: Arc::new(pool),
            tile_queue,
            refine_tx,
            local_queue: std::collections::VecDeque::new(),
            delayed_fallback,
            hdr_target_capacity_bits,
            hdr_tone_exposure_ev_bits,
            hdr_tone_sdr_white_nits_bits,
            hdr_tone_max_display_nits_bits,
        }
    }

    pub fn is_loading(&self, index: usize, generation: u64) -> bool {
        self.loading.lock().unwrap().get(&index) == Some(&generation)
    }

    #[allow(dead_code)]
    pub fn current_generation(&self, index: usize) -> u64 {
        self.loading
            .lock()
            .unwrap()
            .get(&index)
            .copied()
            .unwrap_or(0)
    }

    /// Update the global generation counter so stale preloads abort early.
    pub fn set_generation(&self, generation: u64) {
        self.current_gen
            .store(generation, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn set_hdr_target_capacity(&self, capacity: f32) {
        self.hdr_target_capacity_bits
            .store(capacity.to_bits(), std::sync::atomic::Ordering::Relaxed);
    }

    fn hdr_target_capacity(&self) -> f32 {
        f32::from_bits(
            self.hdr_target_capacity_bits
                .load(std::sync::atomic::Ordering::Relaxed),
        )
    }

    /// Snapshot used by the next background decode for CPU SDR fallbacks (PQ/HLG peak, exposure).
    pub fn set_hdr_tone_map_settings(&self, tone: HdrToneMapSettings) {
        self.hdr_tone_exposure_ev_bits.store(
            tone.exposure_ev.to_bits(),
            std::sync::atomic::Ordering::Relaxed,
        );
        self.hdr_tone_sdr_white_nits_bits.store(
            tone.sdr_white_nits.to_bits(),
            std::sync::atomic::Ordering::Relaxed,
        );
        self.hdr_tone_max_display_nits_bits.store(
            tone.max_display_nits.to_bits(),
            std::sync::atomic::Ordering::Relaxed,
        );
    }

    fn hdr_tone_map_settings_snapshot(&self) -> HdrToneMapSettings {
        HdrToneMapSettings {
            exposure_ev: f32::from_bits(
                self.hdr_tone_exposure_ev_bits
                    .load(std::sync::atomic::Ordering::Relaxed),
            ),
            sdr_white_nits: f32::from_bits(
                self.hdr_tone_sdr_white_nits_bits
                    .load(std::sync::atomic::Ordering::Relaxed),
            ),
            max_display_nits: f32::from_bits(
                self.hdr_tone_max_display_nits_bits
                    .load(std::sync::atomic::Ordering::Relaxed),
            ),
        }
    }

    pub fn request_load(
        &mut self,
        index: usize,
        generation: u64,
        path: PathBuf,
        high_quality: bool,
    ) {
        {
            let mut loading = self.loading.lock().unwrap();
            if let Some(&existing) = loading.get(&index) {
                if generation > existing {
                    loading.insert(index, generation);
                }
                return;
            }
            loading.insert(index, generation);
        }

        let claimed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let current_gen = Arc::clone(&self.current_gen);

        let path1 = path.clone();
        let path2 = path;
        let tx1 = self.tx.clone();
        let tx2 = self.tx.clone();
        let loading1 = Arc::clone(&self.loading);
        let loading2 = Arc::clone(&self.loading);
        let claimed1 = Arc::clone(&claimed);
        let claimed2 = Arc::clone(&claimed);
        let current_gen1 = Arc::clone(&current_gen);
        let current_gen2 = Arc::clone(&current_gen);
        let rtx1 = self.refine_tx.clone();
        let rtx2 = self.refine_tx.clone();
        let hdr_target_capacity = self.hdr_target_capacity();
        let hdr_tone_map = self.hdr_tone_map_settings_snapshot();

        self.pool.spawn(move || {
            let global_gen = current_gen1.load(std::sync::atomic::Ordering::Relaxed);
            if generation != global_gen {
                let mut loading = loading1.lock().unwrap();
                if loading.get(&index) == Some(&generation) {
                    loading.remove(&index);
                }
                return;
            }
            if claimed1
                .compare_exchange(
                    false,
                    true,
                    std::sync::atomic::Ordering::AcqRel,
                    std::sync::atomic::Ordering::Relaxed,
                )
                .is_err()
            {
                return;
            }
            Self::do_load(
                index,
                generation,
                &path1,
                tx1,
                rtx1,
                loading1,
                current_gen1,
                high_quality,
                hdr_target_capacity,
                hdr_tone_map,
            );
        });

        // Fallback: one shared worker sleeps 50ms then tries `do_load` if the pool task
        // did not claim first. Pending jobs are coalesced to a single slot (no per-request OS thread).
        let delayed_job = DelayedFallbackJob {
            index,
            generation,
            path: path2,
            high_quality,
            claimed: claimed2,
            loading: loading2,
            current_gen: current_gen2,
            tx: tx2,
            refine_tx: rtx2,
            hdr_target_capacity,
            hdr_tone_map,
        };
        {
            let (lock, cvar) = &*self.delayed_fallback;
            let mut slot = lock.lock().unwrap();
            *slot = Some(delayed_job);
            cvar.notify_one();
        }
    }

    /// True when [`ImageLoader::loading`] shows a **strictly newer** registered load generation for
    /// `index` than the adoption generation from the decode worker (`adoptee_generation`).
    ///
    /// HQ refinement must **not** use `loader.current_gen` alone for staleness: prefetch promotion to
    /// the current TileManager bumps [`ImageLoader::set_generation`] without re-queuing a load while
    /// the UI deliberately accepts prefetch-era previews (`prefetch_prev_generation` in image
    /// management). Likewise, `finish_image_request` clears the map without implying supersession for
    /// in-flight refinement.
    #[inline]
    fn hq_refinement_superseded(
        loading: &Arc<Mutex<HashMap<usize, u64>>>,
        index: usize,
        adoptee_generation: u64,
    ) -> bool {
        loading
            .lock()
            .unwrap()
            .get(&index)
            .is_some_and(|&registered| registered > adoptee_generation)
    }

    fn do_load(
        index: usize,
        generation: u64,
        path: &PathBuf,
        tx: Sender<LoaderOutput>,
        refine_tx: Sender<RefinementRequest>,
        loading_ref: Arc<Mutex<HashMap<usize, u64>>>,
        _current_gen: Arc<std::sync::atomic::AtomicU64>,
        high_quality: bool,
        hdr_target_capacity: f32,
        hdr_tone_map: HdrToneMapSettings,
    ) {
        // Adoption logic: We no longer abort if global_gen has changed.
        // As long as our index is still in the loading map, we continue.
        {
            let loading = loading_ref.lock().unwrap();
            if !loading.contains_key(&index) {
                return;
            }
        }

        let mut load_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            load_image_file(
                generation,
                index,
                path,
                tx.clone(),
                refine_tx.clone(),
                high_quality,
                hdr_target_capacity,
                hdr_tone_map,
            )
        }))
        .unwrap_or_else(|e| {
            let msg = if let Some(s) = e.downcast_ref::<&str>() {
                (*s).to_string()
            } else if let Some(s) = e.downcast_ref::<String>() {
                s.clone()
            } else {
                "Unknown panic".to_string()
            };
            log::error!(
                "[Loader] DECODER CRASH (panic) for index={}: {}",
                index,
                msg
            );
            LoadResult {
                index,
                generation,
                result: Err(format!("Decoder Panic: {}", msg)),
                preview_bundle: PreviewBundle::initial(),
                ultra_hdr_capacity_sensitive: false,
                sdr_fallback_is_placeholder: false,
            }
        });

        if let Err(ref e) = load_result.result {
            log::error!("[Loader] Load FAILED for index={}: {}", index, e);
        }

        // Finalize result generation: read the LATEST generation ID from the map.
        // This allows the worker to "adopt" newer generations that were requested
        // while the decode was in progress.
        let final_gen = {
            let map = loading_ref.lock().unwrap();
            if let Some(&latest) = map.get(&index) {
                latest
            } else {
                // Index was removed from loading map (cancelled)
                return;
            }
        };

        load_result.generation = final_gen;

        // Tiled HQ preview: only `Arc::clone` the source; `load_result` moves to the channel once
        // (avoids cloning full Static/Animated pixel buffers).
        if let Ok(ref image_data) = load_result.result {
            let sdr_source = image_data.tiled_sdr_source().cloned();
            let hdr_source = image_data.tiled_hdr_source().cloned();
            let tx_cloned = tx.clone();
            let loading_for_hq = Arc::clone(&loading_ref);
            match (hdr_source, sdr_source) {
                (Some(source), _) => {
                    let file_name = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("unknown")
                        .to_string();
                    REFINEMENT_POOL.spawn(move || {
                        if Self::hq_refinement_superseded(&loading_for_hq, index, final_gen) {
                            return;
                        }

                        #[cfg(target_os = "windows")]
                        let _com = crate::wic::ComGuard::new();

                        let limit = hq_preview_max_side();
                        let started_at = std::time::Instant::now();
                        log::info!(
                            "[Loader] [{}] HQ preview start: index={} generation={} limit={} source={}x{} (hdr_mode={})",
                            file_name,
                            index,
                            final_gen,
                            limit,
                            source.width(),
                            source.height(),
                            hdr_target_capacity > 1.0
                        );
                        let is_hdr_mode = hdr_target_capacity > 1.0;
                        let r_result =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<_, String> {
                                let hdr = source.generate_hdr_preview(limit, limit)?;
                                let sdr = if !is_hdr_mode {
                                    Some(crate::hdr::tiled::sdr_preview_from_hdr_preview(&hdr)?)
                                } else {
                                    None
                                };
                                Ok((hdr, sdr))
                            }));

                        match r_result {
                            Ok(Ok((hdr, sdr))) => {
                                if Self::hq_refinement_superseded(&loading_for_hq, index, final_gen) {
                                    log::debug!(
                                        "[Loader] [{}] HQ preview discarded as stale: index={} generation={} elapsed={:?}",
                                        file_name,
                                        index,
                                        final_gen,
                                        started_at.elapsed()
                                    );
                                    return;
                                }
                                log::debug!(
                                    "[Loader] [{}] HQ previews generated: {}x{} (source {}x{}, limit={}, elapsed={:?}, hdr_mode={})",
                                    file_name,
                                    hdr.width,
                                    hdr.height,
                                    source.width(),
                                    source.height(),
                                    limit,
                                    started_at.elapsed(),
                                    is_hdr_mode
                                );
                                // Always publish the HDR float preview when we decoded it. `hdr_mode`
                                // only controls whether we also build an SDR tone-map helper plane;
                                // native HDR display samples the HDR preview cache/TM path and would
                                // otherwise stay on the coarse bootstrap HDR if we attached SDR only.
                                let mut bundle =
                                    PreviewBundle::refined().with_hdr(Arc::new(hdr));
                                if let Some(s) = sdr {
                                    bundle = bundle.with_sdr(DecodedImage::new(s.0, s.1, s.2));
                                }

                                let _ = tx_cloned.send(LoaderOutput::Preview(PreviewResult {
                                    index,
                                    generation: final_gen,
                                    preview_bundle: bundle,
                                    error: None,
                                }));
                            }
                            Ok(Err(e)) => {
                                log::error!(
                                    "[Loader] [{}] High-quality HDR preview failed: index={} generation={} limit={} elapsed={:?}: {e}",
                                    file_name,
                                    index,
                                    final_gen,
                                    limit,
                                    started_at.elapsed()
                                );
                            }
                            Err(e) => {
                                log::error!(
                                    "[Loader] [{}] High-quality HDR preview PANICKED: index={} generation={} limit={} elapsed={:?}: {:?}",
                                    file_name,
                                    index,
                                    final_gen,
                                    limit,
                                    started_at.elapsed(),
                                    e
                                );
                            }
                        }
                    });
                }
                (None, Some(source)) => {
                    let loading_sdr_hq = Arc::clone(&loading_ref);
                    REFINEMENT_POOL.spawn(move || {
                        if Self::hq_refinement_superseded(&loading_sdr_hq, index, final_gen) {
                            return;
                        }

                        #[cfg(target_os = "windows")]
                        let _com = crate::wic::ComGuard::new();

                        let limit = hq_preview_max_side();
                        let r_result =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                source.generate_preview(limit, limit)
                            }));

                        match r_result {
                            Ok((pw, ph, p_pixels)) if pw > 0 && ph > 0 => {
                                if Self::hq_refinement_superseded(&loading_sdr_hq, index, final_gen) {
                                    return;
                                }

                                log::debug!(
                                    "[Loader] HQ preview generated: {}x{} (source {}x{})",
                                    pw,
                                    ph,
                                    source.width(),
                                    source.height()
                                );
                                let _ = tx_cloned.send(LoaderOutput::Preview(
                                    PreviewResult::from_sdr_preview(
                                        index,
                                        final_gen,
                                        Ok(DecodedImage::new(pw, ph, p_pixels)),
                                    ),
                                ));
                            }
                            Err(e) => {
                                log::error!("[Loader] High-quality refinement PANICKED: {:?}", e);
                            }
                            _ => {}
                        }
                    });
                }
                (None, None) => {
                    spawn_hdr_sdr_fallback_if_placeholder(
                        &load_result,
                        final_gen,
                        &tx,
                        &loading_ref,
                        hdr_tone_map,
                    );
                    let _ = tx.send(LoaderOutput::Image(load_result));
                    return;
                }
            }
        }

        spawn_hdr_sdr_fallback_if_placeholder(
            &load_result,
            final_gen,
            &tx,
            &loading_ref,
            hdr_tone_map,
        );
        let _ = tx.send(LoaderOutput::Image(load_result));
    }

    pub fn request_tile(
        &self,
        index: usize,
        generation: u64,
        priority: f32,
        source: TileDecodeSource,
        col: u32,
        row: u32,
    ) {
        let (lock, cvar) = &*self.tile_queue;
        let mut heap = lock.lock().unwrap();
        heap.push(TileRequest {
            generation,
            priority,
            index,
            col,
            row,
            source,
        });
        cvar.notify_one();
    }

    /// Drop queued decode results from a previous `generation` so rapid navigation
    /// cannot retain hundreds of megabytes in the unbounded channel / defer queue.
    ///
    /// `also_keep_preview` — when `Some((index, gen))`, Preview results for that
    /// specific (index, generation) are also preserved even though they don't match
    /// `keep_generation`. Used when a prefetched TileManager is promoted to current:
    /// the prefetch-phase HQ preview task carries the old generation and must not be
    /// discarded merely because the generation counter was bumped on promotion.
    pub fn discard_pending_stale_outputs(
        &mut self,
        keep_generation: u64,
        also_keep_preview: Option<(usize, u64)>,
    ) {
        let keep = |output: &LoaderOutput| -> bool {
            match output {
                LoaderOutput::Image(r) => r.generation == keep_generation,
                LoaderOutput::Preview(p) => {
                    p.generation == keep_generation
                        || also_keep_preview
                            .is_some_and(|(idx, old_gen)| p.index == idx && p.generation == old_gen)
                }
                LoaderOutput::HdrSdrFallback(h) => h.generation == keep_generation,
                LoaderOutput::Refined(_, g) => *g == keep_generation,
                LoaderOutput::Tile(t) => t.generation == keep_generation,
            }
        };

        let mut retained = std::collections::VecDeque::new();
        for output in self.local_queue.drain(..) {
            if keep(&output) {
                retained.push_back(output);
            } else if let LoaderOutput::Image(ref r) = output {
                let mut loading = self.loading.lock().unwrap();
                if loading.get(&r.index) == Some(&r.generation) {
                    loading.remove(&r.index);
                }
            }
        }
        self.local_queue = retained;

        while let Ok(output) = self.rx.try_recv() {
            if keep(&output) {
                self.local_queue.push_back(output);
            } else if let LoaderOutput::Image(ref r) = output {
                let mut loading = self.loading.lock().unwrap();
                if loading.get(&r.index) == Some(&r.generation) {
                    loading.remove(&r.index);
                }
            }
        }
    }

    pub fn poll(&mut self) -> Option<LoaderOutput> {
        // Priority: drain deferred items from previous frames first.
        if let Some(output) = self.local_queue.pop_front() {
            return Some(output);
        }

        match self.rx.try_recv() {
            Ok(output) => Some(output),
            Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => None,
        }
    }

    pub fn finish_image_request(&self, index: usize, generation: u64) {
        let mut loading = self.loading.lock().unwrap();
        if let Some(&g) = loading.get(&index) {
            if g <= generation {
                loading.remove(&index);
            }
        }
    }

    /// Push a result back so it is retried on the next frame.
    /// Used by the UI thread when the per-frame upload quota is reached.
    /// Items are pushed to the FRONT so order is preserved across frames.
    pub fn repush(&mut self, output: LoaderOutput) {
        self.local_queue.push_front(output);
    }

    /// Clear all pending tile requests from the queue.
    /// Called on zoom change to discard tiles from stale zoom levels.
    pub fn flush_tile_queue(&self) {
        let (lock, _) = &*self.tile_queue;
        lock.lock().unwrap().clear();
    }

    pub fn cancel_all(&mut self) {
        self.loading.lock().unwrap().clear();
        self.local_queue.clear();
        {
            let (lock, cvar) = &*self.delayed_fallback;
            let mut slot = lock.lock().unwrap();
            *slot = None;
            cvar.notify_one();
        }
        {
            let (lock, _) = &*self.tile_queue;
            lock.lock().unwrap().clear();
        }
        while self.rx.try_recv().is_ok() {}
    }

    #[cfg(test)]
    pub(crate) fn test_register_inflight(&self, index: usize, generation: u64) {
        self.loading.lock().unwrap().insert(index, generation);
    }

    #[cfg(test)]
    pub(crate) fn test_send_loader_output(&self, output: LoaderOutput) {
        self.tx.send(output).expect("test loader channel send");
    }
}
fn spawn_hdr_sdr_fallback_if_placeholder(
    load_result: &LoadResult,
    final_gen: u64,
    tx: &Sender<LoaderOutput>,
    loading: &Arc<Mutex<HashMap<usize, u64>>>,
    tone: HdrToneMapSettings,
) {
    if !load_result.sdr_fallback_is_placeholder {
        return;
    }
    let Ok(ImageData::Hdr { hdr, .. }) = &load_result.result else {
        return;
    };
    let index = load_result.index;
    let hdr = hdr.clone();
    let tx = tx.clone();
    let loading = Arc::clone(loading);
    REFINEMENT_POOL.spawn(move || {
        if ImageLoader::hq_refinement_superseded(&loading, index, final_gen) {
            return;
        }
        #[cfg(target_os = "windows")]
        let _com = crate::wic::ComGuard::new();

        let started_at = std::time::Instant::now();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            hdr_to_sdr_with_user_tone(&hdr, &tone)
        }));
        match r {
            Ok(Ok(pixels)) => {
                if ImageLoader::hq_refinement_superseded(&loading, index, final_gen) {
                    log::info!(
                        "[Loader] HDR SDR fallback refinement discarded (stale): index={index} generation={final_gen}"
                    );
                    return;
                }
                log::debug!(
                    "[Loader] HDR SDR fallback refined after placeholder: index={index} generation={final_gen} elapsed={:?}",
                    started_at.elapsed()
                );
                let fallback = DecodedImage::new(hdr.width, hdr.height, pixels);
                let _ = tx.send(LoaderOutput::HdrSdrFallback(HdrSdrFallbackResult {
                    index,
                    generation: final_gen,
                    fallback,
                }));
            }
            Ok(Err(e)) => {
                log::warn!(
                    "[Loader] HDR SDR fallback refinement failed: index={index} generation={final_gen}: {e}"
                );
            }
            Err(payload) => {
                log::error!(
                    "[Loader] HDR SDR fallback refinement panicked: index={index} generation={final_gen}: {:?}",
                    payload
                );
            }
        }
    });
}

