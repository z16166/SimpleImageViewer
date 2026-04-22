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

use crossbeam_channel::{Receiver, Sender, TryRecvError};
use std::sync::LazyLock;
use crate::constants::{
    MAX_QUALITY_PREVIEW_SIZE, RGBA_CHANNELS, BYTES_PER_MB, 
    BYTES_PER_GB, DEFAULT_PREVIEW_SIZE, DEFAULT_ANIMATION_DELAY_MS,
    MIN_ANIMATION_DELAY_THRESHOLD_MS
};
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

pub static PREVIEW_LIMIT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(MAX_QUALITY_PREVIEW_SIZE / 2);

/// Dedicated pool for heavy high-quality preview generation (refinement).
/// Limited to 2 threads to prevent OOM when multiple giant images are switched rapidly.
static REFINEMENT_POOL: LazyLock<rayon::ThreadPool> = LazyLock::new(|| {
    match rayon::ThreadPoolBuilder::new()
        .num_threads(2)
        .thread_name(|i| format!("refinement-worker-{}", i))
        .build() {
            Ok(p) => p,
            Err(e) => {
                log::error!("[Loader] Failed to create refinement pool: {}. Falling back to default pool.", e);
                rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap()
            }
        }
});

use crate::raw_processor::RawProcessor;
use parking_lot::RwLock as PLRwLock;
use image::{DynamicImage, GenericImageView};

#[derive(Debug, Clone)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>, // RGBA8
}

impl From<image::RgbaImage> for DecodedImage {
    fn from(img: image::RgbaImage) -> Self {
        let (width, height) = img.dimensions();
        Self {
            width,
            height,
            pixels: img.into_raw(),
        }
    }
}

/// Interface for images that can provide pixel data in tiles/chunks on demand.
pub trait TiledImageSource: Send + Sync {
    fn width(&self) -> u32;
    fn height(&self) -> u32;
    /// Extract a rectangular region of the image as RGBA8.
    fn extract_tile(&self, x: u32, y: u32, w: u32, h: u32) -> Vec<u8>;
    /// Generate a downscaled preview of the full image.
    fn generate_preview(&self, max_w: u32, max_h: u32) -> (u32, u32, Vec<u8>);
    /// Optionally provide the full pixel buffer if already in memory.
    fn full_pixels(&self) -> Option<std::sync::Arc<Vec<u8>>>;
    /// Trigger background refinement to replace preview data with full-quality pixels.
    /// Default no-op; only RAW sources need background demosaicing.
    fn request_refinement(&self, _index: usize, _generation: u64) {}
}

/// A single frame of an animated image.
#[derive(Debug, Clone)]
pub struct AnimationFrame {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>, // RGBA8
    pub delay: Duration,
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
    pub _generation: u64,
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
}

