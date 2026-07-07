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
    DelayedFallbackJob, FALLBACK_DEBOUNCE, ImageLoader, LOADER_OUTPUT_CHANNEL_CAPACITY,
    LOADER_WORKER_IDLE_POLL, LoaderOutputSender, LoaderWorkerLifetime,
    REFINEMENT_REQUEST_CHANNEL_CAPACITY, TileInFlightKey, TileRequest, should_spawn_load_task,
};

use crate::hdr::types::HdrOutputMode;
use crate::hdr::types::HdrToneMapSettings;
use crate::loader::decode::{ImageLoadRequest, load_image_file};
use crate::loader::preview_caps::{REFINEMENT_POOL, finalize_raw_hq_hdr_buffer};
use crate::loader::{
    DecodeProfile, DecodedImage, ImageData, InFlightLoad, LoadIntent, LoadResult, LoaderOutput,
    MAX_CURRENT_IMAGE_OS_THREADS, MAX_IMG_LOADER_THREADS, PreviewBundle, PreviewResult,
    RawDevelopedImageRank, RefinementRequest, TileDecodeSource, TileResult,
    hdr_display_requests_sdr_preview, hdr_sdr_fallback_rgba8_or_placeholder, hq_preview_max_side,
    in_flight_profile_supersedes_hq_refinement, in_flight_profile_supersedes_load_result,
    source_key_for_path, static_hdr_background_plane_upload_eligible,
};
use crate::raw_processor::RawProcessor;
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use image::DynamicImage;
use parking_lot::{Condvar, Mutex};

use std::collections::{BinaryHeap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

#[cfg(feature = "preload-debug")]
fn preload_debug_hq_refinement_skip(
    index: usize,
    when: &'static str,
    worker_profile: &DecodeProfile,
    loading: &Arc<Mutex<HashMap<usize, InFlightLoad>>>,
    path_label: &str,
) {
    let inflight_epoch = loading
        .lock()
        .get(&index)
        .map(|entry| entry.profile.profile_epoch);
    crate::preload_debug!(
        "[PreloadDebug][Refine] skip idx={} when={} worker_epoch={} inflight_epoch={:?} path={}",
        index,
        when,
        worker_profile.profile_epoch,
        inflight_epoch,
        path_label,
    );
}

#[cfg(not(feature = "preload-debug"))]
#[inline]
fn preload_debug_hq_refinement_skip(
    _index: usize,
    _when: &'static str,
    _worker_profile: &DecodeProfile,
    _loading: &Arc<Mutex<HashMap<usize, InFlightLoad>>>,
    _path_label: &str,
) {
}

fn send_raw_hq_refined_notifications(
    worker_tx: &LoaderOutputSender,
    req: &RefinementRequest,
    preview_bundle: PreviewBundle,
    preview_w: u32,
    preview_h: u32,
    cpu_demosaic_ms: u32,
    sdr_texture_tag: Option<crate::loader::TexturePreviewBufferTag>,
) {
    let refine_osd =
        crate::loader::RawOsdInfo::refine_complete(preview_w, preview_h, cpu_demosaic_ms);
    let _ = worker_tx.send(LoaderOutput::Preview(PreviewResult {
        index: req.index,
        decode_profile: req.decode_profile.clone(),
        source_key: req.source_key,
        preview_bundle,
        error: None,
        cpu_demosaic_ms: Some(cpu_demosaic_ms),
        raw_bootstrap_osd: Some(refine_osd),
        sdr_texture_tag,
    }));
    let _ = worker_tx.send(LoaderOutput::Refined {
        index: req.index,
        source_key: req.source_key,
    });
}

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
    hdr_pending_gpu_writes: Option<Arc<crate::hdr::renderer::HdrPendingWorkQueues>>,
}

