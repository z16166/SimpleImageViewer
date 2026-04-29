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
use eframe::egui;

use crate::constants::{
    BYTES_PER_GB, BYTES_PER_MB, DEFAULT_ANIMATION_DELAY_MS, DEFAULT_PREVIEW_SIZE,
    MAX_QUALITY_PREVIEW_SIZE, MIN_ANIMATION_DELAY_THRESHOLD_MS, RGBA_CHANNELS,
};
use crossbeam_channel::{Receiver, Sender, TryRecvError};
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// Hardware-tier cap for HQ preview / refine (written at startup from
/// [`crate::app::HardwareTier::max_preview_size`]).
///
/// **Display cap:** do not use the window’s **client size**; the user may fullscreen at any time.
/// **Multi-monitor (policy):** use the monitor for the **current** root viewport (eframe/winit:
/// the monitor that contains the window, aligned with centering/fullscreen on that display).
///
/// **`k_zoom`:** [`crate::constants::HQ_PREVIEW_MONITOR_HEADROOM`] (**1.1**).
pub static PREVIEW_LIMIT: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(MAX_QUALITY_PREVIEW_SIZE / 2);

/// Max preview side derived from the current monitor’s **physical** long edge × headroom
/// (see [`refresh_hq_preview_monitor_cap`]). Capped at [`MAX_QUALITY_PREVIEW_SIZE`]; combined with
/// [`PREVIEW_LIMIT`] in [`hq_preview_max_side`].
pub static MONITOR_PREVIEW_CAP: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(MAX_QUALITY_PREVIEW_SIZE);

/// Recompute [`MONITOR_PREVIEW_CAP`] from egui viewport info (physical pixels). Call each frame
/// from the UI thread (cheap). If monitor size is unknown, the atomic is left unchanged.
pub fn refresh_hq_preview_monitor_cap(ctx: &egui::Context) {
    let cap = ctx.input(|i| {
        let vp = i.viewport();
        let (Some(ms), Some(npp)) = (vp.monitor_size, vp.native_pixels_per_point) else {
            return None;
        };
        if ms.x < 1.0 || ms.y < 1.0 || !npp.is_finite() || npp <= 0.0 {
            return None;
        }
        // `monitor_size` is in UI points; scale by OS native pixels-per-point → physical pixels.
        let phys_w = (ms.x * npp).round().clamp(1.0, u32::MAX as f32) as u32;
        let phys_h = (ms.y * npp).round().clamp(1.0, u32::MAX as f32) as u32;
        let long = phys_w.max(phys_h);
        let scaled = (long as f32) * crate::constants::HQ_PREVIEW_MONITOR_HEADROOM;
        let cap = scaled.ceil().max(256.0) as u32;
        Some(cap.min(MAX_QUALITY_PREVIEW_SIZE))
    });
    if let Some(c) = cap {
        MONITOR_PREVIEW_CAP.store(c, std::sync::atomic::Ordering::Relaxed);
    }
}

/// HQ preview / refine max side: `min` of hardware tier ([`PREVIEW_LIMIT`]), monitor-based cap
/// ([`MONITOR_PREVIEW_CAP`]), and [`MAX_QUALITY_PREVIEW_SIZE`].
#[inline]
pub fn hq_preview_max_side() -> u32 {
    let tier = PREVIEW_LIMIT.load(std::sync::atomic::Ordering::Relaxed);
    let tier_v = if tier == 0 {
        MAX_QUALITY_PREVIEW_SIZE
    } else {
        tier.min(MAX_QUALITY_PREVIEW_SIZE)
    };
    let mon = MONITOR_PREVIEW_CAP.load(std::sync::atomic::Ordering::Relaxed);
    let mon_v = if mon == 0 {
        MAX_QUALITY_PREVIEW_SIZE
    } else {
        mon.min(MAX_QUALITY_PREVIEW_SIZE)
    };
    tier_v.min(mon_v)
}

/// Dedicated pool for heavy high-quality preview generation (refinement).
/// Limited to 2 threads to prevent OOM when multiple giant images are switched rapidly.
static REFINEMENT_POOL: LazyLock<rayon::ThreadPool> = LazyLock::new(|| {
    match rayon::ThreadPoolBuilder::new()
        .num_threads(2)
        .thread_name(|i| format!("refinement-worker-{}", i))
        .build()
    {
        Ok(p) => p,
        Err(e) => {
            log::error!(
                "[Loader] Failed to create refinement pool: {}. Falling back to default pool.",
                e
            );
            rayon::ThreadPoolBuilder::new()
                .num_threads(1)
                .build()
                .unwrap()
        }
    }
});

use crate::raw_processor::RawProcessor;
use image::{DynamicImage, GenericImageView, RgbaImage};
use parking_lot::RwLock as PLRwLock;

/// RGBA8 in a shared [`Arc`] so decode → channel → UI can reuse one allocation (cheap `Clone`).
/// `egui::ColorImage::from_rgba_unmultiplied` still converts RGBA8 → `Color32` once at upload time.
#[derive(Clone)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pixels: Arc<Vec<u8>>,
}

impl std::fmt::Debug for DecodedImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecodedImage")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("rgba_bytes", &self.pixels.len())
            .finish()
    }
}

impl DecodedImage {
    #[inline]
    pub fn rgba(&self) -> &[u8] {
        self.pixels.as_slice()
    }

    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Self {
        Self {
            width,
            height,
            pixels: Arc::new(pixels),
        }
    }

    /// Wrap an existing RGBA8 buffer without copying.
    pub fn from_arc(width: u32, height: u32, pixels: Arc<Vec<u8>>) -> Self {
        Self {
            width,
            height,
            pixels,
        }
    }

    pub fn into_arc_pixels(self) -> Arc<Vec<u8>> {
        self.pixels
    }

    /// Build `RgbaImage`; avoids copying the buffer when this is the only [`Arc`] handle.
    pub fn into_rgba8_image(self) -> RgbaImage {
        let w = self.width;
        let h = self.height;
        let vec = Arc::try_unwrap(self.pixels).unwrap_or_else(|a| (*a).clone());
        RgbaImage::from_raw(w, h, vec).expect("DecodedImage dimensions must match RGBA buffer")
    }

    pub fn set_rgba_buffer(&mut self, width: u32, height: u32, pixels: Vec<u8>) {
        self.width = width;
        self.height = height;
        self.pixels = Arc::new(pixels);
    }

    /// Take ownership of the RGBA buffer for in-place transforms.
    /// If shared, clones the bytes; leaves `self` with an empty buffer until reassigned.
    pub fn take_rgba_owned(&mut self) -> Vec<u8> {
        let arc = std::mem::replace(&mut self.pixels, Arc::new(Vec::new()));
        Arc::try_unwrap(arc).unwrap_or_else(|a| (*a).clone())
    }
}