impl ImageLoader {
    pub fn new() -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        let (refine_tx, refine_rx): (Sender<RefinementRequest>, Receiver<RefinementRequest>) = crossbeam_channel::unbounded();
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
                log::error!("[Loader] Failed to create image loader thread pool: {}. Falling back to minimal pool.", e);
                rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap()
            }
        };

        let current_gen = Arc::new(std::sync::atomic::AtomicU64::new(0));
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

                            // Convert to RGBA bits
                            let rgba = full_img.to_rgba8();
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
                            let limit = crate::constants::MAX_QUALITY_PREVIEW_SIZE;
                            let scaled = dynamic.thumbnail(limit, limit);
                            let prev_rgba = scaled.to_rgba8();
                            let preview = DecodedImage {
                                width: prev_rgba.width(),
                                height: prev_rgba.height(),
                                pixels: prev_rgba.into_raw(),
                            };
                            
                            let mut dev_lock = req.developed_image.write();
                            *dev_lock = Some(dynamic);
                            drop(dev_lock);

                            let _ = worker_tx.send(LoaderOutput::Preview(PreviewResult {
                                index: req.index,
                                _generation: req.generation,
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
        }
    }

    pub fn is_loading(&self, index: usize) -> bool {
        self.loading.lock().unwrap().contains_key(&index)
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

    pub fn request_load(&mut self, index: usize, generation: u64, path: PathBuf) {
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
            if generation < global_gen {
                let loading = loading1.lock().unwrap();
                if loading.get(&index) != Some(&generation) {
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
            Self::do_load(index, generation, &path1, tx1, rtx1, loading1, current_gen1);
        });

        // Fallback loader: If the primary thread pool is congested or stalls,
        // we spawn a delayed task to ensure the image eventually loads.
        // NOTE: We use std::thread::spawn here instead of self.pool.spawn to ensure
        // this fallback runs independently of the worker pool's current congestion state.
        std::thread::Builder::new()
            .name(format!("loader-fallback-{}", index))
            .spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(50));
                let global_gen = current_gen2.load(std::sync::atomic::Ordering::Relaxed);
                if generation < global_gen {
                    if let Some(g) = loading2.lock().unwrap().get(&index) {
                        if *g != generation {
                            return;
                        }
                    }
                }
                if claimed2
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
                #[cfg(target_os = "windows")]
                let _com = crate::wic::ComGuard::new();
                Self::do_load(index, generation, &path2, tx2, rtx2, loading2, current_gen2);
            })
            .ok();
    }

    fn do_load(
        index: usize,
        generation: u64,
        path: &PathBuf,
        tx: Sender<LoaderOutput>,
        refine_tx: Sender<RefinementRequest>,
        _loading_ref: Arc<Mutex<HashMap<usize, u64>>>,
        current_gen: Arc<std::sync::atomic::AtomicU64>,
    ) {
        let load_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            load_image_file(generation, index, path, tx.clone(), refine_tx.clone())
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

        let _ = tx.send(LoaderOutput::Image(load_result.clone()));

        if let Ok(ImageData::Tiled(source)) = load_result.result {
            let tx_cloned = tx.clone();
            let index = index;
            let generation = generation;
            let gen_ref = Arc::clone(&current_gen);

            REFINEMENT_POOL.spawn(move || {
                // Staleness check: Abort if the user has navigated to a new image
                if gen_ref.load(std::sync::atomic::Ordering::Relaxed) > generation {
                    return;
                }

                #[cfg(target_os = "windows")]
                let _com = crate::wic::ComGuard::new();

                let limit = crate::constants::MAX_QUALITY_PREVIEW_SIZE;
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
                            _generation: generation,
                            result: Ok(DecodedImage {
                                width: pw,
                                height: ph,
                                pixels: p_pixels,
                            }),
                        }));
                    }
                    Err(e) => {
                        log::error!("[Loader] High-quality refinement PANICKED: {:?}", e);
                    }
                    _ => {}
                }
            });
        }
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

    pub fn poll(&mut self) -> Option<LoaderOutput> {
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

    /// Clear all pending tile requests from the queue.
    /// Called on zoom change to discard tiles from stale zoom levels.
    pub fn flush_tile_queue(&self) {
        let (lock, _) = &*self.tile_queue;
        lock.lock().unwrap().clear();
    }

    pub fn cancel_all(&mut self) {
        self.loading.lock().unwrap().clear();
        {
            let (lock, _) = &*self.tile_queue;
            lock.lock().unwrap().clear();
        }
        while self.rx.try_recv().is_ok() {}
    }
}

