use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;
use std::fs::File;
use std::io::BufReader;
use crossbeam_channel::{Receiver, Sender, TryRecvError};



pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>, // RGBA8
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
    Animated(Vec<AnimationFrame>),
}

pub struct LoadResult {
    pub index: usize,
    pub generation: u64,
    pub result: Result<ImageData, String>,
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
        let pool = rayon::ThreadPoolBuilder::new()
            .thread_name(|i| format!("img-loader-{i}"))
            .build()
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
    let result = (|| -> Result<ImageData, String> {
        let ext = path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();

        match ext.as_str() {
            "gif" => load_gif(path),
            "png" | "apng" => load_png(path),
            "webp" => load_webp(path),
            "psd" | "psb" => load_psd(path),
            "heif" | "heic" => load_heic(path),
            _ => load_static(path),
        }
    })();
    LoadResult { index, generation, result }
}

fn load_static(path: &PathBuf) -> Result<ImageData, String> {
    use image::ImageReader;

    let reader = ImageReader::open(path).map_err(|e| e.to_string())?;
    let mut decoder = reader.with_guessed_format().map_err(|e| e.to_string())?;
    // Remove the default memory limit (512MB) to allow gigapixel images
    decoder.no_limits();
    let img = match decoder.decode() {
        Ok(img) => img,
        Err(e) => {
            // If the standard decoder fails on a TIFF, try tiff 0.11 which supports more formats
            let path_str = path.to_string_lossy().to_lowercase();
            if path_str.ends_with(".tif") || path_str.ends_with(".tiff") {
                log::info!("Standard TIFF decoder failed, trying tiff 0.11 fallback: {}", e);
                return load_tiff_fallback(path);
            }
            return Err(e.to_string());
        }
    };
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    let pixels = rgba.into_raw();
    let pixel_count = width as u64 * height as u64;
    log::info!("Decoded {}x{} ({:.1} MP, {:.0} MB RGBA)", width, height,
        pixel_count as f64 / 1e6, pixel_count as f64 * 4.0 / (1024.0 * 1024.0));
    if pixel_count >= crate::tile_cache::TILED_THRESHOLD {
        Ok(ImageData::LargeStatic(DecodedImage { width, height, pixels }))
    } else {
        Ok(ImageData::Static(DecodedImage { width, height, pixels }))
    }
}

/// Fallback TIFF decoder for Palette-indexed (RGBPalette) images.
///
/// Neither tiff 0.9 nor 0.11 support decoding Palette TIFFs. We work around this by:
/// 1. Reading the ColorMap from the original file (tiff crate can parse tags fine).
/// 2. Patching the PhotometricInterpretation tag in a memory copy: Palette(3) → Grayscale(1).
/// 3. Feeding the patched data to the decoder, which now happily decompresses the indices.
/// 4. Manually mapping the grayscale "pixels" (actually palette indices) to true RGBA via ColorMap.
fn load_tiff_fallback(path: &PathBuf) -> Result<ImageData, String> {
    // Step 1: Read ColorMap from the original file
    let file = File::open(path).map_err(|e| e.to_string())?;
    let mut decoder = tiff::decoder::Decoder::new(BufReader::new(file))
        .map_err(|e| e.to_string())?;
    let color_map = decoder.get_tag_u16_vec(tiff::tags::Tag::ColorMap)
        .map_err(|e| format!("No ColorMap in TIFF: {}", e))?;
    let palette_size = color_map.len() / 3;
    if palette_size == 0 {
        return Err("Empty TIFF ColorMap".to_string());
    }

    // Step 2: Read entire file into memory and patch PhotometricInterpretation
    let mut data = std::fs::read(path).map_err(|e| e.to_string())?;
    if !patch_tiff_photometric(&mut data) {
        return Err("Failed to patch TIFF PhotometricInterpretation tag".to_string());
    }

    // Step 3: Decode the patched data as "grayscale" (actually palette indices)
    let cursor = std::io::Cursor::new(&data);
    let mut decoder = tiff::decoder::Decoder::new(cursor)
        .map_err(|e| format!("Failed to decode patched TIFF: {}", e))?
        .with_limits(tiff::decoder::Limits::unlimited());

    let (width, height) = decoder.dimensions().map_err(|e| e.to_string())?;
    let result = decoder.read_image().map_err(|e| format!("Patched TIFF decode failed: {}", e))?;

    // Step 4: Map indices to RGBA using ColorMap
    let indices = match result {
        tiff::decoder::DecodingResult::U8(d) => d,
        _ => return Err("Expected U8 indices from patched TIFF".to_string()),
    };

    let pixel_count = (width as usize) * (height as usize);
    let mut rgba = Vec::with_capacity(pixel_count * 4);
    for i in 0..pixel_count {
        if i >= indices.len() { break; }
        let idx = indices[i] as usize;
        if idx < palette_size {
            let r = (color_map[idx] >> 8) as u8;
            let g = (color_map[idx + palette_size] >> 8) as u8;
            let b = (color_map[idx + 2 * palette_size] >> 8) as u8;
            rgba.extend_from_slice(&[r, g, b, 255]);
        } else {
            rgba.extend_from_slice(&[0, 0, 0, 255]);
        }
    }

    log::info!("Decoded Palette TIFF {}x{} via in-memory patch fallback", width, height);

    let decoded = DecodedImage { width, height, pixels: rgba };
    if (pixel_count as u64) >= crate::tile_cache::TILED_THRESHOLD {
        Ok(ImageData::LargeStatic(decoded))
    } else {
        Ok(ImageData::Static(decoded))
    }
}