impl From<image::RgbaImage> for DecodedImage {
    fn from(img: image::RgbaImage) -> Self {
        let (width, height) = img.dimensions();
        Self::new(width, height, img.into_raw())
    }
}

/// Interface for images that can provide pixel data in tiles/chunks on demand.
pub trait TiledImageSource: Send + Sync {
    fn width(&self) -> u32;
    fn height(&self) -> u32;
    /// Extract a rectangular region of the image as RGBA8.
    fn extract_tile(&self, x: u32, y: u32, w: u32, h: u32) -> std::sync::Arc<Vec<u8>>;
    /// Generate a downscaled preview of the full image.
    fn generate_preview(&self, max_w: u32, max_h: u32) -> (u32, u32, Vec<u8>);
    /// Optionally provide the full pixel buffer if already in memory.
    fn full_pixels(&self) -> Option<std::sync::Arc<Vec<u8>>>;
    /// Trigger background refinement to replace preview data with full-quality pixels.
    /// Default no-op; only RAW sources need background demosaicing.
    fn request_refinement(&self, _index: usize, _generation: u64) {}
}

/// A single frame of an animated image. RGBA8 lives in a shared [`Arc`] so frame lists and
/// deferred GPU uploads clone handles instead of duplicating megabytes per frame.
#[derive(Clone)]
pub struct AnimationFrame {
    pub width: u32,
    pub height: u32,
    pixels: Arc<Vec<u8>>,
    pub delay: Duration,
}

impl std::fmt::Debug for AnimationFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnimationFrame")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("rgba_bytes", &self.pixels.len())
            .field("delay", &self.delay)
            .finish()
    }
}

impl AnimationFrame {
    #[inline]
    pub fn rgba(&self) -> &[u8] {
        self.pixels.as_slice()
    }

    pub fn new(width: u32, height: u32, pixels: Vec<u8>, delay: Duration) -> Self {
        Self {
            width,
            height,
            pixels: Arc::new(pixels),
            delay,
        }
    }

    #[inline]
    pub fn arc_pixels(&self) -> Arc<Vec<u8>> {
        Arc::clone(&self.pixels)
    }
}

/// Decoded image data — either a static image, a large image (for tiled rendering), or an animated sequence.
#[derive(Clone)]
pub enum ImageData {
    Static(DecodedImage),
    /// Virtualized image source — tiles are decoded on-demand from disk or other sources.
    Tiled(std::sync::Arc<dyn TiledImageSource>),
    Animated(Vec<AnimationFrame>),
}

#[derive(Clone)]
pub struct LoadResult {
    pub index: usize,
    pub generation: u64,
    pub result: Result<ImageData, String>,
    pub preview: Option<DecodedImage>,
}

pub struct TileResult {
    pub index: usize,
    pub col: u32,
    pub row: u32,
}

pub struct PreviewResult {
    pub index: usize,
    pub generation: u64,
    pub result: Result<DecodedImage, String>,
}

pub enum LoaderOutput {
    Image(LoadResult),
    Tile(TileResult),
    Preview(PreviewResult),
    /// Background refinement finished (e.g. LibRaw demosaic)
    Refined(usize, u64),
}

pub struct RefinementRequest {
    pub path: PathBuf,
    pub index: usize,
    pub generation: u64,
    pub orientation_override: Option<i32>,
    pub developed_image: Arc<PLRwLock<Option<DynamicImage>>>,
}

