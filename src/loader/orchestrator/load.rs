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
use super::types::{
    DelayedFallbackJob, ImageLoader, LoaderOutputSender, TileInFlightKey, TileRequest,
    should_spawn_load_task,
};

use crate::hdr::types::HdrOutputMode;
use crate::hdr::types::HdrToneMapSettings;
use crate::loader::decode::{ImageLoadRequest, load_image_file};
use crate::loader::preview_caps::{REFINEMENT_POOL, finalize_raw_hq_hdr_buffer};
use crate::loader::{
    DecodeProfile, DecodedImage, HdrSdrFallbackResult, ImageData, InFlightLoad, LoadIntent,
    LoadResult, LoaderOutput, MAX_CURRENT_IMAGE_OS_THREADS, MAX_IMG_LOADER_THREADS, PreviewBundle,
    PreviewResult, RefinementRequest, TileDecodeSource, TileResult, decode_profile_stub,
    hdr_display_requests_sdr_preview, hdr_sdr_fallback_rgba8_eager_or_placeholder,
    hq_preview_max_side, in_flight_profile_supersedes_hq_refinement,
    in_flight_profile_supersedes_load_result, source_key_for_path,
    static_hdr_background_plane_upload_eligible,
};
use crate::raw_processor::RawProcessor;
use crossbeam_channel::{Receiver, Sender};
use image::DynamicImage;
use parking_lot::{Condvar, Mutex};

use std::collections::{BinaryHeap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize};
use std::time::Duration;

/// RAII decrement for [`super::types::ImageLoader::current_image_os_threads`].
struct CurrentImageOsThreadGuard(Arc<AtomicUsize>);

impl Drop for CurrentImageOsThreadGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

struct LoadWorkerInput {
    index: usize,
    path: PathBuf,
    tx: super::types::LoaderOutputSender,
    refine_tx: Sender<RefinementRequest>,
    loading_ref: Arc<Mutex<HashMap<usize, InFlightLoad>>>,
    decode_profile: DecodeProfile,
    high_quality: bool,
    raw_demosaic_mode: crate::settings::RawDemosaicMode,
    hdr_target_capacity: f32,
    hdr_tone_map: HdrToneMapSettings,
    raw_open_prefetch: Arc<super::raw_prefetch::RawOpenPrefetch>,
    wgpu_device: Option<wgpu::Device>,
    wgpu_queue: Option<wgpu::Queue>,
    wgpu_device_id_at_spawn: u64,
    wgpu_is_opengl: bool,
    wgpu_device_id_live: Arc<AtomicU64>,
    hdr_callback_upload_active_live: Arc<std::sync::atomic::AtomicBool>,
    embedded_iso_gain_map_sdr_master_live: Arc<std::sync::atomic::AtomicBool>,
}

impl ImageLoader {
    pub fn new() -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        let (refine_tx, refine_rx): (Sender<RefinementRequest>, Receiver<RefinementRequest>) =
            crossbeam_channel::unbounded();
        let pool_builder = rayon::ThreadPoolBuilder::new()
            .num_threads(MAX_IMG_LOADER_THREADS)
            .thread_name(|i| format!("img-loader-{i}"));

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

        let preload_plan = Arc::new(super::preload_plan::PreloadPlanSnapshot::new());
        let output_mode_bits = Arc::new(AtomicU32::new(0));
        let hdr_target_capacity_bits = Arc::new(AtomicU32::new(
            HdrToneMapSettings::default()
                .target_hdr_capacity()
                .to_bits(),
        ));

