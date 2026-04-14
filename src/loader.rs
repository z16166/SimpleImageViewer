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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use std::sync::{Arc, Mutex};
use crossbeam_channel::{Receiver, Sender, TryRecvError};



#[derive(Debug, Clone)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>, // RGBA8
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
pub enum ImageData {
    Static(DecodedImage),
    /// Large image that exceeds the tiled threshold — kept in CPU RAM for on-demand tile extraction.
    LargeStatic(DecodedImage),
    /// Virtualized image source — tiles are decoded on-demand from disk or other sources.
    Tiled(std::sync::Arc<dyn TiledImageSource>),
    Animated(Vec<AnimationFrame>),
}

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
    pub pixels: Vec<u8>,
}

pub struct PreviewResult {
    pub index: usize,
    pub generation: u64,
    pub preview: DecodedImage,
}

pub enum LoaderOutput {
    Image(LoadResult),
    Tile(TileResult),
    Preview(PreviewResult),
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
}

impl ImageLoader {
    pub fn new() -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        let pool_builder = rayon::ThreadPoolBuilder::new()
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
                    Ok(_com) => {
                        rayon_thread.run()
                    }
                    Err(e) => {
                        log::error!("Failed to initialize COM on loader worker thread: {:?}", e);
                        // Still run the rayon thread tasks, but WIC calls will likely fail gracefully
                        rayon_thread.run()
                    }
                }
            })?;
            Ok(())
        });

        let pool = pool_builder.build()
            .expect("failed to create image loader thread pool");
        Self {
            tx, rx,
            loading: Arc::new(Mutex::new(HashMap::new())),
            current_gen: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            pool: Arc::new(pool),
        }
    }

    pub fn is_loading(&self, index: usize) -> bool {
        self.loading.lock().unwrap().contains_key(&index)
    }

    pub fn current_generation(&self, index: usize) -> u64 {
        self.loading.lock().unwrap().get(&index).copied().unwrap_or(0)
    }

    /// Update the global generation counter so stale preloads abort early.
    pub fn set_generation(&self, generation: u64) {
        self.current_gen.store(generation, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn request_load(&mut self, index: usize, generation: u64, path: PathBuf) {
        {
            let mut loading = self.loading.lock().unwrap();
            if loading.get(&index) == Some(&generation) {
                return;
            }
            loading.insert(index, generation);
        }

        // An AtomicBool to ensure only ONE of the two spawns actually performs the load.
        let claimed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let current_gen = Arc::clone(&self.current_gen);

        // Build shared state for both spawns
        let tx1 = self.tx.clone();
        let tx2 = self.tx.clone();
        let loading1 = Arc::clone(&self.loading);
        let loading2 = Arc::clone(&self.loading);
        let path2 = path.clone();
        let claimed1 = Arc::clone(&claimed);
        let claimed2 = Arc::clone(&claimed);
        let current_gen2 = Arc::clone(&current_gen);

        // SPAWN A: rayon pool (fast start when pool has capacity)
        self.pool.spawn(move || {
            // Stale check
            let global_gen = current_gen.load(std::sync::atomic::Ordering::Relaxed);
            if generation < global_gen {
                let loading = loading1.lock().unwrap();
                if loading.get(&index) != Some(&generation) {
                    log::debug!("[Loader] Aborting stale preload idx={} gen={} (cur={})", index, generation, global_gen);
                    return;
                }
            }
            // Try to claim the work
            if claimed1.compare_exchange(false, true, std::sync::atomic::Ordering::AcqRel, std::sync::atomic::Ordering::Relaxed).is_err() {
                return; // OS thread already claimed it
            }
            Self::do_load(index, generation, &path, tx1, loading1);
        });

        // SPAWN B: dedicated OS thread (safety net for pool saturation)
        std::thread::Builder::new()
            .name(format!("load-backup-{}", index))
            .spawn(move || {
                // Give the pool a small head start (50ms) — if the pool is free,
                // it will claim the work before we wake up.
                std::thread::sleep(std::time::Duration::from_millis(50));

                // Stale check
                let global_gen = current_gen2.load(std::sync::atomic::Ordering::Relaxed);
                if generation < global_gen {
                    let loading = loading2.lock().unwrap();
                    if loading.get(&index) != Some(&generation) {
                        return;
                    }
                }
                // Try to claim
                if claimed2.compare_exchange(false, true, std::sync::atomic::Ordering::AcqRel, std::sync::atomic::Ordering::Relaxed).is_err() {
                    return; // Pool already handled it
                }
                // Pool was saturated — we take over
                #[cfg(target_os = "windows")]
                let _com = crate::wic::ComGuard::new();
                log::debug!("[Loader] OS-thread fallback for idx={} gen={}", index, generation);
                Self::do_load(index, generation, &path2, tx2, loading2);
            })
            .ok();
    }

    /// Shared load logic used by both pool.spawn and OS thread fallback.
    fn do_load(
        index: usize,
        generation: u64,
        path: &PathBuf,
        tx: Sender<LoaderOutput>,
        loading_ref: Arc<Mutex<HashMap<usize, u64>>>,
    ) {
        let load_result = load_image_file(generation, index, path);

        if let Err(e) = &load_result.result {
            log::error!("[Loader] Load FAILED for index={}: {}", index, e);
        }

        let source_opt = if let Ok(ImageData::Tiled(ref source)) = load_result.result {
            Some(Arc::clone(source))
        } else {
            None
        };

        let _ = tx.send(LoaderOutput::Image(load_result));

        if let Some(source_cloned) = source_opt {
            let tx_cloned = tx.clone();
            let loading_p2 = Arc::clone(&loading_ref);

            std::thread::Builder::new()
                .name(format!("refine-{}", index))
                .spawn(move || {
                    #[cfg(target_os = "windows")]
                    let _com = crate::wic::ComGuard::new();

                    let still_valid = {
                        let loading = loading_p2.lock().unwrap();
                        loading.get(&index) == Some(&generation)
                    };

                    if still_valid {
                        let limit = if source_cloned.width() > 32768 || source_cloned.height() > 32768 {
                            4096
                        } else {
                            2048
                        };
                        let (pw, ph, pixels) = source_cloned.generate_preview(limit, limit);

                        let still_valid_after = {
                            let loading = loading_p2.lock().unwrap();
                            loading.get(&index) == Some(&generation)
                        };

                        if still_valid_after && pw > 0 && ph > 0 {
                            let _ = tx_cloned.send(LoaderOutput::Preview(PreviewResult {
                                index,
                                generation,
                                preview: DecodedImage { width: pw, height: ph, pixels },
                            }));
                        }
                    }
                })
                .ok();
        }
    }

    pub fn request_tile(&self, index: usize, generation: u64, source: std::sync::Arc<dyn TiledImageSource>, col: u32, row: u32) {
        let tx = self.tx.clone();
        let current_gen = self.current_gen.clone();
        self.pool.spawn(move || {
            // Check if this request is still relevant for the global counter
            if current_gen.load(std::sync::atomic::Ordering::Relaxed) > generation {
                return;
            }

            let tile_size = crate::tile_cache::TILE_SIZE;
            let x = col * tile_size;
            let y = row * tile_size;
            let full_w = source.width();
            let full_h = source.height();
            let tw = tile_size.min(full_w - x);
            let th = tile_size.min(full_h - y);
            
            // Telemetry: Start extraction
            log::debug!("[Loader] Tile Request: idx={}, gen={}, coord=({},{})", index, generation, col, row);

            let pixels = source.extract_tile(x, y, tw, th);
            
            let result = tx.send(LoaderOutput::Tile(TileResult {
                index,
                col,
                row,
                pixels,
            }));

            if result.is_err() {
                log::error!("[Loader] Failed to send tile result for ({},{})", col, row);
            }
        });
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

    pub fn cancel_all(&mut self) {
        // Clear the in-flight set so completed results are discarded in poll().
        // We cannot cancel work already submitted to rayon, but those results
        // will harmlessly be ignored once the cache is cleared.
        self.loading.lock().unwrap().clear();
        while self.rx.try_recv().is_ok() {}
    }
}

fn load_image_file(generation: u64, index: usize, path: &PathBuf) -> LoadResult {
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("unknown");

    let result = (|| -> Result<ImageData, String> {
        let ext = path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();
        // Get the system-native support registry for the current platform
        let is_system_native = if let Ok(reg) = crate::formats::get_registry().read() {
            reg.extensions.contains(&ext)
        } else {
            false
        };

        // 1. Try format-specific virtualized loaders first (e.g., PSB/PSD v2)
        if ext == "psd" || ext == "psb" {
            if let Ok(item) = load_psd(path) {
                return Ok(item);
            }
        }

        // 2. If natively supported by the OS (TIFF, RAW, etc.), prioritize system loaders (WIC/ImageIO)
        // Skip this step for animation-capable formats (GIF, WebP, APNG) so they use Step 3 instead.
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

        // 3. Try standard image-rs loaders (GIF, PNG, WebP, etc.)
        let result = match ext.as_str() {
            "gif" => load_gif(path),
            "png" | "apng" => load_png(path),
            "webp" => load_webp(path),
            "heif" | "heic" => load_heic(path),
            _ => load_static(path),
        };
        // 4. Final system-native fallback for any failures (e.g., unusual but supported files)
        if result.is_err() {
            #[cfg(target_os = "windows")]
            if let Ok(img) = crate::wic::load_via_wic(path) {
                return Ok(img);
            }
            #[cfg(target_os = "macos")]
            if let Ok(img) = crate::macos_image_io::load_via_image_io(path) {
                return Ok(img);
            }
        }

        result
    })();
    
    // Post-process EVERY result for logging and limits
    let mut preview: Option<DecodedImage> = None;

    let final_result = match result {
        Ok(ImageData::Tiled(source)) => {
            log::info!("[{}] Tiled image source active: {}x{} ({:.1} MP)", 
                file_name, source.width(), source.height(),
                (source.width() as f64 * source.height() as f64) / 1_000_000.0
            );
            
            // PHASE 1: Try instant thumbnail extraction (protected against panics)
            let t0 = std::time::Instant::now();
            let exif_thumb = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                extract_exif_thumbnail(path)
            }));
            match exif_thumb {
                Ok(Some(thumb)) => {
                    log::info!("[{}] EXIF thumbnail extracted in {:?}", file_name, t0.elapsed());
                    preview = Some(thumb);
                }
                Ok(None) => {
                    log::info!("[{}] No EXIF thumbnail found (took {:?}), generating 512px preview...", file_name, t0.elapsed());
                    let t1 = std::time::Instant::now();
                    let gen_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        source.generate_preview(512, 512)
                    }));
                    match gen_result {
                        Ok((pw, ph, p_pixels)) if pw > 0 && ph > 0 => {
                            log::info!("[{}] 512px preview generated ({}x{}) in {:?}", file_name, pw, ph, t1.elapsed());
                            preview = Some(DecodedImage { width: pw, height: ph, pixels: p_pixels });
                        }
                        Ok(_) => {
                            log::warn!("[{}] generate_preview returned empty/zero-size result in {:?}", file_name, t1.elapsed());
                        }
                        Err(e) => {
                            log::error!("[{}] generate_preview PANICKED: {:?} in {:?}", file_name, e, t1.elapsed());
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
            let pixel_count = decoded.width as u64 * decoded.height as u64;
            let max_side = decoded.width.max(decoded.height);
            let limit = crate::tile_cache::get_max_texture_side();
            
            if pixel_count >= crate::tile_cache::TILED_THRESHOLD || max_side > limit {
                log::info!("[{}] Decoded {}x{} ({:.1} MP) - Tiled Mode (limit={})", 
                    file_name, decoded.width, decoded.height, pixel_count as f64 / 1_000_000.0, limit);
                Ok(ImageData::LargeStatic(decoded))
            } else {
                log::info!("[{}] Decoded {}x{} ({:.1} MP) - Static Mode", 
                    file_name, decoded.width, decoded.height, pixel_count as f64 / 1_000_000.0);
                Ok(ImageData::Static(decoded))
            }
        }
        Ok(ImageData::Animated(frames)) => {
            if let Some(first) = frames.first() {
                let width = first.width;
                let height = first.height;
                let max_side = width.max(height);
                let limit = crate::tile_cache::get_max_texture_side();
                
                let total_bytes: usize = frames.iter().map(|f| f.pixels.len()).sum();
                let mb = total_bytes as f64 / (1024.0 * 1024.0);

                if max_side > limit {
                    log::warn!("[{}] Animated image ({}x{}) exceeds GPU limits. Falling back to tiled static mode.", file_name, width, height);
                    log::info!("[{}] Decoded {}x{} ({} frames, {:.1} MB) - Tiled Mode (limit={})", 
                        file_name, width, height, frames.len(), mb, limit);
                    Ok(ImageData::LargeStatic(DecodedImage {
                        width,
                        height,
                        pixels: first.pixels.clone(),
                    }))
                } else {
                    log::info!("[{}] Decoded {}x{} ({} frames, {:.1} MB) - Animated Mode", 
                        file_name, width, height, frames.len(), mb);
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
        other => other,
    };

    LoadResult { index, generation, result: final_result, preview }
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
    
    Ok(ImageData::Static(DecodedImage { width, height, pixels }))
}

fn process_animation_frames(raw_frames: Vec<image::Frame>, path: &PathBuf) -> Result<ImageData, String> {
    if raw_frames.len() <= 1 {
        return load_static(path);
    }

    let frames: Vec<AnimationFrame> = raw_frames.into_iter().map(|frame| {
        let (numer, denom) = frame.delay().numer_denom_ms();
        let delay_ms = if denom == 0 { 100 } else { numer / denom };
        // Standard browser behavior: delays <= 10ms are treated as 100ms
        let delay_ms = if delay_ms <= 10 { 100 } else { delay_ms };
        let buffer = frame.into_buffer();
        let (width, height) = buffer.dimensions();
        let pixels = buffer.into_raw();
        AnimationFrame {
            width,
            height,
            pixels,
            delay: Duration::from_millis(delay_ms as u64),
        }
    }).collect();

    Ok(ImageData::Animated(frames))
}

fn load_gif(path: &PathBuf) -> Result<ImageData, String> {
    use image::codecs::gif::GifDecoder;
    use image::AnimationDecoder;
    use std::io::BufReader;

    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let reader = BufReader::new(file);
    let decoder = GifDecoder::new(reader).map_err(|e| e.to_string())?;
    let raw_frames = decoder.into_frames()
        .collect_frames()
        .map_err(|e| e.to_string())?;

    process_animation_frames(raw_frames, path)
}

fn load_png(path: &PathBuf) -> Result<ImageData, String> {
    use image::codecs::png::PngDecoder;
    use image::AnimationDecoder;
    use std::io::BufReader;

    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let reader = BufReader::new(file);
    let decoder = PngDecoder::new(reader).map_err(|e| e.to_string())?;

    if !decoder.is_apng().map_err(|e| e.to_string())? {
        return load_static(path);
    }

    let raw_frames = decoder.apng()
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
    use image::codecs::webp::WebPDecoder;
    use image::AnimationDecoder;
    use std::io::BufReader;

    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let reader = BufReader::new(file);
    let decoder = WebPDecoder::new(reader).map_err(|e| e.to_string())?;
    let raw_frames = decoder.into_frames()
        .collect_frames()
        .map_err(|e| e.to_string())?;

    process_animation_frames(raw_frames, path)
}

// ---------------------------------------------------------------------------
// PSD / PSB (Photoshop Document / Large Document)
// ---------------------------------------------------------------------------

fn load_psd(path: &PathBuf) -> Result<ImageData, String> {
    // Step 1: Estimate memory requirement from header
    let (width, height, _channels, estimated_bytes) =
        crate::psb_reader::estimate_memory(path)?;
    let estimated_mb = estimated_bytes / (1024 * 1024);

    // Step 2: Check available RAM
    use sysinfo::System;
    let mut sys = System::new();
    sys.refresh_memory();
    let available_mb = sys.available_memory() / (1024 * 1024);

    // Reserve at least 1GB for the OS + app overhead
    let safe_available = available_mb.saturating_sub(1024);
    if estimated_mb > safe_available {
        return Err(format!(
            "Image requires ~{estimated_mb} MB RAM but only ~{safe_available} MB is available. \
             Please close other applications or convert to a smaller format."
        ));
    }

    log::info!(
        "PSD/PSB {}x{}: estimated {estimated_mb} MB, available {available_mb} MB — proceeding",
        width, height
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
        let psd_file = psd::Psd::from_bytes(&bytes)
            .map_err(|e| format!("Failed to parse PSD: {e}"))?;
        let w = psd_file.width();
        let h = psd_file.height();
        let pixels = psd_file.rgba();
        
        let img = DecodedImage { width: w, height: h, pixels };
        if (w as u64 * h as u64) > crate::tile_cache::TILED_THRESHOLD {
            Ok(ImageData::LargeStatic(img))
        } else {
            Ok(ImageData::Static(img))
        }
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

    Ok(ImageData::Static(DecodedImage { width, height, pixels: rgba }))
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
        let offset = exif_data.get_field(exif::Tag::JPEGInterchangeFormat, exif::In::THUMBNAIL)
            .and_then(|f| f.value.get_uint(0));
        let length = exif_data.get_field(exif::Tag::JPEGInterchangeFormatLength, exif::In::THUMBNAIL)
            .and_then(|f| f.value.get_uint(0));

        if let (Some(off), Some(len)) = (offset, length) {
            use std::io::{Seek, SeekFrom, Read};
            let mut f = std::fs::File::open(path).ok()?;
            f.seek(SeekFrom::Start(off as u64)).ok()?;
            let mut blob = vec![0u8; len as usize];
            if f.read_exact(&mut blob).is_ok() {
                if let Ok(img) = image::load_from_memory(&blob) {
                    let rgba = img.to_rgba8();
                    log::info!("[{}] Extracted EXIF thumbnail ({}x{}) from offset {}", 
                        path.file_name().and_then(|n| n.to_str()).unwrap_or("unknown"),
                        rgba.width(), rgba.height(), off
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
            max_size 
        }
    }

    pub fn insert(&mut self, index: usize, handle: egui::TextureHandle, orig_w: u32, orig_h: u32, tiled: bool, current_index: usize, total_count: usize) -> Option<usize> {
        self.textures.insert(index, handle);
        self.original_res.insert(index, (orig_w, orig_h));
        self.is_tiled.insert(index, tiled);
        self.evict(current_index, total_count)
    }

    /// Get the original image dimensions (not the texture/preview size).
    pub fn get_original_res(&self, index: usize) -> Option<(u32, u32)> {
        self.original_res.get(&index).copied()
    }

    /// Check if the image at index is a Tiled/Large image.

    pub fn get(&self, index: usize) -> Option<&egui::TextureHandle> {
        self.textures.get(&index)
    }

    pub fn contains(&self, index: usize) -> bool {
        self.textures.contains_key(&index)
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
        let to_remove = self.textures
            .keys()
            .copied()
            .max_by_key(|&idx| {
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