struct TileRequest {
    generation: u64,
    priority: f32, // Higher is better
    index: usize,
    col: u32,
    row: u32,
    source: Arc<dyn TiledImageSource>,
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
                        );
                    }
                });
        }

        let tile_queue: Arc<(Mutex<BinaryHeap<TileRequest>>, Condvar)> =
            Arc::new((Mutex::new(BinaryHeap::new()), Condvar::new()));
        // Shared set of tiles currently being decoded — prevents duplicate work across workers
        let in_flight: Arc<Mutex<std::collections::HashSet<(usize, u32, u32)>>> =
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

                        let tile_key = (request.index, request.col, request.row);

                        // Skip if already in CPU cache (another worker finished it first)
                        {
                            if let Ok(mut cache) = crate::tile_cache::PIXEL_CACHE.lock() {
                                if cache
                                    .get(
                                        request.index,
                                        crate::tile_cache::TileCoord {
                                            col: request.col,
                                            row: request.row,
                                        },
                                    )
                                    .is_some()
                                {
                                    continue;
                                }
                            }
                        }

                        // Claim this tile — skip if another worker is already decoding it
                        {
                            let mut set = flight.lock().unwrap();
                            if !set.insert(tile_key) {
                                continue; // Another worker is already on it
                            }
                        }

                        let tile_size = crate::tile_cache::get_tile_size();
                        let x = request.col * tile_size;
                        let y = request.row * tile_size;
                        let tw = tile_size.min(request.source.width() - x);
                        let th = tile_size.min(request.source.height() - y);

                        #[cfg(feature = "tile-debug")]
                        let t0 = std::time::Instant::now();
                        let pixels = request.source.extract_tile(x, y, tw, th);
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

                        // Remove from in-flight set (cache already has the data)
                        {
                            let mut set = flight.lock().unwrap();
                            set.remove(&tile_key);
                        }

                        // Notify main thread that tile is ready for GPU upload
                        let _ = tx.send(LoaderOutput::Tile(TileResult {
                            index: request.index,
                            col: request.col,
                            row: request.row,
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
                    log::info!("[Refinement] Starting full demosaic for {:?} (gen={})", req.path.file_name().unwrap_or_default(), req.generation);
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

                            let _ = worker_tx.send(LoaderOutput::Preview(PreviewResult {
                                index: req.index,
                                generation: req.generation,
                                result: Ok(preview),
                            }));

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

    pub fn request_load(
        &mut self,
        index: usize,
        generation: u64,
        path: PathBuf,
        high_quality: bool,
    ) {
        {
            let mut loading = self.loading.lock().unwrap();
            if loading.get(&index) == Some(&generation) {
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
        };
        {
            let (lock, cvar) = &*self.delayed_fallback;
            let mut slot = lock.lock().unwrap();
            *slot = Some(delayed_job);
            cvar.notify_one();
        }
    }

    fn do_load(
        index: usize,
        generation: u64,
        path: &PathBuf,
        tx: Sender<LoaderOutput>,
        refine_tx: Sender<RefinementRequest>,
        loading_ref: Arc<Mutex<HashMap<usize, u64>>>,
        current_gen: Arc<std::sync::atomic::AtomicU64>,
        high_quality: bool,
    ) {
        let global_gen = current_gen.load(std::sync::atomic::Ordering::Relaxed);
        if generation != global_gen {
            let mut loading = loading_ref.lock().unwrap();
            if loading.get(&index) == Some(&generation) {
                loading.remove(&index);
            }
            return;
        }

        let load_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            load_image_file(
                generation,
                index,
                path,
                tx.clone(),
                refine_tx.clone(),
                high_quality,
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
                preview: None,
            }
        });

        if let Err(ref e) = load_result.result {
            log::error!("[Loader] Load FAILED for index={}: {}", index, e);
        }

        // Drop stale results before sending into the channel (rapid navigation).
        {
            let map = loading_ref.lock().unwrap();
            if map.get(&index) != Some(&generation) {
                return;
            }
        }

        // Tiled HQ preview: only `Arc::clone` the source; `load_result` moves to the channel once
        // (avoids cloning full Static/Animated pixel buffers).
        if let Ok(ImageData::Tiled(ref source)) = load_result.result {
            let source = Arc::clone(source);
            let tx_cloned = tx.clone();
            let gen_ref = Arc::clone(&current_gen);
            REFINEMENT_POOL.spawn(move || {
                // Staleness check: Abort if the user has navigated to a new image
                if gen_ref.load(std::sync::atomic::Ordering::Relaxed) > generation {
                    return;
                }

                #[cfg(target_os = "windows")]
                let _com = crate::wic::ComGuard::new();

                let limit = hq_preview_max_side();
                let r_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    source.generate_preview(limit, limit)
                }));

                match r_result {
                    Ok((pw, ph, p_pixels)) if pw > 0 && ph > 0 => {
                        // Double check staleness after the expensive thumbnailing
                        if gen_ref.load(std::sync::atomic::Ordering::Relaxed) > generation {
                            return;
                        }

                        log::info!(
                            "[Loader] HQ preview generated: {}x{} (source {}x{})",
                            pw,
                            ph,
                            source.width(),
                            source.height()
                        );
                        let _ = tx_cloned.send(LoaderOutput::Preview(PreviewResult {
                            index,
                            generation,
                            result: Ok(DecodedImage::new(pw, ph, p_pixels)),
                        }));
                    }
                    Err(e) => {
                        log::error!("[Loader] High-quality refinement PANICKED: {:?}", e);
                    }
                    _ => {}
                }
            });
        }

        let _ = tx.send(LoaderOutput::Image(load_result));
    }

    pub fn request_tile(
        &self,
        index: usize,
        generation: u64,
        priority: f32,
        source: std::sync::Arc<dyn TiledImageSource>,
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
    pub fn discard_pending_stale_outputs(&mut self, keep_generation: u64) {
        let keep = |output: &LoaderOutput| -> bool {
            match output {
                LoaderOutput::Image(r) => r.generation == keep_generation,
                LoaderOutput::Preview(p) => p.generation == keep_generation,
                LoaderOutput::Refined(_, g) => *g == keep_generation,
                // Tile notifications carry no generation; keep to avoid breaking in-flight uploads.
                LoaderOutput::Tile(_) => true,
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
                if let LoaderOutput::Image(ref r) = output {
                    let mut loading = self.loading.lock().unwrap();
                    if let Some(&g) = loading.get(&r.index) {
                        if g <= r.generation {
                            loading.remove(&r.index);
                        }
                    }
                }
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
            Ok(output) => {
                if let LoaderOutput::Image(ref result) = output {
                    let mut loading = self.loading.lock().unwrap();
                    if let Some(&g) = loading.get(&result.index) {
                        if g <= result.generation {
                            loading.remove(&result.index);
                        }
                    }
                }
                Some(output)
            }
            Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => None,
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
}

fn load_image_file(
    generation: u64,
    index: usize,
    path: &PathBuf,
    _tx: Sender<LoaderOutput>,
    refine_tx: Sender<RefinementRequest>,
    high_quality: bool,
) -> LoadResult {
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");

    let result = (|| -> Result<ImageData, String> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();
        let is_system_native = if let Ok(reg) = crate::formats::get_registry().read() {
            reg.extensions.contains(&ext)
        } else {
            false
        };

        if crate::hdr::decode::is_hdr_candidate_ext(&ext) {
            match load_hdr(path) {
                Ok(img) => return Ok(img),
                Err(e) => {
                    log::debug!(
                        "[{}] HDR float decode failed, continuing with standard fallback chain: {}",
                        file_name,
                        e
                    );
                }
            }
        }

        if ext == "psd" || ext == "psb" {
            if let Ok(item) = load_psd(path) {
                return Ok(item);
            }
        }

        let is_raw = crate::raw_processor::is_raw_extension(&ext);

        if is_raw {
            return load_raw(index, generation, path, refine_tx.clone(), high_quality);
        }

        if ext == "jpg" || ext == "jpeg" {
            return load_jpeg(path);
        }
        if ext == "tif" || ext == "tiff" {
            return crate::libtiff_loader::load_via_libtiff(path);
        }

        if is_system_native && !is_maybe_animated(&ext) {
            #[cfg(target_os = "windows")]
            if let Ok(img) = crate::wic::load_via_wic(path, high_quality, None) {
                return Ok(img);
            }
            #[cfg(target_os = "macos")]
            if let Ok(img) = crate::macos_image_io::load_via_image_io(path, high_quality, None) {
                return Ok(img);
            }
        }

        let result = match ext.as_str() {
            "gif" => load_gif(path),
            "png" | "apng" => load_png(path),
            "webp" => load_webp(path),
            "heif" | "heic" => load_heic(path),
            "jpg" | "jpeg" => load_jpeg(path),
            _ => load_static(path),
        };
        if result.is_err() {
            #[cfg(target_os = "windows")]
            if let Ok(img) = crate::wic::load_via_wic(path, high_quality, None) {
                return Ok(img);
            }
            #[cfg(target_os = "macos")]
            if let Ok(img) = crate::macos_image_io::load_via_image_io(path, high_quality, None) {
                return Ok(img);
            }

            // Last resort: Detect format by content (magic bytes)
            if let Ok(retry_img) = load_via_content_detection(path) {
                log::info!(
                    "[{}] Successfully recovered via content-based detection",
                    file_name
                );
                return Ok(retry_img);
            }
        }

        result
    })();

    let mut preview: Option<DecodedImage> = None;

    let final_result = match result {
        Ok(ImageData::Tiled(source)) => {
            log::info!(
                "[{}] Tiled image source active: {}x{} ({:.1} MP)",
                file_name,
                source.width(),
                source.height(),
                (source.width() as f64 * source.height() as f64) / 1_000_000.0
            );

            let t0 = std::time::Instant::now();
            let exif_thumb = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                extract_exif_thumbnail(path)
            }));
            match exif_thumb {
                Ok(Some(thumb)) => {
                    log::info!(
                        "[{}] EXIF thumbnail extracted in {:?}",
                        file_name,
                        t0.elapsed()
                    );
                    preview = Some(thumb);
                }
                Ok(None) => {
                    log::info!(
                        "[{}] No EXIF thumbnail found (took {:?}), generating {}px preview...",
                        file_name,
                        t0.elapsed(),
                        DEFAULT_PREVIEW_SIZE
                    );
                    let t1 = std::time::Instant::now();
                    let gen_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        source.generate_preview(DEFAULT_PREVIEW_SIZE, DEFAULT_PREVIEW_SIZE)
                    }));
                    match gen_result {
                        Ok((pw, ph, p_pixels)) if pw > 0 && ph > 0 => {
                            log::info!(
                                "[{}] {}px preview generated ({}x{}) in {:?}",
                                file_name,
                                DEFAULT_PREVIEW_SIZE,
                                pw,
                                ph,
                                t1.elapsed()
                            );
                            preview = Some(DecodedImage::new(pw, ph, p_pixels));
                        }
                        Ok(_) => {
                            log::warn!(
                                "[{}] generate_preview returned empty/zero-size result in {:?}",
                                file_name,
                                t1.elapsed()
                            );
                        }
                        Err(e) => {
                            log::error!(
                                "[{}] generate_preview PANICKED: {:?} in {:?}",
                                file_name,
                                e,
                                t1.elapsed()
                            );
                        }
                    }
                }
                Err(e) => {
                    log::error!("[{}] extract_exif_thumbnail PANICKED: {:?}", file_name, e);
                }
            }

            Ok(ImageData::Tiled(source))
        }
        Ok(ImageData::Static(decoded)) => Ok(make_image_data(decoded)),
        Ok(ImageData::Animated(frames)) => {
            if let Some(first) = frames.first() {
                let width = first.width;
                let height = first.height;
                let max_side = width.max(height);
                let limit = crate::tile_cache::get_max_texture_side();

                let total_bytes: usize = frames.iter().map(|f| f.rgba().len()).sum();
                let mb = total_bytes as f64 / (BYTES_PER_MB as f64);

                if max_side > limit {
                    log::warn!(
                        "[{}] Animated image ({}x{}) exceeds GPU limits. Falling back to tiled static mode.",
                        file_name,
                        width,
                        height
                    );
                    Ok(make_image_data(DecodedImage::from_arc(
                        width,
                        height,
                        first.arc_pixels(),
                    )))
                } else {
                    log::info!(
                        "[{}] Decoded {}x{} ({} frames, {:.1} MB) - Animated Mode",
                        file_name,
                        width,
                        height,
                        frames.len(),
                        mb
                    );
                    Ok(ImageData::Animated(frames))
                }
            } else {
                Ok(ImageData::Animated(frames))
            }
        }
        Err(e) => {
            log::error!("[{}] Failed to load: {}", file_name, e);
            Err(e)
        }
    };

    LoadResult {
        index,
        generation,
        result: final_result,
        preview,
    }
}

