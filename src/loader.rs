use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use crossbeam_channel::{Receiver, Sender, TryRecvError};

/// Maximum concurrent image decode tasks.
/// = 1 (current) + PRELOAD_AHEAD (2) + PRELOAD_BEHIND (1)
const LOADER_THREADS: usize = 4;

pub struct DecodedImage {
    #[allow(dead_code)]
    pub index: usize,
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>, // RGBA8
}

pub struct LoadResult {
    pub index: usize,
    pub result: Result<DecodedImage, String>,
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
    let result = (|| -> Result<DecodedImage, String> {
        let img = image::open(path).map_err(|e| e.to_string())?;
        let rgba = img.to_rgba8();
        let (width, height) = rgba.dimensions();
        let pixels = rgba.into_raw();
        Ok(DecodedImage { index, width, height, pixels })
    })();
    LoadResult { index, result }
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