        let default_tone = HdrToneMapSettings::default();
        let hdr_tone_exposure_ev_bits =
            Arc::new(AtomicU32::new(default_tone.exposure_ev.to_bits()));
        let hdr_tone_sdr_white_nits_bits =
            Arc::new(AtomicU32::new(default_tone.sdr_white_nits.to_bits()));
        let hdr_tone_max_display_nits_bits =
            Arc::new(AtomicU32::new(default_tone.max_display_nits.to_bits()));
        let hdr_callback_upload_active = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let embedded_iso_gain_map_sdr_master = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let delayed_fallback = Arc::new((Mutex::new(None::<DelayedFallbackJob>), Condvar::new()));
        let raw_open_prefetch = Arc::new(super::raw_prefetch::RawOpenPrefetch::new());
        {
            let state = Arc::clone(&delayed_fallback);
            let _ = std::thread::Builder::new()
                .name("loader-fallback".to_string())
                .spawn(move || {
                    let (lock, cvar) = &*state;
                    loop {
                        let mut job = {
                            let mut g = lock.lock();
                            loop {
                                while g.is_none() {
                                    cvar.wait(&mut g);
                                }
                                if let Some(j) = g.take() {
                                    break j;
                                }
                            }
                        };
                        loop {
                            std::thread::sleep(Duration::from_millis(50));
                            let mut g = lock.lock();
                            if let Some(newer) = g.take() {
                                job = newer;
                                drop(g);
                                continue;
                            }
                            drop(g);
                            break;
                        }

                        {
                            let loading = job.loading.lock();
                            if !loading.contains_key(&job.index) {
                                continue;
                            }
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

                        Self::do_load(LoadWorkerInput {
                            index: job.index,
                            path: job.path.clone(),
                            tx: job.tx.clone(),
                            refine_tx: job.refine_tx.clone(),
                            loading_ref: job.loading.clone(),
                            decode_profile: job.decode_profile.clone(),
                            high_quality: job.high_quality,
                            raw_demosaic_mode: job.raw_demosaic_mode,
                            hdr_target_capacity: job.hdr_target_capacity,
                            hdr_tone_map: job.hdr_tone_map,
                            raw_open_prefetch: Arc::clone(&job.raw_open_prefetch),
                            wgpu_device: job.wgpu_device.clone(),
                            wgpu_queue: job.wgpu_queue.clone(),
                            wgpu_device_id_at_spawn: job.wgpu_device_id_at_spawn,
                            wgpu_is_opengl: job.wgpu_is_opengl,
                            wgpu_device_id_live: Arc::clone(&job.wgpu_device_id_live),
                            hdr_callback_upload_active_live: Arc::clone(
                                &job.hdr_callback_upload_active_live,
                            ),
                            embedded_iso_gain_map_sdr_master_live: Arc::clone(
                                &job.embedded_iso_gain_map_sdr_master_live,
                            ),
                        });
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
            let plan_ref = Arc::clone(&preload_plan);
            let flight = Arc::clone(&in_flight);

            std::thread::Builder::new()
                .name(format!("tile-worker-{}", i))
                .spawn(move || {
                    #[cfg(target_os = "windows")]
                    let _com = crate::wic::ComGuard::new();

                    loop {
                        let request = {
                            let (lock, cvar) = &*queue;
                            let mut heap = lock.lock();
                            while heap.is_empty() {
                                cvar.wait(&mut heap);
                            }
                            if let Some(req) = heap.pop() {
                                req
                            } else {
                                continue;
                            }
                        };

                        // Drop tile work when navigation window or profile epoch moved on.
                        if !plan_ref.index_in_window(request.index) {
                            continue;
                        }
                        let snapshot_epoch = plan_ref.profile_epoch();
                        if request.profile_epoch < snapshot_epoch {
                            continue;
                        }

                        let pixel_kind = request.source.pixel_kind();
                        let tile_key = TileInFlightKey::new(
                            request.index,
                            request.profile_epoch,
                            request.col,
                            request.row,
                            pixel_kind,
                        );

                        let tile_size = crate::tile_cache::get_tile_size();
                        let x = request.col * tile_size;
                        let y = request.row * tile_size;

                        let already_cached = match &request.source {
                            TileDecodeSource::Sdr(_) => {
                                crate::tile_cache::PIXEL_CACHE
                                    .lock()
                                    .get(
                                        request.index,
                                        crate::tile_cache::TileCoord {
                                            col: request.col,
                                            row: request.row,
                                        },
                                    )
                                    .is_some()
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
                                decode_profile: crate::loader::decode_profile_with_epoch(
                                    request.profile_epoch,
                                ),
                                col: request.col,
                                row: request.row,
                                pixel_kind,
                            }));
                            continue;
                        }

                        // Claim this tile — skip if another worker is already decoding it
                        {
                            let mut set = flight.lock();
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
                                        log::debug!(
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
                                    let mut cache = crate::tile_cache::PIXEL_CACHE.lock();
                                    cache.insert(request.index, coord, pixels);
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
                                        log::debug!(
                                            "[tile-worker-{}] decode HDR tile file=\"{}\" index={} epoch={} coord=({},{}) size={}x{} took {}ms",
                                            i,
                                            source.source_name(),
                                            request.index,
                                            request.profile_epoch,
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
                                        "[tile-worker-{}] HDR tile decode failed file=\"{}\" index={} epoch={} coord=({},{}): {}",
                                        i,
                                        source.source_name(),
                                        request.index,
                                        request.profile_epoch,
                                        request.col,
                                        request.row,
                                        err
                                    );
                                    let mut set = flight.lock();
                                    set.remove(&tile_key);
                                    drop(set);
                                    let _ = tx.send(LoaderOutput::Tile(TileResult {
                                        index: request.index,
                                        decode_profile: crate::loader::decode_profile_with_epoch(
                                            request.profile_epoch,
                                        ),
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
                            let mut set = flight.lock();
                            set.remove(&tile_key);
                        }

                        // Notify main thread that tile is ready for GPU upload
                        let _ = tx.send(LoaderOutput::Tile(TileResult {
                            index: request.index,
                            decode_profile: crate::loader::decode_profile_with_epoch(
                                request.profile_epoch,
                            ),
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
        let worker_plan = Arc::clone(&preload_plan);
        let _ = std::thread::Builder::new()
            .name("refinement-worker".to_string())
            .spawn(move || {
                while let Ok(req) = refine_rx.recv() {
                    let snapshot_epoch = worker_plan.profile_epoch();
                    if req.decode_profile.profile_epoch < snapshot_epoch {
                        log::debug!(
                            "[Refinement] Skipping stale profile epoch for {:?} ({} < {})",
                            req.path.file_name().unwrap_or_default(),
                            req.decode_profile.profile_epoch,
                            snapshot_epoch
                        );
                        continue;
                    }
                    if !worker_plan.index_in_window(req.index) {
                        continue;
                    }

                    crate::preload_debug!(
                        "[PreloadDebug][RAW] refine_start idx={} hdr_cap={:.3} path={}",
                        req.index,
                        req.hdr_target_capacity,
                        req.path.display()
                    );

                    // 2. Perform HQ demosaic at full develop resolution.
                    let limit = hq_preview_max_side();
                    log::debug!(
                        "[Refinement] Starting HQ demosaic for {:?} (limit={})",
                        req.path.file_name().unwrap_or_default(),
                        limit,
                    );
                    let t0 = std::time::Instant::now();

                    let mut processor = match RawProcessor::new() {
                        Some(p) => p,
                        None => {
                            log::error!("[Refinement] Failed to create RawProcessor");
                            continue;
                        }
                    };

                    match processor.open(&req.path) {
                        Ok(()) => {}
                        Err(e) => {
                            log::error!(
                                "[Refinement] Failed to open {:?}: {}",
                                req.path.file_name().unwrap_or_default(),
                                e
                            );
                            continue;
                        }
                    }

                    let user_flip = req.orientation_override.unwrap_or(0);
                    processor.set_user_flip(user_flip);
                    if let Err(err) = processor.unpack() {
                        log::error!(
                            "[Refinement] Failed to unpack {:?}: {}",
                            req.path.file_name().unwrap_or_default(),
                            err
                        );
                        continue;
                    }

                    let develop_result = {
                        let started = std::time::Instant::now();
                        processor
                            .develop_scene_linear_hdr()
                            .and_then(|hdr| {
                                finalize_raw_hq_hdr_buffer(
                                    hdr,
                                    req.logical_width,
                                    req.logical_height,
                                )
                            })
                            .map(|hdr| (hdr, crate::loader::elapsed_ms_u32(started)))
                    };

                    match develop_result {
                        Ok((hdr, cpu_demosaic_ms)) => {
                            let elapsed = t0.elapsed();
                            let preview_w = hdr.width;
                            let preview_h = hdr.height;

                            let snapshot_epoch = worker_plan.profile_epoch();
                            if req.decode_profile.profile_epoch < snapshot_epoch {
                                log::debug!(
                                    "[Refinement] Discarding stale HQ HDR develop result for {:?} (epoch {} < {})",
                                    req.path.file_name().unwrap_or_default(),
                                    req.decode_profile.profile_epoch,
                                    snapshot_epoch
                                );
                                continue;
                            }

                            if let Some(slot) = req.hdr_developed_image.as_ref() {
                                *slot.write() = Some(hdr.clone());
                            }

                            let fb = match hdr_sdr_fallback_rgba8_eager_or_placeholder(
                                &hdr,
                                req.hdr_target_capacity,
                                &req.hdr_tone_map,
                            ) {
                                Ok(fb) => fb,
                                Err(e) => {
                                    log::error!(
                                        "[Refinement] HQ HDR SDR fallback failed for {:?}: {}",
                                        req.path.file_name().unwrap_or_default(),
                                        e
                                    );
                                    continue;
                                }
                            };
                            let preview = DecodedImage::from_hdr_sdr_fallback(
                                hdr.width,
                                hdr.height,
                                fb,
                            );
                            let tile_pixels = preview.rgba().to_vec();
                            let dynamic = match image::ImageBuffer::from_raw(
                                hdr.width,
                                hdr.height,
                                tile_pixels,
                            ) {
                                Some(buf) => DynamicImage::ImageRgba8(buf),
                                None => {
                                    log::error!(
                                        "[Refinement] Failed to build tile buffer from HQ HDR fallback"
                                    );
                                    continue;
                                }
                            };

                            {
                                let mut dev_lock = req.developed_image.write();
                                *dev_lock = Some(dynamic);
                            }

                            let bundle = PreviewBundle::refined()
                                .with_hdr(std::sync::Arc::new(hdr))
                                .with_sdr(preview);
                            let refine_osd = crate::loader::RawOsdInfo::refine_complete(
                                preview_w,
                                preview_h,
                                cpu_demosaic_ms,
                            );
                            let _ = worker_tx.send(LoaderOutput::Preview(PreviewResult {
                                index: req.index,
                                decode_profile: req.decode_profile.clone(),
                                source_key: req.source_key,
                                preview_bundle: bundle,
                                error: None,
                                cpu_demosaic_ms: Some(cpu_demosaic_ms),
                                raw_bootstrap_osd: Some(refine_osd),
                            }));
                            let _ =
                                worker_tx.send(LoaderOutput::Refined(req.index));
                            crate::preload_debug!(
                                "[PreloadDebug][RAW] refine_done idx={} mode=Hdr preview={}x{} elapsed={:.1}s path={}",
                                req.index,
                                preview_w,
                                preview_h,
                                elapsed.as_secs_f64(),
                                req.path.display()
                            );
                            log::debug!(
                                "[Refinement] HQ HDR completed {}x{} in {:.1}s",
                                preview_w,
                                preview_h,
                                elapsed.as_secs_f64()
                            );
                        }
                        Err(e) => {
                            log::error!(
                                "[Refinement] LibRaw HQ develop failed for {:?} after {:.1}s: {}",
                                req.path.file_name().unwrap_or_default(),
                                t0.elapsed().as_secs_f64(),
                                e
                            );
                        }
                    }
                }
            });

        Self {
            raw_open_prefetch,
            tx: LoaderOutputSender::new(tx),
            rx,
            loading: Arc::new(Mutex::new(HashMap::new())),
            preload_plan,
            pool: Arc::new(pool),
            tile_queue,
            refine_tx,
            local_queue: std::collections::VecDeque::new(),
            delayed_fallback,
            hdr_target_capacity_bits,
            hdr_tone_exposure_ev_bits,
            hdr_tone_sdr_white_nits_bits,
            hdr_tone_max_display_nits_bits,
            hdr_callback_upload_active,
            embedded_iso_gain_map_sdr_master,
            wgpu_device: None,
            wgpu_queue: None,
            wgpu_device_id: Arc::new(AtomicU64::new(1)),
            wgpu_is_opengl: false,
            output_mode_bits,
            current_image_os_threads: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            capacity_requeue_counts: std::collections::HashMap::new(),
        }
    }

    fn apply_wgpu_context(
        &mut self,
        device: Option<wgpu::Device>,
        queue: Option<wgpu::Queue>,
        device_id: u64,
    ) {
        self.wgpu_is_opengl = device
            .as_ref()
            .is_some_and(|d| d.adapter_info().backend == wgpu::Backend::Gl);
        self.wgpu_device = device;
        self.wgpu_queue = queue;
        self.wgpu_device_id
            .store(device_id, std::sync::atomic::Ordering::Release);
        self.bump_profile_epoch();
    }

    pub fn with_wgpu(
        mut self,
        device: Option<wgpu::Device>,
        queue: Option<wgpu::Queue>,
        device_id: u64,
    ) -> Self {
        self.apply_wgpu_context(device, queue, device_id);
        self
    }

    /// Updates worker GPU handles after a live `wgpu::Device` instance replacement.
    pub fn set_wgpu_context(
        &mut self,
        device: Option<wgpu::Device>,
        queue: Option<wgpu::Queue>,
        device_id: u64,
    ) {
        self.apply_wgpu_context(device, queue, device_id);
    }

    pub fn prefetch_raw_open(&self, path: PathBuf) {
        self.raw_open_prefetch.request(&self.pool, path);
    }

    pub fn is_loading(&self, index: usize) -> bool {
        self.loading.lock().contains_key(&index)
    }

    pub fn profile_epoch(&self) -> u64 {
        self.preload_plan.profile_epoch()
    }

    pub fn in_flight_profile(&self, index: usize) -> Option<DecodeProfile> {
        self.loading.lock().get(&index).map(|e| e.profile.clone())
    }

    pub fn set_output_mode(&self, mode: HdrOutputMode) {
        self.output_mode_bits
            .store(mode.to_storage_bits(), std::sync::atomic::Ordering::Release);
    }

    fn output_mode_snapshot(&self) -> HdrOutputMode {
        HdrOutputMode::from_storage_bits(
            self.output_mode_bits
                .load(std::sync::atomic::Ordering::Acquire),
        )
    }

    pub fn bump_profile_epoch(&self) -> u64 {
        self.preload_plan.bump_profile_epoch()
    }

    pub fn sync_preload_plan(&self, current_index: usize, image_count: usize, max_distance: usize) {
        self.preload_plan
            .write_navigation(current_index, image_count, max_distance);
    }

    /// Upgrade an in-flight neighbor registration to [`LoadIntent::Current`] without spawning
    /// a second decode worker (navigation reuse).
    pub fn promote_inflight_to_current(&mut self, index: usize) -> bool {
        let mut loading = self.loading.lock();
        let Some(entry) = loading.get_mut(&index) else {
            return false;
        };
        entry.profile.load_intent = LoadIntent::Current;
        true
    }

    pub fn cancel_indices(&mut self, indices: impl IntoIterator<Item = usize>) {
        use std::collections::HashSet;

        let cancelled: HashSet<usize> = indices.into_iter().collect();
        if cancelled.is_empty() {
            return;
        }
        {
            let mut loading = self.loading.lock();
            for idx in &cancelled {
                loading.remove(idx);
            }
        }
        {
            for idx in &cancelled {
                self.capacity_requeue_counts.remove(idx);
            }
        }
        {
            let (lock, cvar) = &*self.delayed_fallback;
            let mut slot = lock.lock();
            if slot
                .as_ref()
                .is_some_and(|job| cancelled.contains(&job.index))
            {
                *slot = None;
                cvar.notify_one();
            }
        }
    }

    const MAX_HDR_CAPACITY_REQUEUE_COUNT: u32 = 3;

    pub fn try_note_capacity_requeue(&mut self, index: usize) -> bool {
        let entry = self.capacity_requeue_counts.entry(index).or_insert(0);
        if *entry >= Self::MAX_HDR_CAPACITY_REQUEUE_COUNT {
            return false;
        }
        *entry += 1;
        true
    }

    pub fn clear_capacity_requeue(&mut self, index: usize) {
        self.capacity_requeue_counts.remove(&index);
    }

    #[cfg(test)]
    pub(crate) fn test_capacity_requeue_count(&self, index: usize) -> u32 {
        self.capacity_requeue_counts
            .get(&index)
            .copied()
            .unwrap_or(0)
    }

    pub fn set_hdr_target_capacity(&self, capacity: f32) {
        self.hdr_target_capacity_bits
            .store(capacity.to_bits(), std::sync::atomic::Ordering::Relaxed);
    }

    pub fn set_hdr_callback_upload_active(&self, active: bool) {
        self.hdr_callback_upload_active
            .store(active, std::sync::atomic::Ordering::Release);
    }

    pub fn set_embedded_iso_gain_map_sdr_master(&self, enabled: bool) {
        self.embedded_iso_gain_map_sdr_master
            .store(enabled, std::sync::atomic::Ordering::Release);
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

    pub(crate) fn hdr_tone_map_settings_snapshot(&self) -> HdrToneMapSettings {
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
        path: PathBuf,
        high_quality: bool,
        raw_demosaic_mode: crate::settings::RawDemosaicMode,
    ) {
        let load_intent = if index == self.preload_plan.current_index() {
            LoadIntent::Current
        } else {
            LoadIntent::NeighborPrefetch
        };
        let decode_profile = DecodeProfile {
            raw_high_quality: high_quality,
            raw_demosaic_mode,
            output_mode: self.output_mode_snapshot(),
            ultra_hdr_decode_capacity: self.hdr_target_capacity(),
            render_shape: crate::loader::RenderShape::Unknown,
            load_intent,
            profile_epoch: self.preload_plan.profile_epoch(),
        };
        let decode_profile_for_job = decode_profile.clone();
        let decode_profile_spawn = decode_profile_for_job.clone();
        if load_intent == LoadIntent::NeighborPrefetch {
            // Soft cap before registering the job. Two prefetches can both pass this read-only
            // check while the pool is below the limit; the insert below serializes registration
            // and `should_spawn_load_task` rejects the loser. Benign TOCTOU — do not merge the
            // locks (would hold the mutex across `spawn_decode_profile`).
            let loading_snapshot = self.loading.lock();
            if loading_snapshot.len() >= MAX_IMG_LOADER_THREADS
                && !loading_snapshot.contains_key(&index)
            {
                return;
            }
        }
        {
            let mut loading = self.loading.lock();
            if !should_spawn_load_task(&mut loading, index, decode_profile) {
                return;
            }
        }

        let claimed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let path1 = path.clone();
        let path_is_raw = crate::preload_debug::path_is_raw(&path);
        let path2 = path;
        let tx1 = self.tx.clone();
        let tx2 = self.tx.clone();
        let loading1 = Arc::clone(&self.loading);
        let loading2 = Arc::clone(&self.loading);
        let claimed1 = Arc::clone(&claimed);
        let claimed2 = Arc::clone(&claimed);
        let rtx1 = self.refine_tx.clone();
        let rtx2 = self.refine_tx.clone();
        let hdr_target_capacity = self.hdr_target_capacity();
        let hdr_tone_map = self.hdr_tone_map_settings_snapshot();
        let raw_open_prefetch = Arc::clone(&self.raw_open_prefetch);
        let wgpu_device = self.wgpu_device.clone();
        let wgpu_queue = self.wgpu_queue.clone();
        let wgpu_device_id_at_spawn = self
            .wgpu_device_id
            .load(std::sync::atomic::Ordering::Relaxed);
        let wgpu_is_opengl = self.wgpu_is_opengl;
        let wgpu_device_id_live = Arc::clone(&self.wgpu_device_id);
        let hdr_callback_upload_active_live = Arc::clone(&self.hdr_callback_upload_active);
        let embedded_iso_gain_map_sdr_master_live =
            Arc::clone(&self.embedded_iso_gain_map_sdr_master);

        if path_is_raw {
            crate::preload_debug!(
                "[PreloadDebug][RAW] request_load idx={} hq={} hdr_cap={:.3} path={}",
                index,
                high_quality,
                hdr_target_capacity,
                path1.display()
            );
        }

        let raw_open_prefetch_spawn = Arc::clone(&raw_open_prefetch);
        let wgpu_device_spawn = wgpu_device.clone();
        let wgpu_queue_spawn = wgpu_queue.clone();
        let wgpu_device_id_live_spawn = Arc::clone(&wgpu_device_id_live);
        let hdr_callback_upload_active_live_spawn = Arc::clone(&hdr_callback_upload_active_live);
        let embedded_iso_gain_map_sdr_master_live_spawn =
            Arc::clone(&embedded_iso_gain_map_sdr_master_live);
        let run_worker = move || {
            {
                let loading = loading1.lock();
                if !loading.contains_key(&index) {
                    return;
                }
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
            Self::do_load(LoadWorkerInput {
                index,
                path: path1,
                tx: tx1,
                refine_tx: rtx1,
                loading_ref: loading1,
                decode_profile: decode_profile_spawn,
                high_quality,
                raw_demosaic_mode,
                hdr_target_capacity,
                hdr_tone_map,
                raw_open_prefetch: raw_open_prefetch_spawn,
                wgpu_device: wgpu_device_spawn,
                wgpu_queue: wgpu_queue_spawn,
                wgpu_device_id_at_spawn,
                wgpu_is_opengl,
                wgpu_device_id_live: wgpu_device_id_live_spawn,
                hdr_callback_upload_active_live: hdr_callback_upload_active_live_spawn,
                embedded_iso_gain_map_sdr_master_live: embedded_iso_gain_map_sdr_master_live_spawn,
            });
        };
        if load_intent == LoadIntent::Current {
            // Soft cap before fetch_add. Two current-image loads can both pass this read-only
            // check while the counter is below the limit; both then fetch_add. Benign TOCTOU --
            // same pattern as the neighbor prefetch gate above.
            let os_thread_cap_reached = self
                .current_image_os_threads
                .load(std::sync::atomic::Ordering::Acquire)
                >= MAX_CURRENT_IMAGE_OS_THREADS;
            if os_thread_cap_reached {
                log::debug!(
                    "[Loader] current-image OS thread cap ({MAX_CURRENT_IMAGE_OS_THREADS}) reached; using pool"
                );
                self.pool.spawn(run_worker);
            } else {
                self.current_image_os_threads
                    .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                let thread_name = format!("img-loader-current-{index}");
                // Keep the worker in an Arc so a failed OS-thread spawn can fall back to the pool
                // instead of dropping the closure (which would leave only the 50ms delayed fallback).
                let worker = Arc::new(Mutex::new(Some(run_worker)));
                let worker_for_thread = Arc::clone(&worker);
                let os_threads_live = Arc::clone(&self.current_image_os_threads);
                let spawn_result = {
                    #[cfg(target_os = "windows")]
                    {
                        std::thread::Builder::new().name(thread_name).spawn(move || {
                            let _guard = CurrentImageOsThreadGuard(os_threads_live);
                            match crate::wic::ComGuard::new() {
                                Ok(_com) => {
                                    if let Some(w) = worker_for_thread.lock().take() {
                                        w();
                                    }
                                }
                                Err(e) => {
                                    log::error!(
                                        "Failed to initialize COM on current-image loader thread: {e:?}"
                                    );
                                    if let Some(w) = worker_for_thread.lock().take() {
                                        w();
                                    }
                                }
                            }
                        })
                    }
                    #[cfg(not(target_os = "windows"))]
                    {
                        std::thread::Builder::new()
                            .name(thread_name)
                            .spawn(move || {
                                let _guard = CurrentImageOsThreadGuard(os_threads_live);
                                if let Some(w) = worker_for_thread.lock().take() {
                                    w();
                                }
                            })
                    }
                };
                match spawn_result {
                    Ok(_) => {}
                    Err(e) => {
                        self.current_image_os_threads
                            .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
                        log::error!(
                            "[Loader] Failed to spawn current-image thread: {e}, falling back to pool"
                        );
                        if let Some(w) = worker.lock().take() {
                            self.pool.spawn(w);
                        }
                    }
                }
            }
        } else {
            self.pool.spawn(run_worker);
        }

        // Fallback: one shared worker sleeps 50ms then tries `do_load` if the pool task
        // did not claim first. Pending jobs are coalesced to a single slot (no per-request OS thread).
        let delayed_job = DelayedFallbackJob {
            index,
            decode_profile: decode_profile_for_job,
            path: path2,
            high_quality,
            raw_demosaic_mode,
            claimed: claimed2,
            loading: loading2,
            tx: tx2,
            refine_tx: rtx2,
            hdr_target_capacity,
            hdr_tone_map,
            raw_open_prefetch,
            wgpu_device,
            wgpu_queue,
            wgpu_device_id_at_spawn,
            wgpu_is_opengl,
            wgpu_device_id_live,
            hdr_callback_upload_active_live,
            embedded_iso_gain_map_sdr_master_live,
        };
        {
            let (lock, cvar) = &*self.delayed_fallback;
            let mut slot = lock.lock();
            *slot = Some(delayed_job);
            cvar.notify_one();
        }
    }

    /// True when [`ImageLoader::loading`] shows a **strictly newer** registered profile for
    /// `index` than the adoption profile from the decode / refinement worker.
    ///
    /// Prefetch promotion can accept in-flight previews whose profile still matches display
    /// requirements; `finish_image_request` clearing the map does not alone imply supersession.
    #[inline]
    fn hq_refinement_superseded(
        loading: &Arc<Mutex<HashMap<usize, InFlightLoad>>>,
        index: usize,
        adoptee_profile: &DecodeProfile,
    ) -> bool {
        loading.lock().get(&index).is_some_and(|registered| {
            in_flight_profile_supersedes_hq_refinement(adoptee_profile, &registered.profile)
        })
    }

    /// True when the registered in-flight profile no longer matches this worker's spawn profile.
    fn load_result_superseded(
        loading: &Arc<Mutex<HashMap<usize, InFlightLoad>>>,
        index: usize,
        spawn_profile: &DecodeProfile,
    ) -> bool {
        loading.lock().get(&index).is_some_and(|registered| {
            in_flight_profile_supersedes_load_result(spawn_profile, &registered.profile)
        })
    }

    fn do_load(input: LoadWorkerInput) {
        let LoadWorkerInput {
            index,
            path,
            tx,
            refine_tx,
            loading_ref,
            decode_profile,
            high_quality,
            raw_demosaic_mode,
            hdr_target_capacity,
            hdr_tone_map,
            raw_open_prefetch,
            wgpu_device,
            wgpu_queue,
            wgpu_device_id_at_spawn,
            wgpu_is_opengl,
            wgpu_device_id_live,
            hdr_callback_upload_active_live,
            embedded_iso_gain_map_sdr_master_live,
        } = input;
        // Adoption logic: We no longer abort if global_gen has changed.
        // As long as our index is still in the loading map, we continue.
        {
            let loading = loading_ref.lock();
            if !loading.contains_key(&index) {
                return;
            }
        }

        let decode_profile_for_load = decode_profile.clone();
        let mut load_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            load_image_file(ImageLoadRequest {
                index,
                path: &path,
                tx: tx.clone(),
                refine_tx: refine_tx.clone(),
                decode_profile: decode_profile_for_load,
                high_quality,
                raw_demosaic_mode,
                hdr_target_capacity,
                hdr_tone_map,
                raw_open_prefetch: Some(raw_open_prefetch.as_ref()),
                prefer_embedded_sdr_master: embedded_iso_gain_map_sdr_master_live
                    .load(std::sync::atomic::Ordering::Acquire),
            })
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
                decode_profile: decode_profile.clone(),
                source_key: source_key_for_path(&path),
                result: Err(format!("Decoder Panic: {}", msg)),
                preview_bundle: PreviewBundle::initial(),
                ultra_hdr_capacity_sensitive: false,
                sdr_fallback_is_placeholder: false,
                target_hdr_capacity: hdr_target_capacity,
                raw_osd: None,
                uploaded_planes: None,
                device_id: None,
            }
        });

        if let Err(ref e) = load_result.result {
            log::error!("[Loader] Load FAILED for index={}: {}", index, e);
        }
        #[cfg(feature = "preload-debug")]
        if crate::preload_debug::path_is_raw(&path)
            && let Ok(ref image_data) = load_result.result
        {
            crate::preload_debug!(
                "[PreloadDebug][RAW] load_done idx={} hq={} hdr_cap={:.3} result={} path={}",
                index,
                high_quality,
                hdr_target_capacity,
                crate::preload_debug::summarize_image_data(image_data),
                path.display()
            );
        }

        if Self::load_result_superseded(&loading_ref, index, &decode_profile) {
            return;
        }

        // Stamp with the spawn profile, not the live registered profile — a downgrade can
        // replace `loading[idx]` while this worker still holds a higher decode product.
        let mut result_profile = decode_profile.clone();
        if let Ok(data) = &load_result.result {
            result_profile.render_shape = data.preferred_render_shape();
        }
        load_result.decode_profile = result_profile;

        {
            let loading = loading_ref.lock();
            if !loading.contains_key(&index) {
                return;
            }
        }

        if !wgpu_is_opengl
            && wgpu_device_id_at_spawn
                == wgpu_device_id_live.load(std::sync::atomic::Ordering::Acquire)
            && let (Some(device), Some(queue)) = (&wgpu_device, &wgpu_queue)
            && let Ok(ImageData::Hdr { ref hdr, .. }) = load_result.result
            && static_hdr_background_plane_upload_eligible(
                hdr,
                hdr_target_capacity,
                hdr_callback_upload_active_live.load(std::sync::atomic::Ordering::Acquire),
                embedded_iso_gain_map_sdr_master_live.load(std::sync::atomic::Ordering::Acquire),
            )
        {
            match crate::hdr::renderer::upload_image_plane(device, queue, hdr) {
                Ok(uploaded) => {
                    load_result.uploaded_planes = Some(uploaded);
                    load_result.device_id = Some(wgpu_device_id_at_spawn);
                }
                Err(err) => {
                    log::debug!(
                        "[Loader] Background HDR plane upload skipped for index={}: {err}",
                        index
                    );
                }
            }
        }

        // Tiled HQ preview: only `Arc::clone` the source; `load_result` moves to the channel once
        // (avoids cloning full Static/Animated pixel buffers).
        if let Ok(ref image_data) = load_result.result {
            let result_profile = load_result.decode_profile.clone();
            let sdr_source = image_data.tiled_sdr_source().cloned();
            let hdr_source = image_data.tiled_hdr_source().cloned();
            let tx_cloned = tx.clone();
            let loading_for_hq = Arc::clone(&loading_ref);
            match (hdr_source, sdr_source) {
                (Some(source), _) => {
                    if source.defers_loader_hq_preview() {
                        crate::preload_debug!(
                            "[PreloadDebug][RAW] skip_loader_hq_hdr_preview idx={} reason=async_raw_refinement",
                            index,
                        );
                    } else {
                        let file_name = path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("unknown")
                            .to_string();
                        REFINEMENT_POOL.spawn(move || {
                        if Self::hq_refinement_superseded(&loading_for_hq, index, &result_profile) {
                            return;
                        }

                        #[cfg(target_os = "windows")]
                        let _com = crate::wic::ComGuard::new();

                        let limit = hq_preview_max_side();
                        let started_at = std::time::Instant::now();
                        let is_hdr_mode = !hdr_display_requests_sdr_preview(hdr_target_capacity);
                        log::debug!(
                            "[Loader] [{}] HQ preview start: index={} limit={} source={}x{} (hdr_mode={})",
                            file_name,
                            index,
                            limit,
                            source.width(),
                            source.height(),
                            is_hdr_mode
                        );
                        let r_result =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<_, String> {
                                if is_hdr_mode {
                                    let hdr = source.generate_hdr_preview(limit, limit)?;
                                    Ok((Some(hdr), None))
                                } else {
                                    let sdr = source.generate_sdr_preview(limit, limit)?;
                                    Ok((None, Some(sdr)))
                                }
                            }));

                        match r_result {
                            Ok(Ok((hdr, sdr))) => {
                                if Self::hq_refinement_superseded(&loading_for_hq, index, &result_profile) {
                                    log::debug!(
                                        "[Loader] [{}] HQ preview discarded as stale: index={} elapsed={:?}",
                                        file_name,
                                        index,
                                        started_at.elapsed()
                                    );
                                    return;
                                }
                                let (pw, ph) = if let Some(ref h) = hdr {
                                    (h.width, h.height)
                                } else if let Some(ref s) = sdr {
                                    (s.0, s.1)
                                } else {
                                    (0, 0)
                                };
                                let preview_kind = if is_hdr_mode { "HDR" } else { "SDR" };
                                log::debug!(
                                    "[Loader] [{}] HQ {} preview generated: {}x{} (source {}x{}, limit={}, elapsed={:?})",
                                    file_name,
                                    preview_kind,
                                    pw,
                                    ph,
                                    source.width(),
                                    source.height(),
                                    limit,
                                    started_at.elapsed()
                                );
                                let mut bundle = PreviewBundle::refined();
                                if let Some(h) = hdr {
                                    bundle = bundle.with_hdr(Arc::new(h));
                                }
                                if let Some(s) = sdr {
                                    bundle = bundle.with_sdr(DecodedImage::new(s.0, s.1, s.2));
                                }

                                let _ = tx_cloned.send(LoaderOutput::Preview(PreviewResult {
                                    index,
                                    decode_profile: result_profile.clone(),
                                    source_key: load_result.source_key,
                                    preview_bundle: bundle,
                                    error: None,
                                    cpu_demosaic_ms: None,
                                    raw_bootstrap_osd: None,
                                }));
                            }
                            Ok(Err(e)) => {
                                log::error!(
                                    "[Loader] [{}] High-quality HDR preview failed: index={} limit={} elapsed={:?}: {e}",
                                    file_name,
                                    index,
                                    limit,
                                    started_at.elapsed()
                                );
                            }
                            Err(e) => {
                                log::error!(
                                    "[Loader] [{}] High-quality HDR preview PANICKED: index={} limit={} elapsed={:?}: {:?}",
                                    file_name,
                                    index,
                                    limit,
                                    started_at.elapsed(),
                                    e
                                );
                            }
                        }
                    });
                    }
                }
                (None, Some(source)) => {
                    if source.defers_loader_hq_preview() {
                        crate::preload_debug!(
                            "[PreloadDebug][RAW] skip_loader_hq_preview idx={} reason=async_raw_refinement",
                            index,
                        );
                    } else {
                        let loading_sdr_hq = Arc::clone(&loading_ref);
                        REFINEMENT_POOL.spawn(move || {
                            if Self::hq_refinement_superseded(
                                &loading_sdr_hq,
                                index,
                                &result_profile,
                            ) {
                                return;
                            }

                            #[cfg(target_os = "windows")]
                            let _com = crate::wic::ComGuard::new();

                            let limit = hq_preview_max_side();
                            let r_result =
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    source.generate_full_image_preview(limit, limit)
                                }));

                            match r_result {
                                Ok((pw, ph, p_pixels)) if pw > 0 && ph > 0 => {
                                    if Self::hq_refinement_superseded(
                                        &loading_sdr_hq,
                                        index,
                                        &result_profile,
                                    ) {
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
                                            result_profile.clone(),
                                            load_result.source_key,
                                            Ok(DecodedImage::new(pw, ph, p_pixels)),
                                        ),
                                    ));
                                }
                                Err(e) => {
                                    log::error!(
                                        "[Loader] High-quality refinement PANICKED: {:?}",
                                        e
                                    );
                                }
                                _ => {}
                            }
                        });
                    }
                }
                (None, None) => {
                    if Self::load_result_superseded(&loading_ref, index, &decode_profile) {
                        return;
                    }
                    let _ = tx.send(LoaderOutput::Image(Box::new(load_result)));
                    return;
                }
            }
        }

        if Self::load_result_superseded(&loading_ref, index, &decode_profile) {
            return;
        }
        let _ = tx.send(LoaderOutput::Image(Box::new(load_result)));
    }

    /// Regenerate an HQ SDR preview for a tiled source when bootstrap-only remains in cache.
    pub fn trigger_hq_tiled_sdr_preview(
        &self,
        index: usize,
        source: Arc<dyn crate::loader::TiledImageSource>,
        decode_profile: DecodeProfile,
        source_key: u64,
    ) {
        if source.defers_loader_hq_preview() {
            return;
        }
        let tx = self.tx.clone();
        REFINEMENT_POOL.spawn(move || {
            #[cfg(target_os = "windows")]
            let _com = crate::wic::ComGuard::new();

            let limit = hq_preview_max_side();
            let r_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                source.generate_full_image_preview(limit, limit)
            }));

            match r_result {
                Ok((pw, ph, p_pixels)) if pw > 0 && ph > 0 => {
                    log::debug!(
                        "[Loader] On-demand HQ preview generated: {}x{} (source {}x{}) idx={}",
                        pw,
                        ph,
                        source.width(),
                        source.height(),
                        index,
                    );
                    let _ = tx.send(LoaderOutput::Preview(PreviewResult::from_sdr_preview(
                        index,
                        decode_profile,
                        source_key,
                        Ok(DecodedImage::new(pw, ph, p_pixels)),
                    )));
                }
                Err(e) => {
                    log::error!(
                        "[Loader] On-demand HQ preview PANICKED idx={}: {:?}",
                        index,
                        e
                    );
                }
                _ => {}
            }
        });
    }

    pub fn trigger_hdr_sdr_fallback_refinement(
        &self,
        index: usize,
        hdr: std::sync::Arc<crate::hdr::types::HdrImageBuffer>,
        source_key: u64,
    ) {
        let adoptee_profile = self
            .in_flight_profile(index)
            .unwrap_or_else(decode_profile_stub);
        let tx = self.tx.clone();
        let loading = std::sync::Arc::clone(&self.loading);
        let tone = self.hdr_tone_map_settings_snapshot();
        let fallback_profile = adoptee_profile.clone();

        REFINEMENT_POOL.spawn(move || {
            struct RefinementGuard {
                tx: super::types::LoaderOutputSender,
                index: usize,
                decode_profile: DecodeProfile,
                source_key: u64,
                sent: bool,
            }
            impl Drop for RefinementGuard {
                fn drop(&mut self) {
                    if !self.sent {
                        let _ = self.tx.send(LoaderOutput::HdrSdrFallback(HdrSdrFallbackResult {
                            index: self.index,
                            decode_profile: self.decode_profile.clone(),
                            source_key: self.source_key,
                            fallback: None,
                        }));
                    }
                }
            }

            let mut guard = RefinementGuard {
                tx: tx.clone(),
                index,
                decode_profile: fallback_profile,
                source_key,
                sent: false,
            };

            if Self::hq_refinement_superseded(&loading, index, &guard.decode_profile) {
                return;
            }
            #[cfg(target_os = "windows")]
            let _com = crate::wic::ComGuard::new();

            let started_at = std::time::Instant::now();
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                crate::loader::hdr_fallback::hdr_to_sdr_with_user_tone(&hdr, &tone)
            }));
            match r {
                Ok(Ok(pixels)) => {
                    if Self::hq_refinement_superseded(&loading, index, &guard.decode_profile) {
                        log::debug!(
                            "[Loader] HDR SDR fallback refinement discarded (stale): index={index}"
                        );
                        return;
                    }
                    log::debug!(
                        "[Loader] HDR SDR fallback refined after placeholder: index={index} elapsed={:?}",
                        started_at.elapsed()
                    );
                    let fallback = DecodedImage::new(hdr.width, hdr.height, pixels);
                    guard.sent = true;
                    let _ = tx.send(LoaderOutput::HdrSdrFallback(HdrSdrFallbackResult {
                        index,
                        decode_profile: guard.decode_profile.clone(),
                        source_key,
                        fallback: Some(fallback),
                    }));
                }
                Ok(Err(e)) => {
                    log::warn!(
                        "[Loader] HDR SDR fallback refinement failed: index={index}: {e}"
                    );
                }
                Err(payload) => {
                    log::error!(
                        "[Loader] HDR SDR fallback refinement panicked: index={index}: {:?}",
                        payload
                    );
                }
            }
        });
    }
}