fn load_jpeg(path: &PathBuf) -> Result<ImageData, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mmap = unsafe { memmap2::Mmap::map(&file).map_err(|e| e.to_string())? };
    let (mut w, mut h, mut pixels) = libjpeg_turbo::decode_to_rgba(&mmap)?;

    let orientation = crate::metadata_utils::get_exif_orientation(path);
    if orientation > 1 {
        let (out_w, out_h, out_pixels) =
            crate::libtiff_loader::apply_orientation_buffer(pixels, w, h, orientation);
        w = out_w;
        h = out_h;
        pixels = out_pixels;
    }

    Ok(make_image_data(DecodedImage::new(w, h, pixels)))
}

// Centralized in metadata_utils.rs

fn load_static(path: &PathBuf) -> Result<ImageData, String> {
    use image::ImageReader;

    let reader = ImageReader::open(path).map_err(|e| e.to_string())?;
    let mut decoder = reader.with_guessed_format().map_err(|e| e.to_string())?;
    // Remove the default memory limit (512MB) to allow gigapixel images
    decoder.no_limits();

    let img = match decoder.decode() {
        Ok(img) => img,
        Err(e) => return Err(e.to_string()),
    };
    let rgba = img.into_rgba8();
    let (width, height) = rgba.dimensions();
    let pixels = rgba.into_raw();

    Ok(make_image_data(DecodedImage::new(width, height, pixels)))
}

fn load_hdr(path: &Path) -> Result<ImageData, String> {
    let hdr = crate::hdr::decode::decode_hdr_image(path)?;
    let pixels = crate::hdr::decode::hdr_to_sdr_rgba8(&hdr, 0.0)?;

    Ok(ImageData::Static(DecodedImage::new(
        hdr.width, hdr.height, pixels,
    )))
}

fn process_animation_frames(
    raw_frames: Vec<image::Frame>,
    path: &PathBuf,
) -> Result<ImageData, String> {
    if raw_frames.len() <= 1 {
        return load_static(path);
    }

    let frames: Vec<AnimationFrame> = raw_frames
        .into_iter()
        .map(|frame| {
            let (numer, denom) = frame.delay().numer_denom_ms();
            let delay_ms = if denom == 0 {
                DEFAULT_ANIMATION_DELAY_MS
            } else {
                numer / denom
            };
            // Standard browser behavior: delays <= 10ms are treated as 100ms
            let delay_ms = if delay_ms <= MIN_ANIMATION_DELAY_THRESHOLD_MS {
                DEFAULT_ANIMATION_DELAY_MS
            } else {
                delay_ms
            };
            let buffer = frame.into_buffer();
            let (width, height) = buffer.dimensions();
            AnimationFrame::new(
                width,
                height,
                buffer.into_raw(),
                Duration::from_millis(delay_ms as u64),
            )
        })
        .collect();

    Ok(ImageData::Animated(frames))
}

fn load_gif(path: &PathBuf) -> Result<ImageData, String> {
    use image::AnimationDecoder;
    use image::codecs::gif::GifDecoder;
    use std::io::BufReader;

    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let reader = BufReader::new(file);
    let decoder = GifDecoder::new(reader).map_err(|e| e.to_string())?;
    let raw_frames = decoder
        .into_frames()
        .collect_frames()
        .map_err(|e| e.to_string())?;

    process_animation_frames(raw_frames, path)
}

fn load_png(path: &PathBuf) -> Result<ImageData, String> {
    use image::AnimationDecoder;
    use image::codecs::png::PngDecoder;
    use std::io::BufReader;

    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let reader = BufReader::new(file);
    let decoder = PngDecoder::new(reader).map_err(|e| e.to_string())?;

    if !decoder.is_apng().map_err(|e| e.to_string())? {
        return load_static(path);
    }

    let raw_frames = decoder
        .apng()
        .map_err(|e| e.to_string())?
        .into_frames()
        .collect_frames()
        .map_err(|e| e.to_string())?;

    process_animation_frames(raw_frames, path)
}

