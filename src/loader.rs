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
use std::path::PathBuf;
use std::time::Duration;
use crossbeam_channel::{Receiver, Sender, TryRecvError};



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

pub struct ImageLoader {
    tx: Sender<LoadResult>,
    pub rx: Receiver<LoadResult>,
    /// Maps image index -> latest requested generation ID.
    loading: HashMap<usize, u64>,
    pool: rayon::ThreadPool,
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
                let _com = crate::wic::ComGuard::new().expect("Failed to initialize COM on loader worker");
                rayon_thread.run()
            })?;
            Ok(())
        });

        let pool = pool_builder.build()
            .expect("failed to create image loader thread pool");
        Self { tx, rx, loading: HashMap::new(), pool }
    }

    pub fn is_loading(&self, index: usize) -> bool {
        self.loading.contains_key(&index)
    }

    pub fn request_load(&mut self, index: usize, generation: u64, path: PathBuf) {
        if self.loading.get(&index) == Some(&generation) {
            return;
        }
        self.loading.insert(index, generation);
        let tx = self.tx.clone();
        // Use the bounded thread pool instead of spawning a new OS thread each time.
        self.pool.spawn(move || {
            let result = load_image_file(generation, index, &path);
            let _ = tx.send(result);
        });
    }

    pub fn poll(&mut self) -> Option<LoadResult> {
        match self.rx.try_recv() {
            Ok(result) => {
                // Only remove from loading set if the generation matches what we expect
                if self.loading.get(&result.index) == Some(&result.generation) {
                    self.loading.remove(&result.index);
                }
                Some(result)
            }
            Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => None,
        }
    }

    pub fn cancel_all(&mut self) {
        // Clear the in-flight set so completed results are discarded in poll().
        // We cannot cancel work already submitted to rayon, but those results
        // will harmlessly be ignored once the cache is cleared.
        self.loading.clear();
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
            
            // Generate preview IMMEDIATELY on the worker thread to avoid UI blocking
            let (pw, ph, p_pixels) = source.generate_preview(2048, 2048);
            preview = Some(DecodedImage { width: pw, height: ph, pixels: p_pixels });
            
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
// Texture cache
// ---------------------------------------------------------------------------

pub struct TextureCache {
    pub textures: HashMap<usize, egui::TextureHandle>,
    max_size: usize,
}

impl TextureCache {
    pub fn new(max_size: usize) -> Self {
        Self { textures: HashMap::new(), max_size }
    }

    pub fn insert(&mut self, index: usize, handle: egui::TextureHandle, current_index: usize) -> Option<usize> {
        self.textures.insert(index, handle);
        self.evict(current_index)
    }

    pub fn get(&self, index: usize) -> Option<&egui::TextureHandle> {
        self.textures.get(&index)
    }

    pub fn contains(&self, index: usize) -> bool {
        self.textures.contains_key(&index)
    }

    pub fn clear(&mut self) {
        self.textures.clear();
    }

    fn evict(&mut self, current_index: usize) -> Option<usize> {
        if self.textures.len() <= self.max_size {
            return None;
        }
        // Evict the texture farthest from the current index
        let to_remove = self.textures
            .keys()
            .copied()
            .max_by_key(|&idx| (idx as isize - current_index as isize).unsigned_abs());
        
        if let Some(idx) = to_remove {
            self.textures.remove(&idx);
            Some(idx)
        } else {
            None
        }
    }
}
