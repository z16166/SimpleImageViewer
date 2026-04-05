use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;
use crossbeam_channel::{Receiver, Sender, TryRecvError};

/// Maximum concurrent image decode tasks.
/// = 1 (current) + PRELOAD_AHEAD (2) + PRELOAD_BEHIND (1)
const LOADER_THREADS: usize = 4;

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

/// Decoded image data — either a static image or an animated sequence.
pub enum ImageData {
    Static(DecodedImage),
    Animated(Vec<AnimationFrame>),
}

pub struct LoadResult {
    pub index: usize,
    pub result: Result<ImageData, String>,
}

pub struct ImageLoader {
    tx: Sender<LoadResult>,
    pub rx: Receiver<LoadResult>,
    loading: HashSet<usize>,
    pool: rayon::ThreadPool,
}

impl ImageLoader {
    pub fn new() -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(LOADER_THREADS)
            .thread_name(|i| format!("img-loader-{i}"))
            .build()
            .expect("failed to create image loader thread pool");
        Self { tx, rx, loading: HashSet::new(), pool }
    }

    pub fn is_loading(&self, index: usize) -> bool {
        self.loading.contains(&index)
    }

    pub fn request_load(&mut self, index: usize, path: PathBuf) {
        if self.loading.contains(&index) {
            return;
        }
        self.loading.insert(index);
        let tx = self.tx.clone();
        // Use the bounded thread pool instead of spawning a new OS thread each time.
        self.pool.spawn(move || {
            let result = load_image_file(index, &path);
            let _ = tx.send(result);
        });
    }

    pub fn poll(&mut self) -> Option<LoadResult> {
        match self.rx.try_recv() {
            Ok(result) => {
                self.loading.remove(&result.index);
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

fn load_image_file(index: usize, path: &PathBuf) -> LoadResult {
    let result = (|| -> Result<ImageData, String> {
        let ext = path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();

        match ext.as_str() {
            "gif" => load_gif(path),
            "png" | "apng" => load_png(path),
            "webp" => load_webp(path),
            _ => load_static(path),
        }
    })();
    LoadResult { index, result }
}

fn load_static(path: &PathBuf) -> Result<ImageData, String> {
    let img = image::open(path).map_err(|e| e.to_string())?;
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    let pixels = rgba.into_raw();
    Ok(ImageData::Static(DecodedImage { width, height, pixels }))
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

    // Single-frame (or empty) GIF → treat as static
    if raw_frames.len() <= 1 {
        if let Some(frame) = raw_frames.into_iter().next() {
            let buffer = frame.into_buffer();
            let (width, height) = buffer.dimensions();
            let pixels = buffer.into_raw();
            return Ok(ImageData::Static(DecodedImage { width, height, pixels }));
        }
        return Err("GIF has no frames".to_string());
    }

    let frames: Vec<AnimationFrame> = raw_frames.into_iter().map(|frame| {
        let (numer, denom) = frame.delay().numer_denom_ms();
        let delay_ms = if denom == 0 { 100 } else { numer / denom };
        // GIF spec: delay of 0 (or ≤10ms) is typically rendered as 100ms by browsers
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

fn load_png(path: &PathBuf) -> Result<ImageData, String> {
    use image::codecs::png::PngDecoder;
    use image::AnimationDecoder;
    use std::io::BufReader;

    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let reader = BufReader::new(file);
    let decoder = PngDecoder::new(reader).map_err(|e| e.to_string())?;

    if !decoder.is_apng().map_err(|e| e.to_string())? {
        // Regular (static) PNG — use the standard path
        return load_static(path);
    }

    let raw_frames = decoder.apng()
        .map_err(|e| e.to_string())?
        .into_frames()
        .collect_frames()
        .map_err(|e| e.to_string())?;

    if raw_frames.len() <= 1 {
        return load_static(path);
    }

    let frames: Vec<AnimationFrame> = raw_frames.into_iter().map(|frame| {
        let (numer, denom) = frame.delay().numer_denom_ms();
        let delay_ms = if denom == 0 { 100 } else { numer / denom };
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

    // Single-frame WebP → treat as static
    if raw_frames.len() <= 1 {
        return load_static(path);
    }

    let frames: Vec<AnimationFrame> = raw_frames.into_iter().map(|frame| {
        let (numer, denom) = frame.delay().numer_denom_ms();
        let delay_ms = if denom == 0 { 100 } else { numer / denom };
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

    pub fn insert(&mut self, index: usize, handle: egui::TextureHandle, current_index: usize) {
        self.textures.insert(index, handle);
        self.evict(current_index);
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

    fn evict(&mut self, current_index: usize) {
        if self.textures.len() <= self.max_size {
            return;
        }
        // Evict the texture farthest from the current index
        let to_remove = self.textures
            .keys()
            .copied()
            .max_by_key(|&idx| (idx as isize - current_index as isize).unsigned_abs());
        if let Some(idx) = to_remove {
            self.textures.remove(&idx);
        }
    }
}