/// Patch TIFF tag 262 (PhotometricInterpretation) from Palette(3) to Grayscale(1) in-place.
/// Returns true if the patch was applied successfully.
fn patch_tiff_photometric(data: &mut [u8]) -> bool {
    if data.len() < 8 { return false; }

    // Determine byte order
    let big_endian = match (data[0], data[1]) {
        (b'M', b'M') => true,
        (b'I', b'I') => false,
        _ => return false,
    };

    let read_u16 = |d: &[u8], off: usize| -> u16 {
        if big_endian {
            u16::from_be_bytes([d[off], d[off + 1]])
        } else {
            u16::from_le_bytes([d[off], d[off + 1]])
        }
    };
    let read_u32 = |d: &[u8], off: usize| -> u32 {
        if big_endian {
            u32::from_be_bytes([d[off], d[off + 1], d[off + 2], d[off + 3]])
        } else {
            u32::from_le_bytes([d[off], d[off + 1], d[off + 2], d[off + 3]])
        }
    };
    let write_u16 = |d: &mut [u8], off: usize, val: u16| {
        let bytes = if big_endian { val.to_be_bytes() } else { val.to_le_bytes() };
        d[off] = bytes[0];
        d[off + 1] = bytes[1];
    };

    let magic = read_u16(data, 2);

    // Standard TIFF (magic=42): 4-byte offsets, 12-byte IFD entries
    if magic == 42 {
        let ifd_offset = read_u32(data, 4) as usize;
        if ifd_offset + 2 > data.len() { return false; }
        let entry_count = read_u16(data, ifd_offset) as usize;

        for i in 0..entry_count {
            let entry_off = ifd_offset + 2 + i * 12;
            if entry_off + 12 > data.len() { return false; }
            let tag = read_u16(data, entry_off);
            if tag == 262 {
                // PhotometricInterpretation: value is at offset+8 (SHORT, count=1)
                let val = read_u16(data, entry_off + 8);
                if val == 3 { // Palette
                    write_u16(data, entry_off + 8, 1); // → Grayscale (BlackIsZero)
                    // Also set SamplesPerPixel tag to 1 if needed (usually already 1 for palette)
                    return true;
                }
            }
        }
    }
    // BigTIFF (magic=43): 8-byte offsets, 20-byte IFD entries
    else if magic == 43 {
        if data.len() < 16 { return false; }
        let read_u64 = |d: &[u8], off: usize| -> u64 {
            if big_endian {
                u64::from_be_bytes([d[off], d[off+1], d[off+2], d[off+3], d[off+4], d[off+5], d[off+6], d[off+7]])
            } else {
                u64::from_le_bytes([d[off], d[off+1], d[off+2], d[off+3], d[off+4], d[off+5], d[off+6], d[off+7]])
            }
        };
        let ifd_offset = read_u64(data, 8) as usize;
        if ifd_offset + 8 > data.len() { return false; }
        let entry_count = read_u64(data, ifd_offset) as usize;

        for i in 0..entry_count {
            let entry_off = ifd_offset + 8 + i * 20;
            if entry_off + 20 > data.len() { return false; }
            let tag = read_u16(data, entry_off);
            if tag == 262 {
                let val = read_u16(data, entry_off + 12);
                if val == 3 {
                    write_u16(data, entry_off + 12, 1);
                    return true;
                }
            }
        }
    }

    false
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

    let (w, h, pixels) = if version == 2 {
        // PSB v2: use our custom streaming reader
        log::info!("Using custom PSB reader for v2 format");
        let composite = crate::psb_reader::read_composite(path)?;
        (composite.width, composite.height, composite.pixels)
    } else {
        // PSD v1: use the psd crate (reads entire file into memory)
        let bytes = std::fs::read(path).map_err(|e| format!("Failed to read PSD: {e}"))?;
        let psd_file = psd::Psd::from_bytes(&bytes)
            .map_err(|e| format!("Failed to parse PSD: {e}"))?;
        (psd_file.width(), psd_file.height(), psd_file.rgba())
    };

    let pixel_count = w as u64 * h as u64;
    log::info!("PSD/PSB decoded {}x{} ({:.1} MP, {:.0} MB RGBA)", w, h,
        pixel_count as f64 / 1e6, pixel_count as f64 * 4.0 / (1024.0 * 1024.0));

    if pixel_count >= crate::tile_cache::TILED_THRESHOLD {
        Ok(ImageData::LargeStatic(DecodedImage { width: w, height: h, pixels }))
    } else {
        Ok(ImageData::Static(DecodedImage { width: w, height: h, pixels }))
    }
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

    let pixel_count = width as u64 * height as u64;
    log::info!("HEIC decoded {}x{} ({:.1} MP, {:.0} MB RGBA)", width, height,
        pixel_count as f64 / 1e6, pixel_count as f64 * 4.0 / (1024.0 * 1024.0));

    if pixel_count >= crate::tile_cache::TILED_THRESHOLD {
        Ok(ImageData::LargeStatic(DecodedImage { width, height, pixels: rgba }))
    } else {
        Ok(ImageData::Static(DecodedImage { width, height, pixels: rgba }))
    }
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