impl ImageLoader {
    pub fn new() -> Self {
        let shutdown = Arc::new(AtomicBool::new(false));
        let (output_tx, rx) = crossbeam_channel::bounded(LOADER_OUTPUT_CHANNEL_CAPACITY);
        let (refine_tx, refine_rx): (Sender<RefinementRequest>, Receiver<RefinementRequest>) =
            crossbeam_channel::bounded(REFINEMENT_REQUEST_CHANNEL_CAPACITY);
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
        let tx = LoaderOutputSender::with_shutdown_and_plan(
            output_tx,
            Arc::clone(&shutdown),
            Some(Arc::clone(&preload_plan)),
        );
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
        let tile_queue: Arc<(Mutex<BinaryHeap<TileRequest>>, Condvar)> =
            Arc::new((Mutex::new(BinaryHeap::new()), Condvar::new()));
        let raw_open_prefetch = Arc::new(super::raw_prefetch::RawOpenPrefetch::new(Arc::clone(
            &shutdown,
        )));
        let (worker_lifetime, refine_tx) = LoaderWorkerLifetime::new(
            Arc::clone(&shutdown),
            refine_tx,
            Arc::clone(&delayed_fallback),
            Arc::clone(&tile_queue),
            Arc::clone(&raw_open_prefetch),
        );
        let worker_lifetime = Arc::new(worker_lifetime);
        {
            let state = Arc::clone(&delayed_fallback);
            let shutdown_worker = Arc::clone(&shutdown);
            let workers = Arc::clone(&worker_lifetime);
            if let Ok(handle) = std::thread::Builder::new()
                .name("loader-fallback".to_string())
                .spawn(move || {
                    let (lock, cvar) = &*state;
                    loop {
                        if shutdown_worker.load(Ordering::Acquire) {
                            break;
                        }
                        let mut job = {
                            let mut g = lock.lock();
                            loop {
                                while g.is_none() {
                                    if shutdown_worker.load(Ordering::Acquire) {
                                        return;
                                    }
                                    cvar.wait_for(&mut g, LOADER_WORKER_IDLE_POLL);
                                    if shutdown_worker.load(Ordering::Acquire) {
                                        return;
                                    }
                                }
                                if let Some(j) = g.take() {
                                    break j;
                                }
                            }
                        };
                        loop {
                            if shutdown_worker.load(Ordering::Acquire) {
                                return;
                            }
                            let mut g = lock.lock();
                            let wait_result = cvar.wait_for(&mut g, FALLBACK_DEBOUNCE);
                            if shutdown_worker.load(Ordering::Acquire) {
                                return;
                            }
                            if let Some(newer) = g.take() {
                                job = newer;
                                drop(g);
                                continue;
                            }
                            drop(g);
                            if wait_result.timed_out() {
                                break;
                            }
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
                            hdr_pending_gpu_writes: job.hdr_pending_gpu_writes.clone(),
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
                })
            {
                workers.register_worker(handle);
            }
        }

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
            let shutdown_worker = Arc::clone(&shutdown);
            let workers = Arc::clone(&worker_lifetime);

            if let Ok(handle) = std::thread::Builder::new()
                .name(format!("tile-worker-{}", i))
                .spawn(move || {
                    #[cfg(target_os = "windows")]
                    let _com = crate::wic::ComGuard::new();

                    loop {
                        if shutdown_worker.load(Ordering::Acquire) {
                            break;
                        }
                        let request = {
                            let (lock, cvar) = &*queue;
                            let mut heap = lock.lock();
                            while heap.is_empty() {
                                if shutdown_worker.load(Ordering::Acquire) {
                                    return;
                                }
                                cvar.wait_for(&mut heap, LOADER_WORKER_IDLE_POLL);
                                if shutdown_worker.load(Ordering::Acquire) {
                                    return;
                                }
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

                        let Some(tile_rect) = request.source.tile_rect(request.col, request.row) else {
                            continue;
                        };
                        let x = tile_rect.x;
                        let y = tile_rect.y;
                        let tw = tile_rect.width;
                        let th = tile_rect.height;

                        let already_cached = match &request.source {
                            TileDecodeSource::Sdr(_) => {
                                let coord = crate::tile_cache::TileCoord {
                                    col: request.col,
                                    row: request.row,
                                };
                                crate::tile_cache::PIXEL_CACHE
                                    .read()
                                    .contains_tile(request.index, coord)
                            }
                            TileDecodeSource::Hdr(source) => {
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
                                    let mut cache = crate::tile_cache::PIXEL_CACHE.write();
                                    cache.insert(request.index, coord, pixels);
                                }
                            }
                            TileDecodeSource::Hdr(source) => {
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
            {
                workers.register_worker(handle);
            }
        }

        // Start dedicated Background Refinement Worker (Throttled)
        let worker_tx = tx.clone();
        let worker_plan = Arc::clone(&preload_plan);
        let shutdown_worker = Arc::clone(&shutdown);
        let workers = Arc::clone(&worker_lifetime);
        if let Ok(handle) = std::thread::Builder::new()
            .name("refinement-worker".to_string())
            .spawn(move || {
                loop {
                    if shutdown_worker.load(Ordering::Acquire) {
                        break;
                    }
                    match refine_rx.recv_timeout(LOADER_WORKER_IDLE_POLL) {
                        Ok(req) => {
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

                            if *req.developed_image_rank.read()
                                == RawDevelopedImageRank::FullResolutionDeveloped
                            {
                                crate::preload_debug!(
                                    "[PreloadDebug][RAW] refine_skip idx={} reason=already_developed path={}",
                                    req.index,
                                    req.path.display()
                                );
                                continue;
                            }
                            if let Some(slot) = req.hdr_developed_image.as_ref()
                                && slot.read().is_some()
                            {
                                crate::preload_debug!(
                                    "[PreloadDebug][RAW] refine_skip idx={} reason=hdr_tiled_already_developed path={}",
                                    req.index,
                                    req.path.display()
                                );
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
                                        // HdrTiled RAW uses the HDR tile source for both native HDR and
                                        // SDR tone-mapped output. Do not build a full-frame CPU SDR
                                        // fallback here: render shape is size-driven, and large RAWs must
                                        // stay tiled while exposure remains live in the HDR shader path.
                                        let bundle =
                                            PreviewBundle::refined().with_hdr(Arc::new(hdr));
                                        send_raw_hq_refined_notifications(
                                            &worker_tx,
                                            &req,
                                            bundle,
                                            preview_w,
                                            preview_h,
                                            cpu_demosaic_ms,
                                            None,
                                        );
                                        crate::preload_debug!(
                                            "[PreloadDebug][RAW] refine_done idx={} mode=HdrTiled preview={}x{} elapsed={:.1}s path={}",
                                            req.index,
                                            preview_w,
                                            preview_h,
                                            elapsed.as_secs_f64(),
                                            req.path.display()
                                        );
                                        log::debug!(
                                            "[Refinement] HQ HDR tiled completed {}x{} in {:.1}s",
                                            preview_w,
                                            preview_h,
                                            elapsed.as_secs_f64()
                                        );
                                        continue;
                                    }

                                    let fb = match hdr_sdr_fallback_rgba8_or_placeholder(&hdr) {
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
                                    let mut preview = DecodedImage::from_hdr_sdr_fallback(
                                        hdr.width,
                                        hdr.height,
                                        fb,
                                    );
                                    let tile_pixels = preview.take_rgba_owned();
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
                                    if preview.rgba().is_empty() {
                                        preview = DecodedImage::from(dynamic.to_rgba8());
                                    }

                                    {
                                        let mut dev_lock = req.developed_image.write();
                                        *dev_lock = Some(dynamic);
                                    }
                                    *req.developed_image_rank.write() =
                                        RawDevelopedImageRank::FullResolutionDeveloped;

                                    let bundle = PreviewBundle::refined()
                                        .with_hdr(Arc::new(hdr))
                                        .with_sdr(preview);
                                    send_raw_hq_refined_notifications(
                                        &worker_tx,
                                        &req,
                                        bundle,
                                        preview_w,
                                        preview_h,
                                        cpu_demosaic_ms,
                                        Some(
                                            crate::loader::TexturePreviewBufferTag::TiledRefinedLoader,
                                        ),
                                    );
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
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
            })
        {
            workers.register_worker(handle);
        }

        Self {
            worker_lifetime,
            refine_tx,
            raw_open_prefetch,
            tx,
            rx,
            loading: Arc::new(Mutex::new(HashMap::new())),
            preload_plan,
            pool: Arc::new(pool),
            tile_queue,
            local_queue: std::collections::VecDeque::new(),
            delayed_fallback,
            hdr_target_capacity_bits,
            hdr_tone_exposure_ev_bits,
            hdr_tone_sdr_white_nits_bits,
            hdr_tone_max_display_nits_bits,
            hdr_callback_upload_active,
            embedded_iso_gain_map_sdr_master,
            hdr_pending_gpu_writes: None,
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

    pub fn with_hdr_pending_gpu_writes(
        mut self,
        queues: Arc<crate::hdr::renderer::HdrPendingWorkQueues>,
    ) -> Self {
        self.hdr_pending_gpu_writes = Some(queues);
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

    pub(crate) fn wgpu_device_handle(&self) -> Option<&wgpu::Device> {
        self.wgpu_device.as_ref()
    }

    pub(crate) fn preview_tone_map_wgpu_context(
        &self,
    ) -> (Option<wgpu::Device>, Option<wgpu::Queue>, u64) {
        (
            self.wgpu_device.clone(),
            self.wgpu_queue.clone(),
            self.wgpu_device_id
                .load(std::sync::atomic::Ordering::Acquire),
        )
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

    /// Cancel in-flight loads whose index falls outside the preload window.
    ///
    /// Scans only [`Self::loading`] (typically a handful of entries), not the full image list.
    pub fn cancel_outside_prefetch_window(
        &mut self,
        current_index: usize,
        image_count: usize,
        max_distance: usize,
    ) {
        if image_count == 0 {
            return;
        }
        let cancelled: Vec<usize> = {
            let loading = self.loading.lock();
            loading
                .keys()
                .copied()
                .filter(|&idx| {
                    super::preload_plan::index_outside_prefetch_window(
                        current_index,
                        image_count,
                        idx,
                        max_distance,
                    )
                })
                .collect()
        };
        if !cancelled.is_empty() {
            self.cancel_indices(cancelled);
        }
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
        {
            let mut loading = self.loading.lock();
            if !should_spawn_load_task(&mut loading, index, decode_profile) {
                return;
            }
        }

        if let Err(e) = crate::mmap_util::reject_if_image_file_too_small(&path) {
            log::debug!(
                "[Loader] Rejecting load for index={}: {} path={}",
                index,
                e,
                path.display()
            );
            let load_result = LoadResult {
                index,
                decode_profile: decode_profile_for_job.clone(),
                source_key: source_key_for_path(&path),
                result: Err(e),
                preview_bundle: PreviewBundle::initial(),
                ultra_hdr_capacity_sensitive: false,
                sdr_fallback_is_placeholder: false,
                target_hdr_capacity: self.hdr_target_capacity(),
                raw_osd: None,
                uploaded_planes: None,
                device_id: None,
                staged_gpu_plane_upload: false,
            };
            let _ = self.tx.try_send(LoaderOutput::Image(Box::new(load_result)));
            self.loading.lock().remove(&index);
            return;
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
        let rtx1 = self.clone_refine_tx().expect("refinement channel closed");
        let rtx2 = self.clone_refine_tx().expect("refinement channel closed");
        let hdr_target_capacity = self.hdr_target_capacity();
        let hdr_tone_map = self.hdr_tone_map_settings_snapshot();
        let raw_open_prefetch = Arc::clone(&self.raw_open_prefetch);
        let hdr_pending_gpu_writes = self.hdr_pending_gpu_writes.clone();
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
        let hdr_pending_gpu_writes_spawn = hdr_pending_gpu_writes.clone();
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
                hdr_pending_gpu_writes: hdr_pending_gpu_writes_spawn,
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
            let mut acquired_os_thread = false;
            loop {
                let current = self
                    .current_image_os_threads
                    .load(std::sync::atomic::Ordering::Acquire);
                if current >= MAX_CURRENT_IMAGE_OS_THREADS {
                    break;
                }
                if self
                    .current_image_os_threads
                    .compare_exchange_weak(
                        current,
                        current + 1,
                        std::sync::atomic::Ordering::AcqRel,
                        std::sync::atomic::Ordering::Acquire,
                    )
                    .is_ok()
                {
                    acquired_os_thread = true;
                    break;
                }
            }
            if !acquired_os_thread {
                log::debug!(
                    "[Loader] current-image OS thread cap ({MAX_CURRENT_IMAGE_OS_THREADS}) reached; using pool"
                );
                self.pool.spawn(run_worker);
            } else {
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

        // Fallback: one shared worker waits 50ms (condvar) then tries `do_load` if the pool task
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
            hdr_pending_gpu_writes,
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
            hdr_pending_gpu_writes,
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
        let wgpu_device_for_preview = wgpu_device.clone();
        let wgpu_queue_for_preview = wgpu_queue.clone();
        let mut load_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::hdr::renderer::with_preview_tone_map_gpu(
                wgpu_device_for_preview,
                wgpu_queue_for_preview,
                wgpu_device_id_at_spawn,
                || {
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
                },
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
                staged_gpu_plane_upload: false,
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
            crate::preload_debug!(
                "[PreloadDebug][Refine] load_abort_before_hq idx={} reason=load_result_superseded epoch={} path={}",
                index,
                decode_profile.profile_epoch,
                path.display()
            );
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
                crate::preload_debug!(
                    "[PreloadDebug][Refine] load_abort_before_hq idx={} reason=loading_map_missing epoch={} path={}",
                    index,
                    load_result.decode_profile.profile_epoch,
                    path.display()
                );
                return;
            }
        }

        if !wgpu_is_opengl
            && wgpu_device_id_at_spawn
                == wgpu_device_id_live.load(std::sync::atomic::Ordering::Acquire)
            && let Some(device) = &wgpu_device
            && let Some(pending_work) = hdr_pending_gpu_writes.as_ref()
            && let Ok(ImageData::Hdr { ref hdr, .. }) = load_result.result
            && static_hdr_background_plane_upload_eligible(
                hdr,
                hdr_target_capacity,
                hdr_callback_upload_active_live.load(std::sync::atomic::Ordering::Acquire),
                embedded_iso_gain_map_sdr_master_live.load(std::sync::atomic::Ordering::Acquire),
            )
        {
            match crate::hdr::renderer::loader_background_upload_image_plane(
                device,
                pending_work.as_ref(),
                hdr,
            ) {
                Ok(Some(uploaded)) => {
                    load_result.uploaded_planes = Some(uploaded);
                    load_result.staged_gpu_plane_upload = true;
                    load_result.device_id = Some(wgpu_device_id_at_spawn);
                }
                Ok(None) => {
                    log::debug!(
                        "[Loader] Background HDR plane upload deferred (in-flight cap) for index={index}"
                    );
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
                        let wgpu_device_hq = wgpu_device.clone();
                        let wgpu_queue_hq = wgpu_queue.clone();
                        let refine_limit = hq_preview_max_side();
                        let _refine_epoch = result_profile.profile_epoch;
                        let _refine_source_w = source.width();
                        let _refine_source_h = source.height();
                        let refine_hdr_mode =
                            !hdr_display_requests_sdr_preview(hdr_target_capacity);
                        crate::preload_debug!(
                            "[PreloadDebug][Refine] spawn_scheduled idx={} epoch={} limit={} hdr_mode={} source={}x{} path={}",
                            index,
                            _refine_epoch,
                            refine_limit,
                            refine_hdr_mode,
                            _refine_source_w,
                            _refine_source_h,
                            file_name,
                        );
                        REFINEMENT_POOL.spawn(move || {
                        if Self::hq_refinement_superseded(&loading_for_hq, index, &result_profile) {
                            preload_debug_hq_refinement_skip(
                                index,
                                "worker_start",
                                &result_profile,
                                &loading_for_hq,
                                &file_name,
                            );
                            return;
                        }
                        crate::preload_debug!(
                            "[PreloadDebug][Refine] worker_start idx={} epoch={} limit={} hdr_mode={} source={}x{} path={}",
                            index,
                            _refine_epoch,
                            refine_limit,
                            refine_hdr_mode,
                            _refine_source_w,
                            _refine_source_h,
                            file_name,
                        );

                        #[cfg(target_os = "windows")]
                        let _com = crate::wic::ComGuard::new();

                        crate::hdr::renderer::with_preview_tone_map_gpu(
                            wgpu_device_hq,
                            wgpu_queue_hq,
                            wgpu_device_id_at_spawn,
                            || {
                        let limit = refine_limit;
                        let started_at = std::time::Instant::now();
                        let is_hdr_mode = refine_hdr_mode;
                        crate::preload_debug!(
                            "[PreloadDebug][Refine] decode_start idx={} limit={} hdr_mode={} path={}",
                            index,
                            limit,
                            is_hdr_mode,
                            file_name,
                        );
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
                                    preload_debug_hq_refinement_skip(
                                        index,
                                        "after_decode",
                                        &result_profile,
                                        &loading_for_hq,
                                        &file_name,
                                    );
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
                                let _has_hdr = hdr.is_some();
                                let has_sdr = sdr.is_some();
                                crate::preload_debug!(
                                    "[PreloadDebug][Refine] decode_done idx={} kind={} {}x{} elapsed_ms={} path={}",
                                    index,
                                    preview_kind,
                                    pw,
                                    ph,
                                    crate::preload_debug::elapsed_ms(started_at),
                                    file_name,
                                );
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

                                crate::preload_debug!(
                                    "[PreloadDebug][Refine] send_preview idx={} stage=Refined epoch={} hdr={} sdr={} {}x{} path={}",
                                    index,
                                    result_profile.profile_epoch,
                                    _has_hdr,
                                    has_sdr,
                                    pw,
                                    ph,
                                    file_name,
                                );
                                let _ = tx_cloned.send(LoaderOutput::Preview(PreviewResult {
                                    index,
                                    decode_profile: result_profile.clone(),
                                    source_key: load_result.source_key,
                                    preview_bundle: bundle,
                                    error: None,
                                    cpu_demosaic_ms: None,
                                    raw_bootstrap_osd: None,
                                    sdr_texture_tag: has_sdr.then_some(
                                        crate::loader::TexturePreviewBufferTag::TiledRefinedLoader,
                                    ),
                                }));
                            }
                            Ok(Err(e)) => {
                                crate::preload_debug!(
                                    "[PreloadDebug][Refine] decode_failed idx={} limit={} elapsed_ms={} err={e} path={}",
                                    index,
                                    limit,
                                    crate::preload_debug::elapsed_ms(started_at),
                                    file_name,
                                );
                                log::error!(
                                    "[Loader] [{}] High-quality HDR preview failed: index={} limit={} elapsed={:?}: {e}",
                                    file_name,
                                    index,
                                    limit,
                                    started_at.elapsed()
                                );
                            }
                            Err(e) => {
                                crate::preload_debug!(
                                    "[PreloadDebug][Refine] decode_panicked idx={} limit={} elapsed_ms={} path={}",
                                    index,
                                    limit,
                                    crate::preload_debug::elapsed_ms(started_at),
                                    file_name,
                                );
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
                        let refine_limit = hq_preview_max_side();
                        let _refine_epoch = result_profile.profile_epoch;
                        let file_name = path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("unknown")
                            .to_string();
                        crate::preload_debug!(
                            "[PreloadDebug][Refine] spawn_scheduled idx={} epoch={} limit={} hdr_mode=false source={}x{} path={}",
                            index,
                            _refine_epoch,
                            refine_limit,
                            source.width(),
                            source.height(),
                            file_name,
                        );
                        REFINEMENT_POOL.spawn(move || {
                            if Self::hq_refinement_superseded(
                                &loading_sdr_hq,
                                index,
                                &result_profile,
                            ) {
                                preload_debug_hq_refinement_skip(
                                    index,
                                    "worker_start",
                                    &result_profile,
                                    &loading_sdr_hq,
                                    &file_name,
                                );
                                return;
                            }

                            #[cfg(target_os = "windows")]
                            let _com = crate::wic::ComGuard::new();

                            let limit = refine_limit;
                            let _started_at = std::time::Instant::now();
                            crate::preload_debug!(
                                "[PreloadDebug][Refine] decode_start idx={} limit={} hdr_mode=false path={}",
                                index,
                                limit,
                                file_name,
                            );
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
                                        preload_debug_hq_refinement_skip(
                                            index,
                                            "after_decode",
                                            &result_profile,
                                            &loading_sdr_hq,
                                            &file_name,
                                        );
                                        return;
                                    }

                                    crate::preload_debug!(
                                        "[PreloadDebug][Refine] decode_done idx={} kind=SDR {}x{} elapsed_ms={} path={}",
                                        index,
                                        pw,
                                        ph,
                                        crate::preload_debug::elapsed_ms(_started_at),
                                        file_name,
                                    );
                                    log::debug!(
                                        "[Loader] HQ preview generated: {}x{} (source {}x{})",
                                        pw,
                                        ph,
                                        source.width(),
                                        source.height()
                                    );
                                    crate::preload_debug!(
                                        "[PreloadDebug][Refine] send_preview idx={} stage=Refined epoch={} hdr=false sdr=true {}x{} path={}",
                                        index,
                                        result_profile.profile_epoch,
                                        pw,
                                        ph,
                                        file_name,
                                    );
                                    let _ = tx_cloned.send(LoaderOutput::Preview(
                                        PreviewResult::from_sdr_preview(
                                            index,
                                            result_profile.clone(),
                                            load_result.source_key,
                                            Ok(DecodedImage::new(pw, ph, p_pixels)),
                                            crate::loader::TexturePreviewBufferTag::TiledRefinedLoader,
                                        ),
                                    ));
                                }
                                Err(e) => {
                                    crate::preload_debug!(
                                        "[PreloadDebug][Refine] decode_panicked idx={} limit={} elapsed_ms={} path={}",
                                        index,
                                        limit,
                                        crate::preload_debug::elapsed_ms(_started_at),
                                        file_name,
                                    );
                                    log::error!(
                                        "[Loader] High-quality refinement PANICKED: {:?}",
                                        e
                                    );
                                }
                                _ => {
                                    crate::preload_debug!(
                                        "[PreloadDebug][Refine] decode_empty idx={} limit={} elapsed_ms={} path={}",
                                        index,
                                        limit,
                                        crate::preload_debug::elapsed_ms(_started_at),
                                        file_name,
                                    );
                                }
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
            crate::preload_debug!(
                "[PreloadDebug][Refine] skip_on_demand idx={} reason=async_raw_refinement",
                index,
            );
            return;
        }
        let tx = self.tx.clone();
        let refine_limit = hq_preview_max_side();
        let _refine_epoch = decode_profile.profile_epoch;
        crate::preload_debug!(
            "[PreloadDebug][Refine] on_demand_spawn idx={} epoch={} limit={} source={}x{}",
            index,
            _refine_epoch,
            refine_limit,
            source.width(),
            source.height(),
        );
        REFINEMENT_POOL.spawn(move || {
            #[cfg(target_os = "windows")]
            let _com = crate::wic::ComGuard::new();

            let limit = refine_limit;
            let _started_at = std::time::Instant::now();
            crate::preload_debug!(
                "[PreloadDebug][Refine] on_demand_decode_start idx={} limit={}",
                index,
                limit,
            );
            let r_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                source.generate_full_image_preview(limit, limit)
            }));

            match r_result {
                Ok((pw, ph, p_pixels)) if pw > 0 && ph > 0 => {
                    crate::preload_debug!(
                        "[PreloadDebug][Refine] on_demand_decode_done idx={} {}x{} elapsed_ms={}",
                        index,
                        pw,
                        ph,
                        crate::preload_debug::elapsed_ms(_started_at),
                    );
                    log::debug!(
                        "[Loader] On-demand HQ preview generated: {}x{} (source {}x{}) idx={}",
                        pw,
                        ph,
                        source.width(),
                        source.height(),
                        index,
                    );
                    crate::preload_debug!(
                        "[PreloadDebug][Refine] on_demand_send_preview idx={} stage=Refined epoch={} {}x{}",
                        index,
                        decode_profile.profile_epoch,
                        pw,
                        ph,
                    );
                    let _ = tx.send(LoaderOutput::Preview(PreviewResult::from_sdr_preview(
                        index,
                        decode_profile,
                        source_key,
                        Ok(DecodedImage::new(pw, ph, p_pixels)),
                        crate::loader::TexturePreviewBufferTag::TiledOnDemandSdr,
                    )));
                }
                Err(e) => {
                    crate::preload_debug!(
                        "[PreloadDebug][Refine] on_demand_decode_panicked idx={} elapsed_ms={}",
                        index,
                        crate::preload_debug::elapsed_ms(_started_at),
                    );
                    log::error!(
                        "[Loader] On-demand HQ preview PANICKED idx={}: {:?}",
                        index,
                        e
                    );
                }
                _ => {
                    crate::preload_debug!(
                        "[PreloadDebug][Refine] on_demand_decode_empty idx={} elapsed_ms={}",
                        index,
                        crate::preload_debug::elapsed_ms(_started_at),
                    );
                }
            }
        });
    }

    /// Regenerate an HQ HDR preview for a tiled source when bootstrap-only remains in cache.
    pub fn trigger_hq_tiled_hdr_preview(
        &self,
        index: usize,
        source: Arc<dyn crate::hdr::tiled::HdrTiledSource>,
        decode_profile: DecodeProfile,
        source_key: u64,
    ) {
        if source.defers_loader_hq_preview() {
            crate::preload_debug!(
                "[PreloadDebug][Refine] skip_on_demand_hdr idx={} reason=async_raw_refinement",
                index,
            );
            return;
        }
        let tx = self.tx.clone();
        let refine_limit = hq_preview_max_side();
        let wgpu_device = self.wgpu_device.clone();
        let wgpu_queue = self.wgpu_queue.clone();
        let wgpu_device_id_at_spawn = self
            .wgpu_device_id
            .load(std::sync::atomic::Ordering::Acquire);
        crate::preload_debug!(
            "[PreloadDebug][Refine] on_demand_hdr_spawn idx={} epoch={} limit={} source={}x{}",
            index,
            decode_profile.profile_epoch,
            refine_limit,
            source.width(),
            source.height(),
        );
        REFINEMENT_POOL.spawn(move || {
            #[cfg(target_os = "windows")]
            let _com = crate::wic::ComGuard::new();

            let limit = refine_limit;
            let _started_at = std::time::Instant::now();
            crate::preload_debug!(
                "[PreloadDebug][Refine] on_demand_hdr_decode_start idx={} limit={}",
                index,
                limit,
            );
            let r_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                crate::hdr::renderer::with_preview_tone_map_gpu(
                    wgpu_device,
                    wgpu_queue,
                    wgpu_device_id_at_spawn,
                    || source.generate_hdr_preview(limit, limit),
                )
            }));

            match r_result {
                Ok(Ok(hdr)) if hdr.width > 0 && hdr.height > 0 => {
                    crate::preload_debug!(
                        "[PreloadDebug][Refine] on_demand_hdr_decode_done idx={} {}x{} elapsed_ms={}",
                        index,
                        hdr.width,
                        hdr.height,
                        crate::preload_debug::elapsed_ms(_started_at),
                    );
                    log::debug!(
                        "[Loader] On-demand HQ HDR preview generated: {}x{} (source {}x{}) idx={}",
                        hdr.width,
                        hdr.height,
                        source.width(),
                        source.height(),
                        index,
                    );
                    crate::preload_debug!(
                        "[PreloadDebug][Refine] on_demand_hdr_send_preview idx={} stage=Refined epoch={} {}x{}",
                        index,
                        decode_profile.profile_epoch,
                        hdr.width,
                        hdr.height,
                    );
                    let _ = tx.send(LoaderOutput::Preview(PreviewResult {
                        index,
                        decode_profile: decode_profile.clone(),
                        source_key,
                        preview_bundle: PreviewBundle::refined().with_hdr(Arc::new(hdr)),
                        error: None,
                        cpu_demosaic_ms: None,
                        raw_bootstrap_osd: None,
                        sdr_texture_tag: None,
                    }));
                }
                Ok(Err(err)) => {
                    crate::preload_debug!(
                        "[PreloadDebug][Refine] on_demand_hdr_decode_failed idx={} elapsed_ms={} err={err}",
                        index,
                        crate::preload_debug::elapsed_ms(_started_at),
                    );
                    log::error!(
                        "[Loader] On-demand HQ HDR preview failed idx={}: {err}",
                        index,
                    );
                }
                Err(e) => {
                    crate::preload_debug!(
                        "[PreloadDebug][Refine] on_demand_hdr_decode_panicked idx={} elapsed_ms={}",
                        index,
                        crate::preload_debug::elapsed_ms(_started_at),
                    );
                    log::error!(
                        "[Loader] On-demand HQ HDR preview PANICKED idx={}: {:?}",
                        index,
                        e
                    );
                }
                _ => {
                    crate::preload_debug!(
                        "[PreloadDebug][Refine] on_demand_hdr_decode_empty idx={} elapsed_ms={}",
                        index,
                        crate::preload_debug::elapsed_ms(_started_at),
                    );
                }
            }
        });
    }
}