// ---------------------------------------------------------------------------
// Animated WebP
// ---------------------------------------------------------------------------

fn load_webp(path: &PathBuf) -> Result<ImageData, String> {
    use image::AnimationDecoder;
    use image::codecs::webp::WebPDecoder;
    use std::io::BufReader;

    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let reader = BufReader::new(file);
    let decoder = WebPDecoder::new(reader).map_err(|e| e.to_string())?;
    let raw_frames = decoder
        .into_frames()
        .collect_frames()
        .map_err(|e| e.to_string())?;

    process_animation_frames(raw_frames, path)
}

// ---------------------------------------------------------------------------
// PSD / PSB (Photoshop Document / Large Document)
// ---------------------------------------------------------------------------

fn load_psd(path: &PathBuf) -> Result<ImageData, String> {
    // Step 1: Estimate memory requirement from header
    let (width, height, _channels, estimated_bytes) = crate::psb_reader::estimate_memory(path)?;
    let estimated_mb = estimated_bytes / BYTES_PER_MB;

    // Step 2: Check available RAM
    use sysinfo::System;
    let mut sys = System::new();
    sys.refresh_memory();
    let available_mb = sys.available_memory() / BYTES_PER_MB;

    // Reserve at least 1GB for the OS + app overhead
    let safe_available = available_mb.saturating_sub(BYTES_PER_GB / BYTES_PER_MB);
    if estimated_mb > safe_available {
        return Err(format!(
            "Image requires ~{estimated_mb} MB RAM but only ~{safe_available} MB is available. \
             Please close other applications or convert to a smaller format."
        ));
    }

    log::info!(
        "PSD/PSB {}x{}: estimated {estimated_mb} MB, available {available_mb} MB — proceeding",
        width,
        height
    );

    // Step 3: Detect version and choose decoder
    let mut sig_buf = [0u8; 6];
    {
        use std::io::Read;
        let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
        f.read_exact(&mut sig_buf).map_err(|e| e.to_string())?;
    }
    let version = u16::from_be_bytes([sig_buf[4], sig_buf[5]]);

    if version == 2 {
        // PSB v2: Use tiled source for large files
        log::info!("Using custom PSB tiled source for v2 format");
        let source = crate::psb_reader::open_tiled_source(path)?;
        let arc_source = std::sync::Arc::new(source);
        Ok(ImageData::Tiled(arc_source))
    } else {
        // PSD v1: use the psd crate (reads entire file into memory)
        let bytes = std::fs::read(path).map_err(|e| format!("Failed to read PSD: {e}"))?;
        let psd_file =
            psd::Psd::from_bytes(&bytes).map_err(|e| format!("Failed to parse PSD: {e}"))?;
        let w = psd_file.width();
        let h = psd_file.height();
        let pixels = psd_file.rgba();

        let img = DecodedImage::new(w, h, pixels);
        Ok(make_image_data(img))
    }
}

/// Returns true if the extension belongs to a format that we prefer to load
/// via image-rs to preserve animations (GIF, WebP, APNG).
fn is_maybe_animated(ext: &str) -> bool {
    matches!(ext, "gif" | "webp" | "apng" | "png")
}

// ---------------------------------------------------------------------------
// HEIF / HEIC (High Efficiency Image Format)
// ---------------------------------------------------------------------------

fn load_heic(path: &PathBuf) -> Result<ImageData, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("Failed to read HEIC file: {e}"))?;

    // Decode directly to RGBA8
    let output = heic::DecoderConfig::new()
        .decode(&bytes, heic::PixelLayout::Rgba8)
        .map_err(|e| format!("Failed to decode HEIC: {e:?}"))?;

    let width = output.width;
    let height = output.height;
    let rgba = output.data;

    Ok(make_image_data(DecodedImage::new(width, height, rgba)))
}

/// Helper to create ImageData that respects GPU texture limits.
/// If the image is too large for a single GPU texture, it is returned as ImageData::Tiled
/// using a MemoryImageSource to avoid hardware panics while preserving full resolution.
fn make_image_data(img: DecodedImage) -> ImageData {
    let pixel_count = img.width as u64 * img.height as u64;
    let max_side = img.width.max(img.height);
    // Use the conservative ABSOLUTE_MAX_TEXTURE_SIDE (8192) for the tiling decision,
    // consistent with WIC, macOS ImageIO, and Linux libtiff paths.
    // Images exceeding 8192 on any side benefit from the tiled preview pipeline
    // (instant EXIF preview + async HQ preview) regardless of GPU capability.
    // The GPU's actual texture limit (often 16384) is used only at the wgpu device
    // level to allow tile textures of any supported size.
    let limit = crate::constants::ABSOLUTE_MAX_TEXTURE_SIDE;
    let tiled_limit = crate::tile_cache::TILED_THRESHOLD.load(std::sync::atomic::Ordering::Relaxed);

    if pixel_count >= tiled_limit || max_side > limit {
        log::info!(
            "[Loader] Image {}x{} ({:.1} MP) exceeds GPU limit ({}) or threshold ({:.1} MP). Using forced tiling.",
            img.width,
            img.height,
            pixel_count as f64 / 1_000_000.0,
            limit,
            tiled_limit as f64 / 1_000_000.0
        );
        ImageData::Tiled(Arc::new(MemoryImageSource::new(
            img.width,
            img.height,
            img.into_arc_pixels(),
        )))
    } else {
        ImageData::Static(img)
    }
}

const DETECTION_BUFFER_SIZE: usize = 16;

fn load_by_image_format(format: image::ImageFormat, path: &PathBuf) -> Result<ImageData, String> {
    match format {
        image::ImageFormat::Png => load_png(path),
        image::ImageFormat::Gif => load_gif(path),
        image::ImageFormat::WebP => load_webp(path),
        image::ImageFormat::Tiff => crate::libtiff_loader::load_via_libtiff(path),
        // Standard single-frame formats handled by load_static
        image::ImageFormat::Jpeg => load_jpeg(path),
        image::ImageFormat::Bmp
        | image::ImageFormat::Ico
        | image::ImageFormat::Pnm
        | image::ImageFormat::Tga
        | image::ImageFormat::Dds
        | image::ImageFormat::Farbfeld
        | image::ImageFormat::Avif
        | image::ImageFormat::Qoi => load_static(path),
        image::ImageFormat::Hdr | image::ImageFormat::OpenExr => load_hdr(path),
        _ => Err(rust_i18n::t!(
            "error.unsupported_detected_format",
            format = format!("{:?}", format)
        )
        .to_string()),
    }
}