fn load_image_file(generation: u64, index: usize, path: &PathBuf, _tx: Sender<LoaderOutput>, refine_tx: Sender<RefinementRequest>) -> LoadResult {
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

        if ext == "psd" || ext == "psb" {
            if let Ok(item) = load_psd(path) {
                return Ok(item);
            }
        }

        let is_raw = crate::raw_processor::is_raw_extension(&ext);

        if is_raw {
            return load_raw(index, generation, path, refine_tx.clone());
        }

        if is_system_native && !is_maybe_animated(&ext) {
            #[cfg(target_os = "windows")]
            if let Ok(img) = crate::wic::load_via_wic(path) {
                return Ok(img);
            }
            #[cfg(target_os = "macos")]
            if let Ok(img) = crate::macos_image_io::load_via_image_io(path) {
                return Ok(img);
            }
            #[cfg(target_os = "linux")]
            if ext == "tif" || ext == "tiff" {
                if let Ok(img) = crate::linux_tiff::load_via_libtiff(path) {
                    return Ok(img);
                }
            }
        }

        let result = match ext.as_str() {
            "gif" => load_gif(path),
            "png" | "apng" => load_png(path),
            "webp" => load_webp(path),
            "heif" | "heic" => load_heic(path),
            _ => load_static(path),
        };
        if result.is_err() {
            #[cfg(target_os = "windows")]
            if let Ok(img) = crate::wic::load_via_wic(path) {
                return Ok(img);
            }
            #[cfg(target_os = "macos")]
            if let Ok(img) = crate::macos_image_io::load_via_image_io(path) {
                return Ok(img);
            }

            // Last resort: Detect format by content (magic bytes)
            if let Ok(retry_img) = load_via_content_detection(path) {
                log::info!("[{}] Successfully recovered via content-based detection", file_name);
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
                            preview = Some(DecodedImage {
                                width: pw,
                                height: ph,
                                pixels: p_pixels,
                            });
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
        Ok(ImageData::Static(decoded)) => {
            Ok(make_image_data(decoded))
        }
        Ok(ImageData::Animated(frames)) => {
            if let Some(first) = frames.first() {
                let width = first.width;
                let height = first.height;
                let max_side = width.max(height);
                let limit = crate::tile_cache::get_max_texture_side();

                let total_bytes: usize = frames.iter().map(|f| f.pixels.len()).sum();
                let mb = total_bytes as f64 / (BYTES_PER_MB as f64);

                if max_side > limit {
                    log::warn!(
                        "[{}] Animated image ({}x{}) exceeds GPU limits. Falling back to tiled static mode.",
                        file_name,
                        width,
                        height
                    );
                    Ok(make_image_data(DecodedImage {
                        width,
                        height,
                        pixels: first.pixels.clone(),
                    }))
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
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    let pixels = rgba.into_raw();

    Ok(make_image_data(DecodedImage {
        width,
        height,
        pixels,
    }))
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
            let delay_ms = if denom == 0 { DEFAULT_ANIMATION_DELAY_MS } else { numer / denom };
            // Standard browser behavior: delays <= 10ms are treated as 100ms
            let delay_ms = if delay_ms <= MIN_ANIMATION_DELAY_THRESHOLD_MS { DEFAULT_ANIMATION_DELAY_MS } else { delay_ms };
            let buffer = frame.into_buffer();
            let (width, height) = buffer.dimensions();
            let pixels = buffer.into_raw();
            AnimationFrame {
                width,
                height,
                pixels,
                delay: Duration::from_millis(delay_ms as u64),
            }
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

        let img = DecodedImage {
            width: w,
            height: h,
            pixels,
        };
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

    Ok(make_image_data(DecodedImage {
        width,
        height,
        pixels: rgba,
    }))
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
            img.width, img.height, pixel_count as f64 / 1_000_000.0, limit, tiled_limit as f64 / 1_000_000.0
        );
        ImageData::Tiled(Arc::new(MemoryImageSource::new(img.width, img.height, img.pixels)))
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
        image::ImageFormat::Tiff => {
            #[cfg(target_os = "linux")]
            return crate::linux_tiff::load_via_libtiff(path);
            #[cfg(not(target_os = "linux"))]
            return load_static(path);
        }
        // Standard single-frame formats handled by load_static
        image::ImageFormat::Jpeg | image::ImageFormat::Bmp | image::ImageFormat::Ico |
        image::ImageFormat::Pnm | image::ImageFormat::Tga | image::ImageFormat::Dds |
        image::ImageFormat::Hdr | image::ImageFormat::Farbfeld | image::ImageFormat::OpenExr |
        image::ImageFormat::Avif | image::ImageFormat::Qoi => load_static(path),
        _ => Err(rust_i18n::t!("error.unsupported_detected_format", format = format!("{:?}", format)).to_string()),
    }
}

fn load_via_content_detection(path: &PathBuf) -> Result<ImageData, String> {
    use std::io::{Read};
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
        if sub == b"heic" || sub == b"heix" || sub == b"hevc" || sub == b"hevx" || sub == b"mif1" || sub == b"msf1" {
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
                    let rgba = img.to_rgba8();
                    log::info!(
                        "[{}] Extracted EXIF thumbnail ({}x{}) from offset {}",
                        path.file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("unknown"),
                        rgba.width(),
                        rgba.height(),
                        off
                    );
                    return Some(DecodedImage {
                        width: rgba.width(),
                        height: rgba.height(),
                        pixels: rgba.into_raw(),
                    });
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

    pub fn clear(&mut self) {
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
}

impl RawImageSource {
    pub fn new(
        path: PathBuf,
        preview: DecodedImage,
        raw_width: u32,
        raw_height: u32,
        refine_tx: Sender<RefinementRequest>,
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
        let rgba = if let Some(buf) = image::RgbaImage::from_raw(preview.width, preview.height, preview.pixels) {
            buf
        } else {
            log::error!("[Loader] Failed to create preview RGBA image buffer ({}x{}) for {:?}", preview.width, preview.height, path);
            image::RgbaImage::new(1, 1)
        };
        let preview_dyn = DynamicImage::ImageRgba8(rgba);

        let developed_image = Arc::new(PLRwLock::new(Some(preview_dyn)));

        Self {
            path,
            width: raw_width,
            height: raw_height,
            developed_image,
            refine_tx,
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

    fn extract_tile(&self, x: u32, y: u32, w: u32, h: u32) -> Vec<u8> {
        let img_lock = self.developed_image.read();
        if let Some(ref img) = *img_lock {
            let (iw, ih) = img.dimensions();
            if iw == self.width && ih == self.height {
                // Full-res developed image available — direct crop, no scaling needed.
                let crop = img.crop_imm(x, y, w, h);
                crop.to_rgba8().into_raw()
            } else {
                // Preview image (smaller than RAW dimensions).
                // Map tile coordinates from RAW sensor space → preview space,
                // extract the corresponding small region, then scale it up to
                // the requested tile size. This only allocates ~1MB per tile
                // (e.g. 512×512×4) instead of ~400MB for the whole image.
                let scale_x = iw as f64 / self.width as f64;
                let scale_y = ih as f64 / self.height as f64;
                let px = (x as f64 * scale_x) as u32;
                let py = (y as f64 * scale_y) as u32;
                let pw = ((w as f64 * scale_x).ceil() as u32).min(iw.saturating_sub(px)).max(1);
                let ph = ((h as f64 * scale_y).ceil() as u32).min(ih.saturating_sub(py)).max(1);
                let crop = img.crop_imm(px, py, pw, ph);
                let resized = crop.resize_exact(w, h, image::imageops::FilterType::Triangle);
                resized.to_rgba8().into_raw()
            }
        } else {
            vec![0; (w * h * RGBA_CHANNELS as u32) as usize]
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
        log::info!("[RawImageSource] Triggering refinement for index={}, gen={}", index, generation);
        let _ = self.refine_tx.send(RefinementRequest {
            path: self.path.clone(),
            index,
            generation,
            developed_image: self.developed_image.clone(),
        });
    }
}

fn load_raw(
    _index: usize,
    _generation: u64,
    path: &PathBuf,
    refine_tx: Sender<RefinementRequest>,
) -> Result<ImageData, String> {
    // 1. Initialize Processor and try to open header
    let mut processor = RawProcessor::new().ok_or_else(|| rust_i18n::t!("error.libraw_init").to_string())?;
    
    if let Err(e) = processor.open(path) {
        log::warn!("[Loader] LibRaw could not open {:?}: {}. Falling back to Rule 2 (WIC/ImageIO) only.", path, e);
        #[cfg(target_os = "windows")]
        return crate::wic::load_via_wic(path);
        #[cfg(target_os = "macos")]
        return crate::macos_image_io::load_via_image_io(path);
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        return Err(format!("LibRaw failed and no platform fallback available: {}", e));
    }

    let (width, height) = (processor.width() as u32, processor.height() as u32);
    let area = width as u64 * height as u64;
    let threshold = crate::tile_cache::TILED_THRESHOLD.load(std::sync::atomic::Ordering::Relaxed);

    // 2. Rule 1: < 64MP pictures MUST entirely pre-read TRUE RAW PIXELS natively.
    // They completely ignore embedded JPEGs and do not use Tiled presentation at all.
    // CRITICAL: We also check for GPU texture limits (8192px). If exceeded, we MUST tile.
    if area < threshold && width <= crate::constants::ABSOLUTE_MAX_TEXTURE_SIDE && height <= crate::constants::ABSOLUTE_MAX_TEXTURE_SIDE {
        log::info!("[Loader] RAW {}x{} ({:.1} MP) < 64MP and fits in GPU. Synchronously extracting original pixels...", 
            width, height, area as f64 / 1_000_000.0);
        
        if let Ok(full_img) = processor.develop() {
            let warnings = processor.process_warnings();
            if warnings != 0 {
                log::info!("[Loader] LibRaw reported informational warnings (0x{:x}) for {:?}, proceeding anyway.", warnings, path);
            }
            
            let rgba = full_img.to_rgba8();
            let decoded = DecodedImage {
                width: rgba.width(),
                height: rgba.height(),
                pixels: rgba.into_raw(),
            };
            return Ok(ImageData::Static(decoded));
        } else {
            log::error!("[Loader] Failed to synchronously extract RAW pixels. Falling back to Rule 2/Preview...");
        }
    }

    // 3. Rule 2: Giant RAWs (>= 64MP) (or fallback from errors) use the Tiled async pipeline.
    // We fetch a preview to show instantly, while original pixels decode in background.
    #[cfg(target_os = "windows")]
    let preview_res = crate::wic::load_via_wic(path);
    #[cfg(target_os = "macos")]
    let preview_res = crate::macos_image_io::load_via_image_io(path);
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let preview_res: Result<ImageData, String> = Err("Unsupported".to_string());

    let preview = match preview_res {
        Ok(ImageData::Static(img)) => img,
        Ok(ImageData::Tiled(source)) => {
            let (pw, ph, p) = source.generate_preview(MAX_QUALITY_PREVIEW_SIZE, MAX_QUALITY_PREVIEW_SIZE);
            DecodedImage { width: pw, height: ph, pixels: p }
        }
        _ => {
            match processor.unpack_thumb() {
                Ok(thumb) => thumb,
                Err(e) => {
                    log::warn!("[Loader] LibRaw fast thumbnail failed for {:?}: {}. Falling back to low-quality develop...", path, e);
                    processor.develop()?.to_rgba8().into()
                }
            }
        }
    };

    let source = Arc::new(RawImageSource::new(
        path.clone(),
        preview.clone(),
        width,
        height,
        refine_tx,
    ));

    log::info!("[Loader] RAW {}x{} ({:.1} MP) >= 64MP - Falling back to Async Tiled preview refinement.", 
        width, height, area as f64 / 1_000_000.0);
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
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Self {
        Self {
            width,
            height,
            pixels: Arc::new(pixels),
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

    fn extract_tile(&self, x: u32, y: u32, w: u32, h: u32) -> Vec<u8> {
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
        tile_pixels
    }

    fn generate_preview(&self, max_w: u32, max_h: u32) -> (u32, u32, Vec<u8>) {
        // Since we already have the full image in memory, we can use the image crate
        // to generate a high-quality downscaled preview.
        // OPTIMIZATION: Use ImageBuffer with reference (slice) to avoid cloning giant pixel buffer.
        if let Some(buf) = image::ImageBuffer::<image::Rgba<u8>, &[u8]>::from_raw(self.width, self.height, &self.pixels) {
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