fn load_via_content_detection(path: &PathBuf) -> Result<ImageData, String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;

    // Use constant for buffer size
    let mut header = [0u8; DETECTION_BUFFER_SIZE];
    let n = file.read(&mut header).unwrap_or(0);

    // 1. Try standard image-rs detection
    if let Ok(guessed) = image::guess_format(&header[..n]) {
        return load_by_image_format(guessed, path);
    }

    // 2. Manual HEIC detection (since image-rs 0.25 doesn't natively guess it)
    // HEIF/HEIC signature: "ftyp" (at offset 4) followed by various brands.
    if n >= 12 && &header[4..8] == b"ftyp" {
        let sub = &header[8..12];
        if sub == b"heic"
            || sub == b"heix"
            || sub == b"hevc"
            || sub == b"hevx"
            || sub == b"mif1"
            || sub == b"msf1"
        {
            return load_heic(path);
        }
    }

    Err(rust_i18n::t!("error.detection_failed").to_string())
}

// ---------------------------------------------------------------------------
// Metadata & Thumbnails
// ---------------------------------------------------------------------------

fn extract_exif_thumbnail(path: &Path) -> Option<DecodedImage> {
    use exif::Reader;
    let file = std::fs::File::open(path).ok()?;
    let mut reader = std::io::BufReader::new(file);
    let exifreader = Reader::new();

    if let Ok(exif_data) = exifreader.read_from_container(&mut reader) {
        // Find thumbnail offset and length in IFD1 (THUMBNAIL)
        let offset = exif_data
            .get_field(exif::Tag::JPEGInterchangeFormat, exif::In::THUMBNAIL)
            .and_then(|f| f.value.get_uint(0));
        let length = exif_data
            .get_field(exif::Tag::JPEGInterchangeFormatLength, exif::In::THUMBNAIL)
            .and_then(|f| f.value.get_uint(0));

        if let (Some(off), Some(len)) = (offset, length) {
            use std::io::{Read, Seek, SeekFrom};
            let mut f = std::fs::File::open(path).ok()?;
            f.seek(SeekFrom::Start(off as u64)).ok()?;
            let mut blob = vec![0u8; len as usize];
            if f.read_exact(&mut blob).is_ok() {
                if let Ok(img) = image::load_from_memory(&blob) {
                    let rgba = img.into_rgba8();
                    log::info!(
                        "[{}] Extracted EXIF thumbnail ({}x{}) from offset {}",
                        path.file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("unknown"),
                        rgba.width(),
                        rgba.height(),
                        off
                    );
                    return Some(DecodedImage::new(
                        rgba.width(),
                        rgba.height(),
                        rgba.into_raw(),
                    ));
                }
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Texture cache
// ---------------------------------------------------------------------------

pub struct TextureCache {
    pub textures: HashMap<usize, egui::TextureHandle>,
    /// Original image dimensions (may differ from texture size for Tiled previews).
    original_res: HashMap<usize, (u32, u32)>,
    /// Flag indicating if the image was Tiled/Large and needs TileManager reconstruction.
    is_tiled: HashMap<usize, bool>,
    max_size: usize,
}

impl TextureCache {
    pub fn new(max_size: usize) -> Self {
        Self {
            textures: HashMap::new(),
            original_res: HashMap::new(),
            is_tiled: HashMap::new(),
            max_size,
        }
    }

    pub fn insert(
        &mut self,
        index: usize,
        handle: egui::TextureHandle,
        orig_w: u32,
        orig_h: u32,
        tiled: bool,
        current_index: usize,
        total_count: usize,
    ) -> Option<usize> {
        self.textures.insert(index, handle);
        self.original_res.insert(index, (orig_w, orig_h));
        self.is_tiled.insert(index, tiled);
        self.evict(current_index, total_count)
    }

    pub fn get_original_res(&self, index: usize) -> Option<(u32, u32)> {
        self.original_res.get(&index).copied()
    }

    pub fn remove(&mut self, index: usize) {
        self.textures.remove(&index);
        self.original_res.remove(&index);
        self.is_tiled.remove(&index);
    }

    /// Check if the image at index is a Tiled/Large image.

    pub fn get(&self, index: usize) -> Option<&egui::TextureHandle> {
        self.textures.get(&index)
    }

    pub fn contains(&self, index: usize) -> bool {
        self.textures.contains_key(&index)
    }

    pub fn is_preview_placeholder(&self, index: usize) -> bool {
        self.is_tiled.get(&index).copied().unwrap_or(false)
    }

    /// Longer side of the **uploaded** preview texture in pixels (not the full-image logical size).
    /// Used to avoid replacing a stage-2 HQ preview with a stage-1 bootstrap when re-opening a file.
    pub fn cached_preview_max_side(&self, index: usize) -> Option<u32> {
        self.textures.get(&index).map(|h| {
            let s = h.size();
            s[0].max(s[1]) as u32
        })
    }

    pub fn clear_all(&mut self) {
        self.textures.clear();
        self.original_res.clear();
        self.is_tiled.clear();
    }

    fn evict(&mut self, current_index: usize, total_count: usize) -> Option<usize> {
        if self.textures.len() <= self.max_size {
            return None;
        }
        // Evict the texture with the greatest CIRCULAR distance from current_index.
        // In a 100-image list, index 99 is distance 1 from index 0 (wrapping around).
        let to_remove = self.textures.keys().copied().max_by_key(|&idx| {
            if total_count == 0 {
                (idx as isize - current_index as isize).unsigned_abs()
            } else {
                let forward = (idx + total_count - current_index) % total_count;
                let backward = (current_index + total_count - idx) % total_count;
                forward.min(backward)
            }
        });

        if let Some(idx) = to_remove {
            self.textures.remove(&idx);
            self.original_res.remove(&idx);
            self.is_tiled.remove(&idx);
            Some(idx)
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// RAW Image Support (LibRaw)
// ---------------------------------------------------------------------------

pub struct RawImageSource {
    path: PathBuf,
    /// True RAW sensor dimensions (not thumbnail dimensions).
    width: u32,
    height: u32,
    /// Initially holds the system preview at its ORIGINAL resolution (NOT upscaled).
    /// The refinement worker replaces this with the full-res LibRaw demosaiced image.
    /// extract_tile() dynamically maps coordinates between RAW space and preview space.
    developed_image: Arc<PLRwLock<Option<DynamicImage>>>,
    /// Channel to send refinement requests. Kept here so `request_refinement()` can
    /// be called later (only when the image becomes active) instead of eagerly in the
    /// constructor, preventing prefetched images from spawning ~400MB develop tasks.
    refine_tx: Sender<RefinementRequest>,
    orientation_override: i32,
}

impl RawImageSource {
    pub fn new(
        path: PathBuf,
        preview: DecodedImage,
        raw_width: u32,
        raw_height: u32,
        refine_tx: Sender<RefinementRequest>,
        orientation_override: i32,
    ) -> Self {
        // IMPORTANT: Store preview at its ORIGINAL resolution — NO upscaling!
        // Previously this called resize_exact(raw_width, raw_height) which allocated
        // ~400MB per image (e.g. 11648×8736×4). With rapid switching and prefetching,
        // multiple concurrent allocations of this size caused OOM crashes.
        // Instead, extract_tile() maps coordinates from RAW space to preview space on demand.
        //
        // ALSO: We do NOT send a refinement request here. Refinement is deferred until
        // the image becomes the actively-viewed one (via request_refinement()). This
        // prevents prefetched images from each spawning ~400MB LibRaw develop tasks.

        let rgba = preview.into_rgba8_image();
        let developed_image = Arc::new(PLRwLock::new(Some(DynamicImage::ImageRgba8(rgba))));

        let refine_tx = refine_tx.clone();

        Self {
            path,
            width: raw_width,
            height: raw_height,
            developed_image,
            refine_tx,
            orientation_override,
        }
    }
}

impl TiledImageSource for RawImageSource {
    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn extract_tile(&self, x: u32, y: u32, w: u32, h: u32) -> Arc<Vec<u8>> {
        let img_lock = self.developed_image.read();
        if let Some(ref img) = *img_lock {
            let (iw, ih) = img.dimensions();
            if iw == self.width && ih == self.height {
                // Full-res developed image available — direct crop, no scaling needed.
                if let Some(rgba) = img.as_rgba8() {
                    let mut result = vec![0u8; (w * h * 4) as usize];
                    for row in 0..h {
                        let src_y = y + row;
                        let src_offset = (src_y * iw + x) as usize * 4;
                        let dst_offset = (row * w) as usize * 4;
                        let len =
                            (w as usize * 4).min(rgba.as_raw().len().saturating_sub(src_offset));
                        if len > 0 {
                            result[dst_offset..dst_offset + len]
                                .copy_from_slice(&rgba.as_raw()[src_offset..src_offset + len]);
                        }
                    }
                    Arc::new(result)
                } else {
                    let crop = img.crop_imm(x, y, w, h);
                    Arc::new(crop.into_rgba8().into_raw())
                }
            } else {
                // Preview image (smaller than RAW dimensions).
                let scale_x = iw as f64 / self.width as f64;
                let scale_y = ih as f64 / self.height as f64;
                let px = (x as f64 * scale_x) as u32;
                let py = (y as f64 * scale_y) as u32;
                let pw = ((w as f64 * scale_x).ceil() as u32)
                    .min(iw.saturating_sub(px))
                    .max(1);
                let ph = ((h as f64 * scale_y).ceil() as u32)
                    .min(ih.saturating_sub(py))
                    .max(1);
                let crop = img.crop_imm(px, py, pw, ph);
                let resized = crop.resize_exact(w, h, image::imageops::FilterType::Triangle);
                Arc::new(resized.into_rgba8().into_raw())
            }
        } else {
            Arc::new(vec![0; (w * h * RGBA_CHANNELS as u32) as usize])
        }
    }

    fn generate_preview(&self, max_w: u32, max_h: u32) -> (u32, u32, Vec<u8>) {
        let img_lock = self.developed_image.read();
        if let Some(ref img) = *img_lock {
            let scaled = img.thumbnail(max_w, max_h);
            let rgba = scaled.to_rgba8();
            (rgba.width(), rgba.height(), rgba.into_raw())
        } else {
            (0, 0, Vec::new())
        }
    }

    fn full_pixels(&self) -> Option<Arc<Vec<u8>>> {
        let img_lock = self.developed_image.read();
        if let Some(ref img) = *img_lock {
            let (iw, ih) = img.dimensions();
            // Only return pixels when we have the full-res developed image.
            // If it's still the small preview, the stride would mismatch
            // self.width/self.height and corrupt downstream consumers (e.g. printing).
            if iw == self.width && ih == self.height {
                Some(Arc::new(img.to_rgba8().into_raw()))
            } else {
                None
            }
        } else {
            None
        }
    }

    fn request_refinement(&self, index: usize, generation: u64) {
        log::info!(
            "[RawImageSource] Triggering refinement for index={}, gen={}",
            index,
            generation
        );
        let _ = self.refine_tx.send(RefinementRequest {
            path: self.path.clone(),
            index,
            generation,
            orientation_override: Some(self.orientation_override),
            developed_image: self.developed_image.clone(),
        });
    }
}

fn load_raw(
    _index: usize,
    _generation: u64,
    path: &PathBuf,
    refine_tx: Sender<RefinementRequest>,
    high_quality: bool,
) -> Result<ImageData, String> {
    // 1. Initialize LibRaw Processor and attempt to open the file header.
    let mut processor =
        RawProcessor::new().ok_or_else(|| rust_i18n::t!("error.libraw_init").to_string())?;
    if let Err(e) = processor.open(path) {
        log::warn!(
            "[Loader] LibRaw could not open {:?}: {}. Falling back to Rule 2 (WIC/ImageIO).",
            path,
            e
        );
        #[cfg(target_os = "windows")]
        return crate::wic::load_via_wic(path, high_quality, None);
        #[cfg(target_os = "macos")]
        return crate::macos_image_io::load_via_image_io(path, high_quality, None);
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        return Err(format!(
            "LibRaw failed and no platform fallback available: {}",
            e
        ));
    }

    let (width, height) = (processor.width() as u32, processor.height() as u32);
    let area = width as u64 * height as u64;
    let threshold = crate::tile_cache::TILED_THRESHOLD.load(std::sync::atomic::Ordering::Relaxed);

    // 1. Determine the authoritative orientation once and for all.
    // We prioritize LibRaw's flip metadata, falling back to the exif crate only if LibRaw's value is unknown.
    let lr_flip = processor.flip();
    let final_orientation = match lr_flip {
        0 => 1,
        1 => 2,
        2 => 4,
        3 => 3,
        4 => 5,
        5 => 8,
        6 => 6,
        7 => 7,
        _ => crate::metadata_utils::get_exif_orientation(path),
    };

    // Ensure LibRaw's develop() pipeline uses the SAME orientation as our preview logic.
    // We explicitly set user_flip based on our authoritative decision.
    let final_lr_flip = match final_orientation {
        1 => 0,
        2 => 1,
        3 => 3,
        4 => 2,
        5 => 4,
        6 => 6,
        7 => 7,
        8 => 5,
        _ => 0,
    };
    processor.set_user_flip(final_lr_flip);

    // --- Performance Optimization: Try to use embedded preview to avoid expensive demosaicing ---
    let mut preview_opt = {
        // Step 1: Try platform-native loaders (WIC/ImageIO).
        // We pass Some(final_orientation) to force the system loader to respect our authoritative choice.
        #[cfg(target_os = "windows")]
        let res = crate::wic::load_via_wic(path, high_quality, Some(final_orientation));
        #[cfg(target_os = "macos")]
        let res =
            crate::macos_image_io::load_via_image_io(path, high_quality, Some(final_orientation));
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        let res: Result<ImageData, String> = Err("Unsupported".to_string());

        match res {
            Ok(ImageData::Static(img)) => Some(img),
            Ok(ImageData::Tiled(source)) => {
                let lim = hq_preview_max_side();
                let (pw, ph, p) = source.generate_preview(lim, lim);
                Some(DecodedImage::new(pw, ph, p))
            }
            _ => {
                // Step 2: Fallback to LibRaw's native thumbnail extraction if platform loader failed.
                // We use the same final_orientation to ensure perfect consistency.
                if let Ok(mut p) = processor.unpack_thumb() {
                    if final_orientation > 1 {
                        let pixels = p.take_rgba_owned();
                        if let Some(rgba) = image::RgbaImage::from_raw(p.width, p.height, pixels) {
                            let mut img = image::DynamicImage::ImageRgba8(rgba);
                            match final_orientation {
                                2 => img = img.fliph(),
                                3 => img = img.rotate180(),
                                4 => img = img.flipv(),
                                5 => img = img.fliph().rotate270(),
                                6 => img = img.rotate90(),
                                7 => img = img.fliph().rotate90(),
                                8 => img = img.rotate270(),
                                _ => {}
                            }
                            let rgba_rotated = img.to_rgba8();
                            p.set_rgba_buffer(
                                rgba_rotated.width(),
                                rgba_rotated.height(),
                                rgba_rotated.into_raw(),
                            );
                        }
                    }
                    Some(p)
                } else {
                    None
                }
            }
        }
    };

    // Sanitize: A zero-dimension image will cause a validation error in wgpu (Dimension X is zero).
    if let Some(ref p) = preview_opt {
        if p.width == 0 || p.height == 0 {
            log::warn!(
                "[Loader] Preview path returned a zero-dimension image for {:?}. Invalidate and fallback.",
                path.file_name().unwrap_or_default()
            );
            preview_opt = None;
        }
    }

    if let Some(p) = preview_opt.clone() {
        let hq_lim = hq_preview_max_side();
        let is_hq = p.width >= hq_lim || p.height >= hq_lim;
        // If !high_quality (performance mode), we use any preview to save energy/fans.
        // If high_quality is true, we only use it if it's large enough (HQ).
        if !high_quality || is_hq {
            log::debug!(
                "[Loader] Using embedded preview for {:?} ({}x{}, HQ={})",
                path,
                p.width,
                p.height,
                is_hq
            );
            return Ok(make_image_data(p));
        }
        // If we reach here, high_quality is true but preview is not HQ, so we fall through to develop.
    }

    // 2. Rule 1: High-Performance Synchronous Development for Small Images (< 64MP).
    if area < threshold
        && width <= crate::constants::ABSOLUTE_MAX_TEXTURE_SIDE
        && height <= crate::constants::ABSOLUTE_MAX_TEXTURE_SIDE
    {
        log::info!(
            "[Loader] RAW {}x{} ({:.1} MP) matches Rule 1 (Small). Synchronously extracting pixels...",
            width,
            height,
            area as f64 / 1_000_000.0
        );

        if let Ok(full_img) = processor.develop() {
            let warnings = processor.process_warnings();
            if warnings != 0 {
                log::info!(
                    "[Loader] LibRaw reported informational warnings (0x{:x}) for {:?}, proceeding with native pixels.",
                    warnings,
                    path
                );
            }

            let rgba = full_img.into_rgba8();
            if rgba.width() == 0 || rgba.height() == 0 {
                log::error!(
                    "[Loader] LibRaw developed a zero-dimension image for {:?}. Falling through to Rule 2.",
                    path
                );
            } else {
                let decoded = DecodedImage::new(rgba.width(), rgba.height(), rgba.into_raw());
                return Ok(ImageData::Static(decoded));
            }
        } else {
            log::error!("[Loader] Failed to develop Rule 1 pixels. Falling through to Rule 2.");
        }
    }

    // 3. Rule 2: Asynchronous Tiled Pipeline for Large Images (>= 64MP) or fallback.
    let preview = if let Some(p) = preview_opt {
        p
    } else {
        log::warn!(
            "[Loader] All fast RAW thumbnail paths failed for {:?}. Falling back to slow development...",
            path.file_name().unwrap_or_default()
        );
        processor.develop()?.to_rgba8().into()
    };

    let source = Arc::new(RawImageSource::new(
        path.clone(),
        preview.clone(),
        width,
        height,
        refine_tx,
        final_lr_flip,
    ));

    log::info!(
        "[Loader] RAW {}x{} ({:.1} MP) >= 64MP - Falling back to Async Tiled preview refinement.",
        width,
        height,
        area as f64 / 1_000_000.0
    );
    Ok(ImageData::Tiled(source))
}

/// A TiledImageSource that serves tiles from an in-memory byte buffer.
/// Primarily used for common formats (PNG, JPEG, etc.) that exceed the GPU's single texture limit.
pub struct MemoryImageSource {
    width: u32,
    height: u32,
    pixels: Arc<Vec<u8>>,
}

impl MemoryImageSource {
    pub fn new(width: u32, height: u32, pixels: Arc<Vec<u8>>) -> Self {
        Self {
            width,
            height,
            pixels,
        }
    }
}

impl TiledImageSource for MemoryImageSource {
    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn extract_tile(&self, x: u32, y: u32, w: u32, h: u32) -> Arc<Vec<u8>> {
        let mut tile_pixels = Vec::with_capacity((w * h * 4) as usize);
        let stride = self.width as usize * 4;

        for row in y..(y + h) {
            let start = (row as usize * stride) + (x as usize * 4);
            let end = start + (w as usize * 4);
            if end <= self.pixels.len() {
                tile_pixels.extend_from_slice(&self.pixels[start..end]);
            } else {
                // Safety fallback for out-of-bounds
                tile_pixels.resize(tile_pixels.len() + (w * 4) as usize, 0);
            }
        }
        Arc::new(tile_pixels)
    }

    fn generate_preview(&self, max_w: u32, max_h: u32) -> (u32, u32, Vec<u8>) {
        // Since we already have the full image in memory, we can use the image crate
        // to generate a high-quality downscaled preview.
        // OPTIMIZATION: Use ImageBuffer with reference (slice) to avoid cloning giant pixel buffer.
        if let Some(buf) = image::ImageBuffer::<image::Rgba<u8>, &[u8]>::from_raw(
            self.width,
            self.height,
            &self.pixels,
        ) {
            let img = image::imageops::thumbnail(&buf, max_w, max_h);
            (img.width(), img.height(), img.into_raw())
        } else {
            (0, 0, Vec::new())
        }
    }

    fn full_pixels(&self) -> Option<Arc<Vec<u8>>> {
        Some(Arc::clone(&self.pixels))
    }
}
